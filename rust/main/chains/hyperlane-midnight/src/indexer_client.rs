use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use url::Url;

use hyperlane_core::ChainResult;

use hyperlane_core::{HyperlaneMessage, H256};

use crate::state_decode::{
    decode_deliveries, decode_dispatch_snapshot, decode_dispatched_message, decode_ism_state,
    decode_nonce_count, DispatchSnapshot, IsmState,
};
use crate::HyperlaneMidnightError;

/// How long a decoded ISM state is reused before re-reading from the indexer.
/// Short on purpose: it collapses the per-delivery read burst (the Mailbox
/// re-reads the validator set to sort signatures on every `process`) while
/// staying well under the relayer's 120s ISM cache, so an on-chain validator
/// rotation is still picked up quickly.
const ISM_STATE_TTL: Duration = Duration::from_secs(5);

/// HTTP client for the Midnight indexer's GraphQL endpoint.
#[derive(Debug, Clone)]
pub struct MidnightIndexerClient {
    endpoint: Url,
    http: reqwest::Client,
    /// Per-address cache of the decoded ISM state, shared across clones of
    /// this client (it is cloned into the provider). Caches successful decodes
    /// only. Note: the Mailbox and the ISM build separate client instances, so
    /// they do not share this cache with each other — it removes the
    /// per-delivery re-read within a single long-lived client.
    ism_cache: Arc<Mutex<HashMap<String, (IsmState, Instant)>>>,
}

impl MidnightIndexerClient {
    /// Build a client pointed at the given GraphQL HTTP endpoint.
    pub fn new(endpoint: Url) -> Self {
        Self {
            endpoint,
            http: reqwest::Client::new(),
            ism_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Latest known block height, or `None` if the indexer has not observed any yet.
    pub async fn latest_block_height(&self) -> Result<Option<u64>, HyperlaneMidnightError> {
        let query = r#"query { block { height } }"#;
        let body = GraphqlRequest {
            query,
            variables: serde_json::Value::Null,
        };

        let data = self.post::<LatestBlock>(&body).await?.into_data()?;
        Ok(data.and_then(|d| d.block.map(|b| b.height)))
    }

    /// Fetch the full serialized ledger state of a deployed contract at the
    /// indexer's latest observed block, hex-decoded into raw bytes. This is a
    /// one-shot `contractAction(address)` HTTP query returning the latest
    /// state, not the streaming `contractActions` subscription. No `offset` is
    /// passed, so the read is not pinned to a fixed block; the schema's
    /// `offset: ContractActionOffset` supports block-pinned reads if a future
    /// caller needs them. `address` is the contract address as the indexer's
    /// `HexEncoded` scalar (hex, with or without `0x`).
    pub async fn contract_state(&self, address: &str) -> Result<Vec<u8>, HyperlaneMidnightError> {
        let query =
            r#"query ($address: HexEncoded!) { contractAction(address: $address) { state } }"#;
        let body = GraphqlRequest {
            query,
            variables: serde_json::json!({ "address": address }),
        };

        let data = self.post::<ContractActionData>(&body).await?.into_data()?;
        let state_hex = data
            .and_then(|d| d.contract_action)
            .map(|c| c.state)
            .ok_or_else(|| {
                HyperlaneMidnightError::IndexerGraphql(format!(
                    "indexer returned no contract action for address {address}"
                ))
            })?;
        hex::decode(state_hex.trim_start_matches("0x")).map_err(|e| {
            HyperlaneMidnightError::StateDecode(format!("indexer state is not valid hex: {e}"))
        })
    }

    /// Read and decode the MessageIdMultisigIsm config (validators, threshold,
    /// module type) from the deployed `night` contract's on-chain state.
    /// Cached for `ISM_STATE_TTL` to avoid a fresh network read + decode on
    /// every Mailbox delivery.
    pub async fn read_ism_state(&self, address: &str) -> ChainResult<IsmState> {
        if let Ok(cache) = self.ism_cache.lock() {
            if let Some((state, fetched_at)) = cache.get(address) {
                if fetched_at.elapsed() < ISM_STATE_TTL {
                    return Ok(state.clone());
                }
            }
        }

        let bytes = self.contract_state(address).await?;
        let state = decode_ism_state(&bytes)?;
        if let Ok(mut cache) = self.ism_cache.lock() {
            cache.insert(address.to_string(), (state.clone(), Instant::now()));
        }
        Ok(state)
    }

    /// Read the dispatch `nonce` counter from the deployed `night` contract's
    /// Mailbox state. This is the count of messages dispatched so far; valid
    /// dispatch nonces are `0..count`. Not cached: the dispatch indexer reads
    /// it once per scan to learn the sequence tip.
    pub async fn read_nonce_count(&self, address: &str) -> ChainResult<u32> {
        let bytes = self.contract_state(address).await?;
        decode_nonce_count(&bytes)
    }

    /// Read the dispatched `HyperlaneMessage` stored at `nonce` in the deployed
    /// `night` contract's `dispatched_messages` map. Returns `None` if no
    /// message is stored at that nonce.
    pub async fn read_dispatched_message(
        &self,
        address: &str,
        nonce: u32,
    ) -> ChainResult<Option<HyperlaneMessage>> {
        let bytes = self.contract_state(address).await?;
        decode_dispatched_message(&bytes, nonce)
    }

    /// Read the whole dispatch state (every dispatched `HyperlaneMessage` keyed
    /// by nonce, plus the nonce counter) from a SINGLE state fetch. The dispatch
    /// indexer uses this once per scan to serve a whole nonce range, instead of
    /// re-fetching and re-scanning the full state per nonce. Decoding the count
    /// and the messages from the same blob also keeps them mutually consistent.
    pub async fn read_dispatch_snapshot(&self, address: &str) -> ChainResult<DispatchSnapshot> {
        let bytes = self.contract_state(address).await?;
        decode_dispatch_snapshot(&bytes)
    }

    /// Read the full `deliveries` set (delivered message ids) from the deployed
    /// `night` contract's Mailbox state. The set is unordered.
    pub async fn read_deliveries(&self, address: &str) -> ChainResult<Vec<H256>> {
        let bytes = self.contract_state(address).await?;
        decode_deliveries(&bytes)
    }

    async fn post<T: for<'de> Deserialize<'de>>(
        &self,
        body: &GraphqlRequest<'_>,
    ) -> Result<GraphqlResponse<T>, HyperlaneMidnightError> {
        let response = self
            .http
            .post(self.endpoint.clone())
            .json(body)
            .send()
            .await?;
        let status = response.status();
        let bytes = response.bytes().await?;

        if !status.is_success() {
            return Err(HyperlaneMidnightError::IndexerGraphql(format!(
                "HTTP {status}: {}",
                String::from_utf8_lossy(&bytes)
            )));
        }

        let parsed: GraphqlResponse<T> = serde_json::from_slice(&bytes)?;
        Ok(parsed)
    }
}

#[derive(Debug, Serialize)]
struct GraphqlRequest<'a> {
    query: &'a str,
    variables: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct GraphqlResponse<T> {
    data: Option<T>,
    errors: Option<Vec<GraphqlError>>,
}

impl<T> GraphqlResponse<T> {
    /// Surface any GraphQL-level errors as a single error, otherwise yield the
    /// `data` payload. Keeps error handling in one place for every query.
    fn into_data(self) -> Result<Option<T>, HyperlaneMidnightError> {
        if let Some(errors) = self.errors {
            if !errors.is_empty() {
                return Err(HyperlaneMidnightError::IndexerGraphql(
                    errors
                        .into_iter()
                        .map(|e| e.message)
                        .collect::<Vec<_>>()
                        .join("; "),
                ));
            }
        }
        Ok(self.data)
    }
}

#[derive(Debug, Deserialize)]
struct GraphqlError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct ContractActionData {
    #[serde(rename = "contractAction", default)]
    contract_action: Option<ContractActionState>,
}

#[derive(Debug, Deserialize)]
struct ContractActionState {
    state: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live integration test: read the deployed `night` contract's ISM state
    /// from a running Midnight indexer and assert it decodes to a valid,
    /// internally-consistent MessageIdMultisig config — i.e. the live
    /// `contractAction` query + native decode work end-to-end against a real
    /// standalone node. Ignored by default; run with the devnet up:
    ///
    ///   MIDNIGHT_INDEXER_URL=http://127.0.0.1:8088/api/v3/graphql \
    ///   MIDNIGHT_NIGHT_ADDRESS=<deployed night address hex> \
    ///   cargo test -p hyperlane-midnight reads_live_ism_state -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "requires a running Midnight devnet (node + indexer) with a deployed night contract"]
    async fn reads_live_ism_state_from_indexer() {
        let endpoint = std::env::var("MIDNIGHT_INDEXER_URL")
            .expect("set MIDNIGHT_INDEXER_URL to the indexer GraphQL endpoint");
        let address = std::env::var("MIDNIGHT_NIGHT_ADDRESS")
            .expect("set MIDNIGHT_NIGHT_ADDRESS to the deployed night contract address (hex)");

        let client = MidnightIndexerClient::new(Url::parse(&endpoint).expect("valid indexer URL"));
        let ism = client
            .read_ism_state(&address)
            .await
            .expect("read + decode ISM state from the live indexer");

        println!("decoded ISM state from chain: {ism:?}");

        // MessageIdMultisig is discriminant 5 in Hyperlane's ModuleType enum.
        assert_eq!(ism.module_type, 5, "module_type should be MessageIdMultisig");
        // Decoded slot count must match the validators map.
        assert_eq!(
            ism.validator_count as usize,
            ism.validators.len(),
            "validator_count must match decoded validator slots"
        );
        // A usable multisig: >= 1 validator and 1 <= threshold <= count.
        assert!(!ism.validators.is_empty(), "expected at least one validator");
        assert!(ism.threshold >= 1, "threshold must be at least 1");
        assert!(
            ism.threshold as usize <= ism.validators.len(),
            "threshold {} cannot exceed validator count {}",
            ism.threshold,
            ism.validators.len()
        );
    }
}

#[derive(Debug, Deserialize)]
struct LatestBlock {
    #[serde(default)]
    block: Option<BlockHeight>,
}

#[derive(Debug, Deserialize)]
struct BlockHeight {
    height: u64,
}

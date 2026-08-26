use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use url::Url;

use hyperlane_core::{ChainResult, U256};

use hyperlane_core::H256;

use crate::state_decode::{decode_ism_state, decode_nonce_count, IsmState};
use crate::HyperlaneMidnightError;

/// How long a decoded ISM state is reused before re-reading. Short on purpose:
/// long enough to collapse the per-delivery read burst, short enough that an
/// on-chain validator rotation is still picked up quickly.
const ISM_STATE_TTL: Duration = Duration::from_secs(5);

/// The indexer orders by the monotonic event `id`, so offset paging over the
/// append-only log is stable and a short page marks the end.
const CONTRACT_EVENTS_PAGE_SIZE: usize = 200;

/// Compact `Bytes<32>`, zero-padded by the indexer before serving.
pub const MISC_NAME_LEN: usize = 32;

/// Compact `Bytes<256>`, zero-padded by the indexer before serving.
pub const MISC_PAYLOAD_LEN: usize = 256;

/// From the indexer's `transactionResult.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TxStatus {
    Success,
    PartialSuccess,
    Failure,
}

impl TxStatus {
    /// A failed section drops every effect it produced, events included, and a
    /// transcript only splits at a `kernel.checkpoint()`, which the Hyperlane
    /// contracts never emit. So an event and the state write it reports always
    /// commit or vanish together, which makes `PartialSuccess` safe to index.
    pub fn is_indexable(self) -> bool {
        match self {
            TxStatus::Success | TxStatus::PartialSuccess => true,
            TxStatus::Failure => false,
        }
    }
}

/// A Compact `emit` from a contract call, with the metadata a `LogMeta` needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MiscEvent {
    pub name: [u8; MISC_NAME_LEN],
    /// Always [`MISC_PAYLOAD_LEN`] bytes; consumers slice fixed offsets.
    pub payload: Vec<u8>,
    /// Chain-global monotonic event id. Sparse per contract, since other event
    /// kinds share the sequence, so never assume contiguity.
    pub id: u64,
    pub tx_hash: H256,
    /// Indexer-global transaction id (a monotonic integer, not per-block).
    pub tx_id: u64,
    pub block_hash: H256,
    pub block_height: u64,
    /// Block timestamp in unix milliseconds.
    pub block_timestamp_ms: u64,
    pub tx_status: TxStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockDetails {
    pub hash: H256,
    pub height: u64,
    /// Block timestamp in unix milliseconds (Substrate `Timestamp::set`).
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionDetails {
    pub hash: H256,
    pub id: u64,
    pub block: BlockDetails,
    /// Application status; `None` for transaction kinds that carry no
    /// `transactionResult` (system transactions).
    pub status: Option<TxStatus>,
    /// Paid fee in SPECK (DUST atomic unit); `None` for transaction kinds
    /// that carry no fee.
    pub fee_specks: Option<hyperlane_core::U256>,
}

/// GraphQL selection for `contractEvents`. `name`/`payload` sit behind an
/// inline fragment because they are not on the `ContractEvent` interface, and
/// `transactionResult` only exists on `RegularTransaction`.
const CONTRACT_EVENTS_QUERY: &str = r#"query ($filter: ContractEventFilter!, $limit: Int!, $offset: Int!) {
  contractEvents(filter: $filter, limit: $limit, offset: $offset) {
    __typename
    id
    ... on MiscContractEvent { name payload }
    transaction {
      id
      hash
      block { hash height timestamp }
      ... on RegularTransaction { transactionResult { status } }
    }
  }
}"#;

/// HTTP client for the Midnight indexer's GraphQL endpoint.
#[derive(Debug, Clone)]
pub struct MidnightIndexerClient {
    endpoint: Url,
    http: reqwest::Client,
    /// Per-address cache of successfully decoded ISM state, shared across
    /// clones of this client. The Mailbox and the ISM build separate clients,
    /// so each caches its own reads.
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

    /// The full serialized ledger state at the indexer's latest observed block.
    /// No `offset` is passed, so the read is not pinned to a block.
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

    /// Read and decode the MessageIdMultisigIsm config from the deployed
    /// contract's state. Cached for `ISM_STATE_TTL` so a Mailbox delivery does
    /// not pay for a fresh read and decode.
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

    /// The dispatch and merkle indexers use this as their sequence tip: one
    /// state fetch and one counter decode, with no per-message work.
    pub async fn read_nonce_count(&self, address: &str) -> ChainResult<u32> {
        let bytes = self.contract_state(address).await?;
        decode_nonce_count(&bytes)
    }

    /// In event-id order, paginated transparently, with failed transactions
    /// excluded (see [`TxStatus::is_indexable`]).
    pub async fn misc_events_in_range(
        &self,
        address: &str,
        from_block: u32,
        to_block: u32,
    ) -> Result<Vec<MiscEvent>, HyperlaneMidnightError> {
        let filter = serde_json::json!({
            "contractAddress": address,
            "types": ["MISC"],
            "fromBlock": from_block,
            "toBlock": to_block,
        });
        self.misc_events_filtered(filter).await
    }

    /// Failed transactions are excluded (see [`TxStatus::is_indexable`]).
    pub async fn misc_events_by_tx_hash(
        &self,
        address: &str,
        tx_hash: &H256,
    ) -> Result<Vec<MiscEvent>, HyperlaneMidnightError> {
        let filter = serde_json::json!({
            "contractAddress": address,
            "types": ["MISC"],
            "transactionHash": format!("{tx_hash:x}"),
        });
        self.misc_events_filtered(filter).await
    }

    async fn misc_events_filtered(
        &self,
        filter: serde_json::Value,
    ) -> Result<Vec<MiscEvent>, HyperlaneMidnightError> {
        let mut out = Vec::new();
        let mut offset = 0usize;
        loop {
            let body = GraphqlRequest {
                query: CONTRACT_EVENTS_QUERY,
                variables: serde_json::json!({
                    "filter": filter,
                    "limit": CONTRACT_EVENTS_PAGE_SIZE,
                    "offset": offset,
                }),
            };
            let data = self.post::<ContractEventsData>(&body).await?.into_data()?;
            let raw = data.map(|d| d.contract_events).unwrap_or_default();
            let page_len = raw.len();
            out.extend(misc_events_from_raw(raw)?);
            if page_len < CONTRACT_EVENTS_PAGE_SIZE {
                break;
            }
            offset += page_len;
        }
        Ok(out)
    }

    /// Fetch block metadata by height. `None` if the indexer has not observed
    /// a block at that height.
    pub async fn block_by_height(
        &self,
        height: u64,
    ) -> Result<Option<BlockDetails>, HyperlaneMidnightError> {
        let query = r#"query ($height: Int!) {
  block(offset: { height: $height }) { hash height timestamp }
}"#;
        let body = GraphqlRequest {
            query,
            variables: serde_json::json!({ "height": height }),
        };
        let data = self.post::<BlockData>(&body).await?.into_data()?;
        data.and_then(|d| d.block)
            .map(|b| b.into_details())
            .transpose()
    }

    /// Native-NIGHT (all-zeros token type) unshielded balance of a deployed
    /// contract. `None` when the indexer knows no contract at the address.
    pub async fn contract_native_balance(
        &self,
        address: &str,
    ) -> Result<Option<U256>, HyperlaneMidnightError> {
        let query = r#"query ($address: HexEncoded!) {
  contractAction(address: $address) { unshieldedBalances { tokenType amount } }
}"#;
        let body = GraphqlRequest {
            query,
            variables: serde_json::json!({ "address": address }),
        };
        let data = self
            .post::<ContractActionBalancesData>(&body)
            .await?
            .into_data()?;
        let Some(action) = data.and_then(|d| d.contract_action) else {
            return Ok(None);
        };
        let mut total = U256::zero();
        for entry in action.unshielded_balances {
            let token = entry.token_type.trim_start_matches("0x");
            if !token.is_empty() && token.bytes().all(|b| b == b'0') {
                let amount = U256::from_dec_str(&entry.amount).map_err(|e| {
                    HyperlaneMidnightError::IndexerGraphql(format!(
                        "unshielded balance amount '{}' is not a decimal string: {e}",
                        entry.amount
                    ))
                })?;
                total = total.saturating_add(amount);
            }
        }
        Ok(Some(total))
    }

    /// Fetch transaction metadata by hash (full 32-byte hex). `None` if the
    /// indexer knows no transaction with that hash.
    pub async fn transaction_by_hash(
        &self,
        hash: &H256,
    ) -> Result<Option<TransactionDetails>, HyperlaneMidnightError> {
        let query = r#"query ($hash: HexEncoded!) {
  transactions(offset: { hash: $hash }) {
    id
    hash
    block { hash height timestamp }
    ... on RegularTransaction { transactionResult { status } fee }
  }
}"#;
        let body = GraphqlRequest {
            query,
            variables: serde_json::json!({ "hash": format!("{hash:x}") }),
        };
        let data = self.post::<TransactionsData>(&body).await?.into_data()?;
        let Some(raw) = data.and_then(|d| d.transactions.into_iter().next()) else {
            return Ok(None);
        };
        Ok(Some(raw.into_details()?))
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

#[derive(Debug, Deserialize)]
struct ContractActionBalancesData {
    #[serde(rename = "contractAction", default)]
    contract_action: Option<ContractActionBalances>,
}

#[derive(Debug, Deserialize)]
struct ContractActionBalances {
    #[serde(rename = "unshieldedBalances", default)]
    unshielded_balances: Vec<UnshieldedBalanceEntry>,
}

#[derive(Debug, Deserialize)]
struct UnshieldedBalanceEntry {
    #[serde(rename = "tokenType")]
    token_type: String,
    amount: String,
}

#[derive(Debug, Deserialize)]
struct ContractEventsData {
    #[serde(rename = "contractEvents", default)]
    contract_events: Vec<RawContractEvent>,
}

#[derive(Debug, Deserialize)]
struct RawContractEvent {
    #[serde(rename = "__typename")]
    typename: String,
    id: u64,
    /// Only present on `MiscContractEvent` (behind the inline fragment).
    #[serde(default)]
    name: Option<String>,
    /// Only present on `MiscContractEvent` (behind the inline fragment).
    #[serde(default)]
    payload: Option<String>,
    transaction: RawTransaction,
}

#[derive(Debug, Deserialize)]
struct RawTransaction {
    id: u64,
    hash: String,
    block: RawBlock,
    /// Only present on `RegularTransaction` (behind the inline fragment).
    #[serde(rename = "transactionResult", default)]
    transaction_result: Option<RawTransactionResult>,
    /// Only present on `RegularTransaction`; SPECK decimal string.
    #[serde(default)]
    fee: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawBlock {
    hash: String,
    height: u64,
    /// Unix milliseconds (Substrate `Timestamp::set { now }`).
    timestamp: u64,
}

#[derive(Debug, Deserialize)]
struct RawTransactionResult {
    status: TxStatus,
}

#[derive(Debug, Deserialize)]
struct BlockData {
    #[serde(default)]
    block: Option<RawBlock>,
}

#[derive(Debug, Deserialize)]
struct TransactionsData {
    #[serde(default)]
    transactions: Vec<RawTransaction>,
}

fn hex_bytes(value: &str, what: &str) -> Result<Vec<u8>, HyperlaneMidnightError> {
    hex::decode(value.trim_start_matches("0x")).map_err(|e| {
        HyperlaneMidnightError::IndexerGraphql(format!("{what} is not valid hex: {e}"))
    })
}

fn hex_h256(value: &str, what: &str) -> Result<H256, HyperlaneMidnightError> {
    let bytes = hex_bytes(value, what)?;
    if bytes.len() != 32 {
        return Err(HyperlaneMidnightError::IndexerGraphql(format!(
            "{what} is {} bytes, expected 32",
            bytes.len()
        )));
    }
    Ok(H256::from_slice(&bytes))
}

/// The indexer already re-pads names and payloads to their declared widths, so
/// the padding here is belt-and-braces; only an over-long value is an error.
fn hex_padded(value: &str, width: usize, what: &str) -> Result<Vec<u8>, HyperlaneMidnightError> {
    let mut bytes = hex_bytes(value, what)?;
    if bytes.len() > width {
        return Err(HyperlaneMidnightError::IndexerGraphql(format!(
            "{what} is {} bytes, expected at most {width}",
            bytes.len()
        )));
    }
    bytes.resize(width, 0);
    Ok(bytes)
}

impl RawBlock {
    fn into_details(self) -> Result<BlockDetails, HyperlaneMidnightError> {
        Ok(BlockDetails {
            hash: hex_h256(&self.hash, "block hash")?,
            height: self.height,
            timestamp_ms: self.timestamp,
        })
    }
}

impl RawTransaction {
    fn into_details(self) -> Result<TransactionDetails, HyperlaneMidnightError> {
        let fee_specks = self
            .fee
            .as_deref()
            .map(|fee| {
                hyperlane_core::U256::from_dec_str(fee).map_err(|e| {
                    HyperlaneMidnightError::IndexerGraphql(format!(
                        "transaction fee `{fee}` is not a decimal integer: {e}"
                    ))
                })
            })
            .transpose()?;
        Ok(TransactionDetails {
            hash: hex_h256(&self.hash, "transaction hash")?,
            id: self.id,
            block: self.block.into_details()?,
            status: self.transaction_result.map(|r| r.status),
            fee_specks,
        })
    }
}

/// Rows that are not `MiscContractEvent`, or that carry no `transactionResult`
/// and so cannot be a contract call, are skipped.
fn misc_events_from_raw(
    raw: Vec<RawContractEvent>,
) -> Result<Vec<MiscEvent>, HyperlaneMidnightError> {
    let mut out = Vec::with_capacity(raw.len());
    for event in raw {
        let (Some(name), Some(payload)) = (event.name.as_deref(), event.payload.as_deref()) else {
            if event.typename == "MiscContractEvent" {
                return Err(HyperlaneMidnightError::IndexerGraphql(format!(
                    "MiscContractEvent {} is missing name/payload",
                    event.id
                )));
            }
            continue;
        };
        let Some(result) = &event.transaction.transaction_result else {
            continue;
        };
        if !result.status.is_indexable() {
            continue;
        }

        let name_bytes = hex_padded(name, MISC_NAME_LEN, "misc event name")?;
        let mut name = [0u8; MISC_NAME_LEN];
        name.copy_from_slice(&name_bytes);
        out.push(MiscEvent {
            name,
            payload: hex_padded(payload, MISC_PAYLOAD_LEN, "misc event payload")?,
            id: event.id,
            tx_hash: hex_h256(&event.transaction.hash, "transaction hash")?,
            tx_id: event.transaction.id,
            block_hash: hex_h256(&event.transaction.block.hash, "block hash")?,
            block_height: event.transaction.block.height,
            block_timestamp_ms: event.transaction.block.timestamp,
            tx_status: result.status,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture carries one event of every status, so it pins the filter,
    /// the padded widths, both hash spellings, and the metadata mapping.
    #[test]
    fn parses_contract_events_fixture() {
        let response: GraphqlResponse<ContractEventsData> = serde_json::from_str(include_str!(
            "../tests/fixtures/contract-events-response.json"
        ))
        .expect("fixture parses");
        let raw = response
            .into_data()
            .expect("no graphql errors")
            .expect("data present")
            .contract_events;
        assert_eq!(raw.len(), 5, "fixture has five raw events");

        let events = misc_events_from_raw(raw).expect("convert events");
        assert_eq!(events.len(), 4, "FAILURE event is excluded");
        assert_eq!(
            events.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![11, 14, 19, 23],
            "monotonic event-id order preserved"
        );

        let dispatch = &events[0];
        let mut want_name = [0u8; MISC_NAME_LEN];
        want_name[..12].copy_from_slice(b"HYP_DISPATCH");
        assert_eq!(dispatch.name, want_name, "zero-padded 32-byte name");
        assert_eq!(dispatch.payload.len(), MISC_PAYLOAD_LEN);
        assert_eq!(dispatch.tx_hash, H256::repeat_byte(0x01));
        assert_eq!(dispatch.tx_id, 5);
        assert_eq!(dispatch.block_hash, H256::repeat_byte(0xa1));
        assert_eq!(dispatch.block_height, 100);
        assert_eq!(dispatch.block_timestamp_ms, 1_700_000_000_000);
        assert_eq!(dispatch.tx_status, TxStatus::Success);

        assert_eq!(events[2].tx_status, TxStatus::PartialSuccess);

        // The HYP_PROCESS event's hashes are 0x-prefixed in the fixture.
        let process = &events[3];
        assert_eq!(process.tx_hash, H256::repeat_byte(0x03));
        assert_eq!(process.block_hash, H256::repeat_byte(0xa3));
    }

    #[test]
    fn tx_status_filter_excludes_only_failures() {
        assert!(TxStatus::Success.is_indexable());
        assert!(TxStatus::PartialSuccess.is_indexable());
        assert!(!TxStatus::Failure.is_indexable());
    }

    #[test]
    fn hex_padded_pads_short_and_rejects_long() {
        let padded = hex_padded("aabb", 4, "test").expect("pad");
        assert_eq!(padded, vec![0xaa, 0xbb, 0x00, 0x00]);
        // Over-long values are an error, not a truncation.
        assert!(hex_padded("aabbccddee", 4, "test").is_err());
    }

    /// Covers the `RegularTransaction`-only `transactionResult` and `fee`.
    #[test]
    fn parses_transaction_details() {
        let json = r#"{
          "data": { "transactions": [ {
            "id": 42,
            "hash": "0101010101010101010101010101010101010101010101010101010101010101",
            "block": { "hash": "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1", "height": 7, "timestamp": 1700000000000 },
            "transactionResult": { "status": "SUCCESS" },
            "fee": "123456"
          } ] }
        }"#;
        let response: GraphqlResponse<TransactionsData> =
            serde_json::from_str(json).expect("parses");
        let raw = response
            .into_data()
            .expect("no errors")
            .expect("data")
            .transactions
            .into_iter()
            .next()
            .expect("one transaction");
        let details = raw.into_details().expect("convert");
        assert_eq!(details.id, 42);
        assert_eq!(details.hash, H256::repeat_byte(0x01));
        assert_eq!(details.block.hash, H256::repeat_byte(0xa1));
        assert_eq!(details.block.height, 7);
        assert_eq!(details.block.timestamp_ms, 1_700_000_000_000);
        assert_eq!(details.status, Some(TxStatus::Success));
        assert_eq!(details.fee_specks, Some(hyperlane_core::U256::from(123456)));
    }

    /// Live check that the `contractAction` query and the native decode work
    /// end to end. Run with the devnet up:
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

        assert_eq!(
            ism.module_type, 5,
            "module_type should be MessageIdMultisig"
        );
        assert_eq!(
            ism.validator_count as usize,
            ism.validators.len(),
            "validator_count must match decoded validator slots"
        );
        assert!(
            !ism.validators.is_empty(),
            "expected at least one validator"
        );
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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use url::Url;

use hyperlane_core::ChainResult;

use hyperlane_core::H256;

use crate::state_decode::{decode_ism_state, decode_nonce_count, IsmState};
use crate::HyperlaneMidnightError;

/// How long a decoded ISM state is reused before re-reading from the indexer.
/// Short on purpose: it collapses the per-delivery read burst (the Mailbox
/// re-reads the validator set to sort signatures on every `process`) while
/// staying well under the relayer's 120s ISM cache, so an on-chain validator
/// rotation is still picked up quickly.
const ISM_STATE_TTL: Duration = Duration::from_secs(5);

/// Page size for paginated `contractEvents` queries. The indexer orders
/// results by the monotonic event `id`, so `limit`/`offset` paging over the
/// append-only event log is stable; a page shorter than this marks the end.
const CONTRACT_EVENTS_PAGE_SIZE: usize = 200;

/// Fixed width of a Misc event name (Compact `Bytes<32>`), zero-padded by the
/// indexer before serving.
pub const MISC_NAME_LEN: usize = 32;

/// Fixed width of a Misc event payload (Compact `Bytes<256>`), zero-padded by
/// the indexer before serving — consumers slice fixed offsets directly.
pub const MISC_PAYLOAD_LEN: usize = 256;

/// The application status of the transaction an event was emitted from, as
/// reported by the indexer's `transactionResult.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TxStatus {
    /// Every segment of the transaction applied.
    Success,
    /// Some segments applied, some failed.
    PartialSuccess,
    /// Nothing applied.
    Failure,
}

impl TxStatus {
    /// Whether events from a transaction with this status should be served to
    /// the Hyperlane indexers. This is THE single place that decision lives.
    ///
    /// - `Success`: state writes landed, index the events.
    /// - `Failure`: nothing was applied, so the paired state write never
    ///   happened — drop the events.
    /// - `PartialSuccess`: included FOR NOW. Whether an event can be emitted
    ///   from a failed segment (i.e. without its paired state write) is being
    ///   verified separately on a devnet probe; flip this to `false` for
    ///   `PartialSuccess` if segment attribution turns out unsafe.
    pub fn is_indexable(self) -> bool {
        match self {
            TxStatus::Success | TxStatus::PartialSuccess => true,
            TxStatus::Failure => false,
        }
    }
}

/// A decoded `MiscContractEvent` (a Compact `emit` from a contract call),
/// together with the transaction/block metadata the Hyperlane `LogMeta`
/// needs. Only events whose transaction status is indexable (see
/// [`TxStatus::is_indexable`]) are surfaced by the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MiscEvent {
    /// Contract-defined event name, zero-padded to 32 bytes (e.g.
    /// `HYP_DISPATCH`).
    pub name: [u8; MISC_NAME_LEN],
    /// Opaque zero-padded payload, always [`MISC_PAYLOAD_LEN`] bytes;
    /// consumers slice fixed offsets.
    pub payload: Vec<u8>,
    /// Chain-global monotonic event id. SPARSE per contract: other event
    /// kinds share the sequence, so never assume contiguity.
    pub id: u64,
    /// Hash of the transaction the event was emitted from.
    pub tx_hash: H256,
    /// Indexer-global transaction id (a monotonic integer, not per-block).
    pub tx_id: u64,
    /// Hash of the block containing the transaction.
    pub block_hash: H256,
    /// Height of the block containing the transaction.
    pub block_height: u64,
    /// Block timestamp in unix milliseconds.
    pub block_timestamp_ms: u64,
    /// The transaction's application status.
    pub tx_status: TxStatus,
}

/// Block metadata from `Query.block`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockDetails {
    /// Block hash.
    pub hash: H256,
    /// Block height.
    pub height: u64,
    /// Block timestamp in unix milliseconds (Substrate `Timestamp::set`).
    pub timestamp_ms: u64,
}

/// Transaction metadata from `Query.transactions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionDetails {
    /// Transaction hash.
    pub hash: H256,
    /// Indexer-global transaction id.
    pub id: u64,
    /// The block containing the transaction.
    pub block: BlockDetails,
    /// Application status; `None` for transaction kinds that carry no
    /// `transactionResult` (system transactions).
    pub status: Option<TxStatus>,
    /// Paid fee in SPECK (DUST atomic unit); `None` for transaction kinds
    /// that carry no fee.
    pub fee_specks: Option<hyperlane_core::U256>,
}

/// GraphQL selection for `contractEvents`. The filter always narrows to
/// `types: [MISC]`, so every element is a `MiscContractEvent`; `name`/
/// `payload` live behind the inline fragment because they are not on the
/// `ContractEvent` interface, and `transactionResult` needs the
/// `RegularTransaction` fragment (system transactions carry no result).
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

    /// Read the Mailbox `nonce` counter (the number of messages dispatched so
    /// far) from the deployed `night` contract's state. The dispatch and
    /// merkle indexers use this as the sequence tip — a single state fetch
    /// plus a one-counter decode, no per-message decoding.
    pub async fn read_nonce_count(&self, address: &str) -> ChainResult<u32> {
        let bytes = self.contract_state(address).await?;
        decode_nonce_count(&bytes)
    }

    /// Fetch every Misc contract event `address` emitted in the inclusive
    /// block range, in monotonic event-id order, paginated transparently.
    /// Events from `FAILURE` transactions are excluded (see
    /// [`TxStatus::is_indexable`]).
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

    /// Fetch every Misc contract event `address` emitted from the transaction
    /// with the given hash (the `transactionHash` filter). Events from
    /// `FAILURE` transactions are excluded (see [`TxStatus::is_indexable`]).
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

    /// Run the paginated `contractEvents` query for the given filter, looping
    /// pages until a short page marks the end of the result set.
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

/// Decode a `HexEncoded` scalar (with or without `0x`) into raw bytes.
fn hex_bytes(value: &str, what: &str) -> Result<Vec<u8>, HyperlaneMidnightError> {
    hex::decode(value.trim_start_matches("0x")).map_err(|e| {
        HyperlaneMidnightError::IndexerGraphql(format!("{what} is not valid hex: {e}"))
    })
}

/// Decode a `HexEncoded` scalar into an exact 32-byte hash.
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

/// Decode a `HexEncoded` scalar into a fixed-width buffer, right-padding with
/// zeros. The indexer already re-pads Misc names/payloads to their declared
/// widths, so the pad is defensive; only an over-long value is an error.
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

/// Convert raw `contractEvents` rows into [`MiscEvent`]s, applying the
/// status filter ([`TxStatus::is_indexable`]). Rows that are not
/// `MiscContractEvent` are skipped defensively — the filter always narrows to
/// `types: [MISC]`, so none are expected. A transaction kind without a
/// `transactionResult` cannot be a contract call, so such rows are skipped
/// too (contract events only come from `RegularTransaction`s).
fn misc_events_from_raw(
    raw: Vec<RawContractEvent>,
) -> Result<Vec<MiscEvent>, HyperlaneMidnightError> {
    let mut out = Vec::with_capacity(raw.len());
    for event in raw {
        let (Some(name), Some(payload)) = (event.name.as_deref(), event.payload.as_deref()) else {
            // Not a MiscContractEvent (no inline-fragment fields).
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
        // THE status decision lives in `TxStatus::is_indexable`.
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

    /// Parse the committed `contractEvents` GraphQL response fixture through
    /// the exact serde structs + conversion the client uses. The fixture has
    /// five Misc events: two SUCCESS dispatches, a SUCCESS gas payment, a
    /// PARTIAL_SUCCESS dispatch, and a FAILURE dispatch — so it pins the
    /// status filter (FAILURE dropped, PARTIAL_SUCCESS kept for now), the
    /// zero-padded name/payload widths, `0x`-prefixed and bare hex hashes,
    /// and the tx/block metadata mapping.
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
        // The FAILURE event (id 27) is dropped; PARTIAL_SUCCESS (id 19) kept.
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

        // The PARTIAL_SUCCESS dispatch is included (see TxStatus::is_indexable).
        assert_eq!(events[2].tx_status, TxStatus::PartialSuccess);

        // The HYP_PROCESS event's hashes are 0x-prefixed in the fixture.
        let process = &events[3];
        assert_eq!(process.tx_hash, H256::repeat_byte(0x03));
        assert_eq!(process.block_hash, H256::repeat_byte(0xa3));
    }

    #[test]
    fn tx_status_filter_is_the_single_decision_point() {
        assert!(TxStatus::Success.is_indexable());
        // PARTIAL_SUCCESS is included FOR NOW; segment attribution is being
        // verified separately (devnet probe). Flip in `is_indexable`.
        assert!(TxStatus::PartialSuccess.is_indexable());
        assert!(!TxStatus::Failure.is_indexable());
    }

    #[test]
    fn hex_padded_pads_short_and_rejects_long() {
        // Short values right-pad with zeros (defensive; the indexer re-pads).
        let padded = hex_padded("aabb", 4, "test").expect("pad");
        assert_eq!(padded, vec![0xaa, 0xbb, 0x00, 0x00]);
        // Over-long values are an error, not a truncation.
        assert!(hex_padded("aabbccddee", 4, "test").is_err());
    }

    /// Parse a `transactions(offset: {hash})` response, including the
    /// RegularTransaction-only `transactionResult` + `fee` fields.
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
        assert_eq!(
            ism.module_type, 5,
            "module_type should be MessageIdMultisig"
        );
        // Decoded slot count must match the validators map.
        assert_eq!(
            ism.validator_count as usize,
            ism.validators.len(),
            "validator_count must match decoded validator slots"
        );
        // A usable multisig: >= 1 validator and 1 <= threshold <= count.
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

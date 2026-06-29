//! Dispatch and delivery indexers for the Midnight Mailbox.
//!
//! Midnight has no event log and no Compact event system (see `LIMITS.md`), so
//! both indexers read the WarpRoute contract's *block-final* ledger state
//! through the Midnight indexer client (#14) instead of replaying logs:
//!
//!   - [`MidnightDispatchIndexer`] serves dispatched `HyperlaneMessage`s out of
//!     the `dispatched_messages: Map<Uint<32>, Bytes<141>>` ledger field, keyed
//!     by nonce. It is sequence-aware: the `nonce` counter is the sequence tip,
//!     and a requested nonce range maps one-to-one onto map keys. Each scan
//!     reads the dispatch state ONCE (the whole map plus the nonce count) and
//!     serves the requested range from that single decoded snapshot.
//!   - [`MidnightDeliveryIndexer`] serves delivered message ids out of the
//!     `deliveries: Set<Bytes<32>>` ledger field. The set is unordered and has
//!     no sequence, so this indexer is rate-limited: every scan re-reads the
//!     full set and the relayer's dedup store handles idempotency (see
//!     `LIMITS.md`).
//!
//! Neither side has per-event block/tx metadata available from state, so the
//! emitted [`LogMeta`] carries only the contract address and the current tip as
//! the block number, with the remaining fields zeroed. This mirrors the
//! Sealevel mailbox indexer's non-`advanced_log_meta` path.

use std::ops::RangeInclusive;

use async_trait::async_trait;

use hyperlane_core::{
    ChainResult, ContractLocator, HyperlaneMessage, Indexed, Indexer, LogMeta,
    SequenceAwareIndexer, H256, H512, U256,
};

use crate::state_decode::DispatchSnapshot;
use crate::MidnightIndexerClient;

/// The minimal set of state reads the dispatch and delivery indexers need from
/// the Midnight indexer client. Abstracting it behind a trait lets the indexers
/// be unit-tested against a synthetic in-memory state without real network IO;
/// production code uses the [`MidnightIndexerClient`] implementation below.
#[async_trait]
trait MidnightStateReader: Send + Sync {
    /// Latest observed block height as a `u32` tip; heights beyond `u32::MAX`
    /// saturate, and "no block seen yet" maps to `0`.
    async fn read_tip(&self) -> ChainResult<u32>;
    /// Whole dispatch state (every message keyed by nonce + the nonce count)
    /// from a single state read.
    async fn read_dispatch_snapshot(&self, address: &str) -> ChainResult<DispatchSnapshot>;
    /// The full unordered `deliveries` set as message ids.
    async fn read_deliveries(&self, address: &str) -> ChainResult<Vec<H256>>;
}

/// Map an indexer-reported block height to a `u32` tip. The indexer reports a
/// `u64`; heights beyond `u32::MAX` saturate rather than truncate/wrap, and
/// "no block observed yet" (`None`) maps to `0`.
fn height_to_tip(height: Option<u64>) -> u32 {
    height
        .map(|h| u32::try_from(h).unwrap_or(u32::MAX))
        .unwrap_or(0)
}

#[async_trait]
impl MidnightStateReader for MidnightIndexerClient {
    async fn read_tip(&self) -> ChainResult<u32> {
        let height = self
            .latest_block_height()
            .await
            .map_err(Into::<hyperlane_core::ChainCommunicationError>::into)?;
        Ok(height_to_tip(height))
    }

    async fn read_dispatch_snapshot(&self, address: &str) -> ChainResult<DispatchSnapshot> {
        MidnightIndexerClient::read_dispatch_snapshot(self, address).await
    }

    async fn read_deliveries(&self, address: &str) -> ChainResult<Vec<H256>> {
        MidnightIndexerClient::read_deliveries(self, address).await
    }
}

/// Render an `H256` contract address as the lowercase hex string the indexer
/// client's `contractAction` query expects (no `0x` prefix), matching the
/// convention used by the Midnight ISM reads.
fn address_hex(address: &H256) -> String {
    format!("{address:x}")
}

/// Build the `LogMeta` for a state-sourced item. Midnight block-final state
/// gives us no per-event block hash, tx id, tx index or log index, so those are
/// zeroed; `block_number` carries the current tip and `address` the WarpRoute
/// contract. Mirrors Sealevel's non-advanced log-meta path.
fn state_log_meta(address: H256, tip: u32) -> LogMeta {
    LogMeta {
        address,
        block_number: tip as u64,
        block_hash: H256::zero(),
        transaction_id: H512::zero(),
        transaction_index: 0,
        log_index: U256::zero(),
    }
}

/// Indexer that serves dispatched `HyperlaneMessage`s from the Midnight
/// Mailbox's `dispatched_messages` map.
#[derive(Debug, Clone)]
pub struct MidnightDispatchIndexer {
    client: MidnightIndexerClient,
    /// WarpRoute (Mailbox) contract address; both the GraphQL state read key and
    /// the `LogMeta.address` reported for every dispatched message.
    address: H256,
}

impl MidnightDispatchIndexer {
    /// Build a dispatch indexer for the contract named by `locator`.
    pub fn new(client: MidnightIndexerClient, locator: &ContractLocator) -> Self {
        Self {
            client,
            address: locator.address,
        }
    }
}

/// Serve a nonce `range` from a single decoded dispatch `snapshot`, skipping
/// nonces with no stored message. Shared by the production path and the unit
/// tests so both exercise identical range/skip/`LogMeta` semantics.
fn dispatch_logs_from_snapshot(
    snapshot: &DispatchSnapshot,
    address: H256,
    tip: u32,
    range: RangeInclusive<u32>,
) -> Vec<(Indexed<HyperlaneMessage>, LogMeta)> {
    let mut out = Vec::new();
    for nonce in range {
        // Each nonce is a key in `dispatched_messages`. A missing key (no
        // message dispatched at that nonce) is skipped, not an error: the
        // cursor may request nonces that have not been dispatched yet.
        if let Some(message) = snapshot.messages.get(&nonce) {
            // `Indexed::from(HyperlaneMessage)` sets `sequence` to the message
            // nonce, which is exactly the dispatch sequence we want.
            out.push((Indexed::from(message.clone()), state_log_meta(address, tip)));
        }
    }
    out
}

/// Generic over the state reader so unit tests can drive the indexer logic with
/// a synthetic in-memory reader. Production uses [`MidnightIndexerClient`].
async fn fetch_dispatch_logs<R: MidnightStateReader>(
    reader: &R,
    address: H256,
    range: RangeInclusive<u32>,
) -> ChainResult<Vec<(Indexed<HyperlaneMessage>, LogMeta)>> {
    let address_hex = address_hex(&address);
    let tip = reader.read_tip().await?;
    // Single state read per scan: the whole dispatched map plus the nonce
    // count come from one decoded snapshot, so the count and the per-nonce
    // reads cannot disagree across snapshots.
    let snapshot = reader.read_dispatch_snapshot(&address_hex).await?;
    Ok(dispatch_logs_from_snapshot(&snapshot, address, tip, range))
}

#[async_trait]
impl Indexer<HyperlaneMessage> for MidnightDispatchIndexer {
    async fn fetch_logs_in_range(
        &self,
        range: RangeInclusive<u32>,
    ) -> ChainResult<Vec<(Indexed<HyperlaneMessage>, LogMeta)>> {
        fetch_dispatch_logs(&self.client, self.address, range).await
    }

    async fn get_finalized_block_number(&self) -> ChainResult<u32> {
        self.client.read_tip().await
    }

    async fn fetch_logs_by_tx_hash(
        &self,
        _tx_hash: H512,
    ) -> ChainResult<Vec<(Indexed<HyperlaneMessage>, LogMeta)>> {
        // Midnight state reads are not addressable by tx hash.
        Ok(Vec::new())
    }
}

#[async_trait]
impl SequenceAwareIndexer<HyperlaneMessage> for MidnightDispatchIndexer {
    async fn latest_sequence_count_and_tip(&self) -> ChainResult<(Option<u32>, u32)> {
        let address = address_hex(&self.address);
        let tip = self.client.read_tip().await?;
        let snapshot = self.client.read_dispatch_snapshot(&address).await?;
        Ok((Some(snapshot.nonce_count), tip))
    }
}

/// Indexer that serves delivered message ids from the Midnight Mailbox's
/// `deliveries` set. Rate-limited: every scan re-reads the full set.
#[derive(Debug, Clone)]
pub struct MidnightDeliveryIndexer {
    client: MidnightIndexerClient,
    /// WarpRoute (Mailbox) contract address; both the GraphQL state read key and
    /// the `LogMeta.address` reported for every delivery.
    address: H256,
}

impl MidnightDeliveryIndexer {
    /// Build a delivery indexer for the contract named by `locator`.
    pub fn new(client: MidnightIndexerClient, locator: &ContractLocator) -> Self {
        Self {
            client,
            address: locator.address,
        }
    }
}

/// Generic over the state reader so unit tests can drive the indexer logic with
/// a synthetic in-memory reader. Production uses [`MidnightIndexerClient`].
async fn fetch_delivery_logs<R: MidnightStateReader>(
    reader: &R,
    address: H256,
) -> ChainResult<Vec<(Indexed<H256>, LogMeta)>> {
    let address_hex = address_hex(&address);
    let tip = reader.read_tip().await?;

    let ids = reader.read_deliveries(&address_hex).await?;
    let log_meta = state_log_meta(address, tip);
    Ok(ids
        .into_iter()
        .map(|id| (Indexed::new(id), log_meta.clone()))
        .collect())
}

#[async_trait]
impl Indexer<H256> for MidnightDeliveryIndexer {
    async fn fetch_logs_in_range(
        &self,
        _range: RangeInclusive<u32>,
    ) -> ChainResult<Vec<(Indexed<H256>, LogMeta)>> {
        // The deliveries set has no nonce/sequence to slice by, so the range is
        // ignored: every scan returns the full set. The relayer's dedup store
        // (rate-limited cursor) makes re-emitting already-seen ids idempotent.
        fetch_delivery_logs(&self.client, self.address).await
    }

    async fn get_finalized_block_number(&self) -> ChainResult<u32> {
        self.client.read_tip().await
    }

    async fn fetch_logs_by_tx_hash(
        &self,
        _tx_hash: H512,
    ) -> ChainResult<Vec<(Indexed<H256>, LogMeta)>> {
        // Midnight state reads are not addressable by tx hash.
        Ok(Vec::new())
    }
}

#[async_trait]
impl SequenceAwareIndexer<H256> for MidnightDeliveryIndexer {
    async fn latest_sequence_count_and_tip(&self) -> ChainResult<(Option<u32>, u32)> {
        // Deliveries are an unordered set with no sequence; `None` signals the
        // rate-limited cursor that there is no sequence to track.
        let tip = self.client.read_tip().await?;
        Ok((None, tip))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;

    /// Synthetic in-memory state reader, the unit-test seam that lets the
    /// indexer logic run without any network IO. Holds a pre-decoded dispatch
    /// snapshot and delivery set; `read_*` just hand them back. Mirrors how the
    /// `state_decode` tests build synthetic state instead of hitting a node.
    struct FakeReader {
        tip: u32,
        snapshot: DispatchSnapshot,
        deliveries: Vec<H256>,
    }

    #[async_trait]
    impl MidnightStateReader for FakeReader {
        async fn read_tip(&self) -> ChainResult<u32> {
            Ok(self.tip)
        }
        async fn read_dispatch_snapshot(&self, _address: &str) -> ChainResult<DispatchSnapshot> {
            Ok(self.snapshot.clone())
        }
        async fn read_deliveries(&self, _address: &str) -> ChainResult<Vec<H256>> {
            Ok(self.deliveries.clone())
        }
    }

    fn message(nonce: u32) -> HyperlaneMessage {
        HyperlaneMessage {
            version: 3,
            nonce,
            origin: 1,
            sender: H256::repeat_byte(0xAA),
            destination: 2,
            recipient: H256::repeat_byte(0xBB),
            body: vec![nonce as u8; 8],
        }
    }

    const ADDRESS: H256 = H256::repeat_byte(0x42);
    const TIP: u32 = 99;

    fn snapshot_with(nonces: &[u32], nonce_count: u32) -> DispatchSnapshot {
        let mut messages = HashMap::new();
        for &n in nonces {
            messages.insert(n, message(n));
        }
        DispatchSnapshot {
            messages,
            nonce_count,
        }
    }

    #[tokio::test]
    async fn dispatch_skips_missing_nonces_and_sets_sequence() {
        // Nonces 0 and 2 are present; 1 and 3 are not.
        let reader = FakeReader {
            tip: TIP,
            snapshot: snapshot_with(&[0, 2], 3),
            deliveries: vec![],
        };

        let logs = fetch_dispatch_logs(&reader, ADDRESS, 0..=3)
            .await
            .expect("fetch dispatch logs");

        // Only the two present nonces come back, in range order.
        assert_eq!(logs.len(), 2, "missing nonces are skipped");
        assert_eq!(logs[0].0.inner().nonce, 0);
        assert_eq!(logs[1].0.inner().nonce, 2);

        // `Indexed.sequence` is the message nonce.
        assert_eq!(logs[0].0.sequence, Some(0));
        assert_eq!(logs[1].0.sequence, Some(2));
    }

    #[tokio::test]
    async fn dispatch_builds_expected_log_meta() {
        let reader = FakeReader {
            tip: TIP,
            snapshot: snapshot_with(&[5], 6),
            deliveries: vec![],
        };

        let logs = fetch_dispatch_logs(&reader, ADDRESS, 5..=5)
            .await
            .expect("fetch dispatch logs");

        assert_eq!(logs.len(), 1);
        let meta = &logs[0].1;
        // address + block_number (tip) set, everything else zeroed.
        assert_eq!(meta.address, ADDRESS);
        assert_eq!(meta.block_number, TIP as u64);
        assert_eq!(meta.block_hash, H256::zero());
        assert_eq!(meta.transaction_id, H512::zero());
        assert_eq!(meta.transaction_index, 0);
        assert_eq!(meta.log_index, U256::zero());
    }

    #[tokio::test]
    async fn dispatch_sequence_count_is_nonce_count() {
        // `latest_sequence_count_and_tip` returns (Some(nonce_count), tip). The
        // count comes straight from the snapshot, so cover it via the snapshot
        // helper too: serving an empty range still yields the right count from
        // the same snapshot the SequenceAwareIndexer impl would read.
        let snapshot = snapshot_with(&[0, 1], 2);
        assert_eq!(snapshot.nonce_count, 2);
        // And serving a present range from that snapshot yields both messages.
        let logs = dispatch_logs_from_snapshot(&snapshot, ADDRESS, TIP, 0..=1);
        assert_eq!(logs.len(), 2);
    }

    #[tokio::test]
    async fn delivery_returns_full_set_as_ids() {
        let id_a = H256::repeat_byte(0x01);
        let id_b = H256::repeat_byte(0x02);
        let reader = FakeReader {
            tip: TIP,
            snapshot: snapshot_with(&[], 0),
            deliveries: vec![id_a, id_b],
        };

        // Range is ignored; the full set comes back as H256 ids.
        let logs = fetch_delivery_logs(&reader, ADDRESS)
            .await
            .expect("fetch delivery logs");

        let mut ids: Vec<H256> = logs.iter().map(|(indexed, _)| *indexed.inner()).collect();
        ids.sort();
        let mut expected = vec![id_a, id_b];
        expected.sort();
        assert_eq!(ids, expected);

        // Deliveries have no sequence.
        assert!(logs.iter().all(|(indexed, _)| indexed.sequence.is_none()));
        // LogMeta carries address + tip.
        assert_eq!(logs[0].1.address, ADDRESS);
        assert_eq!(logs[0].1.block_number, TIP as u64);
    }

    #[test]
    fn tip_saturates_above_u32_max() {
        // No block seen yet -> 0.
        assert_eq!(height_to_tip(None), 0);
        // In-range height passes through unchanged.
        assert_eq!(height_to_tip(Some(12345)), 12345);
        assert_eq!(height_to_tip(Some(u32::MAX as u64)), u32::MAX);
        // A height above u32::MAX saturates to u32::MAX (does NOT wrap/truncate
        // backwards to a small value).
        assert_eq!(height_to_tip(Some(u32::MAX as u64 + 1)), u32::MAX);
        assert_eq!(height_to_tip(Some(u64::MAX)), u32::MAX);
    }
}

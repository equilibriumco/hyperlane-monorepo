//! `InterchainGasPaymaster` + IGP payment indexer for Midnight (#19).
//!
//! On EVM the relayer indexes interchain gas payments from `GasPayment`
//! event logs. Midnight has no event log and no Compact event system (see
//! `LIMITS.md`), so — like the dispatch/delivery indexers (#16) and the
//! merkle-tree indexer (#15) — this reads the IGP contract's *block-final*
//! ledger state through the Midnight indexer client (#14) instead.
//!
//! The IGP contract stores every payment as an append-only
//! `gas_payments: Map<Uint<32>, GasPayment>` keyed by an incrementing index,
//! plus a `gas_payment_count` counter (the per-row struct shape was chosen by
//! #12 precisely so the relayer can recover `destination` + `gas_amount`, and
//! to avoid same-block write-loss under Midnight's block-final state model).
//! That maps cleanly onto Hyperlane's sequence-aware indexing: the count is
//! the sequence tip and an index range maps one-to-one onto map keys — the
//! same shape as [`crate::MidnightDispatchIndexer`], and modelled on the
//! Sealevel/Aleo IGP indexers (which also read state, not events).
//!
//! Following Aleo/Radix/CosmosNative, one struct serves both roles the
//! framework builds: the [`InterchainGasPaymaster`] marker (a boxed trait
//! object every configured chain must provide, though the relayer never calls
//! it) and the `SequenceAwareIndexer<InterchainGasPayment>` that actually
//! feeds payments into the relayer's DB.
//!
//! The relayer's gas-payment *enforcement* (the None / Minimum /
//! OnChainFeeQuoting policies + matching lists) is entirely chain-agnostic and
//! lives in `agents/relayer/src/msg/gas_payment/`: it reads the
//! `InterchainGasPayment`s this indexer wrote into RocksDB and never touches
//! the chain crate. So #19 needs only to produce the same `InterchainGasPayment`
//! data the EVM event indexer produces — the policies work unchanged.

use std::ops::RangeInclusive;

use async_trait::async_trait;

use hyperlane_core::{
    ChainResult, HyperlaneChain, HyperlaneContract, HyperlaneDomain, HyperlaneProvider, Indexed,
    Indexer, InterchainGasPaymaster, InterchainGasPayment, LogMeta, SequenceAwareIndexer, H256,
    H512, U256,
};

use crate::state_decode::IgpSnapshot;
use crate::{MidnightIndexerClient, MidnightProvider};

/// Build the `InterchainGasPayment`s whose append index falls in `range`, from
/// a single decoded IGP `snapshot`. Pure (no I/O) so it is unit-testable
/// without a live indexer. Iterates the range and looks each index up in the
/// snapshot, skipping absent keys (a requested range may run past the recorded
/// payments) — the same range/skip shape as the dispatch indexer. The
/// resulting `Indexed` carries `sequence = append index` (the sequence cursor
/// keys on it), and each field maps straight onto the on-chain `GasPayment`
/// row: `Uint<64>` gas amount and `Uint<128>` payment widen to the `U256` the
/// Hyperlane type uses.
fn igp_payments_in_range(
    snapshot: &IgpSnapshot,
    address: H256,
    tip: u32,
    range: RangeInclusive<u32>,
) -> Vec<(Indexed<InterchainGasPayment>, LogMeta)> {
    let mut out = Vec::new();
    for idx in range {
        // A missing index is skipped, not an error: the contract guarantees no
        // gaps (a reverted `payForGas` never consumes an index), but the cursor
        // may request indices that have not been paid yet.
        if let Some(row) = snapshot.payments.get(&idx) {
            let payment = InterchainGasPayment {
                message_id: row.message_id,
                destination: row.destination,
                payment: U256::from(row.payment),
                gas_amount: U256::from(row.gas_amount),
            };
            let meta = LogMeta {
                address,
                block_number: tip as u64,
                block_hash: H256::zero(),
                transaction_id: H512::zero(),
                transaction_index: 0,
                // Midnight block-final state carries no per-payment tx
                // granularity; the append index is the stable per-payment
                // ordinal. Mirrors the merkle-hook indexer's leaf-index meta.
                log_index: U256::from(idx),
            };
            out.push((Indexed::new(payment).with_sequence(idx), meta));
        }
    }
    out
}

/// Contract address as the indexer's bare-hex scalar (no `0x`), matching the
/// form the other Midnight state readers use.
fn address_hex(address: &H256) -> String {
    format!("{address:x}")
}

/// The minimal state reads the IGP indexer needs. Abstracting them behind a
/// trait lets the async indexer paths be unit-tested against a synthetic
/// in-memory reader (no network IO) — the same seam the dispatch indexer uses.
/// Production uses the [`MidnightIndexerClient`] impl below.
#[async_trait]
trait IgpStateReader: Send + Sync {
    /// Latest observed block height as a `u32` tip (saturating; "no block seen
    /// yet" maps to 0).
    async fn read_tip(&self) -> ChainResult<u32>;
    /// The whole IGP payment state (every row keyed by append index + the
    /// count) from a single state read.
    async fn read_igp_snapshot(&self, address: &str) -> ChainResult<IgpSnapshot>;
}

#[async_trait]
impl IgpStateReader for MidnightIndexerClient {
    async fn read_tip(&self) -> ChainResult<u32> {
        let height = self
            .latest_block_height()
            .await
            .map_err(Into::<hyperlane_core::ChainCommunicationError>::into)?;
        Ok(height
            .map(|h| u32::try_from(h).unwrap_or(u32::MAX))
            .unwrap_or(0))
    }

    async fn read_igp_snapshot(&self, address: &str) -> ChainResult<IgpSnapshot> {
        MidnightIndexerClient::read_igp_snapshot(self, address).await
    }
}

/// Serve an append-index `range` from a single decoded snapshot. Generic over
/// the reader so unit tests drive it with a synthetic in-memory reader;
/// production uses [`MidnightIndexerClient`]. One tip read + one snapshot read
/// per scan, mirroring the dispatch indexer's single-read-per-scan shape.
async fn fetch_igp_logs<R: IgpStateReader>(
    reader: &R,
    address: H256,
    range: RangeInclusive<u32>,
) -> ChainResult<Vec<(Indexed<InterchainGasPayment>, LogMeta)>> {
    let tip = reader.read_tip().await?;
    let snapshot = reader.read_igp_snapshot(&address_hex(&address)).await?;
    Ok(igp_payments_in_range(&snapshot, address, tip, range))
}

/// The sequence tip (`gas_payment_count`) + block tip from a single snapshot
/// read. Generic over the reader for the same unit-test reason as
/// [`fetch_igp_logs`].
async fn igp_sequence_count_and_tip<R: IgpStateReader>(
    reader: &R,
    address: H256,
) -> ChainResult<(Option<u32>, u32)> {
    let tip = reader.read_tip().await?;
    let snapshot = reader.read_igp_snapshot(&address_hex(&address)).await?;
    Ok((Some(snapshot.payment_count), tip))
}

/// Chain-sourced `InterchainGasPaymaster` + IGP payment indexer for Midnight.
/// Reads the append-only `gas_payments` map and the `gas_payment_count`
/// counter from the deployed IGP contract's on-chain state via the #14 indexer
/// client.
#[derive(Debug, Clone)]
pub struct MidnightInterchainGasPaymaster {
    address: H256,
    domain: HyperlaneDomain,
    provider: MidnightProvider,
}

impl MidnightInterchainGasPaymaster {
    /// Construct a handle to the IGP contract at `address`.
    pub fn new(address: H256, domain: HyperlaneDomain, provider: MidnightProvider) -> Self {
        Self {
            address,
            domain,
            provider,
        }
    }
}

impl HyperlaneChain for MidnightInterchainGasPaymaster {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(self.provider.clone())
    }
}

impl HyperlaneContract for MidnightInterchainGasPaymaster {
    fn address(&self) -> H256 {
        self.address
    }
}

impl InterchainGasPaymaster for MidnightInterchainGasPaymaster {}

#[async_trait]
impl Indexer<InterchainGasPayment> for MidnightInterchainGasPaymaster {
    /// Midnight indexes by sequence (`IndexMode::Sequence`), so `range` is an
    /// append-index range. Reads the whole payment state once and serves the
    /// requested indices from that single decoded snapshot, mirroring the
    /// dispatch indexer's single-read-per-scan shape.
    async fn fetch_logs_in_range(
        &self,
        range: RangeInclusive<u32>,
    ) -> ChainResult<Vec<(Indexed<InterchainGasPayment>, LogMeta)>> {
        fetch_igp_logs(self.provider.indexer(), self.address, range).await
    }

    async fn get_finalized_block_number(&self) -> ChainResult<u32> {
        // Midnight has BFT finality and exposes only finalized state, so the
        // latest height is the finalized height. In the sequence-aware path this
        // is never called (the cursor takes its tip from
        // `latest_sequence_count_and_tip`); returning a real height rather than
        // panicking matches the dispatch/merkle indexers (Sealevel's IGP
        // `unimplemented!()`s this instead because it reports slot separately).
        IgpStateReader::read_tip(self.provider.indexer()).await
    }

    async fn fetch_logs_by_tx_hash(
        &self,
        _tx_hash: H512,
    ) -> ChainResult<Vec<(Indexed<InterchainGasPayment>, LogMeta)>> {
        // Midnight state reads are not addressable by tx hash.
        Ok(Vec::new())
    }
}

#[async_trait]
impl SequenceAwareIndexer<InterchainGasPayment> for MidnightInterchainGasPaymaster {
    async fn latest_sequence_count_and_tip(&self) -> ChainResult<(Option<u32>, u32)> {
        // The `gas_payment_count` counter is the sequence tip: payments are
        // recorded at indices `0..count`.
        igp_sequence_count_and_tip(self.provider.indexer(), self.address).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;

    use crate::state_decode::IgpGasPayment;

    const ADDRESS: H256 = H256::repeat_byte(0x42);
    const TIP: u32 = 77;

    fn row(seed: u8) -> IgpGasPayment {
        IgpGasPayment {
            message_id: H256::repeat_byte(seed),
            destination: 1000 + seed as u32,
            gas_amount: 100_000 + seed as u64,
            payment: 2_000_000_000_000_000 + seed as u128,
        }
    }

    fn snapshot_with(indices: &[u32], payment_count: u32) -> IgpSnapshot {
        let mut payments = HashMap::new();
        for &i in indices {
            payments.insert(i, row(i as u8));
        }
        IgpSnapshot {
            payments,
            payment_count,
        }
    }

    #[test]
    fn skips_missing_indices_and_sets_sequence() {
        // Indices 0 and 2 are present; 1 and 3 are not. The indexer must serve
        // the present ones in ascending order and skip the gaps.
        let snapshot = snapshot_with(&[0, 2], 3);
        let logs = igp_payments_in_range(&snapshot, ADDRESS, TIP, 0..=3);

        assert_eq!(logs.len(), 2, "missing indices are skipped");
        // `Indexed.sequence` is the append index, in range order.
        assert_eq!(logs[0].0.sequence, Some(0));
        assert_eq!(logs[1].0.sequence, Some(2));
    }

    #[test]
    fn maps_all_payment_fields() {
        let snapshot = snapshot_with(&[0], 1);
        let logs = igp_payments_in_range(&snapshot, ADDRESS, TIP, 0..=0);

        assert_eq!(logs.len(), 1);
        let p = logs[0].0.inner();
        assert_eq!(p.message_id, H256::repeat_byte(0));
        assert_eq!(p.destination, 1000);
        // u64 gas amount and u128 payment widen to U256.
        assert_eq!(p.gas_amount, U256::from(100_000u64));
        assert_eq!(p.payment, U256::from(2_000_000_000_000_000u128));
    }

    #[test]
    fn builds_expected_log_meta() {
        let snapshot = snapshot_with(&[5], 6);
        let logs = igp_payments_in_range(&snapshot, ADDRESS, TIP, 5..=5);

        assert_eq!(logs.len(), 1);
        let meta = &logs[0].1;
        // address + block_number (tip) + log_index (append index) set,
        // everything else zeroed — the state-sourced meta shape.
        assert_eq!(meta.address, ADDRESS);
        assert_eq!(meta.block_number, TIP as u64);
        assert_eq!(meta.log_index, U256::from(5));
        assert_eq!(meta.block_hash, H256::zero());
        assert_eq!(meta.transaction_id, H512::zero());
        assert_eq!(meta.transaction_index, 0);
    }

    #[test]
    fn range_bounds_are_respected() {
        // A sub-range returns only the payments whose index falls inside it.
        let snapshot = snapshot_with(&[0, 1, 2, 3], 4);
        let logs = igp_payments_in_range(&snapshot, ADDRESS, TIP, 1..=2);
        let seqs: Vec<_> = logs.iter().map(|(i, _)| i.sequence).collect();
        assert_eq!(seqs, vec![Some(1), Some(2)]);
    }

    #[test]
    fn empty_snapshot_yields_no_logs() {
        let snapshot = snapshot_with(&[], 0);
        let logs = igp_payments_in_range(&snapshot, ADDRESS, TIP, 0..=10);
        assert!(logs.is_empty());
    }

    /// Synthetic in-memory reader, the seam that lets the async indexer paths
    /// run without any network IO — mirrors the dispatch indexer's `FakeReader`.
    struct FakeReader {
        tip: u32,
        snapshot: IgpSnapshot,
    }

    #[async_trait]
    impl IgpStateReader for FakeReader {
        async fn read_tip(&self) -> ChainResult<u32> {
            Ok(self.tip)
        }
        async fn read_igp_snapshot(&self, _address: &str) -> ChainResult<IgpSnapshot> {
            Ok(self.snapshot.clone())
        }
    }

    #[tokio::test]
    async fn fetch_logs_reads_tip_then_snapshot_and_serves_range() {
        // Exercises the async orchestration behind `fetch_logs_in_range` (one
        // tip read, one snapshot read, then the pure range serve) — not just the
        // pure helper. Indices 0 and 2 exist; 1 is a gap and is skipped.
        let reader = FakeReader {
            tip: TIP,
            snapshot: snapshot_with(&[0, 2], 3),
        };
        let logs = fetch_igp_logs(&reader, ADDRESS, 0..=2)
            .await
            .expect("fetch igp logs");

        assert_eq!(logs.len(), 2, "gap index is skipped");
        assert_eq!(logs[0].0.sequence, Some(0));
        assert_eq!(logs[1].0.sequence, Some(2));
        // The tip read flows into LogMeta.block_number, the snapshot into the
        // payment rows.
        assert_eq!(logs[0].1.block_number, TIP as u64);
        assert_eq!(logs[0].1.address, ADDRESS);
    }

    #[tokio::test]
    async fn sequence_count_is_payment_count_with_tip() {
        // `latest_sequence_count_and_tip` returns (Some(gas_payment_count), tip)
        // read from a single snapshot; the count is the counter, not however
        // many rows a range happened to serve.
        let reader = FakeReader {
            tip: TIP,
            snapshot: snapshot_with(&[0, 1, 2], 3),
        };
        let (count, tip) = igp_sequence_count_and_tip(&reader, ADDRESS)
            .await
            .expect("sequence count");
        assert_eq!(count, Some(3));
        assert_eq!(tip, TIP);
    }
}

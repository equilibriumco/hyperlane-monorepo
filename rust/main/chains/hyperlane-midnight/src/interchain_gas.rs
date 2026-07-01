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
use crate::MidnightProvider;

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

    /// Contract address as the indexer's bare-hex scalar (no `0x`), matching
    /// the form the other Midnight state readers use.
    fn address_hex(&self) -> String {
        format!("{:x}", self.address)
    }

    /// Fetch + decode the whole IGP payment state in one read.
    async fn igp_snapshot(&self) -> ChainResult<IgpSnapshot> {
        self.provider
            .indexer()
            .read_igp_snapshot(&self.address_hex())
            .await
    }

    /// Latest indexer block height, narrowed to the `u32` Hyperlane uses for
    /// block numbers. Saturates rather than truncating; Midnight devnet heights
    /// are far below `u32::MAX`.
    async fn latest_height_u32(&self) -> ChainResult<u32> {
        let height = self
            .provider
            .indexer()
            .latest_block_height()
            .await?
            .unwrap_or(0);
        Ok(u32::try_from(height).unwrap_or(u32::MAX))
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
        let tip = self.latest_height_u32().await?;
        let snapshot = self.igp_snapshot().await?;
        Ok(igp_payments_in_range(&snapshot, self.address, tip, range))
    }

    async fn get_finalized_block_number(&self) -> ChainResult<u32> {
        // Midnight has BFT finality and exposes only finalized state, so the
        // latest height is the finalized height. Matches the dispatch/merkle
        // indexers (Sealevel's IGP `unimplemented!()`s this instead because it
        // reports slot separately; Midnight's tip is a plain height).
        self.latest_height_u32().await
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
        let tip = self.latest_height_u32().await?;
        // The `gas_payment_count` counter is the sequence tip: payments are
        // recorded at indices `0..count`.
        let snapshot = self.igp_snapshot().await?;
        Ok((Some(snapshot.payment_count), tip))
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
}

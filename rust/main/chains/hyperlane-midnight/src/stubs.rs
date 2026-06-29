//! No-op stub implementations for traits the relayer requires but Midnight
//! does not yet exercise.
//!
//! The relayer's `OriginFactory::create` requires every chain in
//! `HYP_RELAYCHAINS` to satisfy origin-side traits (ValidatorAnnounce + three
//! `SequenceAwareIndexer`s), regardless of whether messages are ever
//! dispatched FROM that chain. For inbound-only Midnight today, none of these
//! are exercised — but if any one fails, the chain drops out of the
//! `origins` map and the relayer can't wire up destination delivery either
//! (see `relayer::run` — destination message processors look up their DB by
//! `origins.get(dest_domain)`).
//!
//! These stubs let midnight build as origin so destination delivery works.
//! They will be replaced by real impls under:
//!   - #16  Message dispatch indexer + delivery indexer
//!   - #19  Relayer: IGP payment indexer
//!
//! The merkle-tree indexer (#15) is no longer a stub — see
//! [`crate::MidnightMerkleTreeHook`].
//!
//! All depend on #14 (Midnight indexer client) for chain-state reads.
//!
//! ValidatorAnnounce is now implemented (see `validator_announce.rs`).

use std::ops::RangeInclusive;

use async_trait::async_trait;

use hyperlane_core::{
    ChainResult, Indexed, Indexer, InterchainGasPayment, LogMeta, SequenceAwareIndexer, H512,
};

/// Generic stub `SequenceAwareIndexer` for the one origin-side indexer Midnight
/// still stubs (the IGP payment indexer, #19). Returns no events and zero block
/// height — safe because the relayer never has work to do on midnight
/// outbound today. The dispatch (#16) and merkle (#15) indexers are now real.
#[derive(Debug)]
pub struct MidnightStubIndexer<T> {
    _marker: std::marker::PhantomData<T>,
}

impl<T> Default for MidnightStubIndexer<T> {
    fn default() -> Self {
        Self {
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T> MidnightStubIndexer<T> {
    /// Construct a new stub indexer.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl<T> Indexer<T> for MidnightStubIndexer<T>
where
    T: Send + Sync + std::fmt::Debug + 'static,
{
    async fn fetch_logs_in_range(
        &self,
        _range: RangeInclusive<u32>,
    ) -> ChainResult<Vec<(Indexed<T>, LogMeta)>> {
        Ok(Vec::new())
    }

    async fn get_finalized_block_number(&self) -> ChainResult<u32> {
        Ok(0)
    }

    async fn fetch_logs_by_tx_hash(
        &self,
        _tx_hash: H512,
    ) -> ChainResult<Vec<(Indexed<T>, LogMeta)>> {
        Ok(Vec::new())
    }
}

#[async_trait]
impl<T> SequenceAwareIndexer<T> for MidnightStubIndexer<T>
where
    T: Send + Sync + std::fmt::Debug + 'static,
{
    async fn latest_sequence_count_and_tip(&self) -> ChainResult<(Option<u32>, u32)> {
        // (None, 0) signals "no known sequence yet, chain at block 0" — the
        // cursor stays parked and never advances. The relayer never expects
        // to deliver from this chain anyway.
        Ok((None, 0))
    }
}

/// Type-aliased flavor for the one concrete `T` the relayer still stubs.
/// The dispatch indexer (#16) and merkle indexer (#15) are now real
/// (`MidnightDispatchIndexer` / `MidnightMerkleTreeHook`), so only the IGP
/// payment indexer (#19) remains a stub.
/// Stub for IGP payment indexing.
pub type MidnightIgpIndexerStub = MidnightStubIndexer<InterchainGasPayment>;

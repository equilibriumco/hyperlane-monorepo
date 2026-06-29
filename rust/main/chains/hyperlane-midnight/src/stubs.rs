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
//!   - #33  ValidatorAnnounce agent-side trait
//!   - #15  Validator: Merkle tree indexer
//!   - #16  Message dispatch indexer + delivery indexer
//!   - #19  Relayer: IGP payment indexer
//!
//! All depend on #14 (Midnight indexer client) for chain-state reads.

use std::ops::RangeInclusive;

use async_trait::async_trait;

use hyperlane_core::ValidatorAnnounce;
use hyperlane_core::{
    Announcement, ChainResult, HyperlaneChain, HyperlaneContract, HyperlaneDomain,
    HyperlaneProvider, Indexed, Indexer, InterchainGasPayment, LogMeta, MerkleTreeInsertion,
    SequenceAwareIndexer, SignedType, TxOutcome, H256, H512, U256,
};

use crate::MidnightProvider;

/// Stub `ValidatorAnnounce` — Midnight never announces or queries storage
/// locations through the agent-side trait today. Replaced by #33.
#[derive(Debug)]
pub struct MidnightValidatorAnnounceStub {
    address: H256,
    domain: HyperlaneDomain,
    provider: MidnightProvider,
}

impl MidnightValidatorAnnounceStub {
    /// Construct a new stub.
    pub fn new(address: H256, domain: HyperlaneDomain, provider: MidnightProvider) -> Self {
        Self {
            address,
            domain,
            provider,
        }
    }
}

impl HyperlaneChain for MidnightValidatorAnnounceStub {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(self.provider.clone())
    }
}

impl HyperlaneContract for MidnightValidatorAnnounceStub {
    fn address(&self) -> H256 {
        self.address
    }
}

#[async_trait]
impl ValidatorAnnounce for MidnightValidatorAnnounceStub {
    async fn get_announced_storage_locations(
        &self,
        validators: &[H256],
    ) -> ChainResult<Vec<Vec<String>>> {
        // Return one empty list per validator — the relayer treats this as
        // "no announcement on file" and skips that validator at metadata
        // build time, which is correct: midnight-as-origin signing isn't
        // wired up yet.
        Ok(validators.iter().map(|_| Vec::new()).collect())
    }

    async fn announce(&self, _announcement: SignedType<Announcement>) -> ChainResult<TxOutcome> {
        // Validators on midnight don't announce via this code path (the
        // contract is called directly from outside the agent for now). Any
        // caller reaching here on midnight is a bug.
        Err(hyperlane_core::ChainCommunicationError::CustomError(
            "midnight: ValidatorAnnounce::announce is not implemented (see #33)".to_string(),
        ))
    }

    async fn announce_tokens_needed(
        &self,
        _announcement: SignedType<Announcement>,
        _chain_signer: H256,
    ) -> Option<U256> {
        None
    }
}

/// Generic stub `SequenceAwareIndexer` used for all three origin-side
/// indexers (dispatch / merkle / IGP). Returns no events and zero block
/// height — safe because the relayer never has work to do on midnight
/// outbound today. Replaced by #15, #16, #19.
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

/// Type-aliased flavors for the concrete `T`s the relayer still stubs.
/// These exist purely so `chains.rs` can read like `MidnightMerkleTreeIndexerStub::new()`
/// instead of spelling out the generic. The dispatch indexer is now real
/// (`MidnightDispatchIndexer`, #16), so it no longer has a stub alias here.
/// Stub for merkle tree insertion indexing.
pub type MidnightMerkleTreeIndexerStub = MidnightStubIndexer<MerkleTreeInsertion>;
/// Stub for IGP payment indexing.
pub type MidnightIgpIndexerStub = MidnightStubIndexer<InterchainGasPayment>;

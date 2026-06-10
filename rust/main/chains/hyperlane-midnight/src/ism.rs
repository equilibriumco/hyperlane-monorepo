//! `InterchainSecurityModule` + `MultisigIsm` impls for Midnight.
//!
//! These return a hardcoded `ModuleType::MessageIdMultisig` and a
//! `(validators, threshold)` pair read from `ConnectionConf`. The Midnight
//! WarpRoute's on-chain ISM is a `MessageIdMultisigIsm` by design — there's
//! one ISM variant in the protocol today — so the static module type is
//! correct, not a workaround. The validator set + threshold IS a
//! workaround: agent operators must keep the agent config in lockstep with
//! on-chain state.
//!
//! TODO(#14): once the Midnight indexer client gains a point-in-time state
//! reader, read `validators`, `threshold`, **and** module type from chain
//! state at metadata-build time. `validators` and `threshold` should
//! migrate together — set/threshold drift would either soft-brick delivery
//! (relayer signs against the wrong set) or invalidate the on-chain check.
//! `module_type()` should detect the variant from chain state instead of
//! hardcoding, so additional ISM types (e.g. MerkleRootMultisig,
//! AggregationIsm) can be added without code changes.

use async_trait::async_trait;

use hyperlane_core::{
    ChainResult, HyperlaneChain, HyperlaneContract, HyperlaneDomain, HyperlaneMessage,
    HyperlaneProvider, InterchainSecurityModule, Metadata, ModuleType, MultisigIsm, H160, H256,
    U256,
};

use crate::{ConnectionConf, MidnightProvider};

/// Config-sourced `InterchainSecurityModule` for Midnight. Returns
/// `MessageIdMultisig` — the only ISM variant the Midnight WarpRoute
/// supports today.
#[derive(Debug)]
pub struct MidnightInterchainSecurityModule {
    address: H256,
    domain: HyperlaneDomain,
    provider: MidnightProvider,
}

impl MidnightInterchainSecurityModule {
    /// Construct a new ISM handle.
    pub fn new(address: H256, domain: HyperlaneDomain, provider: MidnightProvider) -> Self {
        Self {
            address,
            domain,
            provider,
        }
    }
}

impl HyperlaneChain for MidnightInterchainSecurityModule {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(self.provider.clone())
    }
}

impl HyperlaneContract for MidnightInterchainSecurityModule {
    fn address(&self) -> H256 {
        self.address
    }
}

#[async_trait]
impl InterchainSecurityModule for MidnightInterchainSecurityModule {
    async fn module_type(&self) -> ChainResult<ModuleType> {
        Ok(ModuleType::MessageIdMultisig)
    }

    async fn dry_run_verify(
        &self,
        _message: &HyperlaneMessage,
        _metadata: &Metadata,
    ) -> ChainResult<Option<U256>> {
        // Midnight uses DUST-based fees the wallet computes at submission
        // time (same pattern as `MidnightMailbox::process_estimate_costs`).
        // Returning `None` signals "I can't estimate" — the relayer falls
        // back to its own logic without dry-running, which is fine.
        Ok(None)
    }
}

/// Config-sourced `MultisigIsm` for Midnight.
#[derive(Debug)]
pub struct MidnightMultisigIsm {
    address: H256,
    domain: HyperlaneDomain,
    provider: MidnightProvider,
    validators: Vec<H160>,
    threshold: u8,
}

impl MidnightMultisigIsm {
    /// Construct from `ConnectionConf`'s validator list + threshold.
    pub fn new(
        address: H256,
        domain: HyperlaneDomain,
        provider: MidnightProvider,
        conf: &ConnectionConf,
    ) -> Self {
        Self {
            address,
            domain,
            provider,
            validators: conf.validators.clone(),
            threshold: conf.threshold,
        }
    }
}

impl HyperlaneChain for MidnightMultisigIsm {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(self.provider.clone())
    }
}

impl HyperlaneContract for MidnightMultisigIsm {
    fn address(&self) -> H256 {
        self.address
    }
}

#[async_trait]
impl MultisigIsm for MidnightMultisigIsm {
    async fn validators_and_threshold(
        &self,
        _message: &HyperlaneMessage,
    ) -> ChainResult<(Vec<H256>, u8)> {
        // ETH validator addresses are stored as H160 in ConnectionConf to
        // match the on-chain Midnight contract's `Bytes<20>` field. The
        // multisig metadata pipeline expects H256, so left-pad each
        // address with 12 zero bytes — this matches Ethereum's standard
        // `addressToBytes32` convention used elsewhere in the codebase.
        let padded: Vec<H256> = self
            .validators
            .iter()
            .map(|addr| {
                let mut bytes = [0u8; 32];
                bytes[12..].copy_from_slice(addr.as_bytes());
                H256::from(bytes)
            })
            .collect();
        Ok((padded, self.threshold))
    }
}

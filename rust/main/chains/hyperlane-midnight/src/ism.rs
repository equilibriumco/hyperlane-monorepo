//! `InterchainSecurityModule` + `MultisigIsm` impls for Midnight.
//!
//! Both read from the deployed `night` contract's on-chain state via the
//! indexer (a one-shot `contractAction` HTTP query returning the latest
//! state), decoded by [`crate::state_decode`]. `validators`, `threshold`,
//! and `module_type` all come from one such read, so the relayer always
//! signs against the set the on-chain ISM will check — no config/chain drift.
//!
//! `module_type` is read from chain rather than hardcoded: the Midnight
//! WarpRoute only implements `MessageIdMultisig` today, so any other value
//! means the deployed contract uses verification logic this agent build
//! does not support, and we error rather than guess.

use async_trait::async_trait;

use hyperlane_core::{
    ChainResult, HyperlaneChain, HyperlaneContract, HyperlaneDomain, HyperlaneMessage,
    HyperlaneProvider, InterchainSecurityModule, Metadata, ModuleType, MultisigIsm, H256, U256,
};

use crate::{HyperlaneMidnightError, MidnightProvider};

/// Map the on-chain `module_type` discriminant to a Hyperlane [`ModuleType`].
/// The Midnight WarpRoute only implements `MessageIdMultisig`; any other
/// value means the deployed contract uses verification logic this agent
/// build does not support.
fn module_type_from_u8(value: u8) -> ChainResult<ModuleType> {
    if value == ModuleType::MessageIdMultisig as u8 {
        Ok(ModuleType::MessageIdMultisig)
    } else {
        Err(HyperlaneMidnightError::StateDecode(format!(
            "unsupported on-chain ISM module_type {value}: only {} (MessageIdMultisig) is supported",
            ModuleType::MessageIdMultisig as u8
        ))
        .into())
    }
}

/// Chain-sourced `InterchainSecurityModule` for Midnight. Reads the module
/// type from the deployed contract's on-chain state.
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
        let address = format!("{:x}", self.address);
        let state = self.provider.indexer().read_ism_state(&address).await?;
        module_type_from_u8(state.module_type)
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

/// Chain-sourced `MultisigIsm` for Midnight. Reads validators + threshold
/// from the deployed contract's on-chain state.
#[derive(Debug)]
pub struct MidnightMultisigIsm {
    address: H256,
    domain: HyperlaneDomain,
    provider: MidnightProvider,
}

impl MidnightMultisigIsm {
    /// Construct an ISM handle. Validators + threshold are read from chain
    /// state on each `validators_and_threshold` call, not from config.
    pub fn new(address: H256, domain: HyperlaneDomain, provider: MidnightProvider) -> Self {
        Self {
            address,
            domain,
            provider,
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
        let address = format!("{:x}", self.address);
        let state = self.provider.indexer().read_ism_state(&address).await?;
        // On-chain validators are `Bytes<64>` secp256k1 pubkeys (#22); the
        // state decoder derives each 20-byte ETH address as
        // `keccak256(pubkey)[12..]`. The multisig metadata pipeline expects
        // H256, so left-pad each with 12 zero bytes — Ethereum's standard
        // `addressToBytes32` convention used elsewhere in the codebase.
        // Validators and threshold come from the same single read, so they
        // cannot drift apart.
        let padded: Vec<H256> = state
            .validators
            .iter()
            .map(|addr| {
                let mut bytes = [0u8; 32];
                bytes[12..].copy_from_slice(addr);
                H256::from(bytes)
            })
            .collect();
        Ok((padded, state.threshold))
    }
}

//! `InterchainSecurityModule` + `MultisigIsm` impls for Midnight.
//!
//! Validators, threshold, and module type all come from a single read of the
//! deployed contract's state, so the relayer always signs against the set the
//! on-chain ISM will check and nothing can drift out of config.

use async_trait::async_trait;

use hyperlane_core::{
    ChainResult, HyperlaneChain, HyperlaneContract, HyperlaneDomain, HyperlaneMessage,
    HyperlaneProvider, InterchainSecurityModule, Metadata, ModuleType, MultisigIsm, H256, U256,
};

use crate::{HyperlaneMidnightError, MidnightProvider};

/// The WarpRoute only implements `MessageIdMultisig`, so any other value means
/// the deployed contract verifies in a way this build does not support.
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

/// `InterchainSecurityModule` for Midnight.
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
        // Fees are DUST the wallet computes at submission time, so there is
        // nothing to estimate; `None` lets the relayer fall back to its own
        // logic without dry-running.
        Ok(None)
    }
}

/// `MultisigIsm` for Midnight.
#[derive(Debug)]
pub struct MidnightMultisigIsm {
    address: H256,
    domain: HyperlaneDomain,
    provider: MidnightProvider,
}

impl MidnightMultisigIsm {
    /// Validators and threshold are read from chain state on each
    /// `validators_and_threshold` call, not from config.
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
        // The metadata pipeline wants H256, so left-pad each address with 12
        // zero bytes — the standard `addressToBytes32` convention.
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

use crate::provider::CardanoProvider;
use async_trait::async_trait;

use hyperlane_core::{
    ChainResult, ContractLocator, HyperlaneChain, HyperlaneContract, HyperlaneDomain,
    HyperlaneMessage, HyperlaneProvider, InterchainSecurityModule, ModuleType, H256, U256,
};

/// A reference to an InterchainSecurityModule contract on Cardano
#[derive(Debug)]
pub struct CardanoInterchainSecurityModule {
    domain: HyperlaneDomain,
}

impl CardanoInterchainSecurityModule {
    /// Create a new Cardano InterchainSecurityModule
    pub fn new(locator: ContractLocator) -> Self {
        Self {
            domain: locator.domain.clone(),
        }
    }
}

impl HyperlaneChain for CardanoInterchainSecurityModule {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(CardanoProvider::new(self.domain.clone()))
    }
}

impl HyperlaneContract for CardanoInterchainSecurityModule {
    fn address(&self) -> H256 {
        // ISM on Cardano is a minting policy, not an address
        // We could return the minting policy hash here?
        !todo!()
    }
}

#[async_trait]
impl InterchainSecurityModule for CardanoInterchainSecurityModule {
    async fn module_type(&self) -> ChainResult<ModuleType> {
        // The only supported ISM at the moment.
        Ok(ModuleType::MessageIdMultisig)
    }

    async fn dry_run_verify(
        &self,
        _message: &HyperlaneMessage,
        _metadata: &[u8],
    ) -> ChainResult<Option<U256>> {
        // TODO[cardano]: What does this mean on Cardano?
        Ok(Some(U256::zero()))
    }
}

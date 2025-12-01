use crate::provider::CardanoProvider;
use crate::ConnectionConf;
use async_trait::async_trait;

use hyperlane_core::{
    ChainResult, ContractLocator, HyperlaneChain, HyperlaneContract, HyperlaneDomain,
    HyperlaneMessage, HyperlaneProvider, InterchainSecurityModule, ModuleType, H256, U256,
};

/// A reference to an InterchainSecurityModule contract on Cardano
#[derive(Debug)]
pub struct CardanoInterchainSecurityModule {
    domain: HyperlaneDomain,
    conf: ConnectionConf,
    address: H256,
}

impl CardanoInterchainSecurityModule {
    /// Create a new Cardano InterchainSecurityModule
    pub fn new(conf: &ConnectionConf, locator: ContractLocator) -> Self {
        Self {
            domain: locator.domain.clone(),
            conf: conf.clone(),
            address: locator.address,
        }
    }
}

impl HyperlaneChain for CardanoInterchainSecurityModule {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(CardanoProvider::new(&self.conf, self.domain.clone()))
    }
}

impl HyperlaneContract for CardanoInterchainSecurityModule {
    fn address(&self) -> H256 {
        // On Cardano, this represents the ISM minting policy hash
        self.address
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
        // dry_run_verify is primarily used for aggregation ISMs to estimate gas costs for verification.
        // Since Cardano doesn't support aggregation ISMs yet (only MessageIdMultisig),
        // we return a non-zero placeholder value to indicate successful verification would occur.
        // In Cardano's UTXO model, the actual cost is determined by the transaction fee,
        // which is calculated when building the transaction.
        Ok(Some(U256::one()))
    }
}

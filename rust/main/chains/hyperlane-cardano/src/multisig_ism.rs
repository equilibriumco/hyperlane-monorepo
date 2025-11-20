use std::str::FromStr;

use crate::provider::CardanoProvider;
use crate::rpc::CardanoRpc;
use crate::ConnectionConf;
use async_trait::async_trait;

use hyperlane_core::{
    ChainResult, ContractLocator, HyperlaneChain, HyperlaneContract, HyperlaneDomain,
    HyperlaneMessage, HyperlaneProvider, MultisigIsm, H256,
};

/// MultisigIsm contract on Cardano
#[derive(Debug)]
pub struct CardanoMultisigIsm {
    cardano_rpc: CardanoRpc,
    domain: HyperlaneDomain,
}

impl CardanoMultisigIsm {
    /// Create a new Cardano CardanoMultisigIsm
    pub fn new(conf: &ConnectionConf, locator: ContractLocator) -> Self {
        let cardano_rpc = CardanoRpc::new(&conf.url);
        Self {
            cardano_rpc,
            domain: locator.domain.clone(),
        }
    }
}

impl HyperlaneChain for CardanoMultisigIsm {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(CardanoProvider::new(self.domain.clone()))
    }
}

impl HyperlaneContract for CardanoMultisigIsm {
    fn address(&self) -> H256 {
        // ISM on Cardano is a minting policy, not an address
        // TODO[cadarno]: We could return the minting policy hash here?
        H256::zero()
    }
}

#[async_trait]
impl MultisigIsm for CardanoMultisigIsm {
    /// Returns the validator and threshold needed to verify message
    async fn validators_and_threshold(
        &self,
        message: &HyperlaneMessage,
    ) -> ChainResult<(Vec<H256>, u8)> {
        // We're using the same multisig ISM for all messages
        // TODO[cadarno]: https://github.com/tvl-labs/hyperlane-cardano/issues/42
        // will enable dApp-defined ISM
        let parameters = self.cardano_rpc.get_ism_parameters().await.unwrap();
        let validators = parameters
            .validators
            .iter()
            .map(|v| H256::from_str(v).unwrap())
            .collect();
        Ok((validators, parameters.threshold as u8))
    }
}

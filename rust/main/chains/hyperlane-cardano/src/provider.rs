use async_trait::async_trait;
use hyperlane_core::{
    BlockInfo, ChainInfo, ChainResult, HyperlaneChain, HyperlaneDomain, HyperlaneProvider, TxnInfo, H256, H512, U256,
};

#[derive(Debug)]
pub struct CardanoProvider {
    domain: HyperlaneDomain,
}

impl CardanoProvider {
    pub fn new(domain: HyperlaneDomain) -> Self {
        CardanoProvider { domain }
    }
}

impl HyperlaneChain for CardanoProvider {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(CardanoProvider {
            domain: self.domain.clone(),
        })
    }
}

#[async_trait]
impl HyperlaneProvider for CardanoProvider {
    async fn get_block_by_height(&self, _height: u64) -> ChainResult<BlockInfo> {
        todo!("Cardano get_block_by_height not yet implemented")
    }

    async fn get_txn_by_hash(&self, _hash: &H512) -> ChainResult<TxnInfo> {
        todo!("Cardano get_txn_by_hash not yet implemented")
    }

    async fn is_contract(&self, _address: &H256) -> ChainResult<bool> {
        Ok(true) // TODO[cardano]
    }

    async fn get_balance(&self, _address: String) -> ChainResult<U256> {
        todo!("Cardano balance checking not yet implemented")
    }

    async fn get_chain_metrics(&self) -> ChainResult<Option<ChainInfo>> {
        todo!("Cardano chain metrics not yet implemented")
    }
}

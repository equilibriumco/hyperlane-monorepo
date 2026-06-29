use async_trait::async_trait;

use hyperlane_core::{
    BlockInfo, ChainInfo, ChainResult, HyperlaneChain, HyperlaneDomain, HyperlaneProvider, TxnInfo,
    H256, H512, U256,
};

use crate::{HyperlaneMidnightError, MidnightIndexerClient};

/// Skeleton provider.
#[derive(Debug, Clone)]
pub struct MidnightProvider {
    domain: HyperlaneDomain,
    indexer: MidnightIndexerClient,
}

impl MidnightProvider {
    /// Build a new provider.
    pub fn new(domain: HyperlaneDomain, indexer: MidnightIndexerClient) -> Self {
        Self { domain, indexer }
    }

    /// Borrow the indexer client, for chain-state reads (e.g. the ISM reading
    /// its validators/threshold/module-type from the deployed contract).
    pub fn indexer(&self) -> &MidnightIndexerClient {
        &self.indexer
    }
}

impl HyperlaneChain for MidnightProvider {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(self.clone())
    }
}

#[async_trait]
impl HyperlaneProvider for MidnightProvider {
    async fn get_block_by_height(&self, _height: u64) -> ChainResult<BlockInfo> {
        Err(HyperlaneMidnightError::NotImplemented("get_block_by_height").into())
    }

    async fn get_txn_by_hash(&self, _hash: &H512) -> ChainResult<TxnInfo> {
        Err(HyperlaneMidnightError::NotImplemented("get_txn_by_hash").into())
    }

    async fn is_contract(&self, _address: &H256) -> ChainResult<bool> {
        // Returning false makes `pending_message` drop every inbound message
        // at the `is_recipient_contract` gate. Every routable recipient on
        // Midnight is the monolithic WarpRoute contract, so `true` is correct
        // until non-WarpRoute recipients exist.
        Ok(true)
    }

    async fn get_balance(&self, _address: String) -> ChainResult<U256> {
        Err(HyperlaneMidnightError::NotImplemented("get_balance").into())
    }

    async fn get_chain_metrics(&self) -> ChainResult<Option<ChainInfo>> {
        let _ = self.indexer.latest_block_height().await;
        Ok(None)
    }
}

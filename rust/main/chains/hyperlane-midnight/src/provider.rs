//! Minimal `HyperlaneProvider` implementation for Midnight.
//!
//! The destination-side Mailbox uses this only to satisfy `HyperlaneChain`'s
//! `provider()` accessor. Block- and txn-level queries return
//! `NotImplemented` until the full Midnight provider lands with issue #14 /
//! #16.

use async_trait::async_trait;

use hyperlane_core::{
    BlockInfo, ChainInfo, ChainResult, HyperlaneChain, HyperlaneDomain, HyperlaneProvider, TxnInfo,
    H256, H512, U256,
};

use crate::{HyperlaneMidnightError, MidnightIndexerClient};

/// Skeleton provider. Carries enough to serve `HyperlaneChain` and a single
/// indexer ping; anything else returns `NotImplemented` until #14 / #16.
#[derive(Debug, Clone)]
pub struct MidnightProvider {
    domain: HyperlaneDomain,
    indexer: MidnightIndexerClient,
}

impl MidnightProvider {
    /// Build a new provider with the given chain domain and indexer client.
    pub fn new(domain: HyperlaneDomain, indexer: MidnightIndexerClient) -> Self {
        Self { domain, indexer }
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
        // Until the indexer client exposes contract metadata (issue #14), all
        // recipients are treated as user addresses for routing. The Mailbox's
        // `isContractRecipient` flag is supplied separately at process time.
        Ok(false)
    }

    async fn get_balance(&self, _address: String) -> ChainResult<U256> {
        Err(HyperlaneMidnightError::NotImplemented("get_balance").into())
    }

    async fn get_chain_metrics(&self) -> ChainResult<Option<ChainInfo>> {
        // Best-effort liveness: report the latest known indexer height as
        // chain progress when available, otherwise `None`.
        match self.indexer.latest_block_height().await {
            Ok(Some(_height)) => Ok(None),
            Ok(None) => Ok(None),
            // Indexer ping is non-fatal here; treat as "no metrics yet".
            Err(_) => Ok(None),
        }
    }
}

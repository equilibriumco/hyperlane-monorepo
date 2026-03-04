use async_trait::async_trait;
use hyperlane_core::{
    BlockInfo, ChainCommunicationError, ChainInfo, ChainResult, HyperlaneChain, HyperlaneDomain,
    HyperlaneProvider, TxnInfo, H256, H512, U256,
};
use std::sync::Arc;

use crate::blockfrost_provider::BlockfrostProvider;
use crate::ConnectionConf;

#[derive(Debug, Clone)]
pub struct CardanoProvider {
    domain: HyperlaneDomain,
    provider: Arc<BlockfrostProvider>,
}

impl CardanoProvider {
    pub fn new(conf: &ConnectionConf, domain: HyperlaneDomain) -> Self {
        let provider =
            BlockfrostProvider::new(&conf.api_key, conf.network, conf.confirmation_block_delay);
        CardanoProvider {
            domain,
            provider: Arc::new(provider),
        }
    }

    pub fn blockfrost(&self) -> &BlockfrostProvider {
        &self.provider
    }
}

impl HyperlaneChain for CardanoProvider {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(self.clone())
    }
}

fn to_chain_err(e: impl std::fmt::Display) -> ChainCommunicationError {
    ChainCommunicationError::from_other_str(&e.to_string())
}

fn block_hash_to_h256(hash_hex: &str) -> ChainResult<H256> {
    let bytes = hex::decode(hash_hex).map_err(|e| {
        ChainCommunicationError::from_other_str(&format!(
            "Invalid block hash hex '{hash_hex}': {e}"
        ))
    })?;
    Ok(H256::from_slice(&bytes))
}

#[async_trait]
impl HyperlaneProvider for CardanoProvider {
    async fn get_block_by_height(&self, height: u64) -> ChainResult<BlockInfo> {
        let finalized = self
            .provider
            .get_latest_block()
            .await
            .map_err(to_chain_err)?;

        if height > finalized {
            return Err(ChainCommunicationError::from_other_str(&format!(
                "Block {height} not yet finalized (current: {finalized})"
            )));
        }

        let block = self
            .provider
            .get_block_by_height(height)
            .await
            .map_err(to_chain_err)?;

        Ok(BlockInfo {
            hash: block_hash_to_h256(&block.hash)?,
            timestamp: block.time,
            number: block.height,
        })
    }

    async fn get_txn_by_hash(&self, hash: &H512) -> ChainResult<TxnInfo> {
        let cardano_tx_hash = H256::from_slice(&hash.as_bytes()[0..32]);

        Ok(TxnInfo {
            hash: cardano_tx_hash.into(),
            gas_limit: U256::zero(),
            max_priority_fee_per_gas: None,
            max_fee_per_gas: None,
            gas_price: None,
            nonce: 0,
            sender: H256::zero(),
            recipient: None,
            receipt: None,
            raw_input_data: None,
        })
    }

    async fn is_contract(&self, _address: &H256) -> ChainResult<bool> {
        Ok(true)
    }

    async fn get_balance(&self, address: String) -> ChainResult<U256> {
        let utxos = self
            .provider
            .get_utxos_at_address(&address)
            .await
            .map_err(to_chain_err)?;
        let total_lovelace: u64 = utxos.iter().map(|u| u.lovelace()).sum();
        Ok(U256::from(total_lovelace))
    }

    async fn get_chain_metrics(&self) -> ChainResult<Option<ChainInfo>> {
        let block = self
            .provider
            .get_latest_block_info()
            .await
            .map_err(to_chain_err)?;

        Ok(Some(ChainInfo {
            latest_block: BlockInfo {
                hash: block_hash_to_h256(&block.hash)?,
                timestamp: block.time,
                number: block.height,
            },
            min_gas_price: None,
        }))
    }
}

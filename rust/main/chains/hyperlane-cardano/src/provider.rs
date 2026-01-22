use async_trait::async_trait;
use hyperlane_core::{
    BlockInfo, ChainInfo, ChainResult, HyperlaneChain, HyperlaneDomain, HyperlaneProvider, TxnInfo,
    H256, H512, U256,
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
        let provider = BlockfrostProvider::new(&conf.api_key, conf.network);
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

#[async_trait]
impl HyperlaneProvider for CardanoProvider {
    async fn get_block_by_height(&self, height: u64) -> ChainResult<BlockInfo> {
        // Get the current finalized block to check if height is valid
        let finalized =
            self.provider.get_latest_block().await.map_err(|e| {
                hyperlane_core::ChainCommunicationError::from_other_str(&e.to_string())
            })?;

        if height > finalized {
            return Err(hyperlane_core::ChainCommunicationError::from_other_str(
                &format!(
                    "Block {} not yet finalized (current: {})",
                    height, finalized
                ),
            ));
        }

        Ok(BlockInfo {
            hash: H256::zero(), // Would need to query Blockfrost for actual block hash
            timestamp: 0,       // Would need to query Blockfrost for actual timestamp
            number: height,
        })
    }

    async fn get_txn_by_hash(&self, hash: &H512) -> ChainResult<TxnInfo> {
        // For Cardano, transaction hashes are 32 bytes (H256), but the trait requires H512
        // We'll use the first 32 bytes of the H512 as the Cardano tx hash
        let cardano_tx_hash = H256::from_slice(&hash.as_bytes()[0..32]);

        // Note: This would require extending the Blockfrost provider to support transaction queries
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
        // In Cardano, we can check if an address is a script address
        // by examining the address type. For now, we'll assume all addresses
        // could potentially be scripts (validators or minting policies)
        Ok(true)
    }

    async fn get_balance(&self, address: String) -> ChainResult<U256> {
        // Query UTXOs at the address and sum the ADA balance
        match self.provider.get_utxos_at_address(&address).await {
            Ok(utxos) => {
                let total_lovelace: u64 = utxos.iter().map(|u| u.lovelace()).sum();
                Ok(U256::from(total_lovelace))
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to get balance for Cardano address {}: {}",
                    address,
                    e
                );
                Ok(U256::zero())
            }
        }
    }

    async fn get_chain_metrics(&self) -> ChainResult<Option<ChainInfo>> {
        // Get the current finalized block number as a basic health metric
        let latest_block_number =
            self.provider.get_latest_block().await.map_err(|e| {
                hyperlane_core::ChainCommunicationError::from_other_str(&e.to_string())
            })?;

        Ok(Some(ChainInfo {
            latest_block: BlockInfo {
                hash: H256::zero(), // Would need to query Blockfrost for actual block hash
                timestamp: 0,       // Would need to query Blockfrost for actual timestamp
                number: latest_block_number,
            },
            min_gas_price: None, // Cardano doesn't have a dynamic gas price mechanism like EIP-1559
        }))
    }
}

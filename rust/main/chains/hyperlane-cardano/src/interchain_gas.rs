use crate::blockfrost_provider::BlockfrostProvider;
use crate::ConnectionConf;
use async_trait::async_trait;
use hyperlane_core::{
    ChainCommunicationError, ChainResult, ContractLocator, Indexed, Indexer, InterchainGasPayment,
    LogMeta, SequenceAwareIndexer, H256, H512, U256,
};
use serde_json::Value;
use std::ops::RangeInclusive;
use std::sync::Arc;
use tracing::{debug, info};

/// Indexer for Interchain Gas Payments on Cardano
///
/// Gas payments on Cardano are represented as UTXOs sent to the gas paymaster address
/// or as metadata in the outbound message transaction. This indexer fetches payment
/// events from Blockfrost by querying transaction data.
///
/// **Gas Payment Lifecycle on Cardano:**
/// 1. User/application dispatches a message via the mailbox
/// 2. In the same transaction or a separate one, they pay for gas by:
///    - Sending ADA to the IGP address
///    - Including payment metadata in transaction outputs
///    - Creating a reference output with payment info
/// 3. This indexer queries Blockfrost for transactions at the IGP address
/// 4. Gas payments are indexed and made available to the relayer
///
/// **Relayer Usage:**
/// - The relayer uses gas payment data to determine if a message has sufficient gas funds
/// - It checks the total payments for a message_id against estimated delivery costs
/// - This enables subsidized relaying where users pre-pay for gas on destination chains
#[derive(Debug)]
pub struct CardanoInterchainGasPaymasterIndexer {
    provider: Arc<BlockfrostProvider>,
    address: H256, // IGP minting policy hash or address
    conf: ConnectionConf,
}

impl CardanoInterchainGasPaymasterIndexer {
    /// Create a new Cardano IGP indexer
    pub fn new(conf: &ConnectionConf, locator: ContractLocator) -> Self {
        let provider = BlockfrostProvider::new(&conf.api_key, conf.network);
        Self {
            provider: Arc::new(provider),
            address: locator.address,
            conf: conf.clone(),
        }
    }

    /// Get the IGP script address
    fn get_igp_address(&self) -> ChainResult<String> {
        self.provider
            .script_hash_to_address(&self.conf.igp_policy_id)
            .map_err(ChainCommunicationError::from_other)
    }

    /// Parse a PayForGas redeemer from Blockfrost's JSON format
    fn parse_pay_for_gas_redeemer(&self, json: &Value) -> Option<InterchainGasPayment> {
        // PayForGas redeemer format (constructor 0):
        // { "constructor": 0, "fields": [message_id, destination, gas_amount] }
        let constructor = json.get("constructor")?.as_u64()?;
        if constructor != 0 {
            return None; // Not a PayForGas redeemer
        }

        let fields = json.get("fields")?.as_array()?;
        if fields.len() < 3 {
            return None;
        }

        // Parse message_id (32 bytes)
        let message_id_hex = fields.get(0)?.get("bytes")?.as_str()?;
        let message_id_bytes = hex::decode(message_id_hex).ok()?;
        if message_id_bytes.len() != 32 {
            return None;
        }
        let mut message_id = [0u8; 32];
        message_id.copy_from_slice(&message_id_bytes);

        // Parse destination domain
        let destination = fields.get(1)?.get("int")?.as_u64()? as u32;

        // Parse gas_amount
        let gas_amount = fields.get(2)?.get("int")?.as_u64()?;

        Some(InterchainGasPayment {
            message_id: H256::from(message_id),
            destination,
            payment: U256::from(gas_amount),
            gas_amount: U256::from(gas_amount),
        })
    }
}

#[async_trait]
impl Indexer<InterchainGasPayment> for CardanoInterchainGasPaymasterIndexer {
    async fn fetch_logs_in_range(
        &self,
        range: RangeInclusive<u32>,
    ) -> ChainResult<Vec<(Indexed<InterchainGasPayment>, LogMeta)>> {
        let from = *range.start();
        let to = *range.end();

        info!(
            "Fetching Cardano gas payments from block {} to {}",
            from, to
        );

        // Get IGP script address
        let igp_address = self.get_igp_address()?;
        debug!("IGP address: {}", igp_address);

        // Query transactions at IGP address in block range
        let transactions = self
            .provider
            .get_address_transactions(&igp_address, Some(from as u64), Some(to as u64))
            .await
            .map_err(ChainCommunicationError::from_other)?;

        info!(
            "Found {} transactions at IGP in block range {} to {}",
            transactions.len(),
            from,
            to
        );

        let mut results = Vec::new();

        for tx_info in transactions {
            // Get transaction redeemers to find PayForGas actions
            let redeemers = match self
                .provider
                .get_transaction_redeemers(&tx_info.tx_hash)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    debug!(
                        "Could not get redeemers for tx {}: {}",
                        tx_info.tx_hash, e
                    );
                    continue;
                }
            };

            // Find redeemers that are for spending
            for redeemer in redeemers {
                if redeemer.purpose != "Spend" {
                    continue;
                }

                // Get the redeemer datum content
                let redeemer_datum = match self
                    .provider
                    .get_redeemer_datum(&redeemer.redeemer_data_hash)
                    .await
                {
                    Ok(d) => d,
                    Err(e) => {
                        debug!(
                            "Could not get redeemer datum for tx {}: {}",
                            tx_info.tx_hash, e
                        );
                        continue;
                    }
                };

                // Try to parse as PayForGas redeemer
                if let Some(payment) = self.parse_pay_for_gas_redeemer(&redeemer_datum) {
                    let indexed = Indexed::new(payment.clone());

                    let log_meta = LogMeta {
                        address: self.address,
                        block_number: tx_info.block_height,
                        block_hash: H256::from_slice(
                            &hex::decode(&tx_info.tx_hash.get(0..64).unwrap_or(""))
                                .unwrap_or_else(|_| vec![0u8; 32]),
                        ),
                        transaction_id: H512::from_slice(&{
                            let mut bytes = [0u8; 64];
                            let tx_bytes =
                                hex::decode(&tx_info.tx_hash).unwrap_or_else(|_| vec![0u8; 32]);
                            bytes[..tx_bytes.len().min(64)]
                                .copy_from_slice(&tx_bytes[..tx_bytes.len().min(64)]);
                            bytes
                        }),
                        transaction_index: tx_info.tx_index as u64,
                        log_index: U256::from(redeemer.tx_index),
                    };

                    info!(
                        "Found gas payment in tx {} for message {}",
                        tx_info.tx_hash,
                        hex::encode(payment.message_id.as_bytes())
                    );
                    results.push((indexed, log_meta));
                }
            }
        }

        Ok(results)
    }

    async fn get_finalized_block_number(&self) -> ChainResult<u32> {
        self.provider
            .get_latest_block()
            .await
            .map(|b| b as u32)
            .map_err(ChainCommunicationError::from_other)
    }
}

#[async_trait]
impl SequenceAwareIndexer<InterchainGasPayment> for CardanoInterchainGasPaymasterIndexer {
    async fn latest_sequence_count_and_tip(&self) -> ChainResult<(Option<u32>, u32)> {
        // Gas payments don't have a sequence count on Cardano
        // They are indexed by block range, not by sequence
        // Return None for count and current finalized block for tip
        let tip = self.get_finalized_block_number().await?;
        Ok((None, tip))
    }
}

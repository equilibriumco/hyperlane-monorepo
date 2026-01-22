use crate::blockfrost_provider::{BlockfrostProvider, TransactionUtxos};
use crate::ConnectionConf;
use async_trait::async_trait;
use hyperlane_core::{
    ChainCommunicationError, ChainResult, ContractLocator, Indexed, Indexer, InterchainGasPayment,
    LogMeta, SequenceAwareIndexer, H256, H512, U256,
};
use serde_json::Value;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::sync::Arc;
use tracing::{debug, info};

/// Parsed PayForGas redeemer data (without payment amount which comes from UTXO diff)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayForGasRedeemerData {
    pub message_id: H256,
    pub destination: u32,
    pub gas_amount: u64,
}

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
}

/// Parse a PayForGas redeemer from Blockfrost's JSON format
///
/// Returns the parsed redeemer data without the payment amount,
/// which must be calculated separately from UTXO value differences.
///
/// PayForGas redeemer format (constructor 0):
/// `{ "constructor": 0, "fields": [message_id, destination, gas_amount] }`
fn parse_pay_for_gas_redeemer(json: &Value) -> Option<PayForGasRedeemerData> {
    let constructor = json.get("constructor")?.as_u64()?;
    if constructor != 0 {
        return None; // Not a PayForGas redeemer
    }

    let fields = json.get("fields")?.as_array()?;
    if fields.len() < 3 {
        return None;
    }

    // Parse message_id (32 bytes)
    let message_id_hex = fields.first()?.get("bytes")?.as_str()?;
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

    Some(PayForGasRedeemerData {
        message_id: H256::from(message_id),
        destination,
        gas_amount,
    })
}

/// Calculate IGP payment amount from transaction UTXOs
///
/// The payment is the difference in lovelace value between the IGP output
/// and the IGP input (output_value - input_value = payment added to IGP).
fn calculate_igp_payment(tx_utxos: &TransactionUtxos, igp_address: &str) -> u64 {
    // Sum lovelace in IGP inputs
    let input_lovelace: u64 = tx_utxos
        .inputs
        .iter()
        .filter(|utxo| utxo.address == igp_address)
        .map(|utxo| utxo.lovelace())
        .sum();

    // Sum lovelace in IGP outputs
    let output_lovelace: u64 = tx_utxos
        .outputs
        .iter()
        .filter(|utxo| utxo.address == igp_address)
        .map(|utxo| utxo.lovelace())
        .sum();

    // Payment is the increase in IGP balance
    output_lovelace.saturating_sub(input_lovelace)
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

        // Collect unique block heights and fetch their hashes
        let unique_heights: Vec<u64> = transactions
            .iter()
            .map(|tx| tx.block_height)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        let mut block_hashes: HashMap<u64, H256> = HashMap::new();
        for height in unique_heights {
            match self.provider.get_block_by_height(height).await {
                Ok(block_info) => {
                    let hash = H256::from_slice(
                        &hex::decode(&block_info.hash).unwrap_or_else(|_| vec![0u8; 32]),
                    );
                    block_hashes.insert(height, hash);
                }
                Err(e) => {
                    debug!("Could not fetch block info for height {}: {}", height, e);
                    // Use zero hash as fallback
                    block_hashes.insert(height, H256::zero());
                }
            }
        }

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
                    debug!("Could not get redeemers for tx {}: {}", tx_info.tx_hash, e);
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
                if let Some(redeemer_data) = parse_pay_for_gas_redeemer(&redeemer_datum) {
                    // Calculate actual payment from UTXO value differences
                    let payment_lovelace =
                        match self.provider.get_transaction_utxos(&tx_info.tx_hash).await {
                            Ok(tx_utxos) => calculate_igp_payment(&tx_utxos, &igp_address),
                            Err(e) => {
                                debug!("Could not get UTxOs for tx {}: {}", tx_info.tx_hash, e);
                                0
                            }
                        };

                    let payment = InterchainGasPayment {
                        message_id: redeemer_data.message_id,
                        destination: redeemer_data.destination,
                        payment: U256::from(payment_lovelace),
                        gas_amount: U256::from(redeemer_data.gas_amount),
                    };

                    let indexed = Indexed::new(payment);

                    // Get the block hash from our cache (fetched earlier)
                    let block_hash = block_hashes
                        .get(&tx_info.block_height)
                        .copied()
                        .unwrap_or_else(H256::zero);

                    let log_meta = LogMeta {
                        address: self.address,
                        block_number: tx_info.block_height,
                        block_hash,
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
                        "Found gas payment in tx {} for message {}: {} lovelace for {} gas",
                        tx_info.tx_hash,
                        hex::encode(payment.message_id.as_bytes()),
                        payment_lovelace,
                        redeemer_data.gas_amount
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blockfrost_provider::{Utxo, UtxoValue};
    use serde_json::json;

    // ==================== parse_pay_for_gas_redeemer tests ====================

    #[test]
    fn test_parse_pay_for_gas_redeemer_valid() {
        let message_id_hex = "ab".repeat(32);
        let redeemer_json = json!({
            "constructor": 0,
            "fields": [
                { "bytes": message_id_hex },
                { "int": 43113 },
                { "int": 200000 }
            ]
        });

        let result = parse_pay_for_gas_redeemer(&redeemer_json);
        assert!(result.is_some());

        let data = result.unwrap();
        assert_eq!(data.message_id, H256::from([0xab; 32]));
        assert_eq!(data.destination, 43113);
        assert_eq!(data.gas_amount, 200000);
    }

    #[test]
    fn test_parse_pay_for_gas_redeemer_wrong_constructor() {
        let message_id_hex = "ab".repeat(32);
        let redeemer_json = json!({
            "constructor": 1,
            "fields": [
                { "bytes": message_id_hex },
                { "int": 43113 },
                { "int": 200000 }
            ]
        });

        assert!(parse_pay_for_gas_redeemer(&redeemer_json).is_none());
    }

    #[test]
    fn test_parse_pay_for_gas_redeemer_missing_fields() {
        let message_id_hex = "ab".repeat(32);
        let redeemer_json = json!({
            "constructor": 0,
            "fields": [
                { "bytes": message_id_hex },
                { "int": 43113 }
            ]
        });

        assert!(parse_pay_for_gas_redeemer(&redeemer_json).is_none());
    }

    #[test]
    fn test_parse_pay_for_gas_redeemer_invalid_message_id_length() {
        let message_id_hex = "ab".repeat(16); // 16 bytes instead of 32
        let redeemer_json = json!({
            "constructor": 0,
            "fields": [
                { "bytes": message_id_hex },
                { "int": 43113 },
                { "int": 200000 }
            ]
        });

        assert!(parse_pay_for_gas_redeemer(&redeemer_json).is_none());
    }

    #[test]
    fn test_parse_pay_for_gas_redeemer_invalid_hex() {
        let redeemer_json = json!({
            "constructor": 0,
            "fields": [
                { "bytes": "not_valid_hex" },
                { "int": 43113 },
                { "int": 200000 }
            ]
        });

        assert!(parse_pay_for_gas_redeemer(&redeemer_json).is_none());
    }

    #[test]
    fn test_parse_pay_for_gas_redeemer_no_constructor() {
        let message_id_hex = "ab".repeat(32);
        let redeemer_json = json!({
            "fields": [
                { "bytes": message_id_hex },
                { "int": 43113 },
                { "int": 200000 }
            ]
        });

        assert!(parse_pay_for_gas_redeemer(&redeemer_json).is_none());
    }

    #[test]
    fn test_parse_pay_for_gas_redeemer_empty_json() {
        assert!(parse_pay_for_gas_redeemer(&json!({})).is_none());
    }

    #[test]
    fn test_parse_pay_for_gas_redeemer_zero_values() {
        let message_id_hex = "00".repeat(32);
        let redeemer_json = json!({
            "constructor": 0,
            "fields": [
                { "bytes": message_id_hex },
                { "int": 0 },
                { "int": 0 }
            ]
        });

        let result = parse_pay_for_gas_redeemer(&redeemer_json);
        assert!(result.is_some());

        let data = result.unwrap();
        assert_eq!(data.message_id, H256::zero());
        assert_eq!(data.destination, 0);
        assert_eq!(data.gas_amount, 0);
    }

    #[test]
    fn test_parse_pay_for_gas_redeemer_max_values() {
        let message_id_hex = "ff".repeat(32);
        let redeemer_json = json!({
            "constructor": 0,
            "fields": [
                { "bytes": message_id_hex },
                { "int": u32::MAX },
                { "int": u64::MAX }
            ]
        });

        let result = parse_pay_for_gas_redeemer(&redeemer_json);
        assert!(result.is_some());

        let data = result.unwrap();
        assert_eq!(data.message_id, H256::from([0xff; 32]));
        assert_eq!(data.destination, u32::MAX);
        assert_eq!(data.gas_amount, u64::MAX);
    }

    // ==================== calculate_igp_payment tests ====================

    fn create_utxo(address: &str, lovelace: u64) -> Utxo {
        Utxo {
            tx_hash: "test_tx".to_string(),
            output_index: 0,
            address: address.to_string(),
            value: vec![UtxoValue {
                unit: "lovelace".to_string(),
                quantity: lovelace.to_string(),
            }],
            inline_datum: None,
            data_hash: None,
            reference_script_hash: None,
        }
    }

    #[test]
    fn test_calculate_igp_payment_basic() {
        let igp_address = "addr_test_igp";
        let tx_utxos = TransactionUtxos {
            hash: "test_tx".to_string(),
            inputs: vec![create_utxo(igp_address, 5_000_000)],
            outputs: vec![create_utxo(igp_address, 7_500_000)],
        };

        let payment = calculate_igp_payment(&tx_utxos, igp_address);
        assert_eq!(payment, 2_500_000); // 7.5 ADA - 5 ADA = 2.5 ADA
    }

    #[test]
    fn test_calculate_igp_payment_multiple_utxos() {
        let igp_address = "addr_test_igp";
        let other_address = "addr_test_other";
        let tx_utxos = TransactionUtxos {
            hash: "test_tx".to_string(),
            inputs: vec![
                create_utxo(igp_address, 3_000_000),
                create_utxo(other_address, 10_000_000), // Should be ignored
                create_utxo(igp_address, 2_000_000),
            ],
            outputs: vec![
                create_utxo(igp_address, 8_000_000),
                create_utxo(other_address, 5_000_000), // Should be ignored
            ],
        };

        let payment = calculate_igp_payment(&tx_utxos, igp_address);
        // Input: 3 + 2 = 5 ADA, Output: 8 ADA, Payment: 3 ADA
        assert_eq!(payment, 3_000_000);
    }

    #[test]
    fn test_calculate_igp_payment_no_igp_inputs() {
        let igp_address = "addr_test_igp";
        let other_address = "addr_test_other";
        let tx_utxos = TransactionUtxos {
            hash: "test_tx".to_string(),
            inputs: vec![create_utxo(other_address, 10_000_000)],
            outputs: vec![create_utxo(igp_address, 5_000_000)],
        };

        let payment = calculate_igp_payment(&tx_utxos, igp_address);
        assert_eq!(payment, 5_000_000); // All output is payment
    }

    #[test]
    fn test_calculate_igp_payment_output_less_than_input() {
        let igp_address = "addr_test_igp";
        let tx_utxos = TransactionUtxos {
            hash: "test_tx".to_string(),
            inputs: vec![create_utxo(igp_address, 10_000_000)],
            outputs: vec![create_utxo(igp_address, 5_000_000)],
        };

        let payment = calculate_igp_payment(&tx_utxos, igp_address);
        // saturating_sub prevents underflow
        assert_eq!(payment, 0);
    }

    #[test]
    fn test_calculate_igp_payment_no_igp_utxos() {
        let igp_address = "addr_test_igp";
        let other_address = "addr_test_other";
        let tx_utxos = TransactionUtxos {
            hash: "test_tx".to_string(),
            inputs: vec![create_utxo(other_address, 10_000_000)],
            outputs: vec![create_utxo(other_address, 10_000_000)],
        };

        let payment = calculate_igp_payment(&tx_utxos, igp_address);
        assert_eq!(payment, 0);
    }

    #[test]
    fn test_calculate_igp_payment_empty_utxos() {
        let igp_address = "addr_test_igp";
        let tx_utxos = TransactionUtxos {
            hash: "test_tx".to_string(),
            inputs: vec![],
            outputs: vec![],
        };

        let payment = calculate_igp_payment(&tx_utxos, igp_address);
        assert_eq!(payment, 0);
    }

    #[test]
    fn test_calculate_igp_payment_equal_input_output() {
        let igp_address = "addr_test_igp";
        let tx_utxos = TransactionUtxos {
            hash: "test_tx".to_string(),
            inputs: vec![create_utxo(igp_address, 5_000_000)],
            outputs: vec![create_utxo(igp_address, 5_000_000)],
        };

        let payment = calculate_igp_payment(&tx_utxos, igp_address);
        assert_eq!(payment, 0); // No net payment
    }
}

use crate::blockfrost_provider::{AddressTransaction, BlockfrostProvider, TransactionUtxos};
use crate::provider::CardanoProvider;
use crate::tx_builder::tx_encoding::extract_int;
use crate::ConnectionConf;
use async_trait::async_trait;
use futures::stream::{self, FuturesUnordered, StreamExt};
use hyperlane_core::{
    ChainCommunicationError, ChainResult, ContractLocator, HyperlaneChain, HyperlaneContract,
    HyperlaneDomain, HyperlaneProvider, Indexed, Indexer, InterchainGasPaymaster,
    InterchainGasPayment, LogMeta, SequenceAwareIndexer, H256, H512, U256,
};
use serde_json::Value;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Cardano implementation of the InterchainGasPaymaster trait.
///
/// This is the struct returned by `build_interchain_gas_paymaster` in the
/// chain settings. It implements the marker trait `InterchainGasPaymaster`
/// (which has no methods) to satisfy the relayer's type requirements.
#[derive(Debug)]
pub struct CardanoInterchainGasPaymaster {
    domain: HyperlaneDomain,
    address: H256,
    conf: ConnectionConf,
}

impl CardanoInterchainGasPaymaster {
    pub fn new(conf: &ConnectionConf, locator: ContractLocator) -> Self {
        Self {
            domain: locator.domain.clone(),
            address: locator.address,
            conf: conf.clone(),
        }
    }
}

impl HyperlaneChain for CardanoInterchainGasPaymaster {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(CardanoProvider::new(&self.conf, self.domain.clone()))
    }
}

impl HyperlaneContract for CardanoInterchainGasPaymaster {
    fn address(&self) -> H256 {
        self.address
    }
}

#[async_trait]
impl InterchainGasPaymaster for CardanoInterchainGasPaymaster {}

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
        let provider =
            BlockfrostProvider::new(&conf.api_key, conf.network, conf.confirmation_block_delay);
        Self {
            provider: Arc::new(provider),
            address: locator.address,
            conf: conf.clone(),
        }
    }

    /// Get the IGP script address
    fn get_igp_address(&self) -> ChainResult<String> {
        self.provider
            .script_hash_to_address(&self.conf.igp_script_hash)
            .map_err(ChainCommunicationError::from_other)
    }

    async fn process_igp_transaction(
        &self,
        tx_info: &AddressTransaction,
        igp_address: &str,
        block_hashes: &HashMap<u64, H256>,
    ) -> Vec<(Indexed<InterchainGasPayment>, LogMeta)> {
        let mut results = Vec::new();

        let redeemers = match self
            .provider
            .get_transaction_redeemers(&tx_info.tx_hash)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                debug!("Could not get redeemers for tx {}: {}", tx_info.tx_hash, e);
                return results;
            }
        };

        for redeemer in redeemers {
            if !redeemer.is_spend_for_script(&self.conf.igp_script_hash) {
                continue;
            }

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

            if let Some(redeemer_data) = parse_pay_for_gas_redeemer(&redeemer_datum) {
                let (payment_lovelace, gas_overhead) =
                    match self.provider.get_transaction_utxos(&tx_info.tx_hash).await {
                        Ok(tx_utxos) => (
                            calculate_igp_payment(&tx_utxos, igp_address),
                            gas_overhead_for(&tx_utxos, igp_address, redeemer_data.destination),
                        ),
                        Err(e) => {
                            debug!("Could not get UTxOs for tx {}: {}", tx_info.tx_hash, e);
                            (0, None)
                        }
                    };

                // The redeemer declares application gas; the contract charged
                // for that plus the destination's overhead. Enforcement compares
                // against the full delivery estimate, so credit the total.
                let Some(gas_overhead) = gas_overhead else {
                    warn!(
                        "Could not read gas overhead for destination {} in tx {}; \
                         skipping this payment rather than crediting it short",
                        redeemer_data.destination, tx_info.tx_hash
                    );
                    continue;
                };
                let total_gas = redeemer_data.gas_amount.saturating_add(gas_overhead);

                let payment = InterchainGasPayment {
                    message_id: redeemer_data.message_id,
                    destination: redeemer_data.destination,
                    payment: U256::from(payment_lovelace),
                    gas_amount: U256::from(total_gas),
                };

                let indexed = Indexed::new(payment);

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
                    "Found gas payment in tx {} for message {}: {} lovelace for {} gas \
                     ({} application + {} overhead)",
                    tx_info.tx_hash,
                    hex::encode(payment.message_id.as_bytes()),
                    payment_lovelace,
                    total_gas,
                    redeemer_data.gas_amount,
                    gas_overhead
                );
                results.push((indexed, log_meta));
            }
        }

        results
    }
}

/// Parse a PayForGas redeemer from Blockfrost's JSON format
///
/// Returns the parsed redeemer data without the payment amount,
/// which must be calculated separately from UTXO value differences.
///
/// Format: `{ "constructor": 0, "fields": [message_id, destination, gas_amount] }`
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
/// Read the destination's `gas_overhead` out of the IGP datum being spent.
///
/// The redeemer carries application gas only; the contract adds this overhead
/// when it prices the payment, so the relayer has to add it too or every
/// message reads as underpaid against the full delivery estimate. Taking it
/// from the spent input rather than current chain state means a payment is
/// always credited at the rate that applied when it was made.
fn gas_overhead_for(
    tx_utxos: &TransactionUtxos,
    igp_address: &str,
    destination: u32,
) -> Option<u64> {
    use pallas_codec::minicbor;
    use pallas_primitives::conway::PlutusData;

    let datum_hex = tx_utxos
        .inputs
        .iter()
        .find(|utxo| utxo.address == igp_address)
        .and_then(|utxo| utxo.inline_datum.as_ref())?;

    let decoded: PlutusData = minicbor::decode(&hex::decode(datum_hex).ok()?).ok()?;

    // IgpDatum: Constr 0 [version, owner, beneficiary, gas_oracles]
    let PlutusData::Constr(datum) = decoded else {
        return None;
    };
    let oracles = datum.fields.to_vec().into_iter().nth(3)?;
    let PlutusData::Array(entries) = oracles else {
        return None;
    };

    for entry in entries.to_vec() {
        // Tuples encode as a two-element array: [domain, GasOracleConfig]
        let PlutusData::Array(pair) = entry else {
            continue;
        };
        let pair = pair.to_vec();
        let Some(domain) = pair.first().and_then(extract_int) else {
            continue;
        };
        if domain as u32 != destination {
            continue;
        }
        // GasOracleConfig: Constr 0 [gas_price, token_exchange_rate, gas_overhead]
        let Some(PlutusData::Constr(config)) = pair.get(1).cloned() else {
            return None;
        };
        return config
            .fields
            .to_vec()
            .get(2)
            .and_then(extract_int)
            .map(|overhead| overhead.max(0) as u64);
    }
    None
}

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

        let block_hashes: HashMap<u64, H256> = stream::iter(unique_heights)
            .map(|height| async move {
                let hash = match self.provider.get_block_by_height(height).await {
                    Ok(block_info) => H256::from_slice(
                        &hex::decode(&block_info.hash).unwrap_or_else(|_| vec![0u8; 32]),
                    ),
                    Err(e) => {
                        debug!("Could not fetch block info for height {}: {}", height, e);
                        H256::zero()
                    }
                };
                (height, hash)
            })
            .buffer_unordered(5)
            .collect()
            .await;

        let futs: FuturesUnordered<_> = transactions
            .iter()
            .map(|tx_info| self.process_igp_transaction(tx_info, &igp_address, &block_hashes))
            .collect();
        let results: Vec<Vec<_>> = futs.collect().await;

        Ok(results.into_iter().flatten().collect())
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
            collateral: false,
            reference: false,
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

#[cfg(test)]
mod gas_overhead_tests {
    use super::*;
    use crate::blockfrost_provider::{TransactionUtxos, Utxo};
    use pallas_codec::minicbor;
    use pallas_codec::utils::MaybeIndefArray;
    use pallas_primitives::conway::{BigInt, Constr, PlutusData};

    const IGP_ADDRESS: &str = "addr_test1igp";

    fn int(v: i64) -> PlutusData {
        PlutusData::BigInt(BigInt::Int(v.into()))
    }

    fn constr(fields: Vec<PlutusData>) -> PlutusData {
        PlutusData::Constr(Constr {
            tag: 121,
            any_constructor: None,
            fields: MaybeIndefArray::Def(fields),
        })
    }

    /// IgpDatum: Constr 0 [version, owner, beneficiary, gas_oracles]
    /// where each oracle is [domain, Constr 0 [gas_price, rate, overhead]]
    fn igp_datum_hex(oracles: Vec<(i64, i64)>) -> String {
        let entries: Vec<PlutusData> = oracles
            .into_iter()
            .map(|(domain, overhead)| {
                PlutusData::Array(MaybeIndefArray::Def(vec![
                    int(domain),
                    constr(vec![int(1_000_000_000), int(7171), int(overhead)]),
                ]))
            })
            .collect();
        let datum = constr(vec![
            int(0), // version
            PlutusData::BoundedBytes(vec![0u8; 28].into()),
            PlutusData::BoundedBytes(vec![1u8; 28].into()),
            PlutusData::Array(MaybeIndefArray::Def(entries)),
        ]);
        let mut buf = Vec::new();
        minicbor::encode(&datum, &mut buf).unwrap();
        hex::encode(buf)
    }

    fn utxos(datum_hex: Option<String>) -> TransactionUtxos {
        TransactionUtxos {
            hash: "aa".repeat(32),
            inputs: vec![Utxo {
                tx_hash: "bb".repeat(32),
                output_index: 0,
                address: IGP_ADDRESS.to_string(),
                value: vec![],
                inline_datum: datum_hex,
                data_hash: None,
                reference_script_hash: None,
                collateral: false,
                reference: false,
            }],
            outputs: vec![],
        }
    }

    #[test]
    fn reads_the_overhead_for_the_requested_destination() {
        let tx = utxos(Some(igp_datum_hex(vec![(11155111, 211000), (2003, 42)])));
        assert_eq!(gas_overhead_for(&tx, IGP_ADDRESS, 11155111), Some(211000));
        assert_eq!(gas_overhead_for(&tx, IGP_ADDRESS, 2003), Some(42));
    }

    /// A destination with no oracle has no overhead to add. Returning None makes
    /// the caller skip the payment rather than credit it short, which would look
    /// like an underpayment forever.
    #[test]
    fn returns_none_for_an_unconfigured_destination() {
        let tx = utxos(Some(igp_datum_hex(vec![(11155111, 211000)])));
        assert_eq!(gas_overhead_for(&tx, IGP_ADDRESS, 99999), None);
    }

    #[test]
    fn returns_none_when_the_igp_input_has_no_datum() {
        assert_eq!(gas_overhead_for(&utxos(None), IGP_ADDRESS, 11155111), None);
    }

    #[test]
    fn returns_none_when_no_input_is_at_the_igp_address() {
        let tx = utxos(Some(igp_datum_hex(vec![(11155111, 211000)])));
        assert_eq!(gas_overhead_for(&tx, "addr_test1other", 11155111), None);
    }

    #[test]
    fn returns_none_on_a_datum_that_is_not_an_igp_datum() {
        let mut buf = Vec::new();
        minicbor::encode(&int(7), &mut buf).unwrap();
        let tx = utxos(Some(hex::encode(buf)));
        assert_eq!(gas_overhead_for(&tx, IGP_ADDRESS, 11155111), None);
    }
}

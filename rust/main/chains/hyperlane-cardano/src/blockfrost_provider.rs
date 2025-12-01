use blockfrost::{BlockfrostAPI, BlockfrostError, Pagination};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::instrument;

#[derive(Error, Debug)]
pub enum BlockfrostProviderError {
    #[error("Blockfrost API error: {0}")]
    Api(#[from] BlockfrostError),
    #[error("UTXO not found: {0}")]
    UtxoNotFound(String),
    #[error("Invalid datum: {0}")]
    InvalidDatum(String),
    #[error("Deserialization error: {0}")]
    Deserialization(String),
    #[error("Script not found: {0}")]
    ScriptNotFound(String),
}

/// Blockfrost-based provider for Cardano chain data
pub struct BlockfrostProvider {
    api: BlockfrostAPI,
    network: CardanoNetwork,
}

impl std::fmt::Debug for BlockfrostProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockfrostProvider")
            .field("network", &self.network)
            .finish()
    }
}

#[derive(Clone, Copy, Debug)]
pub enum CardanoNetwork {
    Mainnet,
    Preprod,
    Preview,
}

impl CardanoNetwork {
    pub fn domain_id(&self) -> u32 {
        match self {
            CardanoNetwork::Mainnet => 2001,
            CardanoNetwork::Preprod => 2002,
            CardanoNetwork::Preview => 2003,
        }
    }
}

/// UTXO data from Blockfrost
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Utxo {
    pub tx_hash: String,
    pub output_index: u32,
    pub address: String,
    pub value: Vec<UtxoValue>,
    pub inline_datum: Option<String>,
    pub data_hash: Option<String>,
    pub reference_script_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UtxoValue {
    pub unit: String,
    pub quantity: String,
}

impl Utxo {
    /// Get the lovelace amount in this UTXO
    pub fn lovelace(&self) -> u64 {
        self.value
            .iter()
            .find(|v| v.unit == "lovelace")
            .and_then(|v| v.quantity.parse().ok())
            .unwrap_or(0)
    }

    /// Check if this UTXO contains a specific asset
    pub fn has_asset(&self, policy_id: &str, asset_name: &str) -> bool {
        let unit = format!("{}{}", policy_id, asset_name);
        self.value.iter().any(|v| v.unit == unit)
    }

    /// Get the quantity of a specific asset
    pub fn asset_quantity(&self, policy_id: &str, asset_name: &str) -> u64 {
        let unit = format!("{}{}", policy_id, asset_name);
        self.value
            .iter()
            .find(|v| v.unit == unit)
            .and_then(|v| v.quantity.parse().ok())
            .unwrap_or(0)
    }
}

impl BlockfrostProvider {
    /// Create a new Blockfrost provider
    pub fn new(api_key: &str, network: CardanoNetwork) -> Self {
        let api = BlockfrostAPI::new(api_key, Default::default());
        Self { api, network }
    }

    /// Get the current network
    pub fn network(&self) -> CardanoNetwork {
        self.network
    }

    /// Get the latest block number
    #[instrument(skip(self))]
    pub async fn get_latest_block(&self) -> Result<u64, BlockfrostProviderError> {
        let block = self.api.blocks_latest().await?;
        Ok(block.height.unwrap_or(0) as u64)
    }

    /// Get UTXOs at an address
    #[instrument(skip(self))]
    pub async fn get_utxos_at_address(
        &self,
        address: &str,
    ) -> Result<Vec<Utxo>, BlockfrostProviderError> {
        let pagination = Pagination::all();
        let utxos = self.api.addresses_utxos(address, pagination).await?;

        Ok(utxos
            .into_iter()
            .map(|u| Utxo {
                tx_hash: u.tx_hash,
                output_index: u.tx_index as u32,
                address: address.to_string(),
                value: u
                    .amount
                    .into_iter()
                    .map(|a| UtxoValue {
                        unit: a.unit,
                        quantity: a.quantity,
                    })
                    .collect(),
                inline_datum: u.inline_datum,
                data_hash: u.data_hash,
                reference_script_hash: u.reference_script_hash,
            })
            .collect())
    }

    /// Get UTXOs containing a specific asset (NFT)
    /// Returns empty vector if the asset doesn't exist (404 from Blockfrost)
    #[instrument(skip(self))]
    pub async fn get_utxos_by_asset(
        &self,
        policy_id: &str,
        asset_name: &str,
    ) -> Result<Vec<Utxo>, BlockfrostProviderError> {
        let asset_id = format!("{}{}", policy_id, asset_name);
        let pagination = Pagination::all();

        // Handle 404 (asset not found) as an empty result rather than an error
        let addresses = match self.api.assets_addresses(&asset_id, pagination).await {
            Ok(addrs) => addrs,
            Err(e) => {
                // Check if this is a 404 error (asset doesn't exist)
                let error_str = format!("{:?}", e);
                if error_str.contains("404") || error_str.contains("Not Found") {
                    return Ok(Vec::new());
                }
                return Err(e.into());
            }
        };

        let mut result = Vec::new();
        for addr_info in addresses {
            let utxos = self.get_utxos_at_address(&addr_info.address).await?;
            for utxo in utxos {
                if utxo.has_asset(policy_id, asset_name) {
                    result.push(utxo);
                }
            }
        }

        Ok(result)
    }

    /// Find a single UTXO by NFT (state marker)
    #[instrument(skip(self))]
    pub async fn find_utxo_by_nft(
        &self,
        policy_id: &str,
        asset_name: &str,
    ) -> Result<Utxo, BlockfrostProviderError> {
        let utxos = self.get_utxos_by_asset(policy_id, asset_name).await?;
        utxos.into_iter().next().ok_or_else(|| {
            BlockfrostProviderError::UtxoNotFound(format!(
                "NFT {}{} not found",
                policy_id, asset_name
            ))
        })
    }

    /// Get script datum by hash (returns JSON representation)
    #[instrument(skip(self))]
    pub async fn get_datum(&self, datum_hash: &str) -> Result<String, BlockfrostProviderError> {
        let datum = self.api.scripts_datum_hash(datum_hash).await?;
        // The API returns serde_json::Value directly
        serde_json::to_string(&datum)
            .map_err(|e| BlockfrostProviderError::Deserialization(e.to_string()))
    }

    /// Get script info by hash
    #[instrument(skip(self))]
    pub async fn get_script(
        &self,
        script_hash: &str,
    ) -> Result<serde_json::Value, BlockfrostProviderError> {
        let script = self.api.scripts_by_id(script_hash).await?;
        serde_json::to_value(&script)
            .map_err(|e| BlockfrostProviderError::Deserialization(e.to_string()))
    }

    /// Submit a transaction
    #[instrument(skip(self, tx_cbor))]
    pub async fn submit_transaction(
        &self,
        tx_cbor: &[u8],
    ) -> Result<String, BlockfrostProviderError> {
        let tx_hash = self.api.transactions_submit(tx_cbor.to_vec()).await?;
        Ok(tx_hash)
    }

    /// Get protocol parameters (returns JSON for flexibility)
    #[instrument(skip(self))]
    pub async fn get_protocol_parameters(
        &self,
    ) -> Result<serde_json::Value, BlockfrostProviderError> {
        let params = self.api.epochs_latest_parameters().await?;
        serde_json::to_value(&params)
            .map_err(|e| BlockfrostProviderError::Deserialization(e.to_string()))
    }

    /// Check if a message has been processed (by checking for processed message marker)
    #[instrument(skip(self))]
    pub async fn is_message_delivered(
        &self,
        processed_messages_policy: &str,
        message_id: &[u8; 32],
    ) -> Result<bool, BlockfrostProviderError> {
        // The processed message marker NFT has the message_id as the asset name
        let asset_name = hex::encode(message_id);
        let utxos = self
            .get_utxos_by_asset(processed_messages_policy, &asset_name)
            .await?;
        Ok(!utxos.is_empty())
    }

    /// Get all script UTXOs (useful for finding mailbox, registry, ISM states)
    #[instrument(skip(self))]
    pub async fn get_script_utxos(
        &self,
        script_hash: &str,
    ) -> Result<Vec<Utxo>, BlockfrostProviderError> {
        // Convert script hash to address (Plutus V3 script address)
        let address = script_hash_to_address(script_hash, self.network)?;
        self.get_utxos_at_address(&address).await
    }

    /// Get transactions at an address within a block range
    ///
    /// When a block range is specified, this uses an optimized approach:
    /// 1. Fetch all historical transactions for the address (1 API call)
    /// 2. Filter them by the block range in-memory
    ///
    /// This is more efficient than the old approach which fetched all transactions
    /// every time, or a naive block-by-block approach which would make too many calls.
    #[instrument(skip(self))]
    pub async fn get_address_transactions(
        &self,
        address: &str,
        from_block: Option<u64>,
        to_block: Option<u64>,
    ) -> Result<Vec<AddressTransaction>, BlockfrostProviderError> {
        // Fetch all transactions for this address once
        let pagination = Pagination::all();
        let txs = self.api.addresses_transactions(address, pagination).await?;

        let mut result = Vec::new();
        for tx in txs {
            let block_height = tx.block_height as u64;

            // Filter by block range if specified
            if let Some(from) = from_block {
                if block_height < from {
                    continue;
                }
            }
            if let Some(to) = to_block {
                if block_height > to {
                    continue;
                }
            }

            result.push(AddressTransaction {
                tx_hash: tx.tx_hash,
                block_height,
                block_time: tx.block_time as u64,
                tx_index: tx.tx_index as u32,
            });
        }

        Ok(result)
    }

    /// Get transaction UTXO details
    #[instrument(skip(self))]
    pub async fn get_transaction_utxos(
        &self,
        tx_hash: &str,
    ) -> Result<TransactionUtxos, BlockfrostProviderError> {
        let utxos = self.api.transactions_utxos(tx_hash).await?;

        let inputs = utxos
            .inputs
            .into_iter()
            .map(|i| Utxo {
                tx_hash: i.tx_hash,
                output_index: i.output_index as u32,
                address: i.address,
                value: i
                    .amount
                    .into_iter()
                    .map(|a| UtxoValue {
                        unit: a.unit,
                        quantity: a.quantity,
                    })
                    .collect(),
                inline_datum: i.inline_datum,
                data_hash: i.data_hash,
                reference_script_hash: i.reference_script_hash,
            })
            .collect();

        let outputs = utxos
            .outputs
            .into_iter()
            .map(|o| Utxo {
                tx_hash: tx_hash.to_string(),
                output_index: o.output_index as u32,
                address: o.address,
                value: o
                    .amount
                    .into_iter()
                    .map(|a| UtxoValue {
                        unit: a.unit,
                        quantity: a.quantity,
                    })
                    .collect(),
                inline_datum: o.inline_datum,
                data_hash: o.data_hash,
                reference_script_hash: o.reference_script_hash,
            })
            .collect();

        Ok(TransactionUtxos {
            hash: utxos.hash,
            inputs,
            outputs,
        })
    }

    /// Get transaction redeemers
    #[instrument(skip(self))]
    pub async fn get_transaction_redeemers(
        &self,
        tx_hash: &str,
    ) -> Result<Vec<TransactionRedeemer>, BlockfrostProviderError> {
        let redeemers = self.api.transactions_redeemers(tx_hash).await?;

        Ok(redeemers
            .into_iter()
            .map(|r| TransactionRedeemer {
                tx_index: r.tx_index as u32,
                purpose: format!("{:?}", r.purpose),
                script_hash: r.script_hash,
                redeemer_data_hash: r.redeemer_data_hash,
                datum_hash: r.datum_hash,
                unit_mem: r.unit_mem.parse().unwrap_or(0),
                unit_steps: r.unit_steps.parse().unwrap_or(0),
                fee: r.fee.parse().unwrap_or(0),
            })
            .collect())
    }

    /// Get a block by height
    #[instrument(skip(self))]
    pub async fn get_block_by_height(
        &self,
        height: u64,
    ) -> Result<BlockInfo, BlockfrostProviderError> {
        let block = self.api.blocks_by_id(&height.to_string()).await?;
        Ok(BlockInfo {
            hash: block.hash,
            height: block.height.unwrap_or(0) as u64,
            slot: block.slot.unwrap_or(0) as u64,
            time: block.time as u64,
            tx_count: block.tx_count as u32,
        })
    }

    /// Get transactions in a block
    #[instrument(skip(self))]
    pub async fn get_block_transactions(
        &self,
        block_hash: &str,
    ) -> Result<Vec<String>, BlockfrostProviderError> {
        let pagination = Pagination::all();
        let txs = self.api.blocks_txs(block_hash, pagination).await?;
        Ok(txs)
    }

    /// Get redeemer data by hash (the actual datum content)
    #[instrument(skip(self))]
    pub async fn get_redeemer_datum(
        &self,
        datum_hash: &str,
    ) -> Result<serde_json::Value, BlockfrostProviderError> {
        let datum = self.api.scripts_datum_hash(datum_hash).await?;
        Ok(datum)
    }

    /// Get script address from hash
    pub fn script_hash_to_address(&self, script_hash: &str) -> Result<String, BlockfrostProviderError> {
        script_hash_to_address(script_hash, self.network)
    }
}

/// Transaction at an address
#[derive(Debug, Clone)]
pub struct AddressTransaction {
    pub tx_hash: String,
    pub block_height: u64,
    pub block_time: u64,
    pub tx_index: u32,
}

/// Transaction UTXOs
#[derive(Debug, Clone)]
pub struct TransactionUtxos {
    pub hash: String,
    pub inputs: Vec<Utxo>,
    pub outputs: Vec<Utxo>,
}

/// Transaction redeemer
#[derive(Debug, Clone)]
pub struct TransactionRedeemer {
    pub tx_index: u32,
    pub purpose: String,
    pub script_hash: String,
    pub redeemer_data_hash: String,
    pub datum_hash: String,
    pub unit_mem: u64,
    pub unit_steps: u64,
    pub fee: u64,
}

/// Block info
#[derive(Debug, Clone)]
pub struct BlockInfo {
    pub hash: String,
    pub height: u64,
    pub slot: u64,
    pub time: u64,
    pub tx_count: u32,
}

/// Convert a script hash to a bech32 script address
fn script_hash_to_address(
    script_hash: &str,
    network: CardanoNetwork,
) -> Result<String, BlockfrostProviderError> {
    use pallas_addresses::{Address, Network, ShelleyAddress, ShelleyDelegationPart, ShelleyPaymentPart};

    let hash_bytes = hex::decode(script_hash)
        .map_err(|e| BlockfrostProviderError::Deserialization(e.to_string()))?;

    let hash: [u8; 28] = hash_bytes.try_into().map_err(|_| {
        BlockfrostProviderError::Deserialization("Invalid script hash length".to_string())
    })?;

    let network = match network {
        CardanoNetwork::Mainnet => Network::Mainnet,
        CardanoNetwork::Preprod | CardanoNetwork::Preview => Network::Testnet,
    };

    let payment_part = ShelleyPaymentPart::Script(pallas_crypto::hash::Hash::new(hash));
    let delegation_part = ShelleyDelegationPart::Null;
    let address = ShelleyAddress::new(network, payment_part, delegation_part);

    Ok(Address::Shelley(address).to_bech32().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_utxo_has_asset() {
        let utxo = Utxo {
            tx_hash: "abc123".to_string(),
            output_index: 0,
            address: "addr_test1...".to_string(),
            value: vec![
                UtxoValue {
                    unit: "lovelace".to_string(),
                    quantity: "5000000".to_string(),
                },
                UtxoValue {
                    unit: "abc123def456".to_string(),
                    quantity: "1".to_string(),
                },
            ],
            inline_datum: None,
            data_hash: None,
            reference_script_hash: None,
        };

        assert!(utxo.has_asset("abc123", "def456"));
        assert!(!utxo.has_asset("abc123", "other"));
        assert_eq!(utxo.lovelace(), 5000000);
    }
}

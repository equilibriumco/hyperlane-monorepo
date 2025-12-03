use blockfrost::{BlockfrostAPI, BlockfrostError, Order, Pagination};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::Semaphore;
use tokio::time::{sleep, Duration};
use tracing::{debug, instrument};

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
    /// Rate limiter: max 8 concurrent requests (staying under 10/sec limit)
    rate_limiter: Arc<Semaphore>,
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
        Self {
            api,
            network,
            // Allow max 5 concurrent requests to stay under 10/sec limit
            rate_limiter: Arc::new(Semaphore::new(5)),
        }
    }

    /// Rate-limited delay between API calls
    /// Blockfrost free tier limit is 10 req/sec, so we use 150ms delay with 5 concurrent
    async fn rate_limit(&self) {
        let _permit = self.rate_limiter.acquire().await.unwrap();
        // 150ms delay with 5 concurrent = max ~33 req/sec theoretical,
        // but with serial pagination this gives us breathing room
        sleep(Duration::from_millis(150)).await;
    }

    /// Get the current network
    pub fn network(&self) -> CardanoNetwork {
        self.network
    }

    /// Get the latest block number
    #[instrument(skip(self))]
    pub async fn get_latest_block(&self) -> Result<u64, BlockfrostProviderError> {
        self.rate_limit().await;
        let block = self.api.blocks_latest().await?;
        Ok(block.height.unwrap_or(0) as u64)
    }

    /// Get UTXOs at an address with manual pagination and rate limiting
    /// Returns empty vector if the address has no UTXOs (404 from Blockfrost)
    #[instrument(skip(self))]
    pub async fn get_utxos_at_address(
        &self,
        address: &str,
    ) -> Result<Vec<Utxo>, BlockfrostProviderError> {
        let mut all_utxos = Vec::new();
        let mut page = 1;
        const PAGE_SIZE: usize = 100;

        loop {
            self.rate_limit().await;
            let pagination = Pagination::new(Order::Asc, page, PAGE_SIZE);

            // Handle 404 (address has no UTXOs) as empty result rather than error
            let utxos = match self.api.addresses_utxos(address, pagination).await {
                Ok(utxos) => utxos,
                Err(e) => {
                    let error_str = format!("{:?}", e);
                    if error_str.contains("404") || error_str.contains("Not Found") {
                        tracing::debug!("Address {} has no UTXOs (404)", address);
                        return Ok(all_utxos); // Return what we have so far (or empty)
                    }
                    // Handle 429 (rate limit) errors by returning what we have
                    if error_str.contains("429") || error_str.contains("Too Many Requests") {
                        tracing::warn!("Rate limited while fetching UTXOs for {}, returning {} UTXOs collected so far", address, all_utxos.len());
                        if all_utxos.is_empty() {
                            return Err(e.into());
                        }
                        return Ok(all_utxos);
                    }
                    return Err(e.into());
                }
            };

            let page_len = utxos.len();
            for u in utxos {
                all_utxos.push(Utxo {
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
                });
            }

            // If we got less than PAGE_SIZE results, we've reached the last page
            if page_len < PAGE_SIZE {
                break;
            }
            page += 1;
        }

        Ok(all_utxos)
    }

    /// Get UTXOs containing a specific asset (NFT) with manual pagination
    /// Returns empty vector if the asset doesn't exist (404 from Blockfrost)
    #[instrument(skip(self))]
    pub async fn get_utxos_by_asset(
        &self,
        policy_id: &str,
        asset_name: &str,
    ) -> Result<Vec<Utxo>, BlockfrostProviderError> {
        let asset_id = format!("{}{}", policy_id, asset_name);
        let mut all_addresses = Vec::new();
        let mut page = 1;
        const PAGE_SIZE: usize = 100;

        // Manually paginate through asset addresses
        loop {
            self.rate_limit().await;
            let pagination = Pagination::new(Order::Asc, page, PAGE_SIZE);

            let addresses = match self.api.assets_addresses(&asset_id, pagination).await {
                Ok(addrs) => addrs,
                Err(e) => {
                    let error_str = format!("{:?}", e);
                    if error_str.contains("404") || error_str.contains("Not Found") {
                        return Ok(Vec::new());
                    }
                    // Handle 429 rate limit - return what we have if possible
                    if error_str.contains("429") || error_str.contains("Too Many Requests") {
                        tracing::warn!("Rate limited while fetching asset addresses, continuing with {} addresses", all_addresses.len());
                        break;
                    }
                    return Err(e.into());
                }
            };

            let page_len = addresses.len();
            all_addresses.extend(addresses);

            if page_len < PAGE_SIZE {
                break;
            }
            page += 1;
        }

        let mut result = Vec::new();
        for addr_info in all_addresses {
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
        self.rate_limit().await;
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
        self.rate_limit().await;
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
        self.rate_limit().await;
        let tx_hash = self.api.transactions_submit(tx_cbor.to_vec()).await?;
        Ok(tx_hash)
    }

    /// Get protocol parameters (returns JSON for flexibility)
    #[instrument(skip(self))]
    pub async fn get_protocol_parameters(
        &self,
    ) -> Result<serde_json::Value, BlockfrostProviderError> {
        self.rate_limit().await;
        let params = self.api.epochs_latest_parameters().await?;
        serde_json::to_value(&params)
            .map_err(|e| BlockfrostProviderError::Deserialization(e.to_string()))
    }

    /// Check if a message has been processed (by checking for processed message marker UTXO)
    ///
    /// Processed message markers are UTXOs at the processed_messages_script address
    /// with an inline datum containing the message_id.
    ///
    /// TODO: Migrate to NFT-based tracking for O(1) lookups instead of O(n) scanning.
    #[instrument(skip(self))]
    pub async fn is_message_delivered(
        &self,
        processed_messages_script_hash: &str,
        message_id: &[u8; 32],
    ) -> Result<bool, BlockfrostProviderError> {
        // Get the script address from the hash
        let address = script_hash_to_address(processed_messages_script_hash, self.network)?;

        // Get all UTXOs at the processed messages script address
        let utxos = self.get_utxos_at_address(&address).await?;

        // Check each UTXO's inline datum for the message_id
        // ProcessedMessageDatum is encoded as: d87981 58 20 <32-byte message_id>
        // (Constr 0, 1 field, bytestring of 32 bytes)
        let message_id_hex = hex::encode(message_id);

        for utxo in utxos {
            if let Some(ref datum_hex) = utxo.inline_datum {
                // The datum should end with the 32-byte message_id
                // Format: d8798158200000...0000 (prefix + 32 bytes = 68 hex chars total)
                if datum_hex.len() >= 64 && datum_hex.ends_with(&message_id_hex) {
                    debug!("Found processed message marker for message_id: {}", message_id_hex);
                    return Ok(true);
                }
            }
        }

        debug!("No processed message marker found for message_id: {}", message_id_hex);
        Ok(false)
    }

    /// Check if a message has been processed using NFT lookup (O(1))
    ///
    /// This is the preferred method when processedMessagesNftPolicyId is configured.
    /// It performs a direct asset lookup by policy_id + asset_name, which is O(1)
    /// regardless of how many messages have been processed.
    #[instrument(skip(self))]
    pub async fn is_message_delivered_by_nft(
        &self,
        nft_policy_id: &str,
        message_id: &[u8; 32],
    ) -> Result<bool, BlockfrostProviderError> {
        // The NFT asset name is the 32-byte message_id
        let asset_name = hex::encode(message_id);

        // Use get_utxos_by_asset for efficient O(1) lookup
        let utxos = self.get_utxos_by_asset(nft_policy_id, &asset_name).await?;

        if !utxos.is_empty() {
            debug!("Found processed message NFT for message_id: {}", asset_name);
            return Ok(true);
        }

        debug!("No processed message NFT found for message_id: {}", asset_name);
        Ok(false)
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

    /// Get transactions at an address within a block range with manual pagination
    ///
    /// Uses manual pagination with rate limiting between each page to avoid 429 errors.
    /// Filters by block range in-memory after fetching.
    #[instrument(skip(self))]
    pub async fn get_address_transactions(
        &self,
        address: &str,
        from_block: Option<u64>,
        to_block: Option<u64>,
    ) -> Result<Vec<AddressTransaction>, BlockfrostProviderError> {
        let mut result = Vec::new();
        let mut page = 1;
        const PAGE_SIZE: usize = 100;

        // Manually paginate with rate limiting between each page
        loop {
            self.rate_limit().await;
            let pagination = Pagination::new(Order::Asc, page, PAGE_SIZE);

            let txs = match self.api.addresses_transactions(address, pagination).await {
                Ok(txs) => txs,
                Err(e) => {
                    let error_str = format!("{:?}", e);
                    if error_str.contains("404") || error_str.contains("Not Found") {
                        return Ok(result);
                    }
                    // Handle 429 rate limit - return what we have if possible
                    if error_str.contains("429") || error_str.contains("Too Many Requests") {
                        tracing::warn!("Rate limited while fetching transactions, continuing with {} txs", result.len());
                        break;
                    }
                    return Err(e.into());
                }
            };

            let page_len = txs.len();

            // Convert and filter each transaction immediately
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

            if page_len < PAGE_SIZE {
                break;
            }
            page += 1;
        }

        Ok(result)
    }

    /// Get transaction UTXO details
    #[instrument(skip(self))]
    pub async fn get_transaction_utxos(
        &self,
        tx_hash: &str,
    ) -> Result<TransactionUtxos, BlockfrostProviderError> {
        self.rate_limit().await;
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
        self.rate_limit().await;
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
        self.rate_limit().await;
        let block = self.api.blocks_by_id(&height.to_string()).await?;
        Ok(BlockInfo {
            hash: block.hash,
            height: block.height.unwrap_or(0) as u64,
            slot: block.slot.unwrap_or(0) as u64,
            time: block.time as u64,
            tx_count: block.tx_count as u32,
        })
    }

    /// Get transactions in a block with manual pagination
    #[instrument(skip(self))]
    pub async fn get_block_transactions(
        &self,
        block_hash: &str,
    ) -> Result<Vec<String>, BlockfrostProviderError> {
        let mut all_txs = Vec::new();
        let mut page = 1;
        const PAGE_SIZE: usize = 100;

        loop {
            self.rate_limit().await;
            let pagination = Pagination::new(Order::Asc, page, PAGE_SIZE);

            let txs = match self.api.blocks_txs(block_hash, pagination).await {
                Ok(txs) => txs,
                Err(e) => {
                    let error_str = format!("{:?}", e);
                    if error_str.contains("404") || error_str.contains("Not Found") {
                        return Ok(all_txs);
                    }
                    // Handle 429 rate limit - return what we have
                    if error_str.contains("429") || error_str.contains("Too Many Requests") {
                        tracing::warn!("Rate limited while fetching block txs, returning {} txs", all_txs.len());
                        break;
                    }
                    return Err(e.into());
                }
            };

            let page_len = txs.len();
            all_txs.extend(txs);

            if page_len < PAGE_SIZE {
                break;
            }
            page += 1;
        }

        Ok(all_txs)
    }

    /// Get redeemer data by hash (the actual datum content)
    #[instrument(skip(self))]
    pub async fn get_redeemer_datum(
        &self,
        datum_hash: &str,
    ) -> Result<serde_json::Value, BlockfrostProviderError> {
        self.rate_limit().await;
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

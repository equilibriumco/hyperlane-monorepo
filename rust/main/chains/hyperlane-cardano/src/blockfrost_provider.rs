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
    #[error("Request timeout: {0}")]
    Timeout(String),
}

/// Blockfrost-based provider for Cardano chain data
pub struct BlockfrostProvider {
    api: BlockfrostAPI,
    network: CardanoNetwork,
    /// Rate limiter: max 8 concurrent requests (staying under 10/sec limit)
    rate_limiter: Arc<Semaphore>,
    /// How many blocks behind the tip to report as latest.
    /// Prevents advancing past blocks that Blockfrost hasn't finished
    /// indexing for address-transaction queries.
    confirmation_block_delay: u32,
    /// API key for direct HTTP calls that bypass the blockfrost crate
    api_key: String,
    /// Base URL derived from the API key prefix (e.g. "preview" → cardano-preview)
    base_url: String,
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
    /// Whether this is a collateral input (not included in tx.inputs on-chain)
    #[serde(default)]
    pub collateral: bool,
    /// Whether this is a reference input (not included in tx.inputs on-chain)
    #[serde(default)]
    pub reference: bool,
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
        let unit = format!("{policy_id}{asset_name}");
        self.value.iter().any(|v| v.unit == unit)
    }

    /// Get the quantity of a specific asset
    pub fn asset_quantity(&self, policy_id: &str, asset_name: &str) -> u64 {
        let unit = format!("{policy_id}{asset_name}");
        self.value
            .iter()
            .find(|v| v.unit == unit)
            .and_then(|v| v.quantity.parse().ok())
            .unwrap_or(0)
    }
}

/// Per-request timeout for Blockfrost HTTP calls.
/// Prevents indefinite hangs when the API is unresponsive, which would
/// block the relayer's prepare queue for the entire destination domain.
const BLOCKFROST_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

impl BlockfrostProvider {
    /// Create a new Blockfrost provider
    pub fn new(api_key: &str, network: CardanoNetwork, confirmation_block_delay: u32) -> Self {
        let api = BlockfrostAPI::new(api_key, Default::default());
        let base_url = match network {
            CardanoNetwork::Mainnet => blockfrost::CARDANO_MAINNET_URL,
            CardanoNetwork::Preprod => blockfrost::CARDANO_PREPROD_URL,
            CardanoNetwork::Preview => blockfrost::CARDANO_PREVIEW_URL,
        };
        Self {
            api,
            network,
            // Allow max 5 concurrent requests to stay under 10/sec limit
            rate_limiter: Arc::new(Semaphore::new(5)),
            confirmation_block_delay,
            api_key: api_key.to_string(),
            base_url: base_url.to_string(),
        }
    }

    /// Wrap a future with a timeout to prevent indefinite hangs on
    /// unresponsive Blockfrost API calls.
    async fn with_timeout<T, E>(
        &self,
        fut: impl std::future::Future<Output = Result<T, E>>,
    ) -> Result<T, BlockfrostProviderError>
    where
        BlockfrostProviderError: From<E>,
    {
        tokio::time::timeout(BLOCKFROST_REQUEST_TIMEOUT, fut)
            .await
            .map_err(|_| {
                BlockfrostProviderError::Timeout(
                    "Blockfrost request timed out after 30s".to_string(),
                )
            })?
            .map_err(BlockfrostProviderError::from)
    }

    /// Rate-limited delay between API calls
    /// Blockfrost free tier limit is 10 req/sec, so we use 150ms delay with 5 concurrent
    async fn rate_limit(&self) {
        let _permit = self
            .rate_limiter
            .acquire()
            .await
            .expect("rate limiter semaphore closed");
        // 150ms delay with 5 concurrent = max ~33 req/sec theoretical,
        // but with serial pagination this gives us breathing room
        sleep(Duration::from_millis(150)).await;
    }

    /// Get the current network
    pub fn network(&self) -> CardanoNetwork {
        self.network
    }

    /// Get the latest block number, lagging behind the real tip by
    /// `confirmation_block_delay` blocks to avoid querying blocks that
    /// Blockfrost hasn't finished indexing for address-transaction lookups.
    #[instrument(skip(self))]
    pub async fn get_latest_block(&self) -> Result<u64, BlockfrostProviderError> {
        self.rate_limit().await;
        let block = self.with_timeout(self.api.blocks_latest()).await?;
        let tip = block.height.unwrap_or(0) as u64;
        Ok(tip.saturating_sub(self.confirmation_block_delay as u64))
    }

    /// Get the latest finalized block info (hash, height, time), lagging
    /// behind the real tip by `confirmation_block_delay` blocks.
    #[instrument(skip(self))]
    pub async fn get_latest_block_info(&self) -> Result<BlockInfo, BlockfrostProviderError> {
        let finalized_height = self.get_latest_block().await?;
        self.get_block_by_height(finalized_height).await
    }

    /// Get the latest slot number
    #[instrument(skip(self))]
    pub async fn get_latest_slot(&self) -> Result<u64, BlockfrostProviderError> {
        self.rate_limit().await;
        let block = self.with_timeout(self.api.blocks_latest()).await?;
        Ok(block.slot.unwrap_or(0) as u64)
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
            let utxos = match self
                .with_timeout(self.api.addresses_utxos(address, pagination))
                .await
            {
                Ok(utxos) => utxos,
                Err(e) => {
                    let error_str = format!("{e:?}");
                    if error_str.contains("404") || error_str.contains("Not Found") {
                        tracing::debug!("Address {} has no UTXOs (404)", address);
                        return Ok(all_utxos); // Return what we have so far (or empty)
                    }
                    // Handle 429 (rate limit) errors by returning what we have
                    if error_str.contains("429") || error_str.contains("Too Many Requests") {
                        tracing::warn!("Rate limited while fetching UTXOs for {}, returning {} UTXOs collected so far", address, all_utxos.len());
                        if all_utxos.is_empty() {
                            return Err(e);
                        }
                        return Ok(all_utxos);
                    }
                    return Err(e);
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
                    collateral: false,
                    reference: false,
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
        let asset_id = format!("{policy_id}{asset_name}");

        // Step 1: find the address holding the asset (NFTs have exactly one holder)
        self.rate_limit().await;
        let addresses = match self
            .with_timeout(
                self.api
                    .assets_addresses(&asset_id, Pagination::new(Order::Asc, 1, 1)),
            )
            .await
        {
            Ok(addrs) => addrs,
            Err(e) => {
                let error_str = format!("{e:?}");
                if error_str.contains("404") || error_str.contains("Not Found") {
                    return Ok(Vec::new());
                }
                return Err(e);
            }
        };

        let Some(addr_info) = addresses.into_iter().next() else {
            return Ok(Vec::new());
        };

        // Step 2: fetch only the UTXOs at that address that contain the specific asset.
        // Using /addresses/{address}/utxos/{asset} avoids scanning all UTXOs at the address
        // (script addresses can accumulate hundreds of UTXOs from message receipts).
        let mut result = Vec::new();
        let mut page = 1;
        const PAGE_SIZE: usize = 100;
        loop {
            self.rate_limit().await;
            let pagination = Pagination::new(Order::Asc, page, PAGE_SIZE);
            let utxos = match self
                .with_timeout(self.api.addresses_utxos_asset(
                    &addr_info.address,
                    &asset_id,
                    pagination,
                ))
                .await
            {
                Ok(u) => u,
                Err(e) => {
                    let error_str = format!("{e:?}");
                    if error_str.contains("404") || error_str.contains("Not Found") {
                        break;
                    }
                    return Err(e);
                }
            };
            let page_len = utxos.len();
            for u in utxos {
                result.push(Utxo {
                    tx_hash: u.tx_hash,
                    output_index: u.output_index as u32,
                    address: addr_info.address.clone(),
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
                    collateral: false,
                    reference: false,
                });
            }
            if page_len < PAGE_SIZE {
                break;
            }
            page += 1;
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
            BlockfrostProviderError::UtxoNotFound(format!("NFT {policy_id}{asset_name} not found"))
        })
    }

    /// Fetch a specific UTXO by transaction hash and output index
    #[instrument(skip(self))]
    pub async fn get_utxo(
        &self,
        tx_hash: &str,
        output_index: u32,
    ) -> Result<Utxo, BlockfrostProviderError> {
        let tx_utxos = self.get_transaction_utxos(tx_hash).await?;
        tx_utxos
            .outputs
            .into_iter()
            .find(|o| o.output_index == output_index)
            .ok_or_else(|| {
                BlockfrostProviderError::UtxoNotFound(format!(
                    "Output #{output_index} not found in tx {tx_hash}"
                ))
            })
    }

    /// Get script datum by hash (returns JSON representation)
    #[instrument(skip(self))]
    pub async fn get_datum(&self, datum_hash: &str) -> Result<String, BlockfrostProviderError> {
        self.rate_limit().await;
        let datum = self
            .with_timeout(self.api.scripts_datum_hash(datum_hash))
            .await?;
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
        let script = self
            .with_timeout(self.api.scripts_by_id(script_hash))
            .await?;
        serde_json::to_value(&script)
            .map_err(|e| BlockfrostProviderError::Deserialization(e.to_string()))
    }

    /// Get the serialised size (in bytes) of a script by its hash.
    pub async fn get_script_size(&self, script_hash: &str) -> Result<u64, BlockfrostProviderError> {
        let script_info = self.get_script(script_hash).await?;
        script_info
            .get("serialised_size")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                BlockfrostProviderError::Deserialization(format!(
                    "Missing serialised_size for script {script_hash}"
                ))
            })
    }

    /// Get all asset IDs under a policy (policy_hex + asset_name_hex per entry).
    /// Uses Blockfrost's `/assets/policy/{policy_id}` endpoint with pagination.
    pub async fn get_policy_asset_ids(
        &self,
        policy_id: &str,
    ) -> Result<Vec<String>, BlockfrostProviderError> {
        let mut all_assets = Vec::new();
        let mut page = 1u32;
        loop {
            self.rate_limit().await;
            let url = format!(
                "{}/assets/policy/{}?page={}&count=100",
                self.base_url, policy_id, page
            );
            let response = tokio::time::timeout(
                BLOCKFROST_REQUEST_TIMEOUT,
                reqwest::Client::new()
                    .get(&url)
                    .header("project_id", &self.api_key)
                    .send(),
            )
            .await
            .map_err(|_| BlockfrostProviderError::Timeout("get_policy_asset_ids timed out".into()))?
            .map_err(|e| {
                BlockfrostProviderError::Api(BlockfrostError::from(
                    Box::new(e) as Box<dyn std::error::Error>
                ))
            })?;

            let status = response.status();
            let body = response.text().await.map_err(|e| {
                BlockfrostProviderError::Deserialization(format!("Failed to read response: {e}"))
            })?;

            if status.as_u16() == 404 {
                // No assets for this policy yet
                break;
            }
            if !status.is_success() {
                return Err(BlockfrostProviderError::Api(BlockfrostError::Response {
                    url: url.clone(),
                    reason: blockfrost::error::ResponseError {
                        status_code: status.as_u16(),
                        error: status.canonical_reason().unwrap_or("Unknown").to_string(),
                        message: body,
                    },
                }));
            }

            let items: Vec<serde_json::Value> = serde_json::from_str(&body).map_err(|e| {
                BlockfrostProviderError::Deserialization(format!(
                    "Failed to parse policy assets: {e}"
                ))
            })?;

            if items.is_empty() {
                break;
            }
            for item in &items {
                if let Some(asset) = item.get("asset").and_then(|v| v.as_str()) {
                    all_assets.push(asset.to_string());
                }
            }
            if items.len() < 100 {
                break;
            }
            page += 1;
        }
        Ok(all_assets)
    }

    /// Submit a transaction
    #[instrument(skip(self, tx_cbor))]
    pub async fn submit_transaction(
        &self,
        tx_cbor: &[u8],
    ) -> Result<String, BlockfrostProviderError> {
        self.rate_limit().await;
        let tx_hash = self
            .with_timeout(self.api.transactions_submit(tx_cbor.to_vec()))
            .await?;
        Ok(tx_hash)
    }

    /// Evaluate a transaction to get execution units for each script.
    /// Sends hex-encoded TX CBOR to Blockfrost's /utils/txs/evaluate endpoint.
    ///
    /// The blockfrost crate sends raw binary bytes, but Blockfrost's Ogmios
    /// proxy expects the CBOR as a hex-encoded string. This method bypasses
    /// the crate to send the correct format.
    #[instrument(skip(self, tx_cbor))]
    pub async fn evaluate_tx(
        &self,
        tx_cbor: &[u8],
    ) -> Result<serde_json::Value, BlockfrostProviderError> {
        self.rate_limit().await;
        let url = format!("{}/utils/txs/evaluate", self.base_url);
        let hex_body = hex::encode(tx_cbor);

        let response = tokio::time::timeout(
            BLOCKFROST_REQUEST_TIMEOUT,
            reqwest::Client::new()
                .post(&url)
                .header("project_id", &self.api_key)
                .header("Content-Type", "application/cbor")
                .body(hex_body)
                .send(),
        )
        .await
        .map_err(|_| {
            BlockfrostProviderError::Timeout(
                "Blockfrost evaluate request timed out after 30s".to_string(),
            )
        })?
        .map_err(|e| {
            BlockfrostProviderError::Api(BlockfrostError::from(
                Box::new(e) as Box<dyn std::error::Error>
            ))
        })?;

        let status = response.status();
        let body = response.text().await.map_err(|e| {
            BlockfrostProviderError::Deserialization(format!("Failed to read response body: {e}"))
        })?;

        if !status.is_success() {
            return Err(BlockfrostProviderError::Api(BlockfrostError::Response {
                url: url.clone(),
                reason: blockfrost::error::ResponseError {
                    status_code: status.as_u16(),
                    error: status.canonical_reason().unwrap_or("Unknown").to_string(),
                    message: body,
                },
            }));
        }

        serde_json::from_str(&body).map_err(|e| {
            BlockfrostProviderError::Deserialization(format!(
                "Failed to parse evaluate response: {e}"
            ))
        })
    }

    /// Evaluate a transaction with additional UTXO context.
    /// Required for chained TX evaluation where inputs are outputs of prior
    /// unsubmitted TXs. Posts to Blockfrost's `/utils/txs/evaluate/utxos`.
    ///
    /// `additional_utxos` are serialized as an Ogmios-compatible array of
    /// `[[{txId, index}, {address, value, ...}], ...]`.
    #[instrument(skip(self, tx_cbor, additional_utxos))]
    pub async fn evaluate_tx_with_additional_utxos(
        &self,
        tx_cbor: &[u8],
        additional_utxos: &[Utxo],
    ) -> Result<serde_json::Value, BlockfrostProviderError> {
        self.rate_limit().await;
        let url = format!("{}/utils/txs/evaluate/utxos", self.base_url);

        let utxo_set: Vec<serde_json::Value> = additional_utxos
            .iter()
            .map(|u| -> Result<serde_json::Value, BlockfrostProviderError> {
                let mut value = serde_json::json!({
                    "coins": u.lovelace(),
                });
                // Add native assets if present
                let assets: Vec<&UtxoValue> =
                    u.value.iter().filter(|v| v.unit != "lovelace").collect();
                // In Ogmios v5, native assets are top-level keys in the value
                // object (not nested under "assets"). The key is the policy ID
                // (56 hex chars) and the nested map uses asset names as keys.
                for a in assets {
                    if a.unit.len() >= 56 {
                        let policy = &a.unit[..56];
                        let name = &a.unit[56..];
                        let policy_entry = value
                            .as_object_mut()
                            .ok_or_else(|| {
                                BlockfrostProviderError::Deserialization(
                                    "Value is not a JSON object".to_string(),
                                )
                            })?
                            .entry(policy.to_string())
                            .or_insert_with(|| serde_json::json!({}));
                        if let Some(obj) = policy_entry.as_object_mut() {
                            let quantity = a.quantity.parse::<u64>().map_err(|e| {
                                BlockfrostProviderError::Deserialization(format!(
                                    "Invalid asset quantity '{}' for unit '{}': {e}",
                                    a.quantity, a.unit
                                ))
                            })?;
                            obj.insert(name.to_string(), serde_json::json!(quantity));
                        }
                    }
                }

                let mut output = serde_json::json!({
                    "address": u.address,
                    "value": value,
                });
                if let Some(ref datum) = u.inline_datum {
                    output["datum"] = serde_json::json!(datum);
                }

                Ok(serde_json::json!([
                    { "txId": u.tx_hash, "index": u.output_index },
                    output
                ]))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let cbor_hex = hex::encode(tx_cbor);
        let body = serde_json::json!({
            "cbor": cbor_hex,
            "additionalUtxoSet": utxo_set,
        });
        let body = serde_json::to_string(&body).map_err(|e| {
            BlockfrostProviderError::Deserialization(format!(
                "Failed to serialize evaluate/utxos request body: {e}"
            ))
        })?;

        let response = tokio::time::timeout(
            BLOCKFROST_REQUEST_TIMEOUT,
            reqwest::Client::new()
                .post(&url)
                .header("project_id", &self.api_key)
                .header("Content-Type", "application/json")
                .body(body)
                .send(),
        )
        .await
        .map_err(|_| {
            BlockfrostProviderError::Timeout(
                "Blockfrost evaluate/utxos request timed out".to_string(),
            )
        })?
        .map_err(|e| {
            BlockfrostProviderError::Api(BlockfrostError::from(
                Box::new(e) as Box<dyn std::error::Error>
            ))
        })?;

        let status = response.status();
        let resp_body = response.text().await.map_err(|e| {
            BlockfrostProviderError::Deserialization(format!("Failed to read response body: {e}"))
        })?;

        if !status.is_success() {
            return Err(BlockfrostProviderError::Api(BlockfrostError::Response {
                url,
                reason: blockfrost::error::ResponseError {
                    status_code: status.as_u16(),
                    error: status.canonical_reason().unwrap_or("Unknown").to_string(),
                    message: resp_body,
                },
            }));
        }

        serde_json::from_str(&resp_body).map_err(|e| {
            BlockfrostProviderError::Deserialization(format!(
                "Failed to parse evaluate/utxos response: {e}"
            ))
        })
    }

    /// Get protocol parameters (returns JSON for flexibility)
    #[instrument(skip(self))]
    pub async fn get_protocol_parameters(
        &self,
    ) -> Result<serde_json::Value, BlockfrostProviderError> {
        self.rate_limit().await;
        let params = self
            .with_timeout(self.api.epochs_latest_parameters())
            .await?;
        serde_json::to_value(&params)
            .map_err(|e| BlockfrostProviderError::Deserialization(e.to_string()))
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

            let txs = match self
                .with_timeout(self.api.addresses_transactions(address, pagination))
                .await
            {
                Ok(txs) => txs,
                Err(e) => {
                    let error_str = format!("{e:?}");
                    if error_str.contains("404") || error_str.contains("Not Found") {
                        return Ok(result);
                    }
                    // Handle 429 rate limit - return what we have if possible
                    if error_str.contains("429") || error_str.contains("Too Many Requests") {
                        tracing::warn!(
                            "Rate limited while fetching transactions, continuing with {} txs",
                            result.len()
                        );
                        break;
                    }
                    return Err(e);
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
        let utxos = self
            .with_timeout(self.api.transactions_utxos(tx_hash))
            .await?;

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
                collateral: i.collateral,
                reference: i.reference.unwrap_or(false),
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
                collateral: o.collateral,
                reference: false, // Outputs are never reference inputs
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
        let redeemers = self
            .with_timeout(self.api.transactions_redeemers(tx_hash))
            .await?;

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
        let block = self
            .with_timeout(self.api.blocks_by_id(&height.to_string()))
            .await?;
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

            let txs = match self
                .with_timeout(self.api.blocks_txs(block_hash, pagination))
                .await
            {
                Ok(txs) => txs,
                Err(e) => {
                    let error_str = format!("{e:?}");
                    if error_str.contains("404") || error_str.contains("Not Found") {
                        return Ok(all_txs);
                    }
                    // Handle 429 rate limit - return what we have
                    if error_str.contains("429") || error_str.contains("Too Many Requests") {
                        tracing::warn!(
                            "Rate limited while fetching block txs, returning {} txs",
                            all_txs.len()
                        );
                        break;
                    }
                    return Err(e);
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
    /// Returns the json_value from the Blockfrost response which contains the actual datum
    #[instrument(skip(self))]
    pub async fn get_redeemer_datum(
        &self,
        datum_hash: &str,
    ) -> Result<serde_json::Value, BlockfrostProviderError> {
        self.rate_limit().await;
        let datum = self
            .with_timeout(self.api.scripts_datum_hash(datum_hash))
            .await?;
        // Blockfrost returns the datum under "json_value" key
        if let Some(json_value) = datum.get("json_value") {
            Ok(json_value.clone())
        } else {
            // Fall back to the full response if json_value is not present
            Ok(datum)
        }
    }

    /// Get script address from hash
    pub fn script_hash_to_address(
        &self,
        script_hash: &str,
    ) -> Result<String, BlockfrostProviderError> {
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

fn script_hash_to_address(
    script_hash: &str,
    network: CardanoNetwork,
) -> Result<String, BlockfrostProviderError> {
    use pallas_addresses::Network;

    let hash_bytes = hex::decode(script_hash)
        .map_err(|e| BlockfrostProviderError::Deserialization(e.to_string()))?;

    let hash: [u8; 28] = hash_bytes.try_into().map_err(|_| {
        BlockfrostProviderError::Deserialization("Invalid script hash length".to_string())
    })?;

    let pallas_network = match network {
        CardanoNetwork::Mainnet => Network::Mainnet,
        CardanoNetwork::Preprod | CardanoNetwork::Preview => Network::Testnet,
    };

    crate::types::script_hash_bytes_to_address(&hash, pallas_network)
        .map_err(BlockfrostProviderError::Deserialization)
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
            collateral: false,
            reference: false,
        };

        assert!(utxo.has_asset("abc123", "def456"));
        assert!(!utxo.has_asset("abc123", "other"));
        assert_eq!(utxo.lovelace(), 5000000);
    }
}

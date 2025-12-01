use std::sync::Arc;

use crate::blockfrost_provider::{BlockfrostProvider, Utxo};
use crate::provider::CardanoProvider;
use crate::types::MultisigIsmDatum;
use crate::ConnectionConf;
use async_trait::async_trait;
use serde_json::Value;

use hyperlane_core::{
    ChainCommunicationError, ChainResult, ContractLocator, HyperlaneChain, HyperlaneContract,
    HyperlaneDomain, HyperlaneMessage, HyperlaneProvider, MultisigIsm, H256,
};

/// MultisigIsm contract on Cardano
#[derive(Debug)]
pub struct CardanoMultisigIsm {
    provider: Arc<BlockfrostProvider>,
    domain: HyperlaneDomain,
    conf: ConnectionConf,
    address: H256,
}

impl CardanoMultisigIsm {
    /// Create a new Cardano CardanoMultisigIsm
    pub fn new(conf: &ConnectionConf, locator: ContractLocator) -> Self {
        let provider = BlockfrostProvider::new(&conf.api_key, conf.network);
        Self {
            provider: Arc::new(provider),
            domain: locator.domain.clone(),
            conf: conf.clone(),
            address: locator.address,
        }
    }

    /// Find the ISM UTXO by its state NFT
    async fn find_ism_utxo(&self) -> ChainResult<Utxo> {
        use tracing::info;

        let ism_asset_name = ""; // Empty asset name for state NFT
        let nft_result = self.provider
            .find_utxo_by_nft(&self.conf.ism_policy_id, ism_asset_name)
            .await;

        match nft_result {
            Ok(utxo) => {
                info!("Found ISM UTXO by NFT: {}#{}", utxo.tx_hash, utxo.output_index);
                return Ok(utxo);
            }
            Err(e) => {
                // Log that NFT lookup failed, will try script address lookup
                info!(
                    "NFT lookup failed ({}), falling back to script address lookup",
                    e
                );
            }
        }

        // Fallback: Find UTXOs at the ISM script address
        // The ism_policy_id is actually the script hash
        let script_address = self.provider
            .script_hash_to_address(&self.conf.ism_policy_id)
            .map_err(|e| {
                ChainCommunicationError::from_other_str(&format!(
                    "Failed to compute ISM script address: {}",
                    e
                ))
            })?;

        info!("Looking up ISM UTXOs at script address: {}", script_address);

        let utxos = self.provider
            .get_utxos_at_address(&script_address)
            .await
            .map_err(|e| {
                ChainCommunicationError::from_other_str(&format!(
                    "Failed to get UTXOs at ISM address: {}",
                    e
                ))
            })?;

        // Find the first UTXO with an inline datum (the ISM state UTXO)
        for utxo in utxos {
            if utxo.inline_datum.is_some() {
                info!(
                    "Found ISM UTXO by script address: {}#{}",
                    utxo.tx_hash, utxo.output_index
                );
                return Ok(utxo);
            }
        }

        Err(ChainCommunicationError::from_other_str(
            "No ISM UTXO found with inline datum at script address",
        ))
    }

    /// Parse ISM datum from UTXO
    ///
    /// Handles both JSON-formatted datum and raw CBOR hex from Blockfrost.
    /// If inline_datum is CBOR hex, fetches JSON representation via data_hash.
    async fn parse_ism_datum(&self, utxo: &Utxo) -> ChainResult<MultisigIsmDatum> {
        let inline_datum = utxo.inline_datum.as_ref().ok_or_else(|| {
            ChainCommunicationError::from_other_str("ISM UTXO has no inline datum")
        })?;

        // First try parsing as JSON (may already be JSON from some API responses)
        if let Ok(datum_json) = serde_json::from_str::<Value>(inline_datum) {
            return self.parse_ism_datum_json(&datum_json);
        }

        // If inline_datum is CBOR hex (starts with hex chars), fetch JSON via data_hash
        let data_hash = utxo.data_hash.as_ref().ok_or_else(|| {
            ChainCommunicationError::from_other_str(
                "ISM UTXO has CBOR datum but no data_hash for JSON lookup",
            )
        })?;

        tracing::debug!("Fetching ISM datum JSON via data_hash: {}", data_hash);
        let datum_json_str = self
            .provider
            .get_datum(data_hash)
            .await
            .map_err(|e| {
                ChainCommunicationError::from_other_str(&format!(
                    "Failed to fetch ISM datum JSON: {}",
                    e
                ))
            })?;

        let datum_json: Value = serde_json::from_str(&datum_json_str).map_err(|e| {
            ChainCommunicationError::from_other_str(&format!(
                "Failed to parse ISM datum JSON: {}",
                e
            ))
        })?;

        self.parse_ism_datum_json(&datum_json)
    }

    /// Parse ISM datum from Blockfrost's JSON format
    fn parse_ism_datum_json(&self, json: &Value) -> ChainResult<MultisigIsmDatum> {
        let fields = json
            .get("fields")
            .and_then(|f| f.as_array())
            .ok_or_else(|| {
                ChainCommunicationError::from_other_str("Invalid ISM datum: missing fields")
            })?;

        if fields.len() < 3 {
            return Err(ChainCommunicationError::from_other_str(
                "Invalid ISM datum: insufficient fields",
            ));
        }

        // Parse validators list (field 0)
        // Format: list of (domain, list of validator pubkeys)
        let empty_vec = vec![];
        let validators_list = fields
            .get(0)
            .and_then(|f| f.get("list"))
            .and_then(|l| l.as_array())
            .unwrap_or(&empty_vec);

        let mut validators = Vec::new();
        for entry in validators_list {
            let entry_fields = entry.get("fields").and_then(|f| f.as_array());
            if let Some(fields) = entry_fields {
                let domain = fields
                    .get(0)
                    .and_then(|d| d.get("int"))
                    .and_then(|i| i.as_u64())
                    .unwrap_or(0) as u32;

                let empty_pubkeys = vec![];
                let pubkeys_list = fields
                    .get(1)
                    .and_then(|p| p.get("list"))
                    .and_then(|l| l.as_array())
                    .unwrap_or(&empty_pubkeys);

                let mut pubkeys = Vec::new();
                for pk in pubkeys_list {
                    if let Some(pk_hex) = pk.get("bytes").and_then(|b| b.as_str()) {
                        if let Ok(pk_bytes) = hex::decode(pk_hex) {
                            if pk_bytes.len() == 32 {
                                let mut arr = [0u8; 32];
                                arr.copy_from_slice(&pk_bytes);
                                pubkeys.push(arr);
                            }
                        }
                    }
                }

                validators.push((domain, pubkeys));
            }
        }

        // Parse thresholds list (field 1)
        // Format: list of (domain, threshold)
        let empty_thresholds = vec![];
        let thresholds_list = fields
            .get(1)
            .and_then(|f| f.get("list"))
            .and_then(|l| l.as_array())
            .unwrap_or(&empty_thresholds);

        let mut thresholds = Vec::new();
        for entry in thresholds_list {
            let entry_fields = entry.get("fields").and_then(|f| f.as_array());
            if let Some(fields) = entry_fields {
                let domain = fields
                    .get(0)
                    .and_then(|d| d.get("int"))
                    .and_then(|i| i.as_u64())
                    .unwrap_or(0) as u32;

                let threshold = fields
                    .get(1)
                    .and_then(|t| t.get("int"))
                    .and_then(|i| i.as_u64())
                    .unwrap_or(0) as u32;

                thresholds.push((domain, threshold));
            }
        }

        // Parse owner (field 2)
        let owner_hex = fields
            .get(2)
            .and_then(|f| f.get("bytes"))
            .and_then(|b| b.as_str())
            .ok_or_else(|| {
                ChainCommunicationError::from_other_str("Invalid owner in ISM datum")
            })?;
        let owner_bytes = hex::decode(owner_hex).map_err(|e| {
            ChainCommunicationError::from_other_str(&format!("Failed to decode owner: {}", e))
        })?;
        let owner: [u8; 28] = owner_bytes.try_into().map_err(|_| {
            ChainCommunicationError::from_other_str("Invalid owner length")
        })?;

        Ok(MultisigIsmDatum {
            validators,
            thresholds,
            owner,
        })
    }
}

impl HyperlaneChain for CardanoMultisigIsm {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(CardanoProvider::new(&self.conf, self.domain.clone()))
    }
}

impl HyperlaneContract for CardanoMultisigIsm {
    fn address(&self) -> H256 {
        // On Cardano, this represents the MultisigIsm minting policy hash
        self.address
    }
}

#[async_trait]
impl MultisigIsm for CardanoMultisigIsm {
    /// Returns the validator and threshold needed to verify message
    async fn validators_and_threshold(
        &self,
        message: &HyperlaneMessage,
    ) -> ChainResult<(Vec<H256>, u8)> {
        // Find and parse the ISM UTXO
        let utxo = self.find_ism_utxo().await?;
        let datum = self.parse_ism_datum(&utxo).await?;

        // Get validators and threshold for the message's origin domain
        let origin_domain = message.origin;

        let validators = datum
            .validators
            .iter()
            .find(|(d, _)| *d == origin_domain)
            .map(|(_, v)| {
                v.iter()
                    .map(|pubkey| H256::from(*pubkey))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let threshold = datum
            .thresholds
            .iter()
            .find(|(d, _)| *d == origin_domain)
            .map(|(_, t)| *t as u8)
            .unwrap_or(0);

        Ok((validators, threshold))
    }
}

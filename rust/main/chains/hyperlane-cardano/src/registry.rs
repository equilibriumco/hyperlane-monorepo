use crate::blockfrost_provider::{BlockfrostProvider, BlockfrostProviderError, Utxo};
use crate::types::{
    AdditionalInput, RecipientRegistration, RecipientType, RegistryDatum, ScriptHash, UtxoLocator,
};
use serde_json::Value;
use std::collections::HashMap;
use thiserror::Error;
use tracing::{info, instrument};

#[derive(Error, Debug)]
pub enum RegistryError {
    #[error("Blockfrost error: {0}")]
    Blockfrost(#[from] BlockfrostProviderError),
    #[error("Registry UTXO not found")]
    RegistryNotFound,
    #[error("Invalid registry datum: {0}")]
    InvalidDatum(String),
    #[error("Recipient not registered: {0}")]
    RecipientNotFound(String),
    #[error("Deserialization error: {0}")]
    Deserialization(String),
}

/// Registry client for querying recipient registrations
pub struct RecipientRegistry {
    provider: BlockfrostProvider,
    registry_policy_id: String,
    registry_asset_name: String,
    /// Cached registrations
    cache: tokio::sync::RwLock<Option<RegistryCache>>,
}

struct RegistryCache {
    datum: RegistryDatum,
    utxo: Utxo,
    /// Map from script hash (hex) to registration
    by_script_hash: HashMap<String, RecipientRegistration>,
}

impl RecipientRegistry {
    /// Create a new registry client
    pub fn new(
        provider: BlockfrostProvider,
        registry_policy_id: String,
        registry_asset_name: String,
    ) -> Self {
        Self {
            provider,
            registry_policy_id,
            registry_asset_name,
            cache: tokio::sync::RwLock::new(None),
        }
    }

    /// Refresh the registry cache
    #[instrument(skip(self))]
    pub async fn refresh(&self) -> Result<(), RegistryError> {
        let utxo = self
            .provider
            .find_utxo_by_nft(&self.registry_policy_id, &self.registry_asset_name)
            .await?;

        let datum = self.parse_registry_datum(&utxo)?;

        // Build lookup map
        let mut by_script_hash = HashMap::new();
        for reg in &datum.registrations {
            let hash_hex = hex::encode(&reg.script_hash);
            by_script_hash.insert(hash_hex, reg.clone());
        }

        let cache = RegistryCache {
            datum,
            utxo,
            by_script_hash,
        };

        *self.cache.write().await = Some(cache);
        info!("Registry cache refreshed");
        Ok(())
    }

    /// Get a recipient registration by script hash
    #[instrument(skip(self))]
    pub async fn get_registration(
        &self,
        script_hash: &ScriptHash,
    ) -> Result<RecipientRegistration, RegistryError> {
        // Ensure cache is populated
        {
            let cache = self.cache.read().await;
            if cache.is_none() {
                drop(cache);
                self.refresh().await?;
            }
        }

        let cache = self.cache.read().await;
        let cache = cache.as_ref().ok_or(RegistryError::RegistryNotFound)?;

        let hash_hex = hex::encode(script_hash);
        cache
            .by_script_hash
            .get(&hash_hex)
            .cloned()
            .ok_or_else(|| RegistryError::RecipientNotFound(hash_hex))
    }

    /// Get all registrations
    #[instrument(skip(self))]
    pub async fn get_all_registrations(&self) -> Result<Vec<RecipientRegistration>, RegistryError> {
        // Ensure cache is populated
        {
            let cache = self.cache.read().await;
            if cache.is_none() {
                drop(cache);
                self.refresh().await?;
            }
        }

        let cache = self.cache.read().await;
        let cache = cache.as_ref().ok_or(RegistryError::RegistryNotFound)?;
        Ok(cache.datum.registrations.clone())
    }

    /// Get the registry UTXO
    #[instrument(skip(self))]
    pub async fn get_registry_utxo(&self) -> Result<Utxo, RegistryError> {
        // Ensure cache is populated
        {
            let cache = self.cache.read().await;
            if cache.is_none() {
                drop(cache);
                self.refresh().await?;
            }
        }

        let cache = self.cache.read().await;
        let cache = cache.as_ref().ok_or(RegistryError::RegistryNotFound)?;
        Ok(cache.utxo.clone())
    }

    /// Parse the registry datum from a UTXO
    fn parse_registry_datum(&self, utxo: &Utxo) -> Result<RegistryDatum, RegistryError> {
        let inline_datum = utxo
            .inline_datum
            .as_ref()
            .ok_or_else(|| RegistryError::InvalidDatum("No inline datum".to_string()))?;

        // Parse the CBOR datum
        // The datum is in Blockfrost's JSON format, we need to parse it
        let datum_json: Value = serde_json::from_str(inline_datum)
            .map_err(|e| RegistryError::Deserialization(e.to_string()))?;

        self.parse_registry_datum_json(&datum_json)
    }

    /// Parse registry datum from Blockfrost's JSON format
    fn parse_registry_datum_json(&self, json: &Value) -> Result<RegistryDatum, RegistryError> {
        // Blockfrost returns datum as JSON with Plutus data structure
        // Format: { "fields": [...], "constructor": N }

        let fields = json
            .get("fields")
            .and_then(|f| f.as_array())
            .ok_or_else(|| RegistryError::InvalidDatum("Missing fields".to_string()))?;

        if fields.len() < 2 {
            return Err(RegistryError::InvalidDatum(
                "Invalid registry datum structure".to_string(),
            ));
        }

        // Parse registrations list
        let registrations_json = fields
            .get(0)
            .and_then(|r| r.get("list"))
            .and_then(|l| l.as_array())
            .ok_or_else(|| RegistryError::InvalidDatum("Invalid registrations list".to_string()))?;

        let mut registrations = Vec::new();
        for reg_json in registrations_json {
            let reg = self.parse_registration_json(reg_json)?;
            registrations.push(reg);
        }

        // Parse owner
        let owner_hex = fields
            .get(1)
            .and_then(|o| o.get("bytes"))
            .and_then(|b| b.as_str())
            .ok_or_else(|| RegistryError::InvalidDatum("Invalid owner".to_string()))?;

        let owner_bytes =
            hex::decode(owner_hex).map_err(|e| RegistryError::Deserialization(e.to_string()))?;

        let owner: [u8; 28] = owner_bytes
            .try_into()
            .map_err(|_| RegistryError::InvalidDatum("Invalid owner length".to_string()))?;

        Ok(RegistryDatum {
            registrations,
            owner,
        })
    }

    /// Parse a single registration from JSON
    fn parse_registration_json(
        &self,
        json: &Value,
    ) -> Result<RecipientRegistration, RegistryError> {
        let fields = json
            .get("fields")
            .and_then(|f| f.as_array())
            .ok_or_else(|| {
                RegistryError::InvalidDatum("Invalid registration structure".to_string())
            })?;

        if fields.len() < 5 {
            return Err(RegistryError::InvalidDatum(
                "Invalid registration field count".to_string(),
            ));
        }

        // Script hash
        let script_hash_hex = fields
            .get(0)
            .and_then(|s| s.get("bytes"))
            .and_then(|b| b.as_str())
            .ok_or_else(|| RegistryError::InvalidDatum("Invalid script_hash".to_string()))?;

        let script_hash_bytes = hex::decode(script_hash_hex)
            .map_err(|e| RegistryError::Deserialization(e.to_string()))?;

        let script_hash: ScriptHash = script_hash_bytes
            .try_into()
            .map_err(|_| RegistryError::InvalidDatum("Invalid script_hash length".to_string()))?;

        // State locator
        let state_locator =
            self.parse_utxo_locator_json(fields.get(1).ok_or_else(|| {
                RegistryError::InvalidDatum("Missing state_locator".to_string())
            })?)?;

        // Additional inputs
        let empty_inputs = vec![];
        let additional_inputs_json = fields
            .get(2)
            .and_then(|a| a.get("list"))
            .and_then(|l| l.as_array())
            .unwrap_or(&empty_inputs);

        let mut additional_inputs = Vec::new();
        for input_json in additional_inputs_json {
            let input = self.parse_additional_input_json(input_json)?;
            additional_inputs.push(input);
        }

        // Recipient type
        let recipient_type =
            self.parse_recipient_type_json(fields.get(3).ok_or_else(|| {
                RegistryError::InvalidDatum("Missing recipient_type".to_string())
            })?)?;

        // Custom ISM (optional)
        let custom_ism = self.parse_optional_script_hash_json(
            fields
                .get(4)
                .ok_or_else(|| RegistryError::InvalidDatum("Missing custom_ism".to_string()))?,
        )?;

        Ok(RecipientRegistration {
            script_hash,
            state_locator,
            additional_inputs,
            recipient_type,
            custom_ism,
        })
    }

    /// Parse UTXO locator from JSON
    fn parse_utxo_locator_json(&self, json: &Value) -> Result<UtxoLocator, RegistryError> {
        let fields = json
            .get("fields")
            .and_then(|f| f.as_array())
            .ok_or_else(|| RegistryError::InvalidDatum("Invalid locator structure".to_string()))?;

        let policy_id = fields
            .get(0)
            .and_then(|p| p.get("bytes"))
            .and_then(|b| b.as_str())
            .ok_or_else(|| RegistryError::InvalidDatum("Invalid policy_id".to_string()))?
            .to_string();

        let asset_name = fields
            .get(1)
            .and_then(|a| a.get("bytes"))
            .and_then(|b| b.as_str())
            .ok_or_else(|| RegistryError::InvalidDatum("Invalid asset_name".to_string()))?
            .to_string();

        Ok(UtxoLocator {
            policy_id,
            asset_name,
        })
    }

    /// Parse additional input from JSON
    fn parse_additional_input_json(&self, json: &Value) -> Result<AdditionalInput, RegistryError> {
        let fields = json
            .get("fields")
            .and_then(|f| f.as_array())
            .ok_or_else(|| RegistryError::InvalidDatum("Invalid additional input".to_string()))?;

        let name = fields
            .get(0)
            .and_then(|n| n.get("bytes"))
            .and_then(|b| b.as_str())
            .map(|s| String::from_utf8_lossy(&hex::decode(s).unwrap_or_default()).to_string())
            .ok_or_else(|| RegistryError::InvalidDatum("Invalid input name".to_string()))?;

        let locator =
            self.parse_utxo_locator_json(fields.get(1).ok_or_else(|| {
                RegistryError::InvalidDatum("Missing input locator".to_string())
            })?)?;

        let must_be_spent = fields
            .get(2)
            .and_then(|m| m.get("constructor"))
            .and_then(|c| c.as_u64())
            .map(|c| c == 1) // constructor 1 = True, 0 = False
            .unwrap_or(false);

        Ok(AdditionalInput {
            name,
            locator,
            must_be_spent,
        })
    }

    /// Parse recipient type from JSON
    fn parse_recipient_type_json(&self, json: &Value) -> Result<RecipientType, RegistryError> {
        let constructor = json
            .get("constructor")
            .and_then(|c| c.as_u64())
            .ok_or_else(|| RegistryError::InvalidDatum("Invalid recipient type".to_string()))?;

        match constructor {
            0 => Ok(RecipientType::GenericHandler),
            1 => {
                let fields = json
                    .get("fields")
                    .and_then(|f| f.as_array())
                    .ok_or_else(|| {
                        RegistryError::InvalidDatum("Invalid TokenReceiver fields".to_string())
                    })?;

                let vault_locator = if let Some(vault) = fields.get(0) {
                    self.parse_optional_locator_json(vault)?
                } else {
                    None
                };

                let minting_policy = if let Some(policy) = fields.get(1) {
                    self.parse_optional_script_hash_json(policy)?
                } else {
                    None
                };

                Ok(RecipientType::TokenReceiver {
                    vault_locator,
                    minting_policy,
                })
            }
            2 => {
                let fields = json
                    .get("fields")
                    .and_then(|f| f.as_array())
                    .ok_or_else(|| {
                        RegistryError::InvalidDatum("Invalid ContractCaller fields".to_string())
                    })?;

                let target_locator =
                    self.parse_utxo_locator_json(fields.get(0).ok_or_else(|| {
                        RegistryError::InvalidDatum("Missing target locator".to_string())
                    })?)?;

                Ok(RecipientType::ContractCaller { target_locator })
            }
            _ => Err(RegistryError::InvalidDatum(format!(
                "Unknown recipient type constructor: {}",
                constructor
            ))),
        }
    }

    /// Parse optional UTXO locator
    fn parse_optional_locator_json(
        &self,
        json: &Value,
    ) -> Result<Option<UtxoLocator>, RegistryError> {
        let constructor = json.get("constructor").and_then(|c| c.as_u64());

        match constructor {
            Some(0) => Ok(None), // Nothing
            Some(1) => {
                // Just
                let fields = json.get("fields").and_then(|f| f.as_array());
                if let Some(fields) = fields {
                    if let Some(locator_json) = fields.get(0) {
                        return Ok(Some(self.parse_utxo_locator_json(locator_json)?));
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    /// Parse optional script hash
    fn parse_optional_script_hash_json(
        &self,
        json: &Value,
    ) -> Result<Option<ScriptHash>, RegistryError> {
        let constructor = json.get("constructor").and_then(|c| c.as_u64());

        match constructor {
            Some(0) => Ok(None), // Nothing
            Some(1) => {
                // Just
                let fields = json.get("fields").and_then(|f| f.as_array());
                if let Some(fields) = fields {
                    if let Some(hash_json) = fields.get(0) {
                        let hash_hex =
                            hash_json
                                .get("bytes")
                                .and_then(|b| b.as_str())
                                .ok_or_else(|| {
                                    RegistryError::InvalidDatum("Invalid script hash".to_string())
                                })?;

                        let hash_bytes = hex::decode(hash_hex)
                            .map_err(|e| RegistryError::Deserialization(e.to_string()))?;

                        let hash: ScriptHash = hash_bytes.try_into().map_err(|_| {
                            RegistryError::InvalidDatum("Invalid script hash length".to_string())
                        })?;

                        return Ok(Some(hash));
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }
}

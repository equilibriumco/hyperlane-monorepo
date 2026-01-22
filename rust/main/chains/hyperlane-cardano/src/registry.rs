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
            info!(
                "Registry: parsed registration script_hash={}, recipient_type={:?}",
                hash_hex, reg.recipient_type
            );
            by_script_hash.insert(hash_hex, reg.clone());
        }

        let num_registrations = by_script_hash.len();
        let cache = RegistryCache {
            datum,
            utxo,
            by_script_hash,
        };

        *self.cache.write().await = Some(cache);
        info!(
            "Registry cache refreshed with {} registrations",
            num_registrations
        );
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
    ///
    /// Handles both JSON-formatted datum and raw CBOR hex from Blockfrost.
    fn parse_registry_datum(&self, utxo: &Utxo) -> Result<RegistryDatum, RegistryError> {
        let inline_datum = utxo
            .inline_datum
            .as_ref()
            .ok_or_else(|| RegistryError::InvalidDatum("No inline datum".to_string()))?;

        // First try parsing as JSON object with expected structure
        if let Ok(datum_json) = serde_json::from_str::<Value>(inline_datum) {
            // Only use JSON parsing if it's an object with "fields" key
            if datum_json.get("fields").is_some() {
                return self.parse_registry_datum_json(&datum_json);
            }
        }

        // Blockfrost returns inline_datum as a raw CBOR hex string (quoted in JSON)
        // Strip quotes if present and decode from CBOR
        let hex_str = inline_datum.trim_matches('"');
        tracing::debug!(
            "Parsing registry datum from CBOR hex: {}...",
            &hex_str[..hex_str.len().min(40)]
        );
        self.parse_registry_datum_from_cbor(hex_str)
    }

    /// Parse registry datum from raw CBOR hex string
    fn parse_registry_datum_from_cbor(
        &self,
        hex_str: &str,
    ) -> Result<RegistryDatum, RegistryError> {
        use pallas_codec::minicbor;
        use pallas_primitives::conway::PlutusData;

        let cbor_bytes =
            hex::decode(hex_str).map_err(|e| RegistryError::Deserialization(e.to_string()))?;

        let plutus_data: PlutusData = minicbor::decode(&cbor_bytes)
            .map_err(|e| RegistryError::Deserialization(format!("CBOR decode error: {}", e)))?;

        // Registry datum structure: Constr 0 [registrations_list, owner]
        let (tag, fields) = match &plutus_data {
            PlutusData::Constr(c) => (c.tag, &c.fields),
            _ => {
                return Err(RegistryError::InvalidDatum(
                    "Registry datum is not a Constr".to_string(),
                ))
            }
        };

        if tag != 121 {
            // Tag 121 = Constr 0
            return Err(RegistryError::InvalidDatum(format!(
                "Registry datum has wrong constructor tag: {} (expected 121)",
                tag
            )));
        }

        let fields: Vec<_> = fields.iter().collect();
        if fields.len() < 2 {
            return Err(RegistryError::InvalidDatum(format!(
                "Registry datum has {} fields, expected at least 2",
                fields.len()
            )));
        }

        // Parse registrations list (field 0)
        let registrations = self.parse_registrations_from_plutus(&fields[0])?;

        // Parse owner (field 1): 28-byte pubkey hash
        let owner = self.parse_owner_from_plutus(&fields[1])?;

        tracing::debug!(
            "Parsed registry datum: {} registrations",
            registrations.len()
        );

        Ok(RegistryDatum {
            registrations,
            owner,
        })
    }

    /// Parse registrations list from PlutusData
    fn parse_registrations_from_plutus(
        &self,
        data: &pallas_primitives::conway::PlutusData,
    ) -> Result<Vec<RecipientRegistration>, RegistryError> {
        use pallas_primitives::conway::PlutusData;

        let list = match data {
            PlutusData::Array(arr) => arr.iter().collect::<Vec<_>>(),
            _ => {
                return Err(RegistryError::InvalidDatum(
                    "Registrations field is not a list".to_string(),
                ))
            }
        };

        let mut registrations = Vec::new();
        for entry in list {
            if let Ok(reg) = self.parse_registration_from_plutus(entry) {
                registrations.push(reg);
            }
        }

        Ok(registrations)
    }

    /// Parse a single registration from PlutusData
    ///
    /// Registration structure has 7 fields:
    /// - Field 0: script_hash (28-byte ByteArray)
    /// - Field 1: owner (28-byte VerificationKeyHash)
    /// - Field 2: state_locator (UtxoLocator)
    /// - Field 3: reference_script_locator (Option<UtxoLocator>)
    /// - Field 4: additional_inputs (List<AdditionalInput>)
    /// - Field 5: recipient_type (RecipientType)
    /// - Field 6: custom_ism (Option<ScriptHash>)
    fn parse_registration_from_plutus(
        &self,
        data: &pallas_primitives::conway::PlutusData,
    ) -> Result<RecipientRegistration, RegistryError> {
        use pallas_primitives::conway::PlutusData;

        let (tag, fields) = match data {
            PlutusData::Constr(c) => (c.tag, c.fields.iter().collect::<Vec<_>>()),
            _ => {
                return Err(RegistryError::InvalidDatum(
                    "Registration is not a Constr".to_string(),
                ))
            }
        };

        if tag != 121 || fields.len() < 7 {
            return Err(RegistryError::InvalidDatum(format!(
                "Invalid registration structure: expected 7 fields, got {}",
                fields.len()
            )));
        }

        // Script hash (field 0)
        let script_hash = match &fields[0] {
            PlutusData::BoundedBytes(bytes) => {
                let bytes: &[u8] = bytes.as_ref();
                if bytes.len() != 28 {
                    return Err(RegistryError::InvalidDatum(
                        "Invalid script_hash length".to_string(),
                    ));
                }
                let mut hash = [0u8; 28];
                hash.copy_from_slice(bytes);
                hash
            }
            _ => {
                return Err(RegistryError::InvalidDatum(
                    "Invalid script_hash".to_string(),
                ))
            }
        };

        // Owner (field 1) - VerificationKeyHash
        let owner = match &fields[1] {
            PlutusData::BoundedBytes(bytes) => {
                let bytes: &[u8] = bytes.as_ref();
                if bytes.len() != 28 {
                    return Err(RegistryError::InvalidDatum(
                        "Invalid owner length".to_string(),
                    ));
                }
                let mut hash = [0u8; 28];
                hash.copy_from_slice(bytes);
                hash
            }
            _ => return Err(RegistryError::InvalidDatum("Invalid owner".to_string())),
        };

        // State locator (field 2)
        let state_locator = self.parse_utxo_locator_from_plutus(&fields[2])?;

        // Reference script locator (field 3) - Option<UtxoLocator>
        let reference_script_locator = self.parse_optional_locator_from_plutus(&fields[3])?;

        // Additional inputs (field 4)
        let additional_inputs = match &fields[4] {
            PlutusData::Array(arr) => {
                let mut inputs = Vec::new();
                for input in arr.iter() {
                    if let Ok(ai) = self.parse_additional_input_from_plutus(input) {
                        inputs.push(ai);
                    }
                }
                inputs
            }
            _ => Vec::new(),
        };

        // Recipient type (field 5) - Constr tag determines type
        let recipient_type = self.parse_recipient_type_from_plutus(&fields[5])?;

        // Custom ISM (field 6)
        let custom_ism = match &fields[6] {
            PlutusData::Constr(c) => {
                // Some(ism_hash) = Constr 0 [bytes], None = Constr 1 []
                if c.tag == 121 {
                    // Constr 0 = Some
                    if let Some(PlutusData::BoundedBytes(bytes)) = c.fields.first() {
                        let bytes: &[u8] = bytes.as_ref();
                        if bytes.len() == 28 {
                            let mut hash = [0u8; 28];
                            hash.copy_from_slice(bytes);
                            Some(hash)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
        };

        Ok(RecipientRegistration {
            script_hash,
            owner,
            state_locator,
            reference_script_locator,
            additional_inputs,
            recipient_type,
            custom_ism,
        })
    }

    /// Parse UtxoLocator from PlutusData
    fn parse_utxo_locator_from_plutus(
        &self,
        data: &pallas_primitives::conway::PlutusData,
    ) -> Result<UtxoLocator, RegistryError> {
        use pallas_primitives::conway::PlutusData;

        let (tag, fields) = match data {
            PlutusData::Constr(c) => (c.tag, c.fields.iter().collect::<Vec<_>>()),
            _ => {
                return Err(RegistryError::InvalidDatum(
                    "Invalid UtxoLocator".to_string(),
                ))
            }
        };

        if tag != 121 || fields.len() < 2 {
            return Err(RegistryError::InvalidDatum(
                "Invalid UtxoLocator structure".to_string(),
            ));
        }

        let policy_id = match &fields[0] {
            PlutusData::BoundedBytes(bytes) => {
                let slice: &[u8] = bytes.as_ref();
                hex::encode(slice)
            }
            _ => return Err(RegistryError::InvalidDatum("Invalid policy_id".to_string())),
        };

        let asset_name = match &fields[1] {
            PlutusData::BoundedBytes(bytes) => {
                let slice: &[u8] = bytes.as_ref();
                hex::encode(slice)
            }
            _ => {
                return Err(RegistryError::InvalidDatum(
                    "Invalid asset_name".to_string(),
                ))
            }
        };

        Ok(UtxoLocator {
            policy_id,
            asset_name,
        })
    }

    /// Parse AdditionalInput from PlutusData
    fn parse_additional_input_from_plutus(
        &self,
        data: &pallas_primitives::conway::PlutusData,
    ) -> Result<AdditionalInput, RegistryError> {
        use pallas_primitives::conway::PlutusData;

        let (tag, fields) = match data {
            PlutusData::Constr(c) => (c.tag, c.fields.iter().collect::<Vec<_>>()),
            _ => {
                return Err(RegistryError::InvalidDatum(
                    "Invalid AdditionalInput".to_string(),
                ))
            }
        };

        if tag != 121 || fields.len() < 3 {
            return Err(RegistryError::InvalidDatum(
                "Invalid AdditionalInput structure".to_string(),
            ));
        }

        // name (field 0) - bytes decoded as UTF-8 string
        let name = match &fields[0] {
            PlutusData::BoundedBytes(bytes) => {
                let slice: &[u8] = bytes.as_ref();
                String::from_utf8_lossy(slice).to_string()
            }
            _ => {
                return Err(RegistryError::InvalidDatum(
                    "Invalid input name".to_string(),
                ))
            }
        };

        let locator = self.parse_utxo_locator_from_plutus(&fields[1])?;

        // must_be_spent is Constr 1 for True, Constr 0 for False
        let must_be_spent = match &fields[2] {
            PlutusData::Constr(c) => c.tag == 122, // Constr 1 = True
            _ => false,
        };

        Ok(AdditionalInput {
            name,
            locator,
            must_be_spent,
        })
    }

    /// Parse RecipientType from PlutusData
    fn parse_recipient_type_from_plutus(
        &self,
        data: &pallas_primitives::conway::PlutusData,
    ) -> Result<RecipientType, RegistryError> {
        use pallas_primitives::conway::PlutusData;

        let (tag, fields) = match data {
            PlutusData::Constr(c) => (c.tag, c.fields.iter().collect::<Vec<_>>()),
            _ => return Ok(RecipientType::Generic),
        };

        match tag {
            121 => Ok(RecipientType::Generic), // Constr 0
            122 => {
                // TokenReceiver - Constr 1 [vault_locator, minting_policy]
                let vault_locator = if fields.len() > 0 {
                    self.parse_optional_locator_from_plutus(&fields[0])?
                } else {
                    None
                };
                let minting_policy = if fields.len() > 1 {
                    self.parse_optional_script_hash_from_plutus(&fields[1])?
                } else {
                    None
                };
                Ok(RecipientType::TokenReceiver {
                    vault_locator,
                    minting_policy,
                })
            }
            123 => {
                // Deferred - Constr 2 [message_policy]
                if fields.is_empty() {
                    return Err(RegistryError::InvalidDatum(
                        "Deferred missing message_policy".to_string(),
                    ));
                }
                let message_policy = match &fields[0] {
                    PlutusData::BoundedBytes(bytes) => {
                        let bytes: &[u8] = bytes.as_ref();
                        if bytes.len() != 28 {
                            return Err(RegistryError::InvalidDatum(
                                "Invalid message_policy length".to_string(),
                            ));
                        }
                        let mut hash = [0u8; 28];
                        hash.copy_from_slice(bytes);
                        hash
                    }
                    _ => {
                        return Err(RegistryError::InvalidDatum(
                            "Invalid message_policy".to_string(),
                        ))
                    }
                };
                Ok(RecipientType::Deferred { message_policy })
            }
            _ => Ok(RecipientType::Generic),
        }
    }

    /// Parse optional UtxoLocator from PlutusData (Option type)
    fn parse_optional_locator_from_plutus(
        &self,
        data: &pallas_primitives::conway::PlutusData,
    ) -> Result<Option<UtxoLocator>, RegistryError> {
        use pallas_primitives::conway::PlutusData;

        match data {
            PlutusData::Constr(c) => {
                if c.tag == 121 {
                    // Some
                    if let Some(inner) = c.fields.first() {
                        Ok(Some(self.parse_utxo_locator_from_plutus(inner)?))
                    } else {
                        Ok(None)
                    }
                } else {
                    Ok(None) // None
                }
            }
            _ => Ok(None),
        }
    }

    /// Parse optional ScriptHash from PlutusData (Option type)
    fn parse_optional_script_hash_from_plutus(
        &self,
        data: &pallas_primitives::conway::PlutusData,
    ) -> Result<Option<ScriptHash>, RegistryError> {
        use pallas_primitives::conway::PlutusData;

        match data {
            PlutusData::Constr(c) => {
                if c.tag == 121 {
                    // Some
                    if let Some(PlutusData::BoundedBytes(bytes)) = c.fields.first() {
                        let slice: &[u8] = bytes.as_ref();
                        if slice.len() == 28 {
                            let mut hash = [0u8; 28];
                            hash.copy_from_slice(slice);
                            return Ok(Some(hash));
                        }
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    /// Parse owner from PlutusData
    fn parse_owner_from_plutus(
        &self,
        data: &pallas_primitives::conway::PlutusData,
    ) -> Result<[u8; 28], RegistryError> {
        use pallas_primitives::conway::PlutusData;

        match data {
            PlutusData::BoundedBytes(bytes) => {
                let bytes: &[u8] = bytes.as_ref();
                if bytes.len() != 28 {
                    return Err(RegistryError::InvalidDatum(format!(
                        "Owner has wrong length: {} (expected 28)",
                        bytes.len()
                    )));
                }
                let mut owner = [0u8; 28];
                owner.copy_from_slice(bytes);
                Ok(owner)
            }
            _ => Err(RegistryError::InvalidDatum(
                "Owner field is not bytes".to_string(),
            )),
        }
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
    ///
    /// Registration structure (7 fields):
    /// - Field 0: script_hash (28-byte ByteArray)
    /// - Field 1: owner (28-byte VerificationKeyHash)
    /// - Field 2: state_locator (UtxoLocator)
    /// - Field 3: reference_script_locator (Option<UtxoLocator>)
    /// - Field 4: additional_inputs (List<AdditionalInput>)
    /// - Field 5: recipient_type (RecipientType)
    /// - Field 6: custom_ism (Option<ScriptHash>)
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

        if fields.len() < 7 {
            return Err(RegistryError::InvalidDatum(format!(
                "Invalid registration field count: expected 7, got {}",
                fields.len()
            )));
        }

        // Field 0: Script hash
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

        // Field 1: Owner (VerificationKeyHash)
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

        // Field 2: State locator
        let state_locator =
            self.parse_utxo_locator_json(fields.get(2).ok_or_else(|| {
                RegistryError::InvalidDatum("Missing state_locator".to_string())
            })?)?;

        // Field 3: Reference script locator (Option<UtxoLocator>)
        let reference_script_locator =
            self.parse_optional_locator_json(fields.get(3).ok_or_else(|| {
                RegistryError::InvalidDatum("Missing reference_script_locator".to_string())
            })?)?;

        // Field 4: Additional inputs
        let empty_inputs = vec![];
        let additional_inputs_json = fields
            .get(4)
            .and_then(|a| a.get("list"))
            .and_then(|l| l.as_array())
            .unwrap_or(&empty_inputs);

        let mut additional_inputs = Vec::new();
        for input_json in additional_inputs_json {
            let input = self.parse_additional_input_json(input_json)?;
            additional_inputs.push(input);
        }

        // Field 5: Recipient type
        let recipient_type =
            self.parse_recipient_type_json(fields.get(5).ok_or_else(|| {
                RegistryError::InvalidDatum("Missing recipient_type".to_string())
            })?)?;

        // Field 6: Custom ISM (optional)
        let custom_ism = self.parse_optional_script_hash_json(
            fields
                .get(6)
                .ok_or_else(|| RegistryError::InvalidDatum("Missing custom_ism".to_string()))?,
        )?;

        Ok(RecipientRegistration {
            script_hash,
            owner,
            state_locator,
            reference_script_locator,
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
            0 => Ok(RecipientType::Generic),
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
                        RegistryError::InvalidDatum("Invalid Deferred fields".to_string())
                    })?;

                let message_policy_bytes = fields
                    .get(0)
                    .and_then(|v| v.get("bytes"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        RegistryError::InvalidDatum("Missing message_policy".to_string())
                    })?;

                let message_policy_vec = hex::decode(message_policy_bytes).map_err(|e| {
                    RegistryError::InvalidDatum(format!("Invalid message_policy hex: {}", e))
                })?;

                if message_policy_vec.len() != 28 {
                    return Err(RegistryError::InvalidDatum(
                        "message_policy must be 28 bytes".to_string(),
                    ));
                }

                let mut message_policy = [0u8; 28];
                message_policy.copy_from_slice(&message_policy_vec);

                Ok(RecipientType::Deferred { message_policy })
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

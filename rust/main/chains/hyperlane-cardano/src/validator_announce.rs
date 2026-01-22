use crate::blockfrost_provider::BlockfrostProvider;
use crate::provider::CardanoProvider;
use crate::ConnectionConf;
use async_trait::async_trait;
use ciborium::Value as CborValue;
use hyperlane_core::{
    Announcement, ChainCommunicationError, ChainResult, ContractLocator, FixedPointNumber,
    HyperlaneChain, HyperlaneContract, HyperlaneDomain, HyperlaneProvider, SignedType, TxOutcome,
    ValidatorAnnounce, H256, H512, U256,
};
use serde_json::Value as JsonValue;
use std::sync::Arc;
use tracing::{debug, info, warn};

#[derive(Debug)]
pub struct CardanoValidatorAnnounce {
    provider: Arc<BlockfrostProvider>,
    domain: HyperlaneDomain,
    conf: ConnectionConf,
    address: H256,
}

impl CardanoValidatorAnnounce {
    pub fn new(conf: &ConnectionConf, locator: ContractLocator) -> Self {
        let provider = BlockfrostProvider::new(&conf.api_key, conf.network);
        Self {
            provider: Arc::new(provider),
            domain: locator.domain.clone(),
            conf: conf.clone(),
            address: locator.address,
        }
    }

    /// Get the validator announce script address
    fn get_validator_announce_address(&self) -> ChainResult<String> {
        self.provider
            .script_hash_to_address(&self.conf.validator_announce_policy_id)
            .map_err(ChainCommunicationError::from_other)
    }

    /// Parse a validator announcement datum from Blockfrost's JSON format
    /// Returns (validator_bytes, storage_location)
    fn parse_announcement_datum_json(&self, json: &JsonValue) -> Option<(Vec<u8>, String)> {
        // ValidatorAnnounceDatum format:
        // { "constructor": 0, "fields": [validator, mailbox_policy_id, mailbox_domain, storage_location] }
        let fields = json.get("fields")?.as_array()?;
        if fields.len() < 4 {
            return None;
        }

        // Parse validator (20 or 32 bytes - pad 20-byte Ethereum addresses to 32 bytes)
        let validator_hex = fields.get(0)?.get("bytes")?.as_str()?;
        let validator_bytes = hex::decode(validator_hex).ok()?;
        let validator_bytes = Self::normalize_validator_bytes(validator_bytes)?;

        // Parse storage_location (bytes as UTF-8 string)
        let storage_location_hex = fields.get(3)?.get("bytes")?.as_str()?;
        let storage_location_bytes = hex::decode(storage_location_hex).ok()?;
        let storage_location = String::from_utf8(storage_location_bytes).ok()?;

        Some((validator_bytes, storage_location))
    }

    /// Parse a validator announcement datum from raw CBOR hex
    /// ValidatorAnnounceDatum CBOR structure: Constr 0 [validator, mailbox_policy_id, domain, storage_location]
    /// Uses ciborium for CBOR decoding which handles indefinite arrays and byte strings transparently
    fn parse_announcement_datum_cbor(&self, cbor_hex: &str) -> Option<(Vec<u8>, String)> {
        debug!("Parsing CBOR datum: {}", cbor_hex);
        let cbor_bytes = hex::decode(cbor_hex).ok()?;

        // Decode the entire CBOR structure using ciborium
        let value: CborValue = ciborium::from_reader(&cbor_bytes[..]).ok()?;
        debug!("Decoded CBOR value type: {:?}", value);

        // Extract the tagged value (Constr 0 = tag 121)
        let fields = match &value {
            CborValue::Tag(121, inner) => {
                // Tag 121 is Plutus Constr 0
                match inner.as_ref() {
                    CborValue::Array(arr) => arr,
                    _ => {
                        debug!("Expected array inside tag 121");
                        return None;
                    }
                }
            }
            _ => {
                debug!("Expected tag 121 (Constr 0), got: {:?}", value);
                return None;
            }
        };

        if fields.len() < 4 {
            debug!("Expected 4 fields, got {}", fields.len());
            return None;
        }

        // Field 0: validator address (20 or 32 bytes)
        let validator_raw = match &fields[0] {
            CborValue::Bytes(b) => b.clone(),
            _ => {
                debug!("Expected bytes for validator, got: {:?}", fields[0]);
                return None;
            }
        };
        debug!(
            "Validator raw bytes: {} bytes = {}",
            validator_raw.len(),
            hex::encode(&validator_raw)
        );

        let validator_bytes = match Self::normalize_validator_bytes(validator_raw) {
            Some(v) => v,
            None => {
                debug!("Failed to normalize validator bytes");
                return None;
            }
        };

        // Field 1: mailbox_policy_id (28 bytes) - skip (already validated structure)
        // Field 2: domain (integer) - skip (already validated structure)

        // Field 3: storage_location (bytes)
        let storage_bytes = match &fields[3] {
            CborValue::Bytes(b) => b.clone(),
            _ => {
                debug!("Expected bytes for storage_location, got: {:?}", fields[3]);
                return None;
            }
        };

        let storage_location = match String::from_utf8(storage_bytes) {
            Ok(s) => s,
            Err(e) => {
                debug!("Failed to convert storage location to UTF-8: {:?}", e);
                return None;
            }
        };

        debug!(
            "Successfully parsed announcement: validator={}, storage={}",
            hex::encode(&validator_bytes),
            storage_location
        );
        Some((validator_bytes, storage_location))
    }

    /// Normalize validator bytes to 32 bytes (pad 20-byte Ethereum addresses)
    fn normalize_validator_bytes(bytes: Vec<u8>) -> Option<Vec<u8>> {
        match bytes.len() {
            20 => {
                // Pad 20-byte Ethereum address to 32 bytes with leading zeros
                let mut padded = vec![0u8; 12];
                padded.extend_from_slice(&bytes);
                Some(padded)
            }
            32 => Some(bytes),
            _ => {
                debug!("Unexpected validator bytes length: {}", bytes.len());
                None
            }
        }
    }
}

impl HyperlaneContract for CardanoValidatorAnnounce {
    fn address(&self) -> H256 {
        // On Cardano, this represents the validator announce minting policy hash
        self.address
    }
}

impl HyperlaneChain for CardanoValidatorAnnounce {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(CardanoProvider::new(&self.conf, self.domain.clone()))
    }
}

#[async_trait]
impl ValidatorAnnounce for CardanoValidatorAnnounce {
    async fn get_announced_storage_locations(
        &self,
        validators: &[H256],
    ) -> ChainResult<Vec<Vec<String>>> {
        // On Cardano, validator storage locations are stored as UTXOs with datum
        // at the validator announce script address.
        //
        // The datum format (ValidatorAnnounceDatum) contains:
        // - validator: 32 bytes (padded verification key hash)
        // - mailbox_policy_id: 28 bytes
        // - mailbox_domain: u32
        // - storage_location: bytes (UTF-8 URL)

        debug!(
            "Looking up storage locations for {} validators",
            validators.len()
        );

        // Get the validator announce script address
        let va_address = match self.get_validator_announce_address() {
            Ok(addr) => addr,
            Err(e) => {
                warn!("Could not get validator announce address: {}", e);
                return Ok(validators.iter().map(|_| Vec::new()).collect());
            }
        };

        debug!("Validator announce address: {}", va_address);

        // Query all UTXOs at the validator announce address
        let utxos = match self.provider.get_utxos_at_address(&va_address).await {
            Ok(u) => u,
            Err(e) => {
                warn!("Could not fetch UTXOs at validator announce address: {}", e);
                return Ok(validators.iter().map(|_| Vec::new()).collect());
            }
        };

        info!("Found {} UTXOs at validator announce address", utxos.len());

        // Build a map of validator -> storage locations
        let mut announcements: std::collections::HashMap<H256, Vec<String>> =
            std::collections::HashMap::new();

        for utxo in utxos {
            // Parse inline datum - try JSON first, then CBOR
            if let Some(inline_datum) = &utxo.inline_datum {
                let parsed = if let Ok(datum_json) = serde_json::from_str::<JsonValue>(inline_datum)
                {
                    // JSON format (legacy or from some data providers)
                    self.parse_announcement_datum_json(&datum_json)
                } else {
                    // Raw CBOR hex format (Blockfrost)
                    self.parse_announcement_datum_cbor(inline_datum)
                };

                if let Some((validator_bytes, storage_location)) = parsed {
                    // Convert validator bytes to H256
                    let mut validator_arr = [0u8; 32];
                    validator_arr.copy_from_slice(&validator_bytes);
                    let validator_h256 = H256::from(validator_arr);

                    debug!(
                        "Found announcement for validator {}: {}",
                        hex::encode(validator_arr),
                        storage_location
                    );

                    announcements
                        .entry(validator_h256)
                        .or_default()
                        .push(storage_location);
                } else {
                    debug!("Could not parse inline datum: {}", inline_datum);
                }
            }
        }

        // Return storage locations for each requested validator in order
        let results: Vec<Vec<String>> = validators
            .iter()
            .map(|v| announcements.get(v).cloned().unwrap_or_default())
            .collect();

        info!(
            "Returning storage locations for {} validators, {} have announcements",
            validators.len(),
            results.iter().filter(|v| !v.is_empty()).count()
        );

        Ok(results)
    }

    async fn announce(&self, _announcement: SignedType<Announcement>) -> ChainResult<TxOutcome> {
        // Validator announcements on Cardano require:
        // 1. Building a transaction that creates a UTXO at the validator announce address
        // 2. Including the announcement data in the datum
        // 3. Signing and submitting the transaction
        //
        // This returns a no-op transaction until the transaction builder is implemented
        Ok(TxOutcome {
            transaction_id: H512::zero(),
            executed: false,
            gas_used: U256::zero(),
            gas_price: FixedPointNumber::zero(),
        })
    }

    async fn announce_tokens_needed(
        &self,
        _announcement: SignedType<Announcement>,
        _chain_signer: H256,
    ) -> Option<U256> {
        // Estimate the ADA needed for a validator announcement transaction
        // A typical announcement UTXO needs ~2 ADA for min UTXO + fees
        Some(U256::from(3_000_000u64)) // 3 ADA
    }
}

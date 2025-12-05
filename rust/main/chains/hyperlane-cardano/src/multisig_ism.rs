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

        // Asset name is configured from deployment info (e.g., "49534d205374617465" for "ISM State")
        let ism_asset_name = &self.conf.ism_asset_name_hex;
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
        let script_address = self.provider
            .script_hash_to_address(&self.conf.ism_script_hash)
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
    async fn parse_ism_datum(&self, utxo: &Utxo) -> ChainResult<MultisigIsmDatum> {
        let inline_datum = utxo.inline_datum.as_ref().ok_or_else(|| {
            ChainCommunicationError::from_other_str("ISM UTXO has no inline datum")
        })?;

        // First try parsing as JSON object with expected structure
        if let Ok(datum_json) = serde_json::from_str::<Value>(inline_datum) {
            // Only use JSON parsing if it's an object with "fields" key
            if datum_json.get("fields").is_some() {
                return self.parse_ism_datum_json(&datum_json);
            }
        }

        // Blockfrost returns inline_datum as a raw CBOR hex string (quoted in JSON)
        // Strip quotes if present and decode from CBOR
        let hex_str = inline_datum.trim_matches('"');

        tracing::debug!("Parsing ISM datum from CBOR hex: {}...", &hex_str[..hex_str.len().min(40)]);

        self.parse_ism_datum_from_cbor(hex_str)
    }

    /// Parse ISM datum from raw CBOR hex string
    fn parse_ism_datum_from_cbor(&self, hex_str: &str) -> ChainResult<MultisigIsmDatum> {
        use pallas_primitives::conway::PlutusData;
        use pallas_codec::minicbor;

        let cbor_bytes = hex::decode(hex_str).map_err(|e| {
            ChainCommunicationError::from_other_str(&format!(
                "Failed to decode ISM datum hex: {}",
                e
            ))
        })?;

        let plutus_data: PlutusData = minicbor::decode(&cbor_bytes).map_err(|e| {
            ChainCommunicationError::from_other_str(&format!(
                "Failed to decode ISM datum CBOR: {}",
                e
            ))
        })?;

        // ISM datum structure: Constr 0 [validators_list, thresholds_list, owner]
        let (tag, fields) = match &plutus_data {
            PlutusData::Constr(c) => (c.tag, &c.fields),
            _ => {
                return Err(ChainCommunicationError::from_other_str(
                    "ISM datum is not a Constr",
                ))
            }
        };

        if tag != 121 {
            // Tag 121 = Constr 0
            return Err(ChainCommunicationError::from_other_str(&format!(
                "ISM datum has wrong constructor tag: {} (expected 121)",
                tag
            )));
        }

        let fields: Vec<_> = fields.iter().collect();
        if fields.len() < 3 {
            return Err(ChainCommunicationError::from_other_str(&format!(
                "ISM datum has {} fields, expected 3",
                fields.len()
            )));
        }

        // Parse validators list (field 0): list of (domain, list of pubkeys)
        let validators = self.parse_validators_from_plutus(&fields[0])?;

        // Parse thresholds list (field 1): list of (domain, threshold)
        let thresholds = self.parse_thresholds_from_plutus(&fields[1])?;

        // Parse owner (field 2): 28-byte pubkey hash
        let owner = self.parse_owner_from_plutus(&fields[2])?;

        tracing::debug!(
            "Parsed ISM datum: {} validator entries, {} threshold entries",
            validators.len(),
            thresholds.len()
        );

        Ok(MultisigIsmDatum {
            validators,
            thresholds,
            owner,
        })
    }

    fn parse_validators_from_plutus(
        &self,
        data: &pallas_primitives::conway::PlutusData,
    ) -> ChainResult<Vec<(u32, Vec<[u8; 32]>)>> {
        use pallas_primitives::conway::PlutusData;

        let list = match data {
            PlutusData::Array(arr) => arr.iter().collect::<Vec<_>>(),
            _ => {
                return Err(ChainCommunicationError::from_other_str(
                    "Validators field is not a list",
                ))
            }
        };

        let mut validators = Vec::new();
        for entry in list {
            // Each entry is a tuple (domain, pubkeys_list)
            // In Aiken/Plutus, tuples are encoded as plain arrays [a, b], NOT as Constr 0
            // But we also support Constr 0 for backwards compatibility
            let fields: Vec<&PlutusData> = match entry {
                PlutusData::Array(arr) => arr.iter().collect(),
                PlutusData::Constr(c) if c.tag == 121 => c.fields.iter().collect(),
                _ => continue,
            };
            if fields.len() < 2 {
                continue;
            }

            // Parse domain (BigInt)
            let domain = match &fields[0] {
                PlutusData::BigInt(bi) => {
                    match bi {
                        pallas_primitives::conway::BigInt::Int(i) => {
                            let val: i128 = (*i).into();
                            val as u32
                        }
                        _ => continue,
                    }
                }
                _ => continue,
            };

            // Parse pubkeys list
            let pubkeys_list = match &fields[1] {
                PlutusData::Array(arr) => arr.iter().collect::<Vec<_>>(),
                _ => continue,
            };

            let mut eth_addresses = Vec::new();
            for pk in pubkeys_list {
                if let PlutusData::BoundedBytes(bytes) = pk {
                    let pk_bytes: &[u8] = bytes.as_ref();

                    if pk_bytes.len() == 33 {
                        // 33-byte compressed secp256k1 public key
                        // Derive Ethereum address: keccak256(uncompressed_pubkey)[12..32]
                        if let Some(eth_addr) = self.compressed_pubkey_to_eth_address(pk_bytes) {
                            eth_addresses.push(eth_addr);
                        }
                    } else if pk_bytes.len() == 20 {
                        // Already an Ethereum address (20 bytes), pad to 32 bytes
                        let mut arr = [0u8; 32];
                        arr[12..].copy_from_slice(pk_bytes);
                        eth_addresses.push(arr);
                    } else if pk_bytes.len() == 32 {
                        // 32-byte value (possibly already padded address)
                        let mut arr = [0u8; 32];
                        arr.copy_from_slice(pk_bytes);
                        eth_addresses.push(arr);
                    }
                }
            }

            validators.push((domain, eth_addresses));
        }

        Ok(validators)
    }

    /// Convert a 33-byte compressed secp256k1 public key to an Ethereum address (32 bytes, left-padded)
    fn compressed_pubkey_to_eth_address(&self, compressed: &[u8]) -> Option<[u8; 32]> {
        use k256::elliptic_curve::sec1::ToEncodedPoint;
        use k256::PublicKey;
        use sha3::{Digest, Keccak256};

        // Parse the compressed public key
        let pubkey = PublicKey::from_sec1_bytes(compressed).ok()?;

        // Get the uncompressed form (65 bytes: 04 || x || y)
        let uncompressed = pubkey.to_encoded_point(false);
        let uncompressed_bytes = uncompressed.as_bytes();

        if uncompressed_bytes.len() != 65 {
            return None;
        }

        // Keccak256 hash of the 64-byte public key (skip the 04 prefix)
        let hash = Keccak256::digest(&uncompressed_bytes[1..]);

        // Ethereum address is the last 20 bytes, left-padded to 32 bytes for H256
        let mut eth_addr = [0u8; 32];
        eth_addr[12..].copy_from_slice(&hash[12..]);

        tracing::debug!(
            "Derived Ethereum address 0x{} from compressed pubkey 0x{}",
            hex::encode(&eth_addr[12..]),
            hex::encode(compressed)
        );

        Some(eth_addr)
    }

    fn parse_thresholds_from_plutus(
        &self,
        data: &pallas_primitives::conway::PlutusData,
    ) -> ChainResult<Vec<(u32, u32)>> {
        use pallas_primitives::conway::PlutusData;

        let list = match data {
            PlutusData::Array(arr) => arr.iter().collect::<Vec<_>>(),
            _ => {
                return Err(ChainCommunicationError::from_other_str(
                    "Thresholds field is not a list",
                ))
            }
        };

        let mut thresholds = Vec::new();
        for entry in list {
            // Each entry is a tuple (domain, threshold)
            // In Aiken/Plutus, tuples are encoded as plain arrays [a, b], NOT as Constr 0
            // But we also support Constr 0 for backwards compatibility
            let fields: Vec<&PlutusData> = match entry {
                PlutusData::Array(arr) => arr.iter().collect(),
                PlutusData::Constr(c) if c.tag == 121 => c.fields.iter().collect(),
                _ => continue,
            };
            if fields.len() < 2 {
                continue;
            }

            // Parse domain
            let domain = match &fields[0] {
                PlutusData::BigInt(bi) => {
                    match bi {
                        pallas_primitives::conway::BigInt::Int(i) => {
                            let val: i128 = (*i).into();
                            val as u32
                        }
                        _ => continue,
                    }
                }
                _ => continue,
            };

            // Parse threshold
            let threshold = match &fields[1] {
                PlutusData::BigInt(bi) => {
                    match bi {
                        pallas_primitives::conway::BigInt::Int(i) => {
                            let val: i128 = (*i).into();
                            val as u32
                        }
                        _ => continue,
                    }
                }
                _ => continue,
            };

            thresholds.push((domain, threshold));
        }

        Ok(thresholds)
    }

    fn parse_owner_from_plutus(
        &self,
        data: &pallas_primitives::conway::PlutusData,
    ) -> ChainResult<[u8; 28]> {
        use pallas_primitives::conway::PlutusData;

        match data {
            PlutusData::BoundedBytes(bytes) => {
                let owner_bytes: &[u8] = bytes.as_ref();
                if owner_bytes.len() != 28 {
                    return Err(ChainCommunicationError::from_other_str(&format!(
                        "Owner has wrong length: {} (expected 28)",
                        owner_bytes.len()
                    )));
                }
                let mut owner = [0u8; 28];
                owner.copy_from_slice(owner_bytes);
                Ok(owner)
            }
            _ => Err(ChainCommunicationError::from_other_str(
                "Owner field is not bytes",
            )),
        }
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

                let mut eth_addresses = Vec::new();
                for pk in pubkeys_list {
                    if let Some(pk_hex) = pk.get("bytes").and_then(|b| b.as_str()) {
                        if let Ok(pk_bytes) = hex::decode(pk_hex) {
                            if pk_bytes.len() == 33 {
                                // 33-byte compressed secp256k1 public key
                                // Derive Ethereum address: keccak256(uncompressed_pubkey)[12..32]
                                if let Some(eth_addr) = self.compressed_pubkey_to_eth_address(&pk_bytes) {
                                    eth_addresses.push(eth_addr);
                                }
                            } else if pk_bytes.len() == 20 {
                                // Already an Ethereum address (20 bytes), pad to 32 bytes
                                let mut arr = [0u8; 32];
                                arr[12..].copy_from_slice(&pk_bytes);
                                eth_addresses.push(arr);
                            } else if pk_bytes.len() == 32 {
                                // 32-byte value (possibly already padded address)
                                let mut arr = [0u8; 32];
                                arr.copy_from_slice(&pk_bytes);
                                eth_addresses.push(arr);
                            }
                        }
                    }
                }

                validators.push((domain, eth_addresses));
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

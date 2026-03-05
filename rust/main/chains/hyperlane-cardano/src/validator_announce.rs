use crate::blockfrost_provider::{BlockfrostProvider, CardanoNetwork, Utxo};
use crate::cardano::Keypair;
use crate::provider::CardanoProvider;
use crate::ConnectionConf;
use async_trait::async_trait;
use ciborium::Value as CborValue;
use hyperlane_core::{
    Announcement, ChainCommunicationError, ChainResult, ContractLocator, FixedPointNumber,
    HyperlaneChain, HyperlaneContract, HyperlaneDomain, HyperlaneProvider, Signable, SignedType,
    TxOutcome, ValidatorAnnounce, H256, H512, U256,
};
use k256::ecdsa::{RecoveryId, Signature as EcdsaSignature, VerifyingKey};
use pallas_addresses::Network;
use pallas_codec::utils::MaybeIndefArray;
use pallas_crypto::hash::Hash;
use pallas_primitives::conway::{BigInt, Constr, PlutusData};
use pallas_txbuilder::{BuildConway, ExUnits, Output, ScriptKind, StagingTransaction};
use serde_json::Value as JsonValue;
use sha3::{Digest, Keccak256};
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::tx_builder::{parse_per_redeemer_ex_units, parse_utxo_ref};

/// Fee estimate for validator announce TX (ECDSA verify is expensive)
const VA_ESTIMATED_FEE: u64 = 2_000_000;
/// Minimum ADA for the announcement UTXO at the script address
const VA_MIN_UTXO: u64 = 2_000_000;
/// Minimum lovelace a UTXO must hold to be usable as a fee/collateral input.
/// Covers VA_ESTIMATED_FEE (~2 ADA) + VA_MIN_UTXO (~2 ADA) + headroom for
/// change output min-UTXO. Cannot be computed dynamically because
/// announce_tokens_needed() runs before any TX is built.
const VA_MIN_USABLE_UTXO: u64 = 5_000_000;
/// Default ExUnits for VA spend redeemer (ECDSA verification)
const VA_DEFAULT_MEM: u64 = 14_000_000;
const VA_DEFAULT_STEPS: u64 = 10_000_000_000;

fn get_plutus_v3_cost_model() -> Vec<i64> {
    vec![
        100788, 420, 1, 1, 1000, 173, 0, 1, 1000, 59957, 4, 1, 11183, 32, 201305, 8356, 4, 16000,
        100, 16000, 100, 16000, 100, 16000, 100, 16000, 100, 100, 100, 16000, 100, 94375, 32,
        132994, 32, 61462, 4, 72010, 178, 0, 1, 22151, 32, 91189, 769, 4, 2, 85848, 123203, 7305,
        -900, 1716, 549, 57, 85848, 0, 1, 1, 1000, 42921, 4, 2, 24548, 29498, 38, 1, 898148, 27279,
        1, 51775, 558, 1, 39184, 1000, 60594, 1, 141895, 32, 83150, 32, 15299, 32, 76049, 1, 13169,
        4, 22100, 10, 28999, 74, 1, 28999, 74, 1, 43285, 552, 1, 44749, 541, 1, 33852, 32, 68246,
        32, 72362, 32, 7243, 32, 7391, 32, 11546, 32, 85848, 123203, 7305, -900, 1716, 549, 57,
        85848, 0, 1, 90434, 519, 0, 1, 74433, 32, 85848, 123203, 7305, -900, 1716, 549, 57, 85848,
        0, 1, 1, 85848, 123203, 7305, -900, 1716, 549, 57, 85848, 0, 1, 955506, 213312, 0, 2,
        270652, 22588, 4, 1457325, 64566, 4, 20467, 1, 4, 0, 141992, 32, 100788, 420, 1, 1, 81663,
        32, 59498, 32, 20142, 32, 24588, 32, 20744, 32, 25933, 32, 24623, 32, 43053543, 10,
        53384111, 14333, 10, 43574283, 26308, 10, 16000, 100, 16000, 100, 962335, 18, 2780678, 6,
        442008, 1, 52538055, 3756, 18, 267929, 18, 76433006, 8868, 18, 52948122, 18, 1995836, 36,
        3227919, 12, 901022, 1, 166917843, 4307, 36, 284546, 36, 158221314, 26549, 36, 74698472,
        36, 333849714, 1, 254006273, 72, 2174038, 72, 2261318, 64571, 4, 207616, 8310, 4, 1293828,
        28716, 63, 0, 1, 1006041, 43623, 251, 0, 1,
    ]
}

#[derive(Debug)]
pub struct CardanoValidatorAnnounce {
    provider: Arc<BlockfrostProvider>,
    domain: HyperlaneDomain,
    conf: ConnectionConf,
    address: H256,
    signer: Option<Keypair>,
}

impl CardanoValidatorAnnounce {
    pub fn new(conf: &ConnectionConf, locator: ContractLocator, signer: Option<Keypair>) -> Self {
        let provider =
            BlockfrostProvider::new(&conf.api_key, conf.network, conf.confirmation_block_delay);
        Self {
            provider: Arc::new(provider),
            domain: locator.domain.clone(),
            conf: conf.clone(),
            address: locator.address,
            signer,
        }
    }

    /// Get the validator announce script address
    fn get_validator_announce_address(&self) -> ChainResult<String> {
        self.provider
            .script_hash_to_address(&self.conf.validator_announce_policy_id)
            .map_err(ChainCommunicationError::from_other)
    }

    fn pallas_network(&self) -> Network {
        match self.conf.network {
            CardanoNetwork::Mainnet => Network::Mainnet,
            CardanoNetwork::Preprod | CardanoNetwork::Preview => Network::Testnet,
        }
    }

    fn network_id(&self) -> u8 {
        match self.conf.network {
            CardanoNetwork::Mainnet => 1,
            CardanoNetwork::Preprod | CardanoNetwork::Preview => 0,
        }
    }

    /// Parse a validator announcement datum from Blockfrost's JSON format
    /// Returns (validator_bytes, storage_location)
    fn parse_announcement_datum_json(&self, json: &JsonValue) -> Option<(Vec<u8>, String)> {
        let fields = json.get("fields")?.as_array()?;
        if fields.len() < 4 {
            return None;
        }

        let validator_hex = fields.first()?.get("bytes")?.as_str()?;
        let validator_bytes = hex::decode(validator_hex).ok()?;
        let validator_bytes = Self::normalize_validator_bytes(validator_bytes)?;

        let storage_location_hex = fields.get(3)?.get("bytes")?.as_str()?;
        let storage_location_bytes = hex::decode(storage_location_hex).ok()?;
        let storage_location = String::from_utf8(storage_location_bytes).ok()?;

        Some((validator_bytes, storage_location))
    }

    /// Parse a validator announcement datum from raw CBOR hex
    fn parse_announcement_datum_cbor(&self, cbor_hex: &str) -> Option<(Vec<u8>, String)> {
        debug!("Parsing CBOR datum: {}", cbor_hex);
        let cbor_bytes = hex::decode(cbor_hex).ok()?;

        let value: CborValue = ciborium::from_reader(&cbor_bytes[..]).ok()?;
        debug!("Decoded CBOR value type: {:?}", value);

        let fields = match &value {
            CborValue::Tag(121, inner) => match inner.as_ref() {
                CborValue::Array(arr) => arr,
                _ => {
                    debug!("Expected array inside tag 121");
                    return None;
                }
            },
            _ => {
                debug!("Expected tag 121 (Constr 0), got: {:?}", value);
                return None;
            }
        };

        if fields.len() < 4 {
            debug!("Expected 4 fields, got {}", fields.len());
            return None;
        }

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

    /// Find an existing announcement UTXO from this validator at the VA address
    async fn find_existing_announcement(
        &self,
        va_address: &str,
        validator_eth_addr: &[u8; 20],
    ) -> ChainResult<Option<Utxo>> {
        let utxos = self
            .provider
            .get_utxos_at_address(va_address)
            .await
            .map_err(ChainCommunicationError::from_other)?;

        for utxo in utxos {
            if let Some(ref inline_datum) = utxo.inline_datum {
                let parsed = if let Ok(datum_json) = serde_json::from_str::<JsonValue>(inline_datum)
                {
                    self.parse_announcement_datum_json(&datum_json)
                } else {
                    self.parse_announcement_datum_cbor(inline_datum)
                };

                if let Some((validator_bytes, _)) = parsed {
                    // Compare the last 20 bytes (Ethereum address portion)
                    let stored_eth_addr = if validator_bytes.len() == 32 {
                        &validator_bytes[12..32]
                    } else {
                        &validator_bytes[..]
                    };
                    if stored_eth_addr == validator_eth_addr {
                        return Ok(Some(utxo));
                    }
                }
            }
        }
        Ok(None)
    }

    /// Find a bare UTXO (no datum) at the script address, suitable as a seed
    async fn find_bare_utxo(&self, va_address: &str) -> ChainResult<Option<Utxo>> {
        let utxos = self
            .provider
            .get_utxos_at_address(va_address)
            .await
            .map_err(ChainCommunicationError::from_other)?;

        for utxo in utxos {
            if utxo.inline_datum.is_none() && utxo.lovelace() >= VA_MIN_UTXO {
                return Ok(Some(utxo));
            }
        }
        Ok(None)
    }

    /// Create a seed UTXO at the VA script address so the announce TX can spend it
    async fn create_seed_utxo(&self, payer: &Keypair, va_address: &str) -> ChainResult<String> {
        let payer_address = payer.address_bech32(self.pallas_network());
        let payer_utxos = self
            .provider
            .get_utxos_at_address(&payer_address)
            .await
            .map_err(ChainCommunicationError::from_other)?;

        let fee_utxo = payer_utxos
            .iter()
            .find(|u| {
                u.lovelace() >= VA_MIN_USABLE_UTXO
                    && u.value.len() <= 1
                    && u.reference_script_hash.is_none()
            })
            .ok_or_else(|| {
                ChainCommunicationError::from_other_str(
                    "No suitable UTXO for seed transaction (need 5+ ADA pure)",
                )
            })?;

        let fee_tx_hash = Self::parse_tx_hash(&fee_utxo.tx_hash)?;
        let script_addr = pallas_addresses::Address::from_bech32(va_address).map_err(|e| {
            ChainCommunicationError::from_other_str(&format!("Invalid VA address: {e:?}"))
        })?;
        let payer_addr = pallas_addresses::Address::from_bech32(&payer_address).map_err(|e| {
            ChainCommunicationError::from_other_str(&format!("Invalid payer address: {e:?}"))
        })?;

        let seed_amount = VA_MIN_UTXO;
        let fee_estimate = 250_000u64;
        let change = fee_utxo
            .lovelace()
            .saturating_sub(seed_amount)
            .saturating_sub(fee_estimate);

        let current_slot = self
            .provider
            .get_latest_slot()
            .await
            .map_err(ChainCommunicationError::from_other)?;

        let mut staging = StagingTransaction::new()
            .input(pallas_txbuilder::Input::new(
                Hash::new(fee_tx_hash),
                fee_utxo.output_index as u64,
            ))
            .output(Output::new(script_addr, seed_amount))
            .fee(fee_estimate)
            .invalid_from_slot(current_slot + 7200)
            .network_id(self.network_id());

        if change >= 1_000_000 {
            staging = staging.output(Output::new(payer_addr, change));
        }

        let built = staging.build_conway_raw().map_err(|e| {
            ChainCommunicationError::from_other_str(&format!("Failed to build seed TX: {e:?}"))
        })?;

        let signature = payer.sign(&built.tx_hash.0);
        let signed = built
            .add_signature(*payer.pallas_public_key(), signature)
            .map_err(|e| {
                ChainCommunicationError::from_other_str(&format!("Failed to sign seed TX: {e:?}"))
            })?;

        let tx_hash = self
            .provider
            .submit_transaction(&signed.tx_bytes.0)
            .await
            .map_err(ChainCommunicationError::from_other)?;

        info!("Seed UTXO created: {tx_hash}");
        Ok(tx_hash)
    }

    fn parse_tx_hash(hex_str: &str) -> ChainResult<[u8; 32]> {
        let bytes = hex::decode(hex_str).map_err(|e| {
            ChainCommunicationError::from_other_str(&format!("Invalid tx hash hex: {e}"))
        })?;
        bytes
            .try_into()
            .map_err(|_| ChainCommunicationError::from_other_str("Tx hash must be 32 bytes"))
    }

    /// Build ValidatorAnnounceDatum as PlutusData CBOR
    /// Constr 0 [validator_address (20 bytes), mailbox_policy_id, mailbox_domain, storage_location]
    fn build_datum_cbor(
        validator_eth_addr: &[u8; 20],
        mailbox_policy_id: &str,
        mailbox_domain: u32,
        storage_location: &str,
    ) -> ChainResult<Vec<u8>> {
        let policy_bytes = hex::decode(mailbox_policy_id).map_err(|e| {
            ChainCommunicationError::from_other_str(&format!("Invalid policy ID hex: {e}"))
        })?;

        let fields = vec![
            PlutusData::BoundedBytes(validator_eth_addr.to_vec().into()),
            PlutusData::BoundedBytes(policy_bytes.into()),
            PlutusData::BigInt(BigInt::Int((mailbox_domain as i64).into())),
            PlutusData::BoundedBytes(storage_location.as_bytes().to_vec().into()),
        ];

        let constr = Constr {
            tag: 121, // Constr 0
            any_constructor: None,
            fields: MaybeIndefArray::Indef(fields),
        };

        pallas_codec::minicbor::to_vec(PlutusData::Constr(constr)).map_err(|e| {
            ChainCommunicationError::from_other_str(&format!("Failed to encode datum CBOR: {e}"))
        })
    }

    /// Build Announce redeemer as PlutusData CBOR
    /// Constr 0 [storage_location, compressed_pubkey, uncompressed_pubkey, signature]
    fn build_redeemer_cbor(
        storage_location: &str,
        compressed_pubkey: &[u8],
        uncompressed_pubkey: &[u8],
        signature: &[u8],
    ) -> ChainResult<Vec<u8>> {
        let fields = vec![
            PlutusData::BoundedBytes(storage_location.as_bytes().to_vec().into()),
            PlutusData::BoundedBytes(compressed_pubkey.to_vec().into()),
            PlutusData::BoundedBytes(uncompressed_pubkey.to_vec().into()),
            PlutusData::BoundedBytes(signature.to_vec().into()),
        ];

        let constr = Constr {
            tag: 121, // Constr 0 = Announce
            any_constructor: None,
            fields: MaybeIndefArray::Indef(fields),
        };

        pallas_codec::minicbor::to_vec(PlutusData::Constr(constr)).map_err(|e| {
            ChainCommunicationError::from_other_str(&format!("Failed to encode redeemer CBOR: {e}"))
        })
    }

    /// Recover the ECDSA public key from the announcement's signature.
    /// The signature in `SignedType<Announcement>` has r, s, v fields.
    /// Returns (compressed_pubkey_33bytes, uncompressed_pubkey_64bytes, eth_address_20bytes)
    fn recover_pubkey_from_announcement(
        announcement: &SignedType<Announcement>,
    ) -> ChainResult<([u8; 33], [u8; 64], [u8; 20])> {
        let sig = &announcement.signature;

        // Reconstruct the 64-byte r||s signature
        let mut sig_bytes = [0u8; 64];
        let mut r_bytes = [0u8; 32];
        sig.r.to_big_endian(&mut r_bytes);
        let mut s_bytes = [0u8; 32];
        sig.s.to_big_endian(&mut s_bytes);
        sig_bytes[..32].copy_from_slice(&r_bytes);
        sig_bytes[32..].copy_from_slice(&s_bytes);

        let ecdsa_sig = EcdsaSignature::from_slice(&sig_bytes).map_err(|e| {
            ChainCommunicationError::from_other_str(&format!("Invalid ECDSA signature: {e}"))
        })?;

        // The signing hash is the EIP-191 prefixed hash of the announcement
        let signing_hash = announcement.value.signing_hash();
        let eip191_prefix = b"\x19Ethereum Signed Message:\n32";
        let digest = Keccak256::new()
            .chain_update(eip191_prefix)
            .chain_update(signing_hash.as_bytes())
            .finalize();

        // Recovery ID from v: v=27 → 0, v=28 → 1
        let recovery_id = RecoveryId::try_from((sig.v as u8).wrapping_sub(27)).map_err(|e| {
            ChainCommunicationError::from_other_str(&format!(
                "Invalid recovery ID v={}: {e}",
                sig.v
            ))
        })?;

        let verifying_key = VerifyingKey::recover_from_prehash(&digest, &ecdsa_sig, recovery_id)
            .map_err(|e| {
                ChainCommunicationError::from_other_str(&format!(
                    "Failed to recover public key: {e}"
                ))
            })?;

        // Compressed pubkey (33 bytes with 0x02/0x03 prefix)
        let compressed = verifying_key.to_encoded_point(true);
        let compressed_bytes: [u8; 33] = compressed.as_bytes().try_into().map_err(|_| {
            ChainCommunicationError::from_other_str("Compressed pubkey not 33 bytes")
        })?;

        // Uncompressed pubkey without prefix (64 bytes: x || y)
        let uncompressed = verifying_key.to_encoded_point(false);
        let uncompressed_no_prefix: [u8; 64] =
            uncompressed.as_bytes()[1..].try_into().map_err(|_| {
                ChainCommunicationError::from_other_str("Uncompressed pubkey not 64 bytes")
            })?;

        // Derive Ethereum address: keccak256(uncompressed_pubkey)[12:32]
        let hash = Keccak256::digest(uncompressed_no_prefix);
        let mut eth_addr = [0u8; 20];
        eth_addr.copy_from_slice(&hash[12..32]);

        Ok((compressed_bytes, uncompressed_no_prefix, eth_addr))
    }

    #[allow(clippy::too_many_arguments)]
    async fn build_and_submit_announce_tx(
        &self,
        announcement: &SignedType<Announcement>,
        payer: &Keypair,
        script_utxo: &Utxo,
        va_address: &str,
        compressed_pubkey: &[u8; 33],
        uncompressed_pubkey: &[u8; 64],
        eth_addr: &[u8; 20],
    ) -> ChainResult<TxOutcome> {
        let sig = &announcement.signature;
        let storage_location = &announcement.value.storage_location;

        // Build r||s signature bytes (64 bytes, no recovery ID)
        let mut sig_bytes = [0u8; 64];
        let mut r_bytes = [0u8; 32];
        sig.r.to_big_endian(&mut r_bytes);
        let mut s_bytes = [0u8; 32];
        sig.s.to_big_endian(&mut s_bytes);
        sig_bytes[..32].copy_from_slice(&r_bytes);
        sig_bytes[32..].copy_from_slice(&s_bytes);

        // Build datum and redeemer CBOR
        let datum_cbor = Self::build_datum_cbor(
            eth_addr,
            &self.conf.mailbox_policy_id,
            announcement.value.mailbox_domain,
            storage_location,
        )?;

        let redeemer_cbor = Self::build_redeemer_cbor(
            storage_location,
            compressed_pubkey,
            uncompressed_pubkey,
            &sig_bytes,
        )?;

        // Parse addresses
        let va_addr = pallas_addresses::Address::from_bech32(va_address).map_err(|e| {
            ChainCommunicationError::from_other_str(&format!("Invalid VA address: {e:?}"))
        })?;
        let payer_address = payer.address_bech32(self.pallas_network());
        let payer_addr = pallas_addresses::Address::from_bech32(&payer_address).map_err(|e| {
            ChainCommunicationError::from_other_str(&format!("Invalid payer address: {e:?}"))
        })?;

        // Parse script UTXO hash
        let script_tx_hash = Self::parse_tx_hash(&script_utxo.tx_hash)?;

        // Get payer UTXOs for fees and collateral
        let payer_utxos = self
            .provider
            .get_utxos_at_address(&payer_address)
            .await
            .map_err(ChainCommunicationError::from_other)?;

        // Find collateral (pure ADA, no reference script)
        let collateral_utxo = payer_utxos
            .iter()
            .find(|u| {
                u.lovelace() >= VA_MIN_USABLE_UTXO
                    && u.value.len() <= 1
                    && u.reference_script_hash.is_none()
            })
            .ok_or_else(|| {
                ChainCommunicationError::from_other_str(
                    "No suitable collateral UTXO (need 5+ ADA pure)",
                )
            })?;

        // Find fee input (different from collateral if possible)
        let fee_utxo = payer_utxos
            .iter()
            .find(|u| {
                u.lovelace() >= VA_MIN_USABLE_UTXO
                    && u.value.len() <= 1
                    && u.reference_script_hash.is_none()
                    && (u.tx_hash != collateral_utxo.tx_hash
                        || u.output_index != collateral_utxo.output_index)
            })
            .unwrap_or(collateral_utxo);

        let collateral_tx_hash = Self::parse_tx_hash(&collateral_utxo.tx_hash)?;
        let fee_tx_hash = Self::parse_tx_hash(&fee_utxo.tx_hash)?;

        // Output lovelace (at least min UTXO)
        let output_lovelace = std::cmp::max(script_utxo.lovelace(), VA_MIN_UTXO);

        // Get current slot for validity
        let current_slot = self
            .provider
            .get_latest_slot()
            .await
            .map_err(ChainCommunicationError::from_other)?;

        // Build initial TX with placeholder ExUnits and fee
        let build_tx = |fee: u64, ex_units: ExUnits| -> ChainResult<Vec<u8>> {
            let va_output =
                Output::new(va_addr.clone(), output_lovelace).set_inline_datum(datum_cbor.clone());
            let change = fee_utxo.lovelace().saturating_sub(fee);

            let script_input = pallas_txbuilder::Input::new(
                Hash::new(script_tx_hash),
                script_utxo.output_index as u64,
            );

            let mut staging = StagingTransaction::new()
                .input(script_input.clone())
                .input(pallas_txbuilder::Input::new(
                    Hash::new(fee_tx_hash),
                    fee_utxo.output_index as u64,
                ))
                .collateral_input(pallas_txbuilder::Input::new(
                    Hash::new(collateral_tx_hash),
                    collateral_utxo.output_index as u64,
                ))
                .output(va_output)
                .add_spend_redeemer(script_input, redeemer_cbor.clone(), Some(ex_units))
                .fee(fee)
                .invalid_from_slot(current_slot + 7200)
                .network_id(self.network_id());

            // Add reference script UTXO or error
            if let Some(ref ref_utxo_str) = self.conf.validator_announce_reference_script_utxo {
                let ref_input = parse_utxo_ref(ref_utxo_str)
                    .map_err(|e| ChainCommunicationError::from_other_str(&e.to_string()))?;
                staging = staging.reference_input(ref_input);
            } else {
                return Err(ChainCommunicationError::from_other_str(
                    "validator_announce_reference_script_utxo is required for announce()",
                ));
            }

            // Cost model
            staging = staging.language_view(ScriptKind::PlutusV3, get_plutus_v3_cost_model());

            // Disclosed signer
            let payer_hash: Hash<28> = Hash::new(*payer.payment_credential_hash());
            staging = staging.disclosed_signer(payer_hash);

            // Change output
            if change >= 1_000_000 {
                staging = staging.output(Output::new(payer_addr.clone(), change));
            }

            let built = staging.build_conway_raw().map_err(|e| {
                ChainCommunicationError::from_other_str(&format!(
                    "Failed to build announce TX: {e:?}"
                ))
            })?;

            let signature = payer.sign(&built.tx_hash.0);
            let signed = built
                .add_signature(*payer.pallas_public_key(), signature)
                .map_err(|e| {
                    ChainCommunicationError::from_other_str(&format!(
                        "Failed to sign announce TX: {e:?}"
                    ))
                })?;

            Ok(signed.tx_bytes.0.clone())
        };

        // First build with placeholder ExUnits for evaluation
        let signed_tx = build_tx(
            VA_ESTIMATED_FEE,
            ExUnits {
                mem: VA_DEFAULT_MEM,
                steps: VA_DEFAULT_STEPS,
            },
        )?;

        // Evaluate to get real ExUnits
        let (final_fee, final_ex_units) = match self.provider.evaluate_tx(&signed_tx).await {
            Ok(eval_result) => {
                match parse_per_redeemer_ex_units(&eval_result) {
                    Ok(ex_units_map) => {
                        // Use the spend:0 ExUnits (the VA script)
                        let (mem, steps) = ex_units_map
                            .get("spend:0")
                            .copied()
                            .unwrap_or((VA_DEFAULT_MEM, VA_DEFAULT_STEPS));
                        // Add 20% margin
                        let mem = mem * 12 / 10;
                        let steps = steps * 12 / 10;
                        // Compute real fee (rough estimate based on TX size)
                        let tx_size = signed_tx.len() as u64;
                        let fee = std::cmp::max(
                            200_000,
                            44 * tx_size
                                + 155_381
                                + ((mem as f64 * 0.0577) as u64)
                                + ((steps as f64 * 0.0000721) as u64),
                        );
                        info!("Evaluated VA TX: mem={mem}, steps={steps}, fee={fee}");
                        (fee, ExUnits { mem, steps })
                    }
                    Err(e) => {
                        warn!("Failed to parse evaluation result: {e}, using defaults");
                        (
                            VA_ESTIMATED_FEE,
                            ExUnits {
                                mem: VA_DEFAULT_MEM,
                                steps: VA_DEFAULT_STEPS,
                            },
                        )
                    }
                }
            }
            Err(e) => {
                warn!("TX evaluation failed: {e}, using default ExUnits and fee");
                (
                    VA_ESTIMATED_FEE,
                    ExUnits {
                        mem: VA_DEFAULT_MEM,
                        steps: VA_DEFAULT_STEPS,
                    },
                )
            }
        };

        // Rebuild with real fee and ExUnits
        let final_signed_tx = build_tx(final_fee, final_ex_units)?;

        // Submit
        let tx_hash = self
            .provider
            .submit_transaction(&final_signed_tx)
            .await
            .map_err(ChainCommunicationError::from_other)?;

        info!("Validator announce TX submitted: {tx_hash}");

        let hash_bytes = hex::decode(&tx_hash).unwrap_or_default();
        let mut tx_id_bytes = [0u8; 64];
        tx_id_bytes[..hash_bytes.len().min(32)]
            .copy_from_slice(&hash_bytes[..hash_bytes.len().min(32)]);

        Ok(TxOutcome {
            transaction_id: H512::from(tx_id_bytes),
            executed: true,
            gas_used: U256::from(final_fee),
            gas_price: FixedPointNumber::zero(),
        })
    }
}

impl HyperlaneContract for CardanoValidatorAnnounce {
    fn address(&self) -> H256 {
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
        debug!(
            "Looking up storage locations for {} validators",
            validators.len()
        );

        let va_address = match self.get_validator_announce_address() {
            Ok(addr) => addr,
            Err(e) => {
                warn!("Could not get validator announce address: {}", e);
                return Ok(validators.iter().map(|_| Vec::new()).collect());
            }
        };

        debug!("Validator announce address: {}", va_address);

        let utxos = match self.provider.get_utxos_at_address(&va_address).await {
            Ok(u) => u,
            Err(e) => {
                warn!("Could not fetch UTXOs at validator announce address: {}", e);
                return Ok(validators.iter().map(|_| Vec::new()).collect());
            }
        };

        info!("Found {} UTXOs at validator announce address", utxos.len());

        let mut announcements: std::collections::HashMap<H256, Vec<String>> =
            std::collections::HashMap::new();

        for utxo in utxos {
            if let Some(inline_datum) = &utxo.inline_datum {
                let parsed = if let Ok(datum_json) = serde_json::from_str::<JsonValue>(inline_datum)
                {
                    self.parse_announcement_datum_json(&datum_json)
                } else {
                    self.parse_announcement_datum_cbor(inline_datum)
                };

                if let Some((validator_bytes, storage_location)) = parsed {
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

    async fn announce(&self, announcement: SignedType<Announcement>) -> ChainResult<TxOutcome> {
        let payer = self.signer.as_ref().ok_or_else(|| {
            ChainCommunicationError::from_other_str(
                "Cardano signer not configured — cannot submit announce TX",
            )
        })?;

        if self.conf.validator_announce_reference_script_utxo.is_none() {
            return Err(ChainCommunicationError::from_other_str(
                "validator_announce_reference_script_utxo not configured — \
                 deploy the reference script first with 'deploy reference-scripts validator_announce'",
            ));
        }

        // Recover ECDSA public key and Ethereum address from the signature
        let (compressed_pubkey, uncompressed_pubkey, eth_addr) =
            Self::recover_pubkey_from_announcement(&announcement)?;

        info!(
            "Announcing validator 0x{} with storage: {}",
            hex::encode(eth_addr),
            announcement.value.storage_location
        );

        // Get the VA script address
        let va_address = self.get_validator_announce_address()?;

        // Check for existing announcement from this validator
        let existing = self
            .find_existing_announcement(&va_address, &eth_addr)
            .await?;

        // Find the script UTXO to spend
        let script_utxo = if let Some(existing_utxo) = existing {
            info!("Updating existing announcement");
            existing_utxo
        } else {
            // Check for bare UTXO
            match self.find_bare_utxo(&va_address).await? {
                Some(bare) => {
                    info!("Creating new announcement using bare UTXO");
                    bare
                }
                None => {
                    info!("No spendable UTXO at VA address, creating seed UTXO");
                    self.create_seed_utxo(payer, &va_address).await?;

                    // Wait for the seed UTXO to appear
                    let mut retries = 0;
                    loop {
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                        if let Some(utxo) = self.find_bare_utxo(&va_address).await? {
                            break utxo;
                        }
                        retries += 1;
                        if retries >= 12 {
                            return Err(ChainCommunicationError::from_other_str(
                                "Seed UTXO not found after 60s",
                            ));
                        }
                    }
                }
            }
        };

        self.build_and_submit_announce_tx(
            &announcement,
            payer,
            &script_utxo,
            &va_address,
            &compressed_pubkey,
            &uncompressed_pubkey,
            &eth_addr,
        )
        .await
    }

    async fn announce_tokens_needed(
        &self,
        _announcement: SignedType<Announcement>,
        _chain_signer: H256,
    ) -> Option<U256> {
        // Cardano's UTXO model makes precise pre-checks impractical:
        // announce() requires specific UTXO shapes (pure ADA, no reference
        // scripts) and may need to create a seed TX that consumes the only
        // usable UTXO. Always allow the attempt — announce() returns clear
        // errors on UTXO issues and the validator retries on the next loop.
        None
    }
}

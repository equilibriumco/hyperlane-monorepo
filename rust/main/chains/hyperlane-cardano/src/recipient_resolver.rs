use crate::blockfrost_provider::{
    BlockfrostProvider, BlockfrostProviderError, CardanoNetwork, Utxo,
};
use crate::types::{hyperlane_address_to_policy_id, hyperlane_address_to_script_hash, ScriptHash};
use pallas_codec::minicbor;
use pallas_primitives::conway::PlutusData;
use thiserror::Error;
use tracing::{debug, info, instrument};

#[derive(Error, Debug)]
pub enum ResolverError {
    #[error("Blockfrost error: {0}")]
    Blockfrost(#[from] BlockfrostProviderError),
    #[error("Invalid recipient address: {0}")]
    InvalidRecipient(String),
    #[error("Recipient UTXO not found for policy {0}")]
    RecipientNotFound(String),
    #[error("Invalid datum: {0}")]
    InvalidDatum(String),
}

/// How the relayer should handle a recipient
#[derive(Debug, Clone)]
pub enum RecipientKind {
    /// Datum parses as WarpRouteDatum - use WarpRouteRedeemer
    WarpRoute,
    /// Generic recipient - message delivered directly to script address
    GenericRecipient,
}

/// Resolved recipient information (replaces registry)
#[derive(Debug, Clone)]
pub struct ResolvedRecipient {
    /// The state UTXO holding the recipient's NFT and datum.
    /// Present for WarpRoute; None for GenericRecipient.
    pub state_utxo: Option<Utxo>,
    /// Script hash extracted from the UTXO's address
    pub script_hash: ScriptHash,
    /// What kind of recipient this is
    pub recipient_kind: RecipientKind,
    /// Custom ISM override (from datum)
    pub ism: Option<ScriptHash>,
    /// Recipient's NFT policy ID (28 bytes)
    pub recipient_policy: [u8; 28],
    /// For generic recipients with an ISM override: the UTXO holding the
    /// HyperlaneRecipientDatum at the recipient's script address.
    /// The relayer adds this as a reference input so the mailbox can read
    /// the ism field on-chain without spending the recipient's state.
    pub ism_config_utxo: Option<Utxo>,
}

/// Resolves recipient info by querying the state NFT directly.
/// Replaces the registry-based approach with O(1) NFT lookup + datum inspection.
pub struct RecipientResolver {
    provider: BlockfrostProvider,
    warp_route_ref_script_utxo: Option<String>,
    network: CardanoNetwork,
}

impl RecipientResolver {
    pub fn new(
        provider: BlockfrostProvider,
        warp_route_ref_script_utxo: Option<String>,
        network: CardanoNetwork,
    ) -> Self {
        Self {
            provider,
            warp_route_ref_script_utxo,
            network,
        }
    }

    pub fn warp_route_ref_script_utxo(&self) -> Option<&str> {
        self.warp_route_ref_script_utxo.as_deref()
    }

    #[instrument(skip(self))]
    pub async fn resolve(&self, recipient: &[u8; 32]) -> Result<ResolvedRecipient, ResolverError> {
        // 0x02 prefix = script-hash addressing (generic recipients)
        if let Some(script_hash) = hyperlane_address_to_script_hash(recipient) {
            // Only treat as script-hash if prefix is actually 0x02
            if recipient[0] == 0x02 {
                let hash_hex = hex::encode(script_hash);
                info!("Resolving script-hash recipient: {}", hash_hex);

                // Look for the canonical config NFT UTXO at the recipient's script address.
                // If found, use the ism field as an override and include the UTXO as a
                // reference input so the mailbox can verify the override on-chain.
                let (ism, ism_config_utxo) =
                    find_generic_recipient_ism(&self.provider, &script_hash, self.network).await;

                return Ok(ResolvedRecipient {
                    state_utxo: None,
                    script_hash,
                    recipient_kind: RecipientKind::GenericRecipient,
                    ism,
                    recipient_policy: [0u8; 28], // not used for generic recipients
                    ism_config_utxo,
                });
            }
        }

        // 0x01 prefix = NFT policy addressing (warp routes)
        let policy_id = hyperlane_address_to_policy_id(recipient).ok_or_else(|| {
            ResolverError::InvalidRecipient(format!(
                "Unsupported address format (expected 0x01 or 0x02 prefix): {}",
                hex::encode(recipient)
            ))
        })?;
        let policy_hex = hex::encode(policy_id);

        info!(
            "Resolving recipient via NFT query: policy_id={}",
            policy_hex
        );

        // O(1) NFT query - find UTXO holding the state NFT with empty asset name
        let state_utxo = self
            .provider
            .find_utxo_by_nft(&policy_hex, "")
            .await
            .map_err(|e| {
                ResolverError::RecipientNotFound(format!(
                    "NFT lookup failed for policy {policy_hex}: {e}"
                ))
            })?;

        info!(
            "Found state UTXO: {}#{} at address {}",
            state_utxo.tx_hash, state_utxo.output_index, state_utxo.address
        );

        let script_hash = extract_script_hash_from_address(&state_utxo.address)?;

        let (recipient_kind, ism) = detect_recipient_kind_and_ism(&state_utxo)?;

        debug!(
            "Resolved recipient: script_hash={}, kind={:?}, ism={:?}",
            hex::encode(script_hash),
            recipient_kind,
            ism.map(hex::encode)
        );

        Ok(ResolvedRecipient {
            state_utxo: match recipient_kind {
                RecipientKind::WarpRoute => Some(state_utxo),
                RecipientKind::GenericRecipient => None,
            },
            script_hash,
            recipient_kind,
            ism,
            recipient_policy: policy_id,
            ism_config_utxo: None, // warp routes embed ISM in their own datum (tx.inputs)
        })
    }
}

/// Extract script hash from a bech32 Cardano address.
/// Script addresses have a script hash as the payment credential.
fn extract_script_hash_from_address(address: &str) -> Result<ScriptHash, ResolverError> {
    use pallas_addresses::Address;

    let addr = Address::from_bech32(address).map_err(|e| {
        ResolverError::InvalidDatum(format!("Invalid bech32 address {address}: {e}"))
    })?;

    match addr {
        Address::Shelley(shelley) => {
            let hash_bytes = shelley.payment().as_hash().as_ref();
            let mut hash = [0u8; 28];
            hash.copy_from_slice(hash_bytes);
            Ok(hash)
        }
        _ => Err(ResolverError::InvalidDatum(format!(
            "Expected Shelley address, got: {address}"
        ))),
    }
}

/// Detect recipient kind and extract ISM from datum.
///
/// Tries to parse the datum as a WarpRouteDatum (Constr 0 with 4 fields:
/// config, owner, total_bridged, ism). If the config field contains a
/// token_type sub-structure, it's a WarpRoute. Otherwise falls back to Generic.
fn detect_recipient_kind_and_ism(
    utxo: &Utxo,
) -> Result<(RecipientKind, Option<ScriptHash>), ResolverError> {
    let inline_datum = match &utxo.inline_datum {
        Some(d) => d,
        None => return Ok((RecipientKind::GenericRecipient, None)),
    };

    // Try CBOR hex first (Blockfrost returns raw CBOR for inline datums)
    let hex_str = inline_datum.trim_matches('"');
    if let Ok(cbor_bytes) = hex::decode(hex_str) {
        if let Ok(plutus_data) = minicbor::decode::<PlutusData>(&cbor_bytes) {
            return detect_from_plutus_data(&plutus_data);
        }
    }

    // Try JSON format
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(inline_datum) {
        return detect_from_json(&json);
    }

    Ok((RecipientKind::GenericRecipient, None))
}

/// Detect from PlutusData (CBOR-decoded).
///
/// WarpRouteDatum = Constr 0 [config, owner, total_bridged, ism]
/// where config = Constr 0 [token_type, decimals, remote_decimals, remote_routes]
/// and token_type is Constr 0/1/2 (Collateral/Synthetic/Native)
fn detect_from_plutus_data(
    data: &PlutusData,
) -> Result<(RecipientKind, Option<ScriptHash>), ResolverError> {
    let (tag, fields) = match data {
        PlutusData::Constr(c) => (c.tag, c.fields.iter().collect::<Vec<_>>()),
        _ => return Ok((RecipientKind::GenericRecipient, None)),
    };

    // Constr 0 (tag 121) with 4 fields = WarpRouteDatum
    if tag != 121 || fields.len() < 4 {
        return Ok((RecipientKind::GenericRecipient, None));
    }

    // Check field 0 is config (Constr 0 with 4 fields including token_type)
    let is_warp = match fields[0] {
        PlutusData::Constr(c) => c.tag == 121 && c.fields.len() >= 4,
        _ => false,
    };

    if !is_warp {
        return Ok((RecipientKind::GenericRecipient, None));
    }

    // Extract ISM from field 3 (Option<ScriptHash>)
    let ism = parse_optional_script_hash_cbor(fields[3]);

    Ok((RecipientKind::WarpRoute, ism))
}

fn parse_optional_script_hash_cbor(data: &PlutusData) -> Option<ScriptHash> {
    match data {
        PlutusData::Constr(c) => {
            // Some = Constr 0 (tag 121) with 1 field
            if c.tag == 121 {
                if let Some(PlutusData::BoundedBytes(bytes)) = c.fields.first() {
                    let bytes: &[u8] = bytes.as_ref();
                    if bytes.len() == 28 {
                        let mut hash = [0u8; 28];
                        hash.copy_from_slice(bytes);
                        return Some(hash);
                    }
                }
            }
            // None = Constr 1 (tag 122)
            None
        }
        _ => None,
    }
}

/// Detect from JSON (Blockfrost JSON datum format).
fn detect_from_json(
    json: &serde_json::Value,
) -> Result<(RecipientKind, Option<ScriptHash>), ResolverError> {
    let fields = match json.get("fields").and_then(|f| f.as_array()) {
        Some(f) => f,
        None => return Ok((RecipientKind::GenericRecipient, None)),
    };

    // WarpRouteDatum has 4 fields
    if fields.len() < 4 {
        return Ok((RecipientKind::GenericRecipient, None));
    }

    // Check field 0 is config (has constructor 0 with 4 sub-fields)
    let is_warp = fields[0]
        .get("fields")
        .and_then(|f| f.as_array())
        .map(|f| f.len() >= 4)
        .unwrap_or(false);

    if !is_warp {
        return Ok((RecipientKind::GenericRecipient, None));
    }

    // Extract ISM from field 3
    let ism = parse_optional_script_hash_json(&fields[3]);

    Ok((RecipientKind::WarpRoute, ism))
}

/// Fixed policy ID of the canonical_config_nft validator (non-parameterized).
///
/// The asset name of the minted token is the recipient's script hash (28 bytes),
/// so the relayer queries `CANONICAL_CONFIG_NFT_POLICY / hex(script_hash)`.
///
/// IMPORTANT: Update after every `aiken build` if the validator source changes.
/// Extract `hash` from the `canonical_config_nft.canonical_config_nft.mint`
/// entry in `cardano/contracts/plutus.json`.
const CANONICAL_CONFIG_NFT_POLICY: &str =
    "1ae447f5a98155243852ba2264e150c03ace89cf3e17fd1da670ae36";

/// Find the canonical config NFT UTXO at the recipient's script address.
///
/// Queries for `CANONICAL_CONFIG_NFT_POLICY / hex(script_hash)` and parses
/// the datum as `Option<ScriptHash>` (the ISM override, or None for default).
///
/// Returns (ism_override, utxo_to_use_as_reference_input).
async fn find_generic_recipient_ism(
    provider: &BlockfrostProvider,
    script_hash: &ScriptHash,
    _network: CardanoNetwork,
) -> (Option<ScriptHash>, Option<Utxo>) {
    let script_hash_hex = hex::encode(script_hash);
    let asset_name_hex = script_hash_hex.clone();

    let config_utxos: Vec<Utxo> = {
        debug!(
            "Looking up canonical config NFT: policy={}, asset={}",
            CANONICAL_CONFIG_NFT_POLICY, asset_name_hex
        );
        match provider
            .get_utxos_by_asset(CANONICAL_CONFIG_NFT_POLICY, &asset_name_hex)
            .await
        {
            Ok(utxos) => utxos,
            Err(e) => {
                debug!("Canonical config NFT not found for script {script_hash_hex}: {e}");
                Vec::new()
            }
        }
    };

    if config_utxos.is_empty() {
        return (None, None);
    }

    // When multiple config UTXOs exist (e.g., after re-init), prefer an explicit ISM
    // override (Some) over the default (None). If none have an override, use the first.
    let (utxo, ism) = config_utxos
        .into_iter()
        .map(|u| {
            let ism = u.inline_datum.as_deref().and_then(parse_ism_config_datum);
            (u, ism)
        })
        .max_by_key(|(_, ism)| ism.is_some())
        .unwrap();

    (ism, Some(utxo))
}

/// Parse an ISM config datum (CBOR hex or JSON) as `Option<ScriptHash>`.
///
/// Datum format:
/// - `None`  = Constr 1 [] (tag 122, no fields)
/// - `Some`  = Constr 0 [ByteArray(28)] (tag 121, 1 field)
fn parse_ism_config_datum(inline_datum: &str) -> Option<ScriptHash> {
    let hex_str = inline_datum.trim_matches('"');
    if let Ok(cbor_bytes) = hex::decode(hex_str) {
        if let Ok(data) = minicbor::decode::<PlutusData>(&cbor_bytes) {
            return parse_optional_script_hash_cbor(&data);
        }
    }
    // JSON fallback
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(inline_datum) {
        return parse_optional_script_hash_json(&json);
    }
    None
}

fn parse_optional_script_hash_json(json: &serde_json::Value) -> Option<ScriptHash> {
    let constructor = json.get("constructor").and_then(|c| c.as_u64())?;

    if constructor == 0 {
        // Some
        let fields = json.get("fields").and_then(|f| f.as_array())?;
        let hash_hex = fields.first()?.get("bytes")?.as_str()?;
        let hash_bytes = hex::decode(hash_hex).ok()?;
        if hash_bytes.len() == 28 {
            let mut hash = [0u8; 28];
            hash.copy_from_slice(&hash_bytes);
            return Some(hash);
        }
    }
    // constructor 1 = None
    None
}

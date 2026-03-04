//! Utility functions for Cardano transaction building.
//!
//! Parsing, conversion, and small helpers used by the main tx_builder.

use std::collections::HashMap;

use hyperlane_core::U256;
use pallas_addresses::{Address, Network};
use pallas_crypto::hash::Hash;
use pallas_txbuilder::Input;

use crate::blockfrost_provider::Utxo;

use super::{EvaluatedExUnits, OutputIndices, ProcessTxComponents, TxBuilderError};

/// Parse the evaluation result from Blockfrost/Ogmios to extract per-redeemer
/// memory and CPU steps, keyed by "spend:N" or "mint:N".
pub fn parse_per_redeemer_ex_units(
    result: &serde_json::Value,
) -> Result<EvaluatedExUnits, TxBuilderError> {
    let mut ex_units_map: EvaluatedExUnits = HashMap::new();

    // Ogmios v6 format: { "result": [{ "validator": {...}, "budget": {...} }] }
    if let Some(evaluations) = result.get("result").and_then(|v| v.as_array()) {
        for entry in evaluations {
            if let (Some(validator), Some(budget)) = (entry.get("validator"), entry.get("budget")) {
                let purpose = validator
                    .get("purpose")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let index = validator.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                let mem = budget.get("memory").and_then(|v| v.as_u64()).unwrap_or(0);
                let steps = budget.get("cpu").and_then(|v| v.as_u64()).unwrap_or(0);
                let key = format!("{purpose}:{index}");
                ex_units_map.insert(key, (mem, steps));
            }
        }
        if !ex_units_map.is_empty() {
            return Ok(ex_units_map);
        }
    }

    // Blockfrost/Ogmios v5: { "result": { "EvaluationResult": { "spend:0": {...} } } }
    if let Some(eval_result) = result.get("result").and_then(|r| r.get("EvaluationResult")) {
        if let Some(obj) = eval_result.as_object() {
            for (key, value) in obj {
                let mem = value.get("memory").and_then(|v| v.as_u64()).unwrap_or(0);
                let steps = value.get("steps").and_then(|v| v.as_u64()).unwrap_or(0);
                ex_units_map.insert(key.clone(), (mem, steps));
            }
        }
        if !ex_units_map.is_empty() {
            return Ok(ex_units_map);
        }
    }

    // Top-level EvaluationResult (alternative format)
    if let Some(eval_result) = result.get("EvaluationResult") {
        if let Some(obj) = eval_result.as_object() {
            for (key, value) in obj {
                let mem = value.get("memory").and_then(|v| v.as_u64()).unwrap_or(0);
                let steps = value.get("steps").and_then(|v| v.as_u64()).unwrap_or(0);
                ex_units_map.insert(key.clone(), (mem, steps));
            }
        }
        if !ex_units_map.is_empty() {
            return Ok(ex_units_map);
        }
    }

    // Ogmios evaluation failure
    if let Some(fault) = result.get("fault") {
        let fault_msg = fault
            .get("string")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        return Err(TxBuilderError::Encoding(format!(
            "TX evaluation failed: {fault_msg}"
        )));
    }
    if let Some(err) = result
        .get("result")
        .and_then(|r| r.get("EvaluationFailure"))
    {
        return Err(TxBuilderError::Encoding(format!(
            "TX evaluation failed: {err}"
        )));
    }

    Err(TxBuilderError::Encoding(format!(
        "Could not parse per-redeemer evaluation result: {result}"
    )))
}

/// Extract the node-required fee from a FeeTooSmallUTxO error string.
pub(crate) fn parse_fee_too_small_expected(err: &str) -> Option<u64> {
    let needle = "mismatchExpected = Coin ";
    let pos = err.find(needle)?;
    let after = &err[pos + needle.len()..];
    let end = after
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after.len());
    after[..end].parse().ok()
}

pub(crate) fn is_retryable_bad_inputs_error(err: &TxBuilderError) -> bool {
    err.to_string().contains("BadInputsUTxO")
}

pub(crate) fn compute_output_indices(components: &ProcessTxComponents) -> OutputIndices {
    let mut idx: u32 = 1; // 0 = mailbox continuation (always)

    if components.recipient_utxo.is_some() {
        idx += 1;
    }
    if components.token_release_amount.is_some() {
        idx += 1;
    }

    let ism = idx;
    idx += 1; // ISM continuation

    idx += 1; // Processed message marker

    if components.verified_message_datum_cbor.is_some() {
        idx += 1;
    }

    let change = idx;
    OutputIndices { ism, change }
}

/// Convert a Utxo to a pallas-txbuilder Input.
pub(crate) fn utxo_to_input(utxo: &Utxo) -> Result<Input, TxBuilderError> {
    let tx_hash_bytes = hex::decode(&utxo.tx_hash)
        .map_err(|e| TxBuilderError::Encoding(format!("Invalid tx hash hex: {e}")))?;

    let tx_hash: Hash<32> = Hash::new(
        tx_hash_bytes
            .try_into()
            .map_err(|_| TxBuilderError::Encoding("Tx hash must be 32 bytes".to_string()))?,
    );

    Ok(Input::new(tx_hash, utxo.output_index as u64))
}

/// Parse a bech32 address string into a pallas Address.
pub(crate) fn parse_address(address: &str) -> Result<Address, TxBuilderError> {
    Address::from_bech32(address)
        .map_err(|e| TxBuilderError::InvalidAddress(format!("Invalid bech32 address: {e:?}")))
}

/// Parse a policy ID hex string into a Hash<28>.
pub(crate) fn parse_policy_id(policy_id: &str) -> Result<Hash<28>, TxBuilderError> {
    let bytes = hex::decode(policy_id)
        .map_err(|e| TxBuilderError::Encoding(format!("Invalid policy ID hex: {e}")))?;

    let hash_bytes: [u8; 28] = bytes
        .try_into()
        .map_err(|_| TxBuilderError::Encoding("Policy ID must be 28 bytes".to_string()))?;

    Ok(Hash::new(hash_bytes))
}

/// Parse a UTXO reference string in the format "tx_hash#output_index" into an Input.
pub(crate) fn parse_utxo_ref(utxo_ref: &str) -> Result<Input, TxBuilderError> {
    let parts: Vec<&str> = utxo_ref.split('#').collect();
    if parts.len() != 2 {
        return Err(TxBuilderError::Encoding(format!(
            "Invalid UTXO reference format '{utxo_ref}'. Expected 'tx_hash#output_index'"
        )));
    }

    let tx_hash_hex = parts[0];
    let output_index: u64 = parts[1].parse().map_err(|e| {
        TxBuilderError::Encoding(format!("Invalid output index '{}': {}", parts[1], e))
    })?;

    let tx_hash_bytes = hex::decode(tx_hash_hex)
        .map_err(|e| TxBuilderError::Encoding(format!("Invalid tx hash hex: {e}")))?;

    let tx_hash: Hash<32> = Hash::new(
        tx_hash_bytes
            .try_into()
            .map_err(|_| TxBuilderError::Encoding("Tx hash must be 32 bytes".to_string()))?,
    );

    Ok(Input::new(tx_hash, output_index))
}

/// Convert a 28-byte credential to a Cardano address.
pub(crate) fn credential_to_address(
    credential_bytes: &[u8],
    network: Network,
) -> Result<Address, TxBuilderError> {
    if credential_bytes.len() != 28 {
        return Err(TxBuilderError::Encoding(format!(
            "Credential must be 28 bytes, got {}",
            credential_bytes.len()
        )));
    }

    let header_byte = match network {
        Network::Testnet => 0x60,
        Network::Mainnet => 0x61,
        _ => 0x60,
    };

    let mut address_bytes = Vec::with_capacity(29);
    address_bytes.push(header_byte);
    address_bytes.extend_from_slice(credential_bytes);

    Address::from_bytes(&address_bytes).map_err(|e| {
        TxBuilderError::InvalidAddress(format!("Failed to create address from credential: {e:?}"))
    })
}

/// Convert wire format amount (U256) to local token amount (u64).
pub(crate) fn convert_wire_to_local_amount(
    wire_amount: U256,
    remote_decimals: u8,
    local_decimals: u8,
) -> Result<u64, TxBuilderError> {
    let result = if local_decimals >= remote_decimals {
        let multiplier = U256::from(10u64).pow(U256::from(local_decimals - remote_decimals));
        wire_amount.saturating_mul(multiplier)
    } else {
        let divisor = U256::from(10u64).pow(U256::from(remote_decimals - local_decimals));
        wire_amount / divisor
    };

    if result > U256::from(u64::MAX) {
        return Err(TxBuilderError::Encoding(format!(
            "Amount overflow: converted value {result} exceeds u64::MAX (wire={wire_amount}, remote_dec={remote_decimals}, local_dec={local_decimals})"
        )));
    }

    Ok(result.as_u64())
}

/// Build a warp transfer body with the given recipient and amount.
/// Format: recipient (variable) || amount (8 bytes big-endian)
#[allow(dead_code)]
pub(crate) fn build_warp_transfer_body(recipient: &[u8], amount: u64) -> Vec<u8> {
    let mut body = recipient.to_vec();
    body.extend_from_slice(&amount.to_be_bytes());
    body
}

/// Parse a Hyperlane TokenMessage body.
/// Standard wire format: recipient (32 bytes) || amount (uint256, big-endian) || metadata (optional)
pub(crate) fn parse_token_message(body: &[u8]) -> Result<super::TokenMessage, TxBuilderError> {
    if body.len() < 64 {
        return Err(TxBuilderError::Encoding(format!(
            "TokenMessage too short: {} bytes, expected at least 64",
            body.len()
        )));
    }

    let recipient: [u8; 32] = body[0..32].try_into().map_err(|_| {
        TxBuilderError::Encoding("Failed to extract recipient from TokenMessage".to_string())
    })?;

    let amount_bytes: [u8; 32] = body[32..64].try_into().map_err(|_| {
        TxBuilderError::Encoding("Failed to extract amount from TokenMessage".to_string())
    })?;
    let amount = U256::from_big_endian(&amount_bytes);

    Ok(super::TokenMessage { recipient, amount })
}

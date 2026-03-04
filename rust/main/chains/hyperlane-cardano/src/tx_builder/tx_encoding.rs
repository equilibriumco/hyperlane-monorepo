//! CBOR/Plutus Data encoding and decoding for Cardano transactions.
//!
//! Converts between Blockfrost JSON datum format, PlutusData, and CBOR bytes.

use pallas_codec::minicbor;
use pallas_codec::utils::{KeyValuePairs, MaybeIndefArray};
use pallas_primitives::conway::{BigInt, Constr, PlutusData};

use crate::redeemers::{
    plutus_constr_tag, MailboxRedeemerTag, MultisigIsmRedeemerTag, WarpRouteRedeemerTag,
};
use crate::types::{MailboxRedeemer, Message, ProcessedMessageDatum};

use super::TxBuilderError;

/// Convert a datum string (from Blockfrost) to CBOR bytes.
/// Blockfrost can return either JSON format or raw CBOR hex — this handles both.
pub(crate) fn json_datum_to_cbor(datum_str: &str) -> Result<Vec<u8>, TxBuilderError> {
    use serde_json::Value;

    if let Ok(json) = serde_json::from_str::<Value>(datum_str) {
        let plutus_data = json_to_plutus_data(&json)?;
        return encode_plutus_data(&plutus_data);
    }

    let hex_str = datum_str.trim_matches('"');
    if hex_str.chars().all(|c| c.is_ascii_hexdigit()) && !hex_str.is_empty() {
        let cbor_bytes = hex::decode(hex_str)
            .map_err(|e| TxBuilderError::Encoding(format!("Invalid CBOR hex: {e}")))?;
        return Ok(cbor_bytes);
    }

    Err(TxBuilderError::Encoding(format!(
        "Datum is neither valid JSON nor CBOR hex: {}",
        &datum_str[..datum_str.len().min(100)]
    )))
}

/// Convert JSON value to PlutusData.
pub(crate) fn json_to_plutus_data(json: &serde_json::Value) -> Result<PlutusData, TxBuilderError> {
    use serde_json::Value;

    match json {
        Value::Number(n) => {
            let i = n
                .as_i64()
                .ok_or_else(|| TxBuilderError::Encoding("Number too large".to_string()))?;
            Ok(PlutusData::BigInt(BigInt::Int(i.into())))
        }

        Value::String(s) => {
            if s.starts_with("0x") || s.chars().all(|c| c.is_ascii_hexdigit()) {
                let hex_str = s.strip_prefix("0x").unwrap_or(s);
                let bytes = hex::decode(hex_str)
                    .map_err(|e| TxBuilderError::Encoding(format!("Invalid hex string: {e}")))?;
                Ok(PlutusData::BoundedBytes(bytes.into()))
            } else {
                Ok(PlutusData::BoundedBytes(s.as_bytes().to_vec().into()))
            }
        }

        Value::Object(obj) => {
            if let (Some(constructor), Some(fields)) = (obj.get("constructor"), obj.get("fields")) {
                let tag = constructor
                    .as_u64()
                    .ok_or_else(|| TxBuilderError::Encoding("Invalid constructor".to_string()))?;

                let fields_vec = fields
                    .as_array()
                    .ok_or_else(|| TxBuilderError::Encoding("Fields must be array".to_string()))?;

                let mut parsed_fields = Vec::new();
                for field in fields_vec {
                    parsed_fields.push(json_to_plutus_data(field)?);
                }

                let plutus_tag = if tag <= 6 {
                    121 + tag
                } else {
                    1280 + (tag - 7)
                };

                Ok(PlutusData::Constr(Constr {
                    tag: plutus_tag,
                    any_constructor: None,
                    fields: MaybeIndefArray::Def(parsed_fields),
                }))
            } else if let Some(bytes) = obj.get("bytes") {
                let hex_str = bytes
                    .as_str()
                    .ok_or_else(|| TxBuilderError::Encoding("bytes must be string".to_string()))?;
                let bytes = hex::decode(hex_str)
                    .map_err(|e| TxBuilderError::Encoding(format!("Invalid hex: {e}")))?;
                Ok(PlutusData::BoundedBytes(bytes.into()))
            } else if let Some(int_val) = obj.get("int") {
                let i = int_val
                    .as_i64()
                    .ok_or_else(|| TxBuilderError::Encoding("int must be number".to_string()))?;
                Ok(PlutusData::BigInt(BigInt::Int(i.into())))
            } else if let Some(list) = obj.get("list") {
                let items = list
                    .as_array()
                    .ok_or_else(|| TxBuilderError::Encoding("list must be array".to_string()))?;
                let mut parsed_items = Vec::new();
                for item in items {
                    parsed_items.push(json_to_plutus_data(item)?);
                }
                Ok(PlutusData::Array(MaybeIndefArray::Def(parsed_items)))
            } else if let Some(map) = obj.get("map") {
                let entries = map
                    .as_array()
                    .ok_or_else(|| TxBuilderError::Encoding("map must be array".to_string()))?;
                let mut parsed_map = Vec::new();
                for entry in entries {
                    let k = entry.get("k").ok_or_else(|| {
                        TxBuilderError::Encoding("map entry missing k".to_string())
                    })?;
                    let v = entry.get("v").ok_or_else(|| {
                        TxBuilderError::Encoding("map entry missing v".to_string())
                    })?;
                    parsed_map.push((json_to_plutus_data(k)?, json_to_plutus_data(v)?));
                }
                Ok(PlutusData::Map(KeyValuePairs::from(parsed_map)))
            } else {
                Err(TxBuilderError::Encoding(
                    "Unknown JSON object format".to_string(),
                ))
            }
        }

        Value::Array(arr) => {
            let mut items = Vec::new();
            for item in arr {
                items.push(json_to_plutus_data(item)?);
            }
            Ok(PlutusData::Array(MaybeIndefArray::Def(items)))
        }

        _ => Err(TxBuilderError::Encoding(format!(
            "Unsupported JSON value type: {json:?}"
        ))),
    }
}

/// Encode PlutusData to CBOR bytes.
pub(crate) fn encode_plutus_data(data: &PlutusData) -> Result<Vec<u8>, TxBuilderError> {
    minicbor::to_vec(data)
        .map_err(|e| TxBuilderError::Encoding(format!("CBOR encoding failed: {e:?}")))
}

/// Encode a Constr 0 [] redeemer (used for MintMessage/Mint in minting policies).
pub(crate) fn encode_constructor_0_redeemer() -> Vec<u8> {
    let redeemer = PlutusData::Constr(Constr {
        tag: plutus_constr_tag(0),
        any_constructor: None,
        fields: MaybeIndefArray::Def(vec![]),
    });

    let mut encoded = Vec::new();
    minicbor::encode(&redeemer, &mut encoded).expect("Failed to encode constructor 0 redeemer");
    encoded
}

/// Encode a Message as Plutus Data.
pub(crate) fn encode_message_as_plutus_data(msg: &Message) -> PlutusData {
    PlutusData::Constr(Constr {
        tag: plutus_constr_tag(0),
        any_constructor: None,
        fields: MaybeIndefArray::Def(vec![
            PlutusData::BigInt(BigInt::Int((msg.version as i64).into())),
            PlutusData::BigInt(BigInt::Int((msg.nonce as i64).into())),
            PlutusData::BigInt(BigInt::Int((msg.origin as i64).into())),
            PlutusData::BoundedBytes(msg.sender.to_vec().into()),
            PlutusData::BigInt(BigInt::Int((msg.destination as i64).into())),
            PlutusData::BoundedBytes(msg.recipient.to_vec().into()),
            PlutusData::BoundedBytes(msg.body.clone().into()),
        ]),
    })
}

/// Encode a MailboxRedeemer as Plutus Data CBOR.
pub fn encode_mailbox_redeemer(redeemer: &MailboxRedeemer) -> Result<Vec<u8>, TxBuilderError> {
    let plutus_data = match redeemer {
        MailboxRedeemer::Dispatch {
            destination,
            recipient,
            body,
            sender_ref,
            hook_metadata,
        } => {
            let sender_ref_data = PlutusData::Constr(Constr {
                tag: plutus_constr_tag(0),
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    PlutusData::BoundedBytes(sender_ref.0.to_vec().into()),
                    PlutusData::BigInt(BigInt::Int((sender_ref.1 as i64).into())),
                ]),
            });
            PlutusData::Constr(Constr {
                tag: MailboxRedeemerTag::Dispatch.plutus_tag(),
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    PlutusData::BigInt(BigInt::Int((*destination as i64).into())),
                    PlutusData::BoundedBytes(recipient.to_vec().into()),
                    PlutusData::BoundedBytes(body.clone().into()),
                    sender_ref_data,
                    PlutusData::BoundedBytes(hook_metadata.clone().into()),
                ]),
            })
        }
        MailboxRedeemer::Process {
            message,
            metadata,
            message_id,
            smt_proof,
        } => {
            let proof_list: Vec<PlutusData> = smt_proof
                .iter()
                .map(|hash| PlutusData::BoundedBytes(hash.to_vec().into()))
                .collect();
            PlutusData::Constr(Constr {
                tag: MailboxRedeemerTag::Process.plutus_tag(),
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    encode_message_as_plutus_data(message),
                    PlutusData::BoundedBytes(metadata.clone().into()),
                    PlutusData::BoundedBytes(message_id.to_vec().into()),
                    PlutusData::Array(MaybeIndefArray::Def(proof_list)),
                ]),
            })
        }
        MailboxRedeemer::SetDefaultIsm { new_ism } => PlutusData::Constr(Constr {
            tag: MailboxRedeemerTag::SetDefaultIsm.plutus_tag(),
            any_constructor: None,
            fields: MaybeIndefArray::Def(vec![PlutusData::BoundedBytes(new_ism.to_vec().into())]),
        }),
        MailboxRedeemer::TransferOwnership { new_owner } => PlutusData::Constr(Constr {
            tag: MailboxRedeemerTag::TransferOwnership.plutus_tag(),
            any_constructor: None,
            fields: MaybeIndefArray::Def(vec![PlutusData::BoundedBytes(new_owner.to_vec().into())]),
        }),
    };

    encode_plutus_data(&plutus_data)
}

/// Encode a ProcessedMessageDatum as Plutus Data CBOR.
pub fn encode_processed_message_datum(
    datum: &ProcessedMessageDatum,
) -> Result<Vec<u8>, TxBuilderError> {
    let plutus_data = PlutusData::Constr(Constr {
        tag: plutus_constr_tag(0),
        any_constructor: None,
        fields: MaybeIndefArray::Def(vec![PlutusData::BoundedBytes(
            datum.message_id.to_vec().into(),
        )]),
    });

    encode_plutus_data(&plutus_data)
}

/// Encode a VerifiedMessageDatum as Plutus Data CBOR.
/// VerifiedMessageDatum { origin, sender, body, message_id, nonce }
pub fn encode_verified_message_datum(
    datum: &crate::types::VerifiedMessageDatum,
) -> Result<Vec<u8>, TxBuilderError> {
    let plutus_data = PlutusData::Constr(Constr {
        tag: plutus_constr_tag(0),
        any_constructor: None,
        fields: MaybeIndefArray::Def(vec![
            PlutusData::BigInt(BigInt::Int((datum.origin as i64).into())),
            PlutusData::BoundedBytes(datum.sender.clone().into()),
            PlutusData::BoundedBytes(datum.body.clone().into()),
            PlutusData::BoundedBytes(datum.message_id.clone().into()),
            PlutusData::BigInt(BigInt::Int((datum.nonce as i64).into())),
        ]),
    });

    encode_plutus_data(&plutus_data)
}

/// Encode ISM redeemer to CBOR.
pub(crate) fn encode_ism_redeemer(
    redeemer: &crate::types::MultisigIsmRedeemer,
) -> Result<Vec<u8>, TxBuilderError> {
    let plutus_data = match redeemer {
        crate::types::MultisigIsmRedeemer::Verify {
            checkpoint,
            validator_signatures,
        } => {
            let checkpoint_data = PlutusData::Constr(Constr {
                tag: plutus_constr_tag(0),
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    PlutusData::BigInt(BigInt::Int((checkpoint.origin as i64).into())),
                    PlutusData::BoundedBytes(checkpoint.merkle_root.to_vec().into()),
                    PlutusData::BoundedBytes(checkpoint.origin_merkle_tree_hook.to_vec().into()),
                    PlutusData::BigInt(BigInt::Int((checkpoint.merkle_index as i64).into())),
                    PlutusData::BoundedBytes(checkpoint.message_id.to_vec().into()),
                ]),
            });

            let sig_list: Vec<PlutusData> = validator_signatures
                .iter()
                .map(|val_sig| {
                    PlutusData::Constr(Constr {
                        tag: plutus_constr_tag(0),
                        any_constructor: None,
                        fields: MaybeIndefArray::Def(vec![
                            PlutusData::BoundedBytes(val_sig.compressed_pubkey.to_vec().into()),
                            PlutusData::BoundedBytes(val_sig.uncompressed_pubkey.to_vec().into()),
                            PlutusData::BoundedBytes(val_sig.signature.to_vec().into()),
                        ]),
                    })
                })
                .collect();

            PlutusData::Constr(Constr {
                tag: MultisigIsmRedeemerTag::Verify.plutus_tag(),
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    checkpoint_data,
                    PlutusData::Array(MaybeIndefArray::Def(sig_list)),
                ]),
            })
        }
        crate::types::MultisigIsmRedeemer::SetValidators { domain, validators } => {
            let validator_bytes: Vec<PlutusData> = validators
                .iter()
                .map(|v| PlutusData::BoundedBytes(v.0.to_vec().into()))
                .collect();

            PlutusData::Constr(Constr {
                tag: MultisigIsmRedeemerTag::SetValidators.plutus_tag(),
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    PlutusData::BigInt(BigInt::Int((*domain as i64).into())),
                    PlutusData::Array(MaybeIndefArray::Def(validator_bytes)),
                ]),
            })
        }
        crate::types::MultisigIsmRedeemer::SetThreshold { domain, threshold } => {
            PlutusData::Constr(Constr {
                tag: MultisigIsmRedeemerTag::SetThreshold.plutus_tag(),
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    PlutusData::BigInt(BigInt::Int((*domain as i64).into())),
                    PlutusData::BigInt(BigInt::Int((*threshold as i64).into())),
                ]),
            })
        }
    };

    encode_plutus_data(&plutus_data)
}

/// Encode warp route redeemer to CBOR.
pub fn encode_warp_route_redeemer(
    redeemer: &crate::types::WarpRouteRedeemer,
) -> Result<Vec<u8>, TxBuilderError> {
    let plutus_data = match redeemer {
        crate::types::WarpRouteRedeemer::TransferRemote {
            destination,
            recipient,
            amount,
        } => PlutusData::Constr(Constr {
            tag: WarpRouteRedeemerTag::TransferRemote.plutus_tag(),
            any_constructor: None,
            fields: MaybeIndefArray::Def(vec![
                PlutusData::BigInt(BigInt::Int((*destination as i64).into())),
                PlutusData::BoundedBytes(recipient.to_vec().into()),
                PlutusData::BigInt(BigInt::Int((*amount as i64).into())),
            ]),
        }),
        crate::types::WarpRouteRedeemer::ReceiveTransfer {
            message,
            message_id,
        } => {
            let message_data = PlutusData::Constr(Constr {
                tag: plutus_constr_tag(0),
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    PlutusData::BigInt(BigInt::Int((message.version as i64).into())),
                    PlutusData::BigInt(BigInt::Int((message.nonce as i64).into())),
                    PlutusData::BigInt(BigInt::Int((message.origin as i64).into())),
                    PlutusData::BoundedBytes(message.sender.to_vec().into()),
                    PlutusData::BigInt(BigInt::Int((message.destination as i64).into())),
                    PlutusData::BoundedBytes(message.recipient.to_vec().into()),
                    PlutusData::BoundedBytes(message.body.clone().into()),
                ]),
            });

            PlutusData::Constr(Constr {
                tag: WarpRouteRedeemerTag::ReceiveTransfer.plutus_tag(),
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    message_data,
                    PlutusData::BoundedBytes(message_id.to_vec().into()),
                ]),
            })
        }
        crate::types::WarpRouteRedeemer::EnrollRemoteRoute { domain, route } => {
            PlutusData::Constr(Constr {
                tag: WarpRouteRedeemerTag::EnrollRemoteRoute.plutus_tag(),
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    PlutusData::BigInt(BigInt::Int((*domain as i64).into())),
                    PlutusData::BoundedBytes(route.to_vec().into()),
                ]),
            })
        }
    };

    encode_plutus_data(&plutus_data)
}

/// Extract Int from PlutusData.
pub(crate) fn extract_int(data: &PlutusData) -> Option<i64> {
    if let PlutusData::BigInt(bigint) = data {
        match bigint {
            BigInt::Int(i) => {
                let val: i128 = (*i).into();
                i64::try_from(val).ok()
            }
            BigInt::BigUInt(bytes) => {
                if bytes.len() <= 8 {
                    let mut arr = [0u8; 8];
                    arr[8 - bytes.len()..].copy_from_slice(bytes);
                    Some(i64::from_be_bytes(arr))
                } else {
                    None
                }
            }
            BigInt::BigNInt(_) => None,
        }
    } else {
        None
    }
}

/// Extract ByteArray from PlutusData.
pub(crate) fn extract_bytes(data: &PlutusData) -> Option<Vec<u8>> {
    if let PlutusData::BoundedBytes(bytes) = data {
        Some(bytes.to_vec())
    } else {
        None
    }
}

/// Build ISM datum with updated validators.
/// Structure: Constr(121, [validators_list, thresholds_list, owner_bytes])
pub(crate) fn build_ism_datum(
    domain: u32,
    validators: Vec<Vec<u8>>,
    threshold: u32,
    owner: [u8; 28],
) -> Result<PlutusData, TxBuilderError> {
    use serde_json::json;

    let validator_hex_list: Vec<String> = validators.into_iter().map(|v| hex::encode(&v)).collect();

    let validators_json = json!({
        "list": [
            {
                "constructor": 0,
                "fields": [
                    {"int": domain},
                    {
                        "list": validator_hex_list.iter().map(|h| json!({"bytes": h})).collect::<Vec<_>>()
                    }
                ]
            }
        ]
    });

    let thresholds_json = json!({
        "list": [
            {
                "constructor": 0,
                "fields": [
                    {"int": domain},
                    {"int": threshold}
                ]
            }
        ]
    });

    let datum_json = json!({
        "constructor": 0,
        "fields": [
            validators_json,
            thresholds_json,
            {"bytes": hex::encode(owner)}
        ]
    });

    json_to_plutus_data(&datum_json)
}

/// Extract owner from ISM datum PlutusData.
/// ISM datum structure: Constr(121, [validators_list, thresholds_list, owner_bytes])
pub(crate) fn extract_ism_owner(datum: &PlutusData) -> Result<[u8; 28], TxBuilderError> {
    match datum {
        PlutusData::Constr(constr) if constr.fields.len() == 3 => {
            let owner_field = &constr.fields[2];

            let owner_bytes: &[u8] = match owner_field {
                PlutusData::BoundedBytes(bytes) => bytes.as_ref(),
                _ => {
                    return Err(TxBuilderError::Encoding(format!(
                        "Owner field must be BoundedBytes, got: {owner_field:?}"
                    )))
                }
            };

            let bytes: [u8; 28] = owner_bytes.try_into().map_err(|_| {
                TxBuilderError::Encoding(format!(
                    "Owner must be 28 bytes, got {}",
                    owner_bytes.len()
                ))
            })?;
            Ok(bytes)
        }
        _ => Err(TxBuilderError::Encoding(format!(
            "Invalid ISM datum structure: expected Constr with 3 fields, got {datum:?}"
        ))),
    }
}

use crate::blockfrost_provider::{
    BlockfrostProvider, BlockfrostProviderError, TransactionRedeemer,
};
use tracing::warn;

/// Filter redeemers that match the mailbox script_hash with "spend" purpose.
fn filter_mailbox_spend_redeemers<'a>(
    redeemers: &'a [TransactionRedeemer],
    mailbox_script_hash: &str,
) -> Vec<&'a TransactionRedeemer> {
    redeemers
        .iter()
        .filter(|r| r.script_hash == mailbox_script_hash && r.purpose.eq_ignore_ascii_case("spend"))
        .collect()
}

/// Parse a message_id from a Process redeemer datum JSON.
///
/// The datum is expected to have constructor == 1 (Process variant) with
/// fields[2].bytes containing a 32-byte hex-encoded message_id.
/// Returns None for non-Process redeemers or malformed data.
fn parse_message_id_from_datum(datum: &serde_json::Value, tx_hash: &str) -> Option<[u8; 32]> {
    let constructor = datum.get("constructor").and_then(|v| v.as_u64())?;
    if constructor != 1 {
        return None;
    }

    let fields = match datum.get("fields").and_then(|v| v.as_array()) {
        Some(f) => f,
        None => {
            warn!(tx_hash, "Process redeemer missing fields array, skipping");
            return None;
        }
    };

    if fields.len() < 3 {
        warn!(
            tx_hash,
            "Process redeemer has {} fields, expected >= 3, skipping",
            fields.len()
        );
        return None;
    }

    let message_id_hex = match fields[2].get("bytes").and_then(|v| v.as_str()) {
        Some(h) => h,
        None => {
            warn!(
                tx_hash,
                "Process redeemer fields[2] missing bytes, skipping"
            );
            return None;
        }
    };

    match hex::decode(message_id_hex) {
        Ok(bytes) if bytes.len() == 32 => {
            let mut id = [0u8; 32];
            id.copy_from_slice(&bytes);
            Some(id)
        }
        Ok(bytes) => {
            warn!(
                tx_hash,
                "message_id has {} bytes, expected 32, skipping",
                bytes.len()
            );
            None
        }
        Err(e) => {
            warn!(tx_hash, "Invalid message_id hex: {e}, skipping");
            None
        }
    }
}

/// Extract message_ids from Process redeemers in a transaction.
///
/// Uses Blockfrost's redeemers API (which already resolves script_hash per
/// redeemer) to identify mailbox Process actions, then fetches each
/// redeemer's datum to parse the message_id field.
pub async fn extract_process_message_ids(
    provider: &BlockfrostProvider,
    tx_hash: &str,
    mailbox_script_hash: &str,
) -> Result<Vec<[u8; 32]>, BlockfrostProviderError> {
    let redeemers = provider.get_transaction_redeemers(tx_hash).await?;
    let matching = filter_mailbox_spend_redeemers(&redeemers, mailbox_script_hash);

    let mut message_ids = Vec::new();
    let mut skipped = 0u32;

    for redeemer in matching {
        let datum = match provider
            .get_redeemer_datum(&redeemer.redeemer_data_hash)
            .await
        {
            Ok(d) => d,
            Err(e) => {
                warn!(
                    tx_hash,
                    redeemer_data_hash = redeemer.redeemer_data_hash,
                    "Failed to fetch redeemer datum, skipping: {e}"
                );
                skipped += 1;
                continue;
            }
        };

        match parse_message_id_from_datum(&datum, tx_hash) {
            Some(id) => message_ids.push(id),
            None => {
                skipped += 1;
            }
        }
    }

    if skipped > 0 {
        warn!(
            tx_hash,
            skipped, "Skipped {skipped} malformed redeemers while extracting Process message_ids"
        );
    }

    Ok(message_ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_redeemer(script_hash: &str, purpose: &str, data_hash: &str) -> TransactionRedeemer {
        TransactionRedeemer {
            tx_index: 0,
            purpose: purpose.to_string(),
            script_hash: script_hash.to_string(),
            redeemer_data_hash: data_hash.to_string(),
            datum_hash: String::new(),
            unit_mem: 0,
            unit_steps: 0,
            fee: 0,
        }
    }

    fn valid_process_datum(message_id_hex: &str) -> serde_json::Value {
        json!({
            "constructor": 1,
            "fields": [
                {"bytes": "aabbccdd"},
                {"bytes": "eeff0011"},
                {"bytes": message_id_hex},
                {"list": []}
            ]
        })
    }

    const MAILBOX_HASH: &str = "abcdef1234567890abcdef1234567890abcdef1234567890abcdef12";
    const OTHER_HASH: &str = "9999991234567890abcdef1234567890abcdef1234567890abcdef12";

    // --- filter_mailbox_spend_redeemers tests ---

    #[test]
    fn filter_matches_correct_script_hash_and_spend_purpose() {
        let redeemers = vec![
            make_redeemer(MAILBOX_HASH, "spend", "hash1"),
            make_redeemer(OTHER_HASH, "spend", "hash2"),
            make_redeemer(MAILBOX_HASH, "mint", "hash3"),
        ];

        let result = filter_mailbox_spend_redeemers(&redeemers, MAILBOX_HASH);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].redeemer_data_hash, "hash1");
    }

    #[test]
    fn filter_excludes_wrong_script_hash() {
        let redeemers = vec![make_redeemer(OTHER_HASH, "spend", "hash1")];
        let result = filter_mailbox_spend_redeemers(&redeemers, MAILBOX_HASH);
        assert!(result.is_empty());
    }

    #[test]
    fn filter_excludes_non_spend_purpose() {
        let redeemers = vec![
            make_redeemer(MAILBOX_HASH, "mint", "hash1"),
            make_redeemer(MAILBOX_HASH, "cert", "hash2"),
        ];
        let result = filter_mailbox_spend_redeemers(&redeemers, MAILBOX_HASH);
        assert!(result.is_empty());
    }

    #[test]
    fn filter_is_case_insensitive_on_purpose() {
        let redeemers = vec![
            make_redeemer(MAILBOX_HASH, "Spend", "hash1"),
            make_redeemer(MAILBOX_HASH, "SPEND", "hash2"),
        ];
        let result = filter_mailbox_spend_redeemers(&redeemers, MAILBOX_HASH);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn filter_returns_multiple_matching_redeemers() {
        let redeemers = vec![
            make_redeemer(MAILBOX_HASH, "spend", "hash1"),
            make_redeemer(MAILBOX_HASH, "spend", "hash2"),
        ];
        let result = filter_mailbox_spend_redeemers(&redeemers, MAILBOX_HASH);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn filter_empty_redeemers() {
        let result = filter_mailbox_spend_redeemers(&[], MAILBOX_HASH);
        assert!(result.is_empty());
    }

    // --- parse_message_id_from_datum tests ---

    #[test]
    fn parse_valid_process_redeemer() {
        let msg_id_hex = "aa".repeat(32);
        let datum = valid_process_datum(&msg_id_hex);

        let result = parse_message_id_from_datum(&datum, "tx_test");
        assert_eq!(result, Some([0xaa; 32]));
    }

    #[test]
    fn parse_returns_none_for_dispatch_redeemer() {
        let datum = json!({
            "constructor": 0,
            "fields": [
                {"bytes": "aabb"},
                {"bytes": "ccdd"},
                {"bytes": "aa".repeat(32)}
            ]
        });

        let result = parse_message_id_from_datum(&datum, "tx_test");
        assert_eq!(result, None);
    }

    #[test]
    fn parse_returns_none_for_constructor_2() {
        let datum = json!({
            "constructor": 2,
            "fields": [
                {"bytes": "aa".repeat(32)}
            ]
        });

        let result = parse_message_id_from_datum(&datum, "tx_test");
        assert_eq!(result, None);
    }

    #[test]
    fn parse_returns_none_when_no_constructor() {
        let datum = json!({
            "fields": [
                {"bytes": "aa"},
                {"bytes": "bb"},
                {"bytes": "cc".repeat(32)}
            ]
        });

        let result = parse_message_id_from_datum(&datum, "tx_test");
        assert_eq!(result, None);
    }

    #[test]
    fn parse_returns_none_when_fields_missing() {
        let datum = json!({ "constructor": 1 });

        let result = parse_message_id_from_datum(&datum, "tx_test");
        assert_eq!(result, None);
    }

    #[test]
    fn parse_returns_none_when_fields_not_array() {
        let datum = json!({
            "constructor": 1,
            "fields": "not_an_array"
        });

        let result = parse_message_id_from_datum(&datum, "tx_test");
        assert_eq!(result, None);
    }

    #[test]
    fn parse_returns_none_when_too_few_fields() {
        let datum = json!({
            "constructor": 1,
            "fields": [
                {"bytes": "aa"},
                {"bytes": "bb"}
            ]
        });

        let result = parse_message_id_from_datum(&datum, "tx_test");
        assert_eq!(result, None);
    }

    #[test]
    fn parse_returns_none_when_field2_missing_bytes() {
        let datum = json!({
            "constructor": 1,
            "fields": [
                {"bytes": "aa"},
                {"bytes": "bb"},
                {"int": 42}
            ]
        });

        let result = parse_message_id_from_datum(&datum, "tx_test");
        assert_eq!(result, None);
    }

    #[test]
    fn parse_returns_none_for_short_message_id() {
        let datum = json!({
            "constructor": 1,
            "fields": [
                {"bytes": "aa"},
                {"bytes": "bb"},
                {"bytes": "aabb"}
            ]
        });

        let result = parse_message_id_from_datum(&datum, "tx_test");
        assert_eq!(result, None);
    }

    #[test]
    fn parse_returns_none_for_long_message_id() {
        let datum = json!({
            "constructor": 1,
            "fields": [
                {"bytes": "aa"},
                {"bytes": "bb"},
                {"bytes": "aa".repeat(33)}
            ]
        });

        let result = parse_message_id_from_datum(&datum, "tx_test");
        assert_eq!(result, None);
    }

    #[test]
    fn parse_returns_none_for_invalid_hex() {
        let datum = json!({
            "constructor": 1,
            "fields": [
                {"bytes": "aa"},
                {"bytes": "bb"},
                {"bytes": "not_hex_at_all_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}
            ]
        });

        let result = parse_message_id_from_datum(&datum, "tx_test");
        assert_eq!(result, None);
    }

    #[test]
    fn parse_handles_empty_datum() {
        let datum = json!({});
        let result = parse_message_id_from_datum(&datum, "tx_test");
        assert_eq!(result, None);
    }

    #[test]
    fn parse_handles_null_datum() {
        let datum = serde_json::Value::Null;
        let result = parse_message_id_from_datum(&datum, "tx_test");
        assert_eq!(result, None);
    }

    #[test]
    fn parse_various_valid_message_ids() {
        let all_zeros = "00".repeat(32);
        let datum = valid_process_datum(&all_zeros);
        assert_eq!(
            parse_message_id_from_datum(&datum, "tx_test"),
            Some([0x00; 32])
        );

        let all_ff = "ff".repeat(32);
        let datum = valid_process_datum(&all_ff);
        assert_eq!(
            parse_message_id_from_datum(&datum, "tx_test"),
            Some([0xff; 32])
        );

        let incremental: String = (0..32u8).map(|b| format!("{:02x}", b)).collect();
        let datum = valid_process_datum(&incremental);
        let expected: [u8; 32] = core::array::from_fn(|i| i as u8);
        assert_eq!(
            parse_message_id_from_datum(&datum, "tx_test"),
            Some(expected)
        );
    }

    #[test]
    fn parse_extra_fields_still_works() {
        let datum = json!({
            "constructor": 1,
            "fields": [
                {"bytes": "aa"},
                {"bytes": "bb"},
                {"bytes": "cc".repeat(32)},
                {"list": []},
                {"bytes": "extra_field"}
            ]
        });

        let result = parse_message_id_from_datum(&datum, "tx_test");
        assert_eq!(result, Some([0xcc; 32]));
    }
}

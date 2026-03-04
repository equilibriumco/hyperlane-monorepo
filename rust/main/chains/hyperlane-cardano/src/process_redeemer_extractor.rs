use crate::blockfrost_provider::{BlockfrostProvider, BlockfrostProviderError};
use tracing::warn;

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

    let mut message_ids = Vec::new();
    let mut skipped = 0u32;

    for redeemer in &redeemers {
        if redeemer.script_hash != mailbox_script_hash {
            continue;
        }
        let purpose = redeemer.purpose.to_lowercase();
        if purpose != "spend" {
            continue;
        }

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

        // Process redeemer: constructor == 1
        // Fields: [message, metadata, message_id, smt_proof]
        let constructor = match datum.get("constructor").and_then(|v| v.as_u64()) {
            Some(c) => c,
            None => continue,
        };
        if constructor != 1 {
            continue;
        }

        let fields = match datum.get("fields").and_then(|v| v.as_array()) {
            Some(f) => f,
            None => {
                warn!(tx_hash, "Process redeemer missing fields array, skipping");
                skipped += 1;
                continue;
            }
        };

        // message_id is at index 2
        if fields.len() < 3 {
            warn!(
                tx_hash,
                "Process redeemer has {} fields, expected >= 3, skipping",
                fields.len()
            );
            skipped += 1;
            continue;
        }

        let message_id_hex = match fields[2].get("bytes").and_then(|v| v.as_str()) {
            Some(h) => h,
            None => {
                warn!(
                    tx_hash,
                    "Process redeemer fields[2] missing bytes, skipping"
                );
                skipped += 1;
                continue;
            }
        };

        match hex::decode(message_id_hex) {
            Ok(bytes) if bytes.len() == 32 => {
                let mut id = [0u8; 32];
                id.copy_from_slice(&bytes);
                message_ids.push(id);
            }
            Ok(bytes) => {
                warn!(
                    tx_hash,
                    "message_id has {} bytes, expected 32, skipping",
                    bytes.len()
                );
                skipped += 1;
            }
            Err(e) => {
                warn!(tx_hash, "Invalid message_id hex: {e}, skipping");
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

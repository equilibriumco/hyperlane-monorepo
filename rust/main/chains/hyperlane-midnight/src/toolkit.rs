//! Subprocess wrapper for the Midnight handle submitter.
//!
//! The Midnight contract layer only has a TypeScript SDK
//! (`@midnight-ntwrk/midnight-js-contracts`), so the Rust relayer drives it
//! by spawning a Node.js submitter binary. The submitter reads a JSON
//! payload from stdin, calls the `handle` circuit, and prints a JSON
//! response on stdout. Anything else (logs, traces) goes to stderr and is
//! captured into the error variants so operators see what failed.

use std::process::Stdio;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use hyperlane_core::{ChainResult, HyperlaneMessage, H256, H512};

use crate::HyperlaneMidnightError;

/// JSON payload written to the submitter's stdin.
///
/// Field names use camelCase to match the Node ecosystem convention. All
/// byte arrays are encoded as `0x`-prefixed lowercase hex.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitRequest<'a> {
    /// Operation discriminator. Always `"submit"` for handle submissions.
    pub op: &'static str,
    /// Address of the deployed WarpRoute contract on Midnight.
    pub contract_address: String,
    /// Indexer GraphQL endpoint (HTTP) so the submitter can sync state.
    pub indexer_graphql_url: String,
    /// Indexer GraphQL WebSocket endpoint, derived from the HTTP URL.
    pub indexer_ws_url: String,
    /// Midnight node RPC endpoint.
    pub node_rpc_url: String,
    /// Proof server endpoint (HTTP).
    pub proof_server_url: String,
    /// Midnight network ID (e.g. `undeployed`, `testnet`, `mainnet`).
    pub network_id: String,
    /// Hyperlane message in canonical field form (numbers as decimal strings
    /// in JSON; bytes as `0x`-prefixed hex).
    pub message: WireMessage<'a>,
    /// Metadata fields extracted from MessageIdMultisigIsmMetadata.
    pub metadata: WireMetadata,
    /// Whether the recipient bytes should be decoded as a contract address
    /// (vs. user address). The WarpRoute exposes both variants via the
    /// circuit's final parameter.
    pub is_contract_recipient: bool,
}

/// Wire-format HyperlaneMessage. Numbers go through as decimal strings so
/// JavaScript's BigInt can ingest them without precision loss.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireMessage<'a> {
    /// Hyperlane protocol version.
    pub version: u8,
    /// Origin-chain dispatch nonce (decimal string).
    pub nonce: String,
    /// Origin domain id.
    pub origin: u32,
    /// `0x`-prefixed hex, 32 bytes.
    pub sender: String,
    /// Destination domain id (should match this chain's local domain).
    pub destination: u32,
    /// `0x`-prefixed hex, 32 bytes.
    pub recipient: String,
    /// `0x`-prefixed hex, 64 bytes (TokenMessage payload).
    pub body: String,
    /// Reference into the borrowed source message so we don't need to clone
    /// the body bytes when serializing. The hex string above is what the
    /// submitter actually consumes; this field is `#[serde(skip)]`.
    #[serde(skip)]
    pub _marker: std::marker::PhantomData<&'a HyperlaneMessage>,
}

/// Wire-format MessageIdMultisigIsm metadata, broken out per field so the
/// submitter doesn't have to know the upstream packed layout.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireMetadata {
    /// Origin-chain merkle tree hook address, `0x`-prefixed 32-byte hex.
    pub merkle_tree_hook: String,
    /// Validator-signed merkle root, `0x`-prefixed 32-byte hex.
    pub root: String,
    /// Index of this message in the origin chain's merkle tree.
    pub index: u32,
    /// Validator signatures, padded to 16 entries. Each entry is
    /// `0x`-prefixed 65-byte hex (`r || s || v`).
    pub signatures: Vec<String>,
}

/// JSON envelope written by the submitter on stdout.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitResponse {
    /// `0x`-prefixed transaction hash. Present on success, absent on error.
    #[serde(default)]
    pub tx_hash: Option<String>,
    /// Block height the transaction was included in. Reserved for future
    /// use by the operations dashboards (#34 scraper); parsed today so the
    /// JSON shape is stable.
    #[serde(default)]
    #[allow(dead_code)]
    pub block_height: Option<u64>,
    /// Structured error block. Present iff submission failed.
    #[serde(default)]
    pub error: Option<SubmitError>,
}

/// Structured error reported by the submitter.
#[derive(Debug, Deserialize)]
pub struct SubmitError {
    /// Short tag. Known values: `proofTimeout`, `insufficientDust`,
    /// `staleState`, `replay`, `signerUnavailable`, `rpcUnreachable`,
    /// `contractRevert`, `internal`.
    pub kind: String,
    /// Human-readable message.
    pub message: String,
}

/// Result of a successful submission, ready to be wrapped in `TxOutcome`.
#[derive(Debug)]
pub struct ToolkitOutcome {
    /// Transaction hash widened to 64 bytes (H256 → H512 for `TxOutcome`).
    pub transaction_id: H512,
    /// Whether the contract reported success. The submitter only reports
    /// `true` when the on-chain replay set was updated for this message.
    pub executed: bool,
}

/// Configuration values the toolkit wrapper needs at spawn time. These are
/// not all in `ConnectionConf` because some (proof server, network id) are
/// agent-side env, not chain config — operators set them via env vars.
#[derive(Debug, Clone)]
pub struct ToolkitContext {
    /// Path to the submitter binary (or wrapper script). Comes from
    /// `ConnectionConf::toolkit_path`.
    pub binary_path: String,
    /// Indexer GraphQL HTTP endpoint.
    pub indexer_graphql_url: String,
    /// Indexer GraphQL WebSocket endpoint.
    pub indexer_ws_url: String,
    /// Midnight node RPC endpoint.
    pub node_rpc_url: String,
    /// Proof server URL.
    pub proof_server_url: String,
    /// Midnight network id.
    pub network_id: String,
}

/// Invoke the submitter for one message and return the resulting outcome.
///
/// On failure each known submitter error kind maps to an explicit
/// [`HyperlaneMidnightError`] variant so the agent can surface meaningful
/// retry / alert behavior.
pub async fn submit_handle(
    ctx: &ToolkitContext,
    request: &SubmitRequest<'_>,
) -> ChainResult<ToolkitOutcome> {
    if ctx.binary_path.is_empty() {
        return Err(HyperlaneMidnightError::MissingSubmitterPath.into());
    }

    let payload = serde_json::to_vec(request).map_err(HyperlaneMidnightError::from)?;

    let mut child = Command::new(&ctx.binary_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| HyperlaneMidnightError::SubmitterSpawn {
            path: ctx.binary_path.clone(),
            source,
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&payload)
            .await
            .map_err(|source| HyperlaneMidnightError::SubmitterSpawn {
                path: ctx.binary_path.clone(),
                source,
            })?;
        stdin
            .shutdown()
            .await
            .map_err(|source| HyperlaneMidnightError::SubmitterSpawn {
                path: ctx.binary_path.clone(),
                source,
            })?;
    }

    let output =
        child
            .wait_with_output()
            .await
            .map_err(|source| HyperlaneMidnightError::SubmitterSpawn {
                path: ctx.binary_path.clone(),
                source,
            })?;

    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        return Err(HyperlaneMidnightError::SubmitterFailed {
            status: output.status.code().unwrap_or(-1),
            stderr,
        }
        .into());
    }

    let raw = String::from_utf8_lossy(&output.stdout).to_string();
    let response: SubmitResponse =
        serde_json::from_str(&raw).map_err(|err| HyperlaneMidnightError::SubmitterMalformed {
            message: err.to_string(),
            raw: truncate(&raw, 1024),
        })?;

    if let Some(error) = response.error {
        return Err(HyperlaneMidnightError::SubmitterReported {
            kind: error.kind,
            message: error.message,
        }
        .into());
    }

    let tx_hash = response.tx_hash.ok_or_else(|| {
        HyperlaneMidnightError::SubmitterMalformed {
            message: "missing `txHash` in response".to_string(),
            raw: truncate(&raw, 1024),
        }
    })?;

    Ok(ToolkitOutcome {
        transaction_id: parse_tx_hash(&tx_hash, &raw)?,
        executed: true,
    })
}

fn parse_tx_hash(hex_input: &str, raw: &str) -> Result<H512, HyperlaneMidnightError> {
    let trimmed = hex_input.strip_prefix("0x").unwrap_or(hex_input);
    let bytes = hex::decode(trimmed).map_err(|err| HyperlaneMidnightError::SubmitterMalformed {
        message: format!("invalid txHash hex: {err}"),
        raw: truncate(raw, 1024),
    })?;

    // Midnight tx hashes are 32 bytes; the relayer's `TxOutcome` carries an
    // H512 to accommodate longer hashes from other chains (e.g. Solana
    // signatures). Pack the 32 bytes into the high half and leave the rest
    // zeroed so the H512 round-trip is deterministic.
    if bytes.len() > 64 {
        return Err(HyperlaneMidnightError::SubmitterMalformed {
            message: format!("txHash too long: {} bytes", bytes.len()),
            raw: truncate(raw, 1024),
        });
    }
    let mut buf = [0u8; 64];
    buf[..bytes.len()].copy_from_slice(&bytes);
    Ok(H512::from(buf))
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut out = s[..max].to_string();
        out.push_str("…[truncated]");
        out
    }
}

/// JSON payload for the `delivered(id)` query. Same submitter binary, dispatched
/// on the `op` field.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IsDeliveredRequest {
    /// Operation discriminator.
    pub op: &'static str,
    /// Address of the deployed WarpRoute contract on Midnight.
    pub contract_address: String,
    /// Indexer GraphQL endpoint (HTTP).
    pub indexer_graphql_url: String,
    /// Indexer GraphQL WebSocket endpoint.
    pub indexer_ws_url: String,
    /// Midnight network ID.
    pub network_id: String,
    /// Message id to check, `0x`-prefixed 32-byte hex.
    pub message_id: String,
}

/// JSON envelope for `delivered(id)`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IsDeliveredResponse {
    /// True iff the WarpRoute contract has marked this `messageId` delivered.
    /// Absent when the submitter reports a structured error.
    #[serde(default)]
    pub delivered: Option<bool>,
    /// Structured error block. Present iff the query failed.
    #[serde(default)]
    pub error: Option<SubmitError>,
}

/// Query the on-chain `deliveries: Set<Bytes<32>>` membership for `message_id`
/// via the submitter subprocess. The submitter handles state deserialization
/// (Midnight ledger encoding) on the TS side; the Rust crate stays
/// runtime-free.
pub async fn query_delivered(
    ctx: &ToolkitContext,
    contract_address: H256,
    message_id: H256,
) -> ChainResult<bool> {
    if ctx.binary_path.is_empty() {
        return Err(HyperlaneMidnightError::MissingSubmitterPath.into());
    }

    let request = IsDeliveredRequest {
        op: "isDelivered",
        contract_address: format!("0x{contract_address:x}"),
        indexer_graphql_url: ctx.indexer_graphql_url.clone(),
        indexer_ws_url: ctx.indexer_ws_url.clone(),
        network_id: ctx.network_id.clone(),
        message_id: format!("0x{message_id:x}"),
    };
    let payload = serde_json::to_vec(&request).map_err(HyperlaneMidnightError::from)?;

    let mut child = Command::new(&ctx.binary_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| HyperlaneMidnightError::SubmitterSpawn {
            path: ctx.binary_path.clone(),
            source,
        })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&payload)
            .await
            .map_err(|source| HyperlaneMidnightError::SubmitterSpawn {
                path: ctx.binary_path.clone(),
                source,
            })?;
        stdin
            .shutdown()
            .await
            .map_err(|source| HyperlaneMidnightError::SubmitterSpawn {
                path: ctx.binary_path.clone(),
                source,
            })?;
    }

    let output =
        child
            .wait_with_output()
            .await
            .map_err(|source| HyperlaneMidnightError::SubmitterSpawn {
                path: ctx.binary_path.clone(),
                source,
            })?;

    if !output.status.success() {
        return Err(HyperlaneMidnightError::SubmitterFailed {
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        }
        .into());
    }

    let raw = String::from_utf8_lossy(&output.stdout).to_string();
    let response: IsDeliveredResponse =
        serde_json::from_str(&raw).map_err(|err| HyperlaneMidnightError::SubmitterMalformed {
            message: err.to_string(),
            raw: truncate(&raw, 1024),
        })?;

    if let Some(error) = response.error {
        return Err(HyperlaneMidnightError::SubmitterReported {
            kind: error.kind,
            message: error.message,
        }
        .into());
    }

    response
        .delivered
        .ok_or_else(|| HyperlaneMidnightError::SubmitterMalformed {
            message: "missing `delivered` boolean in response".to_string(),
            raw: truncate(&raw, 1024),
        })
        .map_err(Into::into)
}

/// Helper: take the relayer-supplied `(message, metadata, contract_address)`
/// triple and produce the JSON wire form expected by the submitter.
pub fn build_request<'a>(
    contract_address: H256,
    ctx: &ToolkitContext,
    message: &'a HyperlaneMessage,
    metadata: WireMetadata,
    is_contract_recipient: bool,
) -> SubmitRequest<'a> {
    SubmitRequest {
        op: "submit",
        contract_address: format!("0x{contract_address:x}"),
        indexer_graphql_url: ctx.indexer_graphql_url.clone(),
        indexer_ws_url: ctx.indexer_ws_url.clone(),
        node_rpc_url: ctx.node_rpc_url.clone(),
        proof_server_url: ctx.proof_server_url.clone(),
        network_id: ctx.network_id.clone(),
        message: WireMessage {
            version: message.version,
            nonce: message.nonce.to_string(),
            origin: message.origin,
            sender: format!("0x{:x}", message.sender),
            destination: message.destination,
            recipient: format!("0x{:x}", message.recipient),
            body: format!("0x{}", hex::encode(&message.body)),
            _marker: std::marker::PhantomData,
        },
        metadata,
        is_contract_recipient,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolkitContext {
        ToolkitContext {
            binary_path: "/bin/false".to_string(),
            indexer_graphql_url: "http://indexer/graphql".to_string(),
            indexer_ws_url: "ws://indexer/graphql".to_string(),
            node_rpc_url: "http://node:9944".to_string(),
            proof_server_url: "http://proof:6300".to_string(),
            network_id: "undeployed".to_string(),
        }
    }

    fn metadata() -> WireMetadata {
        WireMetadata {
            merkle_tree_hook: "0x00".to_string(),
            root: "0x11".to_string(),
            index: 7,
            signatures: vec!["0x22".to_string(); 16],
        }
    }

    #[test]
    fn build_request_serializes_to_expected_shape() {
        let message = HyperlaneMessage {
            version: 3,
            nonce: 42,
            origin: 5,
            sender: H256::from_low_u64_be(0xAB),
            destination: 1234,
            recipient: H256::from_low_u64_be(0xCD),
            body: vec![0u8; 64],
        };
        let request = build_request(H256::from_low_u64_be(1), &ctx(), &message, metadata(), false);
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"op\":\"submit\""));
        assert!(json.contains("\"version\":3"));
        assert!(json.contains("\"nonce\":\"42\""));
        assert!(json.contains("\"isContractRecipient\":false"));
        assert!(json.contains("\"signatures\":[\"0x22\","));
    }

    #[tokio::test]
    async fn missing_submitter_path_short_circuits() {
        let mut ctx = ctx();
        ctx.binary_path.clear();
        let message = HyperlaneMessage::default();
        let request = build_request(H256::zero(), &ctx, &message, metadata(), false);
        let err = submit_handle(&ctx, &request).await.unwrap_err();
        let display = format!("{err}");
        assert!(
            display.contains("not configured") || display.contains("toolkitPath"),
            "actual error: {display}"
        );
    }

    #[tokio::test]
    async fn empty_stdout_maps_to_malformed_error() {
        // `/usr/bin/true` exits 0 with no stdout. The wrapper must surface
        // this as `SubmitterMalformed` (JSON parse failure on empty input).
        let mut ctx = ctx();
        ctx.binary_path = "/usr/bin/true".to_string();
        let message = HyperlaneMessage::default();
        let request = build_request(H256::zero(), &ctx, &message, metadata(), false);
        let err = submit_handle(&ctx, &request).await.unwrap_err();
        let msg = format!("{err:?}").to_lowercase();
        assert!(
            msg.contains("malformed") || msg.contains("eof") || msg.contains("expected"),
            "actual error: {msg}"
        );
    }
}

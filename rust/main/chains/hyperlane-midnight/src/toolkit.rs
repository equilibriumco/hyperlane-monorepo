use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};

use hyperlane_core::{ChainResult, HyperlaneMessage, H160, H256, H512};

use crate::HyperlaneMidnightError;

const SUBMIT_TIMEOUT: Duration = Duration::from_secs(120);
const DELIVERED_TIMEOUT: Duration = Duration::from_secs(30);
const ANNOUNCE_TIMEOUT: Duration = Duration::from_secs(120);
const STORAGE_LOCATIONS_TIMEOUT: Duration = Duration::from_secs(60);

/// Maximum byte length of a storage location, matching the on-chain
/// `Bytes<480>` buffer and the `MAX_STORAGE_LOCATION_LEN` circuit. A location
/// longer than this is rejected before spawning the submitter.
pub const MAX_STORAGE_LOCATION_LEN: usize = 480;

/// JSON payload for the `submit` op.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitRequest<'a> {
    /// Operation discriminator.
    pub op: &'static str,
    /// Deployed WarpRoute contract address.
    pub contract_address: String,
    /// Indexer GraphQL endpoint (HTTP).
    pub indexer_graphql_url: String,
    /// Indexer GraphQL endpoint (WebSocket).
    pub indexer_ws_url: String,
    /// Midnight node RPC endpoint.
    pub node_rpc_url: String,
    /// Proof server endpoint.
    pub proof_server_url: String,
    /// Midnight network id.
    pub network_id: String,
    /// Hyperlane message.
    pub message: WireMessage<'a>,
    /// MessageIdMultisigIsm metadata.
    pub metadata: WireMetadata,
    /// Whether the recipient is a contract address.
    pub is_contract_recipient: bool,
}

/// Wire-format HyperlaneMessage.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireMessage<'a> {
    /// Hyperlane protocol version.
    pub version: u8,
    /// Origin-chain dispatch nonce (decimal string for JS BigInt safety).
    pub nonce: String,
    /// Origin domain id.
    pub origin: u32,
    /// `0x`-prefixed 32-byte hex.
    pub sender: String,
    /// Destination domain id.
    pub destination: u32,
    /// `0x`-prefixed 32-byte hex.
    pub recipient: String,
    /// `0x`-prefixed 64-byte hex (TokenMessage payload).
    pub body: String,
    /// Avoids cloning the body bytes during serialization.
    #[serde(skip)]
    pub _marker: std::marker::PhantomData<&'a HyperlaneMessage>,
}

/// Wire-format ISM metadata.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireMetadata {
    /// Origin-chain merkle tree hook address.
    pub merkle_tree_hook: String,
    /// Validator-signed merkle root.
    pub root: String,
    /// Merkle tree index.
    pub index: u32,
    /// Validator signatures, padded to 16 entries.
    pub signatures: Vec<String>,
}

/// JSON envelope returned by the `submit` op.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitResponse {
    /// Transaction hash on success.
    #[serde(default)]
    pub tx_hash: Option<String>,
    /// Block height on success.
    #[serde(default)]
    #[allow(dead_code)]
    pub block_height: Option<u64>,
    /// Structured error on failure.
    #[serde(default)]
    pub error: Option<SubmitError>,
}

/// Structured error from the submitter.
#[derive(Debug, Deserialize)]
pub struct SubmitError {
    /// Short kind tag.
    pub kind: String,
    /// Human-readable message.
    pub message: String,
}

/// Successful submission outcome.
#[derive(Debug)]
pub struct ToolkitOutcome {
    /// Transaction hash (Midnight 32 bytes packed into the low end of H512).
    pub transaction_id: H512,
    /// Whether the contract reported success.
    pub executed: bool,
}

/// Runtime values needed at spawn time.
#[derive(Debug, Clone)]
pub struct ToolkitContext {
    /// Path to the submitter binary.
    pub binary_path: String,
    /// Indexer GraphQL endpoint (HTTP).
    pub indexer_graphql_url: String,
    /// Indexer GraphQL endpoint (WebSocket).
    pub indexer_ws_url: String,
    /// Midnight node RPC endpoint.
    pub node_rpc_url: String,
    /// Proof server endpoint.
    pub proof_server_url: String,
    /// Midnight network id.
    pub network_id: String,
}

/// Invoke the submitter for one message and return its outcome.
pub async fn submit_handle(
    ctx: &ToolkitContext,
    request: &SubmitRequest<'_>,
) -> ChainResult<ToolkitOutcome> {
    let raw = run_submitter(ctx, request, SUBMIT_TIMEOUT).await?;
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

/// JSON payload for the `isDelivered` op.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IsDeliveredRequest {
    /// Operation discriminator.
    pub op: &'static str,
    /// Deployed WarpRoute contract address.
    pub contract_address: String,
    /// Indexer GraphQL endpoint (HTTP).
    pub indexer_graphql_url: String,
    /// Indexer GraphQL endpoint (WebSocket).
    pub indexer_ws_url: String,
    /// Midnight network id.
    pub network_id: String,
    /// Message id to check.
    pub message_id: String,
}

/// JSON envelope returned by the `isDelivered` op.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IsDeliveredResponse {
    /// Membership flag on success.
    #[serde(default)]
    pub delivered: Option<bool>,
    /// Structured error on failure.
    #[serde(default)]
    pub error: Option<SubmitError>,
}

/// Query on-chain `deliveries` membership via the submitter.
pub async fn query_delivered(
    ctx: &ToolkitContext,
    contract_address: H256,
    message_id: H256,
) -> ChainResult<bool> {
    let request = IsDeliveredRequest {
        op: "isDelivered",
        // Midnight contract addresses are bare hex — `@midnight-ntwrk/midnight-js-utils`
        // throws TypeError on any leading `0x`. Hyperlane addresses elsewhere
        // (EVM, Substrate, our own config parser) are `0x`-prefixed, so we
        // strip only at the Midnight-SDK seam.
        contract_address: format!("{contract_address:x}"),
        indexer_graphql_url: ctx.indexer_graphql_url.clone(),
        indexer_ws_url: ctx.indexer_ws_url.clone(),
        network_id: ctx.network_id.clone(),
        message_id: format!("0x{message_id:x}"),
    };

    let raw = run_submitter(ctx, &request, DELIVERED_TIMEOUT).await?;
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

/// JSON payload for the `announce` op (write tx).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnounceRequest {
    /// Operation discriminator.
    pub op: &'static str,
    /// Deployed ValidatorAnnounce contract address.
    pub contract_address: String,
    /// Indexer GraphQL endpoint (HTTP).
    pub indexer_graphql_url: String,
    /// Indexer GraphQL endpoint (WebSocket).
    pub indexer_ws_url: String,
    /// Midnight node RPC endpoint.
    pub node_rpc_url: String,
    /// Proof server endpoint.
    pub proof_server_url: String,
    /// Midnight network id.
    pub network_id: String,
    /// `0x`-prefixed 20-byte validator address.
    pub validator: String,
    /// `0x`-prefixed hex of the storage location bytes (unpadded; the
    /// submitter zero-pads to the on-chain `Bytes<480>` buffer).
    pub storage_location: String,
    /// Real byte length of the storage location.
    pub location_len: u16,
    /// `0x`-prefixed 65-byte ECDSA signature.
    pub signature: String,
}

/// Submit an `announce` write tx via the submitter. Rejects an empty or
/// over-long (> `MAX_STORAGE_LOCATION_LEN`) storage location before spawning
/// the subprocess so the on-chain asserts never fire on input we can catch.
pub async fn announce_tx(
    ctx: &ToolkitContext,
    contract_address: H256,
    validator: H160,
    storage_location: &str,
    signature: &[u8],
) -> ChainResult<ToolkitOutcome> {
    let bytes = storage_location.as_bytes();
    if bytes.is_empty() {
        return Err(HyperlaneMidnightError::Other(
            "announce: empty storage location".to_string(),
        )
        .into());
    }
    if bytes.len() > MAX_STORAGE_LOCATION_LEN {
        return Err(HyperlaneMidnightError::Other(format!(
            "announce: storage location is {} bytes, exceeds on-chain bound of {MAX_STORAGE_LOCATION_LEN}",
            bytes.len()
        ))
        .into());
    }

    let request = AnnounceRequest {
        op: "announce",
        // See `query_delivered` — Midnight rejects `0x` on contract addresses.
        contract_address: format!("{contract_address:x}"),
        indexer_graphql_url: ctx.indexer_graphql_url.clone(),
        indexer_ws_url: ctx.indexer_ws_url.clone(),
        node_rpc_url: ctx.node_rpc_url.clone(),
        proof_server_url: ctx.proof_server_url.clone(),
        network_id: ctx.network_id.clone(),
        validator: format!("0x{validator:x}"),
        storage_location: format!("0x{}", hex::encode(bytes)),
        location_len: bytes.len() as u16,
        signature: format!("0x{}", hex::encode(signature)),
    };

    let raw = run_submitter(ctx, &request, ANNOUNCE_TIMEOUT).await?;
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

/// JSON payload for the `getStorageLocations` op (read).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageLocationsRequest {
    /// Operation discriminator.
    pub op: &'static str,
    /// Deployed ValidatorAnnounce contract address.
    pub contract_address: String,
    /// Indexer GraphQL endpoint (HTTP).
    pub indexer_graphql_url: String,
    /// Indexer GraphQL endpoint (WebSocket).
    pub indexer_ws_url: String,
    /// Midnight network id.
    pub network_id: String,
    /// `0x`-prefixed 20-byte validator addresses to query.
    pub validators: Vec<String>,
}

/// JSON envelope returned by the `getStorageLocations` op.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageLocationsResponse {
    /// One list of locations per requested validator, in request order.
    #[serde(default)]
    pub locations: Option<Vec<Vec<String>>>,
    /// Structured error on failure.
    #[serde(default)]
    pub error: Option<SubmitError>,
}

/// Read announced storage locations for `validators` via the submitter.
pub async fn query_storage_locations(
    ctx: &ToolkitContext,
    contract_address: H256,
    validators: &[H160],
) -> ChainResult<Vec<Vec<String>>> {
    let request = StorageLocationsRequest {
        op: "getStorageLocations",
        // See `query_delivered` — Midnight rejects `0x` on contract addresses.
        contract_address: format!("{contract_address:x}"),
        indexer_graphql_url: ctx.indexer_graphql_url.clone(),
        indexer_ws_url: ctx.indexer_ws_url.clone(),
        network_id: ctx.network_id.clone(),
        validators: validators.iter().map(|v| format!("0x{v:x}")).collect(),
    };

    let raw = run_submitter(ctx, &request, STORAGE_LOCATIONS_TIMEOUT).await?;
    let response: StorageLocationsResponse =
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

    let locations = response.locations.ok_or_else(|| {
        HyperlaneMidnightError::SubmitterMalformed {
            message: "missing `locations` in response".to_string(),
            raw: truncate(&raw, 1024),
        }
    })?;

    if locations.len() != validators.len() {
        return Err(HyperlaneMidnightError::SubmitterMalformed {
            message: format!(
                "expected {} location lists, got {}",
                validators.len(),
                locations.len()
            ),
            raw: truncate(&raw, 1024),
        }
        .into());
    }

    Ok(locations)
}

async fn run_submitter<R: Serialize>(
    ctx: &ToolkitContext,
    request: &R,
    timeout: Duration,
) -> ChainResult<String> {
    if ctx.binary_path.is_empty() {
        return Err(HyperlaneMidnightError::MissingSubmitterPath.into());
    }

    let payload = serde_json::to_vec(request).map_err(HyperlaneMidnightError::from)?;

    let mut child: Child = Command::new(&ctx.binary_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
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

    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(source)) => {
            return Err(HyperlaneMidnightError::SubmitterSpawn {
                path: ctx.binary_path.clone(),
                source,
            }
            .into());
        }
        Err(_elapsed) => {
            return Err(HyperlaneMidnightError::SubmitterTimeout {
                elapsed_secs: timeout.as_secs(),
            }
            .into());
        }
    };

    if !output.status.success() {
        return Err(HyperlaneMidnightError::SubmitterFailed {
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        }
        .into());
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Build a `SubmitRequest` from a HyperlaneMessage + metadata.
pub fn build_request<'a>(
    contract_address: H256,
    ctx: &ToolkitContext,
    message: &'a HyperlaneMessage,
    metadata: WireMetadata,
    is_contract_recipient: bool,
) -> SubmitRequest<'a> {
    SubmitRequest {
        op: "submit",
        // See `query_delivered` — Midnight rejects `0x` on contract addresses.
        contract_address: format!("{contract_address:x}"),
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
        assert!(display.contains("not configured") || display.contains("toolkitPath"));
    }

    #[tokio::test]
    async fn empty_stdout_maps_to_malformed_error() {
        let mut ctx = ctx();
        ctx.binary_path = "/usr/bin/true".to_string();
        let message = HyperlaneMessage::default();
        let request = build_request(H256::zero(), &ctx, &message, metadata(), false);
        let err = submit_handle(&ctx, &request).await.unwrap_err();
        let msg = format!("{err:?}").to_lowercase();
        assert!(msg.contains("malformed") || msg.contains("eof") || msg.contains("expected"));
    }

    #[tokio::test]
    async fn timeout_elapses_and_kills_child() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let tmp = std::env::temp_dir().join(format!(
            "midnight-toolkit-timeout-{}.sh",
            std::process::id()
        ));
        {
            let mut f = std::fs::File::create(&tmp).unwrap();
            f.write_all(b"#!/bin/sh\nsleep 5\n").unwrap();
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let mut ctx = ctx();
        ctx.binary_path = tmp.to_string_lossy().into_owned();
        let payload = serde_json::json!({"op": "submit"});

        let started = std::time::Instant::now();
        let err = run_submitter(&ctx, &payload, Duration::from_millis(100))
            .await
            .unwrap_err();
        let elapsed = started.elapsed();

        let _ = std::fs::remove_file(&tmp);

        assert!(elapsed < Duration::from_secs(2));
        let display = format!("{err}");
        assert!(display.contains("timeout") || display.contains("SIGKILL"));
    }
}

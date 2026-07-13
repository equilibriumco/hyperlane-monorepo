use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};

use hyperlane_core::{ChainResult, HyperlaneMessage, H160, H256, H512};

use crate::HyperlaneMidnightError;

/// How long the relayer waits for the submitter subprocess to build the handle
/// proof and land the tx. A real handle proof (multisig + ZK verify) can take
/// many minutes on a RAM-constrained host, so this is overridable via
/// `MIDNIGHT_SUBMIT_TIMEOUT_SECS` (default 120s) — mirrors the proof server's
/// own `MIDNIGHT_PROOF_SERVER_JOB_TIMEOUT`.
fn submit_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("MIDNIGHT_SUBMIT_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(120),
    )
}
const DELIVERED_TIMEOUT: Duration = Duration::from_secs(30);
const ANNOUNCE_TIMEOUT: Duration = Duration::from_secs(120);
const STORAGE_LOCATIONS_TIMEOUT: Duration = Duration::from_secs(60);
// Dry-run fetches state and executes `handle` locally (keccak/ecrecover
// witnesses, no proving, no broadcast) — heavier than the `isDelivered`
// read but far cheaper than a real submission.
const DRY_RUN_TIMEOUT: Duration = Duration::from_secs(60);

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
    /// Validator signatures — the real, quorum-sized set the metadata carried
    /// (at most `MAX_VALIDATORS`), forwarded unpadded. The Midnight submitter
    /// pads the on-chain `Vector<4>` by repeating slot 0.
    pub signatures: Vec<String>,
}

/// JSON payload for the `dryRunHandle` op — same message + metadata as
/// `submit`, minus the node/proof endpoints (no transaction is built).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DryRunHandleRequest<'a> {
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
    /// Hyperlane message.
    pub message: WireMessage<'a>,
    /// MessageIdMultisigIsm metadata.
    pub metadata: WireMetadata,
    /// Whether the recipient is a contract address.
    pub is_contract_recipient: bool,
}

/// JSON envelope returned by the `dryRunHandle` op.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DryRunResponse {
    /// `true` when the message would be accepted on-chain (no revert).
    #[serde(default)]
    pub ok: Option<bool>,
    /// Structured error when the message would revert, or on a transport
    /// failure talking to the indexer.
    #[serde(default)]
    pub error: Option<SubmitError>,
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
    let raw = run_submitter(ctx, request, submit_timeout()).await?;
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

    let tx_hash = response
        .tx_hash
        .ok_or_else(|| HyperlaneMidnightError::SubmitterMalformed {
            message: "missing `txHash` in response".to_string(),
            raw: truncate(&raw, 1024),
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
        return Err(
            HyperlaneMidnightError::Other("announce: empty storage location".to_string()).into(),
        );
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

    let tx_hash = response
        .tx_hash
        .ok_or_else(|| HyperlaneMidnightError::SubmitterMalformed {
            message: "missing `txHash` in response".to_string(),
            raw: truncate(&raw, 1024),
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

    let locations =
        response
            .locations
            .ok_or_else(|| HyperlaneMidnightError::SubmitterMalformed {
                message: "missing `locations` in response".to_string(),
                raw: truncate(&raw, 1024),
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
        stdin.write_all(&payload).await.map_err(|source| {
            HyperlaneMidnightError::SubmitterSpawn {
                path: ctx.binary_path.clone(),
                source,
            }
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

/// Build the wire-format message shared by the `submit` and `dryRunHandle`
/// payloads. `sender`/`recipient` keep the `0x` prefix (only the contract
/// address is bare-hex at the Midnight-SDK seam — see `query_delivered`).
fn wire_message(message: &HyperlaneMessage) -> WireMessage<'_> {
    WireMessage {
        version: message.version,
        nonce: message.nonce.to_string(),
        origin: message.origin,
        sender: format!("0x{:x}", message.sender),
        destination: message.destination,
        recipient: format!("0x{:x}", message.recipient),
        body: format!("0x{}", hex::encode(&message.body)),
        _marker: std::marker::PhantomData,
    }
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
        message: wire_message(message),
        metadata,
        is_contract_recipient,
    }
}

/// Build a `DryRunHandleRequest` mirroring `build_request` so the dry-run
/// executes exactly what `process` would submit — only the op and the
/// absence of node/proof endpoints differ.
pub fn build_dry_run_request<'a>(
    contract_address: H256,
    ctx: &ToolkitContext,
    message: &'a HyperlaneMessage,
    metadata: WireMetadata,
    is_contract_recipient: bool,
) -> DryRunHandleRequest<'a> {
    DryRunHandleRequest {
        op: "dryRunHandle",
        contract_address: format!("{contract_address:x}"),
        indexer_graphql_url: ctx.indexer_graphql_url.clone(),
        indexer_ws_url: ctx.indexer_ws_url.clone(),
        network_id: ctx.network_id.clone(),
        message: wire_message(message),
        metadata,
        is_contract_recipient,
    }
}

/// Dry-run `handle` against current chain state via the submitter, WITHOUT
/// proving or submitting. `Ok(())` means the message would be accepted
/// on-chain; `Err` means it would revert (mapped from the submitter's
/// structured error) or the submitter failed. The relayer's
/// `process_estimate_costs` uses this so a reverting message is caught at
/// prepare time and backs off instead of busy-looping (issue #80).
pub async fn dry_run_handle(
    ctx: &ToolkitContext,
    request: &DryRunHandleRequest<'_>,
) -> ChainResult<()> {
    let raw = run_submitter(ctx, request, DRY_RUN_TIMEOUT).await?;
    let response: DryRunResponse =
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

    if response.ok == Some(true) {
        Ok(())
    } else {
        Err(HyperlaneMidnightError::SubmitterMalformed {
            message: "dryRunHandle response missing both `ok` and `error`".to_string(),
            raw: truncate(&raw, 1024),
        }
        .into())
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
            signatures: vec!["0x22".to_string(); 2],
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
        let request = build_request(
            H256::from_low_u64_be(1),
            &ctx(),
            &message,
            metadata(),
            false,
        );
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
    async fn storage_locations_length_mismatch_is_malformed() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        // Mock submitter: echo a well-formed `getStorageLocations` envelope
        // whose `locations` array has fewer entries than the validators we
        // request. This guards the positional validator->location mapping:
        // the results must line up with the requested order, so a length
        // mismatch has to surface as a clear error rather than silently
        // misaligning.
        let tmp = std::env::temp_dir().join(format!(
            "midnight-toolkit-locmismatch-{}.sh",
            std::process::id()
        ));
        {
            let mut f = std::fs::File::create(&tmp).unwrap();
            // Two validators requested below, but only one location list back.
            f.write_all(b"#!/bin/sh\necho '{\"locations\":[[\"s3://only-one\"]]}'\n")
                .unwrap();
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let mut ctx = ctx();
        ctx.binary_path = tmp.to_string_lossy().into_owned();

        let validators = vec![H160::repeat_byte(0x11), H160::repeat_byte(0x22)];
        let err = query_storage_locations(&ctx, H256::from_low_u64_be(0xabcd), &validators)
            .await
            .unwrap_err();

        let _ = std::fs::remove_file(&tmp);

        // The guard reports the expected vs. actual list counts.
        let msg = format!("{err}");
        assert!(
            msg.contains("expected 2 location lists, got 1"),
            "unexpected error: {msg}"
        );
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

    // Write an executable mock submitter that prints `json_response` on
    // stdout, mirroring the inline scripts above. Caller removes the file.
    fn write_mock_submitter(tag: &str, json_response: &str) -> std::path::PathBuf {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let path =
            std::env::temp_dir().join(format!("midnight-toolkit-{tag}-{}.sh", std::process::id()));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(format!("#!/bin/sh\necho '{json_response}'\n").as_bytes())
            .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn sample_message() -> HyperlaneMessage {
        HyperlaneMessage {
            version: 3,
            nonce: 42,
            origin: 5,
            sender: H256::from_low_u64_be(0xAB),
            destination: 1234,
            recipient: H256::from_low_u64_be(0xCD),
            body: vec![0u8; 64],
        }
    }

    #[test]
    fn build_dry_run_request_serializes_to_expected_shape() {
        let msg = sample_message();
        let request = build_dry_run_request(H256::from_low_u64_be(1), &ctx(), &msg, metadata(), false);
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"op\":\"dryRunHandle\""));
        assert!(json.contains("\"version\":3"));
        assert!(json.contains("\"isContractRecipient\":false"));
        // The dry-run payload omits the node/proof endpoints — no tx is built.
        assert!(!json.contains("nodeRpcUrl"));
        assert!(!json.contains("proofServerUrl"));
    }

    // `{"ok":true}` -> the message would be accepted -> `Ok(())`, which the
    // relayer reads as "gas estimate succeeded, proceed".
    #[tokio::test]
    async fn dry_run_ok_response_is_ok() {
        let script = write_mock_submitter("dryrun-ok", "{\"ok\":true}");
        let mut ctx = ctx();
        ctx.binary_path = script.to_string_lossy().into_owned();
        let msg = sample_message();
        let request = build_dry_run_request(H256::zero(), &ctx, &msg, metadata(), false);
        let result = dry_run_handle(&ctx, &request).await;
        let _ = std::fs::remove_file(&script);
        assert!(result.is_ok(), "expected Ok, got {result:?}");
    }

    // A structured revert error -> `Err` carrying the kind + message, which
    // the relayer turns into `ErrorEstimatingGas` -> exponential backoff.
    #[tokio::test]
    async fn dry_run_error_response_maps_to_submitter_reported() {
        let script = write_mock_submitter(
            "dryrun-revert",
            "{\"error\":{\"kind\":\"contractRevert\",\"message\":\"Mailbox: bad version\"}}",
        );
        let mut ctx = ctx();
        ctx.binary_path = script.to_string_lossy().into_owned();
        let msg = sample_message();
        let request = build_dry_run_request(H256::zero(), &ctx, &msg, metadata(), false);
        let err = dry_run_handle(&ctx, &request).await.unwrap_err();
        let _ = std::fs::remove_file(&script);
        let display = format!("{err}");
        assert!(display.contains("contractRevert"), "unexpected error: {display}");
        assert!(display.contains("bad version"), "unexpected error: {display}");
    }

    // Neither `ok` nor `error` -> malformed. Defensive: the Node op always
    // sends one or the other, so this only fires on a protocol drift.
    #[tokio::test]
    async fn dry_run_missing_ok_and_error_is_malformed() {
        let script = write_mock_submitter("dryrun-empty", "{}");
        let mut ctx = ctx();
        ctx.binary_path = script.to_string_lossy().into_owned();
        let msg = sample_message();
        let request = build_dry_run_request(H256::zero(), &ctx, &msg, metadata(), false);
        let err = dry_run_handle(&ctx, &request).await.unwrap_err();
        let _ = std::fs::remove_file(&script);
        let display = format!("{err:?}").to_lowercase();
        assert!(
            display.contains("missing") || display.contains("malformed"),
            "unexpected error: {display}"
        );
    }

    // The submitter-timeout / SIGKILL path is shared with all ops via
    // `run_submitter`, covered by `timeout_elapses_and_kills_child` above.
}

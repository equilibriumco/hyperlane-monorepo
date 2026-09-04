use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};

use hyperlane_core::{ChainResult, HyperlaneMessage, H160, H256, H512, U256};

use crate::HyperlaneMidnightError;

/// How long an agent waits for the submitter subprocess to prove and land a
/// transaction. Governs the heavy `submit` and `announce` proofs, which take
/// minutes on a RAM-constrained host, so the default is 1800s and
/// `MIDNIGHT_SUBMIT_TIMEOUT_SECS` overrides it.
fn submit_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("MIDNIGHT_SUBMIT_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(1800),
    )
}
const DELIVERED_TIMEOUT: Duration = Duration::from_secs(30);
const STORAGE_LOCATIONS_TIMEOUT: Duration = Duration::from_secs(60);
const DRY_RUN_TIMEOUT: Duration = Duration::from_secs(60);
const BALANCE_TIMEOUT: Duration = Duration::from_secs(120);

/// Matches the contract's `Bytes<480>` location buffer.
pub const MAX_STORAGE_LOCATION_LEN: usize = 480;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitRequest<'a> {
    pub op: &'static str,
    pub contract_address: String,
    pub indexer_graphql_url: String,
    pub indexer_ws_url: String,
    pub proof_server_url: String,
    pub message: WireMessage<'a>,
    pub metadata: WireMetadata,
    pub is_contract_recipient: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireMessage<'a> {
    pub version: u8,
    /// Origin-chain dispatch nonce (decimal string for JS BigInt safety).
    pub nonce: String,
    pub origin: u32,
    pub sender: String,
    pub destination: u32,
    pub recipient: String,
    pub body: String,
    /// Avoids cloning the body bytes during serialization.
    #[serde(skip)]
    pub _marker: std::marker::PhantomData<&'a HyperlaneMessage>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireMetadata {
    pub merkle_tree_hook: String,
    pub root: String,
    pub index: u32,
    /// Validator signatures, forwarded unpadded. The submitter pads the
    /// on-chain `Vector<4>` by repeating slot 0.
    pub signatures: Vec<String>,
}

/// Same message and metadata as `submit`, minus the node and proof endpoints:
/// no transaction is built.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DryRunHandleRequest<'a> {
    pub op: &'static str,
    pub contract_address: String,
    pub indexer_graphql_url: String,
    pub indexer_ws_url: String,
    pub message: WireMessage<'a>,
    pub metadata: WireMetadata,
    pub is_contract_recipient: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DryRunResponse {
    #[serde(default)]
    pub ok: Option<bool>,
    #[serde(default)]
    pub error: Option<SubmitError>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitResponse {
    #[serde(default)]
    pub tx_hash: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub block_height: Option<u64>,
    #[serde(default)]
    pub fee_specks: Option<String>,
    #[serde(default)]
    pub error: Option<SubmitError>,
}

#[derive(Debug, Deserialize)]
pub struct SubmitError {
    pub kind: String,
    pub message: String,
}

/// JSON payload for the `balance` op. The sidecar reads its own wallet;
/// `address`, when set, must match that wallet's Bech32m unshielded address.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceRequest<'a> {
    pub op: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceResponse {
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub night_micro: Option<String>,
    #[serde(default)]
    pub dust_specks: Option<String>,
    #[serde(default)]
    pub error: Option<SubmitError>,
}

#[derive(Debug, Clone)]
pub struct WalletBalances {
    pub address: String,
    pub night_micro: U256,
    pub dust_specks: U256,
}

/// Read the sidecar wallet's balances. Costs a wallet sync inside the
/// subprocess, so callers should cache.
pub async fn query_wallet_balance(
    ctx: &ToolkitContext,
    address: Option<&str>,
) -> ChainResult<WalletBalances> {
    let request = BalanceRequest {
        op: "balance",
        address,
    };
    let raw = run_submitter(ctx, &request, BALANCE_TIMEOUT).await?;
    let response: BalanceResponse =
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

    let malformed = |what: &str| HyperlaneMidnightError::SubmitterMalformed {
        message: format!("missing or invalid `{what}` in balance response"),
        raw: truncate(&raw, 1024),
    };
    let wallet_address = response.address.ok_or_else(|| malformed("address"))?;
    let night_micro = response
        .night_micro
        .as_deref()
        .and_then(|s| U256::from_dec_str(s).ok())
        .ok_or_else(|| malformed("nightMicro"))?;
    let dust_specks = response
        .dust_specks
        .as_deref()
        .and_then(|s| U256::from_dec_str(s).ok())
        .unwrap_or_default();

    Ok(WalletBalances {
        address: wallet_address,
        night_micro,
        dust_specks,
    })
}

#[derive(Debug)]
pub struct ToolkitOutcome {
    /// Transaction hash (Midnight 32 bytes packed into the low end of H512).
    pub transaction_id: H512,
    pub executed: bool,
    /// DUST paid, in specks; `None` when the submitter omits the field.
    pub fee_specks: Option<U256>,
}

#[derive(Debug, Clone)]
pub struct ToolkitContext {
    pub binary_path: String,
    pub indexer_graphql_url: String,
    pub indexer_ws_url: String,
    pub proof_server_url: String,
}

const DEFAULT_PROOF_SERVER_URL: &str = "http://127.0.0.1:6300";

impl ToolkitContext {
    /// Build the sidecar context from the chain config and the `MIDNIGHT_*` env.
    pub fn from_conf(conf: &crate::ConnectionConf) -> Self {
        Self {
            binary_path: conf.toolkit_path.clone().unwrap_or_default(),
            indexer_graphql_url: conf.indexer_graphql_url.to_string(),
            indexer_ws_url: derive_ws_url(&conf.indexer_graphql_url),
            proof_server_url: std::env::var("MIDNIGHT_PROOF_SERVER_URL")
                .unwrap_or_else(|_| DEFAULT_PROOF_SERVER_URL.to_string()),
        }
    }
}

fn derive_ws_url(http: &url::Url) -> String {
    let mut ws = http.clone();
    let scheme = match http.scheme() {
        "https" => "wss",
        _ => "ws",
    };
    let _ = ws.set_scheme(scheme);
    let path = ws.path().to_string();
    if !path.ends_with("/ws") {
        ws.set_path(&format!("{path}/ws"));
    }
    ws.to_string()
}

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

    let fee_specks = response
        .fee_specks
        .as_deref()
        .and_then(|s| U256::from_dec_str(s).ok());

    Ok(ToolkitOutcome {
        transaction_id: parse_tx_hash(&tx_hash, &raw)?,
        executed: true,
        fee_specks,
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
    // Right-aligned into the H512, matching the H256 -> H512 widening the
    // event indexers use, so `h512_to_h256` accepts either form.
    let mut buf = [0u8; 64];
    buf[64 - bytes.len()..].copy_from_slice(&bytes);
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IsDeliveredRequest {
    pub op: &'static str,
    pub contract_address: String,
    pub indexer_graphql_url: String,
    pub indexer_ws_url: String,
    pub message_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IsDeliveredResponse {
    #[serde(default)]
    pub delivered: Option<bool>,
    #[serde(default)]
    pub error: Option<SubmitError>,
}

pub async fn query_delivered(
    ctx: &ToolkitContext,
    contract_address: H256,
    message_id: H256,
) -> ChainResult<bool> {
    let request = IsDeliveredRequest {
        op: "isDelivered",
        // Midnight contract addresses are bare hex; the SDK throws on a
        // leading `0x`. Hyperlane addresses carry one everywhere else, so the
        // prefix is stripped only at this seam.
        contract_address: format!("{contract_address:x}"),
        indexer_graphql_url: ctx.indexer_graphql_url.clone(),
        indexer_ws_url: ctx.indexer_ws_url.clone(),
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnounceRequest {
    pub op: &'static str,
    pub contract_address: String,
    pub indexer_graphql_url: String,
    pub indexer_ws_url: String,
    pub proof_server_url: String,
    pub validator: String,
    /// `0x`-prefixed hex of the whole zero-padded `Bytes<480>` buffer — the
    /// exact bytes the validator signed over, since the on-chain digest hashes
    /// the padding too.
    pub storage_location: String,
    pub signature: String,
    /// `0x`-prefixed 64-byte secp256k1 public-key body (X_be || Y_be, no 0x04
    /// SEC1 tag), recovered off-chain. Compact has no in-circuit ecrecover, so
    /// the `announce` circuit derives the validator address from the pubkey.
    pub pubkey: String,
}

/// Submit an `announce` write tx. Both the padded location buffer and the
/// recovered pubkey are validated before spawning the subprocess, so the
/// on-chain asserts never fire on input catchable here.
pub async fn announce_tx(
    ctx: &ToolkitContext,
    contract_address: H256,
    validator: H160,
    storage_location: &str,
    signature: &[u8],
    pubkey: &[u8],
) -> ChainResult<ToolkitOutcome> {
    let bytes = storage_location.as_bytes();
    if bytes.len() != MAX_STORAGE_LOCATION_LEN {
        return Err(HyperlaneMidnightError::Other(format!(
            "announce: storage location must be the padded {MAX_STORAGE_LOCATION_LEN}-byte buffer, got {} bytes",
            bytes.len()
        ))
        .into());
    }
    if bytes[0] == 0 {
        return Err(
            HyperlaneMidnightError::Other("announce: empty storage location".to_string()).into(),
        );
    }
    // A trailing NUL must survive so off-chain readers can trim the padding.
    if bytes[MAX_STORAGE_LOCATION_LEN - 1] != 0 {
        return Err(HyperlaneMidnightError::Other(
            "announce: storage location fills the padded buffer with no trailing NUL".to_string(),
        )
        .into());
    }
    if pubkey.len() != 64 {
        return Err(HyperlaneMidnightError::Other(format!(
            "announce: pubkey must be the 64-byte X_be||Y_be body, got {} bytes",
            pubkey.len()
        ))
        .into());
    }

    let request = AnnounceRequest {
        op: "announce",
        // See `query_delivered` — Midnight rejects `0x` on contract addresses.
        contract_address: format!("{contract_address:x}"),
        indexer_graphql_url: ctx.indexer_graphql_url.clone(),
        indexer_ws_url: ctx.indexer_ws_url.clone(),
        proof_server_url: ctx.proof_server_url.clone(),
        validator: format!("0x{validator:x}"),
        storage_location: format!("0x{}", hex::encode(bytes)),
        signature: format!("0x{}", hex::encode(signature)),
        pubkey: format!("0x{}", hex::encode(pubkey)),
    };

    let raw = run_submitter(ctx, &request, submit_timeout()).await?;
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

    let fee_specks = response
        .fee_specks
        .as_deref()
        .and_then(|s| U256::from_dec_str(s).ok());

    Ok(ToolkitOutcome {
        transaction_id: parse_tx_hash(&tx_hash, &raw)?,
        executed: true,
        fee_specks,
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageLocationsRequest {
    pub op: &'static str,
    pub contract_address: String,
    pub indexer_graphql_url: String,
    pub indexer_ws_url: String,
    pub validators: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageLocationsResponse {
    #[serde(default)]
    pub locations: Option<Vec<Vec<String>>>,
    #[serde(default)]
    pub error: Option<SubmitError>,
}

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

    // The read op already trims at the first NUL, but trim again here: the
    // validator compares this against its own unpadded location and would
    // re-announce forever if padding leaked through.
    let trimmed = locations
        .into_iter()
        .map(|per_validator| {
            per_validator
                .into_iter()
                .map(|loc| loc.split('\0').next().unwrap_or("").to_string())
                .collect()
        })
        .collect();

    Ok(trimmed)
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
/// payloads. Only the contract address is bare hex; everything else keeps its
/// `0x` prefix.
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
        proof_server_url: ctx.proof_server_url.clone(),
        message: wire_message(message),
        metadata,
        is_contract_recipient,
    }
}

/// Build a `DryRunHandleRequest` mirroring `build_request`, so the dry run
/// executes exactly what `process` would submit.
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
        message: wire_message(message),
        metadata,
        is_contract_recipient,
    }
}

/// `Err` means the message would revert on-chain, or the submitter failed.
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

    #[test]
    fn derive_ws_url_appends_ws_path_and_swaps_scheme() {
        let http = url::Url::parse("http://indexer.local/api/v3/graphql").unwrap();
        assert_eq!(derive_ws_url(&http), "ws://indexer.local/api/v3/graphql/ws");

        let https = url::Url::parse("https://indexer.example/graphql").unwrap();
        assert_eq!(derive_ws_url(&https), "wss://indexer.example/graphql/ws");
    }

    #[test]
    fn balance_response_parses_and_validates() {
        let ok: BalanceResponse = serde_json::from_str(
            r#"{"address":"mn_addr_test1qxy","nightMicro":"123456","dustSpecks":"789"}"#,
        )
        .unwrap();
        assert_eq!(ok.address.as_deref(), Some("mn_addr_test1qxy"));
        assert_eq!(ok.night_micro.as_deref(), Some("123456"));
        assert_eq!(ok.dust_specks.as_deref(), Some("789"));

        let err: BalanceResponse =
            serde_json::from_str(r#"{"error":{"kind":"internal","message":"address mismatch"}}"#)
                .unwrap();
        assert!(err.error.is_some());
    }

    fn ctx() -> ToolkitContext {
        ToolkitContext {
            binary_path: "/bin/false".to_string(),
            indexer_graphql_url: "http://indexer/graphql".to_string(),
            indexer_ws_url: "ws://indexer/graphql".to_string(),
            proof_server_url: "http://proof:6300".to_string(),
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

        // The results are mapped to validators positionally, so a short
        // `locations` array must error rather than silently misalign.
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

    // An executable mock submitter that prints `json_response` on stdout. The
    // caller removes the file.
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
        let request =
            build_dry_run_request(H256::from_low_u64_be(1), &ctx(), &msg, metadata(), false);
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("\"op\":\"dryRunHandle\""));
        assert!(json.contains("\"version\":3"));
        assert!(json.contains("\"isContractRecipient\":false"));
        // No tx is built, so the proof endpoint is absent.
        assert!(!json.contains("proofServerUrl"));
    }

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

    // A revert must carry the kind and message through, since the relayer turns
    // that into `ErrorEstimatingGas` and backs off.
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
        assert!(
            display.contains("contractRevert"),
            "unexpected error: {display}"
        );
        assert!(
            display.contains("bad version"),
            "unexpected error: {display}"
        );
    }

    // The op always sends one or the other, so this only fires on drift.
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
}

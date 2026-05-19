//! Destination-side `Mailbox` implementation for Midnight.
//!
//! The relayer calls `process(message, metadata, ...)` once it has gathered a
//! signed checkpoint quorum. This impl parses the canonical
//! MessageIdMultisigIsmMetadata layout, packages the fields as JSON, and
//! spawns the Midnight handle submitter as a subprocess. The submitter
//! (TypeScript, in `equilibriumco/hyperlane-midnight`) generates the ZK
//! proof and broadcasts the `handle` circuit call.

use std::str::FromStr;

use async_trait::async_trait;

use hyperlane_core::{
    ChainCommunicationError, ChainResult, ContractLocator, FixedPointNumber, HyperlaneChain,
    HyperlaneContract, HyperlaneDomain, HyperlaneMessage, HyperlaneProvider, Mailbox, Metadata,
    ReorgPeriod, TxCostEstimate, TxOutcome, H256, U256,
};

use crate::toolkit::{self, ToolkitContext, WireMetadata};
use crate::{ConnectionConf, HyperlaneMidnightError, MidnightIndexerClient, MidnightProvider};

/// Maximum signature slots in the on-chain `Vector<16, Bytes<65>>`. Mirrors
/// `MAX_VALIDATORS` in the Compact contract.
const MAX_SIGNATURES: usize = 16;
/// Length of a single signature (`r || s || v`).
const SIGNATURE_LEN: usize = 65;
/// MessageIdMultisigIsmMetadata layout header length:
/// `merkle_tree_hook (32) || root (32) || index (4)`.
const METADATA_HEADER_LEN: usize = 32 + 32 + 4;

/// Default proof-server endpoint used when the env override is unset. Keeps
/// the devnet shape (see `hyperlane-midnight/devnet/src/config.ts`).
const DEFAULT_PROOF_SERVER_URL: &str = "http://127.0.0.1:6300";
/// Default Midnight network id when not overridden via env.
const DEFAULT_NETWORK_ID: &str = "undeployed";

/// Destination-side Mailbox.
#[derive(Debug, Clone)]
pub struct MidnightMailbox {
    address: H256,
    domain: HyperlaneDomain,
    provider: MidnightProvider,
    toolkit_ctx: ToolkitContext,
}

impl MidnightMailbox {
    /// Build a new Mailbox bound to the given chain config and contract
    /// locator. The provider exposes the indexer URL for `HyperlaneChain`
    /// consumers; the toolkit context drives `process`.
    pub fn new(locator: &ContractLocator<'_>, conf: &ConnectionConf) -> ChainResult<Self> {
        let indexer = MidnightIndexerClient::new(conf.indexer_graphql_url.clone());
        let provider = MidnightProvider::new(locator.domain.clone(), indexer);

        let binary_path = conf.toolkit_path.clone().unwrap_or_default();
        let indexer_graphql_url = conf.indexer_graphql_url.to_string();
        let indexer_ws_url = derive_ws_url(&conf.indexer_graphql_url);
        let node_rpc_url = std::env::var("MIDNIGHT_NODE_RPC_URL").unwrap_or_default();
        let proof_server_url = std::env::var("MIDNIGHT_PROOF_SERVER_URL")
            .unwrap_or_else(|_| DEFAULT_PROOF_SERVER_URL.to_string());
        let network_id =
            std::env::var("MIDNIGHT_NETWORK_ID").unwrap_or_else(|_| DEFAULT_NETWORK_ID.to_string());

        let toolkit_ctx = ToolkitContext {
            binary_path,
            indexer_graphql_url,
            indexer_ws_url,
            node_rpc_url,
            proof_server_url,
            network_id,
        };

        Ok(Self {
            address: locator.address,
            domain: locator.domain.clone(),
            provider,
            toolkit_ctx,
        })
    }
}

/// Convert the indexer's HTTP URL to a WebSocket URL using the same path
/// convention midnight-indexer ships (`/graphql/ws` alongside `/graphql`).
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

impl HyperlaneChain for MidnightMailbox {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(self.provider.clone())
    }
}

impl HyperlaneContract for MidnightMailbox {
    fn address(&self) -> H256 {
        self.address
    }
}

#[async_trait]
impl Mailbox for MidnightMailbox {
    /// Destination-side has no dispatch nonce to report. The relayer doesn't
    /// rely on this value for inbound delivery; the proper outbound count
    /// arrives with #9 + #16.
    async fn count(&self, _reorg_period: &ReorgPeriod) -> ChainResult<u32> {
        Ok(0)
    }

    async fn delivered(&self, id: H256) -> ChainResult<bool> {
        toolkit::query_delivered(&self.toolkit_ctx, self.address, id).await
    }

    /// The WarpRoute is monolithic — the Mailbox, ISM, hook, and warp logic
    /// all live in one contract, so the "default ISM" address is this
    /// contract's own address.
    async fn default_ism(&self) -> ChainResult<H256> {
        Ok(self.address)
    }

    /// Same monolithic contract — every recipient routes through the same
    /// embedded MessageIdMultisigIsm.
    async fn recipient_ism(&self, _recipient: H256) -> ChainResult<H256> {
        Ok(self.address)
    }

    async fn process(
        &self,
        message: &HyperlaneMessage,
        metadata: &Metadata,
        _tx_gas_limit: Option<U256>,
    ) -> ChainResult<TxOutcome> {
        let parsed = parse_metadata(metadata.as_ref())?;
        let request = toolkit::build_request(
            self.address,
            &self.toolkit_ctx,
            message,
            parsed,
            // The destination-side WarpRoute is user-facing. Contract
            // recipients are out of scope until the warp route grows a
            // routing layer; default to false so `sendUnshielded` lands as
            // `UserAddress`.
            false,
        );

        let outcome = toolkit::submit_handle(&self.toolkit_ctx, &request).await?;

        // The submitter only returns success when the on-chain replay set
        // was updated for this message. Gas accounting is not exposed by
        // the Midnight contract today; surface a non-zero placeholder so
        // the relayer's queue metrics aren't divided by zero.
        Ok(TxOutcome {
            transaction_id: outcome.transaction_id,
            executed: outcome.executed,
            gas_used: U256::from(1_u32),
            gas_price: FixedPointNumber::from_str("1")
                .map_err(|err| ChainCommunicationError::from_other_str(&err.to_string()))?,
        })
    }

    async fn process_estimate_costs(
        &self,
        _message: &HyperlaneMessage,
        _metadata: &Metadata,
    ) -> ChainResult<TxCostEstimate> {
        // Midnight fees are denominated in DUST and computed by the wallet
        // at submission time. The relayer just needs a non-zero placeholder
        // here; the submitter handles the real fee math. Same minimal
        // pattern Aleo uses.
        Ok(TxCostEstimate {
            gas_limit: U256::from(1_000_000_u32),
            gas_price: FixedPointNumber::from_str("1")
                .map_err(|err| ChainCommunicationError::from_other_str(&err.to_string()))?,
            l2_gas_limit: None,
        })
    }

    async fn process_calldata(
        &self,
        _message: &HyperlaneMessage,
        _metadata: &Metadata,
    ) -> ChainResult<Vec<u8>> {
        // Only used by the Lander submitter path. Midnight uses the Classic
        // path (subprocess), so there's no on-chain calldata to surface
        // here.
        Err(HyperlaneMidnightError::NotImplemented("process_calldata (Lander path)").into())
    }

    fn delivered_calldata(&self, _message_id: H256) -> ChainResult<Option<Vec<u8>>> {
        // Same rationale as `process_calldata` — Lander-only.
        Ok(None)
    }
}

/// Parse a MessageIdMultisigIsmMetadata blob into the fields the Midnight
/// `handle` circuit expects.
///
/// Layout (matches upstream `relayer/src/msg/metadata/multisig/base.rs`):
///
/// ```text
/// [0..32]   merkle tree hook address
/// [32..64]  root
/// [64..68]  index (u32 big-endian)
/// [68..]    signatures (65 bytes each, up to 16)
/// ```
///
/// Signatures shorter than `MAX_SIGNATURES` slots are padded with zeroed
/// 65-byte entries — the on-chain ISM ignores anything past the threshold
/// and accepts dummy filler in the unused tail positions.
fn parse_metadata(bytes: &[u8]) -> ChainResult<WireMetadata> {
    if bytes.len() < METADATA_HEADER_LEN {
        return Err(HyperlaneMidnightError::Other(format!(
            "metadata too short: {} bytes (need at least {METADATA_HEADER_LEN})",
            bytes.len()
        ))
        .into());
    }

    let sigs_blob = &bytes[METADATA_HEADER_LEN..];
    if sigs_blob.len() % SIGNATURE_LEN != 0 {
        return Err(HyperlaneMidnightError::Other(format!(
            "metadata signature region {} bytes is not a multiple of {SIGNATURE_LEN}",
            sigs_blob.len()
        ))
        .into());
    }

    let sig_count = sigs_blob.len() / SIGNATURE_LEN;
    if sig_count > MAX_SIGNATURES {
        return Err(HyperlaneMidnightError::Other(format!(
            "metadata carries {sig_count} signatures, exceeds on-chain bound of {MAX_SIGNATURES}"
        ))
        .into());
    }

    let merkle_tree_hook = format!("0x{}", hex::encode(&bytes[0..32]));
    let root = format!("0x{}", hex::encode(&bytes[32..64]));
    let mut index_be = [0u8; 4];
    index_be.copy_from_slice(&bytes[64..68]);
    let index = u32::from_be_bytes(index_be);

    let mut signatures: Vec<String> = (0..sig_count)
        .map(|i| {
            let start = i * SIGNATURE_LEN;
            format!(
                "0x{}",
                hex::encode(&sigs_blob[start..start + SIGNATURE_LEN])
            )
        })
        .collect();

    // Pad to MAX_SIGNATURES so the on-chain `Vector<16, Bytes<65>>` always
    // gets a full payload. The contract ignores entries past the threshold.
    let padding = format!("0x{}", hex::encode([0u8; SIGNATURE_LEN]));
    while signatures.len() < MAX_SIGNATURES {
        signatures.push(padding.clone());
    }

    Ok(WireMetadata {
        merkle_tree_hook,
        root,
        index,
        signatures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_metadata_extracts_fields() {
        let mut bytes = vec![0u8; METADATA_HEADER_LEN];
        // merkle_tree_hook: 0xAB...AB
        for b in &mut bytes[0..32] {
            *b = 0xAB;
        }
        // root: 0xCD...CD
        for b in &mut bytes[32..64] {
            *b = 0xCD;
        }
        // index: 0x0000_002A (42 big-endian)
        bytes[64..68].copy_from_slice(&42u32.to_be_bytes());
        // 2 signatures, each filled with a marker byte.
        for marker in [0x11u8, 0x22u8] {
            bytes.extend(std::iter::repeat_n(marker, SIGNATURE_LEN));
        }

        let parsed = parse_metadata(&bytes).expect("parse should succeed");
        assert_eq!(
            parsed.merkle_tree_hook,
            format!("0x{}", hex::encode([0xABu8; 32]))
        );
        assert_eq!(parsed.root, format!("0x{}", hex::encode([0xCDu8; 32])));
        assert_eq!(parsed.index, 42);
        assert_eq!(parsed.signatures.len(), MAX_SIGNATURES);
        assert_eq!(
            parsed.signatures[0],
            format!("0x{}", hex::encode([0x11u8; SIGNATURE_LEN]))
        );
        assert_eq!(
            parsed.signatures[1],
            format!("0x{}", hex::encode([0x22u8; SIGNATURE_LEN]))
        );
        // Slot 2 onwards is zero-padding.
        assert_eq!(
            parsed.signatures[2],
            format!("0x{}", hex::encode([0u8; SIGNATURE_LEN]))
        );
    }

    #[test]
    fn parse_metadata_rejects_short_input() {
        let bytes = vec![0u8; METADATA_HEADER_LEN - 1];
        assert!(parse_metadata(&bytes).is_err());
    }

    #[test]
    fn parse_metadata_rejects_unaligned_signatures() {
        let mut bytes = vec![0u8; METADATA_HEADER_LEN];
        bytes.extend(std::iter::repeat_n(0u8, SIGNATURE_LEN - 1));
        assert!(parse_metadata(&bytes).is_err());
    }

    #[test]
    fn parse_metadata_rejects_too_many_signatures() {
        let mut bytes = vec![0u8; METADATA_HEADER_LEN];
        bytes.extend(std::iter::repeat_n(0u8, (MAX_SIGNATURES + 1) * SIGNATURE_LEN));
        assert!(parse_metadata(&bytes).is_err());
    }

    #[test]
    fn derive_ws_url_appends_ws_path_and_swaps_scheme() {
        let http = url::Url::parse("http://indexer.local/api/v3/graphql").unwrap();
        assert_eq!(
            derive_ws_url(&http),
            "ws://indexer.local/api/v3/graphql/ws"
        );

        let https = url::Url::parse("https://indexer.example/graphql").unwrap();
        assert_eq!(derive_ws_url(&https), "wss://indexer.example/graphql/ws");
    }
}

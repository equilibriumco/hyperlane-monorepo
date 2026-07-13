use std::str::FromStr;

use async_trait::async_trait;

use hyperlane_core::{
    ChainCommunicationError, ChainResult, ContractLocator, FixedPointNumber, HyperlaneChain,
    HyperlaneContract, HyperlaneDomain, HyperlaneMessage, HyperlaneProvider, Mailbox, Metadata,
    ReorgPeriod, TxCostEstimate, TxOutcome, H160, H256, U256,
};

use crate::toolkit::{self, ToolkitContext, WireMetadata};
use crate::{ConnectionConf, HyperlaneMidnightError, MidnightIndexerClient, MidnightProvider};

// Upper bound on how many validator signatures a metadata blob may carry,
// matching the on-chain `MessageIdMultisigIsm.MAX_VALIDATORS` (#22 reduced this
// from 16 to 4 to keep the handle proof tractable). The relayer forwards the
// real, quorum-sized signature set (typically `threshold` entries) verbatim;
// the Midnight submitter (`relayer/src/checkpoint-digest.ts`) is what pads the
// on-chain `Vector<4, ...>` by repeating slot 0, so DO NOT pad here — a
// zero-signature pad would recover to a garbage pubkey and be rejected.
const MAX_SIGNATURES: usize = 4;
const SIGNATURE_LEN: usize = 65;
const METADATA_HEADER_LEN: usize = 32 + 32 + 4;

const DEFAULT_PROOF_SERVER_URL: &str = "http://127.0.0.1:6300";
const DEFAULT_NETWORK_ID: &str = "undeployed";

/// Destination-side Mailbox.
#[derive(Debug, Clone)]
pub struct MidnightMailbox {
    address: H256,
    domain: HyperlaneDomain,
    provider: MidnightProvider,
    toolkit_ctx: ToolkitContext,
    /// Test-only override for the on-chain validator order. Production reads
    /// the set from chain state in [`Self::validator_order`]; tests set this
    /// (to an empty vec) so `process` can run offline without an indexer.
    #[cfg(test)]
    validator_override: Option<Vec<H160>>,
}

impl MidnightMailbox {
    /// Build a new Mailbox.
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
            #[cfg(test)]
            validator_override: None,
        })
    }

    /// Read the on-chain validator set (in slot order) from the deployed
    /// contract, used to sort signatures by validator index before submitting.
    async fn validator_order(&self) -> ChainResult<Vec<H160>> {
        #[cfg(test)]
        if let Some(validators) = &self.validator_override {
            return Ok(validators.clone());
        }
        let address = format!("{:x}", self.address);
        let validators = self
            .provider
            .indexer()
            .read_ism_state(&address)
            .await?
            .validators
            .into_iter()
            .map(H160::from)
            .collect();
        Ok(validators)
    }

    /// Test-only: pin the validator order so `process` skips the chain read.
    #[cfg(test)]
    pub(crate) fn with_validator_override(mut self, validators: Vec<H160>) -> Self {
        self.validator_override = Some(validators);
        self
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
    async fn count(&self, _reorg_period: &ReorgPeriod) -> ChainResult<u32> {
        Ok(0)
    }

    async fn delivered(&self, id: H256) -> ChainResult<bool> {
        toolkit::query_delivered(&self.toolkit_ctx, self.address, id).await
    }

    async fn default_ism(&self) -> ChainResult<H256> {
        Ok(self.address)
    }

    async fn recipient_ism(&self, _recipient: H256) -> ChainResult<H256> {
        Ok(self.address)
    }

    async fn process(
        &self,
        message: &HyperlaneMessage,
        metadata: &Metadata,
        _tx_gas_limit: Option<U256>,
    ) -> ChainResult<TxOutcome> {
        let mut parsed = parse_metadata(metadata.as_ref())?;

        // The on-chain MessageIdMultisigIsm requires signatures in
        // validator-set-index order (a two-pointer match). The relayer's
        // metadata builder emits them in checkpoint-syncer fetch order, which
        // only incidentally matches validator-index order within a single
        // batch and is not guaranteed (only the Aleo destination gets an
        // explicit ordering pass in `hyperlane-base/src/types/multisig.rs`).
        // So read the on-chain validator set and sort here. An empty set means
        // nothing to sort against (also the offline cross-boundary test path).
        let validators = self.validator_order().await?;
        if !validators.is_empty() {
            sort_signatures_by_validator_index(message, &mut parsed, &validators)?;
        }

        let request =
            toolkit::build_request(self.address, &self.toolkit_ctx, message, parsed, false);

        let outcome = toolkit::submit_handle(&self.toolkit_ctx, &request).await?;

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
        message: &HyperlaneMessage,
        metadata: &Metadata,
    ) -> ChainResult<TxCostEstimate> {
        // Dry-run `handle` against current chain state (no proving, no
        // submission). A message that would revert (unenrolled sender, amount
        // over escrow, paused, replay, failed ISM, ...) returns `Err` here, so
        // the relayer catches it at prepare time and applies the standard
        // exponential backoff — rather than the revert only surfacing at submit
        // time on the no-backoff `ErrorSubmitting` path, where it would
        // busy-loop and starve every other inbound delivery (issue #80). This
        // mirrors every other Hyperlane chain, whose `process_estimate_costs`
        // simulates the call.
        //
        // Prepare the metadata exactly as `process` does (parse + sort by
        // validator index + pad) so the dry-run executes what a real
        // submission would — in particular the on-chain ISM's forward-only
        // two-pointer walk requires signatures in validator-set order, so an
        // unsorted vector would spuriously "revert".
        let mut parsed = parse_metadata(metadata.as_ref())?;
        let validators = self.validator_order().await?;
        if !validators.is_empty() {
            sort_signatures_by_validator_index(message, &mut parsed, &validators)?;
        }
        let request = toolkit::build_dry_run_request(
            self.address,
            &self.toolkit_ctx,
            message,
            parsed,
            false,
        );
        toolkit::dry_run_handle(&self.toolkit_ctx, &request).await?;

        // The message would be accepted on-chain. Midnight fees are denominated
        // in DUST and computed by the wallet at submission time, so the cost
        // fields stay a fixed placeholder — unchanged from the previous stub,
        // which keeps the gas-payment-enforcement policies that read these
        // fields behaving exactly as before.
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
        Err(HyperlaneMidnightError::NotImplemented("process_calldata (Lander path)").into())
    }

    fn delivered_calldata(&self, _message_id: H256) -> ChainResult<Option<Vec<u8>>> {
        Ok(None)
    }
}

// Layout: merkle_tree_hook (32) || root (32) || index (u32 BE, 4) || sigs (65 each, up to 4).
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

    // No padding: forward exactly the real signatures the metadata carried.
    // The Midnight submitter pads the on-chain `Vector<4>` by repeating slot 0.
    Ok(WireMetadata {
        merkle_tree_hook,
        root,
        index,
        signatures,
    })
}

/// Reorder the real signatures in `metadata` by their signer's index in
/// `validators`. No padding is added — the Midnight submitter pads the
/// on-chain `Vector<4>` by repeating slot 0.
/// Errors if any signature recovers to an address not present in
/// `validators` — that indicates either a malformed validator config or a
/// genuinely invalid signature; either way the on-chain ISM would reject
/// the call so failing fast here surfaces the issue with a clearer error
/// than a Compact `assert` revert.
fn sort_signatures_by_validator_index(
    message: &HyperlaneMessage,
    metadata: &mut WireMetadata,
    validators: &[H160],
) -> ChainResult<()> {
    let zero_sig_hex = format!("0x{}", hex::encode([0u8; SIGNATURE_LEN]));
    let mut reals: Vec<String> = metadata
        .signatures
        .iter()
        .filter(|s| **s != zero_sig_hex)
        .cloned()
        .collect();
    if reals.is_empty() {
        return Ok(());
    }

    let inner_hash = compute_checkpoint_inner_hash(message, metadata)?;

    let mut indexed: Vec<(usize, String)> = Vec::with_capacity(reals.len());
    for sig_hex in reals.drain(..) {
        let bytes = hex::decode(sig_hex.trim_start_matches("0x")).map_err(|e| {
            HyperlaneMidnightError::Other(format!("signature hex decode: {e}"))
        })?;
        if bytes.len() != SIGNATURE_LEN {
            return Err(HyperlaneMidnightError::Other(format!(
                "signature must be {SIGNATURE_LEN} bytes, got {}",
                bytes.len()
            ))
            .into());
        }
        let signer = recover_signer(&inner_hash, &bytes)?;
        let idx = validators.iter().position(|v| *v == signer).ok_or_else(|| {
            HyperlaneMidnightError::Other(format!(
                "signer {signer:?} not in configured validator set"
            ))
        })?;
        indexed.push((idx, sig_hex));
    }
    indexed.sort_by_key(|(i, _)| *i);

    metadata.signatures = indexed.into_iter().map(|(_, s)| s).collect();
    Ok(())
}

/// keccak(domainHash || root || index_be || messageId), where
/// domainHash = keccak(origin_domain_be32 || merkle_tree_hook || "HYPERLANE").
/// Matches the on-chain Midnight MessageIdMultisigIsm digest pre-EIP-191
/// (validators sign the EIP-191 wrap of this; ecrecover with
/// RecoveryMessage::Data applies the same wrap).
fn compute_checkpoint_inner_hash(
    message: &HyperlaneMessage,
    metadata: &WireMetadata,
) -> ChainResult<[u8; 32]> {
    let merkle_tree_hook = decode_hex_32(&metadata.merkle_tree_hook)?;
    let root = decode_hex_32(&metadata.root)?;
    let message_id_h256: H256 = message.id();
    let message_id: [u8; 32] = message_id_h256.into();

    let mut buf = Vec::with_capacity(4 + 32 + 9);
    buf.extend_from_slice(&message.origin.to_be_bytes());
    buf.extend_from_slice(&merkle_tree_hook);
    buf.extend_from_slice(b"HYPERLANE");
    let domain_hash = ethers::utils::keccak256(&buf);

    let mut buf = Vec::with_capacity(32 + 32 + 4 + 32);
    buf.extend_from_slice(&domain_hash);
    buf.extend_from_slice(&root);
    buf.extend_from_slice(&metadata.index.to_be_bytes());
    buf.extend_from_slice(&message_id);
    Ok(ethers::utils::keccak256(&buf))
}

fn decode_hex_32(s: &str) -> ChainResult<[u8; 32]> {
    let bytes = hex::decode(s.trim_start_matches("0x"))
        .map_err(|e| HyperlaneMidnightError::Other(format!("hex decode: {e}")))?;
    if bytes.len() != 32 {
        return Err(HyperlaneMidnightError::Other(format!(
            "expected 32 bytes, got {}",
            bytes.len()
        ))
        .into());
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn recover_signer(inner_hash: &[u8; 32], sig_bytes: &[u8]) -> ChainResult<H160> {
    let sig = ethers::types::Signature::try_from(sig_bytes)
        .map_err(|e| HyperlaneMidnightError::Other(format!("malformed signature: {e}")))?;
    let recovered = sig
        .recover(inner_hash.to_vec())
        .map_err(|e| HyperlaneMidnightError::Other(format!("ecrecover failed: {e}")))?;
    // ethers::types::H160 and hyperlane_core::H160 are both 20-byte primitive
    // hashes but distinct types; convert through the inner byte array.
    Ok(H160::from(recovered.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_metadata_extracts_fields() {
        let mut bytes = vec![0u8; METADATA_HEADER_LEN];
        for b in &mut bytes[0..32] {
            *b = 0xAB;
        }
        for b in &mut bytes[32..64] {
            *b = 0xCD;
        }
        bytes[64..68].copy_from_slice(&42u32.to_be_bytes());
        for marker in [0x11u8, 0x22u8] {
            bytes.extend(std::iter::repeat_n(marker, SIGNATURE_LEN));
        }

        let parsed = parse_metadata(&bytes).unwrap();
        assert_eq!(parsed.merkle_tree_hook, format!("0x{}", hex::encode([0xABu8; 32])));
        assert_eq!(parsed.root, format!("0x{}", hex::encode([0xCDu8; 32])));
        assert_eq!(parsed.index, 42);
        // No padding: exactly the two real signatures the blob carried.
        assert_eq!(parsed.signatures.len(), 2);
        assert_eq!(parsed.signatures[0], format!("0x{}", hex::encode([0x11u8; SIGNATURE_LEN])));
        assert_eq!(parsed.signatures[1], format!("0x{}", hex::encode([0x22u8; SIGNATURE_LEN])));
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

    #[tokio::test]
    async fn sort_signatures_reorders_by_validator_index_and_drops_padding() {
        use ethers::signers::{LocalWallet, Signer};

        let k0: LocalWallet =
            "1111111111111111111111111111111111111111111111111111111111111111"
                .parse()
                .unwrap();
        let k1: LocalWallet =
            "2222222222222222222222222222222222222222222222222222222222222222"
                .parse()
                .unwrap();
        let v0 = H160::from(k0.address().0);
        let v1 = H160::from(k1.address().0);
        let validators = vec![v0, v1];

        let message = HyperlaneMessage {
            version: 3,
            nonce: 0,
            origin: 31337,
            sender: H256::repeat_byte(0x33),
            destination: 1234,
            recipient: H256::repeat_byte(0x44),
            body: vec![0u8; 64],
        };
        let mut metadata = WireMetadata {
            merkle_tree_hook: format!("0x{}", hex::encode([0xABu8; 32])),
            root: format!("0x{}", hex::encode([0xCDu8; 32])),
            index: 7,
            signatures: vec![],
        };

        let inner = compute_checkpoint_inner_hash(&message, &metadata).unwrap();
        let sig0 = k0.sign_message(&inner[..]).await.unwrap();
        let sig1 = k1.sign_message(&inner[..]).await.unwrap();
        let sig0_hex = format!("0x{}", hex::encode(<[u8; 65]>::from(sig0)));
        let sig1_hex = format!("0x{}", hex::encode(<[u8; 65]>::from(sig1)));
        let zero_hex = format!("0x{}", hex::encode([0u8; SIGNATURE_LEN]));

        // Insert signatures in REVERSE validator-index order, plus a stale zero
        // pad that the sort must drop (it never re-pads).
        metadata.signatures = vec![sig1_hex.clone(), sig0_hex.clone(), zero_hex.clone()];

        sort_signatures_by_validator_index(&message, &mut metadata, &validators).unwrap();

        // Reordered to validator-index order, zero padding dropped, no re-pad.
        assert_eq!(metadata.signatures.len(), 2);
        assert_eq!(metadata.signatures[0], sig0_hex);
        assert_eq!(metadata.signatures[1], sig1_hex);
    }

    #[tokio::test]
    async fn sort_signatures_errors_on_unknown_signer() {
        use ethers::signers::{LocalWallet, Signer};

        let k_known: LocalWallet =
            "1111111111111111111111111111111111111111111111111111111111111111"
                .parse()
                .unwrap();
        let k_rogue: LocalWallet =
            "9999999999999999999999999999999999999999999999999999999999999999"
                .parse()
                .unwrap();
        // Validator set knows only k_known; k_rogue must be rejected.
        let validators = vec![H160::from(k_known.address().0)];

        let message = HyperlaneMessage {
            version: 3,
            nonce: 0,
            origin: 31337,
            sender: H256::repeat_byte(0x33),
            destination: 1234,
            recipient: H256::repeat_byte(0x44),
            body: vec![0u8; 64],
        };
        let mut metadata = WireMetadata {
            merkle_tree_hook: format!("0x{}", hex::encode([0xABu8; 32])),
            root: format!("0x{}", hex::encode([0xCDu8; 32])),
            index: 7,
            signatures: vec![],
        };
        let inner = compute_checkpoint_inner_hash(&message, &metadata).unwrap();
        let sig_rogue = k_rogue.sign_message(&inner[..]).await.unwrap();
        let zero_hex = format!("0x{}", hex::encode([0u8; SIGNATURE_LEN]));
        metadata.signatures = vec![format!("0x{}", hex::encode(<[u8; 65]>::from(sig_rogue)))];
        while metadata.signatures.len() < MAX_SIGNATURES {
            metadata.signatures.push(zero_hex.clone());
        }

        assert!(sort_signatures_by_validator_index(&message, &mut metadata, &validators).is_err());
    }
}

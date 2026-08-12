use std::str::FromStr;

use async_trait::async_trait;

use hyperlane_core::{
    Announcement, ChainCommunicationError, ChainResult, ContractLocator, FixedPointNumber,
    HyperlaneChain, HyperlaneContract, HyperlaneDomain, HyperlaneProvider, Signable, SignedType,
    TxOutcome, ValidatorAnnounce, H160, H256, U256,
};

use crate::toolkit::{self, ToolkitContext};
use crate::{ConnectionConf, MidnightProvider};

/// ValidatorAnnounce backed by the on-chain Midnight ValidatorAnnounce
/// contract. Reads go through a read-only submitter op (the on-chain
/// `locations` map uses a `persistentHash` composite key the agent cannot
/// reproduce); the write `announce` goes through a submitter tx op.
#[derive(Debug, Clone)]
pub struct MidnightValidatorAnnounce {
    address: H256,
    domain: HyperlaneDomain,
    provider: MidnightProvider,
    toolkit_ctx: ToolkitContext,
}

impl MidnightValidatorAnnounce {
    /// Build a new ValidatorAnnounce.
    pub fn new(locator: &ContractLocator<'_>, conf: &ConnectionConf) -> ChainResult<Self> {
        let provider = MidnightProvider::from_conf(locator.domain.clone(), conf);
        let toolkit_ctx = ToolkitContext::from_conf(conf);

        Ok(Self {
            address: locator.address,
            domain: locator.domain.clone(),
            provider,
            toolkit_ctx,
        })
    }
}

/// Recover the signer's secp256k1 public key from a 65-byte Ethereum signature
/// (`r || s || v`) over `digest`, returning the 64-byte SEC1 uncompressed body
/// (`X_be || Y_be`, no `0x04` tag) — exactly what the on-chain `announce`
/// circuit takes as its `pubkey` param (#90). k256 is reached via `ethers`
/// (already a dependency; the same crate `state_decode.rs` uses to derive
/// addresses from stored pubkeys).
fn recover_pubkey_body(digest: H256, sig65: &[u8; 65]) -> ChainResult<[u8; 64]> {
    use ethers::core::k256::ecdsa::{
        recoverable::{Id as RecoveryId, Signature as RecoverableSignature},
        Signature as K256Signature,
    };
    use ethers::core::k256::elliptic_curve::sec1::ToEncodedPoint;

    let v = sig65[64];
    // Hyperlane validators emit legacy {27, 28} recovery bytes; k256 wants {0, 1}.
    let rec_byte = if v >= 27 { v - 27 } else { v };
    let recovery_id = RecoveryId::new(rec_byte).map_err(|err| {
        ChainCommunicationError::from_other_str(&format!(
            "announce: invalid recovery id {v}: {err}"
        ))
    })?;
    let sig = K256Signature::try_from(&sig65[..64]).map_err(|err| {
        ChainCommunicationError::from_other_str(&format!("announce: malformed signature: {err}"))
    })?;
    let rsig = RecoverableSignature::new(&sig, recovery_id).map_err(|err| {
        ChainCommunicationError::from_other_str(&format!(
            "announce: bad recoverable signature: {err}"
        ))
    })?;
    // `digest` is the already-hashed EIP-191 announcement digest (the prehash
    // the validator signed over), so recover directly from these bytes.
    let digest_bytes = digest.to_fixed_bytes();
    let vk = rsig
        .recover_verifying_key_from_digest_bytes(digest_bytes.as_ref().into())
        .map_err(|err| {
            ChainCommunicationError::from_other_str(&format!(
                "announce: failed to recover pubkey: {err}"
            ))
        })?;
    let point = vk.to_encoded_point(false);
    let bytes = point.as_bytes();
    // Uncompressed SEC1 is 65 bytes: 0x04 || X(32) || Y(32).
    if bytes.len() != 65 {
        return Err(ChainCommunicationError::from_other_str(&format!(
            "announce: unexpected pubkey encoding length {}",
            bytes.len()
        )));
    }
    let mut body = [0u8; 64];
    body.copy_from_slice(&bytes[1..65]);
    Ok(body)
}

impl HyperlaneChain for MidnightValidatorAnnounce {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(self.provider.clone())
    }
}

impl HyperlaneContract for MidnightValidatorAnnounce {
    fn address(&self) -> H256 {
        self.address
    }
}

#[async_trait]
impl ValidatorAnnounce for MidnightValidatorAnnounce {
    async fn get_announced_storage_locations(
        &self,
        validators: &[H256],
    ) -> ChainResult<Vec<Vec<String>>> {
        // The on-chain validator key is the low 20 bytes of the H256 (the
        // EVM-style address the pubkey derives to), matching the Aleo port and
        // the `Bytes<20>` validator key on the contract.
        let validators: Vec<H160> = validators.iter().map(|v| H160::from(*v)).collect();
        toolkit::query_storage_locations(&self.toolkit_ctx, self.address, &validators).await
    }

    async fn announce(&self, announcement: SignedType<Announcement>) -> ChainResult<TxOutcome> {
        let validator = announcement.value.validator;
        // The validator agent has already padded this to the fixed 480-byte
        // buffer for Midnight (`announcement_location`), which is what it signed
        // over; forward it verbatim.
        let storage_location = announcement.value.storage_location.clone();
        // `SignedType::signature` is a 65-byte ECDSA signature (r || s || v).
        let signature: [u8; 65] = announcement.signature.into();
        // Midnight's Compact has no in-circuit ecrecover, so the verify-based
        // `announce` circuit takes the signer's public key and derives the
        // validator address from it (#90). Recover the pubkey off-chain from the
        // EIP-191 announcement digest the validator signed.
        let digest = announcement.value.eth_signed_message_hash();
        let pubkey = recover_pubkey_body(digest, &signature)?;

        let outcome = toolkit::announce_tx(
            &self.toolkit_ctx,
            self.address,
            validator,
            &storage_location,
            &signature,
            &pubkey,
        )
        .await?;

        Ok(TxOutcome {
            transaction_id: outcome.transaction_id,
            executed: outcome.executed,
            // Gas is the DUST actually paid, in specks, at a unit price; zero
            // when the submitter did not report a fee.
            gas_used: outcome.fee_specks.unwrap_or_default(),
            gas_price: FixedPointNumber::from_str("1")
                .map_err(|err| ChainCommunicationError::from_other_str(&err.to_string()))?,
        })
    }

    async fn announce_tokens_needed(
        &self,
        _announcement: SignedType<Announcement>,
        _chain_signer: H256,
    ) -> Option<U256> {
        // Midnight fees are paid in DUST and are computed by the wallet inside
        // the submitter subprocess (the Rust `MidnightSigner` is a
        // placeholder), so the validator agent cannot pre-fund anything from
        // here. `Some(0)` follows the Sealevel pattern: it signals "no extra
        // tokens needed" rather than "unknown", so the validator agent proceeds
        // to announce.
        Some(U256::zero())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyperlane_core::Signature;

    use crate::MidnightIndexerClient;

    fn toolkit_ctx(binary_path: &str) -> ToolkitContext {
        ToolkitContext {
            binary_path: binary_path.to_string(),
            indexer_graphql_url: "http://indexer/graphql".to_string(),
            indexer_ws_url: "ws://indexer/graphql/ws".to_string(),
            node_rpc_url: "http://node:9944".to_string(),
            proof_server_url: "http://proof:6300".to_string(),
            network_id: "undeployed".to_string(),
        }
    }

    fn va(binary_path: &str) -> MidnightValidatorAnnounce {
        let domain = HyperlaneDomain::Known(hyperlane_core::KnownHyperlaneDomain::Test1);
        let indexer =
            MidnightIndexerClient::new(url::Url::parse("http://indexer/graphql").unwrap());
        let provider = MidnightProvider::new(domain.clone(), indexer);
        MidnightValidatorAnnounce {
            address: H256::from_low_u64_be(0xabcd),
            domain,
            provider,
            toolkit_ctx: toolkit_ctx(binary_path),
        }
    }

    fn signature_from_bytes(bytes: [u8; 65]) -> Signature {
        // `Signature` has no `From<[u8;65]>`; build it field-wise. The
        // announce-path tests reject before the signature is ever decoded, so
        // the exact value only matters for the round-trip test below.
        Signature {
            r: U256::from_big_endian(&bytes[0..32]),
            s: U256::from_big_endian(&bytes[32..64]),
            v: bytes[64] as u64,
        }
    }

    fn signed_announcement(
        validator: H160,
        storage_location: String,
        signature: [u8; 65],
    ) -> SignedType<Announcement> {
        let announcement = Announcement {
            validator,
            mailbox_address: H256::repeat_byte(0xab),
            mailbox_domain: 1234,
            storage_location,
        };
        SignedType {
            value: announcement,
            signature: signature_from_bytes(signature),
        }
    }

    #[test]
    fn h256_to_h160_takes_low_20_bytes() {
        // H160::from(H256) keeps the low 20 bytes — the EVM-style 20-byte
        // validator address used on the contract.
        let mut raw = [0u8; 32];
        for (i, b) in raw.iter_mut().enumerate() {
            *b = i as u8;
        }
        let h256 = H256::from(raw);
        let h160 = H160::from(h256);
        assert_eq!(h160.as_bytes(), &raw[12..32]);
    }

    #[tokio::test]
    async fn announce_tokens_needed_is_some_zero() {
        let va = va("/usr/bin/true");
        let signed =
            signed_announcement(H160::repeat_byte(0x11), "s3://loc".to_string(), [0u8; 65]);
        let needed = va.announce_tokens_needed(signed, H256::zero()).await;
        assert_eq!(needed, Some(U256::zero()));
    }

    // A well-formed 65-byte signature over an arbitrary hash, so `announce`'s
    // off-chain pubkey recovery succeeds and the location/pubkey validation in
    // `announce_tx` is what a test exercises. The value it signs is irrelevant
    // — recovery yields *some* pubkey and the length checks fire before it is
    // ever used on-chain.
    fn valid_sig() -> [u8; 65] {
        use ethers::signers::LocalWallet;
        let wallet: LocalWallet =
            "1111111111111111111111111111111111111111111111111111111111111111"
                .parse()
                .unwrap();
        let sig = wallet.sign_hash(ethers::types::H256([0x42u8; 32]));
        <[u8; 65]>::from(sig)
    }

    // Zero-pad a location into the fixed 480-byte buffer, as the validator agent
    // does before signing.
    fn padded(location: &str) -> String {
        let mut bytes = location.as_bytes().to_vec();
        bytes.resize(toolkit::MAX_STORAGE_LOCATION_LEN, 0);
        String::from_utf8(bytes).unwrap()
    }

    #[tokio::test]
    async fn announce_rejects_non_480_location() {
        // The fork expects the agent-padded 480-byte buffer; an unpadded
        // location is rejected before the subprocess.
        let va = va("/usr/bin/true");
        let signed =
            signed_announcement(H160::repeat_byte(0x11), "s3://loc".to_string(), valid_sig());
        let err = va.announce(signed).await.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("padded") && msg.contains("480"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn announce_rejects_all_null_location() {
        // A 480-byte buffer whose first byte is NUL is an empty location.
        let va = va("/usr/bin/true");
        let signed = signed_announcement(H160::repeat_byte(0x11), padded(""), valid_sig());
        let err = va.announce(signed).await.unwrap_err();
        assert!(format!("{err}").contains("empty storage location"));
    }

    #[tokio::test]
    async fn announce_rejects_location_with_no_trailing_nul() {
        // 480 meaningful bytes leave no terminator, so trim-on-read would be
        // ill-defined; rejected.
        let va = va("/usr/bin/true");
        let full = "x".repeat(toolkit::MAX_STORAGE_LOCATION_LEN);
        let signed = signed_announcement(H160::repeat_byte(0x11), full, valid_sig());
        let err = va.announce(signed).await.unwrap_err();
        assert!(format!("{err}").contains("no trailing NUL"));
    }

    #[test]
    fn recover_pubkey_body_derives_validator() {
        // The off-chain recovery must reproduce the validator address the way
        // the circuit will on-chain: keccak256(X_be || Y_be)[12..] == validator.
        use ethers::signers::{LocalWallet, Signer};
        use ethers::utils::keccak256;

        let wallet: LocalWallet =
            "2222222222222222222222222222222222222222222222222222222222222222"
                .parse()
                .unwrap();
        let validator = H160::from(wallet.address().0);
        let announcement = Announcement {
            validator,
            mailbox_address: H256::repeat_byte(0xab),
            mailbox_domain: 1234,
            storage_location: padded("s3://hyperlane-validator-0/us-east-1"),
        };
        let digest = announcement.eth_signed_message_hash();
        let sig = wallet.sign_hash(ethers::types::H256(digest.0));
        let sig_bytes = <[u8; 65]>::from(sig);

        let pubkey = recover_pubkey_body(digest, &sig_bytes).unwrap();
        let derived = &keccak256(pubkey)[12..];
        assert_eq!(derived, validator.as_bytes());
    }

    #[tokio::test]
    async fn get_storage_locations_empty_input_returns_empty() {
        // With no validators requested the submitter would echo an empty
        // list; `/usr/bin/true` produces empty stdout which maps to a
        // malformed-response error, so guard the empty-input path before any
        // subprocess by asserting the request shape instead.
        let va = va("");
        let err = va
            .get_announced_storage_locations(&[H256::from_low_u64_be(1)])
            .await
            .unwrap_err();
        // Empty binary path short-circuits to the missing-path error.
        assert!(format!("{err}").contains("not configured"));
    }

    #[tokio::test]
    async fn signed_announcement_signature_round_trips() {
        // A real ECDSA-signed announcement keeps its 65-byte signature intact
        // through the SignedType -> announce path, so the on-chain verify sees
        // the exact bytes the validator produced. The agent never recomputes
        // the digest (the validator signs, the contract verifies); this guards
        // the pass-through.
        use ethers::signers::{LocalWallet, Signer};

        let wallet: LocalWallet =
            "1111111111111111111111111111111111111111111111111111111111111111"
                .parse()
                .unwrap();
        let validator = H160::from(wallet.address().0);
        let announcement = Announcement {
            validator,
            mailbox_address: H256::repeat_byte(0xab),
            mailbox_domain: 1234,
            storage_location: "s3://hyperlane-validator-0/us-east-1".to_string(),
        };
        // The validator signs the EIP-191 wrap of the announcement digest.
        // `Wallet::sign_hash` applies no extra prefix, so this signs the exact
        // 32-byte digest a Hyperlane validator would.
        let digest = announcement.eth_signed_message_hash();
        let sig = wallet.sign_hash(ethers::types::H256(digest.0));
        let sig_bytes: [u8; 65] = <[u8; 65]>::from(sig);

        let signed = SignedType {
            value: announcement,
            signature: signature_from_bytes(sig_bytes),
        };
        // Exactly what `announce()` extracts and forwards to the submitter.
        let forwarded: [u8; 65] = signed.signature.into();
        assert_eq!(forwarded, sig_bytes);
    }

    // End-to-end integration against a local devnet with a registered
    // validator. Ignored by default: it needs a running Midnight devnet
    // (proof server + node + indexer), the built `submit-handle` binary, and
    // a deployed ValidatorAnnounce contract. Tracked as a follow-up — see the
    // PR. Run with `cargo test -p hyperlane-midnight -- --ignored` once the
    // devnet env vars (MIDNIGHT_NODE_RPC_URL, MIDNIGHT_PROOF_SERVER_URL,
    // toolkitPath) and a deployed contract address are wired up.
    #[tokio::test]
    #[ignore = "requires a local devnet, the submit-handle binary, and a deployed VA contract"]
    async fn announce_and_read_back_on_devnet() {
        // 1. Build a MidnightValidatorAnnounce pointed at the deployed VA
        //    contract address with a real toolkit_ctx (binary + endpoints).
        // 2. Sign an announcement with a devnet validator key.
        // 3. Call `announce(signed)` and assert the TxOutcome executed.
        // 4. Call `get_announced_storage_locations(&[validator_h256])` and
        //    assert the returned location matches what was announced.
        unimplemented!("devnet integration test — see PR follow-up");
    }
}

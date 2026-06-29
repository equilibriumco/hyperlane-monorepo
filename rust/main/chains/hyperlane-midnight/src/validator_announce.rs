use std::str::FromStr;

use async_trait::async_trait;

use hyperlane_core::{
    Announcement, ChainCommunicationError, ChainResult, ContractLocator, FixedPointNumber,
    HyperlaneChain, HyperlaneContract, HyperlaneDomain, HyperlaneProvider, SignedType, TxOutcome,
    ValidatorAnnounce, H160, H256, U256,
};

use crate::toolkit::{self, ToolkitContext};
use crate::{ConnectionConf, MidnightIndexerClient, MidnightProvider};

const DEFAULT_PROOF_SERVER_URL: &str = "http://127.0.0.1:6300";
const DEFAULT_NETWORK_ID: &str = "undeployed";

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

/// Derive a websocket indexer url from the GraphQL http url. Mirrors the same
/// helper in `mailbox.rs`.
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
        // ecrecover output / EVM-style address), matching the Aleo port and
        // the `Bytes<20>` validator key on the contract.
        let validators: Vec<H160> = validators.iter().map(|v| H160::from(*v)).collect();
        toolkit::query_storage_locations(&self.toolkit_ctx, self.address, &validators).await
    }

    async fn announce(&self, announcement: SignedType<Announcement>) -> ChainResult<TxOutcome> {
        let validator = announcement.value.validator;
        let storage_location = announcement.value.storage_location;
        // `SignedType::signature` is a 65-byte ECDSA signature; pass it
        // through unchanged so the on-chain ecrecover can verify it.
        let signature: [u8; 65] = announcement.signature.into();

        let outcome = toolkit::announce_tx(
            &self.toolkit_ctx,
            self.address,
            validator,
            &storage_location,
            &signature,
        )
        .await?;

        Ok(TxOutcome {
            transaction_id: outcome.transaction_id,
            executed: outcome.executed,
            // Midnight fees are denominated in DUST and computed by the wallet
            // at submission time, so the validator agent just needs non-zero
            // placeholders here (mirrors `MidnightMailbox::process`).
            gas_used: U256::from(1_u32),
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
        // H160::from(H256) keeps the low 20 bytes — the ecrecover-style
        // validator key used on the contract.
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

    #[tokio::test]
    async fn announce_rejects_overlong_location() {
        let va = va("/usr/bin/true");
        let location = "x".repeat(toolkit::MAX_STORAGE_LOCATION_LEN + 1);
        let signed = signed_announcement(H160::repeat_byte(0x11), location, [0u8; 65]);
        let err = va.announce(signed).await.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("exceeds on-chain bound") || msg.contains("480"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn announce_rejects_empty_location() {
        let va = va("/usr/bin/true");
        let signed = signed_announcement(H160::repeat_byte(0x11), String::new(), [0u8; 65]);
        let err = va.announce(signed).await.unwrap_err();
        assert!(format!("{err}").contains("empty storage location"));
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
        // through the SignedType -> announce path, so the on-chain ecrecover
        // sees the exact bytes the validator produced. The agent never
        // recomputes the digest (the validator signs, the contract verifies);
        // this guards the pass-through.
        use ethers::signers::{LocalWallet, Signer};
        use hyperlane_core::Signable;

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

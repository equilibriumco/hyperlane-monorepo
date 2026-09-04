//! `MerkleTreeHook` + `MerkleTreeHookIndexer` for Midnight, fused onto one
//! struct the way Aleo and Radix do it.
//!
//! The WarpRoute contract keeps no on-chain merkle tree — Hyperlane's
//! MessageId security model never reads an origin-chain root, and the
//! in-circuit keccak walk a replicated tree needs dominated the outbound
//! proving key. So the contract reads (`tree` / `count` /
//! `latest_checkpoint`) rebuild the tree off-chain from the append-only
//! `dispatched_messages` map, as Sealevel does from its outbox account, while
//! the indexer half derives one leaf per `HYP_DISPATCH` event.

use std::ops::RangeInclusive;

use async_trait::async_trait;

use hyperlane_core::{
    accumulator::incremental::IncrementalMerkle, ChainCommunicationError, ChainResult, Checkpoint,
    CheckpointAtBlock, HyperlaneChain, HyperlaneContract, HyperlaneDomain, HyperlaneProvider,
    IncrementalMerkleAtBlock, Indexed, Indexer, LogMeta, MerkleTreeHook, MerkleTreeInsertion,
    ReorgPeriod, SequenceAwareIndexer, H256, H512,
};

use crate::events::{
    decode_dispatch_event, event_log_meta, h512_to_h256, has_name, MidnightEventReader,
    HYP_DISPATCH,
};
use crate::indexer_client::MiscEvent;
use crate::state_decode::decode_dispatched_messages;
use crate::MidnightProvider;

/// Midnight has BFT finality and the indexer serves only finalized state, so a
/// configured `reorg_period` has no effect. Log it rather than drop it
/// silently, so an operator who set one finds out why.
fn note_reorg_ignored(reorg_period: &ReorgPeriod) {
    if !reorg_period.is_none() {
        tracing::debug!(
            ?reorg_period,
            "Midnight reads finalized state only; ignoring configured reorg_period",
        );
    }
}

fn checkpoint_from_tree(
    tree: &IncrementalMerkle,
    merkle_tree_hook_address: H256,
    mailbox_domain: u32,
) -> ChainResult<CheckpointAtBlock> {
    if tree.count() == 0 {
        return Err(ChainCommunicationError::from_contract_error_str(
            "Midnight merkle tree is empty, cannot compute checkpoint",
        ));
    }
    Ok(CheckpointAtBlock {
        checkpoint: Checkpoint {
            merkle_tree_hook_address,
            mailbox_domain,
            root: tree.root(),
            index: tree.index(),
        },
        block_height: None,
    })
}

/// Every dispatch inserts exactly one leaf, so `leaf_index == nonce` and the
/// leaf is the message's keccak id — the same one the destination ISM
/// re-derives.
fn insertions_from_events(
    events: &[MiscEvent],
    address: H256,
) -> ChainResult<Vec<(Indexed<MerkleTreeInsertion>, LogMeta)>> {
    events
        .iter()
        .filter(|event| has_name(event, HYP_DISPATCH))
        .map(|event| {
            let message = decode_dispatch_event(event)?;
            let insertion = MerkleTreeInsertion::new(message.nonce, message.id());
            Ok((insertion.into(), event_log_meta(event, address)))
        })
        .collect()
}

/// `MerkleTreeHook` + indexer for Midnight's monolithic WarpRoute.
#[derive(Debug, Clone)]
pub struct MidnightMerkleTreeHook {
    address: H256,
    domain: HyperlaneDomain,
    provider: MidnightProvider,
}

impl MidnightMerkleTreeHook {
    /// `address` is the monolithic contract, shared with the mailbox and ISM.
    pub fn new(address: H256, domain: HyperlaneDomain, provider: MidnightProvider) -> Self {
        Self {
            address,
            domain,
            provider,
        }
    }

    fn address_hex(&self) -> String {
        format!("{:x}", self.address)
    }

    async fn dispatched_messages(
        &self,
    ) -> ChainResult<Vec<(u32, hyperlane_core::HyperlaneMessage)>> {
        let bytes = self
            .provider
            .indexer()
            .contract_state(&self.address_hex())
            .await?;
        decode_dispatched_messages(&bytes)
    }

    async fn latest_height_u32(&self) -> ChainResult<u32> {
        let height = self
            .provider
            .indexer()
            .latest_block_height()
            .await?
            .unwrap_or(0);
        Ok(u32::try_from(height).unwrap_or(u32::MAX))
    }
}

impl HyperlaneChain for MidnightMerkleTreeHook {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(self.provider.clone())
    }
}

impl HyperlaneContract for MidnightMerkleTreeHook {
    fn address(&self) -> H256 {
        self.address
    }
}

#[async_trait]
impl MerkleTreeHook for MidnightMerkleTreeHook {
    async fn tree(&self, reorg_period: &ReorgPeriod) -> ChainResult<IncrementalMerkleAtBlock> {
        note_reorg_ignored(reorg_period);
        let messages = self.dispatched_messages().await?;
        let mut tree = IncrementalMerkle::default();
        for (_, message) in &messages {
            tree.ingest(message.id());
        }
        Ok(IncrementalMerkleAtBlock {
            tree,
            block_height: None,
        })
    }

    async fn count(&self, reorg_period: &ReorgPeriod) -> ChainResult<u32> {
        note_reorg_ignored(reorg_period);
        Ok(self.tree(reorg_period).await?.tree.count() as u32)
    }

    /// This root is the one the validator signs, so its local cross-check is
    /// against the same reconstruction.
    async fn latest_checkpoint(
        &self,
        reorg_period: &ReorgPeriod,
    ) -> ChainResult<CheckpointAtBlock> {
        note_reorg_ignored(reorg_period);
        let tree = self.tree(reorg_period).await?.tree;
        checkpoint_from_tree(&tree, self.address, self.domain.id())
    }

    /// Midnight cannot read point-in-time state yet, so this returns the latest
    /// checkpoint regardless of height.
    async fn latest_checkpoint_at_block(&self, _height: u64) -> ChainResult<CheckpointAtBlock> {
        self.latest_checkpoint(&ReorgPeriod::None).await
    }
}

#[async_trait]
impl Indexer<MerkleTreeInsertion> for MidnightMerkleTreeHook {
    async fn fetch_logs_in_range(
        &self,
        range: RangeInclusive<u32>,
    ) -> ChainResult<Vec<(Indexed<MerkleTreeInsertion>, LogMeta)>> {
        let events = self
            .provider
            .indexer()
            .misc_events(&self.address_hex(), *range.start(), *range.end())
            .await?;
        insertions_from_events(&events, self.address)
    }

    /// Midnight has BFT finality, so the latest observed height is final.
    async fn get_finalized_block_number(&self) -> ChainResult<u32> {
        self.latest_height_u32().await
    }

    async fn fetch_logs_by_tx_hash(
        &self,
        tx_hash: H512,
    ) -> ChainResult<Vec<(Indexed<MerkleTreeInsertion>, LogMeta)>> {
        // A dispatch and its insertion are the same event on Midnight, so the
        // transactionHash filter serves both sync tasks.
        let Some(tx_hash) = h512_to_h256(tx_hash) else {
            return Ok(Vec::new());
        };
        let events = self
            .provider
            .indexer()
            .misc_events_by_tx(&self.address_hex(), &tx_hash)
            .await?;
        insertions_from_events(&events, self.address)
    }
}

#[async_trait]
impl SequenceAwareIndexer<MerkleTreeInsertion> for MidnightMerkleTreeHook {
    async fn latest_sequence_count_and_tip(&self) -> ChainResult<(Option<u32>, u32)> {
        // Every dispatch inserts one leaf, so the Mailbox `nonce` counter is
        // the leaf count — one cheap state read instead of rebuilding the whole
        // tree on every cursor tick.
        let tip = self.latest_height_u32().await?;
        let count = self
            .provider
            .indexer()
            .read_nonce_count(&self.address_hex())
            .await?;
        Ok((Some(count), tip))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyperlane_core::accumulator::incremental::IncrementalMerkle;
    use hyperlane_core::KnownHyperlaneDomain;
    use url::Url;

    const TEST_DOMAIN: u32 = 1234;
    fn test_addr() -> H256 {
        H256::repeat_byte(0xaa)
    }

    // The validator hits this at startup, so it must surface "no checkpoint"
    // rather than an index underflow.
    #[test]
    fn checkpoint_errors_on_empty_tree() {
        let tree = IncrementalMerkle::default();
        assert!(
            checkpoint_from_tree(&tree, test_addr(), TEST_DOMAIN).is_err(),
            "an empty tree has no checkpoint to sign"
        );
    }

    #[test]
    fn checkpoint_from_nonempty_tree() {
        let mut tree = IncrementalMerkle::default();
        tree.ingest(H256::repeat_byte(0x11));
        tree.ingest(H256::repeat_byte(0x22));
        let cp = checkpoint_from_tree(&tree, test_addr(), TEST_DOMAIN)
            .expect("non-empty tree yields a checkpoint");
        assert_eq!(cp.checkpoint.index, 1, "index == count - 1");
        assert_eq!(
            cp.checkpoint.root,
            tree.root(),
            "root is the reconstructed tree root"
        );
        assert_eq!(cp.checkpoint.mailbox_domain, TEST_DOMAIN);
        assert_eq!(cp.checkpoint.merkle_tree_hook_address, test_addr());
    }

    // Cross-checks the event-derived leaves against the same messages decoded
    // from the state fixture, which is the parity `tree()` depends on.
    #[test]
    fn insertions_derive_from_dispatch_events() {
        use hyperlane_core::{Encode as _, U256};

        use crate::events::test_util::misc_event;
        use crate::events::HYP_PROCESS;

        let hex = include_str!("../tests/fixtures/night-state-dispatched.hex").trim();
        let bytes = hex::decode(hex).expect("fixture is valid hex");
        let mut messages = decode_dispatched_messages(&bytes).expect("decode dispatched messages");
        messages.sort_by_key(|(nonce, _)| *nonce);
        assert_eq!(messages.len(), 2, "fixture has two dispatches");

        let events = vec![
            misc_event(1, HYP_DISPATCH, &messages[0].1.to_vec()),
            misc_event(2, HYP_PROCESS, H256::repeat_byte(0x11).as_bytes()),
            misc_event(3, HYP_DISPATCH, &messages[1].1.to_vec()),
        ];

        let all = insertions_from_events(&events, test_addr()).expect("derive insertions");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].0.sequence, Some(0));
        assert_eq!(all[1].0.sequence, Some(1));
        assert_eq!(
            *all[0].0.inner(),
            MerkleTreeInsertion::new(0, messages[0].1.id()),
            "leaf = (nonce, keccak id) of the decoded message"
        );
        assert_eq!(
            *all[1].0.inner(),
            MerkleTreeInsertion::new(1, messages[1].1.id())
        );

        assert_eq!(all[0].1.address, test_addr());
        assert_eq!(all[0].1.block_number, events[0].block_height);
        assert_eq!(all[0].1.block_hash, events[0].block_hash);
        assert_eq!(all[0].1.transaction_id, H512::from(events[0].tx_hash));
        assert_eq!(all[0].1.transaction_index, events[0].tx_id);
        assert_eq!(all[0].1.log_index, U256::from(events[0].id));
    }

    /// Live check that a tree rebuilt from the indexer's insertions matches the
    /// root `latest_checkpoint` reconstructs from state — the event/state
    /// parity the validator relies on. Run after a `transferRemote`:
    ///
    ///   MIDNIGHT_INDEXER_URL=http://127.0.0.1:8088/api/v3/graphql \
    ///   MIDNIGHT_NIGHT_ADDRESS=<deployed night address hex> \
    ///   cargo test -p hyperlane-midnight merkle_root_parity -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "requires a running Midnight devnet (events-epic indexer) with at least one dispatch"]
    async fn merkle_root_parity_against_live_devnet() {
        let endpoint = std::env::var("MIDNIGHT_INDEXER_URL")
            .expect("set MIDNIGHT_INDEXER_URL to the indexer GraphQL endpoint");
        let address = std::env::var("MIDNIGHT_NIGHT_ADDRESS")
            .expect("set MIDNIGHT_NIGHT_ADDRESS to the deployed night contract address (hex)");

        let indexer = crate::MidnightIndexerClient::new(Url::parse(&endpoint).expect("valid URL"));
        let domain = HyperlaneDomain::Known(KnownHyperlaneDomain::Midnight);
        let addr_bytes = hex::decode(address.trim_start_matches("0x")).expect("hex address");
        let addr = H256::from_slice(&addr_bytes);
        let provider = MidnightProvider::new(domain.clone(), indexer);
        let hook = MidnightMerkleTreeHook::new(addr, domain, provider);

        let (count_opt, tip) = hook
            .latest_sequence_count_and_tip()
            .await
            .expect("latest sequence count");
        let count = count_opt.expect("a leaf count");
        assert!(
            count > 0,
            "dispatch at least one message before running this"
        );

        let logs = hook
            .fetch_logs_in_range(0..=tip)
            .await
            .expect("fetch insertions");
        assert_eq!(logs.len(), count as usize, "one insertion per leaf");

        let mut tree = IncrementalMerkle::default();
        for (indexed, _meta) in &logs {
            tree.ingest(indexed.inner().message_id());
        }

        let checkpoint = hook
            .latest_checkpoint(&ReorgPeriod::None)
            .await
            .expect("latest checkpoint");
        assert_eq!(
            tree.root(),
            checkpoint.checkpoint.root,
            "local root rebuilt from indexer insertions must match the hook's reconstructed checkpoint root"
        );
        assert_eq!(
            checkpoint.checkpoint.index,
            count - 1,
            "checkpoint index is count - 1"
        );
    }

    // Proves offline that a Midnight-origin checkpoint is byte-identical to an
    // EVM-origin one and that a standard Hyperlane signature over it recovers
    // to the expected validator. The committed vector's digests and signature
    // come from the independent `@hyperlane-xyz/utils` oracle.
    #[test]
    fn checkpoint_signing_matches_evm_reference_vector() {
        use hyperlane_core::{CheckpointWithMessageId, Signable, Signature, SignedType, H160};
        use serde::Deserialize;

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Vector {
            origin: u32,
            merkle_tree_hook: String,
            root: String,
            index: u32,
            message_id: String,
            domain_hash: String,
            inner: String,
            digest: String,
            validator: String,
            signature: String,
        }

        fn h256(hex_str: &str) -> H256 {
            H256::from_slice(&hex::decode(hex_str.trim_start_matches("0x")).expect("hex"))
        }

        let vector: Vector = serde_json::from_str(include_str!(
            "../tests/fixtures/hyperlane-checkpoint-vector.json"
        ))
        .expect("vector json parses");

        // Drift guard: the committed fixture and vector must describe one
        // dispatch state.
        let bytes =
            hex::decode(include_str!("../tests/fixtures/night-state-dispatched.hex").trim())
                .expect("fixture is valid hex");
        let mut messages = decode_dispatched_messages(&bytes).expect("decode dispatched messages");
        messages.sort_by_key(|(nonce, _)| *nonce);

        let mut tree = IncrementalMerkle::default();
        for (_nonce, message) in &messages {
            tree.ingest(message.id());
        }
        assert_eq!(
            tree.root(),
            h256(&vector.root),
            "reconstructed root must match the vector's root input (fixture/vector drift)"
        );
        assert_eq!(tree.index(), vector.index, "tip index is count - 1");

        let tip = messages
            .iter()
            .find(|(nonce, _)| *nonce == vector.index)
            .expect("tip leaf present in fixture");
        assert_eq!(
            tip.1.id(),
            h256(&vector.message_id),
            "fixture tip messageId must match the vector (fixture/vector drift)"
        );

        let checkpoint = CheckpointWithMessageId {
            checkpoint: Checkpoint {
                merkle_tree_hook_address: h256(&vector.merkle_tree_hook),
                mailbox_domain: vector.origin,
                root: h256(&vector.root),
                index: vector.index,
            },
            message_id: h256(&vector.message_id),
        };

        // Redundant with the inner digest below, but it localises a domain-hash
        // bug one step earlier.
        assert_eq!(
            hyperlane_core::utils::domain_hash(
                checkpoint.merkle_tree_hook_address,
                checkpoint.mailbox_domain,
            ),
            h256(&vector.domain_hash),
            "domain_hash(merkle_tree_hook, origin) must equal the EVM reference domainHash"
        );

        assert_eq!(
            checkpoint.signing_hash(),
            h256(&vector.inner),
            "signing_hash must equal the EVM reference inner digest"
        );
        assert_eq!(
            checkpoint.eth_signed_message_hash(),
            h256(&vector.digest),
            "EIP-191 digest must equal the EVM reference digest"
        );

        let sig_bytes = hex::decode(vector.signature.trim_start_matches("0x")).expect("sig hex");
        let signature: Signature = ethers::core::types::Signature::try_from(sig_bytes.as_slice())
            .expect("65-byte signature")
            .into();
        let signed = SignedType {
            value: checkpoint,
            signature,
        };
        let recovered: H160 = signed.recover().expect("recover signer");
        assert_eq!(
            recovered,
            H160::from_slice(&hex::decode(vector.validator.trim_start_matches("0x")).unwrap()),
            "signature must recover to the announced validator address"
        );
    }
}

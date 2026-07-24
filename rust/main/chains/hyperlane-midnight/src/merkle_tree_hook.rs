//! `MerkleTreeHook` + `MerkleTreeHookIndexer` for Midnight.
//!
//! Both the contract abstraction (root/count/checkpoint reads) and the
//! sequence-aware indexer (leaf insertions) are implemented on one
//! `MidnightMerkleTreeHook` struct — the fused shape Aleo and Radix use, and
//! the one the fork already scopes under #15 (`build_merkle_tree_hook`
//! errored with "see issue #15" and `build_merkle_tree_hook_indexer` carried
//! a `TODO(#15)`).
//!
//! The WarpRoute (`night`) contract is monolithic, so there is no separate
//! merkle-tree-hook contract: the Mailbox module lives in the same contract as
//! the ISM. The `night` contract does NOT maintain an on-chain merkle tree
//! (Hyperlane's MessageId security model never reads an origin-chain root, and
//! the in-circuit keccak walk that a replicated tree needs dominated the
//! outbound proving key), so the CONTRACT half of this hook (`tree()` /
//! `count()` / `latest_checkpoint()`) reconstructs the tree entirely off-chain
//! by ingesting the append-only `dispatched_messages` map read from the
//! deployed contract's state via the #14 indexer client, decoded by
//! [`crate::state_decode`]. Sealevel likewise rebuilds its tree from the
//! mailbox/outbox account; we mirror that.
//!
//! The INDEXER half is event-based (#95): it derives one
//! `MerkleTreeInsertion(nonce, message.id())` per `HYP_DISPATCH` Misc event,
//! fetched over block ranges — the same events the dispatch indexer replays,
//! so the leaves it emits are exactly the messages the state reconstruction
//! ingests.
//!
//! How the validator uses this (`agents/validator/src/submit.rs`): it feeds
//! the `MerkleTreeInsertion`s emitted by the indexer into its local RocksDB
//! merkle replica, then compares the replica's root against the root returned
//! by [`MerkleTreeHook::latest_checkpoint`]. Both come from the same off-chain
//! reconstruction (`tree()` -> `checkpoint_from_tree`), so the cross-check is
//! self-consistent. The leaves match because the on-chain message id is
//! `keccak256(message)` (real in-circuit keccak, #21), so `HyperlaneMessage::id()`
//! on the Rust side reproduces the exact leaf the destination ISM re-derives.

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

/// Midnight reads only finalized contract state (BFT finality; the runtime API
/// exposes block-final state), so a configured `reorg_period` is ignored. Log
/// it rather than silently dropping it, so an operator who set one sees why it
/// has no effect (Sealevel asserts instead; we prefer not to panic).
fn note_reorg_ignored(reorg_period: &ReorgPeriod) {
    if !reorg_period.is_none() {
        tracing::debug!(
            ?reorg_period,
            "Midnight reads finalized state only; ignoring configured reorg_period",
        );
    }
}

/// Build a checkpoint from the off-chain reconstructed merkle tree. The
/// WarpRoute does not maintain an on-chain merkle tree — Hyperlane's MessageId
/// security model never reads an origin-chain root — so the validator's local
/// replica is the sole source of the root. The checkpoint root is therefore
/// `tree.root()`, exactly what the validator signs in `submit.rs`, so its
/// local-vs-anchor cross-check is self-consistent. Errors on an empty tree
/// (nothing to sign yet); `index = count - 1`.
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

/// Build a `MerkleTreeInsertion` for every `HYP_DISPATCH` event in a
/// Misc-event batch. Every dispatch inserts exactly one leaf, so
/// `leaf_index == nonce` (decoded from the 141-byte message wire form) and
/// `message_id == HyperlaneMessage::id()` (keccak of those bytes — the same
/// leaf the destination ISM re-derives). `insertion.into()` sets
/// `Indexed.sequence = leaf_index`, which the sequence-aware cursor keys on.
/// Non-dispatch events are skipped; a dispatch that fails to decode is an
/// error, not a skip.
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

/// Chain-sourced `MerkleTreeHook` + indexer for Midnight's monolithic
/// WarpRoute. Reads the append-only `dispatched_messages` map from the deployed
/// contract's on-chain state and reconstructs the incremental merkle tree from
/// it off-chain; the contract keeps no on-chain tree.
#[derive(Debug, Clone)]
pub struct MidnightMerkleTreeHook {
    address: H256,
    domain: HyperlaneDomain,
    provider: MidnightProvider,
}

impl MidnightMerkleTreeHook {
    /// Construct a handle to the WarpRoute's merkle tree hook. `address` is the
    /// monolithic `night` contract (the same address used for the mailbox and
    /// ISM).
    pub fn new(address: H256, domain: HyperlaneDomain, provider: MidnightProvider) -> Self {
        Self {
            address,
            domain,
            provider,
        }
    }

    /// Contract address as the indexer's bare-hex scalar (no `0x`), matching
    /// the form `MidnightInterchainSecurityModule` already uses.
    fn address_hex(&self) -> String {
        format!("{:x}", self.address)
    }

    /// Fetch + decode the dispatched messages, sorted by nonce.
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

    /// Latest indexer block height, narrowed to the `u32` Hyperlane uses for
    /// block numbers. Saturates rather than truncating; Midnight devnet
    /// heights are far below `u32::MAX`.
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
    /// Reconstruct the incremental merkle tree by ingesting every dispatched
    /// message id in nonce order. Reproduces the on-chain tree (same leaves,
    /// same Hyperlane incremental-merkle algorithm). Midnight has no
    /// point-in-time state reads (the indexer `offset` is deferred), so
    /// `reorg_period` is ignored — Midnight has BFT finality and no reorgs.
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
        // The contract keeps no on-chain leaf count; derive it from the
        // off-chain reconstruction (same source as `tree()`).
        Ok(self.tree(reorg_period).await?.tree.count() as u32)
    }

    /// The latest checkpoint, built from the off-chain reconstructed tree. The
    /// contract keeps no on-chain root, and this root is exactly the one the
    /// validator signs (`submit.rs` `tree.root()`), so the validator's
    /// correctness cross-check is self-consistent. Errors on an empty tree.
    async fn latest_checkpoint(
        &self,
        reorg_period: &ReorgPeriod,
    ) -> ChainResult<CheckpointAtBlock> {
        note_reorg_ignored(reorg_period);
        let tree = self.tree(reorg_period).await?.tree;
        checkpoint_from_tree(&tree, self.address, self.domain.id())
    }

    /// Midnight cannot read point-in-time state yet (indexer `offset` is
    /// deferred), so this returns the latest checkpoint regardless of height,
    /// the same stance as Sealevel.
    async fn latest_checkpoint_at_block(&self, _height: u64) -> ChainResult<CheckpointAtBlock> {
        self.latest_checkpoint(&ReorgPeriod::None).await
    }
}

#[async_trait]
impl Indexer<MerkleTreeInsertion> for MidnightMerkleTreeHook {
    /// Midnight indexes insertions over BLOCK ranges (`IndexMode::Block`, EVM
    /// parity, #95): replay the `HYP_DISPATCH` events in the range and derive
    /// one leaf per dispatch. The sequence-aware cursor validates leaf-index
    /// contiguity across ranges.
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
    /// Returns it (rather than panicking like Sealevel's merkle indexer) so
    /// any framework path that reads it gets a sane watermark.
    async fn get_finalized_block_number(&self) -> ChainResult<u32> {
        self.latest_height_u32().await
    }

    async fn fetch_logs_by_tx_hash(
        &self,
        tx_hash: H512,
    ) -> ChainResult<Vec<(Indexed<MerkleTreeInsertion>, LogMeta)>> {
        // The relayer broadcasts dispatch txids from the message sync task to
        // the merkle sync task; the dispatch and the insertion are the same
        // event on Midnight, so the transactionHash filter serves both.
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
        // Every dispatch inserts exactly one leaf, so the Mailbox `nonce`
        // counter IS the leaf count — a cheap state read (one fetch, one
        // counter decode) instead of rebuilding the whole tree per cursor
        // tick. `count()` (the MerkleTreeHook contract read) still derives
        // from the full reconstruction above.
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

    // Empty tree (count == 0) is the day-one state of every deployment, before
    // the first dispatch — the validator hits it at startup. `latest_checkpoint`
    // must surface "no checkpoint", not index-underflow.
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

    // The core indexer logic the validator consumes: every HYP_DISPATCH event
    // yields one leaf with `sequence == leaf_index == nonce` and
    // `message_id == HyperlaneMessage::id()`, carried with the real per-event
    // LogMeta. The leaves are cross-checked against the SAME messages decoded
    // from the committed state fixture, proving the event-derived leaves match
    // what the state reconstruction (`tree()`) ingests.
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

        // The wire bytes the contract emits are exactly the encoded message.
        let events = vec![
            misc_event(1, HYP_DISPATCH, &messages[0].1.to_vec()),
            // A non-dispatch event in the same range is not an insertion.
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

        // Real per-event LogMeta.
        assert_eq!(all[0].1.address, test_addr());
        assert_eq!(all[0].1.block_number, events[0].block_height);
        assert_eq!(all[0].1.block_hash, events[0].block_hash);
        assert_eq!(all[0].1.transaction_id, H512::from(events[0].tx_hash));
        assert_eq!(all[0].1.transaction_index, events[0].tx_id);
        assert_eq!(all[0].1.log_index, U256::from(events[0].id));
    }

    /// Live integration test against a running Midnight devnet with at least
    /// one dispatch. Exercises the full merkle path end to end: the leaf
    /// count, that `fetch_logs_in_range` over the whole block range yields
    /// one insertion per leaf (from the HYP_DISPATCH events), and that a
    /// local `IncrementalMerkle` rebuilt from those insertions matches the
    /// root returned by `latest_checkpoint` (which reconstructs from the
    /// `dispatched_messages` state) — the event/state parity the validator
    /// relies on. This is the "roots match on synthetic traffic" acceptance
    /// check; the panic-on-mismatch half lives in the upstream validator
    /// submitter and is covered by the outbound E2E (#26). Ignored by
    /// default; run after a `transferRemote`:
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

        // Block-range fetch (IndexMode::Block): the whole chain so far.
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

    // #17: the validator signs Midnight-origin checkpoints with the upstream,
    // chain-agnostic signing path. This proves, offline (no node, no proof),
    // that the checkpoint a Midnight validator produces is byte-identical to an
    // EVM-origin checkpoint and that a standard Hyperlane signature over it
    // recovers to the expected validator address.
    //
    // The committed `hyperlane-checkpoint-vector.json` is generated by
    // `contracts/tests/utils/generate-checkpoint-vector.ts` from the SAME
    // two-dispatch scenario as `night-state-dispatched.hex`, with the digest +
    // signature produced by the independent `@hyperlane-xyz/utils` oracle. This
    // test first cross-checks that the committed fixture and the committed
    // vector describe one dispatch state (drift guard), then asserts the Rust
    // signing path matches the EVM oracle byte-for-byte.
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

        // --- Drift guard: the committed fixture and vector are one dispatch ---
        let bytes =
            hex::decode(include_str!("../tests/fixtures/night-state-dispatched.hex").trim())
                .expect("fixture is valid hex");
        let mut messages = decode_dispatched_messages(&bytes).expect("decode dispatched messages");
        messages.sort_by_key(|(nonce, _)| *nonce);

        // Local tree rebuilt from the indexer's leaves reproduces the EVM
        // vector's `root` input. The contract keeps no on-chain root, so this
        // off-chain reconstruction is the sole source — exactly the root the
        // validator signs.
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

        // --- Build the checkpoint the validator would sign ---
        let checkpoint = CheckpointWithMessageId {
            checkpoint: Checkpoint {
                merkle_tree_hook_address: h256(&vector.merkle_tree_hook),
                mailbox_domain: vector.origin,
                root: h256(&vector.root),
                index: vector.index,
            },
            message_id: h256(&vector.message_id),
        };

        // Domain hash matches `domain_hash(merkle_tree_hook, origin)` — a redundant
        // layer (it also feeds `inner` below), but it localises a domain-hash bug
        // one step earlier instead of only surfacing in the inner-digest assert.
        assert_eq!(
            hyperlane_core::utils::domain_hash(
                checkpoint.merkle_tree_hook_address,
                checkpoint.mailbox_domain,
            ),
            h256(&vector.domain_hash),
            "domain_hash(merkle_tree_hook, origin) must equal the EVM reference domainHash"
        );

        // Rust signing hash == EVM oracle inner hash (BaseValidator.messageHash);
        // EIP-191 wrap == oracle digest. Byte-identical => format parity.
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

        // A standard Hyperlane signature over that checkpoint recovers to the
        // expected validator address — the exact destination-side verify path.
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

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
//! merkle-tree-hook contract: the Mailbox + MerkleTree modules live in the
//! same contract as the ISM. We read their ledger fields from the deployed
//! contract's on-chain state via the #14 indexer client, decoded by
//! [`crate::state_decode`]. Sealevel reads its tree from the mailbox/outbox
//! account the same way; we mirror that.
//!
//! How the validator uses this (`agents/validator/src/submit.rs`): it feeds
//! the `MerkleTreeInsertion`s emitted by the indexer into its local RocksDB
//! merkle replica, then compares the replica's root against the on-chain root
//! returned by [`MerkleTreeHook::latest_checkpoint`] and panics on mismatch.
//! `latest_checkpoint` therefore reads the chain's cached `current_root`
//! directly (never recomputed), so the comparison is meaningful. The leaves
//! match because the on-chain message id is `keccak256(message)` and the #2
//! keccak mock computes real keccak values, so `HyperlaneMessage::id()` on the
//! Rust side reproduces the exact on-chain leaf.

use std::ops::RangeInclusive;

use async_trait::async_trait;

use hyperlane_core::{
    accumulator::incremental::IncrementalMerkle, ChainCommunicationError, ChainResult, Checkpoint,
    CheckpointAtBlock, HyperlaneChain, HyperlaneContract, HyperlaneDomain, HyperlaneMessage,
    HyperlaneProvider, IncrementalMerkleAtBlock, Indexed, Indexer, LogMeta, MerkleTreeHook,
    MerkleTreeInsertion, ReorgPeriod, SequenceAwareIndexer, H256, H512, U256,
};

use crate::state_decode::{decode_dispatched_messages, decode_merkle_state, MerkleState};
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

/// Build a checkpoint from decoded merkle state. Pure (no I/O) so it is unit-
/// testable without a live indexer. Errors on an empty tree (nothing to sign
/// yet), matching Sealevel/EVM; `index = count - 1`. Anchors on the chain's
/// cached `current_root` — never recomputed — so the validator's
/// local-replica-vs-chain comparison stays meaningful.
fn checkpoint_from_merkle_state(
    state: &MerkleState,
    merkle_tree_hook_address: H256,
    mailbox_domain: u32,
) -> ChainResult<CheckpointAtBlock> {
    let index = state.count.checked_sub(1).ok_or_else(|| {
        ChainCommunicationError::from_contract_error_str(
            "Midnight merkle tree is empty, cannot compute checkpoint",
        )
    })?;
    Ok(CheckpointAtBlock {
        checkpoint: Checkpoint {
            merkle_tree_hook_address,
            mailbox_domain,
            root: state.current_root,
            index,
        },
        block_height: None,
    })
}

/// Build the `MerkleTreeInsertion`s whose leaf index falls in `range`, from the
/// decoded dispatched messages. Pure (no I/O) so it is unit-testable. Under
/// `IndexMode::Sequence` the framework hands a leaf-index range; the resulting
/// `Indexed` carries `sequence = leaf_index` (the sequence cursor keys on it),
/// `leaf_index == nonce`, and `message_id == HyperlaneMessage::id()`.
fn insertions_in_range(
    messages: &[(u32, HyperlaneMessage)],
    range: &RangeInclusive<u32>,
    address: H256,
    block_number: u64,
) -> Vec<(Indexed<MerkleTreeInsertion>, LogMeta)> {
    messages
        .iter()
        .filter(|(nonce, _)| range.contains(nonce))
        .map(|(nonce, message)| {
            let insertion = MerkleTreeInsertion::new(*nonce, message.id());
            let meta = LogMeta {
                address,
                block_number,
                block_hash: H256::zero(),
                transaction_id: H512::zero(),
                transaction_index: 0,
                // Midnight state carries no per-message tx granularity; the
                // leaf index is the stable per-insertion ordinal.
                log_index: U256::from(*nonce),
            };
            (insertion.into(), meta)
        })
        .collect()
}

/// Chain-sourced `MerkleTreeHook` + indexer for Midnight's monolithic
/// WarpRoute. Reads the merkle `count` / `current_root` and the append-only
/// `dispatched_messages` map from the deployed contract's on-chain state.
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

    /// Fetch + decode the merkle `count` and cached `current_root`.
    async fn merkle_state(&self) -> ChainResult<crate::state_decode::MerkleState> {
        let bytes = self.provider.indexer().contract_state(&self.address_hex()).await?;
        decode_merkle_state(&bytes)
    }

    /// Fetch + decode the dispatched messages, sorted by nonce.
    async fn dispatched_messages(
        &self,
    ) -> ChainResult<Vec<(u32, hyperlane_core::HyperlaneMessage)>> {
        let bytes = self.provider.indexer().contract_state(&self.address_hex()).await?;
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
        Ok(self.merkle_state().await?.count)
    }

    /// The latest checkpoint, anchored on the chain's cached `current_root`
    /// (read directly, not recomputed) so the validator's local-vs-on-chain
    /// comparison is meaningful. Errors on an empty tree, matching Sealevel.
    async fn latest_checkpoint(
        &self,
        reorg_period: &ReorgPeriod,
    ) -> ChainResult<CheckpointAtBlock> {
        note_reorg_ignored(reorg_period);
        let state = self.merkle_state().await?;
        checkpoint_from_merkle_state(&state, self.address, self.domain.id())
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
    /// Midnight indexes by sequence (`IndexMode::Sequence`), so `range` is a
    /// leaf-index range. We read the full append-only `dispatched_messages`
    /// map and emit a `MerkleTreeInsertion` for each leaf whose index falls in
    /// the range; the framework's sequence cursor handles dedup and progress.
    /// This is the #14-established pull model (one `contractAction` read of
    /// latest state, no per-block history).
    async fn fetch_logs_in_range(
        &self,
        range: RangeInclusive<u32>,
    ) -> ChainResult<Vec<(Indexed<MerkleTreeInsertion>, LogMeta)>> {
        let messages = self.dispatched_messages().await?;
        let block_number = u64::from(self.latest_height_u32().await?);
        Ok(insertions_in_range(
            &messages,
            &range,
            self.address,
            block_number,
        ))
    }

    /// Midnight has BFT finality, so the latest observed height is final.
    /// Returns it (rather than panicking like Sealevel's merkle indexer) so
    /// any framework path that reads it gets a sane watermark.
    async fn get_finalized_block_number(&self) -> ChainResult<u32> {
        self.latest_height_u32().await
    }
}

#[async_trait]
impl SequenceAwareIndexer<MerkleTreeInsertion> for MidnightMerkleTreeHook {
    async fn latest_sequence_count_and_tip(&self) -> ChainResult<(Option<u32>, u32)> {
        let count = self.merkle_state().await?.count;
        let tip = self.latest_height_u32().await?;
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
        let state = MerkleState {
            count: 0,
            current_root: H256::zero(),
        };
        assert!(
            checkpoint_from_merkle_state(&state, test_addr(), TEST_DOMAIN).is_err(),
            "an empty tree has no checkpoint to sign"
        );
    }

    #[test]
    fn checkpoint_from_nonempty_state() {
        let root = H256::repeat_byte(0x99);
        let state = MerkleState {
            count: 2,
            current_root: root,
        };
        let cp = checkpoint_from_merkle_state(&state, test_addr(), TEST_DOMAIN)
            .expect("non-empty tree yields a checkpoint");
        assert_eq!(cp.checkpoint.index, 1, "index == count - 1");
        assert_eq!(cp.checkpoint.root, root, "anchored on the cached current_root");
        assert_eq!(cp.checkpoint.mailbox_domain, TEST_DOMAIN);
        assert_eq!(cp.checkpoint.merkle_tree_hook_address, test_addr());
    }

    // The core indexer logic the validator consumes: which leaves a range
    // yields, and that each carries `sequence == leaf_index` and the right
    // message id. Exercised offline against the committed dispatched fixture.
    #[test]
    fn insertions_respect_range_and_sequence() {
        let hex = include_str!("../tests/fixtures/night-state-dispatched.hex").trim();
        let bytes = hex::decode(hex).expect("fixture is valid hex");
        let messages = decode_dispatched_messages(&bytes).expect("decode dispatched messages");
        assert_eq!(messages.len(), 2, "fixture has two dispatches");

        // Full range -> both leaves, sequence == leaf index, id == message.id().
        let all = insertions_in_range(&messages, &(0..=1), test_addr(), 7);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].0.sequence, Some(0));
        assert_eq!(all[1].0.sequence, Some(1));
        assert_eq!(
            *all[0].0.inner(),
            MerkleTreeInsertion::new(0, messages[0].1.id())
        );
        assert_eq!(all[0].1.log_index, U256::from(0u32));
        assert_eq!(all[0].1.address, test_addr());
        assert_eq!(all[0].1.block_number, 7);

        // Sub-range -> only the matching leaf.
        let one = insertions_in_range(&messages, &(1..=1), test_addr(), 7);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].0.sequence, Some(1));

        // Out-of-range -> nothing.
        assert!(insertions_in_range(&messages, &(5..=9), test_addr(), 7).is_empty());
    }

    /// Live integration test against a running Midnight devnet with at least
    /// one dispatch. Exercises the full merkle path end to end: the leaf
    /// count, that `fetch_logs_in_range` yields one insertion per leaf, and
    /// that a local `IncrementalMerkle` rebuilt from those insertions
    /// reproduces the on-chain `current_root` returned by `latest_checkpoint`
    /// — the same local-vs-on-chain comparison the validator performs. This is
    /// the "roots match on synthetic traffic" acceptance check; the
    /// panic-on-mismatch half lives in the upstream validator submitter and is
    /// covered by the outbound E2E (#26). Ignored by default; run after a
    /// `transferRemote`:
    ///
    ///   MIDNIGHT_INDEXER_URL=http://127.0.0.1:8088/api/v3/graphql \
    ///   MIDNIGHT_NIGHT_ADDRESS=<deployed night address hex> \
    ///   cargo test -p hyperlane-midnight merkle_root_parity -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "requires a running Midnight devnet with at least one dispatch"]
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

        let (count_opt, _tip) = hook
            .latest_sequence_count_and_tip()
            .await
            .expect("latest sequence count");
        let count = count_opt.expect("a leaf count");
        assert!(count > 0, "dispatch at least one message before running this");

        let logs = hook
            .fetch_logs_in_range(0..=count - 1)
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
            "local root rebuilt from indexer insertions must match on-chain current_root"
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

        let vector: Vector =
            serde_json::from_str(include_str!("../tests/fixtures/hyperlane-checkpoint-vector.json"))
                .expect("vector json parses");

        // --- Drift guard: the committed fixture and vector are one dispatch ---
        let bytes = hex::decode(
            include_str!("../tests/fixtures/night-state-dispatched.hex").trim(),
        )
        .expect("fixture is valid hex");
        let mut messages = decode_dispatched_messages(&bytes).expect("decode dispatched messages");
        messages.sort_by_key(|(nonce, _)| *nonce);
        let merkle = decode_merkle_state(&bytes).expect("decode merkle state");

        // Local tree rebuilt from the indexer's leaves reproduces both the
        // chain's cached `current_root` and the EVM vector's `root` input.
        let mut tree = IncrementalMerkle::default();
        for (_nonce, message) in &messages {
            tree.ingest(message.id());
        }
        assert_eq!(
            tree.root(),
            merkle.current_root,
            "local replica root must match the chain's cached current_root"
        );
        assert_eq!(
            merkle.current_root,
            h256(&vector.root),
            "fixture root must match the vector's root input (fixture/vector drift)"
        );
        assert_eq!(merkle.count - 1, vector.index, "tip index is count - 1");

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

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
    CheckpointAtBlock, HyperlaneChain, HyperlaneContract, HyperlaneDomain, HyperlaneProvider,
    IncrementalMerkleAtBlock, Indexed, Indexer, LogMeta, MerkleTreeHook, MerkleTreeInsertion,
    ReorgPeriod, SequenceAwareIndexer, H256, H512, U256,
};

use crate::state_decode::{decode_dispatched_messages, decode_merkle_state};
use crate::MidnightProvider;

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
    /// message id in nonce order. The result is byte-identical to the on-chain
    /// tree (same leaves, same Hyperlane incremental-merkle algorithm).
    /// Midnight has no point-in-time state reads (the indexer `offset` is
    /// deferred), so `reorg_period` is ignored — Midnight has BFT finality and
    /// no reorgs.
    async fn tree(&self, _reorg_period: &ReorgPeriod) -> ChainResult<IncrementalMerkleAtBlock> {
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

    async fn count(&self, _reorg_period: &ReorgPeriod) -> ChainResult<u32> {
        Ok(self.merkle_state().await?.count)
    }

    /// The latest checkpoint, anchored on the chain's cached `current_root`
    /// (read directly, not recomputed) so the validator's local-vs-on-chain
    /// comparison is meaningful. Errors on an empty tree, matching Sealevel.
    async fn latest_checkpoint(
        &self,
        _reorg_period: &ReorgPeriod,
    ) -> ChainResult<CheckpointAtBlock> {
        let state = self.merkle_state().await?;
        let index = state.count.checked_sub(1).ok_or_else(|| {
            ChainCommunicationError::from_contract_error_str(
                "Midnight merkle tree is empty, cannot compute checkpoint",
            )
        })?;
        let checkpoint = Checkpoint {
            merkle_tree_hook_address: self.address,
            mailbox_domain: self.domain.id(),
            root: state.current_root,
            index,
        };
        Ok(CheckpointAtBlock {
            checkpoint,
            block_height: None,
        })
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

        let out = messages
            .into_iter()
            .filter(|(nonce, _)| range.contains(nonce))
            .map(|(nonce, message)| {
                let insertion = MerkleTreeInsertion::new(nonce, message.id());
                let meta = LogMeta {
                    address: self.address,
                    block_number,
                    block_hash: H256::zero(),
                    transaction_id: H512::zero(),
                    transaction_index: 0,
                    // Midnight state carries no per-message tx granularity;
                    // the leaf index is the stable per-insertion ordinal.
                    log_index: U256::from(nonce),
                };
                // `Indexed::from(MerkleTreeInsertion)` sets `sequence` to the
                // leaf index, which the sequence cursor keys on.
                (insertion.into(), meta)
            })
            .collect();
        Ok(out)
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
}

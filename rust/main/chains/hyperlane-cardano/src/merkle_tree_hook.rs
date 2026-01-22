use async_trait::async_trait;
use hyperlane_core::{
    ChainResult, Checkpoint, CheckpointAtBlock, ContractLocator, HyperlaneChain, HyperlaneContract,
    HyperlaneDomain, HyperlaneMessage, HyperlaneProvider, IncrementalMerkleAtBlock, Indexed,
    Indexer, LogMeta, MerkleTreeHook, MerkleTreeInsertion, ReorgPeriod, SequenceAwareIndexer, H256,
};
use std::fmt::{Debug, Formatter};
use std::ops::RangeInclusive;
use tracing::{info, instrument};

use crate::mailbox::CardanoMailbox;
use crate::mailbox_indexer::CardanoMailboxIndexer;
use crate::provider::CardanoProvider;
use crate::ConnectionConf;

/// A reference to a Merkle Tree Hook on Cardano
/// On Cardano, the merkle tree is stored in the mailbox datum
pub struct CardanoMerkleTreeHook {
    mailbox: CardanoMailbox,
    domain: HyperlaneDomain,
    address: H256,
    conf: ConnectionConf,
}

impl CardanoMerkleTreeHook {
    /// Create a new CardanoMerkleTreeHook
    pub fn new(conf: &ConnectionConf, locator: ContractLocator) -> ChainResult<Self> {
        let mailbox = CardanoMailbox::new(conf, locator.clone(), None)?;

        Ok(Self {
            mailbox,
            domain: locator.domain.clone(),
            address: locator.address,
            conf: conf.clone(),
        })
    }
}

impl Debug for CardanoMerkleTreeHook {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "CardanoMerkleTreeHook {{ domain: {:?}, address: {:?} }}",
            self.domain, self.address
        )
    }
}

impl HyperlaneChain for CardanoMerkleTreeHook {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(CardanoProvider::new(&self.conf, self.domain.clone()))
    }
}

impl HyperlaneContract for CardanoMerkleTreeHook {
    fn address(&self) -> H256 {
        self.address
    }
}

#[async_trait]
impl MerkleTreeHook for CardanoMerkleTreeHook {
    #[instrument(skip(self))]
    async fn latest_checkpoint(
        &self,
        _reorg_period: &ReorgPeriod,
    ) -> ChainResult<CheckpointAtBlock> {
        // Get the tree state from the mailbox datum
        // The Aiken contracts now store full branch state, so tree.root() is correct
        let (tree, block_height) = self.mailbox.tree_and_tip(None).await?;

        let root = tree.root();
        let index = tree.count().saturating_sub(1) as u32;

        info!(
            root = ?root,
            index = index,
            block_height = block_height,
            "Returning latest checkpoint from Cardano mailbox"
        );

        Ok(CheckpointAtBlock {
            checkpoint: Checkpoint {
                merkle_tree_hook_address: self.address(),
                mailbox_domain: self.domain.id(),
                root,
                index,
            },
            block_height: Some(block_height as u64),
        })
    }

    #[instrument(skip(self))]
    async fn tree(&self, _reorg_period: &ReorgPeriod) -> ChainResult<IncrementalMerkleAtBlock> {
        // Get the tree state from the mailbox
        // The Aiken contracts now store full branch state, so the tree is complete
        let (tree, block_height) = self.mailbox.tree_and_tip(None).await?;

        Ok(IncrementalMerkleAtBlock {
            tree,
            block_height: Some(block_height as u64),
        })
    }

    #[instrument(skip(self))]
    async fn count(&self, _reorg_period: &ReorgPeriod) -> ChainResult<u32> {
        let (tree, _) = self.mailbox.tree_and_tip(None).await?;
        Ok(tree.count() as u32)
    }

    async fn latest_checkpoint_at_block(&self, _height: u64) -> ChainResult<CheckpointAtBlock> {
        // Cardano doesn't easily support querying at specific block heights
        // For now, return the latest checkpoint
        self.latest_checkpoint(&ReorgPeriod::None).await
    }
}

/// Cardano Merkle Tree Hook Indexer
/// Indexes MerkleTreeInsertion events by wrapping the mailbox indexer
#[derive(Debug)]
pub struct CardanoMerkleTreeHookIndexer {
    mailbox_indexer: CardanoMailboxIndexer,
}

impl CardanoMerkleTreeHookIndexer {
    /// Create a new CardanoMerkleTreeHookIndexer
    pub fn new(conf: &ConnectionConf, locator: ContractLocator) -> ChainResult<Self> {
        let mailbox_indexer = CardanoMailboxIndexer::new(conf, locator)?;
        Ok(Self { mailbox_indexer })
    }
}

#[async_trait]
impl Indexer<MerkleTreeInsertion> for CardanoMerkleTreeHookIndexer {
    async fn fetch_logs_in_range(
        &self,
        range: RangeInclusive<u32>,
    ) -> ChainResult<Vec<(Indexed<MerkleTreeInsertion>, LogMeta)>> {
        info!(
            "Fetching Cardano MerkleTreeInsertion logs from block {} to {}",
            range.start(),
            range.end()
        );

        // Get dispatched messages from the mailbox indexer (use HyperlaneMessage indexer explicitly)
        let messages: Vec<(Indexed<HyperlaneMessage>, LogMeta)> =
            <CardanoMailboxIndexer as Indexer<HyperlaneMessage>>::fetch_logs_in_range(
                &self.mailbox_indexer,
                range,
            )
            .await?;

        // Convert HyperlaneMessage to MerkleTreeInsertion
        // Use the From trait which automatically sets the sequence from message.nonce
        let insertions = messages
            .into_iter()
            .map(|(indexed_message, log_meta)| {
                let message = indexed_message.inner();
                let insertion = MerkleTreeInsertion::new(message.nonce, message.id());
                // Use .into() to leverage the From<MerkleTreeInsertion> impl which sets sequence
                let indexed: Indexed<MerkleTreeInsertion> = insertion.into();
                (indexed, log_meta)
            })
            .collect();

        Ok(insertions)
    }

    async fn get_finalized_block_number(&self) -> ChainResult<u32> {
        <CardanoMailboxIndexer as Indexer<HyperlaneMessage>>::get_finalized_block_number(
            &self.mailbox_indexer,
        )
        .await
    }
}

#[async_trait]
impl SequenceAwareIndexer<MerkleTreeInsertion> for CardanoMerkleTreeHookIndexer {
    async fn latest_sequence_count_and_tip(&self) -> ChainResult<(Option<u32>, u32)> {
        <CardanoMailboxIndexer as SequenceAwareIndexer<HyperlaneMessage>>::latest_sequence_count_and_tip(
            &self.mailbox_indexer
        ).await
    }
}

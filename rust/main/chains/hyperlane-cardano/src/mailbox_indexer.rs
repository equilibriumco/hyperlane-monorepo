use crate::rpc::conversion::FromRpc;
use crate::rpc::CardanoRpc;
use crate::{CardanoMailbox, ConnectionConf};
use async_trait::async_trait;
use hyperlane_core::{
    ChainCommunicationError, ChainResult, ContractLocator, HyperlaneMessage, Indexed, Indexer,
    LogMeta, SequenceAwareIndexer, H256, H512, U256,
};
use std::ops::RangeInclusive;

#[derive(Debug)]
pub struct CardanoMailboxIndexer {
    cardano_rpc: CardanoRpc,
    mailbox: CardanoMailbox,
}

impl CardanoMailboxIndexer {
    pub fn new(conf: &ConnectionConf, locator: ContractLocator) -> ChainResult<Self> {
        let cardano_rpc = CardanoRpc::new(&conf.url);
        let mailbox = CardanoMailbox::new(conf, locator, None)?;
        Ok(Self {
            cardano_rpc,
            mailbox,
        })
    }

    async fn get_finalized_block_number(&self) -> ChainResult<u32> {
        self.mailbox.finalized_block_number().await
    }
}

#[async_trait]
impl Indexer<HyperlaneMessage> for CardanoMailboxIndexer {
    async fn fetch_logs_in_range(
        &self,
        range: RangeInclusive<u32>,
    ) -> ChainResult<Vec<(Indexed<HyperlaneMessage>, LogMeta)>> {
        let from = *range.start();
        let to = *range.end();

        tracing::info!(
            "Fetching Cardano HyperlaneMessage logs from {} to {}",
            from,
            to
        );

        let response = self
            .cardano_rpc
            .get_messages_by_block_range(from, to)
            .await
            .map_err(ChainCommunicationError::from_other)?;
        let vec = response.messages;
        Ok(vec
            .into_iter()
            .map(|m| {
                (
                    Indexed::new(HyperlaneMessage::from_rpc(m.message.as_ref())),
                    LogMeta {
                        address: self.mailbox.outbox,
                        block_number: m.block as u64,
                        // TODO[cardano]: do we need real values?
                        block_hash: H256::zero(),
                        transaction_id: H512::zero(),
                        transaction_index: 0,
                        log_index: U256::zero(),
                    },
                )
            })
            .collect())
    }

    async fn get_finalized_block_number(&self) -> ChainResult<u32> {
        self.get_finalized_block_number().await
    }
}

#[async_trait]
impl SequenceAwareIndexer<HyperlaneMessage> for CardanoMailboxIndexer {
    async fn latest_sequence_count_and_tip(&self) -> ChainResult<(Option<u32>, u32)> {
        self.mailbox
            .tree_and_tip(None)
            .await
            .map(|(tree, tip)| (Some(tree.count() as u32), tip))
    }
}

// TODO[cardano]: only used by 'scraper' agent
#[async_trait]
impl Indexer<H256> for CardanoMailboxIndexer {
    async fn fetch_logs_in_range(
        &self,
        _range: RangeInclusive<u32>,
    ) -> ChainResult<Vec<(Indexed<H256>, LogMeta)>> {
        todo!("Cardano H256 indexing not yet implemented")
    }

    async fn get_finalized_block_number(&self) -> ChainResult<u32> {
        self.get_finalized_block_number().await
    }
}

#[async_trait]
impl SequenceAwareIndexer<H256> for CardanoMailboxIndexer {
    async fn latest_sequence_count_and_tip(&self) -> ChainResult<(Option<u32>, u32)> {
        // Cardano delivery indexing not yet fully implemented
        // Return None for sequence count and current block tip
        let tip = self.get_finalized_block_number().await?;
        Ok((None, tip))
    }
}

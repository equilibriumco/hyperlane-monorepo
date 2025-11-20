use crate::ConnectionConf;
use async_trait::async_trait;
use hyperlane_core::{
    ChainResult, ContractLocator, Indexed, Indexer, InterchainGasPayment, LogMeta,
    SequenceAwareIndexer,
};
use std::ops::RangeInclusive;
use tracing::instrument;

#[derive(Debug)]
pub struct CardanoInterchainGasPaymasterIndexer {}

impl CardanoInterchainGasPaymasterIndexer {
    pub fn new(_conf: &ConnectionConf, _locator: ContractLocator) -> Self {
        Self {}
    }
}

#[async_trait]
impl Indexer<InterchainGasPayment> for CardanoInterchainGasPaymasterIndexer {
    #[instrument(err, skip(self))]
    async fn fetch_logs_in_range(
        &self,
        _range: RangeInclusive<u32>,
    ) -> ChainResult<Vec<(Indexed<InterchainGasPayment>, LogMeta)>> {
        // TODO[cardano]: gas payments?
        Ok(vec![])
    }

    #[instrument(level = "debug", err, ret, skip(self))]
    async fn get_finalized_block_number(&self) -> ChainResult<u32> {
        // TODO[cardano]: gas payments?
        Ok(0)
    }
}

#[async_trait]
impl SequenceAwareIndexer<InterchainGasPayment> for CardanoInterchainGasPaymasterIndexer {
    async fn latest_sequence_count_and_tip(&self) -> ChainResult<(Option<u32>, u32)> {
        // Cardano gas payment indexing not yet fully implemented
        let tip = self.get_finalized_block_number().await?;
        Ok((None, tip))
    }
}

use crate::cardano::Keypair;
use crate::provider::CardanoProvider;
use crate::rpc::CardanoRpc;
use crate::ConnectionConf;
use async_trait::async_trait;
use hyperlane_core::accumulator::incremental::IncrementalMerkle;
use hyperlane_core::accumulator::TREE_DEPTH;
use hyperlane_core::{
    ChainCommunicationError, ChainResult, ContractLocator, FixedPointNumber, HyperlaneChain,
    HyperlaneContract, HyperlaneDomain, HyperlaneMessage, HyperlaneProvider, Mailbox,
    ReorgPeriod, TxCostEstimate, TxOutcome, H256, H512, U256,
};
use std::fmt::{Debug, Formatter};
use std::num::NonZeroU64;
use std::str::FromStr;

pub struct CardanoMailbox {
    inbox: H256,
    pub outbox: H256,
    domain: HyperlaneDomain,
    cardano_rpc: CardanoRpc,
}

impl CardanoMailbox {
    pub fn new(
        conf: &ConnectionConf,
        locator: ContractLocator,
        _payer: Option<Keypair>,
    ) -> ChainResult<Self> {
        Ok(CardanoMailbox {
            domain: locator.domain.clone(),
            inbox: locator.address,
            outbox: locator.address,
            cardano_rpc: CardanoRpc::new(&conf.url),
        })
    }

    pub async fn finalized_block_number(&self) -> Result<u32, ChainCommunicationError> {
        let finalized_block_number = self
            .cardano_rpc
            .get_finalized_block_number()
            .await
            .map_err(ChainCommunicationError::from_other)?;
        Ok(finalized_block_number)
    }

    pub async fn tree_and_tip(
        &self,
        lag: Option<NonZeroU64>,
    ) -> ChainResult<(IncrementalMerkle, u32)> {
        assert!(lag.is_none(), "Cardano always returns the finalized result");
        let merkle_tree_response = self
            .cardano_rpc
            .get_latest_merkle_tree()
            .await
            .map_err(ChainCommunicationError::from_other)?;
        let merkle_tree = merkle_tree_response.merkle_tree;
        let branch: [H256; TREE_DEPTH] = merkle_tree
            .branches
            .iter()
            .map(
                |b| H256::from_str(b).unwrap(), /* TODO[cardano]: better error handling for RPC output */
            )
            .collect::<Vec<H256>>()
            .try_into()
            .unwrap();
        let count = merkle_tree.count as usize;
        Ok((
            IncrementalMerkle::new(branch, count),
            merkle_tree_response.block_number as u32,
        ))
    }
}

impl HyperlaneContract for CardanoMailbox {
    fn address(&self) -> H256 {
        self.outbox
    }
}

impl HyperlaneChain for CardanoMailbox {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(CardanoProvider::new(self.domain.clone()))
    }
}

impl Debug for CardanoMailbox {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self as &dyn HyperlaneContract)
    }
}

#[async_trait]
impl Mailbox for CardanoMailbox {
    async fn count(&self, _reorg_period: &ReorgPeriod) -> ChainResult<u32> {
        // For Cardano, we ignore reorg_period as it always returns finalized results
        self.tree_and_tip(None).await.map(|(tree, _)| tree.count() as u32)
    }

    async fn delivered(&self, id: H256) -> ChainResult<bool> {
        let res = self
            .cardano_rpc
            .is_inbox_message_delivered(id)
            .await
            .map_err(ChainCommunicationError::from_other)?;
        Ok(res.is_delivered)
    }

    async fn default_ism(&self) -> ChainResult<H256> {
        // ISM on Cardano is a minting policy, not an address
        // TODO[cardano]: We could return the minting policy hash here?
        Ok(H256::zero())
    }

    async fn recipient_ism(&self, _recipient: H256) -> ChainResult<H256> {
        // All messages share the same ISM at the moment
        self.default_ism().await
    }

    async fn process(
        &self,
        message: &HyperlaneMessage,
        metadata: &[u8],
        _tx_gas_limit: Option<U256>,
    ) -> ChainResult<TxOutcome> {
        let res = self
            .cardano_rpc
            .submit_inbox_message(message, metadata)
            .await
            .map_err(ChainCommunicationError::from_other)?;

        // Convert H256 to H512 for transaction_id
        let mut txid_bytes = [0u8; 64];
        let h256_bytes = H256::from_str(res.tx_id.as_str())
            .unwrap_or(H256::zero());
        txid_bytes[..32].copy_from_slice(h256_bytes.as_bytes());

        Ok(TxOutcome {
            transaction_id: H512::from(txid_bytes),
            executed: true,
            gas_used: U256::from(res.fee_lovelace),
            // NOTE: There's no "dynamic" gas price on Cardano
            gas_price: FixedPointNumber::try_from(U256::from(res.fee_lovelace))
                .unwrap_or_else(|_| FixedPointNumber::zero()),
        })
    }

    async fn process_estimate_costs(
        &self,
        message: &HyperlaneMessage,
        metadata: &[u8],
    ) -> ChainResult<TxCostEstimate> {
        let res = self
            .cardano_rpc
            .estimate_inbox_message_fee(message, metadata)
            .await
            .map_err(ChainCommunicationError::from_other)?;
        let fee_lovelace = res.fee_lovelace as u32;
        Ok(TxCostEstimate {
            gas_limit: U256::from(fee_lovelace),
            // NOTE: There's no "dynamic" gas price on Cardano
            gas_price: FixedPointNumber::try_from(U256::from(fee_lovelace))
                .unwrap_or_else(|_| FixedPointNumber::zero()),
            l2_gas_limit: None,
        })
    }

    async fn process_calldata(
        &self,
        _message: &HyperlaneMessage,
        _metadata: &[u8],
    ) -> ChainResult<Vec<u8>> {
        todo!("Cardano process_calldata not yet implemented")
    }

    fn delivered_calldata(&self, _message_id: H256) -> ChainResult<Option<Vec<u8>>> {
        todo!("Cardano delivered_calldata not yet implemented")
    }
}

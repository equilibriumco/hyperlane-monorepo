use async_trait::async_trait;

use hyperlane_core::{
    BlockInfo, ChainInfo, ChainResult, HyperlaneChain, HyperlaneDomain, HyperlaneProvider,
    HyperlaneProviderError, TxnInfo, TxnReceiptInfo, H256, H512, U256,
};

use crate::events::h512_to_h256;
use crate::indexer_client::{BlockDetails, TransactionDetails};
use crate::MidnightIndexerClient;

/// Chain provider backed by the Midnight indexer's GraphQL API. Serves the
/// block/transaction lookups the scraper needs (`ensure_blocks` /
/// `ensure_txns` in `agents/scraper/src/store/storage.rs`) plus the contract
/// state reads the other Midnight abstractions share.
#[derive(Debug, Clone)]
pub struct MidnightProvider {
    domain: HyperlaneDomain,
    indexer: MidnightIndexerClient,
}

impl MidnightProvider {
    /// Build a new provider.
    pub fn new(domain: HyperlaneDomain, indexer: MidnightIndexerClient) -> Self {
        Self { domain, indexer }
    }

    /// Borrow the indexer client, for chain-state reads (e.g. the ISM reading
    /// its validators/threshold/module-type from the deployed contract).
    pub fn indexer(&self) -> &MidnightIndexerClient {
        &self.indexer
    }
}

/// Map indexer block details onto the `BlockInfo` the scraper stores.
/// `BlockInfo.timestamp` is unix SECONDS per the trait contract; the indexer
/// reports unix milliseconds (Substrate `Timestamp::set`).
fn block_info_from(details: BlockDetails) -> BlockInfo {
    BlockInfo {
        hash: details.hash,
        timestamp: details.timestamp_ms / 1000,
        number: details.height,
    }
}

/// Map indexer transaction details onto the `TxnInfo` the scraper stores.
/// Midnight has no gas market: fees are paid in DUST (SPECK), so the paid fee
/// stands in for `gas_limit`/`gas_used` (the Aleo stance) and the price
/// fields are unset. There is no public sender/nonce (shielded transactions),
/// so `sender` is zero and `nonce` 0. `receipt` must be `Some` — the
/// scraper's `store_txns` rejects receipt-less transactions as "not yet
/// included", and everything the indexer serves is final.
fn txn_info_from(hash: H512, details: TransactionDetails) -> TxnInfo {
    let fee = details.fee_specks.unwrap_or_default();
    TxnInfo {
        hash,
        gas_limit: fee,
        max_priority_fee_per_gas: None,
        max_fee_per_gas: None,
        gas_price: None,
        nonce: 0,
        sender: H256::zero(),
        recipient: None,
        receipt: Some(TxnReceiptInfo {
            gas_used: fee,
            cumulative_gas_used: fee,
            effective_gas_price: None,
        }),
        raw_input_data: None,
    }
}

impl HyperlaneChain for MidnightProvider {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(self.clone())
    }
}

#[async_trait]
impl HyperlaneProvider for MidnightProvider {
    async fn get_block_by_height(&self, height: u64) -> ChainResult<BlockInfo> {
        let details = self
            .indexer
            .block_by_height(height)
            .await
            .map_err(Into::<hyperlane_core::ChainCommunicationError>::into)?
            .ok_or(HyperlaneProviderError::CouldNotFindBlockByHeight(height))?;
        if details.height != height {
            return Err(
                HyperlaneProviderError::IncorrectBlockByHeight(height, details.height).into(),
            );
        }
        Ok(block_info_from(details))
    }

    async fn get_txn_by_hash(&self, hash: &H512) -> ChainResult<TxnInfo> {
        // Hyperlane widens 32-byte hashes right-aligned; a non-zero upper
        // half cannot be a Midnight tx hash.
        let narrow = h512_to_h256(*hash)
            .ok_or(HyperlaneProviderError::CouldNotFindTransactionByHash(*hash))?;
        let details = self
            .indexer
            .transaction_by_hash(&narrow)
            .await
            .map_err(Into::<hyperlane_core::ChainCommunicationError>::into)?
            .ok_or(HyperlaneProviderError::CouldNotFindTransactionByHash(*hash))?;
        Ok(txn_info_from(*hash, details))
    }

    async fn is_contract(&self, _address: &H256) -> ChainResult<bool> {
        // Returning false makes `pending_message` drop every inbound message
        // at the `is_recipient_contract` gate. Every routable recipient on
        // Midnight is the monolithic WarpRoute contract, so `true` is correct
        // until non-WarpRoute recipients exist.
        Ok(true)
    }

    async fn get_balance(&self, _address: String) -> ChainResult<U256> {
        Err(crate::HyperlaneMidnightError::NotImplemented("get_balance").into())
    }

    async fn get_chain_metrics(&self) -> ChainResult<Option<ChainInfo>> {
        let _ = self.indexer.latest_block_height().await;
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_info_converts_ms_timestamp_to_seconds() {
        let info = block_info_from(BlockDetails {
            hash: H256::repeat_byte(0xA1),
            height: 42,
            timestamp_ms: 1_700_000_012_345,
        });
        assert_eq!(info.hash, H256::repeat_byte(0xA1));
        assert_eq!(info.number, 42);
        assert_eq!(info.timestamp, 1_700_000_012, "unix seconds, not ms");
    }

    #[test]
    fn txn_info_carries_fee_and_a_receipt() {
        use crate::indexer_client::TxStatus;

        let hash: H512 = H256::repeat_byte(0x01).into();
        let info = txn_info_from(
            hash,
            TransactionDetails {
                hash: H256::repeat_byte(0x01),
                id: 7,
                block: BlockDetails {
                    hash: H256::repeat_byte(0xA1),
                    height: 42,
                    timestamp_ms: 1_700_000_000_000,
                },
                status: Some(TxStatus::Success),
                fee_specks: Some(U256::from(12345)),
            },
        );
        assert_eq!(info.hash, hash);
        assert_eq!(info.gas_limit, U256::from(12345), "paid fee stands in");
        // The scraper's store_txns requires a receipt ("not yet included"
        // otherwise); Midnight only serves final transactions.
        let receipt = info.receipt.expect("receipt is mandatory");
        assert_eq!(receipt.gas_used, U256::from(12345));
        assert_eq!(receipt.cumulative_gas_used, U256::from(12345));
        assert_eq!(receipt.effective_gas_price, None);
    }
}

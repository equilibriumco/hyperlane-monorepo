use async_trait::async_trait;
use hyperlane_core::{
    BlockInfo, ChainCommunicationError, ChainInfo, ChainResult, HyperlaneChain, HyperlaneDomain,
    HyperlaneProvider, TxnInfo, TxnReceiptInfo, H256, H512, U256,
};
use std::sync::Arc;

use crate::blockfrost_provider::{BlockfrostProvider, TransactionInfo, TransactionUtxos};
use crate::types::{address_to_h256, h512_to_tx_hash};
use crate::ConnectionConf;

#[derive(Debug, Clone)]
pub struct CardanoProvider {
    domain: HyperlaneDomain,
    provider: Arc<BlockfrostProvider>,
}

impl CardanoProvider {
    pub fn new(conf: &ConnectionConf, domain: HyperlaneDomain) -> Self {
        let provider =
            BlockfrostProvider::new(&conf.api_key, conf.network, conf.confirmation_block_delay);
        CardanoProvider {
            domain,
            provider: Arc::new(provider),
        }
    }

    pub fn blockfrost(&self) -> &BlockfrostProvider {
        &self.provider
    }
}

impl HyperlaneChain for CardanoProvider {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(self.clone())
    }
}

fn to_chain_err(e: impl std::fmt::Display) -> ChainCommunicationError {
    ChainCommunicationError::from_other_str(&e.to_string())
}

fn block_hash_to_h256(hash_hex: &str) -> ChainResult<H256> {
    let bytes = hex::decode(hash_hex).map_err(|e| {
        ChainCommunicationError::from_other_str(&format!(
            "Invalid block hash hex '{hash_hex}': {e}"
        ))
    })?;
    if bytes.len() != 32 {
        return Err(ChainCommunicationError::from_other_str(&format!(
            "Block hash has unexpected length {} (expected 32): '{hash_hex}'",
            bytes.len()
        )));
    }
    Ok(H256::from_slice(&bytes))
}

/// The address that funded a transaction, as the 32-byte Hyperlane encoding of
/// its payment credential.
///
/// Cardano transactions have no single sender, so the first spent (non-reference,
/// non-collateral) input stands in — that is the fee payer for every transaction
/// the Hyperlane agents build. Unresolvable addresses (Byron, malformed) yield
/// zero rather than failing the whole lookup: the scraper would otherwise drop
/// an entire message just because its sender could not be rendered.
fn sender_from(utxos: &TransactionUtxos) -> H256 {
    utxos
        .inputs
        .iter()
        .find(|i| !i.reference && !i.collateral)
        .and_then(|i| address_to_h256(&i.address).ok())
        .unwrap_or_else(H256::zero)
}

/// Map Blockfrost transaction details onto the `TxnInfo` the scraper stores.
///
/// Cardano has no gas market: fees are computed from transaction size plus
/// script execution units and paid in lovelace, so the paid fee stands in for
/// `gas_limit`/`gas_used` and the price fields are unset. `receipt` must be
/// `Some` — the scraper's `store_txns` rejects receipt-less transactions as
/// "not yet included", and Blockfrost only serves on-chain transactions.
fn txn_info_from(hash: H512, tx: TransactionInfo, utxos: TransactionUtxos) -> TxnInfo {
    let fee = U256::from(tx.fees);
    TxnInfo {
        hash,
        gas_limit: fee,
        max_priority_fee_per_gas: None,
        max_fee_per_gas: None,
        gas_price: None,
        // Cardano orders transactions by UTXO consumption, not by an account
        // nonce; the in-block index is the closest stable ordinal.
        nonce: tx.index as u64,
        sender: sender_from(&utxos),
        recipient: None,
        receipt: Some(TxnReceiptInfo {
            gas_used: fee,
            cumulative_gas_used: fee,
            effective_gas_price: None,
        }),
        raw_input_data: None,
    }
}

#[async_trait]
impl HyperlaneProvider for CardanoProvider {
    async fn get_block_by_height(&self, height: u64) -> ChainResult<BlockInfo> {
        let finalized = self
            .provider
            .get_latest_block()
            .await
            .map_err(to_chain_err)?;

        if height > finalized {
            return Err(ChainCommunicationError::from_other_str(&format!(
                "Block {height} not yet finalized (current: {finalized})"
            )));
        }

        let block = self
            .provider
            .get_block_by_height(height)
            .await
            .map_err(to_chain_err)?;

        Ok(BlockInfo {
            hash: block_hash_to_h256(&block.hash)?,
            timestamp: block.time,
            number: block.height,
        })
    }

    async fn get_txn_by_hash(&self, hash: &H512) -> ChainResult<TxnInfo> {
        let tx_hash = h512_to_tx_hash(hash);
        let tx = self
            .provider
            .get_transaction(&tx_hash)
            .await
            .map_err(to_chain_err)?;
        let utxos = self
            .provider
            .get_transaction_utxos(&tx_hash)
            .await
            .map_err(to_chain_err)?;
        Ok(txn_info_from(*hash, tx, utxos))
    }

    async fn is_contract(&self, _address: &H256) -> ChainResult<bool> {
        Ok(true)
    }

    async fn get_balance(&self, address: String) -> ChainResult<U256> {
        let utxos = self
            .provider
            .get_utxos_at_address(&address)
            .await
            .map_err(to_chain_err)?;
        let total_lovelace: u64 = utxos.iter().map(|u| u.lovelace()).sum();
        Ok(U256::from(total_lovelace))
    }

    async fn get_chain_metrics(&self) -> ChainResult<Option<ChainInfo>> {
        let block = self
            .provider
            .get_latest_block_info()
            .await
            .map_err(to_chain_err)?;

        Ok(Some(ChainInfo {
            latest_block: BlockInfo {
                hash: block_hash_to_h256(&block.hash)?,
                timestamp: block.time,
                number: block.height,
            },
            min_gas_price: None,
        }))
    }
}

use async_trait::async_trait;
use hyperlane_core::{
    BlockInfo, ChainCommunicationError, ChainInfo, ChainResult, HyperlaneChain, HyperlaneDomain,
    HyperlaneProvider, TxnInfo, TxnReceiptInfo, H256, H512, U256,
};
use std::sync::Arc;

use crate::blockfrost_provider::{BlockfrostProvider, TransactionInfo, TransactionUtxos};
use crate::consts::KEY_HASH_ADDR_PREFIX;
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
/// Cardano transactions have no single sender, so the fee payer stands in.
///
/// Inputs are ordered canonically by `(tx_hash, index)`, not by role, so the
/// first spendable input is arbitrary — for a dispatch it is usually the mailbox
/// state UTXO, which would report the mailbox as having sent every message.
/// Only a key credential can pay fees (a script cannot sign), so prefer the
/// first spendable input held by one, and fall back to the first spendable input
/// when none is found.
///
/// Unresolvable addresses (Byron, malformed) yield zero rather than failing the
/// whole lookup: the scraper would otherwise drop an entire message just because
/// its sender could not be rendered.
fn sender_from(utxos: &TransactionUtxos) -> H256 {
    let spendable = || {
        utxos
            .inputs
            .iter()
            .filter(|i| !i.reference && !i.collateral)
    };

    spendable()
        .filter_map(|i| address_to_h256(&i.address).ok())
        .find(|h| h[0] == KEY_HASH_ADDR_PREFIX)
        .or_else(|| spendable().find_map(|i| address_to_h256(&i.address).ok()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blockfrost_provider::Utxo;

    const MAILBOX_SCRIPT: &str = "addr_test1wr5xv7dqnkz2pr2fv0fytt30zfv4ffqrkvlg33djsc5e7lgf0907t";
    const PAYER_WALLET: &str = "addr_test1vqfp9gpr8qqzp7x8h99cx8j90w0wvhcqnhuar4vggvxuezg4hvheh";

    fn input(address: &str, reference: bool, collateral: bool) -> Utxo {
        Utxo {
            tx_hash: "00".repeat(32),
            output_index: 0,
            address: address.to_string(),
            value: vec![],
            inline_datum: None,
            data_hash: None,
            reference_script_hash: None,
            collateral,
            reference,
        }
    }

    fn utxos(inputs: Vec<Utxo>) -> TransactionUtxos {
        TransactionUtxos {
            hash: "00".repeat(32),
            inputs,
            outputs: vec![],
        }
    }

    #[test]
    fn sender_is_the_fee_payer_not_the_first_script_input() {
        // Inputs are ordered canonically by (tx_hash, index), so a dispatch puts
        // the mailbox state UTXO ahead of the payer's. Taking the first spendable
        // input would report the mailbox as the sender of every message.
        let tx = utxos(vec![
            input(PAYER_WALLET, true, false), // reference input, not a payer
            input(MAILBOX_SCRIPT, false, false),
            input(PAYER_WALLET, false, false),
            input(PAYER_WALLET, false, true), // collateral, not a payer
        ]);

        let expected = address_to_h256(PAYER_WALLET).unwrap();
        assert_eq!(sender_from(&tx), expected);
        assert_eq!(sender_from(&tx)[0], KEY_HASH_ADDR_PREFIX);
    }

    #[test]
    fn sender_falls_back_to_a_script_when_no_key_input_is_spent() {
        let tx = utxos(vec![input(MAILBOX_SCRIPT, false, false)]);
        assert_eq!(sender_from(&tx), address_to_h256(MAILBOX_SCRIPT).unwrap());
    }

    #[test]
    fn sender_is_zero_when_nothing_resolves() {
        assert_eq!(sender_from(&utxos(vec![])), H256::zero());
        assert_eq!(
            sender_from(&utxos(vec![input("not-an-address", false, false)])),
            H256::zero()
        );
    }
}

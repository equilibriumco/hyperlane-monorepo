//! Dispatch and delivery indexers for the Midnight Mailbox.
//!
//! Midnight has no event log and no Compact event system (see `LIMITS.md`), so
//! both indexers read the WarpRoute contract's *block-final* ledger state
//! through the Midnight indexer client (#14) instead of replaying logs:
//!
//!   - [`MidnightDispatchIndexer`] serves dispatched `HyperlaneMessage`s out of
//!     the `dispatched_messages: Map<Uint<32>, Bytes<141>>` ledger field, keyed
//!     by nonce. It is sequence-aware: the `nonce` counter is the sequence tip,
//!     and a requested nonce range maps one-to-one onto map keys.
//!   - [`MidnightDeliveryIndexer`] serves delivered message ids out of the
//!     `deliveries: Set<Bytes<32>>` ledger field. The set is unordered and has
//!     no sequence, so this indexer is rate-limited: every scan re-reads the
//!     full set and the relayer's dedup store handles idempotency (see
//!     `LIMITS.md`).
//!
//! Neither side has per-event block/tx metadata available from state, so the
//! emitted [`LogMeta`] carries only the contract address and the current tip as
//! the block number, with the remaining fields zeroed. This mirrors the
//! Sealevel mailbox indexer's non-`advanced_log_meta` path.

use std::ops::RangeInclusive;

use async_trait::async_trait;

use hyperlane_core::{
    ChainResult, ContractLocator, HyperlaneMessage, Indexed, Indexer, LogMeta,
    SequenceAwareIndexer, H256, H512, U256,
};

use crate::MidnightIndexerClient;

/// Render an `H256` contract address as the lowercase hex string the indexer
/// client's `contractAction` query expects (no `0x` prefix), matching the
/// convention used by the Midnight ISM reads.
fn address_hex(address: &H256) -> String {
    format!("{address:x}")
}

/// Build the `LogMeta` for a state-sourced item. Midnight block-final state
/// gives us no per-event block hash, tx id, tx index or log index, so those are
/// zeroed; `block_number` carries the current tip and `address` the WarpRoute
/// contract. Mirrors Sealevel's non-advanced log-meta path.
fn state_log_meta(address: H256, tip: u32) -> LogMeta {
    LogMeta {
        address,
        block_number: tip as u64,
        block_hash: H256::zero(),
        transaction_id: H512::zero(),
        transaction_index: 0,
        log_index: U256::zero(),
    }
}

/// Read the indexer's latest observed block height as a `u32` tip. The indexer
/// reports a `u64`; heights beyond `u32::MAX` are saturated rather than
/// truncated. `None` (indexer has seen no block yet) maps to `0`.
async fn read_tip(client: &MidnightIndexerClient) -> ChainResult<u32> {
    let height = client
        .latest_block_height()
        .await
        .map_err(Into::<hyperlane_core::ChainCommunicationError>::into)?;
    Ok(height.map(|h| u32::try_from(h).unwrap_or(u32::MAX)).unwrap_or(0))
}

/// Indexer that serves dispatched `HyperlaneMessage`s from the Midnight
/// Mailbox's `dispatched_messages` map.
#[derive(Debug, Clone)]
pub struct MidnightDispatchIndexer {
    client: MidnightIndexerClient,
    /// WarpRoute (Mailbox) contract address; both the GraphQL state read key and
    /// the `LogMeta.address` reported for every dispatched message.
    address: H256,
}

impl MidnightDispatchIndexer {
    /// Build a dispatch indexer for the contract named by `locator`.
    pub fn new(client: MidnightIndexerClient, locator: &ContractLocator) -> Self {
        Self {
            client,
            address: locator.address,
        }
    }
}

#[async_trait]
impl Indexer<HyperlaneMessage> for MidnightDispatchIndexer {
    async fn fetch_logs_in_range(
        &self,
        range: RangeInclusive<u32>,
    ) -> ChainResult<Vec<(Indexed<HyperlaneMessage>, LogMeta)>> {
        let address = address_hex(&self.address);
        let tip = read_tip(&self.client).await?;

        let mut out = Vec::new();
        for nonce in range {
            // Each nonce is a key in `dispatched_messages`. A missing key (no
            // message dispatched at that nonce) is skipped, not an error: the
            // cursor may request nonces that have not been dispatched yet.
            if let Some(message) = self.client.read_dispatched_message(&address, nonce).await? {
                out.push((
                    Indexed::from(message),
                    state_log_meta(self.address, tip),
                ));
            }
        }
        Ok(out)
    }

    async fn get_finalized_block_number(&self) -> ChainResult<u32> {
        read_tip(&self.client).await
    }

    async fn fetch_logs_by_tx_hash(
        &self,
        _tx_hash: H512,
    ) -> ChainResult<Vec<(Indexed<HyperlaneMessage>, LogMeta)>> {
        // Midnight state reads are not addressable by tx hash.
        Ok(Vec::new())
    }
}

#[async_trait]
impl SequenceAwareIndexer<HyperlaneMessage> for MidnightDispatchIndexer {
    async fn latest_sequence_count_and_tip(&self) -> ChainResult<(Option<u32>, u32)> {
        let address = address_hex(&self.address);
        let count = self.client.read_nonce_count(&address).await?;
        let tip = read_tip(&self.client).await?;
        Ok((Some(count), tip))
    }
}

/// Indexer that serves delivered message ids from the Midnight Mailbox's
/// `deliveries` set. Rate-limited: every scan re-reads the full set.
#[derive(Debug, Clone)]
pub struct MidnightDeliveryIndexer {
    client: MidnightIndexerClient,
    /// WarpRoute (Mailbox) contract address; both the GraphQL state read key and
    /// the `LogMeta.address` reported for every delivery.
    address: H256,
}

impl MidnightDeliveryIndexer {
    /// Build a delivery indexer for the contract named by `locator`.
    pub fn new(client: MidnightIndexerClient, locator: &ContractLocator) -> Self {
        Self {
            client,
            address: locator.address,
        }
    }
}

#[async_trait]
impl Indexer<H256> for MidnightDeliveryIndexer {
    async fn fetch_logs_in_range(
        &self,
        _range: RangeInclusive<u32>,
    ) -> ChainResult<Vec<(Indexed<H256>, LogMeta)>> {
        // The deliveries set has no nonce/sequence to slice by, so the range is
        // ignored: every scan returns the full set. The relayer's dedup store
        // (rate-limited cursor) makes re-emitting already-seen ids idempotent.
        let address = address_hex(&self.address);
        let tip = read_tip(&self.client).await?;

        let ids = self.client.read_deliveries(&address).await?;
        let log_meta = state_log_meta(self.address, tip);
        Ok(ids
            .into_iter()
            .map(|id| (Indexed::new(id), log_meta.clone()))
            .collect())
    }

    async fn get_finalized_block_number(&self) -> ChainResult<u32> {
        read_tip(&self.client).await
    }

    async fn fetch_logs_by_tx_hash(
        &self,
        _tx_hash: H512,
    ) -> ChainResult<Vec<(Indexed<H256>, LogMeta)>> {
        // Midnight state reads are not addressable by tx hash.
        Ok(Vec::new())
    }
}

#[async_trait]
impl SequenceAwareIndexer<H256> for MidnightDeliveryIndexer {
    async fn latest_sequence_count_and_tip(&self) -> ChainResult<(Option<u32>, u32)> {
        // Deliveries are an unordered set with no sequence; `None` signals the
        // rate-limited cursor that there is no sequence to track.
        let tip = read_tip(&self.client).await?;
        Ok((None, tip))
    }
}

//! Dispatch and delivery indexers for the Midnight Mailbox (#95).
//!
//! Both indexers replay the WarpRoute contract's `HYP_*` Misc events served
//! by the Midnight indexer's `contractEvents` query, over BLOCK ranges — the
//! same shape as the EVM event indexers (`IndexMode::Block` + block-range
//! `fetch_logs_in_range`):
//!
//!   - [`MidnightDispatchIndexer`] decodes `HYP_DISPATCH` payloads (the
//!     141-byte `HyperlaneMessage` wire form) into dispatched messages;
//!     `Indexed.sequence` is the nonce decoded from the message, and the
//!     sequence-aware cursor validates nonce contiguity across block ranges.
//!     `latest_sequence_count_and_tip` stays a cheap state read of the
//!     Mailbox `nonce` counter.
//!   - [`MidnightDeliveryIndexer`] decodes `HYP_PROCESS` payloads (the
//!     delivered message id) honoring the requested block range. Deliveries
//!     have no sequence, so `latest_sequence_count_and_tip` is `(None, tip)`
//!     and the framework drives it with the rate-limited (watermark) cursor,
//!     exactly like EVM deliveries.
//!
//! Each event carries its transaction + block metadata, so the emitted
//! [`LogMeta`] is real: block number/hash, tx hash (widened to `H512`), the
//! indexer-global tx id as the transaction index, and the chain-global event
//! id as the log index. This is what lets the scraper store Midnight logs
//! (it drops zero-tx-hash logs).

use std::ops::RangeInclusive;

use async_trait::async_trait;

use hyperlane_core::{
    ChainResult, ContractLocator, HyperlaneMessage, Indexed, Indexer, LogMeta,
    SequenceAwareIndexer, H256, H512,
};

use crate::events::{
    address_hex, decode_dispatch_event, decode_process_event, event_log_meta, h512_to_h256,
    has_name, MidnightEventReader, HYP_DISPATCH, HYP_PROCESS,
};
use crate::indexer_client::MiscEvent;
use crate::MidnightIndexerClient;

/// Indexer that serves dispatched `HyperlaneMessage`s from the Midnight
/// Mailbox's `HYP_DISPATCH` events.
#[derive(Debug, Clone)]
pub struct MidnightDispatchIndexer {
    client: MidnightIndexerClient,
    /// WarpRoute (Mailbox) contract address; both the GraphQL event-filter key
    /// and the `LogMeta.address` reported for every dispatched message.
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

/// Decode the `HYP_DISPATCH` events out of a Misc-event batch. Non-dispatch
/// events (gas payments from a shared-contract deployment, future kinds) are
/// skipped; a dispatch event that fails to decode is an error, not a skip —
/// a malformed dispatch would otherwise silently create a nonce gap that
/// stalls the sequence-aware cursor with no diagnostic.
fn dispatch_logs_from_events(
    events: &[MiscEvent],
    address: H256,
) -> ChainResult<Vec<(Indexed<HyperlaneMessage>, LogMeta)>> {
    events
        .iter()
        .filter(|event| has_name(event, HYP_DISPATCH))
        .map(|event| {
            let message = decode_dispatch_event(event)?;
            // `Indexed::from(HyperlaneMessage)` sets `sequence` to the message
            // nonce, which is exactly the dispatch sequence the cursor keys on.
            Ok((Indexed::from(message), event_log_meta(event, address)))
        })
        .collect()
}

/// Serve a block `range` of dispatches. Generic over the event reader so unit
/// tests can drive the indexer logic with synthetic in-memory events;
/// production uses [`MidnightIndexerClient`].
async fn fetch_dispatch_logs<R: MidnightEventReader>(
    reader: &R,
    address: H256,
    range: RangeInclusive<u32>,
) -> ChainResult<Vec<(Indexed<HyperlaneMessage>, LogMeta)>> {
    let events = reader
        .misc_events(&address_hex(&address), *range.start(), *range.end())
        .await?;
    dispatch_logs_from_events(&events, address)
}

/// Serve the dispatches emitted by one transaction. The framework hands an
/// `H512`; a hash whose upper half is non-zero cannot be a Midnight tx hash,
/// so it matches nothing.
async fn fetch_dispatch_logs_by_tx<R: MidnightEventReader>(
    reader: &R,
    address: H256,
    tx_hash: H512,
) -> ChainResult<Vec<(Indexed<HyperlaneMessage>, LogMeta)>> {
    let Some(tx_hash) = h512_to_h256(tx_hash) else {
        return Ok(Vec::new());
    };
    let events = reader
        .misc_events_by_tx(&address_hex(&address), &tx_hash)
        .await?;
    dispatch_logs_from_events(&events, address)
}

#[async_trait]
impl Indexer<HyperlaneMessage> for MidnightDispatchIndexer {
    async fn fetch_logs_in_range(
        &self,
        range: RangeInclusive<u32>,
    ) -> ChainResult<Vec<(Indexed<HyperlaneMessage>, LogMeta)>> {
        fetch_dispatch_logs(&self.client, self.address, range).await
    }

    async fn get_finalized_block_number(&self) -> ChainResult<u32> {
        self.client.read_tip().await
    }

    async fn fetch_logs_by_tx_hash(
        &self,
        tx_hash: H512,
    ) -> ChainResult<Vec<(Indexed<HyperlaneMessage>, LogMeta)>> {
        fetch_dispatch_logs_by_tx(&self.client, self.address, tx_hash).await
    }
}

#[async_trait]
impl SequenceAwareIndexer<HyperlaneMessage> for MidnightDispatchIndexer {
    async fn latest_sequence_count_and_tip(&self) -> ChainResult<(Option<u32>, u32)> {
        // The Mailbox `nonce` counter is the dispatch count — a cheap state
        // read (one fetch, one counter decode), kept alongside the
        // event-based log fetch the way EVM reads `mailbox.nonce()`.
        let tip = self.client.read_tip().await?;
        let count = self
            .client
            .read_nonce_count(&address_hex(&self.address))
            .await?;
        Ok((Some(count), tip))
    }
}

/// Indexer that serves delivered message ids from the Midnight Mailbox's
/// `HYP_PROCESS` events.
#[derive(Debug, Clone)]
pub struct MidnightDeliveryIndexer {
    client: MidnightIndexerClient,
    /// WarpRoute (Mailbox) contract address; both the GraphQL event-filter key
    /// and the `LogMeta.address` reported for every delivery.
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

/// Decode the `HYP_PROCESS` events out of a Misc-event batch. Deliveries
/// carry no sequence (`Indexed::new`), matching EVM.
fn delivery_logs_from_events(
    events: &[MiscEvent],
    address: H256,
) -> ChainResult<Vec<(Indexed<H256>, LogMeta)>> {
    events
        .iter()
        .filter(|event| has_name(event, HYP_PROCESS))
        .map(|event| {
            let id = decode_process_event(event)?;
            Ok((Indexed::new(id), event_log_meta(event, address)))
        })
        .collect()
}

/// Serve a block `range` of deliveries. Generic over the event reader for the
/// same unit-test reason as [`fetch_dispatch_logs`].
async fn fetch_delivery_logs<R: MidnightEventReader>(
    reader: &R,
    address: H256,
    range: RangeInclusive<u32>,
) -> ChainResult<Vec<(Indexed<H256>, LogMeta)>> {
    let events = reader
        .misc_events(&address_hex(&address), *range.start(), *range.end())
        .await?;
    delivery_logs_from_events(&events, address)
}

#[async_trait]
impl Indexer<H256> for MidnightDeliveryIndexer {
    async fn fetch_logs_in_range(
        &self,
        range: RangeInclusive<u32>,
    ) -> ChainResult<Vec<(Indexed<H256>, LogMeta)>> {
        // The rate-limited (watermark) cursor walks block ranges and expects
        // the fetch to honor them; the `fromBlock`/`toBlock` filter does.
        fetch_delivery_logs(&self.client, self.address, range).await
    }

    async fn get_finalized_block_number(&self) -> ChainResult<u32> {
        self.client.read_tip().await
    }

    async fn fetch_logs_by_tx_hash(
        &self,
        tx_hash: H512,
    ) -> ChainResult<Vec<(Indexed<H256>, LogMeta)>> {
        let Some(tx_hash) = h512_to_h256(tx_hash) else {
            return Ok(Vec::new());
        };
        let events = self
            .client
            .misc_events_by_tx(&address_hex(&self.address), &tx_hash)
            .await?;
        delivery_logs_from_events(&events, self.address)
    }
}

#[async_trait]
impl SequenceAwareIndexer<H256> for MidnightDeliveryIndexer {
    async fn latest_sequence_count_and_tip(&self) -> ChainResult<(Option<u32>, u32)> {
        // Deliveries have no sequence; `(None, tip)` matches the EVM mailbox
        // delivery indexer and keeps the watermark cursor model.
        let tip = self.client.read_tip().await?;
        Ok((None, tip))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use hyperlane_core::{Encode as _, U256};

    use crate::events::test_util::{misc_event, FakeEventReader};
    use crate::events::HYP_GAS_PAYMENT;

    const ADDRESS: H256 = H256::repeat_byte(0x42);
    const TIP: u32 = 2000;

    fn message(nonce: u32) -> HyperlaneMessage {
        HyperlaneMessage {
            version: 3,
            nonce,
            origin: 1,
            sender: H256::repeat_byte(0xAA),
            destination: 2,
            recipient: H256::repeat_byte(0xBB),
            // The contract's wire form is a fixed Bytes<141>, i.e. a 64-byte
            // body. The zero tail also pins the fixed-offset payload slicing
            // (the padding past 141 is not part of the message).
            body: {
                let mut b = vec![0u8; 64];
                b[0] = nonce as u8;
                b
            },
        }
    }

    /// A dispatch event at block `1000 + id` (see `misc_event`).
    fn dispatch_event(id: u64, nonce: u32) -> MiscEvent {
        misc_event(id, HYP_DISPATCH, &message(nonce).to_vec())
    }

    #[tokio::test]
    async fn dispatch_decodes_events_and_sets_sequence_from_nonce() {
        let reader = FakeEventReader {
            tip: TIP,
            events: vec![
                dispatch_event(1, 0),
                // Foreign event kind interleaved in the same range: skipped.
                misc_event(2, HYP_GAS_PAYMENT, &[0u8; 60]),
                dispatch_event(3, 1),
            ],
        };

        let logs = fetch_dispatch_logs(&reader, ADDRESS, 0..=TIP)
            .await
            .expect("fetch dispatch logs");

        assert_eq!(logs.len(), 2, "only HYP_DISPATCH events are served");
        assert_eq!(logs[0].0.inner().nonce, 0);
        assert_eq!(logs[0].0.sequence, Some(0), "sequence == decoded nonce");
        assert_eq!(logs[1].0.inner().nonce, 1);
        assert_eq!(logs[1].0.sequence, Some(1));
        assert_eq!(logs[0].0.inner(), &message(0), "wire form round-trips");
    }

    #[tokio::test]
    async fn dispatch_honors_block_range() {
        // Events sit at blocks 1001 and 1003 (misc_event: 1000 + id).
        let reader = FakeEventReader {
            tip: TIP,
            events: vec![dispatch_event(1, 0), dispatch_event(3, 1)],
        };

        // A range covering only the first block yields only the first event.
        let logs = fetch_dispatch_logs(&reader, ADDRESS, 1000..=1002)
            .await
            .expect("fetch dispatch logs");
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].0.sequence, Some(0));

        // A disjoint range yields nothing.
        let logs = fetch_dispatch_logs(&reader, ADDRESS, 1004..=1100)
            .await
            .expect("fetch dispatch logs");
        assert!(logs.is_empty());
    }

    #[tokio::test]
    async fn dispatch_builds_real_log_meta() {
        let event = dispatch_event(5, 0);
        let reader = FakeEventReader {
            tip: TIP,
            events: vec![event.clone()],
        };

        let logs = fetch_dispatch_logs(&reader, ADDRESS, 0..=TIP)
            .await
            .expect("fetch dispatch logs");

        assert_eq!(logs.len(), 1);
        let meta = &logs[0].1;
        assert_eq!(meta.address, ADDRESS);
        assert_eq!(meta.block_number, event.block_height);
        assert_eq!(meta.block_hash, event.block_hash);
        // Real tx hash, H256 -> H512 right-aligned widening.
        assert_eq!(meta.transaction_id, H512::from(event.tx_hash));
        assert_eq!(meta.transaction_index, event.tx_id);
        assert_eq!(meta.log_index, U256::from(event.id));
    }

    #[tokio::test]
    async fn dispatch_fetch_by_tx_hash_filters_on_hash() {
        let wanted = dispatch_event(1, 0);
        let other = dispatch_event(2, 1);
        let reader = FakeEventReader {
            tip: TIP,
            events: vec![wanted.clone(), other],
        };

        // The transactionHash filter narrows to the one emitting tx.
        let logs = fetch_dispatch_logs_by_tx(&reader, ADDRESS, wanted.tx_hash.into())
            .await
            .expect("fetch by tx hash");
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].0.sequence, Some(0));
        assert_eq!(logs[0].1.transaction_id, H512::from(wanted.tx_hash));

        // A non-Midnight H512 (non-zero upper half) matches nothing.
        let mut foreign: H512 = wanted.tx_hash.into();
        foreign.0[0] = 1;
        let logs = fetch_dispatch_logs_by_tx(&reader, ADDRESS, foreign)
            .await
            .expect("fetch by tx hash");
        assert!(logs.is_empty());
    }

    #[tokio::test]
    async fn malformed_dispatch_event_is_an_error_not_a_gap() {
        // A HYP_DISPATCH payload that is NOT a decodable message (wrong
        // version byte is fine — read_from doesn't validate version; instead
        // give it a short payload) must fail the whole fetch loudly.
        let mut bad = misc_event(1, HYP_DISPATCH, &[0xFFu8; 16]);
        bad.payload.truncate(16);
        let err = dispatch_logs_from_events(&[bad], ADDRESS);
        assert!(err.is_err(), "short dispatch payload must error");
    }

    #[tokio::test]
    async fn delivery_decodes_process_events_in_range() {
        let id_a = H256::repeat_byte(0x01);
        let id_b = H256::repeat_byte(0x02);
        let reader = FakeEventReader {
            tip: TIP,
            events: vec![
                misc_event(1, HYP_PROCESS, id_a.as_bytes()),
                // A dispatch in the same range is not a delivery.
                dispatch_event(2, 0),
                misc_event(3, HYP_PROCESS, id_b.as_bytes()),
            ],
        };

        let logs = fetch_delivery_logs(&reader, ADDRESS, 0..=TIP)
            .await
            .expect("fetch delivery logs");

        assert_eq!(logs.len(), 2);
        assert_eq!(*logs[0].0.inner(), id_a);
        assert_eq!(*logs[1].0.inner(), id_b);
        // Deliveries have no sequence (EVM parity).
        assert!(logs.iter().all(|(indexed, _)| indexed.sequence.is_none()));
        // Real per-event meta.
        assert_eq!(logs[0].1.block_number, 1001);
        assert_eq!(logs[1].1.block_number, 1003);

        // The range is honored: a window over only the second delivery.
        let logs = fetch_delivery_logs(&reader, ADDRESS, 1003..=1003)
            .await
            .expect("fetch delivery logs");
        assert_eq!(logs.len(), 1);
        assert_eq!(*logs[0].0.inner(), id_b);
    }
}

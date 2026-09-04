//! Dispatch and delivery indexers for the Midnight Mailbox.
//!
//! Both replay the WarpRoute contract's `HYP_*` events over block ranges, the
//! same shape as the EVM event indexers. Dispatches carry the message nonce as
//! their sequence; deliveries have none and run off the watermark cursor.

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

/// Serves dispatched messages from the Mailbox's `HYP_DISPATCH` events.
#[derive(Debug, Clone)]
pub struct MidnightDispatchIndexer {
    client: MidnightIndexerClient,
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

/// A dispatch that fails to decode is an error rather than a skip: skipping it
/// would leave a nonce gap that stalls the cursor with no diagnostic.
fn dispatch_logs_from_events(
    events: &[MiscEvent],
    address: H256,
) -> ChainResult<Vec<(Indexed<HyperlaneMessage>, LogMeta)>> {
    events
        .iter()
        .filter(|event| has_name(event, HYP_DISPATCH))
        .map(|event| {
            let message = decode_dispatch_event(event)?;
            // `Indexed::from` sets `sequence` to the message nonce, which is
            // the dispatch sequence the cursor keys on.
            Ok((Indexed::from(message), event_log_meta(event, address)))
        })
        .collect()
}

/// Generic over the event reader so unit tests can drive this without network IO.
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
        // The Mailbox `nonce` counter is the dispatch count.
        let tip = self.client.read_tip().await?;
        let count = self
            .client
            .read_nonce_count(&address_hex(&self.address))
            .await?;
        Ok((Some(count), tip))
    }
}

/// Serves delivered message ids from the Mailbox's `HYP_PROCESS` events.
#[derive(Debug, Clone)]
pub struct MidnightDeliveryIndexer {
    client: MidnightIndexerClient,
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
            body: {
                let mut b = vec![0u8; 64];
                b[0] = nonce as u8;
                b
            },
        }
    }

    fn dispatch_event(id: u64, nonce: u32) -> MiscEvent {
        misc_event(id, HYP_DISPATCH, &message(nonce).to_vec())
    }

    #[tokio::test]
    async fn dispatch_decodes_events_and_sets_sequence_from_nonce() {
        let reader = FakeEventReader {
            tip: TIP,
            events: vec![
                dispatch_event(1, 0),
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
        // These sit at blocks 1001 and 1003.
        let reader = FakeEventReader {
            tip: TIP,
            events: vec![dispatch_event(1, 0), dispatch_event(3, 1)],
        };

        let logs = fetch_dispatch_logs(&reader, ADDRESS, 1000..=1002)
            .await
            .expect("fetch dispatch logs");
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].0.sequence, Some(0));

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

        let logs = fetch_dispatch_logs_by_tx(&reader, ADDRESS, wanted.tx_hash.into())
            .await
            .expect("fetch by tx hash");
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].0.sequence, Some(0));
        assert_eq!(logs[0].1.transaction_id, H512::from(wanted.tx_hash));

        let mut foreign: H512 = wanted.tx_hash.into();
        foreign.0[0] = 1;
        let logs = fetch_dispatch_logs_by_tx(&reader, ADDRESS, foreign)
            .await
            .expect("fetch by tx hash");
        assert!(logs.is_empty());
    }

    #[tokio::test]
    async fn malformed_dispatch_event_is_an_error_not_a_gap() {
        // A short payload, since `read_from` does not validate the version
        // byte. It must fail the whole fetch rather than skip the event.
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
        assert!(logs.iter().all(|(indexed, _)| indexed.sequence.is_none()));
        assert_eq!(logs[0].1.block_number, 1001);
        assert_eq!(logs[1].1.block_number, 1003);

        let logs = fetch_delivery_logs(&reader, ADDRESS, 1003..=1003)
            .await
            .expect("fetch delivery logs");
        assert_eq!(logs.len(), 1);
        assert_eq!(*logs[0].0.inner(), id_b);
    }
}

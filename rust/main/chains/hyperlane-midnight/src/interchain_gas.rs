//! `InterchainGasPaymaster` + IGP payment indexer for Midnight.
//!
//! The IGP emits one `HYP_GAS_PAYMENT` event per `payForGas`, atomically with
//! the state write, and this replays them over block ranges. Payments carry no
//! sequence, so the watermark cursor drives it.

use std::ops::RangeInclusive;

use async_trait::async_trait;

use hyperlane_core::{
    ChainResult, HyperlaneChain, HyperlaneContract, HyperlaneDomain, HyperlaneProvider, Indexed,
    Indexer, InterchainGasPaymaster, InterchainGasPayment, LogMeta, SequenceAwareIndexer, H256,
    H512,
};

use crate::events::{
    address_hex, decode_gas_payment_event, event_log_meta, h512_to_h256, has_name,
    MidnightEventReader, HYP_GAS_PAYMENT,
};
use crate::indexer_client::MiscEvent;
use crate::MidnightProvider;

/// A payment that fails to decode is an error rather than a skip.
fn igp_logs_from_events(
    events: &[MiscEvent],
    address: H256,
) -> ChainResult<Vec<(Indexed<InterchainGasPayment>, LogMeta)>> {
    events
        .iter()
        .filter(|event| has_name(event, HYP_GAS_PAYMENT))
        .map(|event| {
            let payment = decode_gas_payment_event(event)?;
            Ok((Indexed::new(payment), event_log_meta(event, address)))
        })
        .collect()
}

/// Generic over the event reader so unit tests can drive this without network IO.
async fn fetch_igp_logs<R: MidnightEventReader>(
    reader: &R,
    address: H256,
    range: RangeInclusive<u32>,
) -> ChainResult<Vec<(Indexed<InterchainGasPayment>, LogMeta)>> {
    let events = reader
        .misc_events(&address_hex(&address), *range.start(), *range.end())
        .await?;
    igp_logs_from_events(&events, address)
}

async fn fetch_igp_logs_by_tx<R: MidnightEventReader>(
    reader: &R,
    address: H256,
    tx_hash: H512,
) -> ChainResult<Vec<(Indexed<InterchainGasPayment>, LogMeta)>> {
    let Some(tx_hash) = h512_to_h256(tx_hash) else {
        return Ok(Vec::new());
    };
    let events = reader
        .misc_events_by_tx(&address_hex(&address), &tx_hash)
        .await?;
    igp_logs_from_events(&events, address)
}

/// `InterchainGasPaymaster` + IGP payment indexer for Midnight.
#[derive(Debug, Clone)]
pub struct MidnightInterchainGasPaymaster {
    address: H256,
    domain: HyperlaneDomain,
    provider: MidnightProvider,
}

impl MidnightInterchainGasPaymaster {
    /// Construct a handle to the IGP contract at `address`.
    pub fn new(address: H256, domain: HyperlaneDomain, provider: MidnightProvider) -> Self {
        Self {
            address,
            domain,
            provider,
        }
    }
}

impl HyperlaneChain for MidnightInterchainGasPaymaster {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(self.provider.clone())
    }
}

impl HyperlaneContract for MidnightInterchainGasPaymaster {
    fn address(&self) -> H256 {
        self.address
    }
}

impl InterchainGasPaymaster for MidnightInterchainGasPaymaster {}

#[async_trait]
impl Indexer<InterchainGasPayment> for MidnightInterchainGasPaymaster {
    async fn fetch_logs_in_range(
        &self,
        range: RangeInclusive<u32>,
    ) -> ChainResult<Vec<(Indexed<InterchainGasPayment>, LogMeta)>> {
        fetch_igp_logs(self.provider.indexer(), self.address, range).await
    }

    async fn get_finalized_block_number(&self) -> ChainResult<u32> {
        // Midnight has BFT finality and the indexer serves only finalized
        // blocks, so the latest height is the finalized height.
        self.provider.indexer().read_tip().await
    }

    async fn fetch_logs_by_tx_hash(
        &self,
        tx_hash: H512,
    ) -> ChainResult<Vec<(Indexed<InterchainGasPayment>, LogMeta)>> {
        fetch_igp_logs_by_tx(self.provider.indexer(), self.address, tx_hash).await
    }
}

#[async_trait]
impl SequenceAwareIndexer<InterchainGasPayment> for MidnightInterchainGasPaymaster {
    async fn latest_sequence_count_and_tip(&self) -> ChainResult<(Option<u32>, u32)> {
        let tip = self.provider.indexer().read_tip().await?;
        Ok((None, tip))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use hyperlane_core::U256;

    use crate::events::test_util::{misc_event, FakeEventReader};
    use crate::events::HYP_DISPATCH;

    const ADDRESS: H256 = H256::repeat_byte(0x42);
    const TIP: u32 = 2000;

    /// All four record fields are derived from `seed`.
    fn payment_event(id: u64, seed: u8) -> MiscEvent {
        let mut content = Vec::with_capacity(60);
        content.extend_from_slice(H256::repeat_byte(seed).as_bytes());
        content.extend_from_slice(&(1000u32 + seed as u32).to_be_bytes());
        content.extend_from_slice(&(100_000u64 + seed as u64).to_be_bytes());
        content.extend_from_slice(&(2_000_000_000_000_000u128 + seed as u128).to_be_bytes());
        misc_event(id, HYP_GAS_PAYMENT, &content)
    }

    #[tokio::test]
    async fn decodes_payment_events_without_sequence() {
        let reader = FakeEventReader {
            tip: TIP,
            events: vec![
                payment_event(1, 0),
                misc_event(2, HYP_DISPATCH, &[0u8; 141]),
                payment_event(3, 7),
            ],
        };

        let logs = fetch_igp_logs(&reader, ADDRESS, 0..=TIP)
            .await
            .expect("fetch igp logs");

        assert_eq!(logs.len(), 2, "only HYP_GAS_PAYMENT events are served");
        assert!(logs.iter().all(|(indexed, _)| indexed.sequence.is_none()));

        let p = logs[1].0.inner();
        assert_eq!(p.message_id, H256::repeat_byte(7));
        assert_eq!(p.destination, 1007);
        assert_eq!(p.gas_amount, U256::from(100_007u64));
        assert_eq!(p.payment, U256::from(2_000_000_000_000_007u128));
    }

    #[tokio::test]
    async fn honors_block_range() {
        // These sit at blocks 1001 and 1003.
        let reader = FakeEventReader {
            tip: TIP,
            events: vec![payment_event(1, 0), payment_event(3, 1)],
        };

        let logs = fetch_igp_logs(&reader, ADDRESS, 1002..=1004)
            .await
            .expect("fetch igp logs");
        assert_eq!(logs.len(), 1, "only the in-range payment is served");
        assert_eq!(logs[0].0.inner().message_id, H256::repeat_byte(1));
    }

    #[tokio::test]
    async fn builds_real_log_meta() {
        let event = payment_event(5, 3);
        let reader = FakeEventReader {
            tip: TIP,
            events: vec![event.clone()],
        };

        let logs = fetch_igp_logs(&reader, ADDRESS, 0..=TIP)
            .await
            .expect("fetch igp logs");

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
    async fn fetch_by_tx_hash_filters_on_hash() {
        let wanted = payment_event(1, 0);
        let other = payment_event(2, 1);
        let reader = FakeEventReader {
            tip: TIP,
            events: vec![wanted.clone(), other],
        };

        let logs = fetch_igp_logs_by_tx(&reader, ADDRESS, wanted.tx_hash.into())
            .await
            .expect("fetch by tx hash");
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].0.inner().message_id, H256::repeat_byte(0));

        let mut foreign: H512 = wanted.tx_hash.into();
        foreign.0[0] = 1;
        let logs = fetch_igp_logs_by_tx(&reader, ADDRESS, foreign)
            .await
            .expect("fetch by tx hash");
        assert!(logs.is_empty());
    }

    #[tokio::test]
    async fn malformed_payment_event_is_an_error_not_a_skip() {
        let mut bad = payment_event(1, 0);
        bad.payload.truncate(10);
        assert!(igp_logs_from_events(&[bad], ADDRESS).is_err());
    }
}

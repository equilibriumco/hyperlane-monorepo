//! `InterchainGasPaymaster` + IGP payment indexer for Midnight (#95).
//!
//! The IGP contract emits one `HYP_GAS_PAYMENT` Misc event per `payForGas`,
//! atomically with the state write. This indexer replays those events over
//! BLOCK ranges through the Midnight indexer's `contractEvents` query —
//! matching the EVM IGP indexer exactly:
//!
//!   - gas payments are NOT sequence-indexed: `Indexed::new(payment)` carries
//!     no sequence and `latest_sequence_count_and_tip` returns `(None, tip)`,
//!     so the framework drives this indexer with the rate-limited (watermark)
//!     cursor, the same as EVM (`Indexable for InterchainGasPayment`).
//!   - each event carries its transaction + block metadata, so the emitted
//!     [`LogMeta`] is real (block number/hash, tx hash, indexer-global tx id,
//!     chain-global event id).
//!
//! Following Aleo/Radix/CosmosNative, one struct serves both roles the
//! framework builds: the [`InterchainGasPaymaster`] marker (a boxed trait
//! object every configured chain must provide, though the relayer never calls
//! it) and the indexer that actually feeds payments into the relayer's DB.
//!
//! The relayer's gas-payment *enforcement* (the None / Minimum /
//! OnChainFeeQuoting policies + matching lists) is entirely chain-agnostic and
//! lives in `agents/relayer/src/msg/gas_payment/`: it reads the
//! `InterchainGasPayment`s this indexer wrote into RocksDB and never touches
//! the chain crate. So this indexer only needs to produce the same
//! `InterchainGasPayment` data the EVM event indexer produces — the policies
//! work unchanged.

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

/// Decode the `HYP_GAS_PAYMENT` events out of a Misc-event batch. Non-payment
/// events (dispatches from a shared-contract deployment, future kinds) are
/// skipped; a payment event that fails to decode is an error, not a skip.
/// No sequence is attached — gas payments are not sequence-indexed (EVM
/// parity; the watermark cursor tracks progress by block, not sequence).
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

/// Serve a block `range` of gas payments. Generic over the event reader so
/// unit tests can drive the indexer logic with synthetic in-memory events;
/// production uses the provider's [`crate::MidnightIndexerClient`].
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

/// Serve the gas payments emitted by one transaction (the `transactionHash`
/// filter); the relayer broadcasts dispatch txids to the IGP sync task.
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

/// Chain-sourced `InterchainGasPaymaster` + IGP payment indexer for Midnight.
/// Replays the IGP contract's `HYP_GAS_PAYMENT` events via the Midnight
/// indexer's `contractEvents` query.
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
    /// Midnight indexes gas payments over block ranges (`IndexMode::Block` +
    /// the rate-limited cursor, EVM parity), so `range` is a block range and
    /// the `fromBlock`/`toBlock` event filter honors it.
    async fn fetch_logs_in_range(
        &self,
        range: RangeInclusive<u32>,
    ) -> ChainResult<Vec<(Indexed<InterchainGasPayment>, LogMeta)>> {
        fetch_igp_logs(self.provider.indexer(), self.address, range).await
    }

    async fn get_finalized_block_number(&self) -> ChainResult<u32> {
        // Midnight has BFT finality and the indexer exposes only finalized
        // blocks, so the latest height is the finalized height. The watermark
        // cursor reads this as its tip.
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
        // Gas payments are not sequence-indexed; `(None, tip)` matches the
        // EVM IGP indexer (the rate-limited cursor never reads the count).
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

    /// A `HYP_GAS_PAYMENT` event at block `1000 + id` (see `misc_event`),
    /// with all four record fields derived from `seed`.
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
                // A dispatch interleaved in the same range: not a payment.
                misc_event(2, HYP_DISPATCH, &[0u8; 141]),
                payment_event(3, 7),
            ],
        };

        let logs = fetch_igp_logs(&reader, ADDRESS, 0..=TIP)
            .await
            .expect("fetch igp logs");

        assert_eq!(logs.len(), 2, "only HYP_GAS_PAYMENT events are served");
        // EVM parity: no sequence on gas payments.
        assert!(logs.iter().all(|(indexed, _)| indexed.sequence.is_none()));

        let p = logs[1].0.inner();
        assert_eq!(p.message_id, H256::repeat_byte(7));
        assert_eq!(p.destination, 1007);
        assert_eq!(p.gas_amount, U256::from(100_007u64));
        assert_eq!(p.payment, U256::from(2_000_000_000_000_007u128));
    }

    #[tokio::test]
    async fn honors_block_range() {
        // Events sit at blocks 1001 and 1003 (misc_event: 1000 + id).
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

        // A non-Midnight H512 (non-zero upper half) matches nothing.
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

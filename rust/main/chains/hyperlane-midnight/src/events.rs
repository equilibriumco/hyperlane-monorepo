//! Decoding of the WarpRoute/IGP contracts' `HYP_*` Misc events into
//! Hyperlane domain types, plus the shared [`LogMeta`] mapping and the
//! event-reader seam the dispatch/delivery/IGP/merkle indexers share.
//!
//! The contracts emit one Misc event per state transition, atomically with
//! the corresponding ledger write:
//!
//!   - `HYP_DISPATCH`: payload `[0..141)` is the encoded `HyperlaneMessage`
//!     wire form (the exact bytes `HyperlaneMessage::read_from` decodes;
//!     the nonce is inside it).
//!   - `HYP_PROCESS`: payload `[0..32)` is the delivered message id.
//!   - `HYP_GAS_PAYMENT`: payload `[0..32)` messageId, `[32..36)`
//!     destination u32 BE, `[36..44)` gasAmount u64 BE, `[44..60)` payment
//!     u128 BE.
//!
//! Payloads are zero-padded to 256 bytes by the indexer, so decoding is a
//! fixed-offset slice. The standard `Paused`/`Unpaused` ledger events have no
//! Hyperlane consumer and are filtered out server-side (`types: [MISC]`).

use async_trait::async_trait;

use hyperlane_core::{
    ChainResult, Decode as _, HyperlaneMessage, InterchainGasPayment, LogMeta, H256, H512, U256,
};

use crate::indexer_client::{MidnightIndexerClient, MiscEvent};
use crate::state_decode::ENCODED_MESSAGE_LEN;
use crate::HyperlaneMidnightError;

/// Event name for a Mailbox dispatch.
pub(crate) const HYP_DISPATCH: &str = "HYP_DISPATCH";
/// Event name for a Mailbox delivery.
pub(crate) const HYP_PROCESS: &str = "HYP_PROCESS";
/// Event name for an IGP gas payment.
pub(crate) const HYP_GAS_PAYMENT: &str = "HYP_GAS_PAYMENT";

/// Length of a `HYP_PROCESS` payload's message id.
const PROCESS_PAYLOAD_LEN: usize = 32;
/// Length of a `HYP_GAS_PAYMENT` payload record.
const GAS_PAYMENT_PAYLOAD_LEN: usize = 60;

/// Whether `event` carries the given ASCII name (zero-padded to the 32-byte
/// wire width). A name that merely shares the prefix (e.g. a hypothetical
/// `HYP_DISPATCH2`) does NOT match: the padding must be all zeros.
pub(crate) fn has_name(event: &MiscEvent, name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.len() > event.name.len() {
        return false;
    }
    event.name[..bytes.len()] == *bytes && event.name[bytes.len()..].iter().all(|b| *b == 0)
}

/// Build the real per-event [`LogMeta`]:
///
///   - `block_number`/`block_hash`: the block the emitting tx landed in;
///   - `transaction_id`: the tx hash, widened `H256` -> `H512` (right-aligned,
///     as other non-EVM chains do);
///   - `transaction_index`: the indexer-global transaction `id` (monotonic
///     integer, not a per-block index — Midnight's stable per-tx ordinal);
///   - `log_index`: the chain-global monotonic event `id` (sparse per
///     contract; other event kinds share the sequence).
pub(crate) fn event_log_meta(event: &MiscEvent, address: H256) -> LogMeta {
    LogMeta {
        address,
        block_number: event.block_height,
        block_hash: event.block_hash,
        transaction_id: event.tx_hash.into(),
        transaction_index: event.tx_id,
        log_index: U256::from(event.id),
    }
}

/// Decode a `HYP_DISPATCH` payload into the dispatched [`HyperlaneMessage`].
/// The wire form is exactly [`ENCODED_MESSAGE_LEN`] bytes, so the slice must
/// be exact: `read_from` treats everything after the fixed header as the
/// body, and the zero padding beyond 141 is NOT part of the message.
pub(crate) fn decode_dispatch_event(event: &MiscEvent) -> ChainResult<HyperlaneMessage> {
    let payload = payload_slice(event, ENCODED_MESSAGE_LEN, HYP_DISPATCH)?;
    HyperlaneMessage::read_from(&mut &payload[..])
        .map_err(|e| HyperlaneMidnightError::StateDecode(e.to_string()).into())
}

/// Decode a `HYP_PROCESS` payload into the delivered message id.
pub(crate) fn decode_process_event(event: &MiscEvent) -> ChainResult<H256> {
    let payload = payload_slice(event, PROCESS_PAYLOAD_LEN, HYP_PROCESS)?;
    Ok(H256::from_slice(payload))
}

/// Decode a `HYP_GAS_PAYMENT` payload into an [`InterchainGasPayment`]
/// (big-endian fields, mirroring `HyperlaneEvents.compact`'s u32BE/u64BE/
/// u128BE encoders).
pub(crate) fn decode_gas_payment_event(event: &MiscEvent) -> ChainResult<InterchainGasPayment> {
    let payload = payload_slice(event, GAS_PAYMENT_PAYLOAD_LEN, HYP_GAS_PAYMENT)?;
    let destination =
        u32::from_be_bytes(payload[32..36].try_into().expect("fixed 4-byte slice; qed"));
    let gas_amount =
        u64::from_be_bytes(payload[36..44].try_into().expect("fixed 8-byte slice; qed"));
    let payment = u128::from_be_bytes(
        payload[44..60]
            .try_into()
            .expect("fixed 16-byte slice; qed"),
    );
    Ok(InterchainGasPayment {
        message_id: H256::from_slice(&payload[..32]),
        destination,
        payment: U256::from(payment),
        gas_amount: U256::from(gas_amount),
    })
}

/// The leading `len` bytes of the event's zero-padded payload. Errors on a
/// short payload (the indexer pads to 256, so this only fires on a malformed
/// response) instead of panicking on slice bounds.
fn payload_slice<'a>(event: &'a MiscEvent, len: usize, what: &str) -> ChainResult<&'a [u8]> {
    event.payload.get(..len).ok_or_else(|| {
        HyperlaneMidnightError::StateDecode(format!(
            "{what} payload is {} bytes, expected at least {len}",
            event.payload.len()
        ))
        .into()
    })
}

/// Narrow a framework `H512` tx hash to Midnight's 32-byte hash. Hyperlane
/// widens 32-byte hashes right-aligned (`From<H256> for H512`), so the upper
/// 32 bytes must be zero; anything else cannot be a Midnight tx hash.
pub(crate) fn h512_to_h256(hash: H512) -> Option<H256> {
    if hash[..32].iter().any(|b| *b != 0) {
        return None;
    }
    Some(H256::from(hash))
}

/// Render an `H256` contract address as the lowercase bare-hex string the
/// indexer's `HexEncoded` scalar accepts, matching the convention every
/// Midnight state/event read uses.
pub(crate) fn address_hex(address: &H256) -> String {
    format!("{address:x}")
}

/// Map an indexer-reported block height to a `u32` tip. The indexer reports a
/// `u64`; heights beyond `u32::MAX` saturate rather than truncate/wrap, and
/// "no block observed yet" (`None`) maps to `0`.
pub(crate) fn height_to_tip(height: Option<u64>) -> u32 {
    height
        .map(|h| u32::try_from(h).unwrap_or(u32::MAX))
        .unwrap_or(0)
}

/// The minimal reads the event-based indexers need from the Midnight indexer.
/// Abstracting them behind a trait lets the dispatch/delivery/IGP/merkle
/// indexer logic be unit-tested against synthetic in-memory events without
/// network IO; production uses the [`MidnightIndexerClient`] impl below.
#[async_trait]
pub(crate) trait MidnightEventReader: Send + Sync {
    /// Latest observed block height as a `u32` tip; heights beyond `u32::MAX`
    /// saturate, and "no block seen yet" maps to `0`.
    async fn read_tip(&self) -> ChainResult<u32>;
    /// Misc events the contract emitted in the inclusive block range, in
    /// monotonic event-id order, FAILURE transactions already excluded.
    async fn misc_events(
        &self,
        address: &str,
        from_block: u32,
        to_block: u32,
    ) -> ChainResult<Vec<MiscEvent>>;
    /// Misc events the contract emitted from the given transaction, FAILURE
    /// transactions already excluded.
    async fn misc_events_by_tx(&self, address: &str, tx_hash: &H256)
        -> ChainResult<Vec<MiscEvent>>;
}

#[async_trait]
impl MidnightEventReader for MidnightIndexerClient {
    async fn read_tip(&self) -> ChainResult<u32> {
        let height = self
            .latest_block_height()
            .await
            .map_err(Into::<hyperlane_core::ChainCommunicationError>::into)?;
        Ok(height_to_tip(height))
    }

    async fn misc_events(
        &self,
        address: &str,
        from_block: u32,
        to_block: u32,
    ) -> ChainResult<Vec<MiscEvent>> {
        self.misc_events_in_range(address, from_block, to_block)
            .await
            .map_err(Into::<hyperlane_core::ChainCommunicationError>::into)
    }

    async fn misc_events_by_tx(
        &self,
        address: &str,
        tx_hash: &H256,
    ) -> ChainResult<Vec<MiscEvent>> {
        self.misc_events_by_tx_hash(address, tx_hash)
            .await
            .map_err(Into::<hyperlane_core::ChainCommunicationError>::into)
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    use super::*;
    use crate::indexer_client::{TxStatus, MISC_NAME_LEN, MISC_PAYLOAD_LEN};

    /// Build a synthetic [`MiscEvent`] with the given name and payload
    /// content (zero-padded to the wire widths) and deterministic metadata
    /// derived from `id`.
    pub(crate) fn misc_event(id: u64, name: &str, payload_content: &[u8]) -> MiscEvent {
        let mut name_bytes = [0u8; MISC_NAME_LEN];
        name_bytes[..name.len()].copy_from_slice(name.as_bytes());
        let mut payload = vec![0u8; MISC_PAYLOAD_LEN];
        payload[..payload_content.len()].copy_from_slice(payload_content);
        MiscEvent {
            name: name_bytes,
            payload,
            id,
            tx_hash: H256::repeat_byte(id as u8),
            tx_id: 100 + id,
            block_hash: H256::repeat_byte(0xb0 ^ id as u8),
            block_height: 1000 + id,
            block_timestamp_ms: 1_700_000_000_000 + id,
            tx_status: TxStatus::Success,
        }
    }

    /// A synthetic in-memory event reader: the unit-test seam that lets the
    /// indexer logic run without network IO. `misc_events` honors the block
    /// range against each event's `block_height`, mirroring the server-side
    /// `fromBlock`/`toBlock` filter; `misc_events_by_tx` honors the tx hash.
    pub(crate) struct FakeEventReader {
        pub tip: u32,
        pub events: Vec<MiscEvent>,
    }

    #[async_trait]
    impl MidnightEventReader for FakeEventReader {
        async fn read_tip(&self) -> ChainResult<u32> {
            Ok(self.tip)
        }
        async fn misc_events(
            &self,
            _address: &str,
            from_block: u32,
            to_block: u32,
        ) -> ChainResult<Vec<MiscEvent>> {
            Ok(self
                .events
                .iter()
                .filter(|e| {
                    e.block_height >= u64::from(from_block) && e.block_height <= u64::from(to_block)
                })
                .cloned()
                .collect())
        }
        async fn misc_events_by_tx(
            &self,
            _address: &str,
            tx_hash: &H256,
        ) -> ChainResult<Vec<MiscEvent>> {
            Ok(self
                .events
                .iter()
                .filter(|e| e.tx_hash == *tx_hash)
                .cloned()
                .collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::misc_event;
    use super::*;
    use hyperlane_core::Encode as _;

    fn sample_message(nonce: u32) -> HyperlaneMessage {
        HyperlaneMessage {
            version: 3,
            nonce,
            origin: 1234,
            sender: H256::repeat_byte(0xAA),
            destination: 99,
            recipient: H256::repeat_byte(0xBB),
            // 64-byte body ending in zeros: the message wire form ends in
            // 0x00 bytes, pinning that the decoder slices the fixed 141-byte
            // offset out of the zero-padded payload rather than trimming.
            body: {
                let mut b = vec![0u8; 64];
                b[0] = 0x11;
                b[1] = 0x22;
                b
            },
        }
    }

    #[test]
    fn name_matching_requires_zero_padding() {
        let event = misc_event(1, HYP_DISPATCH, &[]);
        assert!(has_name(&event, HYP_DISPATCH));
        assert!(!has_name(&event, HYP_PROCESS));
        // A shared prefix does not match: the tail must be all zeros.
        assert!(!has_name(&event, "HYP_"));

        // A name with a non-zero byte after the ASCII prefix is different.
        let mut noisy = misc_event(2, HYP_DISPATCH, &[]);
        noisy.name[HYP_DISPATCH.len()] = 0x01;
        assert!(!has_name(&noisy, HYP_DISPATCH));
    }

    #[test]
    fn decodes_dispatch_event_with_zero_tail_body() {
        let message = sample_message(7);
        let wire = message.to_vec();
        assert_eq!(wire.len(), ENCODED_MESSAGE_LEN);
        // The wire form ends in 0x00 (zero body tail) and the payload pads
        // with further zeros; only the exact 141-byte slice decodes right.
        assert_eq!(*wire.last().expect("non-empty"), 0);

        let event = misc_event(1, HYP_DISPATCH, &wire);
        let decoded = decode_dispatch_event(&event).expect("decode dispatch");
        assert_eq!(decoded, message);
        assert_eq!(decoded.id(), message.id(), "keccak id round-trips");
    }

    #[test]
    fn decodes_process_event() {
        let id = H256::repeat_byte(0xDD);
        let event = misc_event(2, HYP_PROCESS, id.as_bytes());
        assert_eq!(decode_process_event(&event).expect("decode"), id);
    }

    #[test]
    fn decodes_gas_payment_event_big_endian() {
        let mut content = Vec::with_capacity(60);
        content.extend_from_slice(H256::repeat_byte(0xCC).as_bytes());
        content.extend_from_slice(&99u32.to_be_bytes());
        content.extend_from_slice(&100_000u64.to_be_bytes());
        content.extend_from_slice(&2_000_000_000_000_000u128.to_be_bytes());

        let event = misc_event(3, HYP_GAS_PAYMENT, &content);
        let payment = decode_gas_payment_event(&event).expect("decode");
        assert_eq!(payment.message_id, H256::repeat_byte(0xCC));
        assert_eq!(payment.destination, 99);
        assert_eq!(payment.gas_amount, U256::from(100_000u64));
        assert_eq!(payment.payment, U256::from(2_000_000_000_000_000u128));
    }

    #[test]
    fn log_meta_maps_every_field() {
        let address = H256::repeat_byte(0x42);
        let event = misc_event(9, HYP_DISPATCH, &[]);
        let meta = event_log_meta(&event, address);

        assert_eq!(meta.address, address);
        assert_eq!(meta.block_number, event.block_height);
        assert_eq!(meta.block_hash, event.block_hash);
        // H256 -> H512 widening is right-aligned, so it round-trips back.
        assert_eq!(meta.transaction_id, H512::from(event.tx_hash));
        assert_eq!(H256::from(meta.transaction_id), event.tx_hash);
        assert_eq!(meta.transaction_index, event.tx_id);
        assert_eq!(meta.log_index, U256::from(event.id));
    }

    #[test]
    fn h512_narrowing_requires_zero_upper_half() {
        let h256 = H256::repeat_byte(0x77);
        let widened: H512 = h256.into();
        assert_eq!(h512_to_h256(widened), Some(h256));

        let mut foreign = widened;
        foreign.0[0] = 1;
        assert_eq!(h512_to_h256(foreign), None, "non-zero upper half rejected");
    }

    #[test]
    fn tip_saturates_above_u32_max() {
        // No block seen yet -> 0.
        assert_eq!(height_to_tip(None), 0);
        // In-range height passes through unchanged.
        assert_eq!(height_to_tip(Some(12345)), 12345);
        assert_eq!(height_to_tip(Some(u32::MAX as u64)), u32::MAX);
        // A height above u32::MAX saturates (does NOT wrap/truncate).
        assert_eq!(height_to_tip(Some(u32::MAX as u64 + 1)), u32::MAX);
        assert_eq!(height_to_tip(Some(u64::MAX)), u32::MAX);
    }
}

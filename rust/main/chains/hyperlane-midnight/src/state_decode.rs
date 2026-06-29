//! Native Rust decode of the WarpRoute (`night`) contract's ledger state
//! as served by the Midnight indexer (the `contractAction.state` hex blob).
//!
//! The `night` contract has no usable generated decoder — its compiled TS
//! `Ledger` type is empty because its ledger fields live in imported
//! modules. So we deserialize the tagged ledger state with the same crates
//! the indexer uses (`midnight-onchain-runtime` / `midnight-serialize`) and
//! navigate the resulting `StateValue::Array` positionally.
//!
//! Field positions are taken from the compiled `night` readers'
//! `queryLedgerState` paths and verified against live deployed state in the
//! tests below: `_validatorCount_0` -> `[0, 8]`, `_thresholdValue_0` ->
//! `[0, 9]`, `_moduleType_0` -> `[0, 10]`, and the `validators` map at
//! `[0, 7]`. Slots `[0, 11..13]` are the module's private `_verify_*` scratch
//! fields, so `module_type` sits one slot from scratch state. The paths are
//! pinned to the field declaration order in `MessageIdMultisigIsm.compact`;
//! any reorder/insert above or between these fields shifts them. To
//! re-verify after a contract change, recompile `night.compact` and grep the
//! `queryLedgerState` paths in `managed/night/contract/index.js`. The
//! contracts-repo CI asserts these paths on every compile, and
//! `decode_ism_state` below fails loudly if the decoded fields are mutually
//! inconsistent.

use std::collections::HashMap;

use hyperlane_core::{ChainResult, Decode as _, HyperlaneMessage, H256};
use midnight_onchain_runtime::state::{ContractState, StateValue};
use midnight_serialize::tagged_deserialize;
use midnight_storage_core::DefaultDB;

use crate::error::HyperlaneMidnightError;

// Positional paths into the ledger `StateValue::Array`, from the compiled
// `night` readers. The MessageIdMultisigIsm fields are consecutive slots
// under the first array element, in source-declaration order:
// validators(7), validator_count(8), threshold(9), module_type(10).
const ISM_VALIDATORS_PATH: [usize; 2] = [0, 7];
const ISM_VALIDATOR_COUNT_PATH: [usize; 2] = [0, 8];
const ISM_THRESHOLD_PATH: [usize; 2] = [0, 9];
const ISM_MODULE_TYPE_PATH: [usize; 2] = [0, 10];

// Positional paths into the Mailbox module, which lives under the SECOND root
// array element `[1]` (the first root holds ownership + the ISM module).
// Verified two ways:
//   (1) The compiled `night` readers in `managed/night/contract/index.js`
//       index these exact slots: `isDelivered`/`deliveryCount` -> `[1, 2]`
//       (`deliveries` Set, `member`/`size`), `nonceValue` -> `[1, 3]`
//       (`nonce` Counter, `popeq`), `messageAt` -> `[1, 4]` (the
//       `dispatched_messages` Map, keyed `member`/`idx`).
//   (2) Decoding the live `night-state.hex` fixture (see tests): root is a
//       2-element array; `[1, 2]` is a Map (Set), `[1, 3]` a Cell (Counter),
//       `[1, 4]` a Map. Matches the declaration order in
//       `modules/Mailbox.compact`: deliveries, nonce, dispatched_messages.
// The paths are pinned to that source-declaration order; any reorder/insert
// above or between these fields shifts them. The contracts-repo CI asserts
// the `queryLedgerState` paths on every compile.
const MAILBOX_DELIVERIES_PATH: [usize; 2] = [1, 2];
const MAILBOX_NONCE_PATH: [usize; 2] = [1, 3];
const MAILBOX_DISPATCHED_MESSAGES_PATH: [usize; 2] = [1, 4];

/// Length in bytes of an encoded `HyperlaneMessage` stored in the
/// `dispatched_messages` map (`Bytes<141>`): version(1) + nonce(4) +
/// origin(4) + sender(32) + destination(4) + recipient(32) + body(64).
const ENCODED_MESSAGE_LEN: usize = 141;

/// The MessageIdMultisigIsm configuration read from on-chain state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsmState {
    /// Validator addresses (20-byte ETH addresses), ordered by on-chain
    /// slot index 0..validator_count.
    pub validators: Vec<[u8; 20]>,
    /// Number of populated validator slots.
    pub validator_count: u8,
    /// Multisig threshold (M of N).
    pub threshold: u8,
    /// Hyperlane `ModuleType` discriminant (5 = MessageIdMultisig).
    pub module_type: u8,
}

/// Deserialize the raw indexer-served state bytes into a `ContractState`.
pub fn decode_contract_state(bytes: &[u8]) -> ChainResult<ContractState<DefaultDB>> {
    let mut reader = bytes;
    tagged_deserialize(&mut reader)
        .map_err(|e| HyperlaneMidnightError::StateDecode(e.to_string()).into())
}

/// Navigate a positional path through nested `StateValue::Array`s.
fn nav<'a>(
    root: &'a StateValue<DefaultDB>,
    path: &[usize],
) -> ChainResult<&'a StateValue<DefaultDB>> {
    let mut node = root;
    for &i in path {
        node = match node {
            StateValue::Array(arr) => arr.get(i).ok_or_else(|| {
                HyperlaneMidnightError::StateDecode(format!("array index {i} out of bounds"))
            })?,
            other => {
                return Err(HyperlaneMidnightError::StateDecode(format!(
                    "expected array at path index {i}, got {other:?}"
                ))
                .into())
            }
        };
    }
    Ok(node)
}

/// Raw atom bytes of a leaf `Cell`'s aligned value. Compact integers are
/// little-endian with trailing zero bytes trimmed; byte arrays (`Bytes<N>`)
/// are stored verbatim.
fn cell_atom(node: &StateValue<DefaultDB>) -> ChainResult<&[u8]> {
    match node {
        StateValue::Cell(sp) => {
            let aligned = &**sp;
            let atom = aligned.value.0.first().ok_or_else(|| {
                HyperlaneMidnightError::StateDecode("empty aligned value".to_string())
            })?;
            Ok(&atom.0)
        }
        other => Err(HyperlaneMidnightError::StateDecode(format!(
            "expected cell leaf, got {other:?}"
        ))
        .into()),
    }
}

/// Read a `Uint<8>` leaf (value like 5; `0` is an empty atom).
fn read_u8(node: &StateValue<DefaultDB>) -> ChainResult<u8> {
    let bytes = cell_atom(node)?;
    match bytes.len() {
        0 => Ok(0),
        1 => Ok(bytes[0]),
        n => Err(
            HyperlaneMidnightError::StateDecode(format!("expected u8 leaf, got {n} bytes")).into(),
        ),
    }
}

/// Read a `Bytes<20>` leaf (an ETH validator address).
fn read_bytes20(node: &StateValue<DefaultDB>) -> ChainResult<[u8; 20]> {
    let bytes = cell_atom(node)?;
    <[u8; 20]>::try_from(bytes).map_err(|_| {
        HyperlaneMidnightError::StateDecode(format!(
            "expected 20-byte address, got {} bytes",
            bytes.len()
        ))
        .into()
    })
}

/// Read the `validators: Map<Uint<8>, Bytes<20>>` ledger field, returned in
/// ascending slot-index order (the order the on-chain multisig expects).
fn read_validators(node: &StateValue<DefaultDB>) -> ChainResult<Vec<[u8; 20]>> {
    let map = match node {
        StateValue::Map(m) => m,
        other => {
            return Err(HyperlaneMidnightError::StateDecode(format!(
                "expected map for validators, got {other:?}"
            ))
            .into())
        }
    };
    let mut entries: Vec<(u8, [u8; 20])> = Vec::with_capacity(map.size());
    for entry in map.iter() {
        let pair = &*entry;
        // key is an AlignedValue (the Uint<8> slot index); read its first byte.
        let key_av = &*pair.0;
        let idx = key_av
            .value
            .0
            .first()
            .and_then(|atom| atom.0.first())
            .copied()
            .unwrap_or(0);
        let addr = read_bytes20(&pair.1)?;
        entries.push((idx, addr));
    }
    entries.sort_by_key(|(idx, _)| *idx);
    Ok(entries.into_iter().map(|(_, addr)| addr).collect())
}

/// Decode the MessageIdMultisigIsm config (validators, threshold, module
/// type) from the `night` contract's serialized ledger state.
pub fn decode_ism_state(bytes: &[u8]) -> ChainResult<IsmState> {
    let cs = decode_contract_state(bytes)?;
    let root = cs.data.get_ref();
    let state = IsmState {
        validators: read_validators(nav(root, &ISM_VALIDATORS_PATH)?)?,
        validator_count: read_u8(nav(root, &ISM_VALIDATOR_COUNT_PATH)?)?,
        threshold: read_u8(nav(root, &ISM_THRESHOLD_PATH)?)?,
        module_type: read_u8(nav(root, &ISM_MODULE_TYPE_PATH)?)?,
    };

    // Structural sanity: the decoded slots must be mutually consistent. A
    // mismatch means the positional paths read the wrong slots (e.g. a
    // contract layout shift), not a legitimate on-chain state. `module_type`
    // is validated separately in `ism::module_type_from_u8`, which keeps this
    // decoder agnostic to which ISM variants the agent supports.
    if state.validator_count as usize != state.validators.len() {
        return Err(HyperlaneMidnightError::StateDecode(format!(
            "validator_count {} does not match decoded validator set size {}",
            state.validator_count,
            state.validators.len()
        ))
        .into());
    }
    if state.threshold < 1 || state.threshold > state.validator_count {
        return Err(HyperlaneMidnightError::StateDecode(format!(
            "threshold {} out of range 1..={} (validator_count)",
            state.threshold, state.validator_count
        ))
        .into());
    }

    Ok(state)
}

/// Read a little-endian unsigned integer leaf (a Compact `Counter`/`Uint<64>`)
/// as a `u64`. The on-chain runtime trims trailing zero bytes, so a zero
/// counter is an empty atom and any value is at most 8 bytes.
fn read_counter_u64(node: &StateValue<DefaultDB>) -> ChainResult<u64> {
    let bytes = cell_atom(node)?;
    if bytes.len() > 8 {
        return Err(HyperlaneMidnightError::StateDecode(format!(
            "expected counter leaf of at most 8 bytes, got {}",
            bytes.len()
        ))
        .into());
    }
    let mut le = [0u8; 8];
    le[..bytes.len()].copy_from_slice(bytes);
    Ok(u64::from_le_bytes(le))
}

/// Read the `nonce` Counter from the Mailbox state and return it as a `u32`.
/// This is the number of messages dispatched so far (valid dispatch keys are
/// `0..nonce`), mirroring upstream `Mailbox.nonce`. The on-chain circuit caps
/// the counter below `2^32`, so the cast cannot truncate a legitimate value.
pub fn decode_nonce_count(bytes: &[u8]) -> ChainResult<u32> {
    let cs = decode_contract_state(bytes)?;
    let root = cs.data.get_ref();
    let n = read_counter_u64(nav(root, &MAILBOX_NONCE_PATH)?)?;
    u32::try_from(n).map_err(|_| {
        HyperlaneMidnightError::StateDecode(format!("nonce counter {n} exceeds u32")).into()
    })
}

/// Decode a single `dispatched_messages` map value (a `Bytes<141>` leaf) back
/// into a `HyperlaneMessage`. The runtime trims trailing zero bytes from a
/// `Bytes<141>` leaf, so a message whose body ends in zeros is stored as fewer
/// than 141 bytes; right-pad back to the full fixed width before decoding.
fn decode_message_value(node: &StateValue<DefaultDB>) -> ChainResult<HyperlaneMessage> {
    let encoded = cell_atom(node)?;
    if encoded.len() > ENCODED_MESSAGE_LEN {
        return Err(HyperlaneMidnightError::StateDecode(format!(
            "dispatched message is {} bytes, expected at most {ENCODED_MESSAGE_LEN}",
            encoded.len()
        ))
        .into());
    }
    let mut full = [0u8; ENCODED_MESSAGE_LEN];
    full[..encoded.len()].copy_from_slice(encoded);
    HyperlaneMessage::read_from(&mut &full[..])
        .map_err(|e| HyperlaneMidnightError::StateDecode(e.to_string()).into())
}

/// Parse a `Uint<32>` map key's raw bytes (stored little-endian with trailing
/// zeros trimmed) as a `u32`. Returns `None` if the key is wider than 4 bytes
/// (not a valid nonce key).
fn parse_nonce_key(key_bytes: &[u8]) -> Option<u32> {
    if key_bytes.len() > 4 {
        return None;
    }
    let mut le = [0u8; 4];
    le[..key_bytes.len()].copy_from_slice(key_bytes);
    Some(u32::from_le_bytes(le))
}

/// A single-read snapshot of the Mailbox dispatch state: every dispatched
/// `HyperlaneMessage` keyed by nonce, plus the `nonce` counter, decoded from
/// ONE serialized state blob.
///
/// The dispatch indexer uses this to serve a whole nonce range from one
/// network fetch instead of re-fetching and re-scanning the full state per
/// nonce. Decoding the count and the messages from the same blob also keeps
/// them mutually consistent within a scan.
#[derive(Debug, Clone)]
pub struct DispatchSnapshot {
    /// Every dispatched message, keyed by its nonce.
    pub messages: HashMap<u32, HyperlaneMessage>,
    /// The `nonce` counter: the number of messages dispatched so far.
    pub nonce_count: u32,
}

/// Decode the dispatched message stored at the given nonce key in the
/// `dispatched_messages: Map<Uint<32>, Bytes<141>>` ledger field back into a
/// `HyperlaneMessage`. Returns `None` if no message is stored at that key.
pub fn decode_dispatched_message(
    bytes: &[u8],
    nonce: u32,
) -> ChainResult<Option<HyperlaneMessage>> {
    let cs = decode_contract_state(bytes)?;
    let root = cs.data.get_ref();
    let map = match nav(root, &MAILBOX_DISPATCHED_MESSAGES_PATH)? {
        StateValue::Map(m) => m,
        other => {
            return Err(HyperlaneMidnightError::StateDecode(format!(
                "expected map for dispatched_messages, got {other:?}"
            ))
            .into())
        }
    };

    for entry in map.iter() {
        let pair = &*entry;
        let key_av = &*pair.0;
        // The Uint<32> key is stored little-endian (trailing zeros trimmed).
        let key_bytes = key_av
            .value
            .0
            .first()
            .map(|atom| atom.0.as_ref())
            .unwrap_or(&[][..]);
        if parse_nonce_key(key_bytes) != Some(nonce) {
            continue;
        }
        return Ok(Some(decode_message_value(&pair.1)?));
    }
    Ok(None)
}

/// Decode the whole dispatch state (every dispatched message keyed by nonce,
/// plus the nonce counter) from a single serialized state blob. The dispatch
/// indexer reads this once per scan and serves the requested nonce range from
/// the returned in-memory map, avoiding a network fetch + full-state scan per
/// nonce.
pub fn decode_dispatch_snapshot(bytes: &[u8]) -> ChainResult<DispatchSnapshot> {
    let cs = decode_contract_state(bytes)?;
    let root = cs.data.get_ref();

    let n = read_counter_u64(nav(root, &MAILBOX_NONCE_PATH)?)?;
    let nonce_count = u32::try_from(n).map_err(|_| {
        HyperlaneMidnightError::StateDecode(format!("nonce counter {n} exceeds u32"))
    })?;

    let map = match nav(root, &MAILBOX_DISPATCHED_MESSAGES_PATH)? {
        StateValue::Map(m) => m,
        other => {
            return Err(HyperlaneMidnightError::StateDecode(format!(
                "expected map for dispatched_messages, got {other:?}"
            ))
            .into())
        }
    };

    let mut messages = HashMap::with_capacity(map.size());
    for entry in map.iter() {
        let pair = &*entry;
        let key_av = &*pair.0;
        // The Uint<32> key is stored little-endian (trailing zeros trimmed).
        let key_bytes = key_av
            .value
            .0
            .first()
            .map(|atom| atom.0.as_ref())
            .unwrap_or(&[][..]);
        // A key wider than 4 bytes is not a valid nonce key; skip it rather
        // than fail the whole snapshot.
        let Some(nonce) = parse_nonce_key(key_bytes) else {
            continue;
        };
        messages.insert(nonce, decode_message_value(&pair.1)?);
    }

    Ok(DispatchSnapshot {
        messages,
        nonce_count,
    })
}

/// Decode the `deliveries: Set<Bytes<32>>` ledger field into the set of
/// delivered message ids. The set is unordered; callers must not rely on the
/// returned order.
pub fn decode_deliveries(bytes: &[u8]) -> ChainResult<Vec<H256>> {
    let cs = decode_contract_state(bytes)?;
    let root = cs.data.get_ref();
    let map = match nav(root, &MAILBOX_DELIVERIES_PATH)? {
        StateValue::Map(m) => m,
        other => {
            return Err(HyperlaneMidnightError::StateDecode(format!(
                "expected map for deliveries, got {other:?}"
            ))
            .into())
        }
    };

    let mut ids = Vec::with_capacity(map.size());
    for entry in map.iter() {
        let pair = &*entry;
        // A `Set<Bytes<32>>` stores the 32-byte member as the map KEY.
        let key_av = &*pair.0;
        let key_bytes = key_av
            .value
            .0
            .first()
            .map(|atom| atom.0.as_ref())
            .unwrap_or(&[][..]);
        // The runtime trims trailing zero bytes; right-pad back to 32.
        if key_bytes.len() > 32 {
            return Err(HyperlaneMidnightError::StateDecode(format!(
                "delivery id is {} bytes, expected at most 32",
                key_bytes.len()
            ))
            .into());
        }
        let mut id = [0u8; 32];
        id[..key_bytes.len()].copy_from_slice(key_bytes);
        ids.push(H256::from(id));
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    use hyperlane_core::Encode as _;
    use midnight_base_crypto::fab::{AlignedValue, ValueAtom};
    use midnight_onchain_state::state::ChargedState;
    use midnight_storage::arena::Sp;
    use midnight_storage::storage::{Array, HashMap};

    fn addr(hex: &str) -> [u8; 20] {
        let v = hex::decode(hex.trim_start_matches("0x")).unwrap();
        <[u8; 20]>::try_from(v.as_slice()).unwrap()
    }

    /// A `Cell` leaf wrapping the given `AlignedValue`-convertible value, as the
    /// runtime stores scalar/byte ledger fields.
    fn cell<V: Into<AlignedValue>>(value: V) -> StateValue<DefaultDB> {
        StateValue::Cell(Sp::new(value.into()))
    }

    /// Wrap a sequence of state values into a fixed-size `Array` node.
    fn array(values: Vec<StateValue<DefaultDB>>) -> StateValue<DefaultDB> {
        StateValue::Array(Array::from(values))
    }

    /// Serialize a synthetic Mailbox `StateValue` tree into the same tagged
    /// `ContractState` wire bytes the live indexer serves. The root mirrors the
    /// deployed layout: a 2-element array whose `[1]` element is the Mailbox
    /// module, with `deliveries` at `[1,2]`, `nonce` at `[1,3]` and
    /// `dispatched_messages` at `[1,4]`. Slots before each field are filled
    /// with `Null` so the pinned paths line up.
    fn mailbox_state_bytes(
        deliveries: StateValue<DefaultDB>,
        nonce: StateValue<DefaultDB>,
        dispatched: StateValue<DefaultDB>,
    ) -> Vec<u8> {
        let mailbox = array(vec![
            StateValue::Null, // [1,0]
            StateValue::Null, // [1,1]
            deliveries,       // [1,2]
            nonce,            // [1,3]
            dispatched,       // [1,4]
        ]);
        let root = array(vec![
            StateValue::Null, // [0] ownership + ISM (unused here)
            mailbox,          // [1] Mailbox module
        ]);
        let cs = ContractState::<DefaultDB> {
            data: ChargedState::new(root),
            ..ContractState::default()
        };
        let mut bytes = Vec::new();
        midnight_serialize::tagged_serialize(&cs, &mut bytes).expect("serialize synthetic state");
        bytes
    }

    fn sample_message(nonce: u32) -> HyperlaneMessage {
        HyperlaneMessage {
            version: 3,
            nonce,
            origin: 1234,
            sender: H256::repeat_byte(0xAB),
            destination: 5678,
            recipient: H256::repeat_byte(0xCD),
            // 64-byte body; trailing zeros exercise the right-pad path.
            body: {
                let mut b = vec![0u8; 64];
                b[0] = 0x11;
                b[1] = 0x22;
                b
            },
        }
    }

    #[test]
    fn decodes_synthetic_nonce_count() {
        let bytes = mailbox_state_bytes(
            StateValue::Map(HashMap::new()),
            cell(7u64),
            StateValue::Map(HashMap::new()),
        );
        assert_eq!(decode_nonce_count(&bytes).expect("decode nonce"), 7);
    }

    #[test]
    fn decodes_synthetic_nonce_count_zero() {
        // A zero counter is stored as an empty atom (trailing zeros trimmed).
        let bytes = mailbox_state_bytes(
            StateValue::Map(HashMap::new()),
            cell(0u64),
            StateValue::Map(HashMap::new()),
        );
        assert_eq!(decode_nonce_count(&bytes).expect("decode nonce"), 0);
    }

    #[test]
    fn decodes_synthetic_dispatched_message() {
        let msg = sample_message(2);
        let encoded = msg.to_vec();
        assert_eq!(encoded.len(), ENCODED_MESSAGE_LEN, "message wire length");
        let full: [u8; ENCODED_MESSAGE_LEN] = encoded.try_into().unwrap();

        // `dispatched_messages: Map<Uint<32>, Bytes<141>>` keyed by nonce.
        let map = HashMap::<AlignedValue, StateValue<DefaultDB>, DefaultDB>::new()
            .insert(AlignedValue::from(2u32), cell(full));
        let bytes = mailbox_state_bytes(
            StateValue::Map(HashMap::new()),
            cell(3u64),
            StateValue::Map(map),
        );

        let decoded = decode_dispatched_message(&bytes, 2)
            .expect("decode dispatched message")
            .expect("message present at nonce 2");
        assert_eq!(decoded, msg);

        // A nonce with no stored message returns None.
        assert!(decode_dispatched_message(&bytes, 5)
            .expect("decode dispatched message")
            .is_none());
    }

    #[test]
    fn decodes_correct_message_among_many_entries() {
        // Multiple map entries exercise the key-scan: a wrong-entry bug would
        // return the wrong message body/recipient for a requested nonce.
        let mut map = HashMap::<AlignedValue, StateValue<DefaultDB>, DefaultDB>::new();
        let mut expected = Vec::new();
        for nonce in [0u32, 1, 7, 42] {
            let msg = sample_message(nonce);
            let full: [u8; ENCODED_MESSAGE_LEN] = msg.to_vec().try_into().unwrap();
            map = map.insert(AlignedValue::from(nonce), cell(full));
            expected.push((nonce, msg));
        }
        let bytes = mailbox_state_bytes(
            StateValue::Map(HashMap::new()),
            cell(43u64),
            StateValue::Map(map),
        );

        // Each requested nonce returns exactly its own message.
        for (nonce, msg) in &expected {
            let decoded = decode_dispatched_message(&bytes, *nonce)
                .expect("decode dispatched message")
                .unwrap_or_else(|| panic!("message present at nonce {nonce}"));
            assert_eq!(&decoded, msg, "nonce {nonce} returned the wrong message");
            assert_eq!(decoded.nonce, *nonce);
        }

        // The same holds for the single-read snapshot decode.
        let snapshot = decode_dispatch_snapshot(&bytes).expect("decode snapshot");
        assert_eq!(snapshot.nonce_count, 43);
        assert_eq!(snapshot.messages.len(), expected.len());
        for (nonce, msg) in &expected {
            assert_eq!(snapshot.messages.get(nonce), Some(msg));
        }
    }

    #[test]
    fn decodes_dispatched_message_right_padding_trimmed_value() {
        // The runtime trims trailing zero bytes from a `Bytes<141>` leaf
        // (`From<[u8; N]> for ValueAtom` drops trailing zeros). This message has
        // an all-zero body, so its 141-byte encoding ends in many zeros and is
        // stored as a SHORT atom; the decoder must right-pad back to 141.
        let msg = HyperlaneMessage {
            version: 3,
            nonce: 9,
            origin: 1,
            sender: H256::repeat_byte(0x01),
            destination: 2,
            recipient: H256::repeat_byte(0x02),
            // All-zero 64-byte body, so the wire encoding ends in many zeros.
            body: vec![0u8; 64],
        };
        let encoded = msg.to_vec();
        assert_eq!(encoded.len(), ENCODED_MESSAGE_LEN);
        let full: [u8; ENCODED_MESSAGE_LEN] = encoded.clone().try_into().unwrap();

        // Confirm the stored atom is actually shorter than 141 (the trim
        // happened), so this test really exercises the right-pad branch and not
        // the verbatim path.
        let stored = ValueAtom::from(full);
        assert!(
            stored.0.len() < ENCODED_MESSAGE_LEN,
            "expected the trailing-zero body to be trimmed on store, got {} bytes",
            stored.0.len()
        );

        let map = HashMap::<AlignedValue, StateValue<DefaultDB>, DefaultDB>::new()
            .insert(AlignedValue::from(9u32), cell(full));
        let bytes = mailbox_state_bytes(
            StateValue::Map(HashMap::new()),
            cell(10u64),
            StateValue::Map(map),
        );

        // Decoding right-pads back to the full 141 bytes and recovers the
        // original message (including the all-zero body).
        let decoded = decode_dispatched_message(&bytes, 9)
            .expect("decode dispatched message")
            .expect("message present at nonce 9");
        assert_eq!(decoded, msg);
    }

    #[test]
    fn decodes_synthetic_deliveries_set() {
        let id_a = H256::repeat_byte(0x01);
        let id_b = H256::repeat_byte(0x02);
        // `deliveries: Set<Bytes<32>>` stores each member as the map key with a
        // unit/null value.
        let set = HashMap::<AlignedValue, StateValue<DefaultDB>, DefaultDB>::new()
            .insert(AlignedValue::from(id_a.0), StateValue::Null)
            .insert(AlignedValue::from(id_b.0), StateValue::Null);
        let bytes = mailbox_state_bytes(
            StateValue::Map(set),
            cell(0u64),
            StateValue::Map(HashMap::new()),
        );

        let mut ids = decode_deliveries(&bytes).expect("decode deliveries");
        ids.sort();
        let mut expected = vec![id_a, id_b];
        expected.sort();
        assert_eq!(ids, expected);
    }

    // Real `night` state captured from the local devnet indexer
    // (deploy with validators 0x19e7../0x1563../0x5cbd.., threshold 2,
    // module_type 5).
    #[test]
    fn decodes_live_night_ism_state() {
        let hex = include_str!("../tests/fixtures/night-state.hex").trim();
        let bytes = hex::decode(hex).expect("fixture is valid hex");
        let ism = decode_ism_state(&bytes).expect("decode ISM state");

        assert_eq!(
            ism.module_type, 5,
            "module_type should be MessageIdMultisig"
        );
        assert_eq!(ism.threshold, 2, "threshold");
        assert_eq!(ism.validator_count, 3, "validator_count");
        assert_eq!(
            ism.validators,
            vec![
                addr("19e7e376e7c213b7e7e7e46cc70a5dd086daff2a"),
                addr("1563915e194d8cfba1943570603f7606a3115508"),
                addr("5cbdd86a2fa8dc4bddd8a8f69dba48572eec07fb"),
            ],
            "validator addresses in slot order"
        );
    }
}

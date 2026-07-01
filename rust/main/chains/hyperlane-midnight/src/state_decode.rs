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

// Positional paths into the Mailbox + MerkleTree modules, which live under the
// SECOND root array element `[1]` (the first root holds ownership + the ISM
// module), in module field-declaration order: deliveries(2), nonce(3),
// dispatched_messages(4), branch(5), count(6), current_root(7), then the
// MerkleTree scratch fields (8..10). Verified two ways:
//   (1) The compiled `night` readers in `managed/night/contract/index.js`
//       index these exact slots: `isDelivered`/`deliveryCount` -> `[1, 2]`
//       (`deliveries` Set, `member`/`size`), `nonceValue` -> `[1, 3]`
//       (`nonce` Counter, `popeq`), `messageAt` -> `[1, 4]` (the
//       `dispatched_messages` Map, keyed `member`/`idx`), `_merkleCount_0` ->
//       `[1, 6]`, and `_root_0`'s else branch -> `[1, 7]` (cached root).
//   (2) Decoding the live `night-state.hex` fixture (see tests): root is a
//       2-element array; `[1, 2]` is a Map (Set), `[1, 3]` a Cell (Counter),
//       `[1, 4]` a Map. Matches the declaration order in
//       `modules/Mailbox.compact` / `modules/MerkleTree.compact`.
// The paths are pinned to that source-declaration order; any reorder/insert
// above or between these fields shifts them. The contracts-repo CI asserts
// the `queryLedgerState` paths on every compile.
const MAILBOX_DELIVERIES_PATH: [usize; 2] = [1, 2];
const MAILBOX_NONCE_PATH: [usize; 2] = [1, 3];
const MAILBOX_DISPATCHED_MESSAGES_PATH: [usize; 2] = [1, 4];
// `DISPATCHED_MESSAGES_PATH` is the merkle indexer's reader for the same
// `[1, 4]` slot as `MAILBOX_DISPATCHED_MESSAGES_PATH` above.
const DISPATCHED_MESSAGES_PATH: [usize; 2] = [1, 4];
const MERKLE_COUNT_PATH: [usize; 2] = [1, 6];
const CURRENT_ROOT_PATH: [usize; 2] = [1, 7];

// Positional paths into the IGP contract's ledger state (#19). Unlike `night`
// (whose fields nest under the [0]/[1] module groups), the IGP contract's
// fields are FLAT top-level slots, in field-declaration order after the
// ZOwnablePK + Initializable access-control fields (slots 0..3):
// remote_gas_data(4), gas_payments(5), gas_payment_count(6), beneficiary(7).
// Verified against the compiled `igp` readers in
// `managed/igp/contract/index.js`: `_gasPaymentCount_0` reads idx 6,
// `_gasPaymentAt_0` reads idx 5 (then a key lookup), `_isRegistered_0` idx 4,
// `_beneficiaryValue_0` idx 7. These follow the declaration order in
// `igp.compact`; a reorder/insert above or between them shifts the decoder.
// The contracts-repo layout guard (`scripts/check-ledger-layout.mjs`) pins
// these slots on every compile, same as it does for the `night` slots above.
const IGP_GAS_PAYMENTS_PATH: [usize; 1] = [5];
const IGP_GAS_PAYMENT_COUNT_PATH: [usize; 1] = [6];

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

/// Read a `Counter` / `Uint<64>` leaf. Compact stores it as a little-endian
/// integer cell with trailing zero bytes trimmed, so `0` is an empty atom.
fn read_u64(node: &StateValue<DefaultDB>) -> ChainResult<u64> {
    let bytes = cell_atom(node)?;
    if bytes.len() > 8 {
        return Err(HyperlaneMidnightError::StateDecode(format!(
            "expected u64 leaf, got {} bytes",
            bytes.len()
        ))
        .into());
    }
    let mut buf = [0u8; 8];
    buf[..bytes.len()].copy_from_slice(bytes);
    Ok(u64::from_le_bytes(buf))
}

/// Narrow an on-chain `Counter` value to the `u32` the Hyperlane merkle-tree
/// types use, asserting the upper 32 bits are zero rather than truncating.
/// The on-chain `count` is a `Counter` (u64 width) but Hyperlane caps the
/// tree at `2^32 - 1` leaves, so a value that does not fit is a layout/decode
/// error, not a legitimate state.
fn narrow_u32(value: u64) -> ChainResult<u32> {
    u32::try_from(value).map_err(|_| {
        HyperlaneMidnightError::StateDecode(format!(
            "merkle count {value} exceeds u32::MAX (upper 32 bits must be zero)"
        ))
        .into()
    })
}

/// Read a `Bytes<32>` leaf (a merkle root or message id), stored verbatim.
fn read_bytes32(node: &StateValue<DefaultDB>) -> ChainResult<H256> {
    let bytes = cell_atom(node)?;
    <[u8; 32]>::try_from(bytes).map(H256::from).map_err(|_| {
        HyperlaneMidnightError::StateDecode(format!(
            "expected 32-byte value, got {} bytes",
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


/// Merkle-tree state the validator needs: the leaf `count` and the cached
/// `current_root`. `current_root` is meaningful only when `count > 0` —
/// before the first insert the field is unset and the contract's `root()`
/// returns the empty-tree root instead, so callers must treat `count == 0`
/// as "no checkpoint" rather than trusting the (zero) root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MerkleState {
    /// Number of leaves inserted, narrowed from the on-chain `Counter`.
    pub count: u32,
    /// Cached post-insert root. Zero (and meaningless) when `count == 0`.
    pub current_root: H256,
}

/// Decode the WarpRoute merkle-tree `count` + `current_root` ledger fields
/// from the `night` contract's serialized state.
pub fn decode_merkle_state(bytes: &[u8]) -> ChainResult<MerkleState> {
    let cs = decode_contract_state(bytes)?;
    let root = cs.data.get_ref();
    let count = narrow_u32(read_u64(nav(root, &MERKLE_COUNT_PATH)?)?)?;
    let current_root = if count == 0 {
        H256::zero()
    } else {
        read_bytes32(nav(root, &CURRENT_ROOT_PATH)?)?
    };
    Ok(MerkleState {
        count,
        current_root,
    })
}

/// Decode the append-only `dispatched_messages: Map<Uint<32>, Bytes<141>>`
/// ledger field into `(nonce, message)` pairs sorted by nonce. Each map value
/// is the wire-format encoded `HyperlaneMessage`; the decoder re-parses it and
/// asserts the encoded nonce matches the map key — the same binding
/// `Mailbox.recordDispatch` enforces on-chain, here as defence-in-depth
/// against a malformed state read. Shared with the dispatch indexer (#16).
pub fn decode_dispatched_messages(bytes: &[u8]) -> ChainResult<Vec<(u32, HyperlaneMessage)>> {
    let cs = decode_contract_state(bytes)?;
    let root = cs.data.get_ref();
    let map = match nav(root, &DISPATCHED_MESSAGES_PATH)? {
        StateValue::Map(m) => m,
        other => {
            return Err(HyperlaneMidnightError::StateDecode(format!(
                "expected map for dispatched_messages, got {other:?}"
            ))
            .into())
        }
    };

    let mut out: Vec<(u32, HyperlaneMessage)> = Vec::with_capacity(map.size());
    for entry in map.iter() {
        let pair = &*entry;
        // Map key is a little-endian `Uint<32>` nonce.
        let key_av = &*pair.0;
        let key_bytes = key_av
            .value
            .0
            .first()
            .map(|atom| &atom.0[..])
            .unwrap_or(&[]);
        let mut kb = [0u8; 4];
        let n = key_bytes.len().min(4);
        kb[..n].copy_from_slice(&key_bytes[..n]);
        let nonce = u32::from_le_bytes(kb);

        // Map value is the wire-format encoded message. Compact trims trailing
        // zero bytes from the stored `Bytes<141>` leaf, so a message whose tail
        // is zero (e.g. a decimal-scaled amount ending in zero bytes — 10^17
        // for a 6->18 decimal route ends in 0x0000) is stored as < 141 bytes.
        // Right-pad back to the fixed width before decoding (the trimmed bytes
        // were zeros); only an over-long value is an error. Mirrors
        // `decode_single_message`. (The #15 simulator fixture used an identity
        // scale, so its amount kept a non-zero tail and never triggered this.)
        let value = cell_atom(&pair.1)?;
        if value.len() > ENCODED_MESSAGE_LEN {
            return Err(HyperlaneMidnightError::StateDecode(format!(
                "dispatched message at nonce {nonce} is {} bytes, expected at most {ENCODED_MESSAGE_LEN}",
                value.len()
            ))
            .into());
        }
        let mut full = [0u8; ENCODED_MESSAGE_LEN];
        full[..value.len()].copy_from_slice(value);
        let message = HyperlaneMessage::from(full.to_vec());
        if message.nonce != nonce {
            return Err(HyperlaneMidnightError::StateDecode(format!(
                "dispatched message nonce field {} does not match map key {nonce}",
                message.nonce
            ))
            .into());
        }
        out.push((nonce, message));
    }
    out.sort_by_key(|(nonce, _)| *nonce);
    Ok(out)
}

/// A decoded row from the IGP `gas_payments: Map<Uint<32>, GasPayment>`
/// ledger field. Mirrors the on-chain `GasPayment` struct field-for-field;
/// the IGP indexer (#19) maps it to `hyperlane_core::InterchainGasPayment`.
/// Integer widths match the contract (`gasAmount` is the `Uint<64>` gas
/// limit; `payment` is the `Uint<128>` NIGHT attached).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgpGasPayment {
    /// The message this payment funds. Prover-supplied keccak id until the
    /// keccak MIP lands (see the mocked-primitives note in `LIMITS.md`).
    pub message_id: H256,
    /// Destination Hyperlane domain.
    pub destination: u32,
    /// Requested destination gas (the `gasLimit` argument to `payForGas`).
    pub gas_amount: u64,
    /// NIGHT actually attached, in the token's smallest unit.
    pub payment: u128,
}

/// A single-read snapshot of the IGP payment state: every recorded
/// `GasPayment` keyed by its append index, plus the `gas_payment_count`
/// counter, decoded from ONE serialized state blob — the same
/// single-fetch-per-scan shape as [`DispatchSnapshot`]. Decoding the count
/// and the rows from the same blob keeps them mutually consistent within a
/// scan.
#[derive(Debug, Clone)]
pub struct IgpSnapshot {
    /// Every recorded payment, keyed by its append index (`0..payment_count`).
    pub payments: HashMap<u32, IgpGasPayment>,
    /// The `gas_payment_count` counter: the number of payments recorded.
    pub payment_count: u32,
}

/// Read a little-endian `Uint<32>` from an atom's bytes. The runtime trims
/// trailing zero bytes, so a value is at most 4 bytes and `0` is empty.
fn atom_u32(bytes: &[u8]) -> ChainResult<u32> {
    if bytes.len() > 4 {
        return Err(HyperlaneMidnightError::StateDecode(format!(
            "expected u32 atom of at most 4 bytes, got {}",
            bytes.len()
        ))
        .into());
    }
    let mut le = [0u8; 4];
    le[..bytes.len()].copy_from_slice(bytes);
    Ok(u32::from_le_bytes(le))
}

/// Read a little-endian `Uint<64>` from an atom's bytes (trailing zeros
/// trimmed on chain, so at most 8 bytes; empty == 0).
fn atom_u64(bytes: &[u8]) -> ChainResult<u64> {
    if bytes.len() > 8 {
        return Err(HyperlaneMidnightError::StateDecode(format!(
            "expected u64 atom of at most 8 bytes, got {}",
            bytes.len()
        ))
        .into());
    }
    let mut le = [0u8; 8];
    le[..bytes.len()].copy_from_slice(bytes);
    Ok(u64::from_le_bytes(le))
}

/// Read a little-endian `Uint<128>` from an atom's bytes (trailing zeros
/// trimmed on chain, so at most 16 bytes; empty == 0).
fn atom_u128(bytes: &[u8]) -> ChainResult<u128> {
    if bytes.len() > 16 {
        return Err(HyperlaneMidnightError::StateDecode(format!(
            "expected u128 atom of at most 16 bytes, got {}",
            bytes.len()
        ))
        .into());
    }
    let mut le = [0u8; 16];
    le[..bytes.len()].copy_from_slice(bytes);
    Ok(u128::from_le_bytes(le))
}

/// Read a `Bytes<32>` atom (trailing zeros trimmed on chain), right-padded
/// back to the full 32 bytes — the same trim/pad handling the `Bytes<141>`
/// and delivery-id decoders use.
fn atom_bytes32(bytes: &[u8]) -> ChainResult<H256> {
    if bytes.len() > 32 {
        return Err(HyperlaneMidnightError::StateDecode(format!(
            "expected 32-byte atom, got {}",
            bytes.len()
        ))
        .into());
    }
    let mut id = [0u8; 32];
    id[..bytes.len()].copy_from_slice(bytes);
    Ok(H256::from(id))
}

/// The per-field atoms of a struct-valued ledger `Map` value. A Compact
/// struct stored as a map value is a single `Cell` whose `AlignedValue`
/// concatenates one atom per field in declaration order — see the compiled
/// IGP writer, which stores a `GasPayment` as
/// `newCell(GasPaymentDescriptor.toValue(struct))` where `toValue` chains
/// `messageId.toValue().concat(destination.toValue().concat(...))`. Returns
/// the atom byte-slices in that order.
fn struct_cell_atoms(node: &StateValue<DefaultDB>) -> ChainResult<Vec<&[u8]>> {
    match node {
        StateValue::Cell(sp) => {
            let aligned = &**sp;
            Ok(aligned.value.0.iter().map(|atom| &atom.0[..]).collect())
        }
        other => Err(HyperlaneMidnightError::StateDecode(format!(
            "expected cell for struct value, got {other:?}"
        ))
        .into()),
    }
}

/// Decode a single `gas_payments` map value (the `GasPayment` struct cell)
/// into an [`IgpGasPayment`]. The struct is stored as one cell whose atoms
/// are, in order: messageId (`Bytes<32>`), destination (`Uint<32>`),
/// gasAmount (`Uint<64>`), payment (`Uint<128>`) — the field-declaration
/// order in `igp.compact` and the compiled writer's concatenation order.
/// Fails loudly if the atom count is not exactly four, so a struct-layout
/// change surfaces as a decode error rather than a silent field shift.
fn decode_gas_payment(node: &StateValue<DefaultDB>) -> ChainResult<IgpGasPayment> {
    let atoms = struct_cell_atoms(node)?;
    if atoms.len() != 4 {
        return Err(HyperlaneMidnightError::StateDecode(format!(
            "expected 4 atoms in a GasPayment struct cell \
             (messageId, destination, gasAmount, payment), got {}",
            atoms.len()
        ))
        .into());
    }
    Ok(IgpGasPayment {
        message_id: atom_bytes32(atoms[0])?,
        destination: atom_u32(atoms[1])?,
        gas_amount: atom_u64(atoms[2])?,
        payment: atom_u128(atoms[3])?,
    })
}

/// Decode the whole IGP payment state (every recorded `GasPayment` keyed by
/// its append index, plus the `gas_payment_count` counter) from a single
/// serialized state blob. The IGP indexer (#19) reads this once per scan and
/// serves the requested index range from the returned in-memory map — the
/// same single-fetch shape as [`decode_dispatch_snapshot`]. Decoding the
/// count and the rows from the same blob keeps them mutually consistent.
pub fn decode_igp_snapshot(bytes: &[u8]) -> ChainResult<IgpSnapshot> {
    let cs = decode_contract_state(bytes)?;
    let root = cs.data.get_ref();

    let n = read_counter_u64(nav(root, &IGP_GAS_PAYMENT_COUNT_PATH)?)?;
    let payment_count = u32::try_from(n).map_err(|_| {
        HyperlaneMidnightError::StateDecode(format!(
            "gas_payment_count {n} exceeds u32 (the contract asserts each payment fits a Uint<32> key)"
        ))
    })?;

    let map = match nav(root, &IGP_GAS_PAYMENTS_PATH)? {
        StateValue::Map(m) => m,
        other => {
            return Err(HyperlaneMidnightError::StateDecode(format!(
                "expected map for gas_payments, got {other:?}"
            ))
            .into())
        }
    };

    let mut payments = HashMap::with_capacity(map.size());
    for entry in map.iter() {
        let pair = &*entry;
        // Map key is a little-endian `Uint<32>` append index. `parse_nonce_key`
        // is the same `<= 4-byte LE u32` parse the dispatch map uses; here the
        // key is the gas-payment index, not a nonce.
        let key_av = &*pair.0;
        let key_bytes = key_av
            .value
            .0
            .first()
            .map(|atom| atom.0.as_ref())
            .unwrap_or(&[][..]);
        // A key wider than 4 bytes is not a valid index key; skip rather than
        // fail the whole snapshot (mirrors `decode_dispatch_snapshot`).
        let Some(idx) = parse_nonce_key(key_bytes) else {
            continue;
        };
        payments.insert(idx, decode_gas_payment(&pair.1)?);
    }

    Ok(IgpSnapshot {
        payments,
        payment_count,
    })
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
    fn decode_dispatched_messages_right_pads_trimmed_value() {
        // Regression for the merkle-hook / dispatch-indexer batch path
        // (`decode_dispatched_messages`, used by
        // `MidnightMerkleTreeHook::fetch_logs_in_range`). The runtime trims
        // trailing zero bytes from a `Bytes<141>` leaf, so a real dispatch
        // whose decimal-scaled amount ends in zero bytes is stored as < 141
        // bytes (a 6->18-decimal route scales 10^5 to 10^17, which ends in
        // 0x0000 -> a 139-byte on-chain value). This path previously asserted
        // exactly 141 and rejected such messages with
        // "dispatched message ... is 139 bytes, expected 141"; it must right-pad
        // like the singular decoder. The #15 simulator fixture used an identity
        // scale (non-zero amount tail -> full 141 bytes), so it never caught
        // this. An all-zero body trims the most and pins the branch.
        let msg = HyperlaneMessage {
            version: 3,
            nonce: 0,
            origin: 1234,
            sender: H256::repeat_byte(0x01),
            destination: 2,
            recipient: H256::repeat_byte(0x02),
            body: vec![0u8; 64],
        };
        let full: [u8; ENCODED_MESSAGE_LEN] = msg.to_vec().try_into().unwrap();
        assert!(
            ValueAtom::from(full).0.len() < ENCODED_MESSAGE_LEN,
            "expected the trailing-zero tail to be trimmed on store"
        );

        let map = HashMap::<AlignedValue, StateValue<DefaultDB>, DefaultDB>::new()
            .insert(AlignedValue::from(0u32), cell(full));
        let bytes = mailbox_state_bytes(
            StateValue::Map(HashMap::new()),
            cell(1u64),
            StateValue::Map(map),
        );

        let decoded = decode_dispatched_messages(&bytes).expect("decode dispatched messages");
        assert_eq!(decoded.len(), 1, "one dispatched message");
        assert_eq!(decoded[0].0, 0, "nonce key");
        assert_eq!(
            decoded[0].1, msg,
            "trimmed leaf must right-pad back to the original 141-byte message"
        );
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

    // Decodes the merkle + dispatch state from the committed fixture and
    // checks the cross-field invariants that hold for ANY `night` state,
    // regardless of how many dispatches the fixture captured:
    //   * one stored message per merkle leaf (`len == count`);
    //   * dispatch nonces are contiguous from 0 (the leaf-index domain);
    //   * a local `IncrementalMerkle` of the message ids reproduces the
    //     on-chain `current_root` — the same local-vs-on-chain root check
    //     the validator performs, exercised here over real decoded bytes.
    // For an empty-tree fixture this degenerates to `count == 0` and the
    // root check is skipped; with dispatches present it is a full decode +
    // root-parity test.
    #[test]
    fn decodes_live_night_merkle_state() {
        use hyperlane_core::accumulator::incremental::IncrementalMerkle;

        let hex = include_str!("../tests/fixtures/night-state.hex").trim();
        let bytes = hex::decode(hex).expect("fixture is valid hex");

        let merkle = decode_merkle_state(&bytes).expect("decode merkle state");
        let messages = decode_dispatched_messages(&bytes).expect("decode dispatched messages");

        assert_eq!(
            messages.len(),
            merkle.count as usize,
            "one dispatched message is stored per merkle leaf"
        );
        for (i, (nonce, _)) in messages.iter().enumerate() {
            assert_eq!(*nonce, i as u32, "dispatch nonces are contiguous from 0");
        }

        if merkle.count > 0 {
            let mut tree = IncrementalMerkle::default();
            for (_, message) in &messages {
                tree.ingest(message.id());
            }
            assert_eq!(
                tree.root(),
                merkle.current_root,
                "local merkle root must match the on-chain current_root"
            );
        }
    }

    // Real root-parity check against a fixture captured AFTER two outbound
    // dispatches. Generated by `contracts/tests/utils/generate-dispatch-fixture.ts`
    // (the Compact simulator runs the actual `transferRemote` circuit logic and
    // `ContractState.serialize()` uses the same tagged format the indexer
    // serves — the decoder reads the `.data` fields, which are faithful; the
    // full blob differs in the operations section the decoder skips). Two
    // leaves means the root is a real branch hash `keccak(leaf0 || leaf1)`, not
    // a trivial single-leaf root.
    #[test]
    fn decodes_dispatched_night_merkle_state() {
        use hyperlane_core::accumulator::incremental::IncrementalMerkle;

        let hex = include_str!("../tests/fixtures/night-state-dispatched.hex").trim();
        let bytes = hex::decode(hex).expect("fixture is valid hex");

        let merkle = decode_merkle_state(&bytes).expect("decode merkle state");
        assert_eq!(merkle.count, 2, "two dispatches produce two merkle leaves");

        let messages = decode_dispatched_messages(&bytes).expect("decode dispatched messages");
        assert_eq!(messages.len(), 2, "two dispatched messages are stored");
        assert_eq!(messages[0].0, 0, "first dispatch nonce is 0");
        assert_eq!(messages[1].0, 1, "second dispatch nonce is 1");

        let mut tree = IncrementalMerkle::default();
        for (_, message) in &messages {
            tree.ingest(message.id());
        }
        assert_eq!(
            tree.root(),
            merkle.current_root,
            "local root rebuilt from the two dispatched messages must match on-chain current_root"
        );
    }

    // Decodes the IGP `gas_payments` + `gas_payment_count` state from a
    // committed fixture, generated by
    // `contracts/tests/utils/generate-igp-fixture.ts` (the Compact simulator
    // runs the real `payForGas` circuit logic, then `ContractState.serialize()`
    // produces the same tagged format the indexer serves — the same offline
    // fixture approach as the #15 dispatch/merkle fixtures, because a live
    // `payForGas` proof needs a >12 GB prover the local devnet can't supply).
    //
    // Three rows exercise the full struct decode:
    //   * row 0 — a full 32-byte messageId, multi-byte destination/gasAmount
    //     and a large payment (all four atoms non-empty, full width);
    //   * row 1 — a messageId that trims to one byte on store (0x01 then
    //     zeros) plus an overpayment (exercises the Bytes<32> right-pad);
    //   * row 2 — gasAmount == 0 (an empty MIDDLE atom) and payment == 1 (a
    //     one-byte atom), pinning that a zero field keeps its atom slot so
    //     positional decoding of the following field stays aligned.
    #[test]
    fn decodes_igp_gas_payments_fixture() {
        let hex = include_str!("../tests/fixtures/igp-state.hex").trim();
        let bytes = hex::decode(hex).expect("fixture is valid hex");

        let snapshot = decode_igp_snapshot(&bytes).expect("decode IGP snapshot");
        assert_eq!(snapshot.payment_count, 3, "three payments recorded");
        assert_eq!(snapshot.payments.len(), 3, "three rows decoded");

        // row 0: full 32-byte messageId, quote payment.
        let row0 = snapshot.payments.get(&0).expect("row 0 present");
        assert_eq!(row0.message_id, H256::repeat_byte(0xaa));
        assert_eq!(row0.destination, 99);
        assert_eq!(row0.gas_amount, 100_000);
        assert_eq!(row0.payment, 2_000_000_000_000_000);

        // row 1: messageId trims to a single 0x01 byte on store; the decoder
        // right-pads it back to 32. Overpayment recorded verbatim (no refund).
        let row1 = snapshot.payments.get(&1).expect("row 1 present");
        let mut want_id = [0u8; 32];
        want_id[0] = 0x01;
        assert_eq!(row1.message_id, H256::from(want_id));
        assert_eq!(row1.destination, 99);
        assert_eq!(row1.gas_amount, 100_000);
        assert_eq!(row1.payment, 2_000_000_000_000_007);

        // row 2: zero gasAmount (empty middle atom) + a one-byte payment.
        let row2 = snapshot.payments.get(&2).expect("row 2 present");
        assert_eq!(row2.message_id, H256::repeat_byte(0xcc));
        assert_eq!(row2.destination, 99);
        assert_eq!(row2.gas_amount, 0);
        assert_eq!(row2.payment, 1);
    }
}

//! Native decode of the WarpRoute (`night`) contract's ledger state as served
//! by the Midnight indexer.
//!
//! The contract has no usable generated decoder — its compiled TS `Ledger`
//! type is empty because the ledger fields live in imported modules. So the
//! tagged state is deserialized with the same crates the indexer uses and the
//! resulting `StateValue::Array` is navigated positionally.

use std::collections::BTreeMap;

use hyperlane_core::{ChainResult, HyperlaneMessage, U256};
use midnight_onchain_state::state::{ContractState, StateValue};
use midnight_serialize::tagged_deserialize;
use midnight_storage::merkle_patricia_trie::ALLOW_SHORT_EXTENSION_CHAINS;
use midnight_storage_core::DefaultDB;

use crate::error::HyperlaneMidnightError;

// Positional paths into the ledger `StateValue::Array`. The compiler splits
// night's top-level fields into two groups: root `[0]` holds the three
// ownership fields, `[1]` holds the rest, the Routes, MessageIdMultisigIsm,
// Mailbox and Scale module fields among them. Paths follow each module's
// field declaration order, so inserting or reordering a field shifts them;
// the contracts repo re-checks them on every compile.
//
// `validator_sets` maps an origin domain to one cell holding that origin's
// `ValidatorSet` struct. The key is a little-endian `Uint<32>`. The cell is a
// flat list of atoms: four per key slot, the key's x and y coordinates as two
// `Secp256k1Base` values each, then `count`, then `threshold`. The number of
// slots is a compile-time constant of the contract, so it is taken from the
// cell.
const ISM_VALIDATOR_SETS_PATH: [usize; 2] = [1, 4];
const ISM_MODULE_TYPE_PATH: [usize; 2] = [1, 5];

// Mailbox fields, also under `[1]`. Slot 6 is the `deliveries` set, which is
// not decoded here: deliveries come from `HYP_PROCESS` events and the
// `delivered` read goes through the toolkit.
const MAILBOX_NONCE_PATH: [usize; 2] = [1, 7];
const DISPATCHED_MESSAGES_PATH: [usize; 2] = [1, 8];

/// Length of an encoded `HyperlaneMessage`: version(1) + nonce(4) + origin(4)
/// + sender(32) + destination(4) + recipient(32) + body(64).
pub(crate) const ENCODED_MESSAGE_LEN: usize = 141;

/// One origin chain's validators, as the MessageIdMultisigIsm holds them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatorSet {
    /// Validator addresses, in on-chain slot order. The contract stores each
    /// secp256k1 public key as its coordinates; the address is
    /// `keccak256(x || y)[12..]`, big-endian.
    pub validators: Vec<[u8; 20]>,
    /// Multisig threshold (M of N).
    pub threshold: u8,
    /// Key slots the contract keeps per set, used or not. A delivery carries
    /// one signature slot for each.
    pub slots: usize,
}

/// The MessageIdMultisigIsm configuration read from on-chain state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsmState {
    /// Validator sets by origin domain. The contract checks a message
    /// against the set of the chain it came from.
    pub validator_sets: BTreeMap<u32, ValidatorSet>,
    /// Hyperlane `ModuleType` discriminant (5 = MessageIdMultisig).
    pub module_type: u8,
}

impl IsmState {
    /// The validators that sign for `origin`. An origin with no set delivers
    /// nothing, so a missing one is an error.
    pub fn validator_set(&self, origin: u32) -> ChainResult<&ValidatorSet> {
        self.validator_sets
            .get(&origin)
            .ok_or_else(|| HyperlaneMidnightError::NoValidatorSet(origin).into())
    }
}

/// Deserialize the raw indexer-served state bytes.
///
/// An ordinary on-chain write can leave a trie with a short `Extension` chain,
/// which strict decoding refuses, so the ledger's client-side leniency is set
/// for the decode. Decoding is eager, so nothing is loaded outside this call.
pub fn decode_contract_state(bytes: &[u8]) -> ChainResult<ContractState<DefaultDB>> {
    let mut reader = bytes;
    ALLOW_SHORT_EXTENSION_CHAINS
        .set(&(), || tagged_deserialize(&mut reader))
        .map_err(|e| HyperlaneMidnightError::StateDecode(e.to_string()).into())
}

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

/// Compact integers are little-endian with trailing zero bytes trimmed;
/// `Bytes<N>` is stored verbatim.
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

/// Every atom of a cell that holds a struct, in field order.
fn cell_atoms(node: &StateValue<DefaultDB>) -> ChainResult<Vec<&[u8]>> {
    match node {
        StateValue::Cell(sp) => {
            let aligned = &**sp;
            Ok(aligned.value.0.iter().map(|atom| &atom.0[..]).collect())
        }
        other => Err(HyperlaneMidnightError::StateDecode(format!(
            "expected cell leaf, got {other:?}"
        ))
        .into()),
    }
}

/// A zero `Uint<8>` is stored as an empty atom.
fn u8_from_atom(bytes: &[u8]) -> ChainResult<u8> {
    match bytes.len() {
        0 => Ok(0),
        1 => Ok(bytes[0]),
        n => Err(
            HyperlaneMidnightError::StateDecode(format!("expected u8 leaf, got {n} bytes")).into(),
        ),
    }
}

fn read_u8(node: &StateValue<DefaultDB>) -> ChainResult<u8> {
    u8_from_atom(cell_atom(node)?)
}

/// Atoms per stored key: x and y, two atoms each.
const KEY_ATOMS: usize = 4;

/// secp256k1's base field modulus minus one, big-endian.
const SECP256K1_P_MINUS_ONE: [u8; 32] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe, 0xff, 0xff, 0xfc, 0x2e,
];

/// A `Secp256k1Base` is stored as the low 192 and the high 64 bits of the
/// value minus one, little-endian, so zero is stored as p - 1. Each atom has
/// its trailing zero bytes trimmed.
fn secp256k1_base_from_atoms(low: &[u8], high: &[u8]) -> ChainResult<[u8; 32]> {
    if low.len() > 24 || high.len() > 8 {
        return Err(HyperlaneMidnightError::StateDecode(format!(
            "expected a 24- and an 8-byte coordinate atom, got {} and {} bytes",
            low.len(),
            high.len()
        ))
        .into());
    }
    let mut le = [0u8; 32];
    le[..low.len()].copy_from_slice(low);
    le[24..24 + high.len()].copy_from_slice(high);
    let stored = U256::from_little_endian(&le);
    let p_minus_one = U256::from_big_endian(&SECP256K1_P_MINUS_ONE);
    if stored > p_minus_one {
        return Err(HyperlaneMidnightError::StateDecode(
            "stored coordinate is not a secp256k1 field element".to_string(),
        )
        .into());
    }
    let value = if stored == p_minus_one {
        U256::zero()
    } else {
        stored + 1
    };
    let mut be = [0u8; 32];
    value.to_big_endian(&mut be);
    Ok(be)
}

/// The 64-byte public key (x || y, big-endian) of one stored key slot.
fn pubkey_from_atoms(atoms: &[&[u8]]) -> ChainResult<[u8; 64]> {
    let mut pubkey = [0u8; 64];
    pubkey[..32].copy_from_slice(&secp256k1_base_from_atoms(atoms[0], atoms[1])?);
    pubkey[32..].copy_from_slice(&secp256k1_base_from_atoms(atoms[2], atoms[3])?);
    Ok(pubkey)
}

/// One origin's `ValidatorSet` cell. The contract keeps the first `count`
/// slots as the set and the rest zero, with `1 <= threshold <= count`, so a
/// cell that breaks this means the positional path reads the wrong field,
/// not a legitimate on-chain state.
fn read_validator_set(node: &StateValue<DefaultDB>, origin: u32) -> ChainResult<ValidatorSet> {
    let atoms = cell_atoms(node)?;
    let Some(key_atoms) = atoms.len().checked_sub(2).filter(|n| n % KEY_ATOMS == 0) else {
        return Err(HyperlaneMidnightError::StateDecode(format!(
            "validator set of origin {origin} has {} atoms, expected key slots, count and threshold",
            atoms.len()
        ))
        .into());
    };
    let slots = key_atoms / KEY_ATOMS;
    let (key_atoms, tail) = atoms.split_at(key_atoms);
    let keys = key_atoms
        .chunks(KEY_ATOMS)
        .map(pubkey_from_atoms)
        .collect::<ChainResult<Vec<_>>>()?;
    let count = u8_from_atom(tail[0])? as usize;
    let threshold = u8_from_atom(tail[1])?;

    if count > slots {
        return Err(HyperlaneMidnightError::StateDecode(format!(
            "validator count {count} of origin {origin} exceeds the {slots} key slots"
        ))
        .into());
    }
    if threshold < 1 || threshold as usize > count {
        return Err(HyperlaneMidnightError::StateDecode(format!(
            "threshold {threshold} of origin {origin} out of range 1..={count} (validator count)"
        ))
        .into());
    }
    if let Some(unused) = keys[count..]
        .iter()
        .position(|key| key.iter().any(|byte| *byte != 0))
    {
        return Err(HyperlaneMidnightError::StateDecode(format!(
            "key slot {} of origin {origin} is past the validator count {count} but not zero",
            count + unused
        ))
        .into());
    }

    let mut validators = Vec::with_capacity(count);
    for key in &keys[..count] {
        let digest = ethers::utils::keccak256(key);
        let mut addr = [0u8; 20];
        addr.copy_from_slice(&digest[12..]);
        validators.push(addr);
    }
    Ok(ValidatorSet {
        validators,
        threshold,
        slots,
    })
}

fn read_validator_sets(node: &StateValue<DefaultDB>) -> ChainResult<BTreeMap<u32, ValidatorSet>> {
    let map = match node {
        StateValue::Map(m) => m,
        other => {
            return Err(HyperlaneMidnightError::StateDecode(format!(
                "expected map for validator_sets, got {other:?}"
            ))
            .into())
        }
    };
    let mut sets = BTreeMap::new();
    for entry in map.iter() {
        let pair = &*entry;
        // The key is a little-endian `Uint<32>` origin domain.
        let key_av = &*pair.0;
        let key_bytes = key_av
            .value
            .0
            .first()
            .map(|atom| &atom.0[..])
            .unwrap_or(&[]);
        if key_bytes.len() > 4 {
            return Err(HyperlaneMidnightError::StateDecode(format!(
                "validator_sets key is {} bytes, expected a u32 origin domain",
                key_bytes.len()
            ))
            .into());
        }
        let mut kb = [0u8; 4];
        kb[..key_bytes.len()].copy_from_slice(key_bytes);
        let origin = u32::from_le_bytes(kb);

        sets.insert(origin, read_validator_set(&pair.1, origin)?);
    }
    Ok(sets)
}

/// Decode the MessageIdMultisigIsm config from the serialized ledger state.
pub fn decode_ism_state(bytes: &[u8]) -> ChainResult<IsmState> {
    let cs = decode_contract_state(bytes)?;
    let root = cs.data.get_ref();
    Ok(IsmState {
        validator_sets: read_validator_sets(nav(root, &ISM_VALIDATOR_SETS_PATH)?)?,
        module_type: read_u8(nav(root, &ISM_MODULE_TYPE_PATH)?)?,
    })
}

/// A zero counter is stored as an empty atom.
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

/// The number of messages dispatched so far. The on-chain circuit caps the
/// counter below `2^32`, so the cast cannot truncate a legitimate value.
pub fn decode_nonce_count(bytes: &[u8]) -> ChainResult<u32> {
    let cs = decode_contract_state(bytes)?;
    let root = cs.data.get_ref();
    let n = read_counter_u64(nav(root, &MAILBOX_NONCE_PATH)?)?;
    u32::try_from(n).map_err(|_| {
        HyperlaneMidnightError::StateDecode(format!("nonce counter {n} exceeds u32")).into()
    })
}

/// Sorted by nonce. Each message's encoded nonce must match its map key, the
/// same binding `Mailbox.recordDispatch` enforces on-chain.
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
        // The key is a little-endian `Uint<32>` nonce.
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

        // Trailing zero bytes are trimmed on store, so a message whose tail is
        // zero (a scaled amount ending in 0x00, say) comes back short and has
        // to be right-padded before decoding.
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

#[cfg(test)]
mod tests {
    use super::*;

    use hyperlane_core::{Encode as _, H256};
    use midnight_base_crypto::cost_model::RunningCost;
    use midnight_base_crypto::fab::{AlignedValue, ValueAtom};
    use midnight_onchain_state::state::ChargedState;
    use midnight_storage::arena::Sp;
    use midnight_storage::storage::{Array, HashMap};

    fn addr(hex: &str) -> [u8; 20] {
        let v = hex::decode(hex.trim_start_matches("0x")).unwrap();
        <[u8; 20]>::try_from(v.as_slice()).unwrap()
    }

    fn cell<V: Into<AlignedValue>>(value: V) -> StateValue<DefaultDB> {
        StateValue::Cell(Sp::new(value.into()))
    }

    fn array(values: Vec<StateValue<DefaultDB>>) -> StateValue<DefaultDB> {
        StateValue::Array(Array::from(values))
    }

    /// Laid out so the Mailbox paths line up under `[1]`.
    fn mailbox_root(
        deliveries: StateValue<DefaultDB>,
        nonce: StateValue<DefaultDB>,
        dispatched: StateValue<DefaultDB>,
    ) -> StateValue<DefaultDB> {
        let mailbox = array(vec![
            StateValue::Null, // [1,0]
            StateValue::Null, // [1,1]
            StateValue::Null, // [1,2]
            StateValue::Null, // [1,3]
            StateValue::Null, // [1,4]
            StateValue::Null, // [1,5]
            deliveries,       // [1,6]
            nonce,            // [1,7]
            dispatched,       // [1,8]
        ]);
        array(vec![
            StateValue::Null, // [0] ownership + Routes fields (unused here)
            mailbox,          // [1] ISM/Mailbox module fields
        ])
    }

    /// Produces the same tagged wire bytes the indexer serves.
    fn state_bytes(data: ChargedState<DefaultDB>) -> Vec<u8> {
        let cs = ContractState::<DefaultDB> {
            data,
            ..ContractState::default()
        };
        let mut bytes = Vec::new();
        midnight_serialize::tagged_serialize(&cs, &mut bytes).expect("serialize synthetic state");
        bytes
    }

    fn mailbox_state_bytes(
        deliveries: StateValue<DefaultDB>,
        nonce: StateValue<DefaultDB>,
        dispatched: StateValue<DefaultDB>,
    ) -> Vec<u8> {
        state_bytes(ChargedState::new(mailbox_root(
            deliveries, nonce, dispatched,
        )))
    }

    fn sample_message(nonce: u32) -> HyperlaneMessage {
        HyperlaneMessage {
            version: 3,
            nonce,
            origin: 1234,
            sender: H256::repeat_byte(0xAB),
            destination: 5678,
            recipient: H256::repeat_byte(0xCD),
            // Trailing zeros exercise the right-pad path.
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
        let bytes = mailbox_state_bytes(
            StateValue::Map(HashMap::new()),
            cell(0u64),
            StateValue::Map(HashMap::new()),
        );
        assert_eq!(decode_nonce_count(&bytes).expect("decode nonce"), 0);
    }

    // A small state is stored in-line, so its charge record is keyed by the
    // whole root. An update that collects nothing keeps the old root beside
    // the new one, and the two long keys leave a short `Extension` chain.
    #[test]
    fn decodes_state_with_a_short_extension_chain() {
        let root = |nonce: u64| {
            mailbox_root(
                StateValue::Map(HashMap::new()),
                cell(nonce),
                StateValue::Map(HashMap::new()),
            )
        };
        let (data, _) =
            ChargedState::new(root(7)).update(root(8), |_, _| RunningCost::default(), |_| 0);
        let bytes = state_bytes(data);

        let strict: std::io::Result<ContractState<DefaultDB>> = tagged_deserialize(&mut &bytes[..]);
        let refusal = strict
            .expect_err("strict decoding refuses the chain")
            .to_string();
        assert!(
            refusal.contains("Node::Extension path must be of length 255"),
            "refused for the chain: {refusal}"
        );

        assert_eq!(decode_nonce_count(&bytes).expect("decode nonce"), 8);
    }

    #[test]
    fn decodes_dispatched_messages_sorted_by_nonce() {
        // The merkle reconstruction ingests leaves in nonce order, so a wrong
        // key parse or ordering bug would produce a different root.
        let mut map = HashMap::<AlignedValue, StateValue<DefaultDB>, DefaultDB>::new();
        let mut expected = Vec::new();
        for nonce in [7u32, 0, 42, 1] {
            let msg = sample_message(nonce);
            let full: [u8; ENCODED_MESSAGE_LEN] = msg.to_vec().try_into().unwrap();
            map = map.insert(AlignedValue::from(nonce), cell(full));
            expected.push((nonce, msg));
        }
        expected.sort_by_key(|(nonce, _)| *nonce);
        let bytes = mailbox_state_bytes(
            StateValue::Map(HashMap::new()),
            cell(43u64),
            StateValue::Map(map),
        );

        let decoded = decode_dispatched_messages(&bytes).expect("decode dispatched messages");
        assert_eq!(decoded, expected, "every message, sorted by nonce");
    }

    #[test]
    fn decode_dispatched_messages_right_pads_trimmed_value() {
        // A real dispatch whose scaled amount ends in zero bytes is stored
        // shorter than 141 (a 6->18-decimal route scales 10^5 to 10^17, which
        // ends in 0x0000). An all-zero body trims the most and pins the pad
        // branch.
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

    /// Laid out so the ISM paths line up under `[1]`.
    fn ism_state_bytes(validator_sets: StateValue<DefaultDB>, module_type: u8) -> Vec<u8> {
        let fields = array(vec![
            StateValue::Null,  // [1,0]
            StateValue::Null,  // [1,1]
            StateValue::Null,  // [1,2]
            StateValue::Null,  // [1,3]
            validator_sets,    // [1,4]
            cell(module_type), // [1,5]
        ]);
        let root = array(vec![
            StateValue::Null, // [0] ownership fields (unused here)
            fields,           // [1] every other field
        ]);
        let cs = ContractState::<DefaultDB> {
            data: ChargedState::new(root),
            ..ContractState::default()
        };
        let mut bytes = Vec::new();
        midnight_serialize::tagged_serialize(&cs, &mut bytes).expect("serialize synthetic state");
        bytes
    }

    /// The contract's own slot count. The decoder takes it from the cell, so
    /// a test is free to use another.
    const SLOTS: usize = 9;

    /// A coordinate the way the runtime stores a `Secp256k1Base`.
    fn coordinate_atoms(be: &[u8]) -> [AlignedValue; 2] {
        let value = U256::from_big_endian(be);
        let stored = if value.is_zero() {
            U256::from_big_endian(&SECP256K1_P_MINUS_ONE)
        } else {
            value - 1
        };
        let mut le = [0u8; 32];
        stored.to_little_endian(&mut le);
        let low: [u8; 24] = le[..24].try_into().unwrap();
        let high: [u8; 8] = le[24..].try_into().unwrap();
        [AlignedValue::from(low), AlignedValue::from(high)]
    }

    /// A `ValidatorSet` cell the way the contract stores it: four atoms per
    /// key slot, then `count` and `threshold`.
    fn validator_set_cell(slots: &[[u8; 64]], count: u8, threshold: u8) -> StateValue<DefaultDB> {
        let mut fields: Vec<AlignedValue> = slots
            .iter()
            .flat_map(|key| {
                let [x_low, x_high] = coordinate_atoms(&key[..32]);
                let [y_low, y_high] = coordinate_atoms(&key[32..]);
                [x_low, x_high, y_low, y_high]
            })
            .collect();
        fields.push(AlignedValue::from(count));
        fields.push(AlignedValue::from(threshold));
        cell(AlignedValue::concat(fields.iter()))
    }

    /// `keys` followed by zero slots up to `slots`, as the contract pads a set.
    fn padded(keys: &[[u8; 64]], slots: usize) -> Vec<[u8; 64]> {
        let mut out = keys.to_vec();
        out.resize(slots, [0u8; 64]);
        out
    }

    fn validator_sets(entries: Vec<(u32, StateValue<DefaultDB>)>) -> StateValue<DefaultDB> {
        let mut map = HashMap::<AlignedValue, StateValue<DefaultDB>, DefaultDB>::new();
        for (origin, set) in entries {
            map = map.insert(AlignedValue::from(origin), set);
        }
        StateValue::Map(map)
    }

    /// State with one origin whose set cell is `set`.
    fn single_set_state_bytes(set: StateValue<DefaultDB>) -> Vec<u8> {
        ism_state_bytes(validator_sets(vec![(31337, set)]), 5)
    }

    fn decode_error(bytes: &[u8]) -> String {
        decode_ism_state(bytes)
            .expect_err("the state must be refused")
            .to_string()
    }

    /// The derivation under test is cross-checked against ethers' own
    /// key -> address path, so it cannot silently drift.
    fn validator_keypair(priv_hex: &str) -> ([u8; 64], [u8; 20]) {
        use ethers::core::k256::elliptic_curve::sec1::ToEncodedPoint;
        use ethers::core::k256::PublicKey;
        use ethers::signers::{LocalWallet, Signer};

        let wallet: LocalWallet = priv_hex.parse().unwrap();
        let point = PublicKey::from(&wallet.signer().verifying_key()).to_encoded_point(false);
        assert_eq!(point.as_bytes()[0], 0x04, "uncompressed SEC1 tag");
        let pubkey: [u8; 64] = point.as_bytes()[1..].try_into().unwrap();
        let derived: [u8; 20] = ethers::utils::keccak256(pubkey)[12..].try_into().unwrap();
        assert_eq!(derived, wallet.address().0, "keccak256(pubkey)[12..]");
        (pubkey, derived)
    }

    // Each origin has its own set and threshold, and addresses come back in
    // slot order.
    #[test]
    fn decodes_synthetic_validator_sets_per_origin() {
        let (pk0, addr0) =
            validator_keypair("1111111111111111111111111111111111111111111111111111111111111111");
        let (pk1, addr1) =
            validator_keypair("2222222222222222222222222222222222222222222222222222222222222222");
        let (pk2, addr2) =
            validator_keypair("3333333333333333333333333333333333333333333333333333333333333333");

        let sets = validator_sets(vec![
            (
                31337,
                validator_set_cell(&padded(&[pk1, pk2, pk0], SLOTS), 3, 2),
            ),
            (11155111, validator_set_cell(&padded(&[pk0], SLOTS), 1, 1)),
        ]);
        let bytes = ism_state_bytes(sets, 5);

        let ism = decode_ism_state(&bytes).expect("decode ISM state");
        assert_eq!(ism.module_type, 5, "module_type");
        assert_eq!(
            ism.validator_sets.keys().copied().collect::<Vec<_>>(),
            vec![31337, 11155111],
            "one set per origin"
        );
        assert_eq!(
            ism.validator_sets[&31337],
            ValidatorSet {
                validators: vec![addr1, addr2, addr0],
                threshold: 2,
                slots: SLOTS,
            },
            "addresses derived from the 64-byte pubkeys, in slot order"
        );
        assert_eq!(
            ism.validator_sets[&11155111],
            ValidatorSet {
                validators: vec![addr0],
                threshold: 1,
                slots: SLOTS,
            },
            "the other origin's set is its own"
        );
    }

    // The slot count is the contract's compile-time choice, so a set with a
    // different one decodes the same way.
    #[test]
    fn takes_the_slot_count_from_the_cell() {
        let (pk0, addr0) =
            validator_keypair("1111111111111111111111111111111111111111111111111111111111111111");
        let (pk1, addr1) =
            validator_keypair("2222222222222222222222222222222222222222222222222222222222222222");

        for slots in [2usize, 4, 12] {
            let bytes =
                single_set_state_bytes(validator_set_cell(&padded(&[pk0, pk1], slots), 2, 1));
            let ism = decode_ism_state(&bytes).expect("decode ISM state");
            assert_eq!(
                ism.validator_sets[&31337],
                ValidatorSet {
                    validators: vec![addr0, addr1],
                    threshold: 1,
                    slots,
                },
                "{slots} key slots"
            );
        }
    }

    // A coordinate with a zero top byte is stored in short atoms, and about
    // one key in 128 has one. Synthetic bytes rather than a real curve point:
    // the decoder never validates the point. x = 1 stores two empty atoms and
    // y a one-byte one. A full key follows it, so a short atom must not shift
    // the rest.
    #[test]
    fn decodes_coordinates_stored_in_short_atoms() {
        let mut short = [0u8; 64];
        short[31] = 0x01;
        short[63] = 0x5A;
        let [x_low, x_high] = coordinate_atoms(&short[..32]);
        let [y_low, _] = coordinate_atoms(&short[32..]);
        assert_eq!(
            x_low.value.0[0].0.len(),
            0,
            "x - 1 = 0 stores an empty atom"
        );
        assert_eq!(x_high.value.0[0].0.len(), 0, "and an empty high atom");
        assert_eq!(y_low.value.0[0].0.len(), 1, "y - 1 stores one byte");
        let full = [0x77u8; 64];
        let expected: Vec<[u8; 20]> = [short, full]
            .iter()
            .map(|key| ethers::utils::keccak256(key)[12..].try_into().unwrap())
            .collect();

        let bytes =
            single_set_state_bytes(validator_set_cell(&padded(&[short, full], SLOTS), 2, 2));

        let ism = decode_ism_state(&bytes).expect("decode ISM state");
        assert_eq!(
            ism.validator_sets[&31337].validators, expected,
            "short atoms must pad back to the full coordinates before hashing"
        );
    }

    // A set the compiled contract stored through the runtime: three keys and
    // an empty slot, count 3, threshold 2. The atoms come from the runtime,
    // not from `coordinate_atoms`, so the encoding is pinned on both sides.
    #[test]
    fn decodes_a_set_stored_by_the_runtime() {
        let hex_atoms = [
            "a971580704b7f0b09a855fcab2b54b68905d61b9ce3cef28",
            "f70accb7dc5b354f",
            "c0f1b2dbfbf0072bbe4a2cfed65634a02bcf3f68d95474a6",
            "9c80ad8e1b6b5b38",
            "263fae1b5f2822cf4949a179786104483480b50b87d1a009",
            "cbe563e5ca7f6d46",
            "8927e19777403ef344d0779e3a4f78e265c837dc3845daee",
            "f831643c6c172867",
            "b0b1c73525bb866391851d8acf707e6a382ae97f4dc9f094",
            "af09df4fdbad723c",
            "df0357e54ddad038fe25a2edea086eadd36c67e18ab2c18f",
            "5d6685e00f6b303b",
            "2efcfffffeffffffffffffffffffffffffffffffffffffff",
            "ffffffffffffffff",
            "2efcfffffeffffffffffffffffffffffffffffffffffffff",
            "ffffffffffffffff",
        ];
        let mut fields: Vec<AlignedValue> = hex_atoms
            .iter()
            .map(|h| {
                let bytes = hex::decode(h).unwrap();
                match bytes.len() {
                    24 => AlignedValue::from(<[u8; 24]>::try_from(bytes.as_slice()).unwrap()),
                    8 => AlignedValue::from(<[u8; 8]>::try_from(bytes.as_slice()).unwrap()),
                    n => panic!("unexpected atom length {n}"),
                }
            })
            .collect();
        fields.push(AlignedValue::from(3u8));
        fields.push(AlignedValue::from(2u8));
        let set = cell(AlignedValue::concat(fields.iter()));

        let ism = decode_ism_state(&single_set_state_bytes(set)).expect("decode ISM state");
        assert_eq!(
            ism.validator_sets[&31337],
            ValidatorSet {
                validators: vec![
                    addr("19e7e376e7c213b7e7e7e46cc70a5dd086daff2a"),
                    addr("1563915e194d8cfba1943570603f7606a3115508"),
                    addr("5cbdd86a2fa8dc4bddd8a8f69dba48572eec07fb"),
                ],
                threshold: 2,
                slots: 4,
            }
        );
    }

    #[test]
    fn decodes_empty_validator_sets() {
        let bytes = ism_state_bytes(validator_sets(vec![]), 5);

        let ism = decode_ism_state(&bytes).expect("decode ISM state");
        assert!(ism.validator_sets.is_empty(), "no origin has a set");
        assert_eq!(ism.module_type, 5, "module_type");
    }

    // The contract never stores any of these, so each one means the path
    // points at the wrong field.
    #[test]
    fn rejects_inconsistent_validator_set() {
        let key = [0x5Au8; 64];
        let other = [0x77u8; 64];

        let zero_threshold = validator_set_cell(&padded(&[key, other], SLOTS), 2, 0);
        assert!(
            decode_error(&single_set_state_bytes(zero_threshold)).contains("threshold 0"),
            "threshold 0"
        );

        let threshold_above_count = validator_set_cell(&padded(&[key, other], SLOTS), 2, 3);
        assert!(
            decode_error(&single_set_state_bytes(threshold_above_count)).contains("threshold 3"),
            "threshold above the count"
        );

        let empty_set = validator_set_cell(&padded(&[], SLOTS), 0, 0);
        assert!(
            decode_error(&single_set_state_bytes(empty_set)).contains("threshold 0"),
            "a stored set has at least one validator"
        );

        let count_above_slots = validator_set_cell(&[key, other], 3, 1);
        assert!(
            decode_error(&single_set_state_bytes(count_above_slots))
                .contains("exceeds the 2 key slots"),
            "count above the slots"
        );

        let key_past_count = validator_set_cell(&padded(&[key, other], SLOTS), 1, 1);
        assert!(
            decode_error(&single_set_state_bytes(key_past_count))
                .contains("key slot 1 of origin 31337"),
            "non-zero key past the count"
        );

        let wide_atom = cell(AlignedValue::concat(
            [
                AlignedValue::from([0x5Au8; 25]),
                AlignedValue::from([0x5Au8; 8]),
                AlignedValue::from([0x5Au8; 24]),
                AlignedValue::from([0x5Au8; 8]),
                AlignedValue::from(1u8),
                AlignedValue::from(1u8),
            ]
            .iter(),
        ));
        assert!(
            decode_error(&single_set_state_bytes(wide_atom)).contains("got 25 and 8 bytes"),
            "a coordinate atom wider than 24 bytes"
        );

        let above_field = cell(AlignedValue::concat(
            [
                AlignedValue::from([0xFFu8; 24]),
                AlignedValue::from([0xFFu8; 8]),
                AlignedValue::from([0x5Au8; 24]),
                AlignedValue::from([0x5Au8; 8]),
                AlignedValue::from(1u8),
                AlignedValue::from(1u8),
            ]
            .iter(),
        ));
        assert!(
            decode_error(&single_set_state_bytes(above_field))
                .contains("not a secp256k1 field element"),
            "a coordinate at or above the field modulus"
        );

        let partial_key = cell(AlignedValue::concat(
            [
                AlignedValue::from([0x5Au8; 24]),
                AlignedValue::from([0x5Au8; 8]),
                AlignedValue::from([0x5Au8; 24]),
                AlignedValue::from(1u8),
                AlignedValue::from(1u8),
            ]
            .iter(),
        ));
        assert!(
            decode_error(&single_set_state_bytes(partial_key)).contains("has 5 atoms"),
            "key atoms that do not come in fours"
        );

        let too_short = cell(7u8);
        assert!(
            decode_error(&single_set_state_bytes(too_short)).contains("has 1 atoms"),
            "a cell too short to be a set"
        );
    }

    // A cell where the map should be, as in a contract that predates
    // per-origin sets.
    #[test]
    fn rejects_state_without_a_validator_sets_map() {
        let bytes = ism_state_bytes(cell(1u8), 5);
        assert!(
            decode_error(&bytes).contains("expected map for validator_sets"),
            "a cell at the validator_sets path"
        );
    }

    #[test]
    fn rejects_origin_key_wider_than_u32() {
        let sets = HashMap::<AlignedValue, StateValue<DefaultDB>, DefaultDB>::new().insert(
            AlignedValue::from(1u64 << 32),
            validator_set_cell(&padded(&[[0x5Au8; 64]], SLOTS), 1, 1),
        );
        let bytes = ism_state_bytes(StateValue::Map(sets), 5);
        assert!(
            decode_error(&bytes).contains("expected a u32 origin domain"),
            "a key wider than an origin domain"
        );
    }

    /// The values recorded beside the fixture, from the inputs that made the
    /// state rather than from decoding it.
    #[derive(serde::Deserialize)]
    struct ExpectedState {
        ism: ExpectedIsm,
        nonce: u32,
        messages: Vec<ExpectedMessage>,
    }

    #[derive(serde::Deserialize)]
    struct ExpectedIsm {
        #[serde(rename = "moduleType")]
        module_type: u8,
        #[serde(rename = "maxValidators")]
        max_validators: usize,
        #[serde(rename = "validatorSets")]
        validator_sets: Vec<ExpectedValidatorSet>,
    }

    #[derive(serde::Deserialize)]
    struct ExpectedValidatorSet {
        origin: u32,
        pubkeys: Vec<String>,
        validators: Vec<String>,
        threshold: u8,
    }

    #[derive(serde::Deserialize)]
    struct ExpectedMessage {
        nonce: u32,
        encoded: String,
        #[serde(rename = "messageId")]
        message_id: String,
    }

    fn unhex(s: &str) -> Vec<u8> {
        hex::decode(s.trim_start_matches("0x")).expect("valid hex")
    }

    // Real `night` state, made by running the contracts repo's compiled
    // contract: given a set for each of two origins, one holding a key whose
    // x coordinate starts with a zero byte, then two transfers out whose
    // stored messages end in one. It is
    // serialized the way the indexer serves it. The ledger trims trailing
    // zero bytes on store, so every value here has to come back whole.
    #[test]
    fn decodes_real_night_state() {
        let bytes = unhex(include_str!("../tests/fixtures/night-state.hex").trim());
        let expected: ExpectedState =
            serde_json::from_str(include_str!("../tests/fixtures/night-state.expected.json"))
                .expect("expected values are valid JSON");

        let ism = decode_ism_state(&bytes).expect("decode ISM state");
        assert_eq!(ism.module_type, expected.ism.module_type, "module_type");
        assert_eq!(
            ism.validator_sets.len(),
            expected.ism.validator_sets.len(),
            "one set per origin"
        );
        assert!(
            expected.ism.validator_sets.len() >= 2,
            "the fixture holds more than one origin"
        );
        let mut short_coordinate_keys = 0;
        for want in &expected.ism.validator_sets {
            let set = ism
                .validator_set(want.origin)
                .unwrap_or_else(|e| panic!("origin {}: {e}", want.origin));
            let validators: Vec<[u8; 20]> = want.validators.iter().map(|v| addr(v)).collect();
            assert_eq!(
                set.validators, validators,
                "origin {}: validator addresses in slot order",
                want.origin
            );
            assert_eq!(
                set.threshold, want.threshold,
                "origin {}: threshold",
                want.origin
            );
            assert_eq!(
                set.slots, expected.ism.max_validators,
                "origin {}: key slots",
                want.origin
            );
            short_coordinate_keys += want
                .pubkeys
                .iter()
                .filter(|key| {
                    let key = unhex(key);
                    key[0] == 0 || key[32] == 0
                })
                .count();
        }
        assert!(
            short_coordinate_keys > 0,
            "the fixture holds a key with a coordinate whose top byte is zero"
        );

        assert_eq!(
            decode_nonce_count(&bytes).expect("decode nonce"),
            expected.nonce,
            "nonce"
        );

        let messages = decode_dispatched_messages(&bytes).expect("decode dispatched messages");
        assert_eq!(
            messages.len(),
            expected.messages.len(),
            "every dispatched message"
        );
        for ((nonce, message), want) in messages.iter().zip(&expected.messages) {
            assert_eq!(*nonce, want.nonce, "map key");
            let encoded = unhex(&want.encoded);
            assert_eq!(
                encoded.last().copied(),
                Some(0u8),
                "the fixture's message ends in a zero byte"
            );
            assert_eq!(
                message.to_vec(),
                encoded,
                "message {nonce} byte for byte, trailing zero restored"
            );
            assert_eq!(
                message.id().as_bytes(),
                unhex(&want.message_id).as_slice(),
                "message {nonce} id"
            );
        }
    }
}

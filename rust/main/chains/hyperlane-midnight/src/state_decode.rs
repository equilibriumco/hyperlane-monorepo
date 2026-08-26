//! Native decode of the WarpRoute (`night`) contract's ledger state as served
//! by the Midnight indexer.
//!
//! The contract has no usable generated decoder — its compiled TS `Ledger`
//! type is empty because the ledger fields live in imported modules. So the
//! tagged state is deserialized with the same crates the indexer uses and the
//! resulting `StateValue::Array` is navigated positionally.

use hyperlane_core::{ChainResult, HyperlaneMessage};
use midnight_onchain_state::state::{ContractState, StateValue};
use midnight_serialize::tagged_deserialize;
use midnight_storage_core::DefaultDB;

use crate::error::HyperlaneMidnightError;

// Positional paths into the ledger `StateValue::Array`. Root `[0]` holds the
// ownership commitment and the Routes fields, `[1]` the
// MessageIdMultisigIsm / Mailbox / Scale module fields. Paths follow each
// module's field declaration order, so inserting or reordering a field shifts
// them; the contracts repo re-checks them on every compile.
const ISM_VALIDATORS_PATH: [usize; 2] = [1, 0];
const ISM_VALIDATOR_COUNT_PATH: [usize; 2] = [1, 1];
const ISM_THRESHOLD_PATH: [usize; 2] = [1, 2];
const ISM_MODULE_TYPE_PATH: [usize; 2] = [1, 3];

// Mailbox fields, also under `[1]`. Slot 7 is the `deliveries` set, which is
// not decoded here: deliveries come from `HYP_PROCESS` events and the
// `delivered` read goes through the toolkit.
const MAILBOX_NONCE_PATH: [usize; 2] = [1, 8];
const DISPATCHED_MESSAGES_PATH: [usize; 2] = [1, 9];

/// Length of an encoded `HyperlaneMessage`: version(1) + nonce(4) + origin(4)
/// + sender(32) + destination(4) + recipient(32) + body(64).
pub(crate) const ENCODED_MESSAGE_LEN: usize = 141;

/// The MessageIdMultisigIsm configuration read from on-chain state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsmState {
    /// Validator addresses, in on-chain slot order. The registry stores
    /// 64-byte secp256k1 public keys; the address is
    /// `keccak256(pubkey)[12..]`.
    pub validators: Vec<[u8; 20]>,
    /// Number of populated validator slots.
    pub validator_count: u8,
    /// Multisig threshold (M of N).
    pub threshold: u8,
    /// Hyperlane `ModuleType` discriminant (5 = MessageIdMultisig).
    pub module_type: u8,
}

/// Deserialize the raw indexer-served state bytes.
pub fn decode_contract_state(bytes: &[u8]) -> ChainResult<ContractState<DefaultDB>> {
    let mut reader = bytes;
    tagged_deserialize(&mut reader)
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

/// A zero `Uint<8>` is stored as an empty atom.
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

/// A stored `Bytes<N>` leaf has its trailing zero bytes trimmed, so a key ending
/// in zeros comes back short and has to be right-padded.
fn read_bytes64(node: &StateValue<DefaultDB>) -> ChainResult<[u8; 64]> {
    let bytes = cell_atom(node)?;
    if bytes.len() > 64 {
        return Err(HyperlaneMidnightError::StateDecode(format!(
            "expected 64-byte secp256k1 public key, got {} bytes",
            bytes.len()
        ))
        .into());
    }
    let mut pubkey = [0u8; 64];
    pubkey[..bytes.len()].copy_from_slice(bytes);
    Ok(pubkey)
}

/// Ascending slot-index order is what the on-chain multisig expects.
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
        let key_av = &*pair.0;
        let idx = key_av
            .value
            .0
            .first()
            .and_then(|atom| atom.0.first())
            .copied()
            .unwrap_or(0);
        let pubkey = read_bytes64(&pair.1)?;
        let digest = ethers::utils::keccak256(pubkey);
        let mut addr = [0u8; 20];
        addr.copy_from_slice(&digest[12..]);
        entries.push((idx, addr));
    }
    entries.sort_by_key(|(idx, _)| *idx);
    Ok(entries.into_iter().map(|(_, addr)| addr).collect())
}

/// Decode the MessageIdMultisigIsm config from the serialized ledger state.
pub fn decode_ism_state(bytes: &[u8]) -> ChainResult<IsmState> {
    let cs = decode_contract_state(bytes)?;
    let root = cs.data.get_ref();
    let state = IsmState {
        validators: read_validators(nav(root, &ISM_VALIDATORS_PATH)?)?,
        validator_count: read_u8(nav(root, &ISM_VALIDATOR_COUNT_PATH)?)?,
        threshold: read_u8(nav(root, &ISM_THRESHOLD_PATH)?)?,
        module_type: read_u8(nav(root, &ISM_MODULE_TYPE_PATH)?)?,
    };

    // Mutually inconsistent slots mean the positional paths read the wrong
    // fields, not a legitimate on-chain state.
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

    /// Produces the same tagged wire bytes the indexer serves.
    fn mailbox_state_bytes(
        deliveries: StateValue<DefaultDB>,
        nonce: StateValue<DefaultDB>,
        dispatched: StateValue<DefaultDB>,
    ) -> Vec<u8> {
        let mailbox = array(vec![
            StateValue::Null, // [1,0]
            StateValue::Null, // [1,1]
            StateValue::Null, // [1,2]
            StateValue::Null, // [1,3]
            StateValue::Null, // [1,4]
            StateValue::Null, // [1,5]
            StateValue::Null, // [1,6]
            deliveries,       // [1,7]
            nonce,            // [1,8]
            dispatched,       // [1,9]
        ]);
        let root = array(vec![
            StateValue::Null, // [0] ownership + Routes fields (unused here)
            mailbox,          // [1] ISM/Mailbox module fields
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

    /// Laid out so the ISM paths line up.
    fn ism_state_bytes(
        validators: StateValue<DefaultDB>,
        validator_count: u8,
        threshold: u8,
        module_type: u8,
    ) -> Vec<u8> {
        let ism = array(vec![
            validators,            // [1,0]
            cell(validator_count), // [1,1]
            cell(threshold),       // [1,2]
            cell(module_type),     // [1,3]
        ]);
        let root = array(vec![
            StateValue::Null, // [0] ownership + Routes fields (unused here)
            ism,              // [1] ISM/Mailbox module fields
        ]);
        let cs = ContractState::<DefaultDB> {
            data: ChargedState::new(root),
            ..ContractState::default()
        };
        let mut bytes = Vec::new();
        midnight_serialize::tagged_serialize(&cs, &mut bytes).expect("serialize synthetic state");
        bytes
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

    // Addresses must come back in ascending slot-index order regardless of the
    // map's iteration order, so the entries are inserted out of order here.
    #[test]
    fn decodes_synthetic_ism_state_from_pubkey_registry() {
        let (pk0, addr0) =
            validator_keypair("1111111111111111111111111111111111111111111111111111111111111111");
        let (pk1, addr1) =
            validator_keypair("2222222222222222222222222222222222222222222222222222222222222222");

        let map = HashMap::<AlignedValue, StateValue<DefaultDB>, DefaultDB>::new()
            .insert(AlignedValue::from(1u8), cell(pk1))
            .insert(AlignedValue::from(0u8), cell(pk0));
        let bytes = ism_state_bytes(StateValue::Map(map), 2, 2, 5);

        let ism = decode_ism_state(&bytes).expect("decode ISM state");
        assert_eq!(ism.module_type, 5, "module_type");
        assert_eq!(ism.threshold, 2, "threshold");
        assert_eq!(ism.validator_count, 2, "validator_count");
        assert_eq!(
            ism.validators,
            vec![addr0, addr1],
            "addresses derived from the 64-byte pubkeys, in slot order"
        );
    }

    // A pubkey whose Y coordinate ends in 0x00 is stored short, and roughly
    // one key in 256 does. Synthetic bytes rather than a real curve point: the
    // decoder never validates the point, and a forced zero tail pins the pad
    // branch.
    #[test]
    fn decodes_pubkey_with_trailing_zeros_trimmed_on_store() {
        let mut pubkey = [0x5Au8; 64];
        pubkey[61..].fill(0);
        assert_eq!(
            ValueAtom::from(pubkey).0.len(),
            61,
            "expected the trailing-zero tail to be trimmed on store"
        );
        let expected: [u8; 20] = ethers::utils::keccak256(pubkey)[12..].try_into().unwrap();

        let map = HashMap::<AlignedValue, StateValue<DefaultDB>, DefaultDB>::new()
            .insert(AlignedValue::from(0u8), cell(pubkey));
        let bytes = ism_state_bytes(StateValue::Map(map), 1, 1, 5);

        let ism = decode_ism_state(&bytes).expect("decode ISM state");
        assert_eq!(
            ism.validators,
            vec![expected],
            "trimmed leaf must right-pad back to the full 64-byte pubkey before hashing"
        );
    }

    // Real `night` state captured from a devnet indexer. Ignored because the
    // committed blob predates the switch to 64-byte pubkey validators, so it
    // decodes to garbage addresses. Regenerating it from a deploy of the same
    // three keys leaves the assertions below valid.
    #[test]
    #[ignore = "night-state.hex predates the 64-byte pubkey validator registry; regenerate from a fresh deploy of the same 3 validators"]
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

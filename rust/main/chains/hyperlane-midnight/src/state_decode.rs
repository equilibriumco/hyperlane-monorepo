//! Native Rust decode of the WarpRoute (`night`) contract's ledger state
//! as served by the Midnight indexer (the `contractAction.state` hex blob).
//!
//! The `night` contract has no usable generated decoder — its compiled TS
//! `Ledger` type is empty because its ledger fields live in imported
//! modules. So we deserialize the tagged ledger state with the same crates
//! the indexer uses (`midnight-onchain-state` / `midnight-serialize`) and
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

use hyperlane_core::{ChainResult, HyperlaneMessage};
use midnight_onchain_state::state::{ContractState, StateValue};
use midnight_serialize::tagged_deserialize;
use midnight_storage_core::DefaultDB;

use crate::error::HyperlaneMidnightError;

// Positional paths into the ledger `StateValue::Array`, from the compiled
// `night` readers. `night`'s state is a 2-element root array: `[0]` holds the
// ownership commitment + Routes `local_domain` scalars, `[1]` holds the
// Routes / MessageIdMultisigIsm / Mailbox module maps and scalars. The on-chain
// incremental merkle tree was removed (the validator reconstructs it off-chain
// from `dispatched_messages`), so there is no merkle `count` / `current_root`
// slot to decode.
//
// The MessageIdMultisigIsm fields are consecutive slots under `[1]`, in
// source-declaration order: validators(1), validator_count(2), threshold(3),
// module_type(4).
const ISM_VALIDATORS_PATH: [usize; 2] = [1, 1];
const ISM_VALIDATOR_COUNT_PATH: [usize; 2] = [1, 2];
const ISM_THRESHOLD_PATH: [usize; 2] = [1, 3];
const ISM_MODULE_TYPE_PATH: [usize; 2] = [1, 4];

// The Mailbox fields also live under `[1]`: deliveries(8), nonce(9),
// dispatched_messages(10). Verified two ways:
//   (1) The compiled `night` readers in `managed/night/contract/index.js`
//       index these exact slots: `isDelivered`/`deliveryCount` -> `[1, 8]`
//       (`deliveries` Set, `member`/`size`), `nonceValue` -> `[1, 9]`
//       (`nonce` Counter, `popeq`), `messageAt` -> `[1, 10]` (the
//       `dispatched_messages` Map, keyed `member`/`idx`).
//   (2) Decoding a fresh post-dispatch state: root `[1]` is a 15-element array;
//       `[1, 8]` is a Set, `[1, 9]` a Counter cell, `[1, 10]` a `Bytes<141>`
//       Map. Matches the declaration order in `modules/Mailbox.compact`.
// The paths are pinned to the compiled layout; adding/removing a module or
// reordering a field shifts them (removing the merkle module is exactly what
// moved these from their pre-removal `[1, 2..4]` slots). The contracts-repo
// layout guard re-checks the `queryLedgerState` paths on every compile.
// (The `deliveries` set at `[1, 8]` is no longer decoded: deliveries are
// indexed from `HYP_PROCESS` events since #95, and the Mailbox `delivered`
// read goes through the toolkit.)
const MAILBOX_NONCE_PATH: [usize; 2] = [1, 9];
const DISPATCHED_MESSAGES_PATH: [usize; 2] = [1, 10];

/// Length in bytes of an encoded `HyperlaneMessage` stored in the
/// `dispatched_messages` map (`Bytes<141>`) and carried in `HYP_DISPATCH`
/// event payloads: version(1) + nonce(4) + origin(4) + sender(32) +
/// destination(4) + recipient(32) + body(64).
pub(crate) const ENCODED_MESSAGE_LEN: usize = 141;

/// The MessageIdMultisigIsm configuration read from on-chain state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsmState {
    /// Validator addresses (20-byte ETH addresses), ordered by on-chain
    /// slot index 0..validator_count. The on-chain registry stores each
    /// validator as a `Bytes<64>` secp256k1 public key (X_be || Y_be, the
    /// uncompressed SEC1 body without the 0x04 tag, #22); the decoder
    /// derives the address as `keccak256(pubkey)[12..]`.
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

/// Read a `Bytes<64>` leaf (a validator's secp256k1 public key, stored as
/// X_be(32) || Y_be(32) — the uncompressed SEC1 body without the 0x04 tag).
/// The runtime trims trailing zero bytes from a stored `Bytes<N>` leaf
/// (`From<[u8; N]> for ValueAtom` drops them), so a pubkey whose Y coordinate
/// ends in zero bytes is stored shorter than 64; right-pad back to the fixed
/// width, the same trim/pad handling as the `Bytes<141>` message and
/// `Bytes<32>` atom decoders. Only an over-long value is an error.
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

/// Read the `validators: Map<Uint<8>, Bytes<64>>` ledger field — each value
/// is a secp256k1 public key (X_be || Y_be, the uncompressed SEC1 body the
/// in-circuit `secp256k1EcdsaVerify` checks against) — and derive each
/// validator's 20-byte ETH address as `keccak256(pubkey)[12..]`, the standard
/// Ethereum address derivation. Returned in ascending slot-index order (the
/// order the on-chain multisig expects).
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
        let pubkey = read_bytes64(&pair.1)?;
        let digest = ethers::utils::keccak256(pubkey);
        let mut addr = [0u8; 20];
        addr.copy_from_slice(&digest[12..]);
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
    /// deployed merkle-less layout: a 2-element array whose `[1]` element holds
    /// the Routes/ISM/Mailbox fields, with `deliveries` at `[1,8]`, `nonce` at
    /// `[1,9]` and `dispatched_messages` at `[1,10]`. Slots before each field
    /// are filled with `Null` so the pinned paths line up.
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
            StateValue::Null, // [1,7]
            deliveries,       // [1,8]
            nonce,            // [1,9]
            dispatched,       // [1,10]
        ]);
        let root = array(vec![
            StateValue::Null, // [0] ownership + Routes local_domain (unused here)
            mailbox,          // [1] Routes/ISM/Mailbox module fields
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
    fn decodes_dispatched_messages_sorted_by_nonce() {
        // Multiple map entries exercise the key parse + sort: the merkle
        // reconstruction ingests leaves in nonce order, so a wrong key parse
        // or ordering bug would produce a different root.
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

    /// Serialize a synthetic ISM `StateValue` tree into the tagged
    /// `ContractState` wire bytes. The root mirrors the deployed merkle-less
    /// layout: a 2-element array whose `[1]` element holds the ISM fields, with
    /// `validators` at `[1,1]`, `validator_count` at `[1,2]`, `threshold` at
    /// `[1,3]` and `module_type` at `[1,4]`. Slot `[1,0]` (Routes
    /// `remote_routers`) is `Null` so the pinned paths line up.
    fn ism_state_bytes(
        validators: StateValue<DefaultDB>,
        validator_count: u8,
        threshold: u8,
        module_type: u8,
    ) -> Vec<u8> {
        let ism = array(vec![
            StateValue::Null,      // [1,0] Routes remote_routers (unused here)
            validators,            // [1,1]
            cell(validator_count), // [1,2]
            cell(threshold),       // [1,3]
            cell(module_type),     // [1,4]
        ]);
        let root = array(vec![
            StateValue::Null, // [0] ownership + Routes local_domain (unused here)
            ism,              // [1] Routes/ISM/Mailbox module fields
        ]);
        let cs = ContractState::<DefaultDB> {
            data: ChargedState::new(root),
            ..ContractState::default()
        };
        let mut bytes = Vec::new();
        midnight_serialize::tagged_serialize(&cs, &mut bytes).expect("serialize synthetic state");
        bytes
    }

    /// Real secp256k1 keypair for the pubkey-registry tests: the 64-byte
    /// registry value (X_be || Y_be, the uncompressed SEC1 body without the
    /// 0x04 tag — exactly what the contract's `enrollValidator` stores) plus
    /// the ETH address derived from it, cross-checked against ethers' own
    /// independent secret-key -> address path so the derivation under test
    /// cannot silently drift.
    fn validator_keypair(priv_hex: &str) -> ([u8; 64], [u8; 20]) {
        use ethers::core::k256::elliptic_curve::sec1::ToEncodedPoint;
        use ethers::core::k256::PublicKey;
        use ethers::signers::{LocalWallet, Signer};

        let wallet: LocalWallet = priv_hex.parse().unwrap();
        let point = PublicKey::from(&wallet.signer().verifying_key()).to_encoded_point(false);
        assert_eq!(point.as_bytes()[0], 0x04, "uncompressed SEC1 tag");
        let pubkey: [u8; 64] = point.as_bytes()[1..].try_into().unwrap();
        // The registry-value -> identity derivation under test.
        let derived: [u8; 20] = ethers::utils::keccak256(pubkey)[12..].try_into().unwrap();
        assert_eq!(derived, wallet.address().0, "keccak256(pubkey)[12..]");
        (pubkey, derived)
    }

    // The validators registry stores `Bytes<64>` secp256k1 pubkeys (#22);
    // the decoder must derive each ETH address as `keccak256(pubkey)[12..]`
    // and return them in ascending slot-index order regardless of the map's
    // iteration order (entries are inserted out of order here).
    #[test]
    fn decodes_synthetic_ism_state_from_pubkey_registry() {
        let (pk0, addr0) =
            validator_keypair("1111111111111111111111111111111111111111111111111111111111111111");
        let (pk1, addr1) =
            validator_keypair("2222222222222222222222222222222222222222222222222222222222222222");

        // `validators: Map<Uint<8>, Bytes<64>>`, keyed by slot index; slot 1
        // inserted first to exercise the sort-by-key ordering.
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

    // The runtime trims trailing zero bytes from a stored `Bytes<64>` leaf
    // (`From<[u8; N]> for ValueAtom` drops them), so a pubkey whose Y
    // coordinate ends in 0x00 is stored SHORT; `read_bytes64` must right-pad
    // back to 64 before hashing, or such a validator (~1 in 256 keys) would
    // fail to decode. Synthetic bytes rather than a real curve point — the
    // decoder never validates the point, and forcing a zero tail pins the
    // pad branch deterministically.
    #[test]
    fn decodes_pubkey_with_trailing_zeros_trimmed_on_store() {
        let mut pubkey = [0x5Au8; 64];
        pubkey[61..].fill(0);
        // Confirm the store actually trims, so this exercises the pad branch.
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

    // Real `night` state captured from the local devnet indexer
    // (deploy with validators 0x19e7../0x1563../0x5cbd.., threshold 2,
    // module_type 5).
    //
    // IGNORED until the fixture is regenerated: this blob predates #22, so
    // its `validators` map still stores 20-byte ETH addresses. The decoder
    // now expects `Bytes<64>` secp256k1 pubkeys and derives the address via
    // `keccak256(pubkey)[12..]`, so decoding the stale blob yields garbage
    // addresses. Once the contracts repo regenerates `night-state.hex` from
    // a deploy that enrolls the SAME three validator keys as 64-byte pubkeys,
    // the derived addresses below are unchanged — drop the `#[ignore]`
    // without touching the assertions.
    #[test]
    #[ignore = "night-state.hex is a live-captured fixture predating #22 (32-byte validators) and the merkle-removal layout; regenerate from a fresh deploy of the same 3 validators — see contracts repo"]
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

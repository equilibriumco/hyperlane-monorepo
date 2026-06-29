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

use hyperlane_core::{ChainResult, HyperlaneMessage, H256};
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

// Mailbox + MerkleTree ledger fields live under the SECOND top-level array
// element (`[1, _]`), in module field-declaration order: deliveries(2),
// nonce(3), dispatched_messages(4), branch(5), count(6), current_root(7),
// then the MerkleTree scratch fields (8..10). Derived from the compiled
// `night` readers the same way the ISM paths above were: `_merkleCount_0`
// reads `[1, 6]`, `_messageAt_0` navigates `[1, 4]` then a map lookup, and
// `_root_0` checks the count at `[1, 6]` then returns the cached root at
// `[1, 7]`. Pinned to the field-declaration order in `Mailbox.compact` /
// `MerkleTree.compact`; any reorder/insert above or between them shifts
// these. Re-verify the same way: recompile `night.compact` and grep the
// `queryLedgerState` paths in `managed/night/contract/index.js`.
const DISPATCHED_MESSAGES_PATH: [usize; 2] = [1, 4];
const MERKLE_COUNT_PATH: [usize; 2] = [1, 6];
const CURRENT_ROOT_PATH: [usize; 2] = [1, 7];

/// Width of an encoded warp-route `HyperlaneMessage` stored in
/// `dispatched_messages` (77-byte header + 64-byte `TokenMessage` body).
/// Matches the `Bytes<141>` ledger value width in `Mailbox.compact`.
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
        other => Err(
            HyperlaneMidnightError::StateDecode(format!("expected cell leaf, got {other:?}")).into(),
        ),
    }
}

/// Read a `Uint<8>` leaf (value like 5; `0` is an empty atom).
fn read_u8(node: &StateValue<DefaultDB>) -> ChainResult<u8> {
    let bytes = cell_atom(node)?;
    match bytes.len() {
        0 => Ok(0),
        1 => Ok(bytes[0]),
        n => Err(HyperlaneMidnightError::StateDecode(format!("expected u8 leaf, got {n} bytes")).into()),
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

        // Map value is the verbatim 141-byte encoded message.
        let value = cell_atom(&pair.1)?;
        if value.len() != ENCODED_MESSAGE_LEN {
            return Err(HyperlaneMidnightError::StateDecode(format!(
                "dispatched message at nonce {nonce} is {} bytes, expected {ENCODED_MESSAGE_LEN}",
                value.len()
            ))
            .into());
        }
        let message = HyperlaneMessage::from(value.to_vec());
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

    fn addr(hex: &str) -> [u8; 20] {
        let v = hex::decode(hex.trim_start_matches("0x")).unwrap();
        <[u8; 20]>::try_from(v.as_slice()).unwrap()
    }

    // Real `night` state captured from the local devnet indexer
    // (deploy with validators 0x19e7../0x1563../0x5cbd.., threshold 2,
    // module_type 5).
    #[test]
    fn decodes_live_night_ism_state() {
        let hex = include_str!("../tests/fixtures/night-state.hex").trim();
        let bytes = hex::decode(hex).expect("fixture is valid hex");
        let ism = decode_ism_state(&bytes).expect("decode ISM state");

        assert_eq!(ism.module_type, 5, "module_type should be MessageIdMultisig");
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
    // `ContractState.serialize()` emits the exact tagged bytes the indexer
    // serves — see that script's header). Two leaves means the root is a real
    // branch hash `keccak(leaf0 || leaf1)`, not a trivial single-leaf root.
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
}

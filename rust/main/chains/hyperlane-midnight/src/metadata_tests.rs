//! #18: relayer outbound metadata parity — Midnight as the ORIGIN chain.
//!
//! The relayer's two outbound responsibilities are (a) listing pending
//! Midnight-origin messages and (b) assembling MultisigIsm metadata a STANDARD
//! destination ISM verifies. Both are chain-agnostic upstream once the Midnight
//! crate supplies the standard traits (dispatch indexer #16, merkle indexer #15,
//! validator announce #33, chain-sourced ISM #14) and the validator signs
//! standard checkpoints (#17) — the relayer's `MessageIdMultisigMetadataBuilder`
//! contains no chain-specific code (verified: zero `midnight`/`aleo`/`sealevel`
//! references in `agents/relayer/src`). Enumeration is covered in `indexer.rs`
//! (`dispatch_enumerates_committed_fixture_in_sequence`).
//!
//! This module closes the second half: it proves that the MessageIdMultisig
//! metadata blob built from a Midnight-origin checkpoint is byte-shaped exactly
//! as a stock EVM `MessageIdMultisigIsm` expects, and that the destination's
//! forward-only two-pointer verification accepts it. #17's checkpoint vector
//! already pins the SINGLE checkpoint digest + one signature against the EVM
//! oracle; this goes one layer out to the assembled M-of-N metadata structure:
//!
//!   merkleTreeHook(32) || root(32) || index(u32 BE, 4) || signature(65) * M
//!
//! The committed `hyperlane-metadata-vector.json` is generated offline (no node,
//! no proof) by `contracts/tests/utils/generate-metadata-vector.ts` from the
//! SAME two-dispatch scenario as `night-state-dispatched.hex`, with digests and
//! signatures produced by the independent `@hyperlane-xyz/utils` oracle. This
//! test first drift-guards the fixture against the vector (one dispatch state),
//! then asserts the digest parity and runs the exact destination-side
//! verification. The signing subset is chosen to be descending by address while
//! ascending by set index, so the ordering requirement is exercised
//! adversarially (a verifier matching by address would reject it).

use hyperlane_core::{
    accumulator::incremental::IncrementalMerkle, Checkpoint, CheckpointWithMessageId, Signable,
    Signature, SignedType, H160, H256,
};
use serde::Deserialize;

use crate::state_decode::decode_dispatched_messages;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetadataVector {
    origin: u32,
    merkle_tree_hook: String,
    root: String,
    index: u32,
    message_id: String,
    /// Inner checkpoint digest (`BaseValidator.messageHash`) from the oracle.
    inner: String,
    /// EIP-191-wrapped digest the validators signed, from the oracle.
    digest: String,
    threshold: usize,
    /// Full validator set in configured (index) order, as the destination ISM
    /// walks it.
    validators: Vec<String>,
    /// Signatures in ascending validator-index order (the subset that signed).
    signatures: Vec<String>,
    /// The assembled MessageIdMultisig metadata blob.
    metadata: String,
}

fn h256(hex_str: &str) -> H256 {
    H256::from_slice(&hex::decode(hex_str.trim_start_matches("0x")).expect("hex"))
}

fn bytes(hex_str: &str) -> Vec<u8> {
    hex::decode(hex_str.trim_start_matches("0x")).expect("hex")
}

const SIGNATURE_LEN: usize = 65;
const METADATA_PREFIX_LEN: usize = 32 + 32 + 4; // merkleTreeHook || root || index

#[test]
fn metadata_matches_fixture_and_verifies_as_standard_multisig() {
    let vector: MetadataVector = serde_json::from_str(include_str!(
        "../tests/fixtures/hyperlane-metadata-vector.json"
    ))
    .expect("vector json parses");

    // --- Drift guard: the committed fixture and this vector are one dispatch ---
    let fixture = hex::decode(include_str!("../tests/fixtures/night-state-dispatched.hex").trim())
        .expect("fixture is valid hex");

    let mut messages = decode_dispatched_messages(&fixture).expect("decode dispatched messages");
    messages.sort_by_key(|(nonce, _)| *nonce);

    // A local replica rebuilt from the dispatch leaves reproduces the vector's
    // root input. The contract keeps no on-chain root, so this reconstruction
    // is the sole source — exactly the root the validator signs.
    let mut tree = IncrementalMerkle::default();
    for (_nonce, message) in &messages {
        tree.ingest(message.id());
    }
    assert_eq!(
        tree.root(),
        h256(&vector.root),
        "reconstructed root must match the vector's root (fixture/vector drift)"
    );
    assert_eq!(tree.index(), vector.index, "tip index is count - 1");

    let tip = messages
        .iter()
        .find(|(nonce, _)| *nonce == vector.index)
        .expect("tip leaf present in fixture");
    assert_eq!(
        tip.1.id(),
        h256(&vector.message_id),
        "fixture tip messageId must match the vector (fixture/vector drift)"
    );

    // --- Byte layout the destination MessageIdMultisigIsm reads ---
    let blob = bytes(&vector.metadata);
    assert_eq!(
        blob.len(),
        METADATA_PREFIX_LEN + vector.threshold * SIGNATURE_LEN,
        "metadata length is 68 + 65*threshold"
    );

    // Parse the checkpoint fields straight out of the blob, exactly as the
    // destination ISM does, and confirm they equal the vector's fields.
    let blob_mth = H256::from_slice(&blob[0..32]);
    let blob_root = H256::from_slice(&blob[32..64]);
    let blob_index = u32::from_be_bytes(blob[64..68].try_into().unwrap());
    assert_eq!(
        blob_mth,
        h256(&vector.merkle_tree_hook),
        "merkleTreeHook prefix"
    );
    assert_eq!(blob_root, h256(&vector.root), "root");
    assert_eq!(blob_index, vector.index, "index is big-endian u32");

    // The flat `signatures` list must equal the blob's signature section.
    for (i, sig_hex) in vector.signatures.iter().enumerate() {
        let start = METADATA_PREFIX_LEN + i * SIGNATURE_LEN;
        assert_eq!(
            &blob[start..start + SIGNATURE_LEN],
            bytes(sig_hex).as_slice(),
            "signature {i} matches the blob's signature section"
        );
    }

    // --- Destination-side verification: reconstruct the checkpoint from the
    //     blob + the delivered message's id (what the ISM has), recover each
    //     signature, and match the validator set with a forward-only pointer.
    //     This is exactly the EVM MessageIdMultisigIsm check; passing it means
    //     the Midnight-origin metadata is consumable unchanged. ---
    let checkpoint = CheckpointWithMessageId {
        checkpoint: Checkpoint {
            merkle_tree_hook_address: blob_mth,
            mailbox_domain: vector.origin,
            root: blob_root,
            index: blob_index,
        },
        // The destination computes the messageId from the delivered message, not
        // from the metadata (the blob carries no messageId field). Use the
        // fixture's tip id, already asserted equal to the vector's.
        message_id: tip.1.id(),
    };
    // Digest parity: the checkpoint the destination reconstructs hashes to the
    // same inner + EIP-191 digest the validators signed (produced by the
    // independent `@hyperlane-xyz/utils` oracle). Asserting this here localises a
    // digest-construction regression to a clear failure, instead of surfacing as
    // a confusing "signer not in set" once recovery runs against a wrong digest.
    assert_eq!(
        checkpoint.signing_hash(),
        h256(&vector.inner),
        "signing_hash must equal the oracle inner digest"
    );
    assert_eq!(
        checkpoint.eth_signed_message_hash(),
        h256(&vector.digest),
        "EIP-191 digest must equal the oracle digest"
    );

    let validators: Vec<H160> = vector
        .validators
        .iter()
        .map(|v| H160::from_slice(&bytes(v)))
        .collect();

    // Recover signers from the blob's signature section, in blob order.
    let mut recovered: Vec<H160> = Vec::with_capacity(vector.threshold);
    for i in 0..vector.threshold {
        let start = METADATA_PREFIX_LEN + i * SIGNATURE_LEN;
        let sig_bytes = &blob[start..start + SIGNATURE_LEN];
        let signature: Signature = ethers::core::types::Signature::try_from(sig_bytes)
            .expect("65-byte signature")
            .into();
        let signed = SignedType {
            value: checkpoint,
            signature,
        };
        recovered.push(signed.recover().expect("recover signer"));
    }

    // Forward-only two-pointer over the validator set: each recovered signer
    // matches a validator at a strictly increasing index. This enforces both the
    // ascending-order requirement and implicit duplicate rejection — identical to
    // the destination ISM's walk.
    let mut vptr = 0usize;
    let mut matched = 0usize;
    for signer in &recovered {
        while vptr < validators.len() && &validators[vptr] != signer {
            vptr += 1;
        }
        assert!(
            vptr < validators.len(),
            "recovered signer {signer:?} is not in the validator set or is out of ascending order"
        );
        matched += 1;
        vptr += 1;
    }
    assert_eq!(
        matched, vector.threshold,
        "all signatures matched distinct validators in ascending index order"
    );

    // Concretely: the vector's signers are validators 0 and 2 (index 1 skipped),
    // so the two-pointer walk must have skipped the middle validator.
    assert_eq!(
        recovered,
        vec![validators[0], validators[2]],
        "signers are the ascending set-index subset {{0, 2}} the vector encodes"
    );

    // Sharpened ordering check: those signers are DESCENDING by address (the
    // vector's validator set is ordered so index 0 has a larger address than
    // index 2). A verifier that matched by address order rather than set index
    // would have rejected this order. Passing the forward-only walk above while
    // this holds proves the ordering requirement is enforced by SET INDEX, not by
    // address — a subset ascending under both orderings could not prove that.
    assert!(
        recovered[0] > recovered[1],
        "signing subset must be descending by address so the index-order requirement is adversarial"
    );
}

//! Outbound metadata parity with Midnight as the origin chain.
//!
//! Proves that the MessageIdMultisig metadata assembled from a Midnight-origin
//! checkpoint has the byte layout a stock EVM `MessageIdMultisigIsm` expects:
//!
//!   merkleTreeHook(32) || root(32) || index(u32 BE, 4) || signature(65) * M
//!
//! and that the destination's forward-only verification accepts it. Digests and
//! signatures in the committed vector come from the independent
//! `@hyperlane-xyz/utils` oracle. The signing subset is deliberately descending
//! by address while ascending by set index, so a verifier that matched by
//! address instead of index would fail here.

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
    /// Inner checkpoint digest (`BaseValidator.messageHash`).
    inner: String,
    /// EIP-191-wrapped digest the validators signed.
    digest: String,
    threshold: usize,
    /// Full validator set in index order, as the destination ISM walks it.
    validators: Vec<String>,
    /// The subset that signed, in ascending validator-index order.
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

    // Drift guard: the committed fixture and this vector must describe one
    // dispatch state.
    let fixture = hex::decode(include_str!("../tests/fixtures/night-state-dispatched.hex").trim())
        .expect("fixture is valid hex");

    let mut messages = decode_dispatched_messages(&fixture).expect("decode dispatched messages");
    messages.sort_by_key(|(nonce, _)| *nonce);

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

    let blob = bytes(&vector.metadata);
    assert_eq!(
        blob.len(),
        METADATA_PREFIX_LEN + vector.threshold * SIGNATURE_LEN,
        "metadata length is 68 + 65*threshold"
    );

    // Parse the checkpoint fields out of the blob the way the destination ISM
    // does.
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

    for (i, sig_hex) in vector.signatures.iter().enumerate() {
        let start = METADATA_PREFIX_LEN + i * SIGNATURE_LEN;
        assert_eq!(
            &blob[start..start + SIGNATURE_LEN],
            bytes(sig_hex).as_slice(),
            "signature {i} matches the blob's signature section"
        );
    }

    // Destination-side verification: rebuild the checkpoint from the blob plus
    // the delivered message's id, which is all the ISM has.
    let checkpoint = CheckpointWithMessageId {
        checkpoint: Checkpoint {
            merkle_tree_hook_address: blob_mth,
            mailbox_domain: vector.origin,
            root: blob_root,
            index: blob_index,
        },
        // The blob carries no messageId field, so the destination computes it
        // from the delivered message.
        message_id: tip.1.id(),
    };
    // Checked before recovery: a digest bug surfaces here rather than as a
    // confusing "signer not in set" further down.
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

    // The destination ISM's walk: each signer must match a validator at a
    // strictly increasing index, which also rejects duplicates.
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

    // The vector's signers are validators 0 and 2, so the walk had to skip the
    // middle one.
    assert_eq!(
        recovered,
        vec![validators[0], validators[2]],
        "signers are the ascending set-index subset {{0, 2}} the vector encodes"
    );

    // Those two signers descend by address, so passing the walk above proves
    // the ordering is enforced by set index and not by address.
    assert!(
        recovered[0] > recovered[1],
        "signing subset must be descending by address so the index-order requirement is adversarial"
    );
}

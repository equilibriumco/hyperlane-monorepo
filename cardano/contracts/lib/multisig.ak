//// Shared multisig-ISM logic used by both the MessageId ISM and the MerkleRoot
//// ISM. Extracted so the signature-verification and checkpoint-hashing code
//// exists in exactly one place (the two ISM scripts differ only in how the
//// delivered message is bound to the signed root).

use aiken/builtin.{verify_ecdsa_secp256k1_signature}
use aiken/collection/list
use aiken/crypto.{keccak_256}
use aiken/primitive/bytearray
use cardano/transaction.{InlineDatum, OutputReference, Transaction}
use types.{
  Checkpoint, Domain, MultisigIsmDatum, ValidatorSignature, encode_checkpoint,
  encode_domain_hash,
}
use utils.{assoc_find, find_input, is_signed_by, value_preserved}

/// Verify a checkpoint has a threshold of unique, valid validator signatures for
/// its origin domain. This does NOT bind the delivered message — each ISM script
/// layers that on top (MessageId: checkpoint.message_id; MerkleRoot: a proof).
pub fn verify_checkpoint(
  datum: MultisigIsmDatum,
  checkpoint: Checkpoint,
  validator_signatures: List<ValidatorSignature>,
) -> Bool {
  expect Some(validator_addresses) =
    assoc_find(datum.validators, checkpoint.origin)
  expect Some(threshold) = assoc_find(datum.thresholds, checkpoint.origin)
  expect threshold > 0

  let message_hash = compute_checkpoint_hash(checkpoint)
  let valid_count =
    count_valid_signatures(
      validator_signatures,
      validator_addresses,
      message_hash,
    )
  valid_count >= threshold
}

/// Compute the EIP-191 hash validators sign for a checkpoint:
/// 1. domain_hash    = keccak256(origin || merkle_tree_hook || "HYPERLANE")
/// 2. checkpoint_digest = keccak256(domain_hash || merkle_root || merkle_index || message_id)
/// 3. signed         = keccak256("\x19Ethereum Signed Message:\n32" || checkpoint_digest)
pub fn compute_checkpoint_hash(checkpoint: Checkpoint) -> ByteArray {
  let domain_hash_encoded =
    encode_domain_hash(checkpoint.origin, checkpoint.origin_merkle_tree_hook)
  let domain_hash = keccak_256(domain_hash_encoded)

  let checkpoint_encoded = encode_checkpoint(checkpoint, domain_hash)
  let checkpoint_digest = keccak_256(checkpoint_encoded)

  let eip191_prefix =
    #"19457468657265756d205369676e6564204d6573736167653a0a3332"
  let prefixed = bytearray.concat(eip191_prefix, checkpoint_digest)
  keccak_256(prefixed)
}

/// Count unique valid signatures from trusted validators. Seen addresses are
/// tracked so duplicate signatures from one validator cannot inflate the count.
pub fn count_valid_signatures(
  validator_signatures: List<ValidatorSignature>,
  validator_addresses: List<ByteArray>,
  message_hash: ByteArray,
) -> Int {
  let (valid_count, _seen) =
    list.foldl(
      validator_signatures,
      (0, []),
      fn(val_sig, acc) {
        let (count, seen) = acc
        let ValidatorSignature {
          compressed_pubkey,
          uncompressed_pubkey,
          signature,
        } = val_sig

        let compressed_x = bytearray.drop(compressed_pubkey, 1)
        let uncompressed_x = bytearray.take(uncompressed_pubkey, 32)
        expect compressed_x == uncompressed_x

        let sig_valid =
          verify_ecdsa_secp256k1_signature(
            compressed_pubkey,
            message_hash,
            signature,
          )

        if sig_valid {
          let address = pubkey_to_eth_address(uncompressed_pubkey)
          if list.has(validator_addresses, address) && !list.has(seen, address) {
            (count + 1, [address, ..seen])
          } else {
            (count, seen)
          }
        } else {
          (count, seen)
        }
      },
    )
  valid_count
}

/// address = keccak256(uncompressed_pubkey)[12:32]
fn pubkey_to_eth_address(pubkey: ByteArray) -> ByteArray {
  let pubkey_len = bytearray.length(pubkey)
  if pubkey_len == 64 {
    let hash = keccak_256(pubkey)
    bytearray.drop(hash, 12)
  } else if pubkey_len == 65 {
    let pubkey_no_prefix = bytearray.drop(pubkey, 1)
    let hash = keccak_256(pubkey_no_prefix)
    bytearray.drop(hash, 12)
  } else {
    #""
  }
}

/// Owner-gated update of a domain's validator set.
pub fn validate_set_validators(
  datum: MultisigIsmDatum,
  domain: Domain,
  new_validators: List<ByteArray>,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  expect is_signed_by(tx, datum.owner)
  let new_validators_list =
    update_assoc(datum.validators, domain, new_validators)
  let new_datum = MultisigIsmDatum { ..datum, validators: new_validators_list }
  validate_ism_continuation(datum, new_datum, tx, own_ref)
}

/// Owner-gated update of a domain's threshold (must stay > 0).
pub fn validate_set_threshold(
  datum: MultisigIsmDatum,
  domain: Domain,
  new_threshold: Int,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  expect is_signed_by(tx, datum.owner)
  expect new_threshold > 0
  let new_thresholds_list =
    update_assoc(datum.thresholds, domain, new_threshold)
  let new_datum = MultisigIsmDatum { ..datum, thresholds: new_thresholds_list }
  validate_ism_continuation(datum, new_datum, tx, own_ref)
}

/// The ISM state UTXO must continue with the expected new datum and preserved
/// value (lovelace + all native tokens, including the ISM state NFT).
pub fn validate_ism_continuation(
  _old_datum: MultisigIsmDatum,
  new_datum: MultisigIsmDatum,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address
  let own_value = own_input.output.value

  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: MultisigIsmDatum = cont_datum_data
  expect cont_datum == new_datum

  value_preserved(own_value, continuation.value)
}

fn update_assoc(items: List<(k, v)>, key: k, value: v) -> List<(k, v)> {
  update_assoc_helper(items, key, value, False)
}

fn update_assoc_helper(
  items: List<(k, v)>,
  key: k,
  value: v,
  found: Bool,
) -> List<(k, v)> {
  when items is {
    [] ->
      if found {
        []
      } else {
        [(key, value)]
      }
    [(k, v), ..tail] ->
      if k == key {
        [(key, value), ..update_assoc_helper(tail, key, value, True)]
      } else {
        [(k, v), ..update_assoc_helper(tail, key, value, found)]
      }
  }
}

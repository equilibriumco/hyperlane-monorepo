use aiken/builtin.{verify_ecdsa_secp256k1_signature}
use aiken/collection/list
use aiken/crypto.{keccak_256}
use aiken/primitive/bytearray
use cardano/assets.{lovelace_of}
use cardano/transaction.{InlineDatum, OutputReference, Transaction}
use types.{
  Checkpoint, Domain, MultisigIsmDatum, MultisigIsmRedeemer, ValidatorSignature,
  encode_checkpoint, encode_domain_hash, Verify, SetValidators, SetThreshold,
}
use utils.{assoc_find, find_input, is_signed_by}

/// Multisig ISM (Interchain Security Module) Validator
/// Verifies message authenticity using N-of-M validator signatures
///
/// This ISM is used as a reference input during message processing.
/// The mailbox checks that this ISM is present, and the ISM's datum
/// contains the validator set and threshold for each origin domain.
///
/// SIGNATURE VERIFICATION APPROACH:
/// 1. Relayer recovers public keys from signatures off-chain using ecrecover
/// 2. Relayer passes both compressed (33 bytes) and uncompressed (64 bytes) pubkeys
/// 3. ISM verifies the two formats match (same x-coordinate)
/// 4. ISM verifies signature using compressed pubkey (per CIP-49)
/// 5. ISM computes Ethereum address from uncompressed pubkey: keccak256(pubkey)[12:32]
/// 6. ISM checks each computed address is in the trusted validators list
///
/// Security:
/// - On-chain signature verification provides cryptographic proof
/// - The public key formats are verified to match, preventing substitution
/// - An attacker cannot forge a signature without the validator's private key
/// - The Ethereum address derivation (keccak256) is collision-resistant
///
/// Multisig ISM validator
validator multisig_ism {
  spend(
    datum: Option<MultisigIsmDatum>,
    redeemer: MultisigIsmRedeemer,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    expect Some(ism_datum) = datum

    when redeemer is {
      Verify { checkpoint, validator_signatures } ->
        verify_checkpoint(ism_datum, checkpoint, validator_signatures)

      SetValidators { domain, validators } ->
        validate_set_validators(ism_datum, domain, validators, tx, own_ref)

      SetThreshold { domain, threshold } ->
        validate_set_threshold(ism_datum, domain, threshold, tx, own_ref)
    }
  }

  else(_) {
    fail
  }
}

/// Verify a checkpoint has sufficient valid validator signatures
fn verify_checkpoint(
  datum: MultisigIsmDatum,
  checkpoint: Checkpoint,
  validator_signatures: List<ValidatorSignature>,
) -> Bool {
  // Get trusted validator addresses for the origin domain
  expect Some(validator_addresses) = assoc_find(datum.validators, checkpoint.origin)

  // Get threshold for the origin domain
  expect Some(threshold) = assoc_find(datum.thresholds, checkpoint.origin)

  // Compute the message hash that validators signed
  let message_hash = compute_checkpoint_hash(checkpoint)

  // Count valid signatures from trusted validators
  let valid_count = count_valid_signatures(
    validator_signatures,
    validator_addresses,
    message_hash,
  )

  // Must have at least threshold valid signatures
  valid_count >= threshold
}

/// Compute the hash that validators sign for a checkpoint
/// This matches the Hyperlane signing format:
/// 1. domain_hash = keccak256(origin || merkle_tree_hook || "HYPERLANE")
/// 2. checkpoint_digest = keccak256(domain_hash || merkle_root || merkle_index || message_id)
/// 3. eth_signed_message = keccak256("\x19Ethereum Signed Message:\n32" || checkpoint_digest)
fn compute_checkpoint_hash(checkpoint: Checkpoint) -> ByteArray {
  // Step 1: domain_hash
  let domain_hash_encoded = encode_domain_hash(checkpoint.origin, checkpoint.origin_merkle_tree_hook)
  let domain_hash = keccak_256(domain_hash_encoded)

  // Step 2: checkpoint_digest
  let checkpoint_encoded = encode_checkpoint(checkpoint, domain_hash)
  let checkpoint_digest = keccak_256(checkpoint_encoded)

  // Step 3: EIP-191 hash (this is what validators actually sign)
  let eip191_prefix = #"19457468657265756d205369676e6564204d6573736167653a0a3332"
  let prefixed = bytearray.concat(eip191_prefix, checkpoint_digest)
  keccak_256(prefixed)
}

/// Count how many signatures are valid and from trusted validators
fn count_valid_signatures(
  validator_signatures: List<ValidatorSignature>,
  validator_addresses: List<ByteArray>,
  message_hash: ByteArray,
) -> Int {
  list.foldl(
    validator_signatures,
    0,
    fn(val_sig, acc) {
      let ValidatorSignature { compressed_pubkey, uncompressed_pubkey, signature } = val_sig

      // Step 1: Verify the compressed and uncompressed keys match
      // The x-coordinate should be the same: compressed[1:33] == uncompressed[0:32]
      let compressed_x = bytearray.drop(compressed_pubkey, 1)
      let uncompressed_x = bytearray.take(uncompressed_pubkey, 32)
      expect compressed_x == uncompressed_x

      // Step 2: Verify the signature using compressed pubkey (per CIP-49)
      let sig_valid = verify_ecdsa_secp256k1_signature(
        compressed_pubkey,
        message_hash,
        signature,
      )

      if sig_valid {
        // Step 3: Compute Ethereum address from uncompressed public key
        let address = pubkey_to_eth_address(uncompressed_pubkey)

        // Step 4: Check if this address is in the trusted validators list
        if list.has(validator_addresses, address) {
          acc + 1
        } else {
          // Signature valid but address not trusted
          acc
        }
      } else {
        // Signature verification failed
        acc
      }
    },
  )
}

/// Convert an uncompressed secp256k1 public key (64 bytes) to Ethereum address (20 bytes)
/// address = keccak256(pubkey)[12:32] (last 20 bytes)
fn pubkey_to_eth_address(pubkey: ByteArray) -> ByteArray {
  let pubkey_len = bytearray.length(pubkey)

  if pubkey_len == 64 {
    // Uncompressed key without prefix (64 bytes: X || Y)
    let hash = keccak_256(pubkey)
    bytearray.drop(hash, 12)
  } else if pubkey_len == 65 {
    // Uncompressed key with 0x04 prefix (65 bytes: 04 || X || Y)
    let pubkey_no_prefix = bytearray.drop(pubkey, 1)
    let hash = keccak_256(pubkey_no_prefix)
    bytearray.drop(hash, 12)
  } else {
    // Invalid key length - return empty (won't match any trusted address)
    #""
  }
}

/// Validate setting validators for a domain
fn validate_set_validators(
  datum: MultisigIsmDatum,
  domain: Domain,
  new_validators: List<ByteArray>,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // Only owner can update
  expect is_signed_by(tx, datum.owner)

  // Build new validators list (update or add)
  let new_validators_list =
    update_assoc(datum.validators, domain, new_validators)

  let new_datum = MultisigIsmDatum { ..datum, validators: new_validators_list }

  validate_ism_continuation(datum, new_datum, tx, own_ref)
}

/// Validate setting threshold for a domain
fn validate_set_threshold(
  datum: MultisigIsmDatum,
  domain: Domain,
  new_threshold: Int,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // Only owner can update
  expect is_signed_by(tx, datum.owner)

  // Threshold must be positive
  expect new_threshold > 0

  // Build new thresholds list (update or add)
  let new_thresholds_list =
    update_assoc(datum.thresholds, domain, new_threshold)

  let new_datum = MultisigIsmDatum { ..datum, thresholds: new_thresholds_list }

  validate_ism_continuation(datum, new_datum, tx, own_ref)
}

/// Update or add an entry in an association list
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

/// Validate ISM continuation
fn validate_ism_continuation(
  _old_datum: MultisigIsmDatum,
  new_datum: MultisigIsmDatum,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // Find own input
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address
  let own_value = own_input.output.value

  // Find continuation output
  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  // Verify datum updated correctly
  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: MultisigIsmDatum = cont_datum_data
  expect cont_datum == new_datum

  // Value must be preserved
  lovelace_of(continuation.value) >= lovelace_of(own_value)
}

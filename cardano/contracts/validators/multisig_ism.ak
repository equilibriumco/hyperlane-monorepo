use aiken/builtin.{verify_ecdsa_secp256k1_signature}
use aiken/collection/list
use aiken/crypto.{keccak_256}
use aiken/primitive/bytearray
use cardano/assets.{lovelace_of}
use cardano/transaction.{InlineDatum, OutputReference, Transaction}
use types.{
  Domain, Message, MultisigIsmDatum, MultisigIsmRedeemer, encode_message,
  Verify, SetValidators, SetThreshold,
}
use utils.{assoc_find, at, find_input, is_signed_by}

/// Multisig ISM (Interchain Security Module) Validator
/// Verifies message authenticity using N-of-M validator signatures
///
/// This ISM is used as a reference input during message processing.
/// The mailbox checks that this ISM is present, and the ISM's datum
/// contains the validator set and threshold for each origin domain.
///
/// SIGNATURE VERIFICATION:
/// - Uses ECDSA secp256k1 (same curve as Ethereum/EVM validators)
/// - Applies EIP-191 "Ethereum Signed Message" prefix for compatibility
/// - Accepts 65-byte signatures (r || s || v) or 64-byte (r || s)
/// - Validator keys must be secp256k1 public keys (compressed 33 bytes or uncompressed 65 bytes)
///
/// NOTE: The validators list in the datum must contain secp256k1 PUBLIC KEYS,
/// not Ethereum addresses. To derive the address from a signature, use ecrecover
/// off-chain, then map addresses to public keys in your configuration.
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
      Verify { message, signatures } ->
        verify_message(ism_datum, message, signatures)

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

/// Verify a message has sufficient valid signatures
fn verify_message(
  datum: MultisigIsmDatum,
  message: Message,
  signatures: List<(Int, ByteArray)>,
) -> Bool {
  // Get validators for the origin domain
  expect Some(validators) = assoc_find(datum.validators, message.origin)

  // Get threshold for the origin domain
  expect Some(threshold) = assoc_find(datum.thresholds, message.origin)

  // Count valid signatures
  let valid_count = count_valid_signatures(message, signatures, validators)

  // Must have at least threshold valid signatures
  valid_count >= threshold
}

/// Count the number of valid signatures
fn count_valid_signatures(
  message: Message,
  signatures: List<(Int, ByteArray)>,
  validators: List<ByteArray>,
) -> Int {
  // Compute the signing hash (same as EVM validators)
  // Step 1: Hash the encoded message
  let message_encoded = encode_message(message)
  let signing_hash = keccak_256(message_encoded)

  // Step 2: Apply EIP-191 prefix: "\x19Ethereum Signed Message:\n32" + signing_hash
  let message_hash = eip191_hash(signing_hash)

  // For each signature, verify it's valid and from a validator
  list.foldl(
    signatures,
    0,
    fn(sig_entry, acc) {
      let (validator_index, signature) = sig_entry

      // Get the validator's public key
      when at(validators, validator_index) is {
        Some(validator_key) ->
          if verify_signature(validator_key, message_hash, signature) {
            acc + 1
          } else {
            acc
          }
        None -> acc
      }
    },
  )
}

/// Apply EIP-191 "Ethereum Signed Message" prefix
/// Format: "\x19Ethereum Signed Message:\n32" + hash
fn eip191_hash(hash: ByteArray) -> ByteArray {
  // EIP-191 prefix for 32-byte messages
  let prefix = #"19457468657265756d205369676e6564204d6573736167653a0a3332"
  // prefix = "\x19Ethereum Signed Message:\n32" in hex

  keccak_256(bytearray.concat(prefix, hash))
}

/// Verify a single ECDSA secp256k1 signature
/// Compatible with EVM validator signatures (65 bytes: r=32, s=32, v=1)
/// Extracts r,s (first 64 bytes) and verifies against public key
fn verify_signature(
  validator_key: ByteArray,
  message_hash: ByteArray,
  signature: ByteArray,
) -> Bool {
  // EVM signatures are 65 bytes (r || s || v)
  // Aiken's verify function expects 64 bytes (r || s)
  let signature_length = bytearray.length(signature)

  if signature_length == 65 {
    // Extract first 64 bytes (r || s), discarding v
    let signature_64 = bytearray.take(signature, 64)
    verify_ecdsa_secp256k1_signature(validator_key, message_hash, signature_64)
  } else if signature_length == 64 {
    // Already in correct format
    verify_ecdsa_secp256k1_signature(validator_key, message_hash, signature)
  } else {
    // Invalid signature length
    False
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

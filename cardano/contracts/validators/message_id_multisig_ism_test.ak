use aiken/collection/list
use aiken/crypto.{keccak_256}
use aiken/primitive/bytearray
use multisig.{verify_checkpoint}
use types.{
  Checkpoint, Message, MessageIdMultisig, MultisigIsmDatum, ValidatorSignature,
  encode_checkpoint, encode_domain_hash, encode_message,
}

/// Test helper: create a sample message
fn sample_message() -> Message {
  Message {
    version: 3,
    nonce: 1,
    origin: 11155111,
    // Sepolia
    sender: #"0000000000000000000000001234567890abcdef1234567890abcdef12345678",
    destination: 2002,
    // Cardano preview
    recipient: #"0000000000000000000000009876543210fedcba9876543210fedcba98765432",
    body: #"48656c6c6f20576f726c64",
    // "Hello World"
  }
}

/// Test that EIP-191 prefix is correctly formatted
/// The prefix should be: "\x19Ethereum Signed Message:\n32"
test eip191_prefix_is_correct() {
  let expected_prefix = #"19457468657265756d205369676e6564204d6573736167653a0a3332"
  // Verify it's 28 bytes as expected
  bytearray.length(expected_prefix) == 28
}

/// Test that message hash computation produces 32 bytes
test message_hash_is_32_bytes() {
  let message = sample_message()
  let encoded = encode_message(message)
  let signing_hash = keccak_256(encoded)

  bytearray.length(signing_hash) == 32
}

/// Test that EIP-191 hash produces 32 bytes
test eip191_hash_is_32_bytes() {
  let message = sample_message()
  let encoded = encode_message(message)
  let signing_hash = keccak_256(encoded)

  // Apply EIP-191 prefix manually (same as in contract)
  let prefix = #"19457468657265756d205369676e6564204d6573736167653a0a3332"
  let prefixed = bytearray.concat(prefix, signing_hash)
  let final_hash = keccak_256(prefixed)

  bytearray.length(final_hash) == 32
}

/// Test that different messages produce different signing hashes
test different_messages_different_hashes() {
  let message1 = sample_message()
  let message2 = Message { ..message1, nonce: 2 }

  let hash1 = keccak_256(encode_message(message1))
  let hash2 = keccak_256(encode_message(message2))

  hash1 != hash2
}

/// Test that EIP-191 prefix changes the hash
test eip191_prefix_changes_hash() {
  let message = sample_message()
  let encoded = encode_message(message)
  let signing_hash = keccak_256(encoded)

  // Hash without EIP-191 prefix
  let hash_without_prefix = signing_hash

  // Hash with EIP-191 prefix
  let prefix = #"19457468657265756d205369676e6564204d6573736167653a0a3332"
  let prefixed = bytearray.concat(prefix, signing_hash)
  let hash_with_prefix = keccak_256(prefixed)

  hash_without_prefix != hash_with_prefix
}

/// Test 65-byte signature extraction to 64 bytes
test signature_65_to_64_bytes() {
  // Mock 65-byte signature (r=32, s=32, v=1)
  let sig_65 =
    #"1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef1b"

  expect bytearray.length(sig_65) == 65

  // Extract first 64 bytes
  let sig_64 = bytearray.take(sig_65, 64)

  bytearray.length(sig_64) == 64
}

/// Test that 64-byte signatures remain unchanged
test signature_64_bytes_unchanged() {
  // Mock 64-byte signature (r=32, s=32)
  let sig_64 =
    #"1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef"

  expect bytearray.length(sig_64) == 64

  // Should remain 64 bytes
  let result = bytearray.take(sig_64, 64)

  bytearray.length(result) == 64
}

/// Test validator index lookup behavior
test validator_index_in_bounds() {
  let validators =
    [
      #"0000000000000000000000000000000000000000000000000000000000000001",
      #"0000000000000000000000000000000000000000000000000000000000000002",
      #"0000000000000000000000000000000000000000000000000000000000000003",
    ]

  // Index 0 should be valid
  when validators |> at(0) is {
    Some(_) -> True
    None -> False
  }
}

/// Test validator index out of bounds
test validator_index_out_of_bounds() {
  let validators =
    [
      #"0000000000000000000000000000000000000000000000000000000000000001",
      #"0000000000000000000000000000000000000000000000000000000000000002",
    ]

  // Index 5 should be invalid
  when validators |> at(5) is {
    Some(_) -> False
    None -> True
  }
}

/// Test message encoding is deterministic
test message_encoding_deterministic() {
  let message = sample_message()

  let encoded1 = encode_message(message)
  let encoded2 = encode_message(message)

  encoded1 == encoded2
}

/// Test that origin domain affects message hash
test origin_domain_affects_hash() {
  let message1 = sample_message()
  let message2 = Message { ..message1, origin: 1 }

  let hash1 = keccak_256(encode_message(message1))
  let hash2 = keccak_256(encode_message(message2))

  hash1 != hash2
}

/// Test that destination domain affects message hash
test destination_domain_affects_hash() {
  let message1 = sample_message()
  let message2 = Message { ..message1, destination: 1 }

  let hash1 = keccak_256(encode_message(message1))
  let hash2 = keccak_256(encode_message(message2))

  hash1 != hash2
}

/// Test that message body affects message hash
test message_body_affects_hash() {
  let message1 = sample_message()
  let message2 = Message { ..message1, body: #"446966666572656e74" }

  let hash1 = keccak_256(encode_message(message1))
  let hash2 = keccak_256(encode_message(message2))

  hash1 != hash2
}

// ============================================================================
// REAL SEPOLIA TESTNET DATA TESTS
// These tests use actual data from the Hyperlane Sepolia testnet to verify
// our checkpoint and signature verification matches Hyperlane's format.
// Checkpoint index 869976 signed by the official Hyperlane Sepolia validators
// (fetched from their announced S3 checkpoint syncers); signatures verified to
// recover the exact validator addresses below.
// ============================================================================

/// Real Sepolia checkpoint (index 869976) from the official validator syncers.
fn sepolia_checkpoint() -> Checkpoint {
  Checkpoint {
    origin: 11155111,  // Sepolia
    merkle_root: #"2d652e6c03ec3c97d0a20896a7c0ea274c6abc3370dc0c3b3ca3e558503dd118",
    // Sepolia's merkle tree hook address (0x4917a974...), left-padded to 32 bytes
    origin_merkle_tree_hook: #"0000000000000000000000004917a9746a7b6e0a57159ccb7f5a6744247f2d0d",
    merkle_index: 869976,  // 0xd4658
    message_id: #"424b25342a401ee2029bfe8241e61e3e4ff5209913d03c7dafe46e3f0faa0096",
  }
}

/// Real Sepolia validator Ethereum addresses (20 bytes each)
/// Recovered from the signatures below over the checkpoint's EIP-191 digest.
fn sepolia_validator_addresses() -> List<ByteArray> {
  [
    // Validator 0: 0xb22b65f202558adf86a8bb2847b76ae1036686a5 (hyperlane-testnet4-sepolia-validator-0)
    #"b22b65f202558adf86a8bb2847b76ae1036686a5",
    // Validator 1: 0x469f0940684d147defc44f3647146cb90dd0bc8e (hyperlane-testnet4-sepolia-validator-1)
    #"469f0940684d147defc44f3647146cb90dd0bc8e",
  ]
}

/// Real Sepolia validator uncompressed public keys (64 bytes each, no 0x04 prefix)
/// These are used by the relayer in the ISM redeemer
fn sepolia_validator_pubkeys_uncompressed() -> List<ByteArray> {
  [
    // Validator 0 uncompressed (64 bytes) - address 0xb22b65f202558adf86a8bb2847b76ae1036686a5
    #"311a5187d352df97ffd84cade83c4ff6aafeb0331fb97f2e9f4e246390b8b480022fd19bbc35dc50b48c3b6c1fbb989623eacc0ee112c190543cfd5924d37d50",
    // Validator 1 uncompressed (64 bytes) - address 0x469f0940684d147defc44f3647146cb90dd0bc8e
    #"dca5184a4d3e318d02f7737b36a145b1d44a93612696332e7d97c36bdd2739a4282fd562f1fdc7945d31d90fc8c10d762674a011de4c7a1d470c1d3c1b500a5c",
  ]
}

/// Real signatures from Sepolia validators (65 bytes each: r || s || v)
fn sepolia_signatures() -> List<ByteArray> {
  [
    // Signature from validator 0 (v=0x1b=27)
    #"d9b5bf1d42616ef18e870f74dc7e151e0ac04ba5878ec354bd5c96cdf89df3011e4deab26a33ff112e942632c54f8389bf23d7325e48bf39a02ca48d920fc67d1b",
    // Signature from validator 1 (v=0x1b=27)
    #"a2e7062eaee63dadb0b78120b81eddafcd4c7ad939047cf57e791cc399c65317773b522e365f4c2a68c489e29b2ce54e8d6053e9bae112fae24e6a8db8b59d251b",
  ]
}

/// Test domain hash encoding length
test sepolia_domain_hash_encoding_length() {
  let checkpoint = sepolia_checkpoint()
  let encoded = encode_domain_hash(checkpoint.origin, checkpoint.origin_merkle_tree_hook)

  // Should be: origin(4) + merkle_tree_hook(32) + "HYPERLANE"(9) = 45 bytes
  bytearray.length(encoded) == 45
}

/// Test checkpoint encoding length
test sepolia_checkpoint_encoding_length() {
  let checkpoint = sepolia_checkpoint()
  let domain_hash_encoded = encode_domain_hash(checkpoint.origin, checkpoint.origin_merkle_tree_hook)
  let domain_hash = keccak_256(domain_hash_encoded)

  let checkpoint_encoded = encode_checkpoint(checkpoint, domain_hash)

  // Should be: domain_hash(32) + merkle_root(32) + merkle_index(4) + message_id(32) = 100 bytes
  bytearray.length(checkpoint_encoded) == 100
}

/// Test that domain hash produces 32 bytes
test sepolia_domain_hash_is_32_bytes() {
  let checkpoint = sepolia_checkpoint()
  let domain_hash_encoded = encode_domain_hash(checkpoint.origin, checkpoint.origin_merkle_tree_hook)
  let domain_hash = keccak_256(domain_hash_encoded)

  bytearray.length(domain_hash) == 32
}

/// Test that checkpoint digest produces 32 bytes
test sepolia_checkpoint_digest_is_32_bytes() {
  let checkpoint = sepolia_checkpoint()
  let domain_hash_encoded = encode_domain_hash(checkpoint.origin, checkpoint.origin_merkle_tree_hook)
  let domain_hash = keccak_256(domain_hash_encoded)

  let checkpoint_encoded = encode_checkpoint(checkpoint, domain_hash)
  let checkpoint_digest = keccak_256(checkpoint_encoded)

  bytearray.length(checkpoint_digest) == 32
}

/// Test that EIP-191 message hash produces 32 bytes
test sepolia_eip191_hash_is_32_bytes() {
  let checkpoint = sepolia_checkpoint()

  // Step 1: domain_hash
  let domain_hash_encoded = encode_domain_hash(checkpoint.origin, checkpoint.origin_merkle_tree_hook)
  let domain_hash = keccak_256(domain_hash_encoded)

  // Step 2: checkpoint_digest
  let checkpoint_encoded = encode_checkpoint(checkpoint, domain_hash)
  let checkpoint_digest = keccak_256(checkpoint_encoded)

  // Step 3: EIP-191 hash
  let eip191_prefix = #"19457468657265756d205369676e6564204d6573736167653a0a3332"
  let prefixed = bytearray.concat(eip191_prefix, checkpoint_digest)
  let message_hash = keccak_256(prefixed)

  bytearray.length(message_hash) == 32
}

/// Test validator address lengths
test sepolia_validator_addresses_are_20_bytes() {
  let addresses = sepolia_validator_addresses()

  // All should be 20 bytes (Ethereum addresses)
  expect Some(v0) = addresses |> at(0)
  expect Some(v1) = addresses |> at(1)

  and {
    bytearray.length(v0) == 20,
    bytearray.length(v1) == 20,
  }
}

/// Test uncompressed public key lengths
test sepolia_validator_pubkeys_are_64_bytes() {
  let pubkeys = sepolia_validator_pubkeys_uncompressed()

  // All should be 64 bytes (uncompressed secp256k1 without 0x04 prefix)
  expect Some(v0) = pubkeys |> at(0)
  expect Some(v1) = pubkeys |> at(1)

  and {
    bytearray.length(v0) == 64,
    bytearray.length(v1) == 64,
  }
}

/// Test signature lengths
test sepolia_signatures_are_65_bytes() {
  let sigs = sepolia_signatures()

  expect Some(s0) = sigs |> at(0)
  expect Some(s1) = sigs |> at(1)

  and {
    bytearray.length(s0) == 65,
    bytearray.length(s1) == 65,
  }
}

/// Test extracting 64-byte signature from 65-byte
test sepolia_signature_extraction() {
  let sigs = sepolia_signatures()
  expect Some(sig_65) = sigs |> at(0)

  // Extract first 64 bytes
  let sig_64 = bytearray.take(sig_65, 64)

  bytearray.length(sig_64) == 64
}

/// Test pubkey_to_eth_address for validator 0
/// This verifies our address derivation matches Ethereum's keccak256 method
test sepolia_pubkey_to_address_0() {
  let pubkeys = sepolia_validator_pubkeys_uncompressed()
  let addresses = sepolia_validator_addresses()

  expect Some(pubkey) = pubkeys |> at(0)
  expect Some(expected_address) = addresses |> at(0)

  // Compute address from public key: keccak256(pubkey)[12:32]
  let hash = keccak_256(pubkey)
  let computed_address = bytearray.drop(hash, 12)

  // Verify it matches the expected address
  computed_address == expected_address
}

/// Test pubkey_to_eth_address for validator 1
test sepolia_pubkey_to_address_1() {
  let pubkeys = sepolia_validator_pubkeys_uncompressed()
  let addresses = sepolia_validator_addresses()

  expect Some(pubkey) = pubkeys |> at(1)
  expect Some(expected_address) = addresses |> at(1)

  // Compute address from public key: keccak256(pubkey)[12:32]
  let hash = keccak_256(pubkey)
  let computed_address = bytearray.drop(hash, 12)

  // Verify it matches the expected address
  computed_address == expected_address
}

/// Test full address-based verification flow
/// This simulates what the ISM does when verifying validators
test sepolia_address_based_verification() {
  let addresses = sepolia_validator_addresses()
  let pubkeys = sepolia_validator_pubkeys_uncompressed()

  // For each public key, compute address and check it's in trusted list
  let valid_count = list.foldl(
    pubkeys,
    0,
    fn(pubkey, acc) {
      let hash = keccak_256(pubkey)
      let address = bytearray.drop(hash, 12)

      if list.has(addresses, address) {
        acc + 1
      } else {
        acc
      }
    },
  )

  // Both validators should be valid (threshold = 2)
  valid_count >= 2
}

/// Helper function to get element at index
fn at(items: List<a>, idx: Int) -> Option<a> {
  at_helper(items, idx, 0)
}

fn at_helper(items: List<a>, target_idx: Int, current_idx: Int) -> Option<a> {
  when items is {
    [] -> None
    [head, ..tail] ->
      if current_idx == target_idx {
        Some(head)
      } else {
        at_helper(tail, target_idx, current_idx + 1)
      }
  }
}

// ============================================================================
// HASH VERIFICATION TESTS - from actual relayer execution
// These tests verify that our Aiken hash computation matches the Rust computation
// which should match what the Hyperlane validators signed.
// ============================================================================

/// Actual checkpoint from the Sepolia validator syncers (index 869976,
/// message_id 424b25342a401ee2029bfe8241e61e3e4ff5209913d03c7dafe46e3f0faa0096)
fn actual_checkpoint() -> Checkpoint {
  Checkpoint {
    origin: 11155111,  // Sepolia
    merkle_root: #"2d652e6c03ec3c97d0a20896a7c0ea274c6abc3370dc0c3b3ca3e558503dd118",
    origin_merkle_tree_hook: #"0000000000000000000000004917a9746a7b6e0a57159ccb7f5a6744247f2d0d",
    merkle_index: 869976,
    message_id: #"424b25342a401ee2029bfe8241e61e3e4ff5209913d03c7dafe46e3f0faa0096",
  }
}

/// Debug: Test that origin encodes correctly to big-endian
/// 11155111 = 0x00AA36A7
test origin_encodes_correctly() {
  let origin = 11155111
  let origin_bytes = types.int_to_bytes(origin, 4)
  // Should be 0x00aa36a7 in big-endian
  let expected = #"00aa36a7"
  origin_bytes == expected
}

/// Debug: Test complete domain_hash_encoded bytes match Rust
/// Rust produces: 00aa36a7 || 0000000000000000000000004917a9746a7b6e0a57159ccb7f5a6744247f2d0d || 48595045524c414e45
test domain_hash_encoded_matches_rust() {
  let checkpoint = actual_checkpoint()
  let domain_hash_encoded = encode_domain_hash(checkpoint.origin, checkpoint.origin_merkle_tree_hook)

  // Expected: origin(4) + address(32) + "HYPERLANE"(9) = 45 bytes
  // 00aa36a7 + 0000000000000000000000004917a9746a7b6e0a57159ccb7f5a6744247f2d0d + 48595045524c414e45 (HYPERLANE)
  let expected = #"00aa36a70000000000000000000000004917a9746a7b6e0a57159ccb7f5a6744247f2d0d48595045524c414e45"

  domain_hash_encoded == expected
}

/// Debug: Test domain_hash encoding length
test domain_hash_encoding_correct_length() {
  let checkpoint = actual_checkpoint()
  let domain_hash_encoded = encode_domain_hash(checkpoint.origin, checkpoint.origin_merkle_tree_hook)
  // Should be: origin(4) + merkle_tree_hook(32) + "HYPERLANE"(9) = 45 bytes
  bytearray.length(domain_hash_encoded) == 45
}

/// Test that domain_hash matches the Rust computation
/// Expected from logs: ddc29846dd691ab7d8d8ec0f3008a8bf66116a65c6ef3a859820a7568f42dfd2
test actual_domain_hash_matches() {
  let checkpoint = actual_checkpoint()
  let domain_hash_encoded = encode_domain_hash(checkpoint.origin, checkpoint.origin_merkle_tree_hook)
  let domain_hash = keccak_256(domain_hash_encoded)

  let expected = #"ddc29846dd691ab7d8d8ec0f3008a8bf66116a65c6ef3a859820a7568f42dfd2"
  domain_hash == expected
}

/// Test that checkpoint_digest matches the Rust computation
/// Expected from logs: e78803dcb5f4df980d0e1ac8e0d89ff87ec4bff4be568053056f68a288047b6f
test actual_checkpoint_digest_matches() {
  let checkpoint = actual_checkpoint()

  // Step 1: domain_hash
  let domain_hash_encoded = encode_domain_hash(checkpoint.origin, checkpoint.origin_merkle_tree_hook)
  let domain_hash = keccak_256(domain_hash_encoded)

  // Step 2: checkpoint_digest
  let checkpoint_encoded = encode_checkpoint(checkpoint, domain_hash)
  let checkpoint_digest = keccak_256(checkpoint_encoded)

  let expected = #"e78803dcb5f4df980d0e1ac8e0d89ff87ec4bff4be568053056f68a288047b6f"
  checkpoint_digest == expected
}

/// Test that eth_signed_message (EIP-191) matches the Rust computation
/// Expected from logs: e5bc3d2efa932ef4b864c340fd6775d9b6e7c541feddb0762594c9a596fea2a1
test actual_eth_signed_message_matches() {
  let checkpoint = actual_checkpoint()

  // Step 1: domain_hash
  let domain_hash_encoded = encode_domain_hash(checkpoint.origin, checkpoint.origin_merkle_tree_hook)
  let domain_hash = keccak_256(domain_hash_encoded)

  // Step 2: checkpoint_digest
  let checkpoint_encoded = encode_checkpoint(checkpoint, domain_hash)
  let checkpoint_digest = keccak_256(checkpoint_encoded)

  // Step 3: EIP-191 hash (this is what validators sign)
  let eip191_prefix = #"19457468657265756d205369676e6564204d6573736167653a0a3332"
  let prefixed = bytearray.concat(eip191_prefix, checkpoint_digest)
  let eth_signed_message = keccak_256(prefixed)

  let expected = #"e5bc3d2efa932ef4b864c340fd6775d9b6e7c541feddb0762594c9a596fea2a1"
  eth_signed_message == expected
}

// ---------------------------------------------------------------------------
// Signature counting and threshold
//
// These drive `verify_checkpoint` with the real Sepolia signatures above. The
// tests further up only cover hashing and address derivation, so without these
// the counting path (duplicate rejection, validator-set membership, the ECDSA
// verify itself, and the threshold comparison) runs untested.
// ---------------------------------------------------------------------------

const sepolia_domain = 11155111

/// Compress an uncompressed (x || y) key: 0x02 for an even y, 0x03 for odd.
fn compress(uncompressed: ByteArray) -> ByteArray {
  let x = bytearray.take(uncompressed, 32)
  let prefix =
    if bytearray.at(uncompressed, 63) % 2 == 0 {
      #"02"
    } else {
      #"03"
    }
  bytearray.concat(prefix, x)
}

/// Fixture signatures are 65 bytes (r || s || v); the circuit verifies r || s.
fn sepolia_validator_signature(idx: Int) -> ValidatorSignature {
  expect Some(uncompressed) = at(sepolia_validator_pubkeys_uncompressed(), idx)
  expect Some(signature) = at(sepolia_signatures(), idx)
  ValidatorSignature {
    compressed_pubkey: compress(uncompressed),
    uncompressed_pubkey: uncompressed,
    signature: bytearray.take(signature, 64),
  }
}

fn sepolia_ism_datum(
  threshold: Int,
  validators: List<ByteArray>,
) -> MultisigIsmDatum {
  MultisigIsmDatum {
    validators: [(sepolia_domain, validators)],
    thresholds: [(sepolia_domain, threshold)],
    owner: #"00000000000000000000000000000000000000000000000000000000",
    module_type: MessageIdMultisig,
  }
}

/// Positive control. Without this the negative tests below could pass simply
/// because nothing verifies at all.
test accepts_two_distinct_validators_at_threshold_two() {
  verify_checkpoint(
    sepolia_ism_datum(2, sepolia_validator_addresses()),
    sepolia_checkpoint(),
    [sepolia_validator_signature(0), sepolia_validator_signature(1)],
  )
}

test accepts_one_validator_at_threshold_one() {
  verify_checkpoint(
    sepolia_ism_datum(1, sepolia_validator_addresses()),
    sepolia_checkpoint(),
    [sepolia_validator_signature(0)],
  )
}

/// The error the Hyperlane implementation guide warns about: one validator's
/// signature replayed to reach a threshold it should not reach on its own.
/// Both copies verify and both belong to an enrolled validator, so only the
/// seen-address check in `count_valid_signatures` stops this.
test rejects_one_signature_submitted_twice() {
  !verify_checkpoint(
    sepolia_ism_datum(2, sepolia_validator_addresses()),
    sepolia_checkpoint(),
    [sepolia_validator_signature(0), sepolia_validator_signature(0)],
  )
}

/// Three copies should not reach a threshold of three either.
test rejects_one_signature_submitted_three_times() {
  !verify_checkpoint(
    sepolia_ism_datum(3, sepolia_validator_addresses()),
    sepolia_checkpoint(),
    [
      sepolia_validator_signature(0),
      sepolia_validator_signature(0),
      sepolia_validator_signature(0),
    ],
  )
}

/// A genuine signature from someone who is not in the validator set.
test rejects_a_signer_outside_the_validator_set() {
  expect Some(only_validator_1) = at(sepolia_validator_addresses(), 1)
  !verify_checkpoint(
    sepolia_ism_datum(1, [only_validator_1]),
    sepolia_checkpoint(),
    [sepolia_validator_signature(0)],
  )
}

/// A duplicate must not top up a real quorum either: one enrolled validator
/// plus a repeat of that same validator is still one signature.
test rejects_a_duplicate_padding_a_short_quorum() {
  expect Some(only_validator_0) = at(sepolia_validator_addresses(), 0)
  !verify_checkpoint(
    sepolia_ism_datum(2, [only_validator_0]),
    sepolia_checkpoint(),
    [sepolia_validator_signature(0), sepolia_validator_signature(0)],
  )
}

/// Signatures are bound to the checkpoint they were produced over.
test rejects_signatures_over_a_different_checkpoint() {
  let tampered =
    Checkpoint {
      ..sepolia_checkpoint(),
      merkle_root: #"0000000000000000000000000000000000000000000000000000000000000001",
    }
  !verify_checkpoint(
    sepolia_ism_datum(2, sepolia_validator_addresses()),
    tampered,
    [sepolia_validator_signature(0), sepolia_validator_signature(1)],
  )
}

test rejects_a_tampered_signature() {
  let good = sepolia_validator_signature(0)
  let flipped =
    ValidatorSignature {
      ..good,
      signature: bytearray.concat(
        bytearray.take(good.signature, 63),
        #"ff",
      ),
    }
  !verify_checkpoint(
    sepolia_ism_datum(1, sepolia_validator_addresses()),
    sepolia_checkpoint(),
    [flipped],
  )
}

/// The two key formats must describe the same point. A caller supplying a
/// compressed key from one validator and an uncompressed key from another
/// would otherwise verify against one and be credited as the other.
test rejects_mismatched_key_formats() fail {
  let sig_0 = sepolia_validator_signature(0)
  let sig_1 = sepolia_validator_signature(1)
  let spliced =
    ValidatorSignature { ..sig_0, uncompressed_pubkey: sig_1.uncompressed_pubkey }
  verify_checkpoint(
    sepolia_ism_datum(1, sepolia_validator_addresses()),
    sepolia_checkpoint(),
    [spliced],
  )
}

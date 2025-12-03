use aiken/collection/list
use aiken/crypto.{keccak_256}
use aiken/primitive/bytearray
use types.{Message, Checkpoint, encode_message, encode_domain_hash, encode_checkpoint}

/// Test helper: create a sample message
fn sample_message() -> Message {
  Message {
    version: 3,
    nonce: 1,
    origin: 43114,
    // Fuji Avalanche
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
// REAL FUJI TESTNET DATA TESTS
// These tests use actual data from the Hyperlane Fuji testnet to verify
// our checkpoint and signature verification matches Hyperlane's format.
// ============================================================================

/// Real Fuji checkpoint data from the relayer logs
/// Data extracted from actual Fuji -> Cardano message relay
fn fuji_checkpoint() -> Checkpoint {
  Checkpoint {
    origin: 43113,  // Fuji
    merkle_root: #"08c2d666efa004d027c79c3d7faf7821111493144243a32f8616af99ceff8238",
    // Fuji's merkle tree hook address, left-padded to 32 bytes
    origin_merkle_tree_hook: #"0000000000000000000000009ff6ac3daf63103620bbf76136ea1aff43c2f612",
    merkle_index: 4332,  // 0x10ec
    message_id: #"0ce4b05a9d25d2556f74ddaa1ac84841341623376c9e5cd073f52b1b54dcddbf",
  }
}

/// Real Fuji validator Ethereum addresses (20 bytes each)
/// These addresses were computed from recovered public keys using keccak256
fn fuji_validator_addresses() -> List<ByteArray> {
  [
    // Validator 0: 0xd8154f73d04cc7f7f0c332793692e6e6f6b2402e (official Hyperlane Fuji validator)
    #"d8154f73d04cc7f7f0c332793692e6e6f6b2402e",
    // Validator 1: 0x7095c11126faf3d61b7d1144815720fb09bb8b20 (recovered from actual signature)
    #"7095c11126faf3d61b7d1144815720fb09bb8b20",
  ]
}

/// Real Fuji validator uncompressed public keys (64 bytes each, no 0x04 prefix)
/// These are used by the relayer in the ISM redeemer
fn fuji_validator_pubkeys_uncompressed() -> List<ByteArray> {
  [
    // Validator 0 uncompressed (64 bytes) - address 0xd8154f73d04cc7f7f0c332793692e6e6f6b2402e
    #"225f0eceb966fca4afec433f93cb38d3b0cbb44b066a4a83618fc23d2ccd5c1727d85b0a30fa3607960b5b81d5c4687f5938e5925fd41cebaec73af76971f035",
    // Validator 1 uncompressed (64 bytes) - address 0x7095c11126faf3d61b7d1144815720fb09bb8b20
    #"d55421e166d27cbf76fc924b42c3833604a528b7dd00e0e831c678bd3be76b4f4a989bdd496c5808f1b3de30676246e7a63b9e70c8e80dc3bccb0704ccd13c4b",
  ]
}

/// Real signatures from Fuji validators (65 bytes each: r || s || v)
fn fuji_signatures() -> List<ByteArray> {
  [
    // Signature from validator 0 (v=0x1b=27)
    #"d88d35b30b437c9d069dc3e97263d8b06367ae53840fdb1d0f8009e61ded9cad1ca7cb64f16f21a08634065f7de2cc92d651fa5bd04603e675ad72fffe39b4761b",
    // Signature from validator 1 (v=0x1c=28)
    #"5f2d5eceb1dc4c9a6ce96af2c9d20a4b622a86224535035337fe3c3fdfb71f5e2e195aa0eca28e609a0d2b6550d97d8228d91e976040cf94f8e7124581dfe8261c",
  ]
}

/// Test domain hash encoding length
test fuji_domain_hash_encoding_length() {
  let checkpoint = fuji_checkpoint()
  let encoded = encode_domain_hash(checkpoint.origin, checkpoint.origin_merkle_tree_hook)

  // Should be: origin(4) + merkle_tree_hook(32) + "HYPERLANE"(9) = 45 bytes
  bytearray.length(encoded) == 45
}

/// Test checkpoint encoding length
test fuji_checkpoint_encoding_length() {
  let checkpoint = fuji_checkpoint()
  let domain_hash_encoded = encode_domain_hash(checkpoint.origin, checkpoint.origin_merkle_tree_hook)
  let domain_hash = keccak_256(domain_hash_encoded)

  let checkpoint_encoded = encode_checkpoint(checkpoint, domain_hash)

  // Should be: domain_hash(32) + merkle_root(32) + merkle_index(4) + message_id(32) = 100 bytes
  bytearray.length(checkpoint_encoded) == 100
}

/// Test that domain hash produces 32 bytes
test fuji_domain_hash_is_32_bytes() {
  let checkpoint = fuji_checkpoint()
  let domain_hash_encoded = encode_domain_hash(checkpoint.origin, checkpoint.origin_merkle_tree_hook)
  let domain_hash = keccak_256(domain_hash_encoded)

  bytearray.length(domain_hash) == 32
}

/// Test that checkpoint digest produces 32 bytes
test fuji_checkpoint_digest_is_32_bytes() {
  let checkpoint = fuji_checkpoint()
  let domain_hash_encoded = encode_domain_hash(checkpoint.origin, checkpoint.origin_merkle_tree_hook)
  let domain_hash = keccak_256(domain_hash_encoded)

  let checkpoint_encoded = encode_checkpoint(checkpoint, domain_hash)
  let checkpoint_digest = keccak_256(checkpoint_encoded)

  bytearray.length(checkpoint_digest) == 32
}

/// Test that EIP-191 message hash produces 32 bytes
test fuji_eip191_hash_is_32_bytes() {
  let checkpoint = fuji_checkpoint()

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
test fuji_validator_addresses_are_20_bytes() {
  let addresses = fuji_validator_addresses()

  // All should be 20 bytes (Ethereum addresses)
  expect Some(v0) = addresses |> at(0)
  expect Some(v1) = addresses |> at(1)

  and {
    bytearray.length(v0) == 20,
    bytearray.length(v1) == 20,
  }
}

/// Test uncompressed public key lengths
test fuji_validator_pubkeys_are_64_bytes() {
  let pubkeys = fuji_validator_pubkeys_uncompressed()

  // All should be 64 bytes (uncompressed secp256k1 without 0x04 prefix)
  expect Some(v0) = pubkeys |> at(0)
  expect Some(v1) = pubkeys |> at(1)

  and {
    bytearray.length(v0) == 64,
    bytearray.length(v1) == 64,
  }
}

/// Test signature lengths
test fuji_signatures_are_65_bytes() {
  let sigs = fuji_signatures()

  expect Some(s0) = sigs |> at(0)
  expect Some(s1) = sigs |> at(1)

  and {
    bytearray.length(s0) == 65,
    bytearray.length(s1) == 65,
  }
}

/// Test extracting 64-byte signature from 65-byte
test fuji_signature_extraction() {
  let sigs = fuji_signatures()
  expect Some(sig_65) = sigs |> at(0)

  // Extract first 64 bytes
  let sig_64 = bytearray.take(sig_65, 64)

  bytearray.length(sig_64) == 64
}

/// Test pubkey_to_eth_address for validator 0
/// This verifies our address derivation matches Ethereum's keccak256 method
test fuji_pubkey_to_address_0() {
  let pubkeys = fuji_validator_pubkeys_uncompressed()
  let addresses = fuji_validator_addresses()

  expect Some(pubkey) = pubkeys |> at(0)
  expect Some(expected_address) = addresses |> at(0)

  // Compute address from public key: keccak256(pubkey)[12:32]
  let hash = keccak_256(pubkey)
  let computed_address = bytearray.drop(hash, 12)

  // Verify it matches the expected address
  computed_address == expected_address
}

/// Test pubkey_to_eth_address for validator 1
test fuji_pubkey_to_address_1() {
  let pubkeys = fuji_validator_pubkeys_uncompressed()
  let addresses = fuji_validator_addresses()

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
test fuji_address_based_verification() {
  let addresses = fuji_validator_addresses()
  let pubkeys = fuji_validator_pubkeys_uncompressed()

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

/// Actual checkpoint from relayer logs (message_id 7e2c2f9ef220e8190803eb47033257b562d9104aaa578115aa27601548048d51)
fn actual_checkpoint() -> Checkpoint {
  Checkpoint {
    origin: 43113,  // Fuji
    merkle_root: #"78943434b7600830cf53756b5da5d7bdbed2761edfc997b0e75c9ec95f4f30fb",
    origin_merkle_tree_hook: #"0000000000000000000000009ff6ac3daf63103620bbf76136ea1aff43c2f612",
    merkle_index: 4336,
    message_id: #"7e2c2f9ef220e8190803eb47033257b562d9104aaa578115aa27601548048d51",
  }
}

/// Debug: Test that origin encodes correctly to big-endian
/// 43113 = 0x0000A869
test origin_encodes_correctly() {
  let origin = 43113
  let origin_bytes = types.int_to_bytes(origin, 4)
  // Should be 0x0000a869 in big-endian
  let expected = #"0000a869"
  origin_bytes == expected
}

/// Debug: Test complete domain_hash_encoded bytes match Rust
/// Rust produces: 0000a869 || 0000000000000000000000009ff6ac3daf63103620bbf76136ea1aff43c2f612 || 48595045524c414e45
test domain_hash_encoded_matches_rust() {
  let checkpoint = actual_checkpoint()
  let domain_hash_encoded = encode_domain_hash(checkpoint.origin, checkpoint.origin_merkle_tree_hook)

  // Expected: origin(4) + address(32) + "HYPERLANE"(9) = 45 bytes
  // 0000a869 + 0000000000000000000000009ff6ac3daf63103620bbf76136ea1aff43c2f612 + 48595045524c414e45 (HYPERLANE)
  let expected = #"0000a8690000000000000000000000009ff6ac3daf63103620bbf76136ea1aff43c2f61248595045524c414e45"

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
/// Expected from logs: ce5a1b0dc59cf532dedcf7d71539053b467227b7aaf31e9c5f638de4f3cbcb39
test actual_domain_hash_matches() {
  let checkpoint = actual_checkpoint()
  let domain_hash_encoded = encode_domain_hash(checkpoint.origin, checkpoint.origin_merkle_tree_hook)
  let domain_hash = keccak_256(domain_hash_encoded)

  let expected = #"ce5a1b0dc59cf532dedcf7d71539053b467227b7aaf31e9c5f638de4f3cbcb39"
  domain_hash == expected
}

/// Test that checkpoint_digest matches the Rust computation
/// Expected from logs: ea4d554b7c4b18db749d3ea7db16743763eac877068364b65a53ee1899f7763c
test actual_checkpoint_digest_matches() {
  let checkpoint = actual_checkpoint()

  // Step 1: domain_hash
  let domain_hash_encoded = encode_domain_hash(checkpoint.origin, checkpoint.origin_merkle_tree_hook)
  let domain_hash = keccak_256(domain_hash_encoded)

  // Step 2: checkpoint_digest
  let checkpoint_encoded = encode_checkpoint(checkpoint, domain_hash)
  let checkpoint_digest = keccak_256(checkpoint_encoded)

  let expected = #"ea4d554b7c4b18db749d3ea7db16743763eac877068364b65a53ee1899f7763c"
  checkpoint_digest == expected
}

/// Test that eth_signed_message (EIP-191) matches the Rust computation
/// Expected from logs: 8839216521a0c303cd1d651a5e957b5b770725802f08590aec6c911e9545231f
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

  let expected = #"8839216521a0c303cd1d651a5e957b5b770725802f08590aec6c911e9545231f"
  eth_signed_message == expected
}

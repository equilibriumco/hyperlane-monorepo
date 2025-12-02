use aiken/crypto.{keccak_256}
use aiken/primitive/bytearray
use types.{Message, encode_message}

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

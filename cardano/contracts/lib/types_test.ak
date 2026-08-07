use aiken/crypto.{keccak_256}
use aiken/primitive/bytearray
use types.{HyperlaneAddress, Message, encode_message}

/// Helper to create a valid 32-byte hyperlane address
fn make_hyperlane_address(seed: ByteArray) -> HyperlaneAddress {
  // Hash the seed and take first 32 bytes (keccak_256 produces 32 bytes)
  keccak_256(seed)
}

/// Test that encode_message produces deterministic output
test encode_message_deterministic() {
  let message =
    Message {
      version: 3,
      nonce: 1,
      origin: 1,
      sender: make_hyperlane_address("sender"),
      destination: 2,
      recipient: make_hyperlane_address("recipient"),
      body: #"48656c6c6f",
    }

  let encoded1 = encode_message(message)
  let encoded2 = encode_message(message)

  encoded1 == encoded2
}

/// Test that different messages produce different encodings
test encode_message_different_messages() {
  let message1 =
    Message {
      version: 3,
      nonce: 1,
      origin: 1,
      sender: make_hyperlane_address("sender"),
      destination: 2,
      recipient: make_hyperlane_address("recipient"),
      body: #"48656c6c6f",
    }

  let message2 =
    Message {
      version: 3,
      nonce: 2,
      origin: 1,
      sender: make_hyperlane_address("sender"),
      destination: 2,
      recipient: make_hyperlane_address("recipient"),
      body: #"48656c6c6f",
    }

  encode_message(message1) != encode_message(message2)
}

/// Test that different nonces produce different encodings
test encode_message_nonce_affects_encoding() {
  let base_message =
    Message {
      version: 3,
      nonce: 0,
      origin: 1,
      sender: make_hyperlane_address("sender"),
      destination: 2,
      recipient: make_hyperlane_address("recipient"),
      body: #"",
    }

  let message_nonce_1 = Message { ..base_message, nonce: 1 }
  let message_nonce_2 = Message { ..base_message, nonce: 2 }

  encode_message(message_nonce_1) != encode_message(message_nonce_2)
}

/// Test that different origins produce different encodings
test encode_message_origin_affects_encoding() {
  let base_message =
    Message {
      version: 3,
      nonce: 0,
      origin: 1,
      sender: make_hyperlane_address("sender"),
      destination: 2,
      recipient: make_hyperlane_address("recipient"),
      body: #"",
    }

  let message_origin_1 = Message { ..base_message, origin: 1 }
  let message_origin_2 = Message { ..base_message, origin: 100 }

  encode_message(message_origin_1) != encode_message(message_origin_2)
}

/// Test that different destinations produce different encodings
test encode_message_destination_affects_encoding() {
  let base_message =
    Message {
      version: 3,
      nonce: 0,
      origin: 1,
      sender: make_hyperlane_address("sender"),
      destination: 2,
      recipient: make_hyperlane_address("recipient"),
      body: #"",
    }

  let message_dest_1 = Message { ..base_message, destination: 2 }
  let message_dest_2 = Message { ..base_message, destination: 3 }

  encode_message(message_dest_1) != encode_message(message_dest_2)
}

/// Test that different bodies produce different encodings
test encode_message_body_affects_encoding() {
  let base_message =
    Message {
      version: 3,
      nonce: 0,
      origin: 1,
      sender: make_hyperlane_address("sender"),
      destination: 2,
      recipient: make_hyperlane_address("recipient"),
      body: #"",
    }

  let message_body_1 = Message { ..base_message, body: #"01" }
  let message_body_2 = Message { ..base_message, body: #"02" }

  encode_message(message_body_1) != encode_message(message_body_2)
}

/// Test that different senders produce different encodings
test encode_message_sender_affects_encoding() {
  let base_message =
    Message {
      version: 3,
      nonce: 0,
      origin: 1,
      sender: make_hyperlane_address("sender1"),
      destination: 2,
      recipient: make_hyperlane_address("recipient"),
      body: #"",
    }

  let message_sender_2 =
    Message { ..base_message, sender: make_hyperlane_address("sender2") }

  encode_message(base_message) != encode_message(message_sender_2)
}

/// Test that different recipients produce different encodings
test encode_message_recipient_affects_encoding() {
  let base_message =
    Message {
      version: 3,
      nonce: 0,
      origin: 1,
      sender: make_hyperlane_address("sender"),
      destination: 2,
      recipient: make_hyperlane_address("recipient1"),
      body: #"",
    }

  let message_recipient_2 =
    Message { ..base_message, recipient: make_hyperlane_address("recipient2") }

  encode_message(base_message) != encode_message(message_recipient_2)
}

/// Test encoding produces non-empty result
test encode_message_non_empty() {
  let message =
    Message {
      version: 3,
      nonce: 0,
      origin: 1,
      sender: make_hyperlane_address("sender"),
      destination: 2,
      recipient: make_hyperlane_address("recipient"),
      body: #"",
    }

  bytearray.length(encode_message(message)) > 0
}

/// Test that message ID (hash of encoded message) is 32 bytes
test message_id_is_32_bytes() {
  let message =
    Message {
      version: 3,
      nonce: 0,
      origin: 1,
      sender: make_hyperlane_address("sender"),
      destination: 2,
      recipient: make_hyperlane_address("recipient"),
      body: #"74657374626f6479",
    }

  let message_id = keccak_256(encode_message(message))
  bytearray.length(message_id) == 32
}

/// Test that same message always produces same message ID
test message_id_deterministic() {
  let message =
    Message {
      version: 3,
      nonce: 42,
      origin: 1,
      sender: make_hyperlane_address("sender"),
      destination: 2,
      recipient: make_hyperlane_address("recipient"),
      body: #"68656c6c6f776f726c64",
    }

  let id1 = keccak_256(encode_message(message))
  let id2 = keccak_256(encode_message(message))

  id1 == id2
}

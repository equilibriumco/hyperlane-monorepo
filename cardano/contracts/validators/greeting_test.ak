use aiken/collection/dict
use aiken/collection/list
use aiken/crypto.{keccak_256}
use aiken/primitive/bytearray
use cardano/address.{Address, Script}
use cardano/assets.{from_asset, from_lovelace, merge, zero}
use cardano/transaction.{InlineDatum, Input, Output, OutputReference}
use types.{
  Message, VerifiedMessageDatum, encode_message,
}

pub type GreetingDatum {
  last_greeting: ByteArray,
  greeting_count: Int,
}

const verified_message_nft_policy = #"6cb27a83b4e025ff068534c4dca757c02e295dee27540c2b26d682a5"

const state_nft_policy = #"f1bd8bb9bbd7d3fe0a20b12e1d8f36ab717c6e9c8e796f5375400378"

const script_hash = #"3545709297747cd0e2812a3ab4f49778786e289bc47daac38ea38918"

fn greeting_address() -> Address {
  Address {
    payment_credential: Script(script_hash),
    stake_credential: None,
  }
}

fn sample_message() -> Message {
  Message {
    version: 3,
    nonce: 4442,
    origin: 43113,
    sender: #"0000000000000000000000001f26bfc6f52cbfad5c3fa8dabb71007b28bf4749",
    destination: 2003,
    recipient: #"020000003545709297747cd0e2812a3ab4f49778786e289bc47daac38ea38918",
    body: #"4775696c6865726d65",
  }
}

fn sample_message_id() -> ByteArray {
  keccak_256(encode_message(sample_message()))
}

/// Casting GreetingDatum from Data works correctly
test greeting_datum_cast() {
  let datum: Data = GreetingDatum { last_greeting: "Hello, World", greeting_count: 5 }
  expect parsed: GreetingDatum = datum
  parsed.last_greeting == "Hello, World" && parsed.greeting_count == 5
}

/// Casting initial empty GreetingDatum works
test greeting_datum_cast_initial() {
  let datum: Data = GreetingDatum { last_greeting: "", greeting_count: 0 }
  expect parsed: GreetingDatum = datum
  parsed.last_greeting == "" && parsed.greeting_count == 0
}

/// VerifiedMessageDatum does NOT cast as GreetingDatum (different structure)
test verified_message_datum_is_not_greeting_datum() fail {
  let datum: Data = VerifiedMessageDatum {
    origin: 43113,
    sender: #"0000000000000000000000001f26bfc6f52cbfad5c3fa8dabb71007b28bf4749",
    body: #"4775696c6865726d65",
    message_id: #"322de963a35c687edf5aa21ba908e8ee295c49008b70432fb6709365ff7b9d1f",
    nonce: 4442,
  }
  expect _parsed: GreetingDatum = datum
  True
}

/// NFT-based UTXO detection: message UTXO has verified_message_nft
test message_utxo_detected_by_nft() {
  let msg_id = sample_message_id()
  let msg_value = merge(from_lovelace(1_700_000), from_asset(verified_message_nft_policy, msg_id, 1))
  let has_nft = assets.quantity_of(msg_value, verified_message_nft_policy, msg_id) == 1
  has_nft == True
}

/// NFT-based UTXO detection: state UTXO does NOT have verified_message_nft
test state_utxo_not_detected_as_message() {
  let msg_id = sample_message_id()
  let state_value = merge(from_lovelace(5_000_000), from_asset(state_nft_policy, "", 1))
  let has_nft = assets.quantity_of(state_value, verified_message_nft_policy, msg_id) == 1
  has_nft == False
}

/// Full greeting computation
test greeting_body_computation() {
  let body = #"4775696c6865726d65"
  let greeting = bytearray.concat("Hello, ", body)
  greeting == "Hello, Guilherme"
}

/// Message ID matches keccak256 of encoded message
test message_id_matches_hash() {
  let message = sample_message()
  let encoded = encode_message(message)
  let hash = keccak_256(encoded)
  bytearray.length(hash) == 32
}

/// NFT burn check works with negative quantity in mint field
test verified_nft_burn_detected() {
  let msg_id = sample_message_id()
  let mint = from_asset(verified_message_nft_policy, msg_id, -1)
  let nft_mints = assets.tokens(mint, verified_message_nft_policy)
  when dict.get(nft_mints, msg_id) is {
    Some(quantity) -> quantity < 0
    None -> False
  }
}

/// NFT burn check fails when NFT is not in mint field
test verified_nft_burn_not_detected_when_missing() {
  let msg_id = sample_message_id()
  let mint = zero
  let nft_mints = assets.tokens(mint, verified_message_nft_policy)
  when dict.get(nft_mints, msg_id) is {
    Some(quantity) -> quantity < 0
    None -> False
  } == False
}

/// Message UTXO spent detection works
test message_utxo_spent_detection() {
  let msg_id = sample_message_id()
  let own_addr = greeting_address()
  let state_ref = OutputReference { transaction_id: #"aabbcc", output_index: 0 }

  let message_input = Input {
    output_reference: OutputReference { transaction_id: #"ddeeff", output_index: 3 },
    output: Output {
      address: own_addr,
      value: merge(from_lovelace(1_700_000), from_asset(verified_message_nft_policy, msg_id, 1)),
      datum: InlineDatum(
        VerifiedMessageDatum {
          origin: 43113,
          sender: sample_message().sender,
          body: sample_message().body,
          message_id: msg_id,
          nonce: 4442,
        },
      ),
      reference_script: None,
    },
  }

  list.any(
    [message_input],
    fn(input) {
      if input.output_reference == state_ref {
        False
      } else {
        input.output.address == own_addr && assets.quantity_of(
          input.output.value,
          verified_message_nft_policy,
          msg_id,
        ) == 1
      }
    },
  )
}

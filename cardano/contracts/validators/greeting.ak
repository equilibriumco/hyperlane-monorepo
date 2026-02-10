use aiken/collection/dict
use aiken/collection/list
use aiken/crypto.{keccak_256}
use aiken/primitive/bytearray
use cardano/assets
use cardano/address.{Address}
use cardano/transaction.{InlineDatum, OutputReference, Transaction}
use types.{
  Message, PolicyId,
  encode_message,
}
use utils.{find_input}

pub type GreetingDatum {
  last_greeting: ByteArray,
  greeting_count: Int,
}

pub type GreetingRedeemer {
  /// Handle a verified message delivered by the mailbox to this script address.
  /// Burns the verified_message_nft and updates the greeting state.
  HandleMessage {
    message: Message,
    message_id: ByteArray,
  }
}

validator greeting(verified_message_nft_policy: PolicyId) {
  spend(
    datum: Option<Data>,
    redeemer: GreetingRedeemer,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    expect Some(own_input) = find_input(tx, own_ref)

    let HandleMessage { message, message_id } = redeemer

    // Determine UTXO type by checking which NFT the input holds:
    // - Message UTXO has verified_message_nft → just check burn
    // - State UTXO has the greeting state NFT → full message processing
    let is_message_utxo =
      assets.quantity_of(
        own_input.output.value,
        verified_message_nft_policy,
        message_id,
      ) == 1

    if is_message_utxo {
      verified_nft_burned(tx, verified_message_nft_policy, message_id)
    } else {
      expect Some(raw_datum) = datum
      expect old_datum: GreetingDatum = raw_datum

      let greeting = bytearray.concat("Hello, ", message.body)
      let own_addr = own_input.output.address

      expect Some(continuation) =
        list.find(tx.outputs, fn(output) { output.address == own_addr })

      expect InlineDatum(cont_datum) = continuation.datum
      expect new_datum: GreetingDatum = cont_datum

      expect keccak_256(encode_message(message)) == message_id
      expect verified_nft_burned(tx, verified_message_nft_policy, message_id)
      expect message_utxo_spent(
        tx,
        own_addr,
        verified_message_nft_policy,
        message_id,
        own_ref,
      )

      new_datum.last_greeting == greeting && new_datum.greeting_count == old_datum.greeting_count + 1
    }
  }

  else(_) {
    fail
  }
}

/// Check that the verified_message_nft with this message_id is being burned
fn verified_nft_burned(
  tx: Transaction,
  nft_policy: PolicyId,
  message_id: ByteArray,
) -> Bool {
  let nft_mints = assets.tokens(tx.mint, nft_policy)
  when dict.get(nft_mints, message_id) is {
    Some(quantity) -> quantity < 0
    None -> False
  }
}

/// Check that a message UTXO at our address with the verified_message_nft is being spent
fn message_utxo_spent(
  tx: Transaction,
  own_addr: Address,
  nft_policy: PolicyId,
  message_id: ByteArray,
  state_ref: OutputReference,
) -> Bool {
  list.any(
    tx.inputs,
    fn(input) {
      // Skip the state UTXO itself
      if input.output_reference == state_ref {
        False
      } else {
        input.output.address == own_addr && assets.quantity_of(
          input.output.value,
          nft_policy,
          message_id,
        ) == 1
      }
    },
  )
}

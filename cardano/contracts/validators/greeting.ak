use aiken/collection/list
use aiken/primitive/bytearray
use cardano/transaction.{InlineDatum, OutputReference, Transaction}
use types.{PolicyId}
use utils.{find_input}

pub type GreetingDatum {
  last_greeting: ByteArray,
  greeting_count: Int,
}

pub type GreetingRedeemer {
  HandleMessage { body: ByteArray }
}

validator greeting(_mailbox_policy_id: PolicyId) {
  spend(
    datum: Option<GreetingDatum>,
    redeemer: GreetingRedeemer,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    expect Some(old_datum) = datum
    expect Some(own_input) = find_input(tx, own_ref)

    let HandleMessage { body } = redeemer
    let greeting = bytearray.concat("Hello, ", body)

    let own_addr = own_input.output.address

    expect Some(continuation) =
      list.find(tx.outputs, fn(output) { output.address == own_addr })

    expect InlineDatum(raw_datum) = continuation.datum
    expect new_datum: GreetingDatum = raw_datum

    new_datum.last_greeting == greeting && new_datum.greeting_count == old_datum.greeting_count + 1
  }

  else(_) {
    fail
  }
}

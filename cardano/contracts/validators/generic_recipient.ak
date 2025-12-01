use aiken/collection/list
use cardano/transaction.{InlineDatum, OutputReference, Transaction}
use cardano/assets.{lovelace_of}
use types.{
  Domain, HyperlaneAddress, HyperlaneRecipientDatum, HyperlaneRecipientRedeemer,
  ScriptHash, HandleMessage, ContractAction,
}
use utils.{find_input, has_script_input}

/// Generic Recipient Example
/// A minimal Hyperlane recipient that stores received messages
///
/// This demonstrates the standard pattern for Hyperlane-compatible recipients:
/// 1. Use HyperlaneRecipientDatum wrapper for state
/// 2. Use HyperlaneRecipientRedeemer for actions
/// 3. Verify mailbox is the caller for HandleMessage
/// 4. Maintain state continuation
/// Contract-specific state for this generic recipient
pub type GenericRecipientInner {
  /// Count of messages received
  messages_received: Int,
  /// Last message body (for demonstration)
  last_message: Option<ByteArray>,
}

/// Generic recipient validator
/// Parameterized by the mailbox script hash
validator generic_recipient(mailbox_hash: ScriptHash) {
  spend(
    datum: Option<HyperlaneRecipientDatum<GenericRecipientInner>>,
    redeemer: HyperlaneRecipientRedeemer<Void>,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    expect Some(recipient_datum) = datum

    when redeemer is {
      HandleMessage { origin, sender, body } ->
        handle_message(
          recipient_datum,
          origin,
          sender,
          body,
          tx,
          own_ref,
          mailbox_hash,
        )

      ContractAction { action: _ } ->
        // No custom actions for this simple recipient
        False
    }
  }

  else(_) {
    fail
  }
}

/// Handle an incoming Hyperlane message
fn handle_message(
  datum: HyperlaneRecipientDatum<GenericRecipientInner>,
  _origin: Domain,
  _sender: HyperlaneAddress,
  body: ByteArray,
  tx: Transaction,
  own_ref: OutputReference,
  mailbox_hash: ScriptHash,
) -> Bool {
  // 1. Verify called by Mailbox (mailbox must be spending its UTXO too)
  expect mailbox_is_caller(tx, mailbox_hash)

  // 2. Update state
  let new_inner =
    GenericRecipientInner {
      messages_received: datum.inner.messages_received + 1,
      last_message: Some(body),
    }

  // 3. Optionally update nonce for ordering
  let new_nonce =
    when datum.last_processed_nonce is {
      Some(n) -> Some(n + 1)
      None -> Some(1)
    }

  let new_datum =
    HyperlaneRecipientDatum {
      ism: datum.ism,
      last_processed_nonce: new_nonce,
      inner: new_inner,
    }

  // 4. Validate continuation
  validate_recipient_continuation(datum, new_datum, tx, own_ref)
}

/// Verify that the mailbox is calling this recipient
fn mailbox_is_caller(tx: Transaction, mailbox_hash: ScriptHash) -> Bool {
  // Mailbox UTXO must be spent in the same transaction
  // This ensures the message was validated by the mailbox
  has_script_input(tx, mailbox_hash)
}

/// Validate recipient state continuation
fn validate_recipient_continuation(
  _old_datum: HyperlaneRecipientDatum<GenericRecipientInner>,
  new_datum: HyperlaneRecipientDatum<GenericRecipientInner>,
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
  expect cont_datum: HyperlaneRecipientDatum<GenericRecipientInner> =
    cont_datum_data
  expect cont_datum == new_datum

  // Value must be preserved
  lovelace_of(continuation.value) >= lovelace_of(own_value)
}

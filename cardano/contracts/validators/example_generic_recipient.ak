use aiken/collection/list
use cardano/transaction.{InlineDatum, OutputReference, Transaction}
use cardano/assets.{lovelace_of, quantity_of}
use types.{
  Domain, HyperlaneAddress, HyperlaneRecipientDatum, HyperlaneRecipientRedeemer,
  PolicyId, HandleMessage, ContractAction,
}
use utils.{find_input}

/// Example: Generic Recipient
/// A minimal Hyperlane recipient that stores received messages
///
/// This is an EXAMPLE contract demonstrating the standard pattern for
/// Hyperlane-compatible recipients using the Generic type:
/// 1. Use HyperlaneRecipientDatum wrapper for state
/// 2. Use HyperlaneRecipientRedeemer for actions
/// 3. Verify mailbox is the caller for HandleMessage (via NFT check)
/// 4. Maintain state continuation
///
/// See also: example_deferred_recipient.ak for deferred message processing
///
/// ## Security Model
///
/// Recipients are protected by **NFT-based authentication**:
///
/// ### Why NFT, Not Script Hash?
///
/// Anyone can create a UTXO at any script address with any datum. If we only
/// checked "is there an input from the mailbox script address?", an attacker
/// could:
/// 1. Create a fake UTXO at the mailbox address with malicious datum
/// 2. Spend that fake UTXO to bypass security checks
///
/// NFTs solve this because:
/// - Only ONE NFT with a given policy_id can exist (enforced by minting policy)
/// - The real mailbox UTXO contains this unique NFT
/// - Attackers cannot mint another NFT with the same policy_id
/// - Checking for the NFT guarantees we're interacting with the REAL mailbox
///
/// ### ISM (Interchain Security Module)
///
/// The `ism` field in the datum is reserved for future use. Currently, all
/// recipients use the mailbox's default ISM.
///
/// **Why?** On Cardano, anyone can create a UTXO at any script address with
/// any datum. If the mailbox read the ISM from the recipient's datum, an
/// attacker could create a fake recipient UTXO with a malicious ISM.
///
/// For recipients needing custom security, the mailbox owner can configure
/// a stricter default ISM.
///
/// ## Datum Structure
///
/// ```
/// HyperlaneRecipientDatum {
///   ism: Option<ScriptHash>,        // Custom ISM, or None for default
///   last_processed_nonce: Option<Int>, // For message ordering
///   inner: GenericRecipientInner,   // Your contract-specific state
/// }
/// ```
///
/// Contract-specific state for this generic recipient
pub type GenericRecipientInner {
  /// Count of messages received
  messages_received: Int,
  /// Last message body (for demonstration)
  last_message: Option<ByteArray>,
}

/// Example Generic Recipient validator
///
/// IMPORTANT: Parameterized by `mailbox_policy_id` (the NFT policy), NOT script hash!
/// This ensures we verify the REAL mailbox, not a fake UTXO at the same address.
validator example_generic_recipient(mailbox_policy_id: PolicyId) {
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
          mailbox_policy_id,
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
  mailbox_policy_id: PolicyId,
) -> Bool {
  // 1. Verify called by the REAL Mailbox (check for mailbox NFT, not just script hash)
  //
  // SECURITY: This is critical! We check for the unique mailbox NFT, not just
  // any input from the mailbox script address. This prevents attackers from
  // creating fake mailbox UTXOs to bypass security.
  expect mailbox_is_caller(tx, mailbox_policy_id)

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

/// Verify that the REAL mailbox is calling this recipient
///
/// SECURITY: We check for the mailbox's unique NFT, not just the script hash.
/// This prevents attacks where someone creates a fake UTXO at the mailbox address.
///
/// The mailbox NFT is minted once during deployment and lives in the mailbox UTXO.
/// Only the real mailbox UTXO contains this NFT - attackers cannot mint another.
fn mailbox_is_caller(tx: Transaction, mailbox_policy_id: PolicyId) -> Bool {
  // Check if any input contains the mailbox NFT
  // The NFT has empty asset name "" by convention
  list.any(
    tx.inputs,
    fn(input) { quantity_of(input.output.value, mailbox_policy_id, "") > 0 },
  )
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

use aiken/collection/list
use aiken/primitive/bytearray
use cardano/transaction.{InlineDatum, Output, OutputReference, Transaction}
use types.{
  Domain, PolicyId, ValidatorAnnounceDatum, ValidatorAnnounceRedeemer,
  Announce, Revoke,
}

/// Validator Announce Contract
/// Allows validators to publicly announce their checkpoint storage locations
///
/// This enables relayers to discover where validators store their signed checkpoints.
/// Each validator creates a UTXO at this script address with their announcement datum.
///
/// **Announcement Flow:**
/// 1. Validator signs checkpoints and stores them at a known location (S3, GCS, etc.)
/// 2. Validator creates a transaction that produces a UTXO at this script address
/// 3. The UTXO datum contains: validator pubkey, mailbox reference, storage location
/// 4. Relayers query UTXOs at this address to discover validator storage locations
/// 5. Relayers fetch signed checkpoints from announced locations
///
/// **Security:**
/// - Only the validator who created an announcement can update or revoke it
/// - This is enforced by requiring the validator's signature on update/revoke
validator validator_announce(mailbox_policy_id: PolicyId, mailbox_domain: Domain) {
  spend(
    datum: Option<ValidatorAnnounceDatum>,
    redeemer: ValidatorAnnounceRedeemer,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    when redeemer is {
      Announce { storage_location } ->
        validate_announce(
          datum,
          mailbox_policy_id,
          mailbox_domain,
          storage_location,
          tx,
          own_ref,
        )

      Revoke -> validate_revoke(datum, tx)
    }
  }

  else(_) {
    fail
  }
}

/// Validate a new announcement or update to existing
fn validate_announce(
  datum: Option<ValidatorAnnounceDatum>,
  mailbox_policy_id: PolicyId,
  mailbox_domain: Domain,
  new_storage_location: ByteArray,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // Storage location must not be empty
  expect bytearray.length(new_storage_location) > 0

  when datum is {
    // New announcement - validator is creating their first announcement
    None -> validate_new_announcement(
      mailbox_policy_id,
      mailbox_domain,
      new_storage_location,
      tx,
      own_ref,
    )

    // Update existing announcement
    Some(existing_datum) -> validate_update_announcement(
      existing_datum,
      new_storage_location,
      tx,
      own_ref,
    )
  }
}

/// Validate creating a new announcement
fn validate_new_announcement(
  mailbox_policy_id: PolicyId,
  mailbox_domain: Domain,
  storage_location: ByteArray,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // Find own input to get script address
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address

  // Find continuation output at same address
  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  // Parse the new datum
  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: ValidatorAnnounceDatum = cont_datum_data

  // Verify the datum is properly formed
  expect bytearray.length(cont_datum.validator_pubkey) == 32
  expect cont_datum.mailbox_policy_id == mailbox_policy_id
  expect cont_datum.mailbox_domain == mailbox_domain
  expect cont_datum.storage_location == storage_location

  // Verify the validator signed the transaction
  // The validator_pubkey field should be a verification key hash
  expect is_signed_by_validator(tx, cont_datum.validator_pubkey)

  True
}

/// Validate updating an existing announcement
fn validate_update_announcement(
  existing_datum: ValidatorAnnounceDatum,
  new_storage_location: ByteArray,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // Only the validator who created the announcement can update it
  expect is_signed_by_validator(tx, existing_datum.validator_pubkey)

  // Find own input
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address

  // Find continuation output
  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  // Parse continuation datum
  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: ValidatorAnnounceDatum = cont_datum_data

  // Verify only storage_location changed, other fields preserved
  expect cont_datum.validator_pubkey == existing_datum.validator_pubkey
  expect cont_datum.mailbox_policy_id == existing_datum.mailbox_policy_id
  expect cont_datum.mailbox_domain == existing_datum.mailbox_domain
  expect cont_datum.storage_location == new_storage_location

  True
}

/// Validate revoking an announcement
fn validate_revoke(datum: Option<ValidatorAnnounceDatum>, tx: Transaction) -> Bool {
  // Can only revoke an existing announcement
  expect Some(existing_datum) = datum

  // Only the validator who created the announcement can revoke it
  expect is_signed_by_validator(tx, existing_datum.validator_pubkey)

  // No continuation output required - the UTXO is consumed
  True
}

/// Check if transaction is signed by the validator
/// The validator_pubkey field contains the ed25519 verification key hash (28 bytes padded to 32)
fn is_signed_by_validator(tx: Transaction, pubkey: ByteArray) -> Bool {
  // Extract the 28-byte verification key hash from the 32-byte validator_pubkey field
  // Assuming left-padding with zeros
  let vkh_bytes = bytearray.slice(pubkey, 4, 32)
  list.any(
    tx.extra_signatories,
    fn(signer) {
      // Compare the signer (VerificationKeyHash) with the extracted bytes
      signer == vkh_bytes
    },
  )
}

/// Find an input by output reference
fn find_input(tx: Transaction, ref: OutputReference) {
  list.find(
    tx.inputs,
    fn(input) { input.output_reference == ref },
  )
}

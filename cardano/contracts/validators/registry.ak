use aiken/collection/list
use aiken/primitive/bytearray
use cardano/transaction.{InlineDatum, OutputReference, Transaction}
use cardano/assets.{lovelace_of}
use types.{
  RecipientRegistration, RegistryDatum, RegistryRedeemer, ScriptHash,
  Register, Unregister, UpdateRegistration,
}
use utils.{find_input, has_script_input}

/// Recipient Registry Validator
/// Maintains a registry of Hyperlane-compatible recipients
///
/// The registry stores information about each recipient that allows
/// the relayer to construct transactions without recipient-specific code:
/// - State UTXO location (via NFT marker)
/// - Additional inputs required
/// - Recipient type (determines output construction)
/// - Custom ISM override (optional)
/// Registry validator
validator registry {
  spend(
    datum: Option<RegistryDatum>,
    redeemer: RegistryRedeemer,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    expect Some(registry_datum) = datum

    when redeemer is {
      Register { registration } ->
        validate_register(registry_datum, registration, tx, own_ref)

      Unregister { script_hash } ->
        validate_unregister(registry_datum, script_hash, tx, own_ref)

      UpdateRegistration { registration } ->
        validate_update(registry_datum, registration, tx, own_ref)
    }
  }

  else(_) {
    fail
  }
}

/// Validate registering a new recipient
fn validate_register(
  datum: RegistryDatum,
  registration: RecipientRegistration,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // Verify caller owns the script being registered
  // (the script must be spent in this transaction)
  expect validate_script_ownership(registration.script_hash, tx)

  // Verify registration is valid
  expect validate_registration(registration)

  // Verify script not already registered
  expect
    !list.any(
      datum.registrations,
      fn(r) { r.script_hash == registration.script_hash },
    )

  // Build new registrations list
  let new_registrations = [registration, ..datum.registrations]
  let new_datum = RegistryDatum { ..datum, registrations: new_registrations }

  validate_registry_continuation(datum, new_datum, tx, own_ref)
}

/// Validate unregistering a recipient
fn validate_unregister(
  datum: RegistryDatum,
  script_hash: ScriptHash,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // Verify caller owns the script being unregistered
  expect validate_script_ownership(script_hash, tx)

  // Remove from registrations list
  let new_registrations =
    list.filter(datum.registrations, fn(r) { r.script_hash != script_hash })

  let new_datum = RegistryDatum { ..datum, registrations: new_registrations }

  validate_registry_continuation(datum, new_datum, tx, own_ref)
}

/// Validate updating a registration
fn validate_update(
  datum: RegistryDatum,
  registration: RecipientRegistration,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // Verify caller owns the script being updated
  expect validate_script_ownership(registration.script_hash, tx)

  // Verify registration is valid
  expect validate_registration(registration)

  // Update the registration in the list
  let new_registrations =
    list.map(
      datum.registrations,
      fn(r) {
        if r.script_hash == registration.script_hash {
          registration
        } else {
          r
        }
      },
    )

  let new_datum = RegistryDatum { ..datum, registrations: new_registrations }

  validate_registry_continuation(datum, new_datum, tx, own_ref)
}

/// Verify that the caller owns/controls the script
/// The script must be spent in this transaction to prove ownership
fn validate_script_ownership(script_hash: ScriptHash, tx: Transaction) -> Bool {
  has_script_input(tx, script_hash)
}

/// Validate that a registration is well-formed
fn validate_registration(reg: RecipientRegistration) -> Bool {
  // State locator must have a valid policy ID (28 bytes)
  let valid_policy = bytearray.length(reg.state_locator.policy_id) == 28

  // Script hash must be valid (28 bytes)
  let valid_script = bytearray.length(reg.script_hash) == 28

  valid_policy && valid_script
}

/// Validate registry continuation
fn validate_registry_continuation(
  _old_datum: RegistryDatum,
  new_datum: RegistryDatum,
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
  expect cont_datum: RegistryDatum = cont_datum_data
  expect cont_datum == new_datum

  // Value must be preserved (at least same lovelace)
  lovelace_of(continuation.value) >= lovelace_of(own_value)
}

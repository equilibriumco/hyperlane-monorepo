use aiken/collection/list
use aiken/crypto.{VerificationKeyHash}
use aiken/primitive/bytearray
use cardano/transaction.{InlineDatum, OutputReference, Transaction}
use cardano/assets.{lovelace_of}
use types.{
  AdditionalInput, RecipientRegistration, RecipientType, RegistryDatum,
  RegistryRedeemer, Register, Unregister, UpdateRegistration, AdminRegister,
  TransferRegistrationOwnership, ScriptHash, UtxoLocator,
}
use utils.{find_input, has_nft_in_inputs, is_signed_by}

/// Recipient Registry Validator
///
/// Maintains a registry of Hyperlane-compatible recipients so the relayer
/// knows how to construct transactions for message delivery.
///
/// ## Security Model
///
/// Each registration has an `owner` field (VerificationKeyHash) that controls
/// who can modify or remove the registration. This prevents attacks where:
///
/// 1. Attacker waits for legitimate script spend (e.g., HandleMessage)
/// 2. Attacker includes registry update in same transaction
/// 3. Old check "is script being spent?" would pass
/// 4. Attacker modifies state_locator, recipient_type, etc.
///
/// The fix: Require the registration owner's signature for any modification.
/// Spending the script alone is NOT sufficient.
///
/// ## Actions
///
/// - Register: Create new registration (requires owner signature + state NFT)
/// - UpdateRegistration: Modify registration (requires owner signature)
/// - Unregister: Remove registration (requires owner signature)
/// - TransferOwnership: Change owner (requires both current and new owner)
/// - AdminRegister: Registry admin can register any script
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

      UpdateRegistration {
        script_hash,
        new_state_locator,
        new_reference_script_locator,
        new_additional_inputs,
        new_recipient_type,
        new_custom_ism,
      } ->
        validate_update(
          registry_datum,
          script_hash,
          new_state_locator,
          new_reference_script_locator,
          new_additional_inputs,
          new_recipient_type,
          new_custom_ism,
          tx,
          own_ref,
        )

      Unregister { script_hash } ->
        validate_unregister(registry_datum, script_hash, tx, own_ref)

      TransferRegistrationOwnership { script_hash, new_owner } ->
        validate_transfer_ownership(
          registry_datum,
          script_hash,
          new_owner,
          tx,
          own_ref,
        )

      AdminRegister { registration } ->
        validate_admin_register(registry_datum, registration, tx, own_ref)
    }
  }

  else(_) {
    fail
  }
}

/// Validate registering a new recipient
///
/// Security requirements:
/// 1. Owner must sign the transaction (establishes ownership)
/// 2. State NFT must be in transaction inputs (proves control of state UTXO)
/// 3. Script must not already be registered
/// 4. Registration must be well-formed
fn validate_register(
  datum: RegistryDatum,
  registration: RecipientRegistration,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // 1. Owner must sign - THIS IS KEY FOR SECURITY
  // The owner is who will control this registration going forward
  let owner_signed = is_signed_by(tx, registration.owner)

  // 2. State NFT must be present in inputs
  // This proves the registrant actually controls the state UTXO
  let state_nft_present =
    has_nft_in_inputs(
      tx,
      registration.state_locator.policy_id,
      registration.state_locator.asset_name,
    )

  // 3. Script must not already be registered
  let not_already_registered =
    !list.any(
      datum.registrations,
      fn(r) { r.script_hash == registration.script_hash },
    )

  // 4. Registration must be valid
  let valid_registration = validate_registration(registration)

  // 5. Build new datum with registration added
  let new_registrations = [registration, ..datum.registrations]
  let new_datum = RegistryDatum { ..datum, registrations: new_registrations }

  // 6. Validate continuation
  let valid_continuation =
    validate_registry_continuation(datum, new_datum, tx, own_ref)

  // All checks must pass
  owner_signed && state_nft_present && not_already_registered && valid_registration && valid_continuation
}

/// Validate updating a registration
///
/// Security: ONLY the current owner can update. Spending the script is NOT enough.
/// This prevents attackers from modifying registrations during legitimate script use.
fn validate_update(
  datum: RegistryDatum,
  script_hash: ScriptHash,
  new_state_locator: Option<UtxoLocator>,
  new_reference_script_locator: Option<Option<UtxoLocator>>,
  new_additional_inputs: Option<List<AdditionalInput>>,
  new_recipient_type: Option<RecipientType>,
  new_custom_ism: Option<Option<ScriptHash>>,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // 1. Find existing registration
  expect Some(existing) =
    list.find(datum.registrations, fn(r) { r.script_hash == script_hash })

  // 2. Current owner must sign - THIS IS THE KEY SECURITY CHECK
  // Even if the script is being spent, only the owner can modify the registration
  let owner_signed = is_signed_by(tx, existing.owner)

  // 3. Build updated registration with optional field updates
  let updated =
    RecipientRegistration {
      script_hash: existing.script_hash,
      // Cannot change script_hash
      owner: existing.owner,
      // Use TransferOwnership to change owner
      state_locator: when new_state_locator is {
        Some(loc) -> loc
        None -> existing.state_locator
      },
      reference_script_locator: when new_reference_script_locator is {
        Some(loc) -> loc
        None -> existing.reference_script_locator
      },
      additional_inputs: when new_additional_inputs is {
        Some(inputs) -> inputs
        None -> existing.additional_inputs
      },
      recipient_type: when new_recipient_type is {
        Some(rt) -> rt
        None -> existing.recipient_type
      },
      custom_ism: when new_custom_ism is {
        Some(ism) -> ism
        None -> existing.custom_ism
      },
    }

  // 4. Validate updated registration
  let valid_registration = validate_registration(updated)

  // 5. Build new datum with updated registration
  let new_registrations =
    list.map(
      datum.registrations,
      fn(r) {
        if r.script_hash == script_hash {
          updated
        } else {
          r
        }
      },
    )

  let new_datum = RegistryDatum { ..datum, registrations: new_registrations }

  // 6. Validate continuation
  let valid_continuation =
    validate_registry_continuation(datum, new_datum, tx, own_ref)

  owner_signed && valid_registration && valid_continuation
}

/// Validate unregistering a recipient
///
/// Security: ONLY the current owner can unregister.
fn validate_unregister(
  datum: RegistryDatum,
  script_hash: ScriptHash,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // 1. Find existing registration
  expect Some(existing) =
    list.find(datum.registrations, fn(r) { r.script_hash == script_hash })

  // 2. Current owner must sign
  let owner_signed = is_signed_by(tx, existing.owner)

  // 3. Remove from registrations list
  let new_registrations =
    list.filter(datum.registrations, fn(r) { r.script_hash != script_hash })

  let new_datum = RegistryDatum { ..datum, registrations: new_registrations }

  // 4. Validate continuation
  let valid_continuation =
    validate_registry_continuation(datum, new_datum, tx, own_ref)

  owner_signed && valid_continuation
}

/// Validate ownership transfer
///
/// Security: BOTH current owner AND new owner must sign.
/// - Current owner authorizes the transfer
/// - New owner accepts ownership (prevents unwanted transfers)
fn validate_transfer_ownership(
  datum: RegistryDatum,
  script_hash: ScriptHash,
  new_owner: VerificationKeyHash,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // 1. Find existing registration
  expect Some(existing) =
    list.find(datum.registrations, fn(r) { r.script_hash == script_hash })

  // 2. Current owner must sign (authorizes transfer)
  let current_owner_signed = is_signed_by(tx, existing.owner)

  // 3. New owner must also sign (accepts ownership)
  let new_owner_signed = is_signed_by(tx, new_owner)

  // 4. Update registration with new owner
  let updated = RecipientRegistration { ..existing, owner: new_owner }

  let new_registrations =
    list.map(
      datum.registrations,
      fn(r) {
        if r.script_hash == script_hash {
          updated
        } else {
          r
        }
      },
    )

  let new_datum = RegistryDatum { ..datum, registrations: new_registrations }

  // 5. Validate continuation
  let valid_continuation =
    validate_registry_continuation(datum, new_datum, tx, own_ref)

  current_owner_signed && new_owner_signed && valid_continuation
}

/// Validate admin registration (owner can register any script)
///
/// The registry admin can register scripts without proving ownership.
/// This is useful for:
/// - Registering legacy scripts
/// - Emergency registrations
/// - Initial setup
fn validate_admin_register(
  datum: RegistryDatum,
  registration: RecipientRegistration,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // Only admin can use admin registration
  let admin_signed = is_signed_by(tx, datum.admin)

  // Registration must be valid
  let valid_registration = validate_registration(registration)

  // Script must not already be registered
  let not_already_registered =
    !list.any(
      datum.registrations,
      fn(r) { r.script_hash == registration.script_hash },
    )

  // Build new registrations list
  let new_registrations = [registration, ..datum.registrations]
  let new_datum = RegistryDatum { ..datum, registrations: new_registrations }

  let valid_continuation =
    validate_registry_continuation(datum, new_datum, tx, own_ref)

  admin_signed && valid_registration && not_already_registered && valid_continuation
}

/// Validate that a registration is well-formed
fn validate_registration(reg: RecipientRegistration) -> Bool {
  // State locator must have a valid policy ID (28 bytes)
  let valid_policy = bytearray.length(reg.state_locator.policy_id) == 28

  // Script hash must be valid (28 bytes)
  let valid_script = bytearray.length(reg.script_hash) == 28

  valid_policy && valid_script
}

/// Validate registry continuation (datum updated correctly, value preserved)
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

  // Find continuation output (same address)
  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  // Verify datum updated correctly
  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: RegistryDatum = cont_datum_data
  let datum_correct = cont_datum == new_datum

  // Value must be preserved (at least same lovelace)
  let value_preserved = lovelace_of(continuation.value) >= lovelace_of(own_value)

  datum_correct && value_preserved
}

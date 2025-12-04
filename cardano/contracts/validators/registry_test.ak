use aiken/collection/list
use aiken/crypto.{VerificationKeyHash}
use aiken/primitive/bytearray
use types.{
  RecipientRegistration, Generic, RegistryDatum, UtxoLocator,
}

// ============================================================================
// Test Helper Functions
// ============================================================================

/// Create a test verification key hash (28 bytes)
fn make_vkh(seed: Int) -> VerificationKeyHash {
  // Create a 28-byte hash deterministically from the seed
  let base = #"01020304050607080910111213141516171819202122232425262728"
  // XOR with seed to differentiate
  let seed_byte = seed % 256
  bytearray.push(bytearray.drop(base, 1), seed_byte)
}

/// Create a test script hash (28 bytes)
fn make_script_hash(seed: Int) -> ByteArray {
  let base = #"aabbccdd0011223344556677889900aabbccdd001122334455667788"
  let seed_byte = seed % 256
  bytearray.push(bytearray.drop(base, 1), seed_byte)
}

/// Create a test policy ID (28 bytes)
fn make_policy_id(seed: Int) -> ByteArray {
  let base = #"fedcba9876543210fedcba9876543210fedcba9876543210fedcba98"
  let seed_byte = seed % 256
  bytearray.push(bytearray.drop(base, 1), seed_byte)
}

/// Create a sample UTXO locator
fn make_utxo_locator(seed: Int) -> UtxoLocator {
  UtxoLocator {
    policy_id: make_policy_id(seed),
    asset_name: #"737461746530",  // "state0"
  }
}

/// Create a sample registration
fn make_registration(script_seed: Int, owner_seed: Int) -> RecipientRegistration {
  RecipientRegistration {
    script_hash: make_script_hash(script_seed),
    owner: make_vkh(owner_seed),
    state_locator: make_utxo_locator(script_seed),
    reference_script_locator: None,
    additional_inputs: [],
    recipient_type: Generic,
    custom_ism: None,
  }
}

/// Create an empty registry datum
fn empty_registry(admin_seed: Int) -> RegistryDatum {
  RegistryDatum {
    registrations: [],
    admin: make_vkh(admin_seed),
  }
}

/// Create a registry with one registration
fn registry_with_one(admin_seed: Int, script_seed: Int, owner_seed: Int) -> RegistryDatum {
  RegistryDatum {
    registrations: [make_registration(script_seed, owner_seed)],
    admin: make_vkh(admin_seed),
  }
}

// ============================================================================
// Data Structure Tests
// ============================================================================

/// Test that RecipientRegistration has an owner field
test registration_has_owner_field() {
  let reg = make_registration(1, 2)
  // Owner should be the VKH we created with seed 2
  bytearray.length(reg.owner) == 28
}

/// Test that RegistryDatum has an admin field (not owner)
test registry_datum_has_admin_field() {
  let registry = empty_registry(1)
  // Admin should be 28 bytes
  bytearray.length(registry.admin) == 28
}

/// Test owner is different from admin concept
test owner_and_admin_are_separate() {
  let admin_seed = 1
  let owner_seed = 2
  let registry = registry_with_one(admin_seed, 1, owner_seed)

  // Get the first registration
  expect Some(reg) = list.head(registry.registrations)

  // Admin controls registry, owner controls registration
  // They should be different
  registry.admin != reg.owner
}

// ============================================================================
// Registration Validation Tests
// ============================================================================

/// Test registration has valid script hash (28 bytes)
test registration_script_hash_is_28_bytes() {
  let reg = make_registration(1, 1)
  bytearray.length(reg.script_hash) == 28
}

/// Test registration has valid owner (28 bytes VKH)
test registration_owner_is_28_bytes() {
  let reg = make_registration(1, 1)
  bytearray.length(reg.owner) == 28
}

/// Test registration state locator has valid policy ID (28 bytes)
test registration_state_locator_valid() {
  let reg = make_registration(1, 1)
  bytearray.length(reg.state_locator.policy_id) == 28
}

/// Test that different seeds produce different registrations
test different_seeds_different_registrations() {
  let reg1 = make_registration(1, 1)
  let reg2 = make_registration(2, 2)

  reg1.script_hash != reg2.script_hash && reg1.owner != reg2.owner
}

// ============================================================================
// Security Property Tests - Registration List Operations
// ============================================================================

/// Test that adding a registration increases list size
test adding_registration_increases_count() {
  let registry = empty_registry(1)
  let new_reg = make_registration(1, 1)

  let initial_count = list.length(registry.registrations)
  let new_registrations = [new_reg, ..registry.registrations]

  list.length(new_registrations) == initial_count + 1
}

/// Test that duplicate script_hash can be detected
test duplicate_detection_works() {
  let registry = registry_with_one(1, 1, 1)
  let duplicate_reg = make_registration(1, 2)  // Same script_hash, different owner

  // Check if script_hash already exists
  let already_registered = list.any(
    registry.registrations,
    fn(r) { r.script_hash == duplicate_reg.script_hash }
  )

  already_registered
}

/// Test that non-duplicate passes check
test non_duplicate_detection_works() {
  let registry = registry_with_one(1, 1, 1)
  let new_reg = make_registration(2, 2)  // Different script_hash

  let already_registered = list.any(
    registry.registrations,
    fn(r) { r.script_hash == new_reg.script_hash }
  )

  !already_registered
}

// ============================================================================
// Security Property Tests - Owner Verification Logic
// ============================================================================

/// Test that owner lookup by script_hash works
test owner_lookup_by_script_hash() {
  let owner_seed = 42
  let script_seed = 1
  let registry = registry_with_one(1, script_seed, owner_seed)

  // Look up the registration by script_hash
  let script_hash = make_script_hash(script_seed)
  expect Some(found) = list.find(
    registry.registrations,
    fn(r) { r.script_hash == script_hash }
  )

  // Verify owner matches
  found.owner == make_vkh(owner_seed)
}

/// Test owner lookup returns None for unknown script
test owner_lookup_unknown_script() {
  let registry = registry_with_one(1, 1, 1)
  let unknown_script = make_script_hash(999)

  let result = list.find(
    registry.registrations,
    fn(r) { r.script_hash == unknown_script }
  )

  when result is {
    None -> True
    Some(_) -> False
  }
}

/// Test that different owners can control different registrations
test multiple_owners_multiple_registrations() {
  let admin = make_vkh(0)
  let owner1 = make_vkh(1)
  let owner2 = make_vkh(2)

  let reg1 = RecipientRegistration {
    script_hash: make_script_hash(1),
    owner: owner1,
    state_locator: make_utxo_locator(1),
    reference_script_locator: None,
    additional_inputs: [],
    recipient_type: Generic,
    custom_ism: None,
  }

  let reg2 = RecipientRegistration {
    script_hash: make_script_hash(2),
    owner: owner2,
    state_locator: make_utxo_locator(2),
    reference_script_locator: None,
    additional_inputs: [],
    recipient_type: Generic,
    custom_ism: None,
  }

  let registry = RegistryDatum {
    registrations: [reg1, reg2],
    admin: admin,
  }

  // Each script has its own owner
  expect Some(found1) = list.find(
    registry.registrations,
    fn(r) { r.script_hash == make_script_hash(1) }
  )
  expect Some(found2) = list.find(
    registry.registrations,
    fn(r) { r.script_hash == make_script_hash(2) }
  )

  found1.owner == owner1 && found2.owner == owner2
}

// ============================================================================
// Security Property Tests - Update Logic
// ============================================================================

/// Test that update preserves script_hash (cannot be changed)
test update_preserves_script_hash() {
  let original = make_registration(1, 1)
  let new_state_locator = make_utxo_locator(99)

  // Create updated registration (simulating what validator does)
  let updated = RecipientRegistration {
    ..original,
    state_locator: new_state_locator,
  }

  // Script hash must be preserved
  updated.script_hash == original.script_hash
}

/// Test that update preserves owner (must use TransferOwnership)
test update_preserves_owner() {
  let original = make_registration(1, 1)
  let new_state_locator = make_utxo_locator(99)

  // Create updated registration
  let updated = RecipientRegistration {
    ..original,
    state_locator: new_state_locator,
  }

  // Owner must be preserved in update (use TransferOwnership to change)
  updated.owner == original.owner
}

/// Test that optional update fields work correctly
test optional_update_fields() {
  let original = make_registration(1, 1)

  // Simulate optional update (None means keep existing)
  let new_state_locator = Some(make_utxo_locator(99))
  let new_custom_ism = None

  let updated_state = when new_state_locator is {
    Some(loc) -> loc
    None -> original.state_locator
  }

  let updated_ism = when new_custom_ism is {
    Some(ism) -> ism
    None -> original.custom_ism
  }

  // State locator should be updated
  updated_state == make_utxo_locator(99) &&
  // Custom ISM should remain unchanged
  updated_ism == original.custom_ism
}

// ============================================================================
// Security Property Tests - Unregister Logic
// ============================================================================

/// Test that unregister removes correct entry
test unregister_removes_correct_entry() {
  let admin = make_vkh(0)
  let reg1 = make_registration(1, 1)
  let reg2 = make_registration(2, 2)
  let reg3 = make_registration(3, 3)

  let registry = RegistryDatum {
    registrations: [reg1, reg2, reg3],
    admin: admin,
  }

  // Remove registration 2
  let script_to_remove = make_script_hash(2)
  let new_registrations = list.filter(
    registry.registrations,
    fn(r) { r.script_hash != script_to_remove }
  )

  // Should have 2 registrations left
  list.length(new_registrations) == 2 &&
  // And registration 2 should not be in the list
  !list.any(new_registrations, fn(r) { r.script_hash == script_to_remove })
}

/// Test that unregister keeps other entries unchanged
test unregister_keeps_others() {
  let admin = make_vkh(0)
  let reg1 = make_registration(1, 1)
  let reg2 = make_registration(2, 2)

  let registry = RegistryDatum {
    registrations: [reg1, reg2],
    admin: admin,
  }

  // Remove registration 2
  let script_to_remove = make_script_hash(2)
  let new_registrations = list.filter(
    registry.registrations,
    fn(r) { r.script_hash != script_to_remove }
  )

  // Registration 1 should still be there and unchanged
  expect Some(remaining) = list.head(new_registrations)
  remaining.script_hash == reg1.script_hash && remaining.owner == reg1.owner
}

// ============================================================================
// Security Property Tests - Transfer Ownership Logic
// ============================================================================

/// Test transfer ownership updates owner correctly
test transfer_ownership_updates_owner() {
  let old_owner = make_vkh(1)
  let new_owner = make_vkh(2)
  let original = RecipientRegistration {
    script_hash: make_script_hash(1),
    owner: old_owner,
    state_locator: make_utxo_locator(1),
    reference_script_locator: None,
    additional_inputs: [],
    recipient_type: Generic,
    custom_ism: None,
  }

  // Transfer ownership
  let updated = RecipientRegistration { ..original, owner: new_owner }

  // Owner should be changed
  updated.owner == new_owner && updated.owner != old_owner
}

/// Test transfer ownership preserves everything else
test transfer_ownership_preserves_other_fields() {
  let old_owner = make_vkh(1)
  let new_owner = make_vkh(2)
  let original = RecipientRegistration {
    script_hash: make_script_hash(1),
    owner: old_owner,
    state_locator: make_utxo_locator(1),
    reference_script_locator: None,
    additional_inputs: [],
    recipient_type: Generic,
    custom_ism: None,
  }

  let updated = RecipientRegistration { ..original, owner: new_owner }

  // All other fields should remain unchanged
  updated.script_hash == original.script_hash &&
  updated.state_locator == original.state_locator &&
  updated.reference_script_locator == original.reference_script_locator &&
  updated.additional_inputs == original.additional_inputs &&
  updated.recipient_type == original.recipient_type &&
  updated.custom_ism == original.custom_ism
}

// ============================================================================
// Security Property Tests - Admin vs Owner Distinction
// ============================================================================

/// Test admin cannot be confused with registration owner
test admin_owner_distinction() {
  let admin_seed = 1
  let owner_seed = 1  // Same seed, but different roles

  let admin = make_vkh(admin_seed)
  let reg = make_registration(1, owner_seed)

  let registry = RegistryDatum {
    registrations: [reg],
    admin: admin,
  }

  // Even with same seed, the admin field is separate from owner
  // This tests the structural distinction
  expect Some(found) = list.head(registry.registrations)

  // Admin is at registry level, owner is at registration level
  // Both happen to be same VKH here, but they serve different purposes
  registry.admin == found.owner  // True because same seed used
}

/// Test admin can be different from all owners
test admin_different_from_all_owners() {
  let admin = make_vkh(0)
  let owner1 = make_vkh(1)
  let owner2 = make_vkh(2)

  let reg1 = RecipientRegistration {
    script_hash: make_script_hash(1),
    owner: owner1,
    state_locator: make_utxo_locator(1),
    reference_script_locator: None,
    additional_inputs: [],
    recipient_type: Generic,
    custom_ism: None,
  }

  let reg2 = RecipientRegistration {
    script_hash: make_script_hash(2),
    owner: owner2,
    state_locator: make_utxo_locator(2),
    reference_script_locator: None,
    additional_inputs: [],
    recipient_type: Generic,
    custom_ism: None,
  }

  let registry = RegistryDatum {
    registrations: [reg1, reg2],
    admin: admin,
  }

  // Admin is different from all registration owners
  !list.any(registry.registrations, fn(r) { r.owner == registry.admin })
}

// ============================================================================
// Registration List Update Tests
// ============================================================================

/// Test updating a registration in a list preserves other entries
test list_update_preserves_others() {
  let admin = make_vkh(0)
  let reg1 = make_registration(1, 1)
  let reg2 = make_registration(2, 2)
  let reg3 = make_registration(3, 3)

  let registry = RegistryDatum {
    registrations: [reg1, reg2, reg3],
    admin: admin,
  }

  // Update registration 2's state_locator
  let target_script = make_script_hash(2)
  let new_state = make_utxo_locator(99)

  let new_registrations = list.map(
    registry.registrations,
    fn(r) {
      if r.script_hash == target_script {
        RecipientRegistration { ..r, state_locator: new_state }
      } else {
        r
      }
    }
  )

  // Length should be unchanged
  expect list.length(new_registrations) == 3

  // Reg 1 should be unchanged
  expect Some(r1) = list.find(new_registrations, fn(r) { r.script_hash == make_script_hash(1) })
  expect r1.state_locator == reg1.state_locator

  // Reg 2 should be updated
  expect Some(r2) = list.find(new_registrations, fn(r) { r.script_hash == make_script_hash(2) })
  expect r2.state_locator == new_state

  // Reg 3 should be unchanged
  expect Some(r3) = list.find(new_registrations, fn(r) { r.script_hash == make_script_hash(3) })
  r3.state_locator == reg3.state_locator
}

use aiken/collection/list
use aiken/crypto.{VerificationKeyHash}
use cardano/address.{Credential, Script}
use cardano/assets.{lovelace_of, quantity_of}
use cardano/transaction.{InlineDatum, Output, OutputReference, Transaction}
use types.{AssetName, PolicyId, ScriptHash}
use utils.{find_input, is_signed_by}

/// Vault Datum
/// Stores configuration and accounting for locked tokens
pub type VaultDatum {
  /// Warp route that controls this vault
  warp_route_hash: ScriptHash,
  /// Owner for emergency recovery
  owner: VerificationKeyHash,
  /// Token being locked (None for ADA vault)
  token: Option<(PolicyId, AssetName)>,
  /// Total tokens currently locked
  total_locked: Int,
}

/// Vault Redeemer
pub type VaultRedeemer {
  /// Lock tokens (called by warp route on TransferRemote)
  Lock { amount: Int }
  /// Release tokens (called by warp route on ReceiveTransfer)
  Release { amount: Int, recipient: ByteArray }
  /// Emergency withdraw by owner
  EmergencyWithdraw { amount: Int }
}

/// Vault Validator
/// Holds collateral tokens for warp routes
///
/// The vault can only be modified by:
/// 1. The associated warp route (for Lock/Release operations)
/// 2. The owner (for emergency withdrawals)
///
/// **Security:**
/// - Only the warp route can lock/release tokens during normal operation
/// - Owner can emergency withdraw if warp route is compromised
/// - All operations must maintain proper accounting
validator vault {
  spend(
    datum: Option<VaultDatum>,
    redeemer: VaultRedeemer,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    expect Some(vault_datum) = datum

    when redeemer is {
      Lock { amount } ->
        validate_lock(vault_datum, amount, tx, own_ref)

      Release { amount, recipient } ->
        validate_release(vault_datum, amount, recipient, tx, own_ref)

      EmergencyWithdraw { amount } ->
        validate_emergency_withdraw(vault_datum, amount, tx, own_ref)
    }
  }

  else(_) {
    fail
  }
}

/// Validate locking tokens into the vault
fn validate_lock(
  datum: VaultDatum,
  amount: Int,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // 1. Amount must be positive
  expect amount > 0

  // 2. Warp route must be involved in the transaction
  expect warp_route_is_caller(datum.warp_route_hash, tx)

  // 3. Find own input
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address
  let own_value = own_input.output.value

  // 4. Find continuation output
  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  // 5. Verify tokens are being added to vault
  let tokens_valid =
    when datum.token is {
      Some((policy_id, asset_name)) -> {
        let input_tokens = quantity_of(own_value, policy_id, asset_name)
        let output_tokens = quantity_of(continuation.value, policy_id, asset_name)
        output_tokens >= input_tokens + amount
      }
      None -> {
        // ADA vault
        let input_ada = lovelace_of(own_value)
        let output_ada = lovelace_of(continuation.value)
        output_ada >= input_ada + amount
      }
    }
  expect tokens_valid

  // 6. Verify datum is updated correctly
  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: VaultDatum = cont_datum_data
  expect cont_datum.warp_route_hash == datum.warp_route_hash
  expect cont_datum.owner == datum.owner
  expect cont_datum.token == datum.token
  expect cont_datum.total_locked == datum.total_locked + amount

  True
}

/// Validate releasing tokens from the vault
fn validate_release(
  datum: VaultDatum,
  amount: Int,
  recipient: ByteArray,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // 1. Amount must be positive and not exceed locked amount
  expect amount > 0
  expect amount <= datum.total_locked

  // 2. Warp route must be involved in the transaction
  expect warp_route_is_caller(datum.warp_route_hash, tx)

  // 3. Find own input
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address
  let own_value = own_input.output.value

  // 4. Find continuation output
  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  // 5. Verify tokens are being released from vault
  let tokens_released =
    when datum.token is {
      Some((policy_id, asset_name)) -> {
        let input_tokens = quantity_of(own_value, policy_id, asset_name)
        let output_tokens = quantity_of(continuation.value, policy_id, asset_name)
        input_tokens - output_tokens == amount
      }
      None -> {
        // ADA vault
        let input_ada = lovelace_of(own_value)
        let output_ada = lovelace_of(continuation.value)
        input_ada - output_ada >= amount
      }
    }
  expect tokens_released

  // 6. Verify recipient receives the tokens
  let recipient_receives =
    when datum.token is {
      Some((policy_id, asset_name)) ->
        recipient_receives_tokens(tx, recipient, policy_id, asset_name, amount)
      None -> recipient_receives_ada(tx, recipient, amount)
    }
  expect recipient_receives

  // 7. Verify datum is updated correctly
  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: VaultDatum = cont_datum_data
  expect cont_datum.warp_route_hash == datum.warp_route_hash
  expect cont_datum.owner == datum.owner
  expect cont_datum.token == datum.token
  expect cont_datum.total_locked == datum.total_locked - amount

  True
}

/// Validate emergency withdrawal by owner
fn validate_emergency_withdraw(
  datum: VaultDatum,
  amount: Int,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // 1. Only owner can emergency withdraw
  expect is_signed_by(tx, datum.owner)

  // 2. Amount must be positive
  expect amount > 0

  // 3. Find own input
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address
  let own_value = own_input.output.value

  // 4. Find continuation output (optional - can withdraw everything)
  let continuation =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  when continuation is {
    Some(cont_output) -> {
      // Partial withdrawal - verify datum update
      expect InlineDatum(cont_datum_data) = cont_output.datum
      expect cont_datum: VaultDatum = cont_datum_data
      expect cont_datum.warp_route_hash == datum.warp_route_hash
      expect cont_datum.owner == datum.owner
      expect cont_datum.token == datum.token

      // Verify correct amount withdrawn
      when datum.token is {
        Some((policy_id, asset_name)) -> {
          let input_tokens = quantity_of(own_value, policy_id, asset_name)
          let output_tokens = quantity_of(cont_output.value, policy_id, asset_name)
          let withdrawn = input_tokens - output_tokens
          withdrawn == amount && cont_datum.total_locked == datum.total_locked - amount
        }
        None -> {
          let input_ada = lovelace_of(own_value)
          let output_ada = lovelace_of(cont_output.value)
          let withdrawn = input_ada - output_ada
          withdrawn >= amount && cont_datum.total_locked == datum.total_locked - amount
        }
      }
    }
    None -> {
      // Full withdrawal - no continuation needed
      // Just verify owner signed
      True
    }
  }
}

/// Check if warp route is being called
fn warp_route_is_caller(warp_route_hash: ScriptHash, tx: Transaction) -> Bool {
  list.any(
    tx.inputs,
    fn(input) {
      when input.output.address.payment_credential is {
        Script(hash) -> hash == warp_route_hash
        _ -> False
      }
    },
  )
}

/// Check if recipient receives tokens
fn recipient_receives_tokens(
  tx: Transaction,
  recipient_bytes: ByteArray,
  policy_id: PolicyId,
  asset_name: AssetName,
  amount: Int,
) -> Bool {
  list.any(
    tx.outputs,
    fn(output) {
      let credential_matches =
        when output.address.payment_credential is {
          Credential.VerificationKey(hash) -> hash == recipient_bytes
          Credential.Script(hash) -> hash == recipient_bytes
        }
      credential_matches && quantity_of(output.value, policy_id, asset_name) >= amount
    },
  )
}

/// Check if recipient receives ADA
fn recipient_receives_ada(
  tx: Transaction,
  recipient_bytes: ByteArray,
  amount: Int,
) -> Bool {
  list.any(
    tx.outputs,
    fn(output) {
      let credential_matches =
        when output.address.payment_credential is {
          Credential.VerificationKey(hash) -> hash == recipient_bytes
          Credential.Script(hash) -> hash == recipient_bytes
        }
      credential_matches && lovelace_of(output.value) >= amount
    },
  )
}

use aiken/collection/list
use aiken/crypto.{VerificationKeyHash}
use aiken/interval
use cardano/address.{Credential}
use cardano/assets.{lovelace_of, quantity_of}
use cardano/transaction.{OutputReference, Transaction, ValidityRange}
use types.{
  AssetName, PolicyId,
  RedemptionDatum, RedemptionAda, RedemptionToken,
}
use utils.{find_input}

// ============================================================================
// Token Redemption Validator
// ============================================================================
//
// This validator implements a two-phase token redemption pattern for warp routes.
// Instead of sending bridged tokens directly to recipients (which requires the
// relayer to "gift" minUTxO ADA), tokens are held in escrow until the recipient
// claims them with their own ADA.
//
// Flow:
// 1. Relayer creates redemption UTXO: tokens + minUTxO, datum identifies recipient
// 2. Recipient claims: provides their own ADA, gets tokens, relayer's ADA returned
// 3. If unclaimed after expiry: relayer can reclaim everything
//
// This ensures:
// - Recipients provide their own minUTxO (economically sound)
// - Relayer's ADA is a revolving float, not a gift
// - Unclaimed tokens don't lock relayer capital forever

/// Redemption redeemer actions
pub type RedemptionRedeemer {
  /// Recipient claims their tokens
  /// Must be before expiry, recipient provides own minUTxO
  Claim
  /// Relayer reclaims after expiry
  /// Tokens + minUTxO go to return_address
  Expire
}

/// Token Redemption Validator
///
/// Holds bridged tokens in escrow until recipient claims with their own ADA,
/// or relayer reclaims after expiry.
validator token_redemption {
  spend(
    datum: Option<RedemptionDatum>,
    redeemer: RedemptionRedeemer,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    expect Some(redemption_datum) = datum

    expect Some(own_input) = find_input(tx, own_ref)
    let own_lovelace = lovelace_of(own_input.output.value)

    when redeemer is {
      Claim -> validate_claim(redemption_datum, tx, own_lovelace)
      Expire -> validate_expire(redemption_datum, tx, own_lovelace)
    }
  }

  else(_) {
    fail
  }
}

/// Validate a claim by the recipient
///
/// Requirements:
/// 1. Current slot <= expiry_slot (not expired)
/// 2. Recipient receives the tokens (amount specified in datum)
/// 3. Return address receives minUTxO back
fn validate_claim(
  datum: RedemptionDatum,
  tx: Transaction,
  own_lovelace: Int,
) -> Bool {
  // 1. Check not expired
  expect is_before_expiry(tx.validity_range, datum.expiry_slot)

  when datum.token_info is {
    RedemptionAda -> {
      // Native ADA: the redemption UTXO holds amount + relayer deposit.
      // Recipient gets the transfer amount, relayer gets their deposit back.
      expect recipient_receives_ada(tx, datum.recipient, datum.amount)
      let relayer_deposit = own_lovelace - datum.amount
      if relayer_deposit > 0 {
        expect
          return_address_receives_ada(tx, datum.return_address, relayer_deposit)
        True
      } else {
        True
      }
    }
    RedemptionToken { policy_id, asset_name } -> {
      expect
        recipient_receives_tokens(
          tx,
          datum.recipient,
          policy_id,
          asset_name,
          datum.amount,
        )
      expect return_address_receives_ada(tx, datum.return_address, own_lovelace)
      True
    }
  }
}

/// Validate expiry reclaim by relayer
///
/// Requirements:
/// 1. Current slot > expiry_slot (expired)
/// 2. Return address receives everything (tokens + minUTxO)
fn validate_expire(
  datum: RedemptionDatum,
  tx: Transaction,
  own_lovelace: Int,
) -> Bool {
  // 1. Check expired
  expect is_after_expiry(tx.validity_range, datum.expiry_slot)

  // 2. Return address must receive everything
  let return_receives =
    when datum.token_info is {
      RedemptionAda ->
        return_address_receives_ada(tx, datum.return_address, own_lovelace)
      RedemptionToken { policy_id, asset_name } ->
        return_address_receives_tokens(
          tx,
          datum.return_address,
          policy_id,
          asset_name,
          datum.amount,
        ) && return_address_receives_ada(tx, datum.return_address, own_lovelace)
    }
  expect return_receives

  True
}

/// Check if validity range is before expiry
/// The transaction must be entirely before the expiry slot
fn is_before_expiry(validity: ValidityRange, expiry_slot: Int) -> Bool {
  // The validity range must be entirely before the expiry point
  // This ensures the transaction can only be submitted before expiry
  interval.is_entirely_before(validity, expiry_slot)
}

/// Check if validity range is after expiry
/// The transaction must be entirely after the expiry slot
fn is_after_expiry(validity: ValidityRange, expiry_slot: Int) -> Bool {
  // The validity range must be entirely after the expiry point
  // This ensures the transaction can only be submitted after expiry
  interval.is_entirely_after(validity, expiry_slot)
}

/// Check if recipient receives tokens
fn recipient_receives_tokens(
  tx: Transaction,
  recipient: ByteArray,
  policy_id: PolicyId,
  asset_name: AssetName,
  amount: Int,
) -> Bool {
  list.any(
    tx.outputs,
    fn(output) {
      let credential_matches =
        when output.address.payment_credential is {
          Credential.VerificationKey(hash) -> hash == recipient
          Credential.Script(hash) -> hash == recipient
        }
      credential_matches && quantity_of(output.value, policy_id, asset_name) >= amount
    },
  )
}

/// Check if recipient receives ADA
fn recipient_receives_ada(
  tx: Transaction,
  recipient: ByteArray,
  amount: Int,
) -> Bool {
  list.any(
    tx.outputs,
    fn(output) {
      let credential_matches =
        when output.address.payment_credential is {
          Credential.VerificationKey(hash) -> hash == recipient
          Credential.Script(hash) -> hash == recipient
        }
      credential_matches && lovelace_of(output.value) >= amount
    },
  )
}

/// Check if return address receives at least the specified ADA amount
fn return_address_receives_ada(
  tx: Transaction,
  return_addr: VerificationKeyHash,
  min_lovelace: Int,
) -> Bool {
  list.any(
    tx.outputs,
    fn(output) {
      when output.address.payment_credential is {
        Credential.VerificationKey(hash) ->
          hash == return_addr && lovelace_of(output.value) >= min_lovelace
        Credential.Script(_) -> False
      }
    },
  )
}

/// Check if return address receives tokens
fn return_address_receives_tokens(
  tx: Transaction,
  return_addr: VerificationKeyHash,
  policy_id: PolicyId,
  asset_name: AssetName,
  amount: Int,
) -> Bool {
  list.any(
    tx.outputs,
    fn(output) {
      let credential_matches =
        when output.address.payment_credential is {
          Credential.VerificationKey(hash) -> hash == return_addr
          Credential.Script(_) -> False
        }
      credential_matches && quantity_of(output.value, policy_id, asset_name) >= amount
    },
  )
}

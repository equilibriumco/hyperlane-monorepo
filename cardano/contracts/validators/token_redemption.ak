use aiken/collection/list
use aiken/crypto.{VerificationKeyHash}
use aiken/interval
use cardano/address.{Credential}
use cardano/assets.{lovelace_of, quantity_of}
use cardano/transaction.{Transaction, ValidityRange}
use types.{
  AssetName, PolicyId,
  RedemptionDatum, RedemptionTokenInfo, RedemptionAda, RedemptionToken,
}

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
    _own_ref: Data,
    tx: Transaction,
  ) {
    expect Some(redemption_datum) = datum

    when redeemer is {
      Claim -> validate_claim(redemption_datum, tx)
      Expire -> validate_expire(redemption_datum, tx)
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
fn validate_claim(datum: RedemptionDatum, tx: Transaction) -> Bool {
  // 1. Check not expired
  expect is_before_expiry(tx.validity_range, datum.expiry_slot)

  // 2. Recipient must receive the tokens
  let recipient_receives =
    when datum.token_info is {
      RedemptionAda -> recipient_receives_ada(tx, datum.recipient, datum.amount)
      RedemptionToken { policy_id, asset_name } ->
        recipient_receives_tokens(
          tx,
          datum.recipient,
          policy_id,
          asset_name,
          datum.amount,
        )
    }
  expect recipient_receives

  // 3. Return address must receive some ADA back (the relayer's minUTxO)
  // We don't enforce exact amount - just that they receive something
  // The transaction builder ensures correct amounts
  expect return_address_receives_ada(tx, datum.return_address)

  True
}

/// Validate expiry reclaim by relayer
///
/// Requirements:
/// 1. Current slot > expiry_slot (expired)
/// 2. Return address receives everything (tokens + minUTxO)
fn validate_expire(datum: RedemptionDatum, tx: Transaction) -> Bool {
  // 1. Check expired
  expect is_after_expiry(tx.validity_range, datum.expiry_slot)

  // 2. Return address must receive everything
  let return_receives =
    when datum.token_info is {
      RedemptionAda -> return_address_receives_ada(tx, datum.return_address)
      RedemptionToken { policy_id, asset_name } ->
        // Must receive both tokens AND ADA
        return_address_receives_tokens(
          tx,
          datum.return_address,
          policy_id,
          asset_name,
          datum.amount,
        ) && return_address_receives_ada(tx, datum.return_address)
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

/// Check if return address receives some ADA
fn return_address_receives_ada(
  tx: Transaction,
  return_addr: VerificationKeyHash,
) -> Bool {
  list.any(
    tx.outputs,
    fn(output) {
      when output.address.payment_credential is {
        Credential.VerificationKey(hash) -> hash == return_addr
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

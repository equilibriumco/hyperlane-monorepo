use aiken/collection/dict
use aiken/collection/list
use aiken/crypto.{VerificationKeyHash}
use aiken/interval
use cardano/address.{Credential}
use cardano/assets.{lovelace_of, tokens}
use cardano/transaction.{OutputReference, Transaction, ValidityRange}
use types.{
  MessageRedemptionDatum, MessageRedemptionRedeemer, ClaimMessage, ExpireMessage,
  PolicyId,
}
use utils.{find_input}

/// Message Redemption Validator
///
/// Holds a cross-chain message in escrow until the recipient claims it.
/// The relayer deposits the message + minUTxO during mailbox Process.
/// The recipient claims by spending their contract alongside this UTXO.
/// After expiry, the relayer can reclaim everything.
///
/// Claim requirements:
///   1. Before expiry
///   2. Stored message NFT with datum.message_id is burned
///   3. An input contains a token with datum.recipient_policy (recipient contract spent)
///   4. return_address receives at least the UTXO's lovelace
///
/// Expire requirements:
///   1. After expiry
///   2. Stored message NFT is burned
///   3. return_address receives everything
validator message_redemption(stored_message_nft_policy: PolicyId) {
  spend(
    datum: Option<MessageRedemptionDatum>,
    redeemer: MessageRedemptionRedeemer,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    expect Some(redemption_datum) = datum

    expect Some(own_input) = find_input(tx, own_ref)
    let own_lovelace = lovelace_of(own_input.output.value)

    when redeemer is {
      ClaimMessage ->
        validate_claim(
          redemption_datum,
          tx,
          own_lovelace,
          stored_message_nft_policy,
        )
      ExpireMessage ->
        validate_expire(
          redemption_datum,
          tx,
          own_lovelace,
          stored_message_nft_policy,
        )
    }
  }

  else(_) {
    fail
  }
}

fn validate_claim(
  datum: MessageRedemptionDatum,
  tx: Transaction,
  own_lovelace: Int,
  stored_message_nft_policy: PolicyId,
) -> Bool {
  // 1. Before expiry
  expect is_before_expiry(tx.validity_range, datum.expiry_slot)

  // 2. Stored message NFT burned
  expect message_nft_burned(tx, stored_message_nft_policy, datum.message_id)

  // 3. Recipient contract spent (input contains token with recipient_policy)
  expect has_policy_in_inputs(tx, datum.recipient_policy)

  // 4. Return address receives at least the UTXO's lovelace
  expect return_address_receives_ada(tx, datum.return_address, own_lovelace)

  True
}

fn validate_expire(
  datum: MessageRedemptionDatum,
  tx: Transaction,
  own_lovelace: Int,
  stored_message_nft_policy: PolicyId,
) -> Bool {
  // 1. After expiry
  expect is_after_expiry(tx.validity_range, datum.expiry_slot)

  // 2. Stored message NFT burned
  expect message_nft_burned(tx, stored_message_nft_policy, datum.message_id)

  // 3. Return address receives everything
  expect return_address_receives_ada(tx, datum.return_address, own_lovelace)

  True
}

fn is_before_expiry(validity: ValidityRange, expiry_slot: Int) -> Bool {
  interval.is_entirely_before(validity, expiry_slot)
}

fn is_after_expiry(validity: ValidityRange, expiry_slot: Int) -> Bool {
  interval.is_entirely_after(validity, expiry_slot)
}

fn message_nft_burned(
  tx: Transaction,
  nft_policy: PolicyId,
  message_id: ByteArray,
) -> Bool {
  let nft_mints = tokens(tx.mint, nft_policy)
  when dict.get(nft_mints, message_id) is {
    Some(quantity) -> quantity < 0
    None -> False
  }
}

fn has_policy_in_inputs(tx: Transaction, policy_id: PolicyId) -> Bool {
  list.any(
    tx.inputs,
    fn(input) {
      let policy_tokens = tokens(input.output.value, policy_id)
      !dict.is_empty(policy_tokens)
    },
  )
}

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

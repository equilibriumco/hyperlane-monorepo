use aiken/collection/dict
use aiken/collection/list
use cardano/assets
use cardano/transaction.{OutputReference, Transaction}

/// Test token minting policy for warp route testing
/// One-shot minting tied to specific UTXO consumption
/// Allows minting any amount of test tokens
validator test_token(utxo_ref: OutputReference) {
  mint(_redeemer: Data, policy_id: ByteArray, tx: Transaction) {
    // Can only mint if specific UTXO is consumed (one-shot guarantee)
    let utxo_consumed =
      list.any(tx.inputs, fn(input) { input.output_reference == utxo_ref })

    // Check that we're only minting positive amounts under this policy
    let policy_assets = assets.tokens(tx.mint, policy_id)
    let all_positive =
      dict.foldl(policy_assets, True, fn(_name, qty, acc) { acc && qty > 0 })

    utxo_consumed && all_positive
  }

  else(_) {
    fail
  }
}

use aiken/collection/list
use cardano/address.{Script}
use cardano/transaction.{Transaction}

/// Synthetic token minting policy for warp routes
/// Can only mint/burn if the associated warp route validator is satisfied
/// Script hash type
type ScriptHash =
  ByteArray

/// Validator parameterized by the warp route script hash
validator synthetic_token(warp_route_hash: ScriptHash) {
  mint(_redeemer: Data, _policy_id: ByteArray, tx: Transaction) {
    // Can only mint/burn if warp route validator is being spent in the same tx
    let warp_route_involved =
      list.any(
        tx.inputs,
        fn(input) {
          when input.output.address.payment_credential is {
            Script(hash) -> hash == warp_route_hash
            _ -> False
          }
        },
      )

    warp_route_involved
  }

  else(_) {
    fail
  }
}

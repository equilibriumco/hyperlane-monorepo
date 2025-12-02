use aiken/collection/list
use cardano/assets
use cardano/transaction.{OutputReference, Transaction}

/// State NFT minting policy
/// One-shot minting tied to specific UTXO consumption
/// Used to create unique identifiers for protocol state UTXOs
/// Validator parameterized by the UTXO that must be consumed
///
/// For the two-UTXO reference script pattern, this allows minting up to 2 NFTs:
/// - State NFT (empty asset name) for the state UTXO
/// - Reference Script NFT ("ref") for the reference script UTXO
validator state_nft(utxo_ref: OutputReference) {
  mint(_redeemer: Data, _policy_id: ByteArray, tx: Transaction) {
    // Can only mint if specific UTXO is consumed (one-shot)
    let utxo_consumed =
      list.any(tx.inputs, fn(input) { input.output_reference == utxo_ref })

    // Can mint 1-2 tokens (for two-UTXO pattern: state NFT + optional ref NFT)
    // Flatten the minted value to get all (policy_id, asset_name, quantity) tuples
    let minted_flat = assets.flatten(tx.mint)
    let total_minted =
      list.foldl(
        minted_flat,
        0,
        fn(item, acc) {
          let (_policy, _name, qty) = item
          acc + qty
        },
      )

    utxo_consumed && total_minted >= 1 && total_minted <= 2
  }

  else(_) {
    fail
  }
}

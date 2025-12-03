use aiken/collection/dict
use aiken/collection/list
use aiken/primitive/bytearray
use cardano/address.{Script}
use cardano/assets
use cardano/transaction.{Transaction}
use types.{ScriptHash}

/// Processed Message NFT minting policy
/// Allows minting one NFT per processed message for efficient off-chain tracking
///
/// The NFT is minted with:
/// - Policy ID: This minting policy's hash
/// - Asset name: The 32-byte message_id
///
/// Minting is allowed when:
/// - The mailbox script is being spent (in transaction inputs)
/// - Exactly one NFT is minted with a 32-byte asset name (the message_id)
///
/// This allows the relayer to efficiently query processed messages using
/// Blockfrost's asset API instead of scanning all UTXOs at an address.
validator processed_message_nft(mailbox_script_hash: ScriptHash) {
  mint(_redeemer: Data, own_policy: ByteArray, tx: Transaction) {
    // 1. Verify mailbox script is being spent
    let mailbox_spent =
      list.any(
        tx.inputs,
        fn(input) {
          when input.output.address.payment_credential is {
            Script(hash) -> hash == mailbox_script_hash
            _ -> False
          }
        },
      )

    // 2. Verify exactly one NFT is minted with this policy
    // and the asset name is 32 bytes (message_id length)
    let own_mints = assets.tokens(tx.mint, own_policy)
    let mint_count = list.length(dict.to_pairs(own_mints))

    // Check that we're minting exactly 1 token with 32-byte asset name
    let valid_mint =
      list.any(
        dict.to_pairs(own_mints),
        fn(pair) {
          let Pair(asset_name, quantity) = pair
          // Asset name should be 32 bytes (message_id) and quantity should be 1
          bytearray.length(asset_name) == 32 && quantity == 1
        },
      )

    mailbox_spent && mint_count == 1 && valid_mint
  }

  else(_) {
    fail
  }
}

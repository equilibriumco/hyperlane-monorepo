use aiken/collection/dict
use aiken/collection/list
use aiken/primitive/bytearray
use cardano/assets
use cardano/transaction.{Transaction}
use types.{PolicyId}

/// Processed Message NFT minting policy
/// Allows minting one NFT per processed message for efficient off-chain tracking
///
/// The NFT is minted with:
/// - Policy ID: This minting policy's hash
/// - Asset name: The 32-byte message_id
///
/// Minting is allowed when:
/// - The mailbox NFT (from mailbox_policy_id) is in transaction inputs
/// - Exactly one NFT is minted with a 32-byte asset name (the message_id)
///
/// This allows the relayer to efficiently query processed messages using
/// Blockfrost's asset API instead of scanning all UTXOs at an address.
///
/// ## Why parameterized by mailbox_policy_id (not mailbox_script_hash)?
///
/// Using mailbox_policy_id (the one-shot NFT policy) instead of the mailbox script hash
/// ensures this policy remains stable across mailbox upgrades. The mailbox_policy_id
/// is determined at initialization time and never changes, while the script hash
/// changes whenever the mailbox code is updated.
///
/// This stability is critical for replay protection - if the policy changed with
/// each mailbox upgrade, previously processed message markers would be at a different
/// policy, potentially allowing replay attacks.
///
/// Security: The mailbox NFT can only exist at the real mailbox UTXO (one-shot policy),
/// so requiring it in inputs guarantees the real mailbox validator runs.
validator processed_message_nft(mailbox_policy_id: PolicyId) {
  mint(_redeemer: Data, own_policy: ByteArray, tx: Transaction) {
    // 1. Verify mailbox NFT is in transaction inputs
    // The mailbox NFT can only exist at the real mailbox UTXO (one-shot minting),
    // so its presence guarantees the mailbox validator is running
    let mailbox_involved =
      list.any(
        tx.inputs,
        fn(input) {
          let policy_tokens = assets.tokens(input.output.value, mailbox_policy_id)
          !dict.is_empty(policy_tokens)
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

    mailbox_involved && mint_count == 1 && valid_mint
  }

  else(_) {
    fail
  }
}

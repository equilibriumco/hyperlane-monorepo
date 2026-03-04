use aiken/collection/dict
use aiken/collection/list
use aiken/collection/pairs
use aiken/primitive/bytearray
use cardano/assets
use cardano/transaction.{Spend, Transaction}
use types.{MailboxRedeemer, PolicyId, Process}

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
/// - The minted asset name matches the message_id from the mailbox Process redeemer
///   (defense-in-depth: the SMT in the mailbox is the primary replay guard)
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
    // 1. Find the mailbox input (holds the one-shot mailbox NFT)
    expect Some(mailbox_input) =
      list.find(
        tx.inputs,
        fn(input) {
          let policy_tokens = assets.tokens(input.output.value, mailbox_policy_id)
          !dict.is_empty(policy_tokens)
        },
      )

    // 2. Extract message_id from the mailbox's Process redeemer.
    //    Defense-in-depth: the SMT in the mailbox is the primary replay guard,
    //    but this check independently binds the NFT asset name to the processed
    //    message_id so the token alone proves which message was handled.
    expect Some(mailbox_redeemer_data) =
      pairs.get_first(tx.redeemers, Spend(mailbox_input.output_reference))
    expect mailbox_redeemer: MailboxRedeemer = mailbox_redeemer_data
    expect Process { message_id, .. } = mailbox_redeemer

    // 3. Verify exactly one NFT is minted whose 32-byte asset name equals
    //    the message_id from the mailbox redeemer
    let own_mints = assets.tokens(tx.mint, own_policy)
    expect [Pair(minted_asset_name, 1)] = dict.to_pairs(own_mints)
    expect bytearray.length(minted_asset_name) == 32

    minted_asset_name == message_id
  }

  else(_) {
    fail
  }
}

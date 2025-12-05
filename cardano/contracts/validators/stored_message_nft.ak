use aiken/collection/dict
use aiken/collection/list
use aiken/primitive/bytearray
use cardano/assets
use cardano/transaction.{Transaction}
use types.{MessageNftRedeemer, MintMessage, BurnMessage, PolicyId}

/// Stored Message NFT minting policy
///
/// This policy controls the minting of NFTs that prove a message is legitimate.
/// Only messages processed through the Mailbox can have their NFTs minted.
///
/// ## Purpose
///
/// For DeferredRecipient recipients, the relayer cannot build complex outputs.
/// Instead, it stores the message on-chain with an NFT proving legitimacy.
/// The message is then processed separately (manually, via dApp, automated service, etc.).
///
/// ## Security Model
///
/// - **Minting**: Only allowed when the mailbox NFT is in transaction inputs
///   This ensures only ISM-verified messages can have NFTs minted
/// - **Burning**: Always allowed (for cleanup when processing)
///
/// ## Asset Naming
///
/// The asset name is the 32-byte message_id, ensuring:
/// - Each message gets a unique NFT
/// - No collisions between messages
/// - Easy lookup by message_id
///
/// ## Parameters
///
/// - `mailbox_policy_id`: The NFT policy of the mailbox (NOT the script hash)
///   We check for this NFT to verify the real mailbox is involved
validator stored_message_nft(mailbox_policy_id: PolicyId) {
  mint(redeemer: MessageNftRedeemer, own_policy: ByteArray, tx: Transaction) {
    when redeemer is {
      MintMessage -> validate_mint(tx, own_policy, mailbox_policy_id)
      BurnMessage -> validate_burn(tx, own_policy)
    }
  }

  else(_) {
    fail
  }
}

/// Validate minting of a message NFT
///
/// Requirements:
/// 1. Mailbox NFT must be in transaction inputs (proves real mailbox is processing)
/// 2. Exactly one NFT minted with this policy
/// 3. Asset name must be 32 bytes (message_id)
/// 4. Quantity must be 1
fn validate_mint(
  tx: Transaction,
  own_policy: ByteArray,
  mailbox_policy_id: PolicyId,
) -> Bool {
  // 1. Verify mailbox is involved by checking for its NFT
  // This is more secure than checking script hash (see example_generic_recipient.ak comments)
  let mailbox_involved =
    list.any(
      tx.inputs,
      fn(input) {
        let policy_tokens = assets.tokens(input.output.value, mailbox_policy_id)
        !dict.is_empty(policy_tokens)
      },
    )

  // 2. Get our mints
  let own_mints = assets.tokens(tx.mint, own_policy)
  let mint_pairs = dict.to_pairs(own_mints)
  let mint_count = list.length(mint_pairs)

  // 3. Verify exactly one NFT minted with 32-byte asset name (message_id)
  let valid_mint =
    list.any(
      mint_pairs,
      fn(pair) {
        let Pair(asset_name, quantity) = pair
        // Asset name = message_id (32 bytes), quantity = 1
        bytearray.length(asset_name) == 32 && quantity == 1
      },
    )

  mailbox_involved && mint_count == 1 && valid_mint
}

/// Validate burning of a message NFT
///
/// Burning is always allowed - needed for cleanup when processing stored messages.
/// The recipient validator is responsible for ensuring proper processing.
fn validate_burn(tx: Transaction, own_policy: ByteArray) -> Bool {
  // Get our mints (burns are negative quantities)
  let own_mints = assets.tokens(tx.mint, own_policy)
  let mint_pairs = dict.to_pairs(own_mints)

  // All mints for this policy must be burns (negative quantities)
  // and asset names must be 32 bytes (message_ids)
  list.all(
    mint_pairs,
    fn(pair) {
      let Pair(asset_name, quantity) = pair
      bytearray.length(asset_name) == 32 && quantity < 0
    },
  )
}

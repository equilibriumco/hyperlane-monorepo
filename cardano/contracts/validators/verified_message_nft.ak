use aiken/collection/dict
use aiken/collection/list
use aiken/primitive/bytearray
use cardano/assets
use cardano/transaction.{Transaction}
use types.{MessageNftRedeemer, MintMessage, BurnMessage, PolicyId}

/// Verified Message NFT minting policy
///
/// This policy controls the minting of NFTs that prove a message is legitimate.
/// Only messages processed through the Mailbox can have their NFTs minted.
///
/// During Process, the mailbox mints a verified_message_nft and delivers it
/// to the recipient's script address inside a UTXO with VerifiedMessageDatum.
/// The recipient burns this NFT when handling the message.
validator verified_message_nft(mailbox_policy_id: PolicyId) {
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

fn validate_mint(
  tx: Transaction,
  own_policy: ByteArray,
  mailbox_policy_id: PolicyId,
) -> Bool {
  let mailbox_involved =
    list.any(
      tx.inputs,
      fn(input) {
        let policy_tokens = assets.tokens(input.output.value, mailbox_policy_id)
        !dict.is_empty(policy_tokens)
      },
    )

  let own_mints = assets.tokens(tx.mint, own_policy)
  let mint_pairs = dict.to_pairs(own_mints)
  let mint_count = list.length(mint_pairs)

  let valid_mint =
    list.any(
      mint_pairs,
      fn(pair) {
        let Pair(asset_name, quantity) = pair
        bytearray.length(asset_name) == 32 && quantity == 1
      },
    )

  mailbox_involved && mint_count == 1 && valid_mint
}

fn validate_burn(tx: Transaction, own_policy: ByteArray) -> Bool {
  let own_mints = assets.tokens(tx.mint, own_policy)
  let mint_pairs = dict.to_pairs(own_mints)

  list.all(
    mint_pairs,
    fn(pair) {
      let Pair(asset_name, quantity) = pair
      bytearray.length(asset_name) == 32 && quantity < 0
    },
  )
}

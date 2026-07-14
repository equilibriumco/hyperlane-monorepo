use aiken/collection/dict
use aiken/collection/list
use aiken/collection/pairs
use aiken/interval.{Interval, IntervalBound, NegativeInfinity, PositiveInfinity}
use aiken/primitive/bytearray
use cardano/address.{Address, Script}
use cardano/assets
use cardano/transaction.{
  Input, NoDatum, Output, OutputReference, Spend, Transaction,
}
use types.{
  BurnMessage, Dispatch, MailboxRedeemer, Message, MessageNftRedeemer,
  MintMessage, PolicyId, Process,
}

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
  expect Some(mailbox_input) =
    list.find(
      tx.inputs,
      fn(input) {
        assets.quantity_of(
          input.output.value,
          mailbox_policy_id,
          #"4d61696c626f78205374617465",
        ) == 1
      },
    )

  expect Some(raw_redeemer) =
    pairs.get_first(tx.redeemers, Spend(mailbox_input.output_reference))
  expect mailbox_redeemer: MailboxRedeemer = raw_redeemer
  expect Process { message, message_id, .. } = mailbox_redeemer
  // Verified-message NFTs are only the delivery mechanism for 0x02
  // script-hash recipients. Warp routes and payment credentials must not mint.
  expect bytearray.length(message.recipient) == 32
  expect bytearray.take(message.recipient, 4) == #"02000000"

  let own_mints = assets.tokens(tx.mint, own_policy)
  let mint_pairs = dict.to_pairs(own_mints)
  let mint_count = list.length(mint_pairs)

  let valid_mint =
    list.any(
      mint_pairs,
      fn(pair) {
        let Pair(asset_name, quantity) = pair
        bytearray.length(asset_name) == 32 && asset_name == message_id && quantity == 1
      },
    )

  mint_count == 1 && valid_mint
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

// ============================================================================
// Tests
// ============================================================================

const test_mailbox_policy =
  #"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

const test_message_nft_policy =
  #"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"

const test_message_id =
  #"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"

fn test_mailbox_ref() -> OutputReference {
  OutputReference {
    transaction_id: #"0000000000000000000000000000000000000000000000000000000000000001",
    output_index: 0,
  }
}

fn test_message() -> Message {
  test_message_for(
    #"0200000022222222222222222222222222222222222222222222222222222222",
  )
}

fn test_message_for(recipient: ByteArray) -> Message {
  Message {
    version: 3,
    nonce: 1,
    origin: 1,
    sender: #"1111111111111111111111111111111111111111111111111111111111111111",
    destination: 2,
    recipient,
    body: #"abcd",
  }
}

fn test_validity_range() -> Interval<Int> {
  Interval {
    lower_bound: IntervalBound {
      bound_type: NegativeInfinity,
      is_inclusive: False,
    },
    upper_bound: IntervalBound {
      bound_type: PositiveInfinity,
      is_inclusive: False,
    },
  }
}

fn test_tx(
  mailbox_redeemer: MailboxRedeemer,
  asset_name: ByteArray,
) -> Transaction {
  test_tx_with_mailbox_asset(
    mailbox_redeemer,
    asset_name,
    #"4d61696c626f78205374617465",
  )
}

fn test_tx_with_mailbox_asset(
  mailbox_redeemer: MailboxRedeemer,
  asset_name: ByteArray,
  mailbox_asset_name: ByteArray,
) -> Transaction {
  let raw_redeemer: Data = mailbox_redeemer
  Transaction {
    inputs: [
      Input {
        output_reference: test_mailbox_ref(),
        output: Output {
          address: Address {
            payment_credential: Script(
              #"dddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            ),
            stake_credential: None,
          },
          value: assets.from_asset(test_mailbox_policy, mailbox_asset_name, 1),
          datum: NoDatum,
          reference_script: None,
        },
      },
    ],
    reference_inputs: [],
    outputs: [],
    fee: 0,
    mint: assets.from_asset(test_message_nft_policy, asset_name, 1),
    certificates: [],
    withdrawals: [],
    validity_range: test_validity_range(),
    extra_signatories: [],
    redeemers: [Pair(Spend(test_mailbox_ref()), raw_redeemer)],
    datums: dict.empty,
    id: #"0000",
    votes: [],
    proposal_procedures: [],
    current_treasury_amount: None,
    treasury_donation: None,
  }
}

test mint_requires_matching_process_message_id() {
  let redeemer =
    Process {
      message: test_message(),
      metadata: #"01",
      message_id: test_message_id,
      smt_proof: [],
    }
  validate_mint(
    test_tx(redeemer, test_message_id),
    test_message_nft_policy,
    test_mailbox_policy,
  )
}

test mint_rejects_warp_recipient() fail {
  let message =
    test_message_for(
      #"0100000022222222222222222222222222222222222222222222222222222222",
    )
  let redeemer =
    Process {
      message,
      metadata: #"01",
      message_id: test_message_id,
      smt_proof: [],
    }
  validate_mint(
    test_tx(redeemer, test_message_id),
    test_message_nft_policy,
    test_mailbox_policy,
  )
}

test mint_rejects_payment_credential_recipient() fail {
  let message =
    test_message_for(
      #"0000000022222222222222222222222222222222222222222222222222222222",
    )
  let redeemer =
    Process {
      message,
      metadata: #"01",
      message_id: test_message_id,
      smt_proof: [],
    }
  validate_mint(
    test_tx(redeemer, test_message_id),
    test_message_nft_policy,
    test_mailbox_policy,
  )
}

test mint_rejects_unrelated_dispatch() fail {
  let redeemer =
    Dispatch {
      destination: 2,
      recipient: test_message().recipient,
      body: test_message().body,
      sender_ref: test_mailbox_ref(),
      hook_metadata: #"",
    }
  validate_mint(
    test_tx(redeemer, test_message_id),
    test_message_nft_policy,
    test_mailbox_policy,
  )
}

test mint_rejects_mailbox_reference_token() fail {
  let redeemer =
    Process {
      message: test_message(),
      metadata: #"01",
      message_id: test_message_id,
      smt_proof: [],
    }
  validate_mint(
    test_tx_with_mailbox_asset(redeemer, test_message_id, #"726566"),
    test_message_nft_policy,
    test_mailbox_policy,
  )
}

test mint_rejects_wrong_asset_name() {
  let redeemer =
    Process {
      message: test_message(),
      metadata: #"01",
      message_id: test_message_id,
      smt_proof: [],
    }
  !validate_mint(
    test_tx(
      redeemer,
      #"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
    ),
    test_message_nft_policy,
    test_mailbox_policy,
  )
}

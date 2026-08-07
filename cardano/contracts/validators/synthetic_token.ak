use aiken/collection/dict
use aiken/collection/list
use aiken/collection/pairs
use aiken/interval.{Interval, IntervalBound, NegativeInfinity, PositiveInfinity}
use cardano/address.{Address, Script}
use cardano/assets
use cardano/transaction.{
  Input, NoDatum, Output, OutputReference, Spend, Transaction,
}
use types.{
  EnrollRemoteRoute, Message, ReceiveTransfer, TransferRemote, WarpRouteRedeemer,
}

/// Synthetic token minting policy for warp routes
/// Can only mint/burn if the associated warp route validator is satisfied
/// Script hash type
type ScriptHash =
  ByteArray

/// Validator parameterized by the warp route code and unique state NFT policy.
validator synthetic_token(
  warp_route_hash: ScriptHash,
  warp_state_nft_policy: ByteArray,
) {
  mint(_redeemer: Data, policy_id: ByteArray, tx: Transaction) {
    validate_mint_or_burn(warp_route_hash, warp_state_nft_policy, policy_id, tx)
  }

  else(_) {
    fail
  }
}

fn validate_mint_or_burn(
  warp_route_hash: ScriptHash,
  warp_state_nft_policy: ByteArray,
  policy_id: ByteArray,
  tx: Transaction,
) -> Bool {
  // Exactly one asset name per tx under this policy. WHICH name is valid
  // is the warp route's decision: its mint validation pins the name from
  // its own datum, and the total-minted check leaves no room for others.
  let mint_pairs = dict.to_pairs(assets.tokens(tx.mint, policy_id))
  expect [Pair(_asset_name, quantity)] = mint_pairs
  expect quantity != 0

  // Bind the mint/burn direction to the warp action. The warp validator then
  // binds the exact quantity to ReceiveTransfer/TransferRemote respectively.
  list.any(
    tx.inputs,
    fn(input) {
      when input.output.address.payment_credential is {
        Script(hash) ->
          if hash == warp_route_hash && assets.quantity_of(
            input.output.value,
            warp_state_nft_policy,
            #"",
          ) == 1 {
            when
              pairs.get_first(tx.redeemers, Spend(input.output_reference))
            is {
              Some(raw_redeemer) -> {
                expect warp_redeemer: WarpRouteRedeemer = raw_redeemer
                when warp_redeemer is {
                  ReceiveTransfer { .. } -> quantity > 0
                  TransferRemote { .. } -> quantity < 0
                  _ -> False
                }
              }
              None -> False
            }
          } else {
            False
          }
        _ -> False
      }
    },
  )
}

// ============================================================================
// Tests
// ============================================================================

const test_warp_hash =
  #"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

const test_synthetic_policy =
  #"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"

const test_other_state_policy =
  #"dddddddddddddddddddddddddddddddddddddddddddddddddddddddd"

fn test_warp_ref() -> OutputReference {
  OutputReference {
    transaction_id: #"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
    output_index: 0,
  }
}

fn test_message() -> Message {
  Message {
    version: 3,
    nonce: 1,
    origin: 1,
    sender: #"1111111111111111111111111111111111111111111111111111111111111111",
    destination: 2,
    recipient: #"0100000022222222222222222222222222222222222222222222222222222222",
    body: #"abcd",
  }
}

fn test_tx(redeemer: WarpRouteRedeemer, quantity: Int) -> Transaction {
  test_tx_with_state_policy(redeemer, quantity, test_warp_hash)
}

fn test_tx_with_state_policy(
  redeemer: WarpRouteRedeemer,
  quantity: Int,
  state_policy: ByteArray,
) -> Transaction {
  let raw_redeemer: Data = redeemer
  Transaction {
    inputs: [
      Input {
        output_reference: test_warp_ref(),
        output: Output {
          address: Address {
            payment_credential: Script(test_warp_hash),
            stake_credential: None,
          },
          value: assets.from_asset(state_policy, #"", 1),
          datum: NoDatum,
          reference_script: None,
        },
      },
    ],
    reference_inputs: [],
    outputs: [],
    fee: 0,
    // Non-empty name: the policy is name-agnostic (the warp route pins the
    // name from its datum); "hNIGHT" here locks that property in.
    mint: assets.from_asset(test_synthetic_policy, #"684e49474854", quantity),
    certificates: [],
    withdrawals: [],
    validity_range: Interval {
      lower_bound: IntervalBound {
        bound_type: NegativeInfinity,
        is_inclusive: False,
      },
      upper_bound: IntervalBound {
        bound_type: PositiveInfinity,
        is_inclusive: False,
      },
    },
    extra_signatories: [],
    redeemers: [Pair(Spend(test_warp_ref()), raw_redeemer)],
    datums: dict.empty,
    id: #"0000",
    votes: [],
    proposal_procedures: [],
    current_treasury_amount: None,
    treasury_donation: None,
  }
}

test mint_requires_receive_transfer() {
  let redeemer: WarpRouteRedeemer =
    ReceiveTransfer { message: test_message(), message_id: #"00" }
  validate_mint_or_burn(
    test_warp_hash,
    test_warp_hash,
    test_synthetic_policy,
    test_tx(redeemer, 10),
  )
}

test burn_requires_transfer_remote() {
  let redeemer: WarpRouteRedeemer =
    TransferRemote {
      destination: 1,
      recipient: test_message().recipient,
      amount: 10,
    }
  validate_mint_or_burn(
    test_warp_hash,
    test_warp_hash,
    test_synthetic_policy,
    test_tx(redeemer, -10),
  )
}

test owner_enroll_cannot_mint() {
  let redeemer: WarpRouteRedeemer =
    EnrollRemoteRoute { domain: 1, route: test_message().recipient }
  !validate_mint_or_burn(
    test_warp_hash,
    test_warp_hash,
    test_synthetic_policy,
    test_tx(redeemer, 10),
  )
}

test fake_warp_instance_cannot_mint() {
  let redeemer: WarpRouteRedeemer =
    ReceiveTransfer { message: test_message(), message_id: #"00" }
  !validate_mint_or_burn(
    test_warp_hash,
    test_warp_hash,
    test_synthetic_policy,
    test_tx_with_state_policy(redeemer, 10, test_other_state_policy),
  )
}

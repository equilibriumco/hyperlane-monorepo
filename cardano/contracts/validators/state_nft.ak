use aiken/collection/dict
use aiken/collection/list
use aiken/interval
use cardano/address
use cardano/assets
use cardano/transaction.{OutputReference, Transaction}

/// State NFT minting policy
/// One-shot minting tied to specific UTXO consumption
/// Used to create unique identifiers for protocol state UTXOs
/// Validator parameterized by the UTXO that must be consumed
///
/// For the reference script patterns, this allows minting up to 3 NFTs:
/// - State NFT (empty asset name) for the state UTXO
/// - Reference Script NFT ("ref") for the recipient reference script UTXO
/// - Message Ref NFT ("msg_ref") for the verified_message_nft reference script UTXO (deferred only)
///
/// Every name is capped at a single unit. Downstream validators authenticate a
/// state UTXO by asserting it carries exactly one of these tokens, so a second
/// unit of the same name would produce two UTXOs that both satisfy that check
/// and make state selection ambiguous.
validator state_nft(utxo_ref: OutputReference) {
  mint(_redeemer: Data, policy_id: ByteArray, tx: Transaction) {
    // Can only mint if specific UTXO is consumed (one-shot). The seed UTXO is
    // spent by the deploy transaction, so this policy mints exactly once and
    // its supply is fixed from that point on.
    let utxo_consumed =
      list.any(tx.inputs, fn(input) { input.output_reference == utxo_ref })

    // Only this policy's own mints count. A canonical config NFT or any other
    // policy minted in the same transaction must not consume our budget.
    let own_quantities = dict.values(assets.tokens(tx.mint, policy_id))

    // 1-3 distinct names (state NFT + ref NFT + msg_ref NFT), one unit each.
    let count = list.length(own_quantities)
    let each_unique = list.all(own_quantities, fn(qty) { qty == 1 })

    utxo_consumed && each_unique && count >= 1 && count <= 3
  }

  else(_) {
    fail
  }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const test_seed_ref =
  OutputReference {
    transaction_id: #"0000000000000000000000000000000000000000000000000000000000000001",
    output_index: 0,
  }

const this_policy = #"00000000000000000000000000000000000000000000000000000001"

const other_policy = #"00000000000000000000000000000000000000000000000000000002"

fn seed_input() -> transaction.Input {
  transaction.Input {
    output_reference: test_seed_ref,
    output: transaction.Output {
      address: address.Address {
        payment_credential: address.Script(
          #"00000000000000000000000000000000000000000000000000000009",
        ),
        stake_credential: None,
      },
      value: assets.zero,
      datum: transaction.NoDatum,
      reference_script: None,
    },
  }
}

fn mint_tx(mint: assets.Value) -> Transaction {
  Transaction {
    inputs: [seed_input()],
    reference_inputs: [],
    outputs: [],
    fee: 0,
    mint,
    certificates: [],
    withdrawals: [],
    validity_range: interval.everything,
    extra_signatories: [],
    redeemers: [],
    datums: dict.empty,
    id: #"0000",
    votes: [],
    proposal_procedures: [],
    current_treasury_amount: None,
    treasury_donation: None,
  }
}

/// The canonical deploy shape: one state NFT, one "ref" NFT, both quantity 1.
test accepts_the_canonical_two_nft_deploy() {
  let mint =
    assets.merge(
      assets.from_asset(this_policy, #"", 1),
      assets.from_asset(this_policy, #"726566", 1),
    )
  state_nft.mint(test_seed_ref, Void, this_policy, mint_tx(mint))
}

/// Minting is impossible once the seed UTXO is gone — this is what makes the
/// supply permanent after deploy.
test rejects_when_seed_utxo_is_not_consumed() {
  let mint = assets.from_asset(this_policy, #"", 1)
  let tx = Transaction { ..mint_tx(mint), inputs: [] }
  !state_nft.mint(test_seed_ref, Void, this_policy, tx)
}

/// A single asset name minted three times over. The whole scheme assumes a
/// state NFT is unique; nothing here enforces it.
test rejects_duplicate_units_of_one_asset_name() {
  let mint = assets.from_asset(this_policy, #"", 3)
  !state_nft.mint(test_seed_ref, Void, this_policy, mint_tx(mint))
}

/// Another policy's mints must not consume this policy's budget. The recipient
/// deploy already mints canonical + state + ref = 3, so any further NFT under
/// this policy would otherwise be impossible.
test ignores_mints_from_other_policies() {
  let mint =
    assets.merge(
      assets.merge(
        assets.from_asset(this_policy, #"", 1),
        assets.from_asset(this_policy, #"726566", 1),
      ),
      assets.merge(
        assets.from_asset(other_policy, #"6f7468657231", 1),
        assets.from_asset(other_policy, #"6f7468657232", 1),
      ),
    )
  state_nft.mint(test_seed_ref, Void, this_policy, mint_tx(mint))
}

/// The three-NFT pattern the doc comment describes, alongside a canonical
/// config NFT from a different policy. Must mint.
test accepts_the_full_three_nft_pattern_with_a_canonical_nft() {
  let mint =
    assets.merge(
      assets.merge(
        assets.from_asset(this_policy, #"", 1),
        assets.from_asset(this_policy, #"726566", 1),
      ),
      assets.merge(
        assets.from_asset(this_policy, #"6d73675f726566", 1),
        assets.from_asset(other_policy, #"63616e6f6e", 1),
      ),
    )
  state_nft.mint(test_seed_ref, Void, this_policy, mint_tx(mint))
}

/// Burning is not a mint. Rejected outright rather than netting against mints.
test rejects_a_burn() {
  let mint = assets.from_asset(this_policy, #"", -1)
  !state_nft.mint(test_seed_ref, Void, this_policy, mint_tx(mint))
}

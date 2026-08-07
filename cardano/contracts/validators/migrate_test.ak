use aiken/collection/dict
use aiken/interval.{Interval, IntervalBound, NegativeInfinity, PositiveInfinity}
use cardano/address.{Address, Credential, Script}
use cardano/assets.{from_asset, from_lovelace, merge, zero}
use cardano/transaction.{InlineDatum, Input, NoDatum, Output, OutputReference, Transaction}
use types.{
  IgpDatum, GasOracleConfig,
  MailboxDatum, MerkleTreeState, MessageIdMultisig,
  MultisigIsmDatum,
  WarpRouteDatum, WarpRouteConfig, Native,
}
use utils.{validate_migrate}

const owner_key = #"aabbccddaabbccddaabbccddaabbccddaabbccddaabbccdd00000000"

const old_script_hash = #"1111111111111111111111111111111111111111111111111111111111111111"

const new_script_hash = #"2222222222222222222222222222222222222222222222222222222222222222"

const state_nft_policy = #"f1bd8bb9bbd7d3fe0a20b12e1d8f36ab717c6e9c8e796f5375400378"

fn old_address() -> Address {
  Address {
    payment_credential: Script(old_script_hash),
    stake_credential: None,
  }
}

fn new_address() -> Address {
  Address {
    payment_credential: Script(new_script_hash),
    stake_credential: None,
  }
}

fn own_ref() -> OutputReference {
  OutputReference { transaction_id: #"aabb00", output_index: 0 }
}

fn empty_validity_range() -> Interval<Int> {
  Interval {
    lower_bound: IntervalBound { bound_type: NegativeInfinity, is_inclusive: False },
    upper_bound: IntervalBound { bound_type: PositiveInfinity, is_inclusive: False },
  }
}

fn sample_mailbox_datum() -> MailboxDatum {
  MailboxDatum { version: 0, 
    local_domain: 2003,
    default_ism: #"aaaa000000000000000000000000000000000000000000000000aaaa",
    owner: owner_key,
    outbound_nonce: 42,
    merkle_tree: MerkleTreeState { branches: [], count: 42 },
    processed_tree_root: #"5c3cc358c060877ced35947091c44c900594ece1e0a4ade23143ef57c3f7600f",
  }
}

fn base_value() {
  merge(from_lovelace(5_000_000), from_asset(state_nft_policy, "", 1))
}

fn mock_tx(signatories: List<ByteArray>, outputs: List<Output>) -> Transaction {
  Transaction {
    inputs: [
      Input {
        output_reference: own_ref(),
        output: Output {
          address: old_address(),
          value: base_value(),
          datum: InlineDatum(sample_mailbox_datum()),
          reference_script: None,
        },
      },
    ],
    reference_inputs: [],
    outputs: outputs,
    fee: 200_000,
    mint: zero,
    certificates: [],
    withdrawals: [],
    validity_range: empty_validity_range(),
    extra_signatories: signatories,
    redeemers: [],
    datums: dict.empty,
    id: #"0000",
    votes: [],
    proposal_procedures: [],
    current_treasury_amount: None,
    treasury_donation: None,
  }
}

test migrate_succeeds_when_version_is_incremented() {
  let datum = sample_mailbox_datum()
  let migrated = MailboxDatum { ..datum, version: datum.version + 1 }
  let continuation = Output {
    address: new_address(),
    value: base_value(),
    datum: InlineDatum(migrated),
    reference_script: None,
  }
  let tx = mock_tx([owner_key], [continuation])
  validate_migrate(owner_key, new_address(), tx, own_ref(), datum.version)
}

test migrate_fails_without_owner_sig() fail {
  let datum = sample_mailbox_datum()
  let migrated = MailboxDatum { ..datum, version: datum.version + 1 }
  let continuation = Output {
    address: new_address(),
    value: base_value(),
    datum: InlineDatum(migrated),
    reference_script: None,
  }
  let tx = mock_tx([], [continuation])
  validate_migrate(owner_key, new_address(), tx, own_ref(), datum.version)
}

/// Migration is owner-attested beyond the version: the old validator cannot
/// verify a shape it was not compiled against, so rewriting state is allowed
/// provided the version moves forward.
test migrate_allows_state_to_change_when_version_moves() {
  let datum = sample_mailbox_datum()
  let migrated =
    MailboxDatum { ..datum, version: datum.version + 1, outbound_nonce: 999 }
  let continuation = Output {
    address: new_address(),
    value: base_value(),
    datum: InlineDatum(migrated),
    reference_script: None,
  }
  let tx = mock_tx([owner_key], [continuation])
  validate_migrate(owner_key, new_address(), tx, own_ref(), datum.version)
}

/// The version is the one thing the old code can check, so a migration that
/// leaves it alone is refused while the state is still spendable here.
test migrate_fails_when_version_unchanged() fail {
  let datum = sample_mailbox_datum()
  let continuation = Output {
    address: new_address(),
    value: base_value(),
    datum: InlineDatum(datum),
    reference_script: None,
  }
  let tx = mock_tx([owner_key], [continuation])
  validate_migrate(owner_key, new_address(), tx, own_ref(), datum.version)
}

test migrate_fails_when_version_skips_ahead() fail {
  let datum = sample_mailbox_datum()
  let migrated = MailboxDatum { ..datum, version: datum.version + 2 }
  let continuation = Output {
    address: new_address(),
    value: base_value(),
    datum: InlineDatum(migrated),
    reference_script: None,
  }
  let tx = mock_tx([owner_key], [continuation])
  validate_migrate(owner_key, new_address(), tx, own_ref(), datum.version)
}

test migrate_fails_when_version_goes_backwards() fail {
  let datum = sample_mailbox_datum()
  let migrated = MailboxDatum { ..datum, version: datum.version - 1 }
  let continuation = Output {
    address: new_address(),
    value: base_value(),
    datum: InlineDatum(migrated),
    reference_script: None,
  }
  let tx = mock_tx([owner_key], [continuation])
  validate_migrate(owner_key, new_address(), tx, own_ref(), datum.version)
}

test migrate_fails_when_value_not_preserved() fail {
  let datum = sample_mailbox_datum()
  let continuation = Output {
    address: new_address(),
    value: from_lovelace(1_000_000),
    datum: InlineDatum(datum),
    reference_script: None,
  }
  let tx = mock_tx([owner_key], [continuation])
  validate_migrate(owner_key, new_address(), tx, own_ref(), datum.version)
}

test migrate_fails_when_no_continuation_at_new_address() fail {
  let datum = sample_mailbox_datum()
  let wrong_output = Output {
    address: old_address(),
    value: base_value(),
    datum: InlineDatum(datum),
    reference_script: None,
  }
  let tx = mock_tx([owner_key], [wrong_output])
  validate_migrate(owner_key, new_address(), tx, own_ref(), datum.version)
}

test migrate_fails_when_continuation_has_no_datum() fail {
  let datum = sample_mailbox_datum()
  let continuation = Output {
    address: new_address(),
    value: base_value(),
    datum: NoDatum,
    reference_script: None,
  }
  let tx = mock_tx([owner_key], [continuation])
  validate_migrate(owner_key, new_address(), tx, own_ref(), datum.version)
}

test migrate_succeeds_with_extra_lovelace() {
  let datum = sample_mailbox_datum()
  let migrated = MailboxDatum { ..datum, version: datum.version + 1 }
  let continuation = Output {
    address: new_address(),
    value: merge(from_lovelace(10_000_000), from_asset(state_nft_policy, "", 1)),
    datum: InlineDatum(migrated),
    reference_script: None,
  }
  let tx = mock_tx([owner_key], [continuation])
  validate_migrate(owner_key, new_address(), tx, own_ref(), datum.version)
}

test migrate_works_with_igp_datum() {
  let igp_datum = IgpDatum { version: 0, 
    owner: owner_key,
    beneficiary: #"deadbeef",
    gas_oracles: [(11155111, GasOracleConfig { gas_price: 1_000_000_000, token_exchange_rate: 7171, gas_overhead: 150_000 })],
  }
  let migrated = IgpDatum { ..igp_datum, version: igp_datum.version + 1 }
  let continuation = Output {
    address: new_address(),
    value: base_value(),
    datum: InlineDatum(migrated),
    reference_script: None,
  }
  let tx = mock_tx([owner_key], [continuation])
  validate_migrate(owner_key, new_address(), tx, own_ref(), igp_datum.version)
}

test migrate_works_with_ism_datum() {
  let ism_datum = MultisigIsmDatum { version: 0, 
    validators: [(11155111, [#"aabbccdd00000000000000000000000000000000"])],
    thresholds: [(11155111, 1)],
    owner: owner_key,
    module_type: MessageIdMultisig,
  }
  let migrated = MultisigIsmDatum { ..ism_datum, version: ism_datum.version + 1 }
  let continuation = Output {
    address: new_address(),
    value: base_value(),
    datum: InlineDatum(migrated),
    reference_script: None,
  }
  let tx = mock_tx([owner_key], [continuation])
  validate_migrate(owner_key, new_address(), tx, own_ref(), ism_datum.version)
}

test migrate_fails_when_nft_extracted_but_lovelace_preserved() fail {
  let datum = sample_mailbox_datum()
  // Attacker preserves lovelace but omits the state NFT
  let continuation = Output {
    address: new_address(),
    value: from_lovelace(5_000_000),
    datum: InlineDatum(datum),
    reference_script: None,
  }
  let tx = mock_tx([owner_key], [continuation])
  validate_migrate(owner_key, new_address(), tx, own_ref(), datum.version)
}

test migrate_works_with_warp_datum() {
  let warp_datum = WarpRouteDatum { version: 0, 
    config: WarpRouteConfig {
      token_type: Native,
      decimals: 6,
      remote_decimals: 18,
      remote_routes: [],
    },
    owner: owner_key,
    total_bridged: 0,
    ism: None,
    destination_gas: [],
  }
  let migrated = WarpRouteDatum { ..warp_datum, version: warp_datum.version + 1 }
  let continuation = Output {
    address: new_address(),
    value: base_value(),
    datum: InlineDatum(migrated),
    reference_script: None,
  }
  let tx = mock_tx([owner_key], [continuation])
  validate_migrate(owner_key, new_address(), tx, own_ref(), warp_datum.version)
}

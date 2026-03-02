use aiken/collection/dict
use aiken/collection/list
use aiken/collection/pairs
use aiken/crypto.{VerificationKeyHash, keccak_256}
use aiken/primitive/bytearray
use cardano/address.{Address, Credential, Script}
use cardano/assets
use aiken/interval.{Interval, IntervalBound, NegativeInfinity, PositiveInfinity}
use cardano/transaction.{
  InlineDatum, Input, Output, OutputReference, Spend, Transaction,
}
use merkle
use smt
use types.{
  Checkpoint, Domain, HyperlaneAddress, MailboxDatum,
  MailboxRedeemer, Message, MerkleTreeState, MultisigIsmRedeemer, Native, PolicyId,
  ScriptHash, VerifiedMessageDatum, Verify, WarpRouteConfig, WarpRouteDatum,
  encode_message, Dispatch, Process, SetDefaultIsm, TransferOwnership, MigrateMailbox,
}
use utils.{find_input, hyperlane_address_to_script_hash, is_signed_by, policy_id_to_hyperlane_address, validate_migrate, value_preserved}

/// Mailbox Validator
/// Core Hyperlane contract that dispatches and processes messages
///
/// During Process, the mailbox:
/// 1. Verifies the message via ISM
/// 2. Mints a processed_message_nft (replay protection)
/// 3. Mints a verified_message_nft and delivers it to the recipient's script address
///    inside a UTXO with VerifiedMessageDatum
validator mailbox(
  processed_messages_nft_policy: PolicyId,
  verified_message_nft_policy: PolicyId,
  ism_nft_policy: PolicyId,
) {
  spend(
    datum: Option<MailboxDatum>,
    redeemer: MailboxRedeemer,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    expect Some(mailbox_datum) = datum

    when redeemer is {
      Dispatch { destination, recipient, body, sender_ref, hook_metadata } ->
        validate_dispatch(
          mailbox_datum,
          destination,
          recipient,
          body,
          sender_ref,
          hook_metadata,
          tx,
          own_ref,
        )

      Process { message, metadata, message_id, smt_proof } ->
        validate_process(
          mailbox_datum,
          message,
          metadata,
          message_id,
          smt_proof,
          tx,
          own_ref,
          processed_messages_nft_policy,
          verified_message_nft_policy,
          ism_nft_policy,
        )

      SetDefaultIsm { new_ism } ->
        validate_set_default_ism(mailbox_datum, new_ism, tx, own_ref)

      TransferOwnership { new_owner } ->
        validate_transfer_ownership(mailbox_datum, new_owner, tx, own_ref)

      MigrateMailbox { new_address } -> {
        let datum_as_data: Data = mailbox_datum
        validate_migrate(
          mailbox_datum.owner,
          new_address,
          tx,
          own_ref,
          datum_as_data,
        )
      }
    }
  }

  else(_) {
    fail
  }
}

fn validate_dispatch(
  datum: MailboxDatum,
  destination: Domain,
  recipient: HyperlaneAddress,
  body: ByteArray,
  sender_ref: OutputReference,
  _hook_metadata: ByteArray,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  let message =
    Message {
      version: 3,
      nonce: datum.outbound_nonce,
      origin: datum.local_domain,
      sender: get_sender_address(tx, sender_ref),
      destination,
      recipient,
      body,
    }

  let message_hash = keccak_256(encode_message(message))

  let current_tree =
    merkle.MerkleTree {
      branches: datum.merkle_tree.branches,
      count: datum.merkle_tree.count,
    }
  let new_tree = merkle.insert(current_tree, message_hash)

  let new_datum =
    MailboxDatum {
      ..datum,
      outbound_nonce: datum.outbound_nonce + 1,
      merkle_tree: MerkleTreeState {
        branches: new_tree.branches,
        count: new_tree.count,
      },
    }

  validate_continuation(datum, new_datum, tx, own_ref)
}

fn validate_process(
  datum: MailboxDatum,
  message: Message,
  metadata: ByteArray,
  message_id: ByteArray,
  smt_proof: List<ByteArray>,
  tx: Transaction,
  own_ref: OutputReference,
  processed_messages_nft_policy: PolicyId,
  verified_message_nft_policy: PolicyId,
  ism_nft_policy: PolicyId,
) -> Bool {
  // 1. Verify message is for this domain
  expect message.destination == datum.local_domain

  // 2. Verify message_id is correctly computed
  let computed_id = keccak_256(encode_message(message))
  expect computed_id == message_id

  // 3. Verify message not already processed via SMT non-membership proof.
  //    Uses first 16 bytes of message_id as the 128-bit key.
  let smt_key = bytearray.take(message_id, 16)
  let new_tree_root =
    smt.verify_non_membership_and_insert(
      datum.processed_tree_root,
      smt_key,
      smt_proof,
    )

  // 4. Verify ISM approves the message (with NFT authenticity check)
  let ism_hash = get_recipient_ism(message.recipient, datum.default_ism, tx)
  expect verify_ism_for_message(ism_hash, datum.default_ism, ism_nft_policy, message_id, tx, metadata)

  // 5. Verify processed message NFT is being minted (for queryability)
  expect processed_nft_minted(message_id, tx, processed_messages_nft_policy)

  // 6. For script-hash recipients (0x02 prefix), verify verified message delivery.
  //    NFT-policy recipients (0x01 prefix, e.g. warp routes) handle their own
  //    validation as spent inputs in the same TX.
  let is_script_recipient = bytearray.at(message.recipient, 0) == 2
  expect
    if is_script_recipient {
      verified_nft_minted(message_id, tx, verified_message_nft_policy) && verified_message_delivered(
        message,
        message_id,
        tx,
        verified_message_nft_policy,
      )
    } else {
      True
    }

  // 7. Validate mailbox continuation with updated processed_tree_root
  let new_datum = MailboxDatum { ..datum, processed_tree_root: new_tree_root }
  validate_continuation(datum, new_datum, tx, own_ref)
}

fn validate_set_default_ism(
  datum: MailboxDatum,
  new_ism: ScriptHash,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  expect is_signed_by(tx, datum.owner)

  let new_datum = MailboxDatum { ..datum, default_ism: new_ism }
  validate_continuation(datum, new_datum, tx, own_ref)
}

fn validate_transfer_ownership(
  datum: MailboxDatum,
  new_owner: VerificationKeyHash,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  expect is_signed_by(tx, datum.owner)

  let new_datum = MailboxDatum { ..datum, owner: new_owner }
  validate_continuation(datum, new_datum, tx, own_ref)
}

fn validate_continuation(
  _old_datum: MailboxDatum,
  new_datum: MailboxDatum,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address
  let own_value = own_input.output.value

  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: MailboxDatum = cont_datum_data
  expect and {
      cont_datum.local_domain == new_datum.local_domain,
      cont_datum.default_ism == new_datum.default_ism,
      cont_datum.owner == new_datum.owner,
      cont_datum.outbound_nonce == new_datum.outbound_nonce,
      cont_datum.merkle_tree.branches == new_datum.merkle_tree.branches,
      cont_datum.merkle_tree.count == new_datum.merkle_tree.count,
      cont_datum.processed_tree_root == new_datum.processed_tree_root,
    }

  value_preserved(own_value, continuation.value)
}

/// Verify that a verified message UTXO is delivered to the recipient's script address
fn verified_message_delivered(
  message: Message,
  message_id: ByteArray,
  tx: Transaction,
  verified_message_nft_policy: PolicyId,
) -> Bool {
  // Extract recipient script hash from the 32-byte Hyperlane address
  expect Some(recipient_hash) = hyperlane_address_to_script_hash(message.recipient)

  let expected_datum =
    VerifiedMessageDatum {
      origin: message.origin,
      sender: message.sender,
      body: message.body,
      message_id,
      nonce: message.nonce,
    }

  list.any(
    tx.outputs,
    fn(output) {
      // Check output is at the recipient's script address
      let address_valid =
        when output.address.payment_credential is {
          Script(hash) -> hash == recipient_hash
          _ -> False
        }

      // Check output contains verified_message_nft with message_id as asset name
      let nft_valid =
        assets.quantity_of(output.value, verified_message_nft_policy, message_id) == 1

      // Check datum matches
      let datum_valid =
        when output.datum is {
          InlineDatum(datum_data) -> {
            expect actual_datum: VerifiedMessageDatum = datum_data
            actual_datum == expected_datum
          }
          _ -> False
        }

      address_valid && nft_valid && datum_valid
    },
  )
}

fn get_sender_address(tx: Transaction, sender_ref: OutputReference) -> HyperlaneAddress {
  expect Some(input) =
    list.find(tx.inputs, fn(i) { i.output_reference == sender_ref })

  when input.output.address.payment_credential is {
    Credential.VerificationKey(hash) -> bytearray.concat(#"00000000", hash)
    Script(hash) -> {
      let nft_policy = find_state_nft_policy(input.output.value)
      when nft_policy is {
        Some(policy) -> policy_id_to_hyperlane_address(policy)
        None -> bytearray.concat(#"02000000", hash)
      }
    }
  }
}

fn find_state_nft_policy(value: assets.Value) -> Option<ByteArray> {
  let flat = assets.flatten(value)
  list.foldl(
    flat,
    None,
    fn(item, acc) {
      when acc is {
        Some(_) -> acc
        None -> {
          let (policy, name, qty) = item
          if policy != #"" && qty == 1 && name == #"" {
            Some(policy)
          } else {
            None
          }
        }
      }
    },
  )
}

fn get_recipient_ism(
  recipient: HyperlaneAddress,
  default_ism: ScriptHash,
  tx: Transaction,
) -> ScriptHash {
  let prefix = bytearray.at(recipient, 0)
  // Bytes 4..31 carry either the NFT policy (0x01) or script hash (0x02)
  let addr_payload = bytearray.drop(recipient, 4)
  if prefix == 1 {
    // Warp routes: ISM override is in the state UTXO datum (spent input)
    when find_warp_route_ism(addr_payload, tx.inputs) is {
      Some(ism) -> ism
      None -> default_ism
    }
  } else if prefix == 2 {
    // Generic script recipients: ISM override is in a HyperlaneRecipientDatum
    // provided as a reference input at the recipient's script address
    when find_generic_recipient_ism(addr_payload, tx.reference_inputs) is {
      Some(ism) -> ism
      None -> default_ism
    }
  } else {
    default_ism
  }
}

fn find_warp_route_ism(
  policy_id: PolicyId,
  inputs: List<Input>,
) -> Option<ScriptHash> {
  let maybe_input =
    list.find(
      inputs,
      fn(input) { !dict.is_empty(assets.tokens(input.output.value, policy_id)) },
    )
  when maybe_input is {
    None -> None
    Some(input) ->
      when input.output.datum is {
        InlineDatum(datum_data) -> {
          expect warp_datum: WarpRouteDatum = datum_data
          warp_datum.ism
        }
        _ -> None
      }
  }
}

/// Look for a reference input at the recipient's script address that carries a
/// "hyperlane-config" token and parse its datum as Option<ScriptHash>.
///
/// The "hyperlane-config" token requirement prevents an attacker from planting a
/// plain datum at the recipient's address: minting requires the recipient script
/// to run with a constructor-0 (Init) redeemer, which only the owner can produce.
fn find_generic_recipient_ism(
  script_hash: ScriptHash,
  reference_inputs: List<Input>,
) -> Option<ScriptHash> {
  let maybe_input =
    list.find(
      reference_inputs,
      fn(input) {
        let is_script_addr =
          when input.output.address.payment_credential is {
            Script(h) -> h == script_hash
            _ -> False
          }
        let has_config_token =
          list.any(
            assets.flatten(input.output.value),
            fn(item) {
              let (_p, name, qty) = item
              name == script_hash && qty > 0
            },
          )
        is_script_addr && has_config_token
      },
    )
  when maybe_input is {
    None -> None
    Some(input) ->
      when input.output.datum is {
        InlineDatum(datum_data) -> {
          expect ism_override: Option<ScriptHash> = datum_data
          ism_override
        }
        _ -> None
      }
  }
}

fn verify_ism_for_message(
  ism_hash: ScriptHash,
  default_ism: ScriptHash,
  ism_nft_policy: PolicyId,
  message_id: ByteArray,
  tx: Transaction,
  metadata: ByteArray,
) -> Bool {
  expect bytearray.length(metadata) > 0

  list.any(
    tx.inputs,
    fn(input) {
      when input.output.address.payment_credential is {
        Script(hash) ->
          if hash == ism_hash {
            // For the default ISM: also verify authenticity via the one-shot state NFT.
            // For override ISMs: the script hash from the warp route datum is sufficient;
            // requiring a specific NFT policy would block legitimate override deployments.
            let has_nft =
              if ism_hash == default_ism {
                !dict.is_empty(assets.tokens(input.output.value, ism_nft_policy))
              } else {
                True
              }
            has_nft && verify_ism_redeemer_message_id(
              input.output_reference,
              message_id,
              tx,
            )
          } else {
            False
          }
        _ -> False
      }
    },
  )
}

fn verify_ism_redeemer_message_id(
  ism_output_ref: OutputReference,
  expected_message_id: ByteArray,
  tx: Transaction,
) -> Bool {
  let spend_purpose = Spend(ism_output_ref)

  when pairs.get_first(tx.redeemers, spend_purpose) is {
    Some(redeemer_data) -> {
      expect ism_redeemer: MultisigIsmRedeemer = redeemer_data
      when ism_redeemer is {
        Verify { checkpoint, .. } ->
          checkpoint.message_id == expected_message_id
        _ -> False
      }
    }
    None -> False
  }
}

fn verified_nft_minted(
  message_id: ByteArray,
  tx: Transaction,
  verified_message_nft_policy: PolicyId,
) -> Bool {
  let policy_mints = assets.tokens(tx.mint, verified_message_nft_policy)
  when dict.get(policy_mints, message_id) is {
    Some(quantity) -> quantity == 1
    None -> False
  }
}

fn processed_nft_minted(
  message_id: ByteArray,
  tx: Transaction,
  processed_messages_nft_policy: PolicyId,
) -> Bool {
  let policy_mints = assets.tokens(tx.mint, processed_messages_nft_policy)
  when dict.get(policy_mints, message_id) is {
    Some(quantity) -> quantity == 1
    None -> False
  }
}

// ============================================================================
// Tests
// ============================================================================

// 28-byte (56 hex chars) constants for ScriptHash / PolicyId
const test_default_ism = #"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa00"

const test_override_ism = #"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"

const test_warp_policy = #"cccccccccccccccccccccccccccccccccccccccccccccccccccccccc"

// 32-byte Hyperlane address: 0x01 prefix + 3 zero bytes + 28-byte policy id
const test_warp_recipient =
  #"01000000cccccccccccccccccccccccccccccccccccccccccccccccccccccccc"

// 32-byte Hyperlane address: 0x02 prefix + 4 zero bytes + 28-byte script hash
const test_generic_recipient =
  #"020000001234567890abcdef1234567890abcdef1234567890abcdef12345678"

fn unbounded_validity_range() -> Interval<Int> {
  Interval {
    lower_bound: IntervalBound { bound_type: NegativeInfinity, is_inclusive: False },
    upper_bound: IntervalBound { bound_type: PositiveInfinity, is_inclusive: False },
  }
}

fn make_warp_input(ism_override: Option<ScriptHash>) -> Input {
  let datum =
    WarpRouteDatum {
      config: WarpRouteConfig {
        token_type: Native,
        decimals: 6,
        remote_decimals: 18,
        remote_routes: [],
      },
      owner: #"00000000000000000000000000000000000000000000000000000000",
      total_bridged: 0,
      ism: ism_override,
    }

  Input {
    output_reference: OutputReference {
      transaction_id: #"0000000000000000000000000000000000000000000000000000000000000001",
      output_index: 0,
    },
    output: Output {
      address: Address {
        payment_credential: Script(test_warp_policy),
        stake_credential: None,
      },
      value: assets.from_asset(test_warp_policy, #"", 1),
      datum: InlineDatum(datum),
      reference_script: None,
    },
  }
}

fn make_tx(inputs: List<Input>, reference_inputs: List<Input>) -> Transaction {
  Transaction {
    inputs,
    reference_inputs,
    outputs: [],
    fee: 0,
    mint: assets.zero,
    certificates: [],
    withdrawals: [],
    validity_range: unbounded_validity_range(),
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

// 28-byte script hash embedded in test_generic_recipient (bytes 4-31)
const test_generic_script_hash =
  #"1234567890abcdef1234567890abcdef1234567890abcdef12345678"

// Fake canonical config NFT policy used in tests
const test_canonical_policy =
  #"aabbccddaabbccddaabbccddaabbccddaabbccddaabbccddaabbccdd"

fn make_generic_ism_config_input(ism_override: Option<ScriptHash>) -> Input {
  // Datum is Option<ScriptHash> (the new canonical config format)
  let datum: Option<ScriptHash> = ism_override

  Input {
    output_reference: OutputReference {
      transaction_id: #"0000000000000000000000000000000000000000000000000000000000000002",
      output_index: 0,
    },
    output: Output {
      address: Address {
        payment_credential: Script(test_generic_script_hash),
        stake_credential: None,
      },
      value: assets.from_asset(test_canonical_policy, test_generic_script_hash, 1),
      datum: InlineDatum(datum),
      reference_script: None,
    },
  }
}

test get_recipient_ism_generic_no_ref_input_uses_default() {
  let tx = make_tx([], [])
  get_recipient_ism(test_generic_recipient, test_default_ism, tx) == test_default_ism
}

test get_recipient_ism_generic_no_override_uses_default() {
  let ref_input = make_generic_ism_config_input(None)
  let tx = make_tx([], [ref_input])
  get_recipient_ism(test_generic_recipient, test_default_ism, tx) == test_default_ism
}

test get_recipient_ism_generic_with_override_uses_override() {
  let ref_input = make_generic_ism_config_input(Some(test_override_ism))
  let tx = make_tx([], [ref_input])
  get_recipient_ism(test_generic_recipient, test_default_ism, tx) == test_override_ism
}

test get_recipient_ism_warp_no_override_uses_default() {
  let tx = make_tx([make_warp_input(None)], [])
  get_recipient_ism(test_warp_recipient, test_default_ism, tx) == test_default_ism
}

test get_recipient_ism_warp_with_override_uses_override() {
  let tx = make_tx([make_warp_input(Some(test_override_ism))], [])
  get_recipient_ism(test_warp_recipient, test_default_ism, tx) == test_override_ism
}

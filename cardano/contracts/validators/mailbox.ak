use aiken/collection/dict
use aiken/collection/list
use aiken/collection/pairs
use aiken/crypto.{VerificationKeyHash, keccak_256}
use aiken/interval.{Interval, IntervalBound, NegativeInfinity, PositiveInfinity}
use aiken/primitive/bytearray
use cardano/address.{Address, Credential, Script}
use cardano/assets
use cardano/transaction.{
  InlineDatum, Input, NoDatum, Output, OutputReference, Spend, Transaction,
}
use merkle
use smt
use types.{
  Checkpoint, Dispatch, Domain, HyperlaneAddress, IsmConfig, MailboxDatum,
  MailboxRedeemer, MerkleRootIsmRedeemer, MerkleRootMultisig, MerkleRootVerify,
  MerkleTreeState, Message, MessageIdMultisig, MigrateMailbox, ModuleType,
  MultisigIsmDatum, MultisigIsmRedeemer, Native, PolicyId, Process,
  ReceiveTransfer, ScriptHash, SetDefaultIsm, TransferOwnership,
  VerifiedMessageDatum, Verify, WarpRouteConfig, WarpRouteDatum,
  WarpRouteRedeemer, encode_message,
}
use utils.{
  find_input, hyperlane_address_to_policy_id, hyperlane_address_to_script_hash,
  is_signed_by, policy_id_to_hyperlane_address, validate_migrate,
  value_preserved,
}

/// Fixed policy ID of canonical_config_nft.ak (non-parameterized validator).
/// Any config UTXO at a generic recipient's address MUST hold a token from
/// this policy to be accepted as an ISM override by find_generic_recipient_ism.
const canonical_config_nft_policy: PolicyId =
  #"1ae447f5a98155243852ba2264e150c03ace89cf3e17fd1da670ae36"

/// Mailbox Validator
/// Core Hyperlane contract that dispatches and processes messages
///
/// During Process, the mailbox:
/// 1. Verifies the message via ISM
/// 2. Mints a verified_message_nft and delivers it to the recipient's script address
///    inside a UTXO with VerifiedMessageDatum
validator mailbox(
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
          verified_message_nft_policy,
          ism_nft_policy,
        )

      SetDefaultIsm { new_ism } ->
        validate_set_default_ism(mailbox_datum, new_ism, tx, own_ref)

      TransferOwnership { new_owner } ->
        validate_transfer_ownership(mailbox_datum, new_owner, tx, own_ref)

      MigrateMailbox { new_address } -> {
        validate_migrate(
          mailbox_datum.owner,
          new_address,
          tx,
          own_ref,
          mailbox_datum.version,
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
  let ism_config =
    get_recipient_ism(message.recipient, datum.default_ism, ism_nft_policy, tx)
  expect verify_ism_for_message(ism_config, message, message_id, tx, metadata)

  // 5. For script-hash recipients (0x02 prefix), verify verified message delivery.
  //    NFT-policy recipients (0x01 prefix, e.g. warp routes) handle their own
  //    validation as spent inputs in the same TX.
  expect bytearray.length(message.recipient) == 32
  let recipient_prefix = bytearray.at(message.recipient, 0)
  expect
    if recipient_prefix == 1 {
      warp_receive_matches(message, message_id, tx)
    } else if recipient_prefix == 2 {
      verified_nft_minted(message_id, tx, verified_message_nft_policy) && verified_message_delivered(
        message,
        message_id,
        tx,
        verified_message_nft_policy,
      )
    } else {
      True
    }

  // 6. Validate mailbox continuation with updated processed_tree_root
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
      // Only migration bumps the version.
      cont_datum.version == new_datum.version,
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
  expect Some(recipient_hash) =
    hyperlane_address_to_script_hash(message.recipient)

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
        assets.quantity_of(
          output.value,
          verified_message_nft_policy,
          message_id,
        ) == 1

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

/// Require an NFT-addressed recipient to spend its state UTXO with the exact
/// ReceiveTransfer redeemer. This prevents a standalone mailbox Process from
/// consuming the replay-nullifier before the warp route delivers the transfer.
fn warp_receive_matches(
  message: Message,
  message_id: ByteArray,
  tx: Transaction,
) -> Bool {
  expect Some(policy_id) = hyperlane_address_to_policy_id(message.recipient)
  expect Some(warp_input) =
    list.find(
      tx.inputs,
      fn(input) { assets.quantity_of(input.output.value, policy_id, #"") == 1 },
    )
  expect Some(raw_redeemer) =
    pairs.get_first(tx.redeemers, Spend(warp_input.output_reference))
  expect warp_redeemer: WarpRouteRedeemer = raw_redeemer

  when warp_redeemer is {
    ReceiveTransfer { message: warp_message, message_id: warp_message_id } ->
      warp_message == message && warp_message_id == message_id
    _ -> False
  }
}

fn get_sender_address(
  tx: Transaction,
  sender_ref: OutputReference,
) -> HyperlaneAddress {
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
  default_ism_nft_policy: PolicyId,
  tx: Transaction,
) -> IsmConfig {
  let default_config =
    IsmConfig {
      script_hash: default_ism,
      state_nft_policy: default_ism_nft_policy,
    }
  let prefix = bytearray.at(recipient, 0)
  // Bytes 4..31 carry either the NFT policy (0x01) or script hash (0x02)
  let addr_payload = bytearray.drop(recipient, 4)
  if prefix == 1 {
    // Warp routes: ISM override is in the state UTXO datum (spent input)
    when find_warp_route_ism(addr_payload, tx.inputs) is {
      Some(ism) -> ism
      None -> default_config
    }
  } else if prefix == 2 {
    // Generic script recipients: ISM override is in the authenticated config
    // UTXO provided as a reference input at the recipient's script address.
    when find_generic_recipient_ism(addr_payload, tx.reference_inputs) is {
      Some(ism) -> ism
      None -> default_config
    }
  } else {
    default_config
  }
}

fn find_warp_route_ism(
  policy_id: PolicyId,
  inputs: List<Input>,
) -> Option<IsmConfig> {
  let maybe_input =
    list.find(
      inputs,
      fn(input) { assets.quantity_of(input.output.value, policy_id, #"") == 1 },
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
/// "hyperlane-config" token and parse its datum as Option<IsmConfig>.
///
/// The "hyperlane-config" token requirement prevents an attacker from planting a
/// plain datum at the recipient's address: minting requires the recipient script
/// to run with a constructor-0 (Init) redeemer, which only the owner can produce.
fn find_generic_recipient_ism(
  script_hash: ScriptHash,
  reference_inputs: List<Input>,
) -> Option<IsmConfig> {
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
              let (p, name, qty) = item
              p == canonical_config_nft_policy && name == script_hash && qty > 0
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
          expect ism_override: Option<IsmConfig> = datum_data
          ism_override
        }
        _ -> None
      }
  }
}

fn verify_ism_for_message(
  ism_config: IsmConfig,
  message: Message,
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
          if hash == ism_config.script_hash {
            // Every stateful ISM must be authenticated by the canonical state NFT.
            // Script hash alone authenticates code, not the datum-held validator set.
            assets.quantity_of(
              input.output.value,
              ism_config.state_nft_policy,
              #"49534d205374617465",
            ) == 1 && verify_ism_redeemer(input, message, message_id, tx)
          } else {
            False
          }
        _ -> False
      }
    },
  )
}

/// Read the ISM's flavour from its (state-NFT-authenticated) state datum.
fn ism_module_type(ism_input: Input) -> ModuleType {
  when ism_input.output.datum is {
    InlineDatum(datum_data) -> {
      expect ism_datum: MultisigIsmDatum = datum_data
      ism_datum.module_type
    }
    _ -> fail
  }
}

/// Bind the delivered message to the ISM's verification, dispatching on the
/// ISM's declared module_type:
/// - MessageId: the signed checkpoint names the message (checkpoint.message_id).
/// - MerkleRoot: the redeemer carries the proven leaf (delivered_message_id), and
///   the checkpoint's origin must be the message's origin (validator set binding).
/// The ISM script itself enforces the crypto (signatures, and for MerkleRoot the
/// inclusion proof); a wrong module_type only makes the ISM spend fail.
fn verify_ism_redeemer(
  ism_input: Input,
  message: Message,
  expected_message_id: ByteArray,
  tx: Transaction,
) -> Bool {
  when pairs.get_first(tx.redeemers, Spend(ism_input.output_reference)) is {
    Some(redeemer_data) ->
      when ism_module_type(ism_input) is {
        MessageIdMultisig -> {
          expect ism_redeemer: MultisigIsmRedeemer = redeemer_data
          when ism_redeemer is {
            Verify { checkpoint, .. } ->
              checkpoint.message_id == expected_message_id
            _ -> False
          }
        }
        MerkleRootMultisig -> {
          expect ism_redeemer: MerkleRootIsmRedeemer = redeemer_data
          when ism_redeemer is {
            MerkleRootVerify { delivered_message_id, checkpoint, .. } ->
              delivered_message_id == expected_message_id && checkpoint.origin == message.origin
            _ -> False
          }
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

// ============================================================================
// Tests
// ============================================================================

// 28-byte (56 hex chars) constants for ScriptHash / PolicyId
const test_default_ism =
  #"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa00"

const test_override_ism =
  #"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"

const test_warp_policy =
  #"cccccccccccccccccccccccccccccccccccccccccccccccccccccccc"

// 32-byte Hyperlane address: 0x01 prefix + 3 zero bytes + 28-byte policy id
const test_warp_recipient =
  #"01000000cccccccccccccccccccccccccccccccccccccccccccccccccccccccc"

// 32-byte Hyperlane address: 0x02 prefix + 4 zero bytes + 28-byte script hash
const test_generic_recipient =
  #"020000001234567890abcdef1234567890abcdef1234567890abcdef12345678"

fn unbounded_validity_range() -> Interval<Int> {
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

fn make_warp_input(ism_override: Option<IsmConfig>) -> Input {
  let datum =
    WarpRouteDatum { version: 0, 
      config: WarpRouteConfig {
        token_type: Native,
        decimals: 6,
        remote_decimals: 18,
        remote_routes: [],
      },
      owner: #"00000000000000000000000000000000000000000000000000000000",
      total_bridged: 0,
      ism: ism_override,
      destination_gas: [],
    }

  Input {
    output_reference: test_warp_output_ref(),
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

fn test_warp_output_ref() -> OutputReference {
  OutputReference {
    transaction_id: #"0000000000000000000000000000000000000000000000000000000000000001",
    output_index: 0,
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

// Use the real policy so tests exercise the actual check
const test_canonical_policy = canonical_config_nft_policy

fn make_generic_ism_config_input(ism_override: Option<IsmConfig>) -> Input {
  let datum: Option<IsmConfig> = ism_override

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
      value: assets.from_asset(
        test_canonical_policy,
        test_generic_script_hash,
        1,
      ),
      datum: InlineDatum(datum),
      reference_script: None,
    },
  }
}

test get_recipient_ism_generic_no_ref_input_uses_default() {
  let tx = make_tx([], [])
  get_recipient_ism(
    test_generic_recipient,
    test_default_ism,
    test_ism_nft_policy,
    tx,
  ) == test_default_ism_config()
}

test get_recipient_ism_generic_no_override_uses_default() {
  let ref_input = make_generic_ism_config_input(None)
  let tx = make_tx([], [ref_input])
  get_recipient_ism(
    test_generic_recipient,
    test_default_ism,
    test_ism_nft_policy,
    tx,
  ) == test_default_ism_config()
}

test get_recipient_ism_generic_with_override_uses_override() {
  let ref_input =
    make_generic_ism_config_input(Some(test_override_ism_config()))
  let tx = make_tx([], [ref_input])
  get_recipient_ism(
    test_generic_recipient,
    test_default_ism,
    test_ism_nft_policy,
    tx,
  ) == test_override_ism_config()
}

test get_recipient_ism_warp_no_override_uses_default() {
  let tx = make_tx([make_warp_input(None)], [])
  get_recipient_ism(
    test_warp_recipient,
    test_default_ism,
    test_ism_nft_policy,
    tx,
  ) == test_default_ism_config()
}

test get_recipient_ism_warp_with_override_uses_override() {
  let tx = make_tx([make_warp_input(Some(test_override_ism_config()))], [])
  get_recipient_ism(
    test_warp_recipient,
    test_default_ism,
    test_ism_nft_policy,
    tx,
  ) == test_override_ism_config()
}

const test_ism_nft_policy =
  #"dddddddddddddddddddddddddddddddddddddddddddddddddddddddd"

const test_message_id =
  #"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"

fn test_default_ism_config() -> IsmConfig {
  IsmConfig {
    script_hash: test_default_ism,
    state_nft_policy: test_ism_nft_policy,
  }
}

fn test_override_ism_config() -> IsmConfig {
  IsmConfig {
    script_hash: test_override_ism,
    state_nft_policy: test_ism_nft_policy,
  }
}

fn test_inbound_warp_message() -> Message {
  Message {
    version: 3,
    nonce: 1,
    origin: 10,
    sender: #"1111111111111111111111111111111111111111111111111111111111111111",
    destination: 20,
    recipient: test_warp_recipient,
    body: #"abcd",
  }
}

test warp_recipient_requires_matching_receive_redeemer() {
  let message = test_inbound_warp_message()
  let warp_redeemer: WarpRouteRedeemer =
    ReceiveTransfer { message, message_id: test_message_id }
  let raw_redeemer: Data = warp_redeemer
  let base_tx = make_tx([make_warp_input(None)], [])
  let tx =
    Transaction {
      ..base_tx,
      redeemers: [Pair(Spend(test_warp_output_ref()), raw_redeemer)],
    }

  warp_receive_matches(message, test_message_id, tx)
}

test standalone_process_cannot_consume_warp_message() fail {
  warp_receive_matches(
    test_inbound_warp_message(),
    test_message_id,
    make_tx([make_warp_input(None)], []),
  )
}

fn make_ism_verify_tx(has_state_nft: Bool) -> Transaction {
  let state_asset_name =
    if has_state_nft {
      #"49534d205374617465"
    } else {
      #""
    }
  make_ism_verify_tx_with_asset(state_asset_name)
}

fn make_ism_verify_tx_with_asset(state_asset_name: ByteArray) -> Transaction {
  let ism_ref =
    OutputReference {
      transaction_id: #"0000000000000000000000000000000000000000000000000000000000000003",
      output_index: 0,
    }
  let value =
    if bytearray.length(state_asset_name) > 0 {
      assets.from_asset(test_ism_nft_policy, state_asset_name, 1)
    } else {
      assets.zero
    }
  let checkpoint =
    Checkpoint {
      origin: 1,
      merkle_root: #"0000000000000000000000000000000000000000000000000000000000000000",
      origin_merkle_tree_hook: #"0000000000000000000000000000000000000000000000000000000000000000",
      merkle_index: 0,
      message_id: test_message_id,
    }
  let ism_redeemer = Verify { checkpoint, validator_signatures: [] }
  let raw_redeemer: Data = ism_redeemer
  Transaction {
    inputs: [
      Input {
        output_reference: ism_ref,
        output: Output {
          address: Address {
            payment_credential: Script(test_override_ism),
            stake_credential: None,
          },
          value,
          datum: InlineDatum(
            MultisigIsmDatum { version: 0, 
              validators: [],
              thresholds: [],
              owner: #"00000000000000000000000000000000000000000000000000000000",
              module_type: MessageIdMultisig,
            },
          ),
          reference_script: None,
        },
      },
    ],
    reference_inputs: [],
    outputs: [],
    fee: 0,
    mint: assets.zero,
    certificates: [],
    withdrawals: [],
    validity_range: unbounded_validity_range(),
    extra_signatories: [],
    redeemers: [Pair(Spend(ism_ref), raw_redeemer)],
    datums: dict.empty,
    id: #"0000",
    votes: [],
    proposal_procedures: [],
    current_treasury_amount: None,
    treasury_donation: None,
  }
}

fn test_ism_message() -> Message {
  Message {
    version: 3,
    nonce: 0,
    origin: 1,
    sender: #"0000000000000000000000000000000000000000000000000000000000000000",
    destination: 2003,
    recipient: test_generic_recipient,
    body: #"",
  }
}

test override_ism_requires_canonical_state_nft() {
  !verify_ism_for_message(
    test_override_ism_config(),
    test_ism_message(),
    test_message_id,
    make_ism_verify_tx(False),
    #"01",
  )
}

test authenticated_migrated_ism_state_is_accepted() {
  verify_ism_for_message(
    test_override_ism_config(),
    test_ism_message(),
    test_message_id,
    make_ism_verify_tx(True),
    #"01",
  )
}

test ism_reference_token_does_not_authenticate_state() {
  !verify_ism_for_message(
    test_override_ism_config(),
    test_ism_message(),
    test_message_id,
    make_ism_verify_tx_with_asset(#"726566"),
    #"01",
  )
}

// Exercises the mailbox's MerkleRoot dispatch branch. The mailbox does NOT run
// the crypto (the ISM script does); it binds delivery on the redeemer's
// delivered_message_id and checkpoint.origin, which is what these test.
fn make_merkleroot_ism_tx(
  delivered_id: ByteArray,
  checkpoint_origin: Domain,
) -> Transaction {
  let ism_ref =
    OutputReference {
      transaction_id: #"0000000000000000000000000000000000000000000000000000000000000004",
      output_index: 0,
    }
  let checkpoint =
    Checkpoint {
      origin: checkpoint_origin,
      merkle_root: #"0000000000000000000000000000000000000000000000000000000000000000",
      origin_merkle_tree_hook: #"0000000000000000000000000000000000000000000000000000000000000000",
      merkle_index: 0,
      message_id: #"0000000000000000000000000000000000000000000000000000000000000000",
    }
  let ism_redeemer =
    MerkleRootVerify {
      checkpoint,
      validator_signatures: [],
      delivered_message_id: delivered_id,
      delivered_index: 0,
      merkle_proof: [],
    }
  let raw_redeemer: Data = ism_redeemer
  Transaction {
    inputs: [
      Input {
        output_reference: ism_ref,
        output: Output {
          address: Address {
            payment_credential: Script(test_override_ism),
            stake_credential: None,
          },
          value: assets.from_asset(
            test_ism_nft_policy,
            #"49534d205374617465",
            1,
          ),
          datum: InlineDatum(
            MultisigIsmDatum { version: 0, 
              validators: [],
              thresholds: [],
              owner: #"00000000000000000000000000000000000000000000000000000000",
              module_type: MerkleRootMultisig,
            },
          ),
          reference_script: None,
        },
      },
    ],
    reference_inputs: [],
    outputs: [],
    fee: 0,
    mint: assets.zero,
    certificates: [],
    withdrawals: [],
    validity_range: unbounded_validity_range(),
    extra_signatories: [],
    redeemers: [Pair(Spend(ism_ref), raw_redeemer)],
    datums: dict.empty,
    id: #"0000",
    votes: [],
    proposal_procedures: [],
    current_treasury_amount: None,
    treasury_donation: None,
  }
}

test merkleroot_binding_accepts_matching_delivered_id_and_origin() {
  verify_ism_for_message(
    test_override_ism_config(),
    test_ism_message(),
    test_message_id,
    make_merkleroot_ism_tx(test_message_id, 1),
    #"01",
  )
}

test merkleroot_binding_rejects_wrong_delivered_id() {
  !verify_ism_for_message(
    test_override_ism_config(),
    test_ism_message(),
    test_message_id,
    make_merkleroot_ism_tx(
      #"1212121212121212121212121212121212121212121212121212121212121212",
      1,
    ),
    #"01",
  )
}

test merkleroot_binding_rejects_wrong_origin() {
  !verify_ism_for_message(
    test_override_ism_config(),
    test_ism_message(),
    test_message_id,
    make_merkleroot_ism_tx(test_message_id, 999),
    #"01",
  )
}

// ============================================================================
// Sender derivation
//
// The mailbox derives the sender from an input the caller is spending rather
// than taking it as a parameter, so it cannot be forged: spending that input
// already requires satisfying its credential. These pin the three address
// forms a dispatcher can take.
// ============================================================================

const test_sender_ref =
  OutputReference {
    transaction_id: #"0000000000000000000000000000000000000000000000000000000000000042",
    output_index: 0,
  }

const test_wallet_pkh = #"11111111111111111111111111111111111111111111111111111111"

const test_bare_script_hash =
  #"22222222222222222222222222222222222222222222222222222222"

const test_sender_nft_policy =
  #"33333333333333333333333333333333333333333333333333333333"

fn sender_input(credential: Credential, value: assets.Value) -> Input {
  Input {
    output_reference: test_sender_ref,
    output: Output {
      address: Address { payment_credential: credential, stake_credential: None },
      value,
      datum: NoDatum,
      reference_script: None,
    },
  }
}

fn sender_tx(input: Input) -> Transaction {
  make_tx([input], [])
}

/// A plain wallet dispatching is stamped 0x00000000 || key hash.
test wallet_sender_is_prefixed_key_hash() {
  let tx =
    sender_tx(
      sender_input(
        Credential.VerificationKey(test_wallet_pkh),
        assets.from_lovelace(10000000),
      ),
    )
  get_sender_address(tx, test_sender_ref) == bytearray.concat(
    #"00000000",
    test_wallet_pkh,
  )
}

/// A script with no state NFT is stamped 0x02000000 || script hash, the form a
/// generic recipient like the greeting is addressed by.
test bare_script_sender_is_prefixed_script_hash() {
  let tx =
    sender_tx(
      sender_input(Script(test_bare_script_hash), assets.from_lovelace(10000000)),
    )
  get_sender_address(tx, test_sender_ref) == bytearray.concat(
    #"02000000",
    test_bare_script_hash,
  )
}

/// A script carrying a state NFT is stamped 0x01000000 || policy, which is how
/// a warp route identifies itself to its counterpart.
test script_with_state_nft_sender_is_prefixed_policy() {
  let value =
    assets.merge(
      assets.from_lovelace(10000000),
      assets.from_asset(test_sender_nft_policy, #"", 1),
    )
  let tx = sender_tx(sender_input(Script(test_bare_script_hash), value))
  get_sender_address(tx, test_sender_ref) == bytearray.concat(
    #"01000000",
    test_sender_nft_policy,
  )
}

/// Two dispatchers cannot collide: distinct wallets produce distinct senders.
test distinct_wallets_produce_distinct_senders() {
  let a =
    sender_tx(
      sender_input(
        Credential.VerificationKey(test_wallet_pkh),
        assets.from_lovelace(10000000),
      ),
    )
  let b =
    sender_tx(
      sender_input(
        Credential.VerificationKey(test_bare_script_hash),
        assets.from_lovelace(10000000),
      ),
    )
  get_sender_address(a, test_sender_ref) != get_sender_address(b, test_sender_ref)
}

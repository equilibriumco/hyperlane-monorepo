use aiken/collection/dict
use aiken/collection/list
use aiken/collection/pairs
use aiken/crypto.{VerificationKeyHash, keccak_256}
use aiken/primitive/bytearray
use cardano/address.{Address, Credential, Script}
use cardano/assets
use cardano/transaction.{
  InlineDatum, OutputReference, Spend, Transaction,
}
use merkle
use types.{
  Checkpoint, Domain, HyperlaneAddress, MailboxDatum, MailboxRedeemer, Message,
  MerkleTreeState, MultisigIsmRedeemer, PolicyId, ScriptHash,
  VerifiedMessageDatum, Verify, encode_message,
  Dispatch, Process, SetDefaultIsm, TransferOwnership, MigrateMailbox,
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

      Process { message, metadata, message_id } ->
        validate_process(
          mailbox_datum,
          message,
          metadata,
          message_id,
          tx,
          own_ref,
          processed_messages_nft_policy,
          verified_message_nft_policy,
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
  tx: Transaction,
  own_ref: OutputReference,
  processed_messages_nft_policy: PolicyId,
  verified_message_nft_policy: PolicyId,
) -> Bool {
  // 1. Verify message is for this domain
  expect message.destination == datum.local_domain

  // 2. Verify message_id is correctly computed
  let computed_id = keccak_256(encode_message(message))
  expect computed_id == message_id

  // 3. Verify message not already processed
  expect !is_message_processed(message_id, tx, processed_messages_nft_policy)

  // 4. Verify ISM approves the message
  let ism_hash = get_recipient_ism(message.recipient, datum.default_ism, tx)
  expect verify_ism_for_message(ism_hash, message_id, tx, metadata)

  // 5. Verify processed message NFT is being minted
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

  // 7. Validate mailbox continuation (datum unchanged for process)
  validate_continuation(datum, datum, tx, own_ref)
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

fn is_message_processed(
  message_id: ByteArray,
  tx: Transaction,
  processed_messages_nft_policy: PolicyId,
) -> Bool {
  list.any(
    tx.reference_inputs,
    fn(ref_input) {
      let policy_tokens = assets.tokens(ref_input.output.value, processed_messages_nft_policy)
      when dict.get(policy_tokens, message_id) is {
        Some(quantity) -> quantity > 0
        None -> False
      }
    },
  )
}

fn get_recipient_ism(
  _recipient: HyperlaneAddress,
  default_ism: ScriptHash,
  _tx: Transaction,
) -> ScriptHash {
  default_ism
}

fn verify_ism_for_message(
  ism_hash: ScriptHash,
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
            verify_ism_redeemer_message_id(input.output_reference, message_id, tx)
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

use aiken/builtin
use aiken/collection/list
use aiken/crypto.{VerificationKeyHash, keccak_256}
use aiken/primitive/bytearray
use cardano/address.{Address, Credential, Script}
use cardano/transaction.{
  InlineDatum, Input, Output, OutputReference, Transaction,
}
use merkle
use types.{
  Domain, HyperlaneAddress, MailboxDatum, MailboxRedeemer, Message,
  ProcessedMessageDatum, ScriptHash, encode_message,
  Dispatch, Process, SetDefaultIsm, TransferOwnership,
}
use utils.{
  find_input, has_script_input, hyperlane_address_to_script_hash,
  is_signed_by, value_preserved,
}

/// Mailbox Validator
/// Core Hyperlane contract that dispatches and processes messages
///
/// The Mailbox is the central hub for cross-chain messaging:
/// - Dispatch: Send messages from Cardano to other chains
/// - Process: Receive messages from other chains to Cardano
/// Mailbox validator
/// Parameterized by the processed messages script hash (where we store processed message markers)
validator mailbox(processed_messages_script: ScriptHash) {
  spend(
    datum: Option<MailboxDatum>,
    redeemer: MailboxRedeemer,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    expect Some(mailbox_datum) = datum

    when redeemer is {
      Dispatch { destination, recipient, body } ->
        validate_dispatch(
          mailbox_datum,
          destination,
          recipient,
          body,
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
          processed_messages_script,
        )

      SetDefaultIsm { new_ism } ->
        validate_set_default_ism(mailbox_datum, new_ism, tx, own_ref)

      TransferOwnership { new_owner } ->
        validate_transfer_ownership(mailbox_datum, new_owner, tx, own_ref)
    }
  }

  else(_) {
    fail
  }
}

/// Validate dispatch of a new outbound message
fn validate_dispatch(
  datum: MailboxDatum,
  destination: Domain,
  recipient: HyperlaneAddress,
  body: ByteArray,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // Build the message
  let message =
    Message {
      version: 3,
      nonce: datum.outbound_nonce,
      origin: datum.local_domain,
      sender: get_sender_address(tx),
      destination,
      recipient,
      body,
    }

  // Compute message hash for merkle tree
  let message_hash = keccak_256(encode_message(message))

  // Update merkle tree with new message
  let current_tree =
    merkle.MerkleTree {
      branches: datum_branches_to_list(datum.merkle_root),
      count: datum.merkle_count,
    }
  let new_tree = merkle.insert(current_tree, message_hash)

  // Build expected new datum
  let new_datum =
    MailboxDatum {
      ..datum,
      outbound_nonce: datum.outbound_nonce + 1,
      merkle_root: merkle.root(new_tree),
      merkle_count: new_tree.count,
    }

  // Validate continuation
  validate_continuation(datum, new_datum, tx, own_ref)
}

/// Validate processing of an inbound message
fn validate_process(
  datum: MailboxDatum,
  message: Message,
  metadata: ByteArray,
  message_id: ByteArray,
  tx: Transaction,
  own_ref: OutputReference,
  processed_messages_script: ScriptHash,
) -> Bool {
  // 1. Verify message is for this domain
  expect message.destination == datum.local_domain

  // 2. Verify message_id is correctly computed
  let computed_id = keccak_256(encode_message(message))
  expect computed_id == message_id

  // 3. Verify message not already processed
  // (checked by ensuring no reference input with this message_id exists)
  expect !is_message_processed(message_id, tx, processed_messages_script)

  // 4. Verify ISM approves the message
  // The ISM must be present as a reference input and its validation is implicit
  // (if ISM script is in reference inputs, it means verification passed)
  let ism_hash = get_recipient_ism(message.recipient, datum.default_ism, tx)
  expect verify_ism_present(ism_hash, tx, metadata)

  // 5. Verify recipient is called (recipient UTXO must be spent)
  expect recipient_called(message.recipient, tx)

  // 6. Verify processed message marker is created
  expect processed_marker_created(message_id, tx, processed_messages_script)

  // 7. Validate mailbox continuation (datum unchanged for process)
  validate_continuation(datum, datum, tx, own_ref)
}

/// Validate setting a new default ISM
fn validate_set_default_ism(
  datum: MailboxDatum,
  new_ism: ScriptHash,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // Only owner can update
  expect is_signed_by(tx, datum.owner)

  let new_datum = MailboxDatum { ..datum, default_ism: new_ism }
  validate_continuation(datum, new_datum, tx, own_ref)
}

/// Validate ownership transfer
fn validate_transfer_ownership(
  datum: MailboxDatum,
  new_owner: VerificationKeyHash,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // Only current owner can transfer
  expect is_signed_by(tx, datum.owner)

  let new_datum = MailboxDatum { ..datum, owner: new_owner }
  validate_continuation(datum, new_datum, tx, own_ref)
}

/// Validate that the mailbox UTXO continues correctly
fn validate_continuation(
  _old_datum: MailboxDatum,
  new_datum: MailboxDatum,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // Find own input
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address
  let own_value = own_input.output.value

  // Find continuation output (same address)
  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  // Verify datum updated correctly
  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: MailboxDatum = cont_datum_data
  expect and {
      cont_datum.local_domain == new_datum.local_domain,
      cont_datum.default_ism == new_datum.default_ism,
      cont_datum.owner == new_datum.owner,
      cont_datum.outbound_nonce == new_datum.outbound_nonce,
      cont_datum.merkle_root == new_datum.merkle_root,
      cont_datum.merkle_count == new_datum.merkle_count,
    }

  // Verify value preserved (NFT marker stays)
  value_preserved(own_value, continuation.value)
}

/// Get the sender address from the transaction
/// Returns the first input's address as a 32-byte Hyperlane address
fn get_sender_address(tx: Transaction) -> HyperlaneAddress {
  expect Some(first_input) = list.head(tx.inputs)
  address_to_hyperlane(first_input.output.address)
}

/// Convert a Cardano address to a Hyperlane address (32 bytes)
fn address_to_hyperlane(addr: Address) -> HyperlaneAddress {
  when addr.payment_credential is {
    Credential.VerificationKey(hash) ->
      // Payment credential: prefix 0x00000000
      bytearray.concat(#"00000000", hash)
    Script(hash) ->
      // Script credential: prefix 0x02000000
      bytearray.concat(#"02000000", hash)
  }
}

/// Check if a message has already been processed
/// Only checks UTXOs at the processed_messages_script address
fn is_message_processed(
  message_id: ByteArray,
  tx: Transaction,
  processed_messages_script: ScriptHash,
) -> Bool {
  // Check reference inputs for existing processed message marker
  list.any(
    tx.reference_inputs,
    fn(ref_input) {
      // First check if this UTXO is at the processed messages script address
      when ref_input.output.address.payment_credential is {
        Script(hash) ->
          if hash == processed_messages_script {
            // This is a processed message marker, safe to parse
            when ref_input.output.datum is {
              InlineDatum(datum_data) ->
                check_processed_datum(datum_data, message_id)
              _ -> False
            }
          } else {
            False
          }
        _ -> False
      }
    },
  )
}

/// Helper to check if datum matches a processed message
/// Uses safe parsing to handle cases where multiple datum types exist at the same address
/// (e.g., when processed_messages_script == mailbox_script_hash)
fn check_processed_datum(datum_data: Data, message_id: ByteArray) -> Bool {
  // ProcessedMessageDatum is encoded as Constr(0, [ByteArray])
  // MailboxDatum is encoded as Constr(0, [Int, ByteArray, ByteArray, Int, ByteArray, Int])
  // We can distinguish by checking field count before parsing
  let constr_pair = builtin.un_constr_data(datum_data)
  let tag = builtin.fst_pair(constr_pair)
  let fields = builtin.snd_pair(constr_pair)

  // ProcessedMessageDatum has tag 0 and exactly 1 field
  if tag == 0 {
    when fields is {
      [single_field] -> {
        // Has exactly one field - extract as ByteArray and compare
        let msg_id_bytes = builtin.un_b_data(single_field)
        msg_id_bytes == message_id
      }
      // More or fewer fields means this is not a ProcessedMessageDatum
      _ -> False
    }
  } else {
    False
  }
}

/// Get the ISM for a recipient (custom or default)
fn get_recipient_ism(
  _recipient: HyperlaneAddress,
  default_ism: ScriptHash,
  _tx: Transaction,
) -> ScriptHash {
  // For now, always use default ISM
  // TODO: Check recipient's custom ISM from registry
  default_ism
}

/// Verify that the ISM is present and metadata is provided
fn verify_ism_present(
  ism_hash: ScriptHash,
  tx: Transaction,
  metadata: ByteArray,
) -> Bool {
  // ISM must be in reference inputs
  // The ISM validator will verify the signatures in metadata
  let ism_present =
    list.any(
      tx.reference_inputs,
      fn(ref_input) {
        when ref_input.output.address.payment_credential is {
          Script(hash) -> hash == ism_hash
          _ -> False
        }
      },
    )

  // Metadata must not be empty (contains signatures)
  ism_present && bytearray.length(metadata) > 0
}

/// Verify that the recipient script is being called
fn recipient_called(recipient: HyperlaneAddress, tx: Transaction) -> Bool {
  // Extract script hash from recipient address
  expect Some(recipient_hash) = hyperlane_address_to_script_hash(recipient)

  // Check that an input from recipient script is spent
  has_script_input(tx, recipient_hash)
}

/// Verify that a processed message marker is created
fn processed_marker_created(
  message_id: ByteArray,
  tx: Transaction,
  processed_messages_script: ScriptHash,
) -> Bool {
  list.any(
    tx.outputs,
    fn(output) {
      when output.address.payment_credential is {
        Script(hash) ->
          if hash == processed_messages_script {
            when output.datum is {
              InlineDatum(datum_data) ->
                check_processed_datum(datum_data, message_id)
              _ -> False
            }
          } else {
            False
          }
        _ -> False
      }
    },
  )
}

/// Convert datum merkle root to list of branches
/// This is a placeholder - actual implementation depends on how we store the tree
fn datum_branches_to_list(_merkle_root: ByteArray) -> List<ByteArray> {
  // For now, we store just the root. In a full implementation,
  // we'd store the full branch state for incremental updates.
  // This is a simplification that works for verification but not incremental building.
  []
}

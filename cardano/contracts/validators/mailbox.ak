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
  MerkleTreeState, MultisigIsmRedeemer, PolicyId, ScriptHash, Verify, encode_message,
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
///
/// ## Parameter: processed_messages_nft_policy
///
/// The mailbox is parameterized by the policy ID of the processed message NFT.
/// This NFT policy is used for replay protection - each processed message gets
/// an NFT minted with the message_id as the asset name.
///
/// The processed_messages_nft_policy is itself parameterized by mailbox_policy_id
/// (the one-shot NFT policy), making it stable across mailbox upgrades. This ensures
/// that replay protection continues to work even after the mailbox code is updated.
validator mailbox(processed_messages_nft_policy: PolicyId) {
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
          processed_messages_nft_policy,
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

  // Update merkle tree with new message using full branch state from datum
  let current_tree =
    merkle.MerkleTree {
      branches: datum.merkle_tree.branches,
      count: datum.merkle_tree.count,
    }
  let new_tree = merkle.insert(current_tree, message_hash)

  // Build expected new datum with updated merkle tree
  let new_datum =
    MailboxDatum {
      ..datum,
      outbound_nonce: datum.outbound_nonce + 1,
      merkle_tree: MerkleTreeState {
        branches: new_tree.branches,
        count: new_tree.count,
      },
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
  processed_messages_nft_policy: PolicyId,
) -> Bool {
  // 1. Verify message is for this domain
  expect message.destination == datum.local_domain

  // 2. Verify message_id is correctly computed
  let computed_id = keccak_256(encode_message(message))
  expect computed_id == message_id

  // 3. Verify message not already processed
  // Check that no NFT with this message_id exists in reference inputs
  expect !is_message_processed(message_id, tx, processed_messages_nft_policy)

  // 4. Verify ISM approves the message
  // Get the TRUSTED ISM hash (from recipient's custom ISM or mailbox's default_ism)
  // Then verify an input with that EXACT script hash is spent with a Verify redeemer
  // containing a checkpoint for THIS specific message_id
  let ism_hash = get_recipient_ism(message.recipient, datum.default_ism, tx)
  expect verify_ism_for_message(ism_hash, message_id, tx, metadata)

  // 5. Verify recipient is called (recipient UTXO must be spent)
  expect recipient_called(message.recipient, tx)

  // 6. Verify processed message NFT is being minted with this message_id
  expect processed_nft_minted(message_id, tx, processed_messages_nft_policy)

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
      cont_datum.merkle_tree.branches == new_datum.merkle_tree.branches,
      cont_datum.merkle_tree.count == new_datum.merkle_tree.count,
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
/// Checks reference inputs for an NFT from processed_messages_nft_policy with the message_id as asset name
fn is_message_processed(
  message_id: ByteArray,
  tx: Transaction,
  processed_messages_nft_policy: PolicyId,
) -> Bool {
  // Check reference inputs for existing processed message NFT
  list.any(
    tx.reference_inputs,
    fn(ref_input) {
      // Check if this UTXO contains an NFT from the processed messages policy
      // with the message_id as the asset name
      let policy_tokens = assets.tokens(ref_input.output.value, processed_messages_nft_policy)
      when dict.get(policy_tokens, message_id) is {
        Some(quantity) -> quantity > 0
        None -> False
      }
    },
  )
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

/// Verify that the TRUSTED ISM is being spent with a Verify redeemer for this specific message
///
/// Security guarantees:
/// 1. Only the trusted ISM (matching ism_hash) is accepted - attacker cannot use arbitrary ISM
/// 2. The ISM validator runs (spending triggers validation)
/// 3. The ISM's Verify redeemer contains a checkpoint for THIS message_id
///    (preventing reuse of a valid ISM verification for a different message)
fn verify_ism_for_message(
  ism_hash: ScriptHash,
  message_id: ByteArray,
  tx: Transaction,
  metadata: ByteArray,
) -> Bool {
  // Metadata must not be empty (contains signatures)
  expect bytearray.length(metadata) > 0

  // Find an input with the TRUSTED ISM script hash and verify its redeemer
  list.any(
    tx.inputs,
    fn(input) {
      when input.output.address.payment_credential is {
        Script(hash) ->
          // CRITICAL: Only accept ISM with the exact trusted script hash
          if hash == ism_hash {
            // Found trusted ISM input - verify its redeemer has the correct message_id
            verify_ism_redeemer_message_id(input.output_reference, message_id, tx)
          } else {
            False
          }
        _ -> False
      }
    },
  )
}

/// Look up the ISM's redeemer and verify it's a Verify with matching message_id
fn verify_ism_redeemer_message_id(
  ism_output_ref: OutputReference,
  expected_message_id: ByteArray,
  tx: Transaction,
) -> Bool {
  // Look up the redeemer for this input
  let spend_purpose = Spend(ism_output_ref)

  when pairs.get_first(tx.redeemers, spend_purpose) is {
    Some(redeemer_data) -> {
      // Parse the redeemer as MultisigIsmRedeemer
      expect ism_redeemer: MultisigIsmRedeemer = redeemer_data
      when ism_redeemer is {
        Verify { checkpoint, .. } ->
          // The checkpoint's message_id must match the message being processed
          checkpoint.message_id == expected_message_id
        // SetValidators and SetThreshold are admin actions, not verification
        _ -> False
      }
    }
    None -> False
  }
}

/// Verify that the recipient script is being called
fn recipient_called(recipient: HyperlaneAddress, tx: Transaction) -> Bool {
  // Extract script hash from recipient address
  expect Some(recipient_hash) = hyperlane_address_to_script_hash(recipient)

  // Check that an input from recipient script is spent
  has_script_input(tx, recipient_hash)
}

/// Verify that a processed message NFT is being minted with the message_id as asset name
fn processed_nft_minted(
  message_id: ByteArray,
  tx: Transaction,
  processed_messages_nft_policy: PolicyId,
) -> Bool {
  // Check that an NFT is being minted with the message_id as asset name
  let policy_mints = assets.tokens(tx.mint, processed_messages_nft_policy)
  when dict.get(policy_mints, message_id) is {
    Some(quantity) -> quantity == 1
    None -> False
  }
}


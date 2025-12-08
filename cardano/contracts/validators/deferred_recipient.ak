use aiken/collection/dict
use aiken/collection/list
use aiken/crypto.{keccak_256}
use aiken/primitive/bytearray
use cardano/address.{Address}
use cardano/transaction.{InlineDatum, OutputReference, Transaction}
use cardano/assets.{lovelace_of, tokens}
use types.{
  HyperlaneRecipientDatum, HyperlaneRecipientRedeemer, Message,
  PolicyId, HandleMessage, ContractAction, StoredMessageDatum,
  DeferredAction, ProcessStoredMessage, encode_message,
}
use utils.{find_input}

/// Deferred Recipient
///
/// A Hyperlane recipient that defers message processing to a separate step.
/// Use this pattern when:
/// 1. The relayer cannot know how to build complex outputs
/// 2. Processing needs to happen separately (manually, via dApp, automated service, etc.)
/// 3. You need complex business logic that requires custom output construction
///
/// ## Two-Phase Processing
///
/// **Phase 1 - Relayer stores message (HandleMessage):**
/// - Mailbox.Process verifies ISM
/// - Recipient.HandleMessage is called
/// - State UTXO continues UNCHANGED (or minimal change)
/// - NEW: Message UTXO created at recipient address with StoredMessageDatum
/// - NEW: Message NFT minted (proves legitimacy)
///
/// **Phase 2 - Process stored message (ProcessStoredMessage):**
/// - Query for UTXOs with message NFT policy
/// - Consume Message UTXO + State UTXO
/// - Build complex outputs based on business logic
/// - Message NFT is burned (prevents replay)
///
/// ## Security Model
///
/// The message NFT prevents fake message attacks:
/// - Anyone can create UTXOs at any address on Cardano
/// - Without NFT, attacker could create fake message UTXOs
/// - Message NFT policy ONLY allows minting when Mailbox is in inputs
/// - Mailbox only processes ISM-verified messages
/// - Therefore: NFT existence = message is legitimate
/// - Only process UTXOs containing the NFT
/// - NFT burn prevents replay attacks
///
/// ## State Structure
///
/// The inner state is minimal - just tracks message counts.
/// Complex state should be in the message UTXOs or built during processing.

/// Inner state for deferred recipient - intentionally minimal
pub type DeferredRecipientInner {
  /// Count of messages received (for tracking)
  messages_stored: Int,
  /// Count of messages processed
  messages_processed: Int,
}

/// Deferred Recipient validator
///
/// Parameters:
/// - `mailbox_policy_id`: NFT policy of the mailbox (for caller verification)
/// - `message_nft_policy`: Policy for message NFTs (from stored_message_nft.ak)
validator deferred_recipient(mailbox_policy_id: PolicyId, message_nft_policy: PolicyId) {
  spend(
    datum: Option<HyperlaneRecipientDatum<DeferredRecipientInner>>,
    redeemer: HyperlaneRecipientRedeemer<DeferredAction>,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    expect Some(recipient_datum) = datum

    when redeemer is {
      HandleMessage { message, message_id } ->
        handle_message(
          recipient_datum,
          message,
          message_id,
          tx,
          own_ref,
          mailbox_policy_id,
          message_nft_policy,
        )

      ContractAction { action } -> {
        let ProcessStoredMessage { message_id } = action
        process_stored_message(
          recipient_datum,
          message_id,
          tx,
          own_ref,
          message_nft_policy,
        )
      }
    }
  }

  else(_) {
    fail
  }
}

/// Handle an incoming Hyperlane message by storing it
///
/// This is called by the relayer when a message arrives.
/// Instead of processing immediately, we store the message on-chain.
///
/// SECURITY: We verify the message data matches the ISM-validated message_id
/// to prevent attackers from providing fake data.
fn handle_message(
  datum: HyperlaneRecipientDatum<DeferredRecipientInner>,
  message: Message,
  message_id: ByteArray,
  tx: Transaction,
  own_ref: OutputReference,
  mailbox_policy_id: PolicyId,
  message_nft_policy: PolicyId,
) -> Bool {
  // 1. CRITICAL SECURITY CHECK: Verify message data matches the ISM-validated message_id
  //
  // The ISM validates signatures on message_id = keccak256(encode_message(message)).
  // Without this check, an attacker could provide fake origin/sender/body data
  // while the ISM validates a different (legitimate) message.
  let computed_message_id = keccak_256(encode_message(message))
  expect computed_message_id == message_id

  // 2. Verify called by the REAL Mailbox (check for mailbox NFT)
  expect mailbox_is_caller(tx, mailbox_policy_id)

  // 3. Verify a message NFT is being minted WITH THE CORRECT MESSAGE_ID
  expect message_nft_minted_with_id(tx, message_nft_policy, message_id)

  // 4. Verify a message UTXO is being created with VERIFIED data
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address
  expect message_utxo_created(
    tx,
    own_address,
    message_nft_policy,
    message.origin,
    message.sender,
    message.body,
    message_id,
    message.nonce,
  )

  // 5. Update state (minimal change - just increment counter)
  let new_inner =
    DeferredRecipientInner {
      messages_stored: datum.inner.messages_stored + 1,
      messages_processed: datum.inner.messages_processed,
    }

  let new_datum =
    HyperlaneRecipientDatum {
      ism: datum.ism,
      last_processed_nonce: datum.last_processed_nonce,
      inner: new_inner,
    }

  // 6. Validate state continuation (datum updated, value preserved)
  validate_state_continuation(datum, new_datum, tx, own_ref)
}

/// Process a stored message
///
/// This is called when processing stored messages.
/// The caller has full control over outputs - we just verify:
/// - Message NFT is being burned
/// - State is updated correctly
fn process_stored_message(
  datum: HyperlaneRecipientDatum<DeferredRecipientInner>,
  message_id: ByteArray,
  tx: Transaction,
  own_ref: OutputReference,
  message_nft_policy: PolicyId,
) -> Bool {
  // 1. Verify the message NFT is being burned
  // This proves:
  // - The message was legitimate (NFT could only be minted via Mailbox)
  // - The message can't be replayed (NFT is now gone)
  expect message_nft_burned(tx, message_nft_policy, message_id)

  // 2. Update state
  let new_inner =
    DeferredRecipientInner {
      messages_stored: datum.inner.messages_stored,
      messages_processed: datum.inner.messages_processed + 1,
    }

  let new_datum =
    HyperlaneRecipientDatum {
      ism: datum.ism,
      last_processed_nonce: datum.last_processed_nonce,
      inner: new_inner,
    }

  // 3. Validate state continuation
  // Note: Any other outputs can be created - we don't restrict that
  validate_state_continuation(datum, new_datum, tx, own_ref)
}

/// Verify that the REAL mailbox is calling this recipient
fn mailbox_is_caller(tx: Transaction, mailbox_policy_id: PolicyId) -> Bool {
  list.any(
    tx.inputs,
    fn(input) {
      let policy_tokens = tokens(input.output.value, mailbox_policy_id)
      !dict.is_empty(policy_tokens)
    },
  )
}

/// Verify a message NFT is being minted with the SPECIFIC message_id
///
/// The message NFT has the message_id as its asset name (32 bytes).
/// We verify that the NFT being minted has the exact message_id we verified.
fn message_nft_minted_with_id(
  tx: Transaction,
  message_nft_policy: PolicyId,
  expected_message_id: ByteArray,
) -> Bool {
  let nft_mints = assets.tokens(tx.mint, message_nft_policy)

  // Check the specific message_id NFT has positive quantity (mint)
  when dict.get(nft_mints, expected_message_id) is {
    Some(quantity) -> quantity > 0
    None -> False
  }
}

/// Verify a message NFT is being burned
fn message_nft_burned(
  tx: Transaction,
  message_nft_policy: PolicyId,
  message_id: ByteArray,
) -> Bool {
  let nft_mints = assets.tokens(tx.mint, message_nft_policy)

  // The specific message_id NFT should be burned (negative quantity)
  when dict.get(nft_mints, message_id) is {
    Some(quantity) -> quantity < 0
    None -> False
  }
}

/// Verify a message UTXO is being created with VERIFIED data
///
/// Ensures the stored message contains all verified fields including message_id and nonce.
fn message_utxo_created(
  tx: Transaction,
  recipient_address: Address,
  message_nft_policy: PolicyId,
  expected_origin: Int,
  expected_sender: ByteArray,
  expected_body: ByteArray,
  expected_message_id: ByteArray,
  expected_nonce: Int,
) -> Bool {
  list.any(
    tx.outputs,
    fn(output) {
      // Must be at recipient address
      let at_our_address = output.address == recipient_address

      // Must contain the SPECIFIC message NFT (verified message_id as asset name)
      let has_correct_nft = {
        let nft_assets = assets.tokens(output.value, message_nft_policy)
        when dict.get(nft_assets, expected_message_id) is {
          Some(quantity) -> quantity > 0
          None -> False
        }
      }

      // Must have StoredMessageDatum with ALL correct values
      let has_valid_datum =
        when output.datum is {
          InlineDatum(datum_data) -> {
            expect stored_msg: StoredMessageDatum = datum_data
            and {
              stored_msg.origin == expected_origin,
              stored_msg.sender == expected_sender,
              stored_msg.body == expected_body,
              stored_msg.message_id == expected_message_id,
              stored_msg.nonce == expected_nonce,
            }
          }
          _ -> False
        }

      at_our_address && has_correct_nft && has_valid_datum
    },
  )
}

/// Validate state UTXO continuation
fn validate_state_continuation(
  _old_datum: HyperlaneRecipientDatum<DeferredRecipientInner>,
  new_datum: HyperlaneRecipientDatum<DeferredRecipientInner>,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // Find own input
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address
  let own_value = own_input.output.value

  // Find continuation output (the state UTXO, not the message UTXO)
  // The state UTXO should have a HyperlaneRecipientDatum, not StoredMessageDatum
  // We identify it by checking the datum type
  expect Some(continuation) =
    list.find(
      tx.outputs,
      fn(output) {
        output.address == own_address && {
          when output.datum is {
            InlineDatum(datum_data) -> {
              // Try to parse as HyperlaneRecipientDatum - if it works, this is the state UTXO
              let d = datum_data
              // Check if this looks like a HyperlaneRecipientDatum by comparing inner
              expect parsed: HyperlaneRecipientDatum<DeferredRecipientInner> = d
              // This is the state UTXO if it matches our expected new datum
              parsed == new_datum
            }
            _ -> False
          }
        }
      },
    )

  // Value must be preserved
  lovelace_of(continuation.value) >= lovelace_of(own_value)
}

use aiken/builtin
use aiken/collection/dict
use aiken/collection/list
use aiken/crypto.{keccak_256}
use cardano/address.{Address}
use cardano/transaction.{InlineDatum, OutputReference, Transaction}
use cardano/assets.{lovelace_of, tokens}
use types.{
  HyperlaneRecipientDatum, HyperlaneRecipientRedeemer, Message,
  PolicyId, HandleMessage, ContractAction, StoredMessageDatum,
  DeferredAction, ProcessStoredMessage, encode_message,
}
use utils.{find_input}

/// Example: Deferred Recipient
///
/// This is an EXAMPLE contract demonstrating the DeferredRecipient pattern for
/// Hyperlane recipients that need deferred message processing.
///
/// ## When to Use DeferredRecipient
///
/// Use this pattern when:
/// - The relayer cannot know how to build complex outputs
/// - Processing needs to happen separately (manually, via dApp, automated service, etc.)
/// - You need custom business logic that requires specific output construction
/// - Messages may need human review before processing
///
/// ## Two-Phase Processing Flow
///
/// **Phase 1 - Relayer stores message (HandleMessage):**
/// ```
/// Inputs:
///   - Mailbox UTXO (verifies ISM)
///   - DeferredRecipient State UTXO (this contract)
///   - ISM UTXO (for signature verification)
///
/// Mints:
///   - Message NFT (asset name = message_id, proves legitimacy)
///
/// Outputs:
///   - Mailbox continuation (unchanged)
///   - DeferredRecipient continuation (messages_stored += 1)
///   - Message UTXO (StoredMessageDatum + Message NFT)  <-- NEW
///   - Processed Message Marker
/// ```
///
/// **Phase 2 - Process stored message (ProcessStoredMessage):**
/// ```
/// Inputs:
///   - Message UTXO (contains stored message data)
///   - DeferredRecipient State UTXO (this contract)
///
/// Burns:
///   - Message NFT (proves message was consumed, prevents replay)
///
/// Outputs:
///   - DeferredRecipient continuation (messages_processed += 1)
///   - Custom outputs (your business logic)
/// ```
///
/// ## Security Model
///
/// The message NFT is the key security primitive:
///
/// 1. **NFT proves legitimacy**: The message NFT minting policy only allows
///    minting when the Mailbox NFT is in the transaction inputs. This means
///    the message went through proper ISM verification.
///
/// 2. **NFT prevents replay**: When a message is processed, the NFT MUST be burned.
///    Once burned, the NFT cannot be recreated, so the message cannot be processed again.
///
/// 3. **Processing flexibility**: The processor has full control over output construction.
///    This contract only verifies:
///    - Message NFT is burned (replay protection)
///    - State is updated correctly (bookkeeping)
///
/// ## Example: Token Bridging with Custom Vaults
///
/// Suppose you're building a token bridge where:
/// - Different token types need different vault structures
/// - Vault selection depends on message contents
/// - Some tokens need additional oracle data
///
/// With DeferredRecipient:
/// 1. Relayer stores the bridge message with NFT
/// 2. Read the message, determine vault type
/// 3. Query oracle for exchange rate
/// 4. Build transaction with correct vault and amounts
/// 5. Burn NFT to complete processing
///
/// See also: example_generic_recipient.ak for immediate message processing

/// Inner state for the example deferred recipient
///
/// In a real application, you might add:
/// - `pending_messages: List<ByteArray>` - track pending message IDs
/// - `operator: VerificationKeyHash` - who can process messages
/// - `config: YourConfigType` - application-specific configuration
pub type ExampleDeferredRecipientInner {
  /// Count of messages stored (received from relayer)
  messages_stored: Int,
  /// Count of messages processed
  messages_processed: Int,
}

/// Example Deferred Recipient validator
///
/// Parameters:
/// - `mailbox_policy_id`: NFT policy of the mailbox (for caller verification)
/// - `message_nft_policy`: Policy for message NFTs (deployed from stored_message_nft.ak)
///
/// ## Deployment Steps
///
/// 1. Deploy the message NFT minting policy (stored_message_nft.ak)
///    - Parameter: mailbox_policy_id
///    - Get the message_nft_policy hash
///
/// 2. Deploy this deferred recipient
///    - Parameters: mailbox_policy_id, message_nft_policy
///    - Get the deferred_recipient script hash
///
/// 3. Create the state UTXO
///    - Mint a state NFT (for UTXO identification)
///    - Create UTXO at deferred_recipient address
///    - Datum: HyperlaneRecipientDatum { ism: None, last_processed_nonce: None, inner: { messages_stored: 0, messages_processed: 0 } }
///
/// 4. Register in Hyperlane Registry
///    - script_hash: deferred_recipient hash
///    - state_locator: state NFT policy + asset name
///    - recipient_type: Deferred { message_policy: message_nft_policy }
validator example_deferred_recipient(mailbox_policy_id: PolicyId, message_nft_policy: PolicyId) {
  spend(
    datum_data: Option<Data>,
    redeemer: HyperlaneRecipientRedeemer<DeferredAction>,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    expect Some(raw_datum) = datum_data

    // Dispatch based on datum type
    // State UTXO has HyperlaneRecipientDatum (Constr 0 with 3 fields, inner is Constr 0 with 2 fields)
    // Message UTXO has StoredMessageDatum (Constr 0 with 5 fields)
    if is_recipient_datum(raw_datum) {
      // State UTXO path
      expect recipient_datum: HyperlaneRecipientDatum<ExampleDeferredRecipientInner> = raw_datum
      when redeemer is {
        // Phase 1: Relayer stores an incoming message
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

        // Phase 2: Process a stored message
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
    } else {
      // Message UTXO path - only require NFT is being burned
      when redeemer is {
        ContractAction { action } -> {
          let ProcessStoredMessage { message_id } = action
          message_nft_burned(tx, message_nft_policy, message_id)
        }
        // HandleMessage should not be called on message UTXOs
        _ -> False
      }
    }
  }

  else(_) {
    fail
  }
}

/// Check if datum can be decoded as HyperlaneRecipientDatum
/// HyperlaneRecipientDatum is Constr 0 with 3 fields, where inner is Constr 0 with 2 fields
/// StoredMessageDatum is Constr 0 with 5 fields
fn is_recipient_datum(d: Data) -> Bool {
  // Use builtin to decompose the Data and check structure
  let Pair(index, fields) = builtin.un_constr_data(d)
  // HyperlaneRecipientDatum has 3 fields, StoredMessageDatum has 5
  index == 0 && list.length(fields) == 3
}

/// Handle an incoming Hyperlane message by storing it
///
/// Called by the relayer when a cross-chain message arrives.
/// We don't process the message immediately - instead:
/// 1. Verify message data matches ISM-validated message_id (CRITICAL)
/// 2. Verify the mailbox is calling us (security)
/// 3. Verify message NFT is being minted with correct message_id (legitimacy proof)
/// 4. Verify message UTXO is being created (storage)
/// 5. Update our counter (bookkeeping)
fn handle_message(
  datum: HyperlaneRecipientDatum<ExampleDeferredRecipientInner>,
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
  //
  // This ensures the data we store is EXACTLY what the ISM verified.
  let computed_message_id = keccak_256(encode_message(message))
  expect computed_message_id == message_id

  // 2. Verify called by the REAL Mailbox
  //    We check for the mailbox NFT, not just the script address.
  //    This prevents attackers from creating fake mailbox UTXOs.
  expect mailbox_is_caller(tx, mailbox_policy_id)

  // 3. Verify a message NFT is being minted WITH THE CORRECT MESSAGE_ID
  //    The NFT's asset name must be the verified message_id.
  //    This links the NFT to the specific verified message.
  expect message_nft_minted_with_id(tx, message_nft_policy, message_id)

  // 4. Verify a message UTXO is being created with VERIFIED data
  //    The message UTXO stores the message data for later processing.
  //    We use data from the verified message, not raw redeemer values.
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

  // 5. Update state - just increment the counter
  //    Complex state changes happen in Phase 2 when the message is processed
  let new_inner =
    ExampleDeferredRecipientInner {
      messages_stored: datum.inner.messages_stored + 1,
      messages_processed: datum.inner.messages_processed,
    }

  let new_datum =
    HyperlaneRecipientDatum {
      ism: datum.ism,
      last_processed_nonce: datum.last_processed_nonce,
      inner: new_inner,
    }

  // 6. Validate state continuation
  validate_state_continuation(datum, new_datum, tx, own_ref)
}

/// Process a stored message
///
/// Called when ready to process a stored message.
/// The caller has full control over outputs - this contract only verifies:
/// 1. The correct message NFT is being burned (replay protection)
/// 2. State is updated correctly (bookkeeping)
///
/// ## Processing Steps
///
/// To process a message:
/// - Query for UTXOs with message NFT at the recipient address
/// - Parse StoredMessageDatum to get message details
/// - Build appropriate outputs based on your business logic
/// - Submit transaction that burns the NFT
fn process_stored_message(
  datum: HyperlaneRecipientDatum<ExampleDeferredRecipientInner>,
  message_id: ByteArray,
  tx: Transaction,
  own_ref: OutputReference,
  message_nft_policy: PolicyId,
) -> Bool {
  // 1. Verify the message NFT is being burned
  //    This proves:
  //    - The message was legitimate (NFT could only be minted via Mailbox)
  //    - The message can't be replayed (NFT is now destroyed)
  expect message_nft_burned(tx, message_nft_policy, message_id)

  // 2. Update state - increment processed counter
  let new_inner =
    ExampleDeferredRecipientInner {
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
  //    Note: Any other outputs can be created - we don't restrict that.
  //    This is intentional - flexibility for business logic.
  validate_state_continuation(datum, new_datum, tx, own_ref)
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Verify that the REAL mailbox is calling this recipient
///
/// We check for the mailbox's unique NFT.
/// This prevents attacks where someone creates a fake UTXO at the mailbox address.
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
/// This ensures the NFT is cryptographically linked to the verified message.
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
///
/// For replay protection, we verify the specific message_id NFT is burned.
fn message_nft_burned(
  tx: Transaction,
  message_nft_policy: PolicyId,
  message_id: ByteArray,
) -> Bool {
  let nft_mints = assets.tokens(tx.mint, message_nft_policy)

  // Check the specific message_id NFT has negative quantity (burn)
  when dict.get(nft_mints, message_id) is {
    Some(quantity) -> quantity < 0
    None -> False
  }
}

/// Verify a message UTXO is being created with VERIFIED data
///
/// The message UTXO should:
/// - Be at the recipient address (our address)
/// - Contain the SPECIFIC message NFT (with verified message_id as asset name)
/// - Have a StoredMessageDatum with ALL correct values including message_id and nonce
///
/// This ensures the stored message data is cryptographically linked to the ISM-verified message.
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
      // This includes message_id and nonce, ensuring complete data integrity
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
///
/// Ensures:
/// - Continuation output exists at our address
/// - Datum is updated correctly
/// - Value is preserved (no value extraction)
fn validate_state_continuation(
  _old_datum: HyperlaneRecipientDatum<ExampleDeferredRecipientInner>,
  new_datum: HyperlaneRecipientDatum<ExampleDeferredRecipientInner>,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // Find own input
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address
  let own_value = own_input.output.value

  // Find continuation output (the state UTXO, not the message UTXO)
  // We identify the state UTXO by checking that it has our expected new datum
  expect Some(continuation) =
    list.find(
      tx.outputs,
      fn(output) {
        output.address == own_address && {
          when output.datum is {
            InlineDatum(datum_data) -> {
              // Try to parse as our datum type
              let d = datum_data
              expect parsed: HyperlaneRecipientDatum<ExampleDeferredRecipientInner> = d
              parsed == new_datum
            }
            _ -> False
          }
        }
      },
    )

  // Value must be preserved (prevent value extraction attacks)
  lovelace_of(continuation.value) >= lovelace_of(own_value)
}

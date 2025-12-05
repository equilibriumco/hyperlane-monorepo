use aiken/collection/dict
use aiken/collection/list
use aiken/primitive/bytearray
use cardano/address.{Address}
use cardano/transaction.{InlineDatum, OutputReference, Transaction}
use cardano/assets.{lovelace_of, tokens}
use types.{
  Domain, HyperlaneAddress, HyperlaneRecipientDatum, HyperlaneRecipientRedeemer,
  PolicyId, HandleMessage, ContractAction, StoredMessageDatum,
  DeferredAction, ProcessStoredMessage,
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
      HandleMessage { origin, sender, body } ->
        handle_message(
          recipient_datum,
          origin,
          sender,
          body,
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
fn handle_message(
  datum: HyperlaneRecipientDatum<DeferredRecipientInner>,
  origin: Domain,
  sender: HyperlaneAddress,
  body: ByteArray,
  tx: Transaction,
  own_ref: OutputReference,
  mailbox_policy_id: PolicyId,
  message_nft_policy: PolicyId,
) -> Bool {
  // 1. Verify called by the REAL Mailbox (check for mailbox NFT)
  expect mailbox_is_caller(tx, mailbox_policy_id)

  // 2. Verify a message NFT is being minted
  // The NFT's asset name should be 32 bytes (message_id)
  expect message_nft_minted(tx, message_nft_policy)

  // 3. Verify a message UTXO is being created with StoredMessageDatum
  // Find own input to get our address
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address

  // The message UTXO should be at our address with:
  // - The message NFT
  // - A StoredMessageDatum with matching origin, sender, body
  expect message_utxo_created(tx, own_address, message_nft_policy, origin, sender, body)

  // 4. Update state (minimal change - just increment counter)
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

  // 5. Validate state continuation (datum updated, value preserved)
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

/// Verify a message NFT is being minted
fn message_nft_minted(tx: Transaction, message_nft_policy: PolicyId) -> Bool {
  let nft_mints = assets.tokens(tx.mint, message_nft_policy)
  let mint_pairs = dict.to_pairs(nft_mints)

  // At least one mint with 32-byte asset name and positive quantity
  list.any(
    mint_pairs,
    fn(pair) {
      let Pair(asset_name, quantity) = pair
      bytearray.length(asset_name) == 32 && quantity > 0
    },
  )
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

/// Verify a message UTXO is being created
fn message_utxo_created(
  tx: Transaction,
  recipient_address: Address,
  message_nft_policy: PolicyId,
  expected_origin: Domain,
  expected_sender: HyperlaneAddress,
  expected_body: ByteArray,
) -> Bool {
  // Find an output at our address with the message NFT
  list.any(
    tx.outputs,
    fn(output) {
      // Must be at recipient address
      let at_our_address = output.address == recipient_address

      // Must contain a message NFT (32-byte asset name)
      let has_message_nft = {
        let nft_assets = assets.tokens(output.value, message_nft_policy)
        let nft_pairs = dict.to_pairs(nft_assets)
        list.any(
          nft_pairs,
          fn(pair) {
            let Pair(asset_name, quantity) = pair
            bytearray.length(asset_name) == 32 && quantity > 0
          },
        )
      }

      // Must have StoredMessageDatum with correct values
      let has_valid_datum =
        when output.datum is {
          InlineDatum(datum_data) -> {
            expect stored_msg: StoredMessageDatum = datum_data
            stored_msg.origin == expected_origin && stored_msg.sender == expected_sender && stored_msg.body == expected_body
          }
          _ -> False
        }

      at_our_address && has_message_nft && has_valid_datum
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

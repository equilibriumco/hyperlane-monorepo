use aiken/collection/list
use aiken/primitive/bytearray
use cardano/address.{Address, Credential, Script}
use cardano/assets.{lovelace_of, quantity_of}
use cardano/transaction.{InlineDatum, Output, OutputReference, Transaction}
use types.{
  AssetName, Domain, HyperlaneAddress, PolicyId, ScriptHash, UtxoLocator,
  WarpRouteDatum, WarpRouteRedeemer, WarpTransferBody,
  TransferRemote, ReceiveTransfer, EnrollRemoteRoute,
  Collateral, Synthetic, Native,
}
use utils.{assoc_find, find_input, is_signed_by}

/// Warp Route Validator
/// Enables token bridging between Cardano and other Hyperlane-connected chains
///
/// **Supported Token Types:**
/// - Collateral: Lock native Cardano tokens in a vault, release on receive
/// - Synthetic: Mint synthetic tokens on receive, burn on send
/// - Native: Lock ADA in a vault (for bridging ADA)
///
/// **Token Flow:**
/// Outbound (Cardano → Other):
/// 1. User calls TransferRemote with destination, recipient, amount
/// 2. Tokens are locked (collateral/native) or burned (synthetic)
/// 3. Message is dispatched via Mailbox
/// 4. Relayer delivers message to destination chain warp route
///
/// Inbound (Other → Cardano):
/// 1. Relayer processes message from origin warp route
/// 2. Calls ReceiveTransfer with origin, sender, body
/// 3. Tokens are released (collateral/native) or minted (synthetic)
/// 4. Recipient receives tokens
validator warp_route(mailbox_policy_id: PolicyId) {
  spend(
    datum: Option<WarpRouteDatum>,
    redeemer: WarpRouteRedeemer,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    expect Some(warp_datum) = datum

    when redeemer is {
      TransferRemote { destination, recipient, amount } ->
        validate_transfer_remote(
          warp_datum,
          destination,
          recipient,
          amount,
          tx,
          own_ref,
          mailbox_policy_id,
        )

      ReceiveTransfer { origin, sender, body } ->
        validate_receive_transfer(
          warp_datum,
          origin,
          sender,
          body,
          tx,
          own_ref,
          mailbox_policy_id,
        )

      EnrollRemoteRoute { domain, route } ->
        validate_enroll_remote_route(warp_datum, domain, route, tx, own_ref)
    }
  }

  else(_) {
    fail
  }
}

/// Validate outbound transfer (sending tokens to another chain)
fn validate_transfer_remote(
  datum: WarpRouteDatum,
  destination: Domain,
  recipient: HyperlaneAddress,
  amount: Int,
  tx: Transaction,
  own_ref: OutputReference,
  mailbox_policy_id: PolicyId,
) -> Bool {
  // 1. Amount must be positive
  expect amount > 0

  // 2. Destination must have an enrolled route
  expect Some(_remote_route) = assoc_find(datum.config.remote_routes, destination)

  // 3. Recipient must be valid (32 bytes)
  expect bytearray.length(recipient) == 32

  // 4. Find own input
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address

  // 5. Find continuation output
  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  // 6. Based on token type, validate the token movement
  let token_valid =
    when datum.config.token_type is {
      Collateral { policy_id, asset_name, vault_locator } ->
        validate_collateral_lock(
          vault_locator,
          policy_id,
          asset_name,
          amount,
          tx,
        )

      Synthetic { minting_policy } ->
        validate_synthetic_burn(minting_policy, amount, tx)

      Native { vault_locator } ->
        validate_native_lock(vault_locator, amount, tx)
    }
  expect token_valid

  // 7. Verify mailbox dispatch is being called
  // The mailbox UTXO should be spent with Dispatch redeemer in the same tx
  expect mailbox_dispatch_called(mailbox_policy_id, tx)

  // 8. Verify datum updated (increment total_bridged)
  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: WarpRouteDatum = cont_datum_data
  expect cont_datum.config == datum.config
  expect cont_datum.owner == datum.owner
  expect cont_datum.total_bridged == datum.total_bridged + amount

  True
}

/// Validate inbound transfer (receiving tokens from another chain)
fn validate_receive_transfer(
  datum: WarpRouteDatum,
  origin: Domain,
  sender: HyperlaneAddress,
  body: ByteArray,
  tx: Transaction,
  own_ref: OutputReference,
  mailbox_policy_id: PolicyId,
) -> Bool {
  // 1. Verify sender is an enrolled remote route
  expect Some(expected_sender) = assoc_find(datum.config.remote_routes, origin)
  expect sender == expected_sender

  // 2. Verify mailbox is calling us (mailbox UTXO spent in same tx)
  expect mailbox_is_caller(mailbox_policy_id, tx)

  // 3. Decode the transfer body
  expect Some(transfer) = decode_warp_transfer_body(body)
  expect transfer.amount > 0

  // 4. Find own input
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address

  // 5. Find continuation output
  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  // 6. Based on token type, validate token release/mint
  let token_valid =
    when datum.config.token_type is {
      Collateral { policy_id, asset_name, vault_locator } ->
        validate_collateral_release(
          vault_locator,
          policy_id,
          asset_name,
          transfer.recipient,
          transfer.amount,
          tx,
        )

      Synthetic { minting_policy } ->
        validate_synthetic_mint(
          minting_policy,
          transfer.recipient,
          transfer.amount,
          tx,
        )

      Native { vault_locator } ->
        validate_native_release(vault_locator, transfer.recipient, transfer.amount, tx)
    }
  expect token_valid

  // 7. Verify datum updated (decrement total_bridged)
  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: WarpRouteDatum = cont_datum_data
  expect cont_datum.config == datum.config
  expect cont_datum.owner == datum.owner
  expect cont_datum.total_bridged == datum.total_bridged - transfer.amount

  True
}

/// Validate enrolling a new remote route
fn validate_enroll_remote_route(
  datum: WarpRouteDatum,
  domain: Domain,
  route: HyperlaneAddress,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // 1. Only owner can enroll routes
  expect is_signed_by(tx, datum.owner)

  // 2. Route address must be valid (32 bytes)
  expect bytearray.length(route) == 32

  // 3. Find own input
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address
  let own_value = own_input.output.value

  // 4. Find continuation output
  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  // 5. Verify value is preserved
  expect lovelace_of(continuation.value) >= lovelace_of(own_value)

  // 6. Verify datum is updated with new route
  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: WarpRouteDatum = cont_datum_data

  // New routes should have the domain added
  let new_routes = upsert_route(datum.config.remote_routes, domain, route)
  let expected_config =
    types.WarpRouteConfig { ..datum.config, remote_routes: new_routes }

  expect cont_datum.config == expected_config
  expect cont_datum.owner == datum.owner
  expect cont_datum.total_bridged == datum.total_bridged

  True
}

// ============================================================================
// Collateral Token Helpers (Lock native tokens in vault)
// ============================================================================

/// Validate that tokens are locked in the vault
fn validate_collateral_lock(
  vault_locator: UtxoLocator,
  policy_id: PolicyId,
  asset_name: AssetName,
  amount: Int,
  tx: Transaction,
) -> Bool {
  // Find vault input and output by the NFT locator
  expect Some(vault_in) = find_input_by_nft(tx, vault_locator)
  expect Some(vault_out) = find_output_by_nft(tx, vault_locator)

  // Vault should receive `amount` more tokens
  let input_tokens = quantity_of(vault_in.output.value, policy_id, asset_name)
  let output_tokens = quantity_of(vault_out.value, policy_id, asset_name)

  output_tokens >= input_tokens + amount
}

/// Validate that tokens are released from the vault
fn validate_collateral_release(
  vault_locator: UtxoLocator,
  policy_id: PolicyId,
  asset_name: AssetName,
  recipient: ByteArray,
  amount: Int,
  tx: Transaction,
) -> Bool {
  // Find vault input and output
  expect Some(vault_in) = find_input_by_nft(tx, vault_locator)
  expect Some(vault_out) = find_output_by_nft(tx, vault_locator)

  // Vault should release `amount` tokens
  let input_tokens = quantity_of(vault_in.output.value, policy_id, asset_name)
  let output_tokens = quantity_of(vault_out.value, policy_id, asset_name)
  expect input_tokens - output_tokens == amount

  // Recipient should receive the tokens
  expect
    recipient_receives_tokens(tx, recipient, policy_id, asset_name, amount)

  True
}

// ============================================================================
// Synthetic Token Helpers (Mint/burn synthetic tokens)
// ============================================================================

/// Validate that synthetic tokens are burned
fn validate_synthetic_burn(
  minting_policy: ScriptHash,
  amount: Int,
  tx: Transaction,
) -> Bool {
  // Check that exactly -amount is minted (i.e., burned)
  let minted = get_minted_amount(tx, minting_policy)
  minted == -amount
}

/// Validate that synthetic tokens are minted to recipient
fn validate_synthetic_mint(
  minting_policy: ScriptHash,
  recipient: ByteArray,
  amount: Int,
  tx: Transaction,
) -> Bool {
  // Check that exactly +amount is minted
  let minted = get_minted_amount(tx, minting_policy)
  expect minted == amount

  // Recipient should receive the minted tokens
  // The asset name for synthetic tokens is typically empty or "synthetic"
  expect recipient_receives_tokens(tx, recipient, minting_policy, "", amount)

  True
}

// ============================================================================
// Native ADA Helpers
// ============================================================================

/// Validate that ADA is locked in the vault
fn validate_native_lock(
  vault_locator: UtxoLocator,
  amount: Int,
  tx: Transaction,
) -> Bool {
  // Find vault input and output
  expect Some(vault_in) = find_input_by_nft(tx, vault_locator)
  expect Some(vault_out) = find_output_by_nft(tx, vault_locator)

  // Vault should receive `amount` more lovelace
  let input_ada = lovelace_of(vault_in.output.value)
  let output_ada = lovelace_of(vault_out.value)

  output_ada >= input_ada + amount
}

/// Validate that ADA is released from the vault
fn validate_native_release(
  vault_locator: UtxoLocator,
  recipient: ByteArray,
  amount: Int,
  tx: Transaction,
) -> Bool {
  // Find vault input and output
  expect Some(vault_in) = find_input_by_nft(tx, vault_locator)
  expect Some(vault_out) = find_output_by_nft(tx, vault_locator)

  // Vault should release `amount` lovelace
  let input_ada = lovelace_of(vault_in.output.value)
  let output_ada = lovelace_of(vault_out.value)
  expect input_ada - output_ada >= amount

  // Recipient should receive the ADA
  expect recipient_receives_ada(tx, recipient, amount)

  True
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Check if mailbox is being called (its UTXO is spent)
fn mailbox_is_caller(mailbox_policy_id: PolicyId, tx: Transaction) -> Bool {
  list.any(
    tx.inputs,
    fn(input) {
      // Check if any input contains the mailbox NFT
      quantity_of(input.output.value, mailbox_policy_id, "") > 0
    },
  )
}

/// Check if mailbox dispatch is being called
fn mailbox_dispatch_called(mailbox_policy_id: PolicyId, tx: Transaction) -> Bool {
  // Same check - if mailbox UTXO is spent, dispatch is happening
  mailbox_is_caller(mailbox_policy_id, tx)
}

/// Find an input by NFT locator
fn find_input_by_nft(
  tx: Transaction,
  locator: UtxoLocator,
) -> Option<transaction.Input> {
  list.find(
    tx.inputs,
    fn(input) {
      quantity_of(input.output.value, locator.policy_id, locator.asset_name) > 0
    },
  )
}

/// Find an output by NFT locator
fn find_output_by_nft(tx: Transaction, locator: UtxoLocator) -> Option<Output> {
  list.find(
    tx.outputs,
    fn(output) {
      quantity_of(output.value, locator.policy_id, locator.asset_name) > 0
    },
  )
}

/// Check if recipient receives tokens
fn recipient_receives_tokens(
  tx: Transaction,
  recipient_bytes: ByteArray,
  policy_id: PolicyId,
  asset_name: AssetName,
  amount: Int,
) -> Bool {
  // Find output to recipient with enough tokens
  list.any(
    tx.outputs,
    fn(output) {
      address_matches_bytes(output.address, recipient_bytes) && quantity_of(
        output.value,
        policy_id,
        asset_name,
      ) >= amount
    },
  )
}

/// Check if recipient receives ADA
fn recipient_receives_ada(
  tx: Transaction,
  recipient_bytes: ByteArray,
  amount: Int,
) -> Bool {
  list.any(
    tx.outputs,
    fn(output) {
      address_matches_bytes(output.address, recipient_bytes) && lovelace_of(
        output.value,
      ) >= amount
    },
  )
}

/// Check if address matches recipient bytes
/// The recipient bytes can be a payment credential hash or full address
fn address_matches_bytes(addr: Address, recipient_bytes: ByteArray) -> Bool {
  when addr.payment_credential is {
    Credential.VerificationKey(hash) -> hash == recipient_bytes
    Script(hash) -> hash == recipient_bytes
  }
}

/// Decode warp transfer body
/// Format: recipient (variable) || amount (8 bytes big-endian)
fn decode_warp_transfer_body(body: ByteArray) -> Option<WarpTransferBody> {
  let len = bytearray.length(body)
  if len < 8 {
    None
  } else {
    // Last 8 bytes are the amount
    let amount_bytes = bytearray.slice(body, len - 8, len - 1)
    let amount = bytes_to_int(amount_bytes)

    // Everything before is the recipient
    let recipient = bytearray.slice(body, 0, len - 9)

    Some(WarpTransferBody { recipient, amount })
  }
}

/// Convert big-endian bytes to integer
fn bytes_to_int(bytes: ByteArray) -> Int {
  bytearray.foldl(bytes, 0, fn(byte, acc) { acc * 256 + byte })
}

/// Get minted amount for a policy (negative means burn)
fn get_minted_amount(_tx: Transaction, _policy_id: PolicyId) -> Int {
  // TODO: Implement actual minting amount calculation from tx.mint
  // This is a placeholder - actual implementation would sum tokens under this policy
  0
}

/// Upsert a route in the routes list
fn upsert_route(
  routes: List<(Domain, HyperlaneAddress)>,
  domain: Domain,
  route: HyperlaneAddress,
) -> List<(Domain, HyperlaneAddress)> {
  upsert_route_helper(routes, domain, route, False)
}

fn upsert_route_helper(
  routes: List<(Domain, HyperlaneAddress)>,
  domain: Domain,
  route: HyperlaneAddress,
  found: Bool,
) -> List<(Domain, HyperlaneAddress)> {
  when routes is {
    [] ->
      if found {
        []
      } else {
        [(domain, route)]
      }
    [(d, r), ..rest] ->
      if d == domain {
        [(d, route), ..upsert_route_helper(rest, domain, route, True)]
      } else {
        [(d, r), ..upsert_route_helper(rest, domain, route, found)]
      }
  }
}

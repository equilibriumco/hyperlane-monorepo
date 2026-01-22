use aiken/collection/dict
use aiken/collection/list
use aiken/primitive/bytearray
use cardano/address.{Address, Credential, Script}
use cardano/assets.{lovelace_of, quantity_of, tokens}
use cardano/transaction.{InlineDatum, Output, OutputReference, Transaction}
use types.{
  AssetName, Domain, HyperlaneAddress, PolicyId, ScriptHash,
  WarpRouteDatum, WarpRouteRedeemer, WarpTransferBody,
  TransferRemote, ReceiveTransfer, EnrollRemoteRoute,
  Collateral, Synthetic, Native,
}
use utils.{assoc_find, find_input, is_signed_by}

/// Warp Route Validator
/// Enables token bridging between Cardano and other Hyperlane-connected chains
///
/// **Supported Token Types:**
/// - Collateral: Lock native Cardano tokens in warp route UTXO, release on receive
/// - Synthetic: Mint synthetic tokens on receive, burn on send
/// - Native: Lock ADA in warp route UTXO (for bridging ADA)
///
/// **Token Flow:**
/// Outbound (Cardano → Other):
/// 1. User calls TransferRemote with destination, recipient, amount
/// 2. Tokens are locked in warp route UTXO (collateral/native) or burned (synthetic)
/// 3. Message is dispatched via Mailbox
/// 4. Relayer delivers message to destination chain warp route
///
/// Inbound (Other → Cardano):
/// 1. Relayer processes message from origin warp route
/// 2. Calls ReceiveTransfer with origin, sender, body
/// 3. Tokens are released from warp route UTXO (collateral/native) or minted (synthetic)
/// 4. Recipient receives tokens
validator warp_route(mailbox_policy_id: PolicyId, _state_nft_policy_id: PolicyId) {
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
  // For Native/Collateral: tokens are locked in the warp route UTXO itself
  let token_valid =
    when datum.config.token_type is {
      Collateral { policy_id, asset_name } ->
        validate_collateral_lock(
          own_input.output,
          continuation,
          policy_id,
          asset_name,
          amount,
        )

      Synthetic { minting_policy } ->
        validate_synthetic_burn(minting_policy, amount, tx)

      Native ->
        validate_native_lock(own_input.output, continuation, amount)
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

  // 3. Decode the EVM TokenMessage body (wire format)
  // This parses: recipient (32 bytes) || amount (32 bytes uint256)
  expect Some(wire_transfer) = decode_evm_token_message(body)
  expect wire_transfer.amount > 0

  // 4. Convert wire amount (remote_decimals) to local amount (local_decimals)
  // Example: 1e18 wire (18 decimals) -> 1e6 local (6 decimals ADA)
  let local_amount = convert_decimals(
    wire_transfer.amount,
    datum.config.remote_decimals,
    datum.config.decimals,
  )
  expect local_amount > 0

  // 5. Find own input
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address

  // 6. Find continuation output
  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  // 7. Based on token type, validate token release/mint with LOCAL amount
  // For Native/Collateral: tokens are released from the warp route UTXO itself
  let token_valid =
    when datum.config.token_type is {
      Collateral { policy_id, asset_name } ->
        validate_collateral_release(
          own_input.output,
          continuation,
          policy_id,
          asset_name,
          wire_transfer.recipient,
          local_amount,
          tx,
        )

      Synthetic { minting_policy } ->
        validate_synthetic_mint(
          minting_policy,
          wire_transfer.recipient,
          local_amount,
          tx,
        )

      Native ->
        validate_native_release(
          own_input.output,
          continuation,
          wire_transfer.recipient,
          local_amount,
          tx,
        )
    }
  expect token_valid

  // 8. Verify datum updated (decrement total_bridged by LOCAL amount)
  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: WarpRouteDatum = cont_datum_data
  expect cont_datum.config == datum.config
  expect cont_datum.owner == datum.owner
  expect cont_datum.total_bridged == datum.total_bridged - local_amount

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
// Collateral Token Helpers (Lock native tokens in warp route UTXO)
// ============================================================================

/// Validate that tokens are locked in the warp route UTXO
fn validate_collateral_lock(
  own_input: Output,
  own_output: Output,
  policy_id: PolicyId,
  asset_name: AssetName,
  amount: Int,
) -> Bool {
  // Warp route UTXO should receive `amount` more tokens
  let input_tokens = quantity_of(own_input.value, policy_id, asset_name)
  let output_tokens = quantity_of(own_output.value, policy_id, asset_name)

  output_tokens >= input_tokens + amount
}

/// Validate that tokens are released from the warp route UTXO
fn validate_collateral_release(
  own_input: Output,
  own_output: Output,
  policy_id: PolicyId,
  asset_name: AssetName,
  recipient: ByteArray,
  amount: Int,
  tx: Transaction,
) -> Bool {
  // Warp route UTXO should release `amount` tokens
  let input_tokens = quantity_of(own_input.value, policy_id, asset_name)
  let output_tokens = quantity_of(own_output.value, policy_id, asset_name)
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
// Native ADA Helpers (Lock/release ADA in warp route UTXO)
// ============================================================================

/// Validate that ADA is locked in the warp route UTXO
fn validate_native_lock(own_input: Output, own_output: Output, amount: Int) -> Bool {
  // Warp route UTXO should receive `amount` more lovelace
  let input_ada = lovelace_of(own_input.value)
  let output_ada = lovelace_of(own_output.value)

  output_ada >= input_ada + amount
}

/// Validate that ADA is released from the warp route UTXO
fn validate_native_release(
  own_input: Output,
  own_output: Output,
  recipient: ByteArray,
  amount: Int,
  tx: Transaction,
) -> Bool {
  // Warp route UTXO should release `amount` lovelace
  let input_ada = lovelace_of(own_input.value)
  let output_ada = lovelace_of(own_output.value)
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
      // Check if any input contains a token with the mailbox NFT policy
      let policy_tokens = tokens(input.output.value, mailbox_policy_id)
      !dict.is_empty(policy_tokens)
    },
  )
}

/// Check if mailbox dispatch is being called
fn mailbox_dispatch_called(mailbox_policy_id: PolicyId, tx: Transaction) -> Bool {
  // Same check - if mailbox UTXO is spent, dispatch is happening
  mailbox_is_caller(mailbox_policy_id, tx)
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

/// Decode EVM TokenMessage body (wire format from other chains)
/// EVM TokenMessage format (from TokenMessage.sol):
/// - bytes 0-31: recipient (bytes32) - Cardano credential padded with 4 leading zeros
/// - bytes 32-63: amount (uint256, big-endian) - wire format amount (remote_decimals)
/// - bytes 64+: metadata (optional, ignored)
fn decode_evm_token_message(body: ByteArray) -> Option<WarpTransferBody> {
  let len = bytearray.length(body)
  // Minimum 64 bytes: 32 recipient + 32 amount
  if len < 64 {
    None
  } else {
    // Extract recipient: bytes 0-31 (32 bytes)
    // EVM pads 28-byte Cardano credentials with 4 leading zeros
    // We extract the last 28 bytes: bytes 4-31
    let recipient = bytearray.slice(body, 4, 31)

    // Extract amount: bytes 32-63 (32 bytes uint256)
    // We only use the last 8 bytes since Cardano amounts fit in u64
    let amount_bytes = bytearray.slice(body, 56, 63)
    let amount = bytes_to_int(amount_bytes)

    Some(WarpTransferBody { recipient, amount })
  }
}

/// Convert wire format amount (remote_decimals) to local amount (local_decimals)
/// Formula: local_amount = wire_amount / 10^(remote_decimals - local_decimals)
/// If local_decimals >= remote_decimals, no conversion needed (multiply instead)
fn convert_decimals(wire_amount: Int, remote_decimals: Int, local_decimals: Int) -> Int {
  if local_decimals >= remote_decimals {
    // Upsample: multiply by 10^(local_decimals - remote_decimals)
    let multiplier = pow10(local_decimals - remote_decimals)
    wire_amount * multiplier
  } else {
    // Downsample: divide by 10^(remote_decimals - local_decimals)
    let divisor = pow10(remote_decimals - local_decimals)
    wire_amount / divisor
  }
}

/// Compute 10^n for decimal scaling
fn pow10(n: Int) -> Int {
  if n <= 0 {
    1
  } else {
    10 * pow10(n - 1)
  }
}

/// Convert big-endian bytes to integer
fn bytes_to_int(bytes: ByteArray) -> Int {
  bytearray.foldl(bytes, 0, fn(byte, acc) { acc * 256 + byte })
}

/// Get minted amount for a policy (negative means burn)
/// Sums all token quantities under the given policy from tx.mint
fn get_minted_amount(tx: Transaction, policy_id: PolicyId) -> Int {
  let policy_mints = tokens(tx.mint, policy_id)
  dict.foldl(policy_mints, 0, fn(_asset_name, quantity, acc) { acc + quantity })
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

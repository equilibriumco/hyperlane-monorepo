use aiken/collection/dict
use aiken/collection/list
use aiken/crypto.{VerificationKeyHash, keccak_256}
use aiken/primitive/bytearray
use cardano/address.{Credential, Script}
use cardano/assets.{lovelace_of, quantity_of, tokens}
use cardano/transaction.{InlineDatum, Output, OutputReference, Transaction}
use types.{
  AssetName, Domain, HyperlaneAddress, Message, PolicyId, ScriptHash,
  WarpRouteDatum, WarpRouteRedeemer, WarpTransferBody,
  TransferRemote, ReceiveTransfer, EnrollRemoteRoute,
  Collateral, Synthetic, Native,
  RedemptionDatum, RedemptionTokenInfo, RedemptionAda, RedemptionToken,
  encode_message,
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
/// Inbound (Other → Cardano) - Two-Phase Redemption Pattern:
/// 1. Relayer processes message from origin warp route
/// 2. Calls ReceiveTransfer with origin, sender, body, return_address, expiry_slot
/// 3. Tokens are released to a redemption UTXO (not directly to recipient)
/// 4. Recipient claims tokens from redemption UTXO with their own ADA
/// 5. Relayer's minUTxO ADA is returned to return_address
/// 6. If unclaimed after expiry_slot, relayer can reclaim tokens + minUTxO
///
/// This pattern ensures recipients provide their own minUTxO (economically sound)
/// and relayer's ADA is a revolving float, not a gift.
validator warp_route(
  mailbox_policy_id: PolicyId,
  _state_nft_policy_id: PolicyId,
  processed_messages_nft_policy: PolicyId,
  redemption_script: ScriptHash,
) {
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

      ReceiveTransfer { message, message_id, return_address, expiry_slot } -> {
        // SECURITY: Verify the message hashes to the ISM-validated message_id
        expect keccak_256(encode_message(message)) == message_id
        validate_receive_transfer(
          warp_datum,
          message.origin,
          message.sender,
          message.body,
          tx,
          own_ref,
          mailbox_policy_id,
          processed_messages_nft_policy,
          message_id,
          redemption_script,
          return_address,
          expiry_slot,
        )
      }

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
/// Creates a redemption UTXO for the recipient to claim with their own ADA
fn validate_receive_transfer(
  datum: WarpRouteDatum,
  origin: Domain,
  sender: HyperlaneAddress,
  body: ByteArray,
  tx: Transaction,
  own_ref: OutputReference,
  mailbox_policy_id: PolicyId,
  processed_messages_nft_policy: PolicyId,
  message_id: ByteArray,
  redemption_script: ScriptHash,
  return_address: VerificationKeyHash,
  expiry_slot: Int,
) -> Bool {
  // 1. Verify sender is an enrolled remote route
  expect Some(expected_sender) = assoc_find(datum.config.remote_routes, origin)
  expect sender == expected_sender

  // 2. Verify mailbox is calling us (mailbox UTXO spent in same tx)
  expect mailbox_is_caller(mailbox_policy_id, tx)

  // 3. CRITICAL: Verify the mailbox validated THIS exact message_id
  // The mailbox mints a processed message NFT with the message_id as asset name
  // during Process. Checking this NFT in tx.mint proves the mailbox ISM-validated
  // this specific message_id in this transaction.
  expect processed_nft_minted(processed_messages_nft_policy, message_id, tx)

  // 4. Decode the EVM TokenMessage body (wire format)
  // This parses: recipient (32 bytes) || amount (32 bytes uint256)
  expect Some(wire_transfer) = decode_evm_token_message(body)
  expect wire_transfer.amount > 0

  // 5. Convert wire amount (remote_decimals) to local amount (local_decimals)
  // Example: 1e18 wire (18 decimals) -> 1e6 local (6 decimals ADA)
  let local_amount = convert_decimals(
    wire_transfer.amount,
    datum.config.remote_decimals,
    datum.config.decimals,
  )
  expect local_amount > 0

  // 6. Find own input
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address

  // 7. Find continuation output
  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  // 8. Determine token info for redemption datum
  let redemption_token_info =
    when datum.config.token_type is {
      Native -> RedemptionAda
      Collateral { policy_id, asset_name } ->
        RedemptionToken { policy_id, asset_name }
      Synthetic { minting_policy } ->
        // For synthetic tokens, the policy is the minting policy with empty asset name
        RedemptionToken { policy_id: minting_policy, asset_name: "" }
    }

  // 9. Validate redemption UTXO is created with correct datum and tokens
  let redemption_valid =
    when datum.config.token_type is {
      Collateral { policy_id, asset_name } ->
        validate_collateral_redemption(
          own_input.output,
          continuation,
          policy_id,
          asset_name,
          wire_transfer.recipient,
          local_amount,
          tx,
          redemption_script,
          redemption_token_info,
          return_address,
          expiry_slot,
        )

      Synthetic { minting_policy } ->
        validate_synthetic_redemption(
          minting_policy,
          wire_transfer.recipient,
          local_amount,
          tx,
          redemption_script,
          redemption_token_info,
          return_address,
          expiry_slot,
        )

      Native ->
        validate_native_redemption(
          own_input.output,
          continuation,
          wire_transfer.recipient,
          local_amount,
          tx,
          redemption_script,
          return_address,
          expiry_slot,
        )
    }
  expect redemption_valid

  // 10. Verify datum updated (decrement total_bridged by LOCAL amount)
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

/// Validate that tokens are released to a redemption UTXO
fn validate_collateral_redemption(
  own_input: Output,
  own_output: Output,
  policy_id: PolicyId,
  asset_name: AssetName,
  recipient: ByteArray,
  amount: Int,
  tx: Transaction,
  redemption_script: ScriptHash,
  token_info: RedemptionTokenInfo,
  return_address: VerificationKeyHash,
  expiry_slot: Int,
) -> Bool {
  // Warp route UTXO should release `amount` tokens
  let input_tokens = quantity_of(own_input.value, policy_id, asset_name)
  let output_tokens = quantity_of(own_output.value, policy_id, asset_name)
  expect input_tokens - output_tokens == amount

  // Redemption UTXO should be created with correct datum and tokens
  expect
    redemption_utxo_created(
      tx,
      redemption_script,
      recipient,
      token_info,
      amount,
      return_address,
      expiry_slot,
      Some((policy_id, asset_name)),
    )

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

/// Validate that synthetic tokens are minted to a redemption UTXO
fn validate_synthetic_redemption(
  minting_policy: ScriptHash,
  recipient: ByteArray,
  amount: Int,
  tx: Transaction,
  redemption_script: ScriptHash,
  token_info: RedemptionTokenInfo,
  return_address: VerificationKeyHash,
  expiry_slot: Int,
) -> Bool {
  // Check that exactly +amount is minted
  let minted = get_minted_amount(tx, minting_policy)
  expect minted == amount

  // Redemption UTXO should be created with correct datum and tokens
  // The asset name for synthetic tokens is empty
  expect
    redemption_utxo_created(
      tx,
      redemption_script,
      recipient,
      token_info,
      amount,
      return_address,
      expiry_slot,
      Some((minting_policy, "")),
    )

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

/// Validate that ADA is released to a redemption UTXO
fn validate_native_redemption(
  own_input: Output,
  own_output: Output,
  recipient: ByteArray,
  amount: Int,
  tx: Transaction,
  redemption_script: ScriptHash,
  return_address: VerificationKeyHash,
  expiry_slot: Int,
) -> Bool {
  // Warp route UTXO should release `amount` lovelace
  let input_ada = lovelace_of(own_input.value)
  let output_ada = lovelace_of(own_output.value)
  expect input_ada - output_ada >= amount

  // Redemption UTXO should be created with correct datum and ADA
  // For native ADA, no additional tokens - just the ADA amount
  expect
    redemption_utxo_created(
      tx,
      redemption_script,
      recipient,
      RedemptionAda,
      amount,
      return_address,
      expiry_slot,
      None,
    )

  True
}

// ============================================================================
// Redemption UTXO Helpers
// ============================================================================

/// Validate that a redemption UTXO is created with correct datum and tokens
/// This is the core function for the two-phase redemption pattern
fn redemption_utxo_created(
  tx: Transaction,
  redemption_script: ScriptHash,
  recipient: ByteArray,
  token_info: RedemptionTokenInfo,
  amount: Int,
  return_address: VerificationKeyHash,
  expiry_slot: Int,
  expected_tokens: Option<(PolicyId, AssetName)>,
) -> Bool {
  // Build the expected redemption datum
  let expected_datum =
    RedemptionDatum {
      recipient,
      token_info,
      amount,
      return_address,
      expiry_slot,
    }

  // Find an output at the redemption script address with the correct datum
  list.any(
    tx.outputs,
    fn(output) {
      // Check address is the redemption script
      let address_valid =
        when output.address.payment_credential is {
          Script(hash) -> hash == redemption_script
          _ -> False
        }

      // Check datum matches
      let datum_valid =
        when output.datum is {
          InlineDatum(datum_data) -> {
            expect actual_datum: RedemptionDatum = datum_data
            actual_datum == expected_datum
          }
          _ -> False
        }

      // Check tokens if specified
      let tokens_valid =
        when expected_tokens is {
          Some((policy_id, asset_name)) ->
            quantity_of(output.value, policy_id, asset_name) >= amount
          None ->
            // For native ADA redemption, check lovelace
            lovelace_of(output.value) >= amount
        }

      address_valid && datum_valid && tokens_valid
    },
  )
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Verify the mailbox minted a processed message NFT for this message_id
/// This proves the mailbox ISM-validated this exact message in this transaction
fn processed_nft_minted(
  processed_messages_nft_policy: PolicyId,
  message_id: ByteArray,
  tx: Transaction,
) -> Bool {
  let policy_mints = assets.tokens(tx.mint, processed_messages_nft_policy)
  when dict.get(policy_mints, message_id) is {
    Some(quantity) -> quantity == 1
    None -> False
  }
}

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

/// Decode EVM TokenMessage body (wire format from other chains)
/// EVM TokenMessage format (from TokenMessage.sol):
/// - bytes 0-31: recipient (bytes32) - Cardano credential padded with 4 leading zeros
/// - bytes 32-63: amount (uint256, big-endian) - wire format amount (remote_decimals)
/// - bytes 64+: metadata (optional, ignored)
///
/// Note: The amount is a full 32-byte uint256. We read all 32 bytes to handle
/// large wire amounts (e.g., 35 * 10^18 for 35 tokens with 18 decimal wire format).
/// Aiken's Int type is arbitrary precision so it can handle the full uint256.
/// The decimal conversion (e.g., dividing by 10^12) happens after parsing.
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

    // Extract amount: bytes 32-63 (full 32 bytes uint256, big-endian)
    // We read all 32 bytes to handle large wire amounts that exceed u64
    // (e.g., 35 * 10^18 > 2^64 for tokens using 18 decimal wire format)
    let amount_bytes = bytearray.slice(body, 32, 63)
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

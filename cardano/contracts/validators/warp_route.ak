use aiken/collection/dict
use aiken/collection/list
use aiken/crypto.{keccak_256}
use aiken/primitive/bytearray
use cardano/address.{Credential, Script}
use cardano/assets.{lovelace_of, quantity_of, tokens}
use cardano/transaction.{InlineDatum, Output, OutputReference, Transaction}
use types.{
  AssetName, Domain, HyperlaneAddress, Message, PolicyId, ScriptHash,
  WarpRouteDatum, WarpRouteRedeemer, WarpTransferBody,
  TransferRemote, ReceiveTransfer, EnrollRemoteRoute,
  Collateral, Synthetic, Native,
  encode_message,
}
use utils.{assoc_find, find_input, is_signed_by}

/// Warp Route Validator
/// Enables token bridging between Cardano and other Hyperlane-connected chains
///
/// Inbound (Other → Cardano):
/// 1. Relayer processes message from origin warp route
/// 2. Calls ReceiveTransfer → tokens sent directly to recipient wallet address
/// 3. minUTxO ADA is covered by source chain fees (relayer tops up if needed)
validator warp_route(
  mailbox_policy_id: PolicyId,
  processed_messages_nft_policy: PolicyId,
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

      ReceiveTransfer { message, message_id } -> {
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
  expect amount > 0
  expect Some(_remote_route) = assoc_find(datum.config.remote_routes, destination)
  expect bytearray.length(recipient) == 32

  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address

  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

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

  expect mailbox_dispatch_called(mailbox_policy_id, tx)

  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: WarpRouteDatum = cont_datum_data
  expect cont_datum.config == datum.config
  expect cont_datum.owner == datum.owner
  expect cont_datum.total_bridged == datum.total_bridged + amount
  expect cont_datum.ism == datum.ism

  True
}

/// Validate inbound transfer (receiving tokens from another chain)
/// Sends tokens directly to the recipient wallet address decoded from message body
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
) -> Bool {
  // 1. Verify sender is an enrolled remote route
  expect Some(expected_sender) = assoc_find(datum.config.remote_routes, origin)
  expect sender == expected_sender

  // 2. Verify mailbox is calling us
  expect mailbox_is_caller(mailbox_policy_id, tx)

  // 3. Verify the mailbox validated THIS exact message_id
  expect processed_nft_minted(processed_messages_nft_policy, message_id, tx)

  // 4. Decode the EVM TokenMessage body
  expect Some(wire_transfer) = decode_evm_token_message(body)
  expect wire_transfer.amount > 0

  // 5. Convert wire amount to local amount
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

  // 8. Validate tokens sent directly to recipient
  let transfer_valid =
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
  expect transfer_valid

  // 9. Verify datum updated (decrement total_bridged by LOCAL amount)
  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: WarpRouteDatum = cont_datum_data
  expect cont_datum.config == datum.config
  expect cont_datum.owner == datum.owner
  expect cont_datum.total_bridged == datum.total_bridged - local_amount
  expect cont_datum.ism == datum.ism

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
  expect is_signed_by(tx, datum.owner)
  expect bytearray.length(route) == 32

  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address
  let own_value = own_input.output.value

  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  expect lovelace_of(continuation.value) >= lovelace_of(own_value)

  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: WarpRouteDatum = cont_datum_data

  let new_routes = upsert_route(datum.config.remote_routes, domain, route)
  let expected_config =
    types.WarpRouteConfig { ..datum.config, remote_routes: new_routes }

  expect cont_datum.config == expected_config
  expect cont_datum.owner == datum.owner
  expect cont_datum.total_bridged == datum.total_bridged
  expect cont_datum.ism == datum.ism

  True
}

// ============================================================================
// Collateral Token Helpers
// ============================================================================

fn validate_collateral_lock(
  own_input: Output,
  own_output: Output,
  policy_id: PolicyId,
  asset_name: AssetName,
  amount: Int,
) -> Bool {
  let input_tokens = quantity_of(own_input.value, policy_id, asset_name)
  let output_tokens = quantity_of(own_output.value, policy_id, asset_name)
  output_tokens >= input_tokens + amount
}

/// Validate collateral tokens released to recipient wallet
fn validate_collateral_release(
  own_input: Output,
  own_output: Output,
  policy_id: PolicyId,
  asset_name: AssetName,
  recipient: ByteArray,
  amount: Int,
  tx: Transaction,
) -> Bool {
  let input_tokens = quantity_of(own_input.value, policy_id, asset_name)
  let output_tokens = quantity_of(own_output.value, policy_id, asset_name)
  expect input_tokens - output_tokens == amount

  expect recipient_receives_tokens(tx, recipient, policy_id, asset_name, amount)
  True
}

// ============================================================================
// Synthetic Token Helpers
// ============================================================================

fn validate_synthetic_burn(
  minting_policy: ScriptHash,
  amount: Int,
  tx: Transaction,
) -> Bool {
  let minted = get_minted_amount(tx, minting_policy)
  minted == -amount
}

/// Validate synthetic tokens minted to recipient wallet
fn validate_synthetic_mint(
  minting_policy: ScriptHash,
  recipient: ByteArray,
  amount: Int,
  tx: Transaction,
) -> Bool {
  let minted = get_minted_amount(tx, minting_policy)
  expect minted == amount

  expect recipient_receives_tokens(tx, recipient, minting_policy, "", amount)
  True
}

// ============================================================================
// Native ADA Helpers
// ============================================================================

fn validate_native_lock(own_input: Output, own_output: Output, amount: Int) -> Bool {
  let input_ada = lovelace_of(own_input.value)
  let output_ada = lovelace_of(own_output.value)
  output_ada >= input_ada + amount
}

/// Validate ADA released to recipient wallet
fn validate_native_release(
  own_input: Output,
  own_output: Output,
  recipient: ByteArray,
  amount: Int,
  tx: Transaction,
) -> Bool {
  let input_ada = lovelace_of(own_input.value)
  let output_ada = lovelace_of(own_output.value)
  expect input_ada - output_ada >= amount

  // Recipient must receive at least the transferred amount
  expect recipient_receives_ada(tx, recipient, amount)
  True
}

// ============================================================================
// Recipient Output Helpers
// ============================================================================

/// Check if recipient receives tokens (at a wallet or script address)
fn recipient_receives_tokens(
  tx: Transaction,
  recipient: ByteArray,
  policy_id: PolicyId,
  asset_name: AssetName,
  amount: Int,
) -> Bool {
  list.any(
    tx.outputs,
    fn(output) {
      let credential_matches =
        when output.address.payment_credential is {
          Credential.VerificationKey(hash) -> hash == recipient
          Script(hash) -> hash == recipient
        }
      credential_matches && quantity_of(output.value, policy_id, asset_name) >= amount
    },
  )
}

/// Check if recipient receives ADA
fn recipient_receives_ada(
  tx: Transaction,
  recipient: ByteArray,
  amount: Int,
) -> Bool {
  list.any(
    tx.outputs,
    fn(output) {
      let credential_matches =
        when output.address.payment_credential is {
          Credential.VerificationKey(hash) -> hash == recipient
          Script(hash) -> hash == recipient
        }
      credential_matches && lovelace_of(output.value) >= amount
    },
  )
}

// ============================================================================
// Helper Functions
// ============================================================================

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

fn mailbox_is_caller(mailbox_policy_id: PolicyId, tx: Transaction) -> Bool {
  list.any(
    tx.inputs,
    fn(input) {
      let policy_tokens = tokens(input.output.value, mailbox_policy_id)
      !dict.is_empty(policy_tokens)
    },
  )
}

fn mailbox_dispatch_called(mailbox_policy_id: PolicyId, tx: Transaction) -> Bool {
  mailbox_is_caller(mailbox_policy_id, tx)
}

fn decode_evm_token_message(body: ByteArray) -> Option<WarpTransferBody> {
  let len = bytearray.length(body)
  if len < 64 {
    None
  } else {
    let recipient = bytearray.slice(body, 4, 31)
    let amount_bytes = bytearray.slice(body, 32, 63)
    let amount = bytes_to_int(amount_bytes)
    Some(WarpTransferBody { recipient, amount })
  }
}

fn convert_decimals(wire_amount: Int, remote_decimals: Int, local_decimals: Int) -> Int {
  if local_decimals >= remote_decimals {
    let multiplier = pow10(local_decimals - remote_decimals)
    wire_amount * multiplier
  } else {
    let divisor = pow10(remote_decimals - local_decimals)
    wire_amount / divisor
  }
}

fn pow10(n: Int) -> Int {
  if n <= 0 {
    1
  } else {
    10 * pow10(n - 1)
  }
}

fn bytes_to_int(bytes: ByteArray) -> Int {
  bytearray.foldl(bytes, 0, fn(byte, acc) { acc * 256 + byte })
}

fn get_minted_amount(tx: Transaction, policy_id: PolicyId) -> Int {
  let policy_mints = tokens(tx.mint, policy_id)
  dict.foldl(policy_mints, 0, fn(_asset_name, quantity, acc) { acc + quantity })
}

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

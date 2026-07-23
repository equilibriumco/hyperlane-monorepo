use aiken/collection/dict
use aiken/collection/list
use aiken/collection/pairs
use aiken/crypto.{keccak_256}
use aiken/interval.{Interval, IntervalBound, NegativeInfinity, PositiveInfinity}
use aiken/primitive/bytearray
use cardano/address.{Address, Credential, Script}
use cardano/assets.{from_asset, lovelace_of, quantity_of, tokens, zero}
use cardano/transaction.{
  InlineDatum, Input, NoDatum, Output, OutputReference, Spend, Transaction,
}
use types.{
  AssetName, Collateral, Dispatch, Domain, EnrollRemoteRoute, HyperlaneAddress,
  MailboxRedeemer, Message, MigrateWarp, Native, PolicyId, Process,
  ReceiveTransfer, ScriptHash, SetDestinationGas, Synthetic, TransferRemote,
  WarpRouteDatum,
  WarpRouteRedeemer, WarpTransferBody, encode_message,
}
use utils.{
  assoc_find, find_input, is_signed_by, native_tokens_preserved,
  native_tokens_preserved_except, validate_migrate, value_preserved,
}

/// Warp Route Validator
/// Enables token bridging between Cardano and other Hyperlane-connected chains
///
/// Inbound (Other → Cardano):
/// 1. Relayer processes message from origin warp route
/// 2. Calls ReceiveTransfer → tokens sent directly to recipient wallet address
/// 3. minUTxO ADA is covered by source chain fees (relayer tops up if needed)
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

      ReceiveTransfer { message, message_id } -> {
        expect keccak_256(encode_message(message)) == message_id
        validate_receive_transfer(
          warp_datum,
          message,
          tx,
          own_ref,
          mailbox_policy_id,
          message_id,
        )
      }

      EnrollRemoteRoute { domain, route } ->
        validate_enroll_remote_route(warp_datum, domain, route, tx, own_ref)

      SetDestinationGas { domain, gas } ->
        validate_set_destination_gas(warp_datum, domain, gas, tx, own_ref)

      MigrateWarp { new_address } -> {
        validate_migrate(
          warp_datum.owner,
          new_address,
          tx,
          own_ref,
          warp_datum.version,
        )
      }
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
  // Reject local amounts that truncate to zero in the remote wire format.
  expect
    convert_local_to_wire(
      amount,
      datum.config.decimals,
      datum.config.remote_decimals,
    ) > 0
  expect Some(remote_route) =
    assoc_find(datum.config.remote_routes, destination)
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

      Native -> validate_native_lock(own_input.output, continuation, amount)
    }
  expect token_valid

  expect
    mailbox_dispatch_matches(
      mailbox_policy_id,
      destination,
      remote_route,
      recipient,
      amount,
      datum.config.decimals,
      datum.config.remote_decimals,
      own_ref,
      tx,
    )

  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: WarpRouteDatum = cont_datum_data
  expect cont_datum.config == datum.config
  expect cont_datum.owner == datum.owner
  expect cont_datum.total_bridged == datum.total_bridged + amount
  expect cont_datum.ism == datum.ism
  expect cont_datum.destination_gas == datum.destination_gas
  expect cont_datum.version == datum.version

  // All native tokens (including state NFT) must be preserved.
  // For Native/Collateral lock, token quantities only increase so this always holds.
  // For Synthetic burn, the UTXO's native tokens don't change.
  expect native_tokens_preserved(own_input.output.value, continuation.value)

  True
}

/// Validate inbound transfer (receiving tokens from another chain)
/// Sends tokens directly to the recipient wallet address decoded from message body
fn validate_receive_transfer(
  datum: WarpRouteDatum,
  message: Message,
  tx: Transaction,
  own_ref: OutputReference,
  mailbox_policy_id: PolicyId,
  message_id: ByteArray,
) -> Bool {
  // 1. Verify sender is an enrolled remote route
  expect Some(expected_sender) =
    assoc_find(datum.config.remote_routes, message.origin)
  expect message.sender == expected_sender

  // 2. Bind this warp input to the exact message recipient policy.
  expect Some(own_input) = find_input(tx, own_ref)
  expect bytearray.length(message.recipient) == 32
  expect bytearray.at(message.recipient, 0) == 1
  let recipient_policy = bytearray.drop(message.recipient, 4)
  expect quantity_of(own_input.output.value, recipient_policy, #"") == 1

  // 3. Require the canonical mailbox input to process this exact message.
  expect mailbox_process_matches(mailbox_policy_id, message, message_id, tx)

  // 4. Decode the EVM TokenMessage body
  expect Some(wire_transfer) = decode_evm_token_message(message.body)
  expect wire_transfer.amount > 0

  // 5. Convert wire amount to local amount
  let local_amount =
    convert_decimals(
      wire_transfer.amount,
      datum.config.remote_decimals,
      datum.config.decimals,
    )
  expect local_amount > 0

  let own_address = own_input.output.address

  // 6. Find continuation output
  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  // 7. Validate tokens sent directly to recipient
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

  // 8. Verify datum updated (decrement total_bridged by LOCAL amount)
  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: WarpRouteDatum = cont_datum_data
  expect cont_datum.config == datum.config
  expect cont_datum.owner == datum.owner
  expect cont_datum.total_bridged == datum.total_bridged - local_amount
  expect cont_datum.ism == datum.ism
  expect cont_datum.destination_gas == datum.destination_gas
  expect cont_datum.version == datum.version

  // 9. Ensure state NFT is preserved in continuation.
  //    For collateral release, the collateral token quantity decreases — exclude it.
  //    For native release, only lovelace changes — native tokens check passes.
  //    For synthetic mint, no UTXO value change — native tokens check passes.
  let nft_ok =
    when datum.config.token_type is {
      Collateral { policy_id, asset_name } ->
        native_tokens_preserved_except(
          own_input.output.value,
          continuation.value,
          policy_id,
          asset_name,
        )
      _ -> native_tokens_preserved(own_input.output.value, continuation.value)
    }
  expect nft_ok

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

  expect value_preserved(own_value, continuation.value)

  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: WarpRouteDatum = cont_datum_data

  let new_routes = upsert_route(datum.config.remote_routes, domain, route)
  let expected_config =
    types.WarpRouteConfig { ..datum.config, remote_routes: new_routes }

  expect cont_datum.config == expected_config
  expect cont_datum.owner == datum.owner
  expect cont_datum.total_bridged == datum.total_bridged
  expect cont_datum.ism == datum.ism
  expect cont_datum.destination_gas == datum.destination_gas
  expect cont_datum.version == datum.version

  True
}

/// Owner-gated update of the destination gas for one remote domain.
///
/// This is the recipient-specific half of a gas quote: what delivering to the
/// counterpart route costs beyond the IGP's recipient-independent overhead.
/// Holding it on the route means a sender quotes from the domain alone rather
/// than having to know the number, and one operator decision covers everyone.
fn validate_set_destination_gas(
  datum: WarpRouteDatum,
  domain: Domain,
  gas: Int,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  expect is_signed_by(tx, datum.owner)
  expect gas >= 0

  // Gas for a domain with no enrolled route could never be quoted against,
  // so it is rejected rather than stored where nothing would read it.
  expect list.any(datum.config.remote_routes, fn(entry) { entry.1st == domain })

  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address
  let own_value = own_input.output.value

  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  expect value_preserved(own_value, continuation.value)

  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: WarpRouteDatum = cont_datum_data

  expect cont_datum.config == datum.config
  expect cont_datum.owner == datum.owner
  expect cont_datum.total_bridged == datum.total_bridged
  expect cont_datum.ism == datum.ism
  expect cont_datum.destination_gas == upsert_destination_gas(
    datum.destination_gas,
    domain,
    gas,
  )

  True
}

/// Insert or replace the gas for `domain`, preserving the order of the rest.
fn upsert_destination_gas(
  entries: List<(Domain, Int)>,
  domain: Domain,
  gas: Int,
) -> List<(Domain, Int)> {
  when entries is {
    [] -> [(domain, gas)]
    [(d, g), ..rest] ->
      if d == domain {
        [(domain, gas), ..rest]
      } else {
        [(d, g), ..upsert_destination_gas(rest, domain, gas)]
      }
  }
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

fn validate_native_lock(
  own_input: Output,
  own_output: Output,
  amount: Int,
) -> Bool {
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
  // Continuation must keep at least (input - amount), preventing extra ADA drainage.
  // The payer may top up the continuation to meet minUTxO requirements.
  expect output_ada >= input_ada - amount

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

fn find_mailbox_input(
  mailbox_policy_id: PolicyId,
  tx: Transaction,
) -> Option<Input> {
  list.find(
    tx.inputs,
    fn(input) {
      quantity_of(
        input.output.value,
        mailbox_policy_id,
        #"4d61696c626f78205374617465",
      ) == 1
    },
  )
}

fn mailbox_process_matches(
  mailbox_policy_id: PolicyId,
  expected_message: Message,
  expected_message_id: ByteArray,
  tx: Transaction,
) -> Bool {
  expect Some(mailbox_input) = find_mailbox_input(mailbox_policy_id, tx)
  when pairs.get_first(tx.redeemers, Spend(mailbox_input.output_reference)) is {
    Some(raw_redeemer) -> {
      expect mailbox_redeemer: MailboxRedeemer = raw_redeemer
      when mailbox_redeemer is {
        Process { message, message_id, .. } ->
          message == expected_message && message_id == expected_message_id
        _ -> False
      }
    }
    None -> False
  }
}

fn mailbox_dispatch_matches(
  mailbox_policy_id: PolicyId,
  expected_destination: Domain,
  expected_remote_route: HyperlaneAddress,
  expected_recipient: HyperlaneAddress,
  local_amount: Int,
  local_decimals: Int,
  remote_decimals: Int,
  warp_ref: OutputReference,
  tx: Transaction,
) -> Bool {
  expect Some(mailbox_input) = find_mailbox_input(mailbox_policy_id, tx)
  when pairs.get_first(tx.redeemers, Spend(mailbox_input.output_reference)) is {
    Some(raw_redeemer) -> {
      expect mailbox_redeemer: MailboxRedeemer = raw_redeemer
      when mailbox_redeemer is {
        Dispatch { destination, recipient, body, sender_ref, .. } -> {
          expect bytearray.length(body) >= 64
          let wire_recipient = bytearray.slice(body, 0, 31)
          let wire_amount = bytes_to_int(bytearray.slice(body, 32, 63))
          destination == expected_destination && recipient == expected_remote_route && sender_ref == warp_ref && wire_recipient == expected_recipient && wire_amount == convert_local_to_wire(
            local_amount,
            local_decimals,
            remote_decimals,
          )
        }
        _ -> False
      }
    }
    None -> False
  }
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

fn convert_decimals(
  wire_amount: Int,
  remote_decimals: Int,
  local_decimals: Int,
) -> Int {
  if local_decimals >= remote_decimals {
    let multiplier = pow10(local_decimals - remote_decimals)
    wire_amount * multiplier
  } else {
    let divisor = pow10(remote_decimals - local_decimals)
    wire_amount / divisor
  }
}

fn convert_local_to_wire(
  local_amount: Int,
  local_decimals: Int,
  remote_decimals: Int,
) -> Int {
  if remote_decimals >= local_decimals {
    local_amount * pow10(remote_decimals - local_decimals)
  } else {
    local_amount / pow10(local_decimals - remote_decimals)
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

// ============================================================================
// Mailbox binding tests
// ============================================================================

const test_mailbox_policy =
  #"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

const test_message_id =
  #"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"

const test_remote_route =
  #"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"

const test_recipient =
  #"000000000000000000000000eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"

fn test_mailbox_ref() -> OutputReference {
  OutputReference {
    transaction_id: #"0000000000000000000000000000000000000000000000000000000000000001",
    output_index: 0,
  }
}

fn test_warp_ref() -> OutputReference {
  OutputReference {
    transaction_id: #"0000000000000000000000000000000000000000000000000000000000000002",
    output_index: 0,
  }
}

fn test_message() -> Message {
  Message {
    version: 3,
    nonce: 1,
    origin: 1,
    sender: test_remote_route,
    destination: 2,
    recipient: #"01000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    body: #"abcd",
  }
}

fn test_validity_range() -> Interval<Int> {
  Interval {
    lower_bound: IntervalBound {
      bound_type: NegativeInfinity,
      is_inclusive: False,
    },
    upper_bound: IntervalBound {
      bound_type: PositiveInfinity,
      is_inclusive: False,
    },
  }
}

fn test_tx(mailbox_redeemer: MailboxRedeemer) -> Transaction {
  test_tx_with_mailbox_asset(mailbox_redeemer, #"4d61696c626f78205374617465")
}

fn test_tx_with_mailbox_asset(
  mailbox_redeemer: MailboxRedeemer,
  mailbox_asset_name: ByteArray,
) -> Transaction {
  let raw_redeemer: Data = mailbox_redeemer
  Transaction {
    inputs: [
      Input {
        output_reference: test_mailbox_ref(),
        output: Output {
          address: Address {
            payment_credential: Script(
              #"ffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            ),
            stake_credential: None,
          },
          value: from_asset(test_mailbox_policy, mailbox_asset_name, 1),
          datum: NoDatum,
          reference_script: None,
        },
      },
    ],
    reference_inputs: [],
    outputs: [],
    fee: 0,
    mint: zero,
    certificates: [],
    withdrawals: [],
    validity_range: test_validity_range(),
    extra_signatories: [],
    redeemers: [Pair(Spend(test_mailbox_ref()), raw_redeemer)],
    datums: dict.empty,
    id: #"0000",
    votes: [],
    proposal_procedures: [],
    current_treasury_amount: None,
    treasury_donation: None,
  }
}

test process_binding_accepts_exact_message() {
  let redeemer =
    Process {
      message: test_message(),
      metadata: #"01",
      message_id: test_message_id,
      smt_proof: [],
    }
  mailbox_process_matches(
    test_mailbox_policy,
    test_message(),
    test_message_id,
    test_tx(redeemer),
  )
}

test process_binding_rejects_dispatch() {
  let redeemer =
    Dispatch {
      destination: 2,
      recipient: test_remote_route,
      body: #"",
      sender_ref: test_warp_ref(),
      hook_metadata: #"",
    }
  !mailbox_process_matches(
    test_mailbox_policy,
    test_message(),
    test_message_id,
    test_tx(redeemer),
  )
}

test process_binding_rejects_reference_token() fail {
  let redeemer =
    Process {
      message: test_message(),
      metadata: #"01",
      message_id: test_message_id,
      smt_proof: [],
    }
  mailbox_process_matches(
    test_mailbox_policy,
    test_message(),
    test_message_id,
    test_tx_with_mailbox_asset(redeemer, #"726566"),
  )
}

fn test_dispatch_body(wire_amount: Int) -> ByteArray {
  bytearray.concat(
    test_recipient,
    bytearray.from_int_big_endian(wire_amount, 32),
  )
}

test dispatch_binding_accepts_exact_transfer() {
  let redeemer =
    Dispatch {
      destination: 2,
      recipient: test_remote_route,
      body: test_dispatch_body(500),
      sender_ref: test_warp_ref(),
      hook_metadata: #"",
    }
  mailbox_dispatch_matches(
    test_mailbox_policy,
    2,
    test_remote_route,
    test_recipient,
    5,
    6,
    8,
    test_warp_ref(),
    test_tx(redeemer),
  )
}

test dispatch_binding_rejects_inflated_wire_amount() {
  let redeemer =
    Dispatch {
      destination: 2,
      recipient: test_remote_route,
      body: test_dispatch_body(50_000),
      sender_ref: test_warp_ref(),
      hook_metadata: #"",
    }
  !mailbox_dispatch_matches(
    test_mailbox_policy,
    2,
    test_remote_route,
    test_recipient,
    5,
    6,
    8,
    test_warp_ref(),
    test_tx(redeemer),
  )
}

// ============================================================================
// Destination gas tests
// ============================================================================

const test_owner = #"00000000000000000000000000000000000000000000000000000007"

const test_gas_domain: Domain = 1

fn gas_datum(destination_gas: List<(Domain, Int)>) -> WarpRouteDatum {
  WarpRouteDatum { version: 0, 
    config: types.WarpRouteConfig {
      token_type: Native,
      decimals: 6,
      remote_decimals: 18,
      remote_routes: [(test_gas_domain, test_remote_route)],
    },
    owner: test_owner,
    total_bridged: 0,
    ism: None,
    destination_gas,
  }
}

fn gas_route_address() -> Address {
  Address {
    payment_credential: Script(
      #"00000000000000000000000000000000000000000000000000000008",
    ),
    stake_credential: None,
  }
}

fn set_gas_tx(
  datum: WarpRouteDatum,
  cont_datum: WarpRouteDatum,
  signed: Bool,
) -> Transaction {
  let value = assets.from_lovelace(5000000)
  Transaction {
    inputs: [
      Input {
        output_reference: test_warp_ref(),
        output: Output {
          address: gas_route_address(),
          value,
          datum: InlineDatum(datum),
          reference_script: None,
        },
      },
    ],
    reference_inputs: [],
    outputs: [
      Output {
        address: gas_route_address(),
        value,
        datum: InlineDatum(cont_datum),
        reference_script: None,
      },
    ],
    fee: 0,
    mint: assets.zero,
    certificates: [],
    withdrawals: [],
    validity_range: interval.everything,
    extra_signatories: if signed {
      [test_owner]
    } else {
      []
    },
    redeemers: [],
    datums: dict.empty,
    id: #"0000",
    votes: [],
    proposal_procedures: [],
    current_treasury_amount: None,
    treasury_donation: None,
  }
}

test owner_sets_destination_gas_on_an_empty_route() {
  validate_set_destination_gas(
    gas_datum([]),
    test_gas_domain,
    1800000,
    set_gas_tx(gas_datum([]), gas_datum([(test_gas_domain, 1800000)]), True),
    test_warp_ref(),
  )
}

test owner_replaces_an_existing_destination_gas() {
  validate_set_destination_gas(
    gas_datum([(test_gas_domain, 1)]),
    test_gas_domain,
    1800000,
    set_gas_tx(
      gas_datum([(test_gas_domain, 1)]),
      gas_datum([(test_gas_domain, 1800000)]),
      True,
    ),
    test_warp_ref(),
  )
}

test non_owner_cannot_set_destination_gas() fail {
  validate_set_destination_gas(
    gas_datum([]),
    test_gas_domain,
    1800000,
    set_gas_tx(gas_datum([]), gas_datum([(test_gas_domain, 1800000)]), False),
    test_warp_ref(),
  )
}

/// The continuation must carry the value the redeemer declared, so a signed
/// transaction cannot quietly store something else.
test declared_gas_must_match_the_continuation() fail {
  validate_set_destination_gas(
    gas_datum([]),
    test_gas_domain,
    1800000,
    set_gas_tx(gas_datum([]), gas_datum([(test_gas_domain, 1)]), True),
    test_warp_ref(),
  )
}

/// Gas for a domain with no enrolled route could never be quoted against.
test cannot_set_gas_for_an_unenrolled_domain() fail {
  validate_set_destination_gas(
    gas_datum([]),
    test_gas_domain + 99,
    1800000,
    set_gas_tx(
      gas_datum([]),
      gas_datum([(test_gas_domain + 99, 1800000)]),
      True,
    ),
    test_warp_ref(),
  )
}

test setting_gas_cannot_change_the_owner() fail {
  let cont =
    WarpRouteDatum {
      ..gas_datum([(test_gas_domain, 1800000)]),
      owner: #"00000000000000000000000000000000000000000000000000000099",
    }
  validate_set_destination_gas(
    gas_datum([]),
    test_gas_domain,
    1800000,
    set_gas_tx(gas_datum([]), cont, True),
    test_warp_ref(),
  )
}

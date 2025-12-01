use aiken/collection/list
use aiken/primitive/bytearray
use cardano/address.{Address, Credential, Script}
use cardano/assets
use cardano/transaction.{InlineDatum, Output, OutputReference, Transaction}
use types.{Domain, GasOracleConfig, IgpDatum, IgpRedeemer, PayForGas, Claim, SetGasOracle}
use utils.{find_input, is_signed_by, value_preserved}

/// Interchain Gas Paymaster (IGP) Validator
/// Handles gas payments for cross-chain messages
///
/// The IGP allows:
/// - Users to pay for gas when dispatching messages (PayForGas)
/// - Beneficiary to claim accumulated fees (Claim)
/// - Owner to configure gas oracles per destination (SetGasOracle)
validator igp {
  spend(
    datum: Option<IgpDatum>,
    redeemer: IgpRedeemer,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    expect Some(igp_datum) = datum

    when redeemer is {
      PayForGas { message_id, destination, gas_amount } ->
        validate_pay_for_gas(
          igp_datum,
          message_id,
          destination,
          gas_amount,
          tx,
          own_ref,
        )

      Claim { amount } ->
        validate_claim(igp_datum, amount, tx, own_ref)

      SetGasOracle { domain, config } ->
        validate_set_gas_oracle(igp_datum, domain, config, tx, own_ref)
    }
  }

  else(_) {
    fail
  }
}

/// Validate a gas payment for a message
fn validate_pay_for_gas(
  datum: IgpDatum,
  message_id: ByteArray,
  destination: Domain,
  gas_amount: Int,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // 1. Verify message_id is 32 bytes
  expect bytearray.length(message_id) == 32

  // 2. Calculate required payment
  let required_lovelace = calculate_gas_payment(datum, destination, gas_amount)

  // 3. Find own input
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address
  let own_value = own_input.output.value

  // 4. Find continuation output (same address)
  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  // 5. Verify the IGP received the payment
  // The continuation should have at least own_value + required_lovelace
  let expected_lovelace = assets.lovelace_of(own_value) + required_lovelace
  expect assets.lovelace_of(continuation.value) >= expected_lovelace

  // 6. Verify datum is preserved
  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: IgpDatum = cont_datum_data
  expect datum_preserved(datum, cont_datum)

  // 7. Verify gas payment event is recorded
  // This is implicit in the transaction - the redeemer itself serves as the event log
  True
}

/// Validate claiming accumulated fees
fn validate_claim(
  datum: IgpDatum,
  amount: Int,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // 1. Only beneficiary can claim
  expect beneficiary_receives_payment(datum.beneficiary, amount, tx)

  // 2. Find own input
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address
  let own_value = own_input.output.value

  // 3. Find continuation output
  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  // 4. Verify the amount is deducted from IGP
  let input_lovelace = assets.lovelace_of(own_value)
  let output_lovelace = assets.lovelace_of(continuation.value)
  expect input_lovelace - output_lovelace == amount

  // 5. Verify datum is preserved
  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: IgpDatum = cont_datum_data
  expect datum_preserved(datum, cont_datum)

  True
}

/// Validate setting a gas oracle for a domain
fn validate_set_gas_oracle(
  datum: IgpDatum,
  domain: Domain,
  config: GasOracleConfig,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // 1. Only owner can update gas oracles
  expect is_signed_by(tx, datum.owner)

  // 2. Validate config
  expect config.gas_price > 0
  expect config.token_exchange_rate > 0

  // 3. Find own input
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address
  let own_value = own_input.output.value

  // 4. Find continuation output
  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  // 5. Verify value is preserved
  expect value_preserved(own_value, continuation.value)

  // 6. Verify datum is updated correctly
  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: IgpDatum = cont_datum_data

  // New gas oracles should have the domain updated
  let new_gas_oracles = update_gas_oracle(datum.gas_oracles, domain, config)
  expect cont_datum.gas_oracles == new_gas_oracles
  expect cont_datum.owner == datum.owner
  expect cont_datum.beneficiary == datum.beneficiary
  expect cont_datum.default_gas_limit == datum.default_gas_limit

  True
}

/// Calculate required gas payment in lovelace
fn calculate_gas_payment(
  datum: IgpDatum,
  destination: Domain,
  gas_amount: Int,
) -> Int {
  // Find gas oracle for destination
  let config =
    when find_gas_oracle(datum.gas_oracles, destination) is {
      Some(c) -> c
      None ->
        // Use default values if no oracle configured
        GasOracleConfig { gas_price: 1, token_exchange_rate: 1000000 }
    }

  // Use requested gas amount or default
  let effective_gas =
    if gas_amount > 0 {
      gas_amount
    } else {
      datum.default_gas_limit
    }

  // Calculate: gas_amount * gas_price * token_exchange_rate / scale_factor
  // Scale factor is 10^18 to match EVM precision
  // For simplicity, we use a scaled-down calculation
  effective_gas * config.gas_price * config.token_exchange_rate / 1000000000000
}

/// Find gas oracle config for a domain
fn find_gas_oracle(
  oracles: List<(Domain, GasOracleConfig)>,
  domain: Domain,
) -> Option<GasOracleConfig> {
  when oracles is {
    [] -> None
    [(d, config), ..rest] ->
      if d == domain {
        Some(config)
      } else {
        find_gas_oracle(rest, domain)
      }
  }
}

/// Update gas oracle for a domain (upsert)
fn update_gas_oracle(
  oracles: List<(Domain, GasOracleConfig)>,
  domain: Domain,
  config: GasOracleConfig,
) -> List<(Domain, GasOracleConfig)> {
  update_gas_oracle_helper(oracles, domain, config, False)
}

fn update_gas_oracle_helper(
  oracles: List<(Domain, GasOracleConfig)>,
  domain: Domain,
  config: GasOracleConfig,
  found: Bool,
) -> List<(Domain, GasOracleConfig)> {
  when oracles is {
    [] ->
      if found {
        []
      } else {
        // Domain not found, add new entry
        [(domain, config)]
      }
    [(d, c), ..rest] ->
      if d == domain {
        // Update existing entry
        [(d, config), ..update_gas_oracle_helper(rest, domain, config, True)]
      } else {
        [(d, c), ..update_gas_oracle_helper(rest, domain, config, found)]
      }
  }
}

/// Verify beneficiary receives the claimed payment
fn beneficiary_receives_payment(
  beneficiary_bytes: ByteArray,
  amount: Int,
  tx: Transaction,
) -> Bool {
  // Find an output to the beneficiary address with at least the claimed amount
  list.any(
    tx.outputs,
    fn(output) {
      // Check if this output is to the beneficiary
      // The beneficiary is stored as a serialized address
      let output_address_bytes = address_to_bytes(output.address)
      output_address_bytes == beneficiary_bytes && assets.lovelace_of(
        output.value,
      ) >= amount
    },
  )
}

/// Convert address to bytes for comparison
fn address_to_bytes(addr: Address) -> ByteArray {
  when addr.payment_credential is {
    Credential.VerificationKey(hash) -> hash
    Script(hash) -> hash
  }
}

/// Verify datum fields are preserved (except gas_oracles for SetGasOracle)
fn datum_preserved(old: IgpDatum, new: IgpDatum) -> Bool {
  and {
    old.owner == new.owner,
    old.beneficiary == new.beneficiary,
    old.gas_oracles == new.gas_oracles,
    old.default_gas_limit == new.default_gas_limit,
  }
}

use aiken/collection/dict
use aiken/collection/list
use aiken/interval
use aiken/primitive/bytearray
use cardano/address.{Address, Credential, Script}
use cardano/assets
use cardano/transaction.{InlineDatum, Output, OutputReference, Transaction}
use types.{Domain, GasOracleConfig, IgpDatum, IgpRedeemer, PayForGas, Claim, SetGasOracle, MigrateIgp}
use utils.{find_input, is_signed_by, validate_migrate, value_preserved, native_tokens_preserved}

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

      MigrateIgp { new_address } -> {
        validate_migrate(igp_datum.owner, new_address, tx, own_ref, igp_datum.version)
      }
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

  // 5. Verify the IGP received exactly the payment.
  //    Comparing the delta rather than the total keeps "how much was paid"
  //    separate from "how much the UTXO already held", and the exact match
  //    means an over-funded output cannot be constructed at all. Overpayment
  //    is unrecoverable for the payer here (there is no refund path, only
  //    `claim` to the beneficiary), so rejecting it is kinder than keeping it.
  let paid =
    assets.lovelace_of(continuation.value) - assets.lovelace_of(own_value)
  expect paid == required_lovelace

  // 6. Verify all native tokens (including state NFT) are preserved
  expect native_tokens_preserved(own_value, continuation.value)

  // 7. Verify datum is preserved
  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: IgpDatum = cont_datum_data
  expect datum_preserved(datum, cont_datum)

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

  // 5. Verify all native tokens (including state NFT) are preserved
  expect native_tokens_preserved(own_value, continuation.value)

  // 6. Verify datum is preserved
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
  expect config.gas_overhead >= 0

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

  True
}

/// Calculate required gas payment in lovelace
fn calculate_gas_payment(
  datum: IgpDatum,
  destination: Domain,
  gas_amount: Int,
) -> Int {
  // A destination with no oracle has no price, so paying for it is rejected
  // rather than defaulted. The former fallback (price 1, rate 1e6) divided out
  // to zero for any realistic gas amount, so a domain enrolled without its
  // oracle configured was payable for free: sends succeeded, nothing was ever
  // delivered, and nothing failed anywhere near the missing config.
  expect Some(config) = find_gas_oracle(datum.gas_oracles, destination)

  // `gas_amount` is the caller's application-level gas: what the recipient
  // costs to run, and nothing else. The destination's recipient-independent
  // overhead is added here rather than trusted to the caller, so omitting it
  // is not a way to pay less.
  expect gas_amount >= 0
  let total_gas = gas_amount + config.gas_overhead
  expect total_gas > 0

  // total_gas * gas_price * token_exchange_rate / scale_factor.
  //
  // The scale factor is 1e12, not the 1e10 Hyperlane uses elsewhere. Exchange
  // rates configured here must be scaled to match; a rate copied from tooling
  // that assumes 1e10 will be off by a factor of 100.
  total_gas * config.gas_price * config.token_exchange_rate / 1000000000000
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
    // Only migration bumps the version; an ordinary spend carrying a different
    // one would leave the next migration computing `old + 1` from a moved base.
    old.version == new.version,
    old.owner == new.owner,
    old.beneficiary == new.beneficiary,
    old.gas_oracles == new.gas_oracles,
  }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

const test_domain: Domain = 11155111

const test_message_id =
  #"424b25342a401ee2029bfe8241e61e3e4ff5209913d03c7dafe46e3f0faa0096"

const test_script_hash =
  #"00000000000000000000000000000000000000000000000000000009"

const test_own_ref =
  OutputReference {
    transaction_id: #"0000000000000000000000000000000000000000000000000000000000000001",
    output_index: 0,
  }

/// Matches the deployed Sepolia oracle: 1 gwei gas price, and an exchange rate
/// scaled against the 1e12 divisor in `calculate_gas_payment`.
fn test_igp_datum() -> IgpDatum {
  IgpDatum { version: 0, 
    owner: #"00000000000000000000000000000000000000000000000000000001",
    beneficiary: #"00000000000000000000000000000000000000000000000000000002",
    gas_oracles: [
      (
        test_domain,
        GasOracleConfig {
          gas_price: 1000000000,
          token_exchange_rate: 7171,
          gas_overhead: 11000,
        },
      ),
    ],
  }
}

fn igp_address() -> Address {
  Address {
    payment_credential: Script(test_script_hash),
    stake_credential: None,
  }
}

/// The IGP holds its state NFT plus whatever lovelace has accumulated.
fn igp_value(lovelace: Int) -> assets.Value {
  assets.merge(
    assets.from_lovelace(lovelace),
    assets.from_asset(test_script_hash, #"494750205374617465", 1),
  )
}

fn pay_tx(held: Int, continuation_lovelace: Int) -> Transaction {
  let datum = InlineDatum(test_igp_datum())
  Transaction {
    inputs: [
      transaction.Input {
        output_reference: test_own_ref,
        output: Output {
          address: igp_address(),
          value: igp_value(held),
          datum,
          reference_script: None,
        },
      },
    ],
    reference_inputs: [],
    outputs: [
      Output {
        address: igp_address(),
        value: igp_value(continuation_lovelace),
        datum,
        reference_script: None,
      },
    ],
    fee: 0,
    mint: assets.zero,
    certificates: [],
    withdrawals: [],
    validity_range: interval.everything,
    extra_signatories: [],
    redeemers: [],
    datums: dict.empty,
    id: #"0000",
    votes: [],
    proposal_procedures: [],
    current_treasury_amount: None,
    treasury_donation: None,
  }
}

/// The redeemer carries application gas only; the oracle's 11000 overhead is
/// added on-chain. (200000 + 11000) * 1e9 * 7171 / 1e12 = 1513081 lovelace.
const test_gas_amount = 200000

const test_required = 1513081

const test_held = 5000000

test quoted_payment_is_accepted() {
  validate_pay_for_gas(
    test_igp_datum(),
    test_message_id,
    test_domain,
    test_gas_amount,
    pay_tx(test_held, test_held + test_required),
    test_own_ref,
  )
}

test underpayment_is_rejected() fail {
  validate_pay_for_gas(
    test_igp_datum(),
    test_message_id,
    test_domain,
    test_gas_amount,
    pay_tx(test_held, test_held + test_required - 1),
    test_own_ref,
  )
}

/// The reason the check compares the delta exactly: an over-funded output is
/// unrecoverable for the payer, so it is refused rather than absorbed.
test overpayment_is_rejected() fail {
  validate_pay_for_gas(
    test_igp_datum(),
    test_message_id,
    test_domain,
    test_gas_amount,
    pay_tx(test_held, test_held + test_required + 1),
    test_own_ref,
  )
}

test no_payment_is_rejected() fail {
  validate_pay_for_gas(
    test_igp_datum(),
    test_message_id,
    test_domain,
    test_gas_amount,
    pay_tx(test_held, test_held),
    test_own_ref,
  )
}

/// The accumulated balance must not be drained under cover of a payment.
test draining_the_igp_while_paying_is_rejected() fail {
  validate_pay_for_gas(
    test_igp_datum(),
    test_message_id,
    test_domain,
    test_gas_amount,
    pay_tx(test_held, test_required),
    test_own_ref,
  )
}

/// Omitting the overhead is not a way to pay less: it is added on-chain, so
/// the price for a given application gas is the same however the caller
/// prepares the redeemer.
test overhead_is_charged_on_top_of_application_gas() {
  // Priced as if the caller had declared the full 211000 themselves.
  let without_overhead =
    test_gas_amount * 1000000000 * 7171 / 1000000000000
  expect without_overhead < test_required
  validate_pay_for_gas(
    test_igp_datum(),
    test_message_id,
    test_domain,
    test_gas_amount,
    pay_tx(test_held, test_held + test_required),
    test_own_ref,
  )
}

/// A route whose delivery cost is fully covered by the overhead pays for zero
/// application gas, which is the ordinary case for native and collateral.
test zero_application_gas_still_pays_the_overhead() {
  let overhead_only = 11000 * 1000000000 * 7171 / 1000000000000
  validate_pay_for_gas(
    test_igp_datum(),
    test_message_id,
    test_domain,
    0,
    pay_tx(test_held, test_held + overhead_only),
    test_own_ref,
  )
}

/// The payment scales with the requested gas, so a larger request costs more
/// and the previously correct amount no longer satisfies it.
test payment_scales_with_requested_gas() fail {
  validate_pay_for_gas(
    test_igp_datum(),
    test_message_id,
    test_domain,
    test_gas_amount * 2,
    pay_tx(test_held, test_held + test_required),
    test_own_ref,
  )
}

/// A destination with no configured oracle is not payable. The previous
/// fallback priced it at zero, so a domain enrolled without its oracle looked
/// paid for and never delivered.
test unconfigured_destination_is_rejected() fail {
  validate_pay_for_gas(
    test_igp_datum(),
    test_message_id,
    test_domain + 1,
    test_gas_amount,
    pay_tx(test_held, test_held),
    test_own_ref,
  )
}

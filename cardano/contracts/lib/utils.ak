use aiken/collection/list
use aiken/crypto.{Blake2b_224, Hash, VerificationKeyHash}
use aiken/primitive/bytearray
use cardano/address.{Address, Script}
use cardano/transaction.{Input, Output, OutputReference, Transaction}
use cardano/assets.{Value, lovelace_of, merge, quantity_of, zero}

/// Utility functions for Hyperlane Cardano contracts
/// Script hash type alias
pub type ScriptHash =
  Hash<Blake2b_224, ByteArray>

/// Check if a transaction is signed by a specific key
pub fn is_signed_by(tx: Transaction, key: VerificationKeyHash) -> Bool {
  list.has(tx.extra_signatories, key)
}

/// Find an input by its output reference
pub fn find_input(tx: Transaction, ref: OutputReference) -> Option<Input> {
  list.find(tx.inputs, fn(input) { input.output_reference == ref })
}

/// Find outputs going to a specific address
pub fn find_outputs_to_address(tx: Transaction, addr: Address) -> List<Output> {
  list.filter(tx.outputs, fn(output) { output.address == addr })
}

/// Find outputs containing a specific token (by policy ID and asset name)
pub fn find_outputs_with_token(
  tx: Transaction,
  policy: ByteArray,
  name: ByteArray,
) -> List<Output> {
  list.filter(
    tx.outputs,
    fn(output) { quantity_of(output.value, policy, name) > 0 },
  )
}

/// Check if an input is from a specific script
pub fn is_script_input(input: Input, script_hash: ScriptHash) -> Bool {
  when input.output.address.payment_credential is {
    Script(hash) -> hash == script_hash
    _ -> False
  }
}

/// Find all inputs from a specific script
pub fn find_script_inputs(
  tx: Transaction,
  script_hash: ScriptHash,
) -> List<Input> {
  list.filter(tx.inputs, fn(input) { is_script_input(input, script_hash) })
}

/// Check if any input is from a specific script
pub fn has_script_input(tx: Transaction, script_hash: ScriptHash) -> Bool {
  list.any(tx.inputs, fn(input) { is_script_input(input, script_hash) })
}

/// Get the total value locked in inputs from a script
pub fn get_script_input_value(tx: Transaction, script_hash: ScriptHash) -> Value {
  let inputs = find_script_inputs(tx, script_hash)
  list.foldl(
    inputs,
    zero,
    fn(input, acc) { merge(acc, input.output.value) },
  )
}

/// Check that value is preserved (output >= input for all tokens)
pub fn value_preserved(input_value: Value, output_value: Value) -> Bool {
  lovelace_of(output_value) >= lovelace_of(input_value)
}

/// Convert a Hyperlane address (32 bytes) to a Cardano script hash (28 bytes)
/// The first 4 bytes are the credential type prefix, next 28 bytes are the actual hash
pub fn hyperlane_address_to_script_hash(addr: ByteArray) -> Option<ScriptHash> {
  if bytearray.length(addr) == 32 {
    // Skip first 4 bytes (credential type prefix), take next 28 bytes
    // bytearray.slice uses inclusive end index, so 4-31 = 28 bytes
    Some(bytearray.slice(addr, 4, 31))
  } else {
    None
  }
}

/// Convert a script hash to a Hyperlane address with script credential type prefix
pub fn script_hash_to_hyperlane_address(hash: ScriptHash) -> ByteArray {
  // Prefix: 0x02000000 for validator/script credential
  let prefix = #"02000000"
  bytearray.concat(prefix, hash)
}

/// Verify that a list has at least n elements
pub fn has_at_least(items: List<a>, n: Int) -> Bool {
  list.length(items) >= n
}

/// Safe list access by index
pub fn at(items: List<a>, index: Int) -> Option<a> {
  at_helper(items, index, 0)
}

fn at_helper(items: List<a>, target: Int, current: Int) -> Option<a> {
  when items is {
    [] -> None
    [head, ..tail] ->
      if current == target {
        Some(head)
      } else {
        at_helper(tail, target, current + 1)
      }
  }
}

/// Find a value in an association list by key
pub fn assoc_find(items: List<(k, v)>, key: k) -> Option<v> {
  when items is {
    [] -> None
    [(k, v), ..tail] ->
      if k == key {
        Some(v)
      } else {
        assoc_find(tail, key)
      }
  }
}

[← Epic 3: Gas Payments](./EPIC.md) | [Epics Overview](../README.md)

# Task 3.2: RPC Endpoint for Gas Payments
**Status:** ✅ Complete
**Complexity:** Medium
**Depends On:** [Task 3.0](./task-3.0-init-igp.md)

## Objective

Implement RPC endpoints for gas payment indexing and quote functionality to support the full Hyperlane IGP specification.

## Current State

**File:** `rust/main/chains/hyperlane-cardano/src/interchain_gas.rs`

Implemented:
- `fetch_logs_in_range` - Indexes gas payments from Cardano via Blockfrost
- `parse_pay_for_gas_redeemer` - Parses PayForGas redeemer from JSON
- `calculate_igp_payment` - Calculates payment from UTxO value differences
- Block hash fetching for proper LogMeta
- 16 unit tests passing

Quote functionality is available via CLI (Task 3.1) - not needed in Rust relayer.

## Requirements

### 1. Gas Payment Indexing (`fetch_logs_in_range`)

Query Blockfrost for transactions involving the IGP contract within the given slot range:

**Transaction Query:**
- Get all transactions to the IGP script address within slot range
- Filter for transactions with `PayForGas` redeemers

**Redeemer Parsing:**
- Decode CBOR redeemer data
- Extract `message_id` (32 bytes), `destination` (domain ID), `gas_amount`

**Payment Calculation:**
- Find IGP input UTXO value
- Find IGP continuation output UTXO value
- Payment = output_value - input_value (the amount added to IGP)

**Return Type:**
Return `InterchainGasPayment` structs:
- `message_id: H256` - The message being paid for
- `destination: u32` - The destination domain
- `payment: U256` - ADA paid (in lovelace)
- `gas_amount: U256` - Gas units requested

### 2. Quote Gas Payment Endpoint

Implement `quoteGasPayment(destination, gasAmount)` that:

**Fetches Current Oracle State:**
- Query IGP UTXO to get current datum
- Extract gas oracle config for the destination domain

**Calculates Required Payment:**
- Apply formula: `(gas_amount * gas_price * token_exchange_rate) / 10^18`
- Return required ADA in lovelace

**Error Handling:**
- Return error if no oracle configured for destination
- Handle missing IGP UTXO gracefully

### 3. Sequence Number Tracking

Implement proper sequence number handling:
- Track payment sequence numbers for ordering
- Use transaction index + output index as unique identifier
- Ensure deterministic ordering for relayer

## Files to Modify

| File | Changes |
|------|---------|
| `rust/main/chains/hyperlane-cardano/src/interchain_gas.rs` | Main implementation |
| `rust/main/chains/hyperlane-cardano/src/types.rs` | IGP types if needed |
| `rust/main/chains/hyperlane-cardano/src/provider.rs` | Add quote method |

## Testing

### Unit Tests
- Parse PayForGas redeemer correctly (various CBOR formats)
- Calculate payment amount from tx value differences
- Handle non-payment transactions (return None)
- Quote calculation matches on-chain logic
- Edge cases: zero gas, missing oracle, max values

### Integration Tests
- Fetch payments from testnet IGP
- Verify returned data matches on-chain transaction
- Quote matches actual required payment
- Sequence numbers are consistent

## Definition of Done

- [x] `fetch_logs_in_range` returns actual gas payments
- [x] `quoteGasPayment` returns accurate estimates (via CLI - Task 3.1)
- [x] Payment amounts correctly calculated (`calculate_igp_payment`)
- [x] Sequence numbers properly assigned (`transaction_index` + `log_index`)
- [x] Unit tests pass (16 tests)
- [ ] Integration tests pass (deferred to Task 3.6)
- [ ] Works with relayer's gas payment logic (verified in Task 3.4)

## Acceptance Criteria

1. Relayer can index all gas payments from Cardano
2. Quote endpoint returns accurate payment requirements
3. Payment indexing handles all edge cases
4. Performance acceptable for relayer polling

[← Epic 3: Gas Payments](./EPIC.md) | [Epics Overview](../README.md)

# Task 3.6: IGP Contract Enhancements
**Status:** ⬜ Not Started
**Complexity:** Medium
**Depends On:** None (but should be done early as it affects other tasks)

## Objective

Enhance the IGP contract to fully comply with Hyperlane specification, including refund address handling and per-destination gas defaults.

## Current Contract State

**File:** `cardano/contracts/validators/igp.ak`

The current implementation has:
- `PayForGas { message_id, destination, gas_amount }` - missing refund_address
- Single `default_gas_limit` in datum - not per-destination
- No explicit refund mechanism for overpayments

## Requirements

### 1. Add Refund Address to PayForGas

Update the `PayForGas` redeemer to include a refund address:

```
PayForGas:
  message_id: ByteArray (32 bytes)
  destination: Int (domain ID)
  gas_amount: Int
  refund_address: ByteArray (serialized address)
```

**Refund Logic:**
- Calculate required payment based on oracle
- If user sends more than required, create refund output
- Refund goes to specified refund_address
- If refund_address is empty, no refund (accept overpayment as extra fee)

**Validation:**
- Verify refund output exists if overpayment detected
- Verify refund output goes to specified address
- Verify refund amount is correct (overpayment - tx fee margin)

### 2. Per-Destination Gas Defaults

Update datum to support per-destination default gas amounts:

Current:
```
IgpDatum:
  owner: ByteArray
  beneficiary: ByteArray
  gas_oracles: List<(Domain, GasOracleConfig)>
  default_gas_limit: Int  // Single global default
```

Enhanced:
```
IgpDatum:
  owner: ByteArray
  beneficiary: ByteArray
  gas_oracles: List<(Domain, GasOracleConfig)>
  default_gas_limits: List<(Domain, Int)>  // Per-destination defaults
  fallback_gas_limit: Int  // Used if no per-destination default
```

**Behavior:**
- When `gas_amount` is 0, look up default for destination
- If no per-destination default, use `fallback_gas_limit`
- If no fallback, fail with clear error

### 3. Add SetDefaultGasLimit Action

New redeemer for configuring per-destination defaults:

```
SetDefaultGasLimit:
  domain: Int
  gas_limit: Int
```

**Validation:**
- Only owner can set defaults
- gas_limit must be > 0
- Updates default_gas_limits list (upsert behavior)
- Preserves other datum fields

### 4. Minimum Payment Validation

Add validation to prevent dust payments:

- Define minimum payment threshold (e.g., 1 ADA)
- Reject payments below threshold
- Prevents spam and UTXO bloat

### 5. Payment Event Data

Ensure all data needed for indexing is available:

The PayForGas redeemer already contains:
- message_id ✅
- destination ✅
- gas_amount ✅
- refund_address (adding)

Payment amount is calculated from UTXO value difference.

## Files to Modify

| File | Changes |
|------|---------|
| `cardano/contracts/validators/igp.ak` | Contract updates |
| `cardano/contracts/lib/types.ak` | Type definitions |
| `cardano/offchain/src/igp.rs` | Off-chain types |
| `cardano/cli/src/commands/igp.rs` | CLI for new features |

## Testing

### Unit Tests (Aiken)
- PayForGas with refund address validates correctly
- Refund output created for overpayment
- Per-destination defaults work
- SetDefaultGasLimit validates correctly
- Minimum payment enforced

### Integration Tests
- Deploy updated contract
- Pay with overpayment, verify refund
- Set and use per-destination defaults
- Verify relayer can index new format

### Migration Considerations
- Existing IGP UTXOs may need migration
- Document upgrade path
- Consider backwards compatibility period

## Definition of Done

- [ ] Refund address added to PayForGas
- [ ] Refund logic implemented and tested
- [ ] Per-destination gas defaults implemented
- [ ] SetDefaultGasLimit action added
- [ ] Minimum payment validation added
- [ ] All Aiken tests pass
- [ ] Off-chain types updated
- [ ] CLI updated for new features
- [ ] Migration path documented

## Acceptance Criteria

1. Overpayments correctly refunded
2. Per-destination defaults work
3. Owner can configure defaults via CLI
4. Backwards-compatible migration path exists
5. Relayer can index enhanced payment events
6. No security regressions

## Security Considerations

- Refund address must be validated (correct format)
- Prevent refund to script addresses that could fail
- Ensure refund calculation cannot be manipulated
- Owner-only access for configuration changes
- No arithmetic overflow in calculations

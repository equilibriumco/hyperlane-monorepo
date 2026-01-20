[← Epic 4: Advanced Features](./EPIC.md) | [Epics Overview](../README.md)

# Task 4.6: IGP Refund Support
**Status:** ⬜ Not Started
**Complexity:** Medium
**Depends On:** [Task 3.1](../epic-3-gas-payments/task-3.1-cli-commands.md)

## Objective

Add refund support to the IGP contract, allowing users to receive back unused gas payments after message delivery.

## Background

Currently, when users call `pay-for-gas`, they pay based on the oracle's quoted price. Any overpayment (due to lower actual gas costs, exchange rate changes, or gas estimation buffer) remains in the IGP for the beneficiary to claim.

The EVM Hyperlane IGP supports refunds via a `refundAddress` parameter:

```solidity
function payForGas(
    bytes32 _messageId,
    uint32 _destinationDomain,
    uint256 _gasAmount,
    address _refundAddress  // Where overpayment goes
) external payable;
```

## Current State

The Cardano IGP redeemer has no refund field:

```aiken
pub type IgpRedeemer {
  PayForGas { message_id: ByteArray, destination: Domain, gas_amount: Int }
  Claim { amount: Int }
  SetGasOracle { domain: Domain, config: GasOracleConfig }
}
```

## Proposed Design

### Option A: On-Chain Refund Tracking (Recommended)

**1. Modify IgpRedeemer:**

```aiken
pub type IgpRedeemer {
  PayForGas {
    message_id: ByteArray,
    destination: Domain,
    gas_amount: Int,
    refund_address: Address  // NEW: Where to send refund
  }
  Claim { amount: Int }
  SetGasOracle { domain: Domain, config: GasOracleConfig }
  ProcessRefund { message_id: ByteArray, actual_gas_used: Int }  // NEW
}
```

**2. Add Refund Tracking to IgpDatum:**

```aiken
pub type PendingRefund {
  message_id: ByteArray,
  refund_address: Address,
  max_refund: Int,        // Original payment amount
  gas_amount: Int,        // Requested gas
  destination: Domain,
}

pub type IgpDatum {
  owner: VerificationKeyHash,
  beneficiary: ByteArray,
  gas_oracles: List<(Domain, GasOracleConfig)>,
  default_gas_limit: Int,
  pending_refunds: List<PendingRefund>,  // NEW
}
```

**3. Refund Flow:**

```
1. User calls pay-for-gas with refund_address
   → Payment stored in IGP
   → PendingRefund entry added to datum

2. Relayer delivers message, records actual gas used

3. Relayer calls process-refund with message_id and actual_gas_used
   → Contract calculates refund = payment - (actual_gas_used * rate)
   → Refund sent to refund_address
   → PendingRefund entry removed
   → Remainder stays in IGP for beneficiary
```

### Option B: Off-Chain Refund (Simpler)

Relayer tracks overpayments off-chain and sends refunds directly from operator wallet. No contract changes needed, but requires trusting the relayer.

## Requirements

### Contract Changes

1. Add `refund_address` field to `PayForGas` redeemer
2. Add `ProcessRefund` redeemer variant
3. Add `pending_refunds` field to `IgpDatum`
4. Implement refund calculation and validation logic
5. Ensure only authorized party (relayer) can process refunds

### CLI Changes

1. Add `--refund-address` flag to `igp pay-for-gas` command
2. Add `igp process-refund` command for relayer
3. Add `igp show-pending-refunds` to display pending refunds

### Indexer Changes

1. Track `PayForGas` events with refund addresses
2. Track `ProcessRefund` events
3. Expose pending refunds via RPC

## Files to Modify

| File | Changes |
|------|---------|
| `cardano/contracts/lib/types.ak` | Add refund fields to IgpRedeemer, IgpDatum |
| `cardano/contracts/validators/igp.ak` | Implement ProcessRefund validation |
| `cardano/cli/src/commands/igp.rs` | Add refund-related commands |
| `cardano/cli/src/utils/cbor.rs` | Update datum builder for pending_refunds |

## Testing

- [ ] PayForGas with refund address stores pending refund
- [ ] ProcessRefund calculates correct refund amount
- [ ] ProcessRefund sends to correct address
- [ ] ProcessRefund fails for unknown message_id
- [ ] ProcessRefund fails for unauthorized caller
- [ ] Refund cannot exceed original payment
- [ ] CLI commands work correctly

## Definition of Done

- [ ] Contract updated with refund support
- [ ] CLI commands implemented
- [ ] Indexer tracks refunds
- [ ] Tested on Preview testnet
- [ ] Documentation updated

## Acceptance Criteria

1. Users can specify refund address when paying for gas
2. Relayer can process refunds after message delivery
3. Refund amount correctly calculated based on actual gas used
4. Pending refunds visible via CLI and indexer
5. No regression in existing IGP functionality

## Notes

- This is an optional enhancement - the current IGP without refunds is production-valid
- Most Hyperlane deployments operate without refunds (beneficiary keeps excess)
- Consider implementing only if there's user demand for refund functionality

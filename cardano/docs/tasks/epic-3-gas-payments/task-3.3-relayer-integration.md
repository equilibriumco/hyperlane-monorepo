[← Epic 3: Gas Payments](./EPIC.md) | [Epics Overview](../README.md)

# Task 3.3: Relayer Integration
**Status:** ⬜ Not Started
**Complexity:** Medium
**Depends On:** [Task 3.1](./task-3.1-rpc-endpoint.md)

## Objective

Integrate the Cardano IGP with the relayer's gas payment checking logic.

## Background

The relayer checks for gas payments before delivering messages. With the RPC endpoint implemented, we need to ensure the relayer correctly queries and enforces payments.

## Requirements

### 1. Configure IGP in Relayer

Add IGP configuration to relayer config for Cardano chain:
- Policy ID
- Script hash
- Minimum gas requirements

### 2. Verify Relayer Queries IGP

The relayer should:
- Index gas payments from Cardano
- Check payment exists before delivery
- Handle missing payments appropriately (skip or wait)

### 3. Handle Unpaid Messages

Configure relayer behavior for unpaid messages:
- `allowUnpaid: true` - Deliver anyway (for testing)
- `allowUnpaid: false` - Only deliver paid messages
- Minimum gas threshold

## Testing

### Integration Tests
- Relayer fetches Cardano gas payments
- Paid message delivered
- Unpaid message behavior correct based on config

### E2E Tests
- Dispatch with payment → delivered
- Dispatch without payment → behavior as configured

## Files to Modify

| File | Changes |
|------|---------|
| `rust/main/agents/relayer/src/msg/gas_payment/` | Cardano integration |
| Config files | IGP configuration |

## Definition of Done

- [ ] Relayer queries Cardano IGP
- [ ] Gas payments correctly indexed
- [ ] Enforcement works as configured
- [ ] No regression in message processing

## Acceptance Criteria

1. Relayer integrates with Cardano IGP
2. Payment checking works correctly
3. Configurable enforcement policy

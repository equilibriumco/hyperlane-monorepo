[← Epic 3: Gas Payments](./EPIC.md) | [Epics Overview](../README.md)

# Task 3.5: Post-Dispatch Hook Integration
**Status:** ⬜ Not Started
**Complexity:** High
**Depends On:** [Task 3.1](./task-3.1-cli-commands.md), [Task 3.2](./task-3.2-rpc-endpoint.md), [Task 3.3](./task-3.3-contract-enhancements.md)

## Objective

Implement post-dispatch hook integration to enable automatic gas payment at message dispatch time, matching the Hyperlane standard flow.

## Background

In the standard Hyperlane flow, the IGP is integrated as a **post-dispatch hook**. This means:
1. User calls `dispatch()` with a hook configuration
2. After the message is dispatched, the hook automatically calls `payForGas()`
3. Gas payment happens atomically with message dispatch

Currently, our implementation requires two separate transactions:
1. Dispatch message → get message_id
2. Pay for gas with message_id

The post-dispatch hook enables a single-transaction flow for better UX and atomicity.

## Requirements

### 1. Hook Contract Design

Create or extend existing hook mechanism to support IGP:

**Hook Configuration:**
- IGP script address
- Default gas amount per destination (or use IGP defaults)
- Refund address (defaults to sender)

**Hook Execution:**
- Called after successful dispatch
- Receives message_id from dispatch
- Calculates required payment
- Includes IGP payment in same transaction

### 2. Transaction Structure

The combined dispatch+pay transaction must:
- Spend mailbox UTXO (for dispatch)
- Spend IGP UTXO (for gas payment)
- Include both redeemers in same transaction
- Handle both continuation outputs correctly

**Input UTXOs:**
- Mailbox state UTXO
- IGP state UTXO
- User's payment UTXO(s)

**Output UTXOs:**
- Updated mailbox state (with new nonce, message in tree)
- Updated IGP state (with payment added)
- Message delivery NFT (minted)
- Change to sender
- Refund if overpayment (optional)

### 3. CLI Integration

Update dispatch command to support hooks:

```
hyperlane-cardano mailbox dispatch \
  --destination 43113 \
  --recipient 0x1234... \
  --body "Hello" \
  --hook igp \
  --gas-amount 200000
```

When `--hook igp` is specified:
- Quote the gas payment first
- Build combined transaction
- Submit single transaction
- Return message_id and payment amount

### 4. Fallback Behavior

If hook execution fails, provide graceful degradation:
- Log warning about hook failure
- Complete dispatch without gas payment
- Return message_id so user can pay manually
- Clear error message explaining next steps

## Technical Considerations

### Cardano Transaction Constraints

Unlike EVM where hooks are callback functions, Cardano requires:
- All script inputs declared upfront
- All redeemers included in transaction
- Execution units calculated for all scripts

This means the "hook" is really transaction composition:
- Build dispatch transaction components
- Build IGP payment transaction components
- Combine into single transaction
- Calculate combined execution units

### Message ID Availability

Challenge: The message_id is derived from message contents including nonce. The nonce comes from current mailbox state.

Solution:
1. Read current mailbox state to get nonce
2. Calculate message_id from known components
3. Use calculated message_id in PayForGas redeemer
4. Both scripts validate in same transaction

### Atomic Failure

If either script fails validation:
- Entire transaction fails
- No partial state changes
- User can retry or fall back to manual flow

## Files to Create/Modify

| File | Changes |
|------|---------|
| `cardano/cli/src/commands/mailbox.rs` | Add hook support to dispatch |
| `cardano/cli/src/tx_builder.rs` | Combined transaction building |
| `cardano/offchain/src/hooks/` | Hook abstraction (new) |
| `cardano/offchain/src/hooks/igp_hook.rs` | IGP hook implementation |

## Testing

### Unit Tests
- Message ID calculation matches on-chain derivation
- Combined transaction structure is valid
- Execution units correctly calculated for both scripts
- Refund calculation correct

### Integration Tests
- Dispatch with IGP hook succeeds on testnet
- Gas payment appears in IGP state
- Message delivery works after combined dispatch
- Fallback to manual payment works

### Edge Cases
- Insufficient funds for gas payment
- Missing oracle for destination
- Maximum message size with hook
- Multiple hooks (future consideration)

## Definition of Done

- [ ] Hook abstraction implemented
- [ ] IGP hook implementation complete
- [ ] CLI dispatch supports `--hook igp` flag
- [ ] Combined transactions work on testnet
- [ ] Fallback behavior handles failures gracefully
- [ ] Documentation updated

## Acceptance Criteria

1. Single-transaction dispatch+pay works
2. Message ID correctly calculated pre-dispatch
3. Both mailbox and IGP state updated atomically
4. Fallback to manual payment available
5. Clear error messages on hook failures
6. Performance acceptable (single transaction faster than two)

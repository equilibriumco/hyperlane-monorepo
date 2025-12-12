[← Epic 1: Bidirectional Messaging](./EPIC.md) | [Epics Overview](../README.md)

# Task 1.5: Dispatch CLI Command
**Status:** ⬜ Not Started
**Complexity:** Medium
**Depends On:** None (can be done in parallel with Tasks 1.1-1.4)

## Objective

Implement the `mailbox dispatch` CLI command to send messages from Cardano to remote chains.

## Background

The mailbox contract has `Dispatch` functionality implemented, but there's no CLI command to build and submit dispatch transactions.

## Current State

**File:** `cardano/cli/src/commands/mailbox.rs`

The CLI has `process` command for incoming messages but no `dispatch` command for outgoing.

## Requirements

### 1. CLI Interface

```bash
hyperlane-cardano mailbox dispatch \
  --destination 43113 \
  --recipient 0x1234567890abcdef1234567890abcdef12345678 \
  --body "Hello from Cardano" \
  [--sender <address>]  # Optional, defaults to signing key's address
```

### 2. Command Implementation

The dispatch command should:
1. Parse recipient address (32-byte hex, with or without 0x prefix)
2. Parse body (string or hex with 0x prefix)
3. Get sender address (from arg or default to signing key)
4. Build dispatch transaction with proper redeemer
5. Calculate expected message ID
6. Support dry-run mode to preview without submitting
7. Submit and return transaction hash and message ID

### 3. Transaction Builder

The transaction builder needs a `build_dispatch_tx` method that:
- Fetches current mailbox UTXO and parses datum
- Builds Dispatch redeemer with destination, recipient, body
- Calculates the message hash and updates merkle tree
- Builds new datum with incremented outbound_nonce and updated tree
- Constructs the transaction with proper inputs, outputs, and reference scripts

### 4. Message ID Calculation

Follow Hyperlane message format: concatenate version (3), nonce, origin domain, sender (32 bytes), destination domain, recipient (32 bytes), and body. Message ID is keccak256 of this.

## Files to Modify

| File | Changes |
|------|---------|
| `cardano/cli/src/commands/mailbox.rs` | Add dispatch command |
| `cardano/cli/src/commands/mod.rs` | Export dispatch |
| `cardano/cli/src/tx_builder.rs` | Add build_dispatch_tx |
| `cardano/cli/src/main.rs` | Wire up command |

## Technical Notes

### Sender Address Encoding

Cardano addresses need to be encoded to 32 bytes for Hyperlane. Extract the key hash or script hash and pad to 32 bytes.

### Transaction Fees

The dispatch transaction requires script execution fees (~0.5-1 ADA typical). The mailbox UTXO must continue with same value. Signing key must have enough ADA for fees.

## Testing

### Unit Tests
- Recipient address parsing
- Body encoding (string and hex)
- Message ID calculation matches reference
- Merkle tree update logic

### Integration Tests
- Dispatch to testnet
- Verify nonce increment
- Verify merkle tree updated
- Dry run mode

## Definition of Done

- [ ] `dispatch` command implemented
- [ ] Message ID calculation correct
- [ ] Transaction builds and submits successfully
- [ ] Dry run mode works
- [ ] Unit tests pass
- [ ] Integration test on testnet passes
- [ ] Help text and documentation complete

## Acceptance Criteria

1. Command dispatches message successfully
2. Message ID calculation matches Hyperlane spec
3. Nonce correctly incremented
4. Merkle tree correctly updated
5. Works with string and hex body formats
6. Dry run shows expected values without submitting

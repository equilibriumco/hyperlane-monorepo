[← Epic 1: Bidirectional Messaging](./EPIC.md) | [Epics Overview](../README.md)

# Task 1.5: Dispatch CLI Command
**Status:** ✅ Complete
**Complexity:** Medium
**Depends On:** None (can be done in parallel with Tasks 1.1-1.4)

## Objective

Implement the `mailbox dispatch` CLI command to send messages from Cardano to remote chains.

## Background

The mailbox contract has `Dispatch` functionality implemented, but there's no CLI command to build and submit dispatch transactions.

## Implementation Summary

### CLI Command

Added `mailbox dispatch` command to send Hyperlane messages from Cardano:

```bash
./cli/target/release/hyperlane-cardano mailbox dispatch \
  --destination 43113 \
  --recipient 0x0000000000000000000000001234567890abcdef1234567890abcdef12345678 \
  --body "Hello from Cardano" \
  [--dry-run]
```

### Command Options

| Option | Description | Default |
|--------|-------------|---------|
| `--destination` | Destination domain ID (e.g., 43113 for Avalanche Fuji) | Required |
| `--recipient` | Recipient address (32 bytes hex, with or without 0x) | Required |
| `--body` | Message body (string or hex with 0x prefix) | Required |
| `--mailbox-policy` | Mailbox policy ID | From deployment_info.json |
| `--reference-script` | Reference script UTXO (txhash#index) | From deployment_info.json |
| `--signing-key` | Path to signing key | From env CARDANO_SIGNING_KEY |
| `--dry-run` | Preview message details without submitting | false |

### Implementation Details

1. **Message ID Calculation**: Follows Hyperlane message format
   - Version (1 byte): 3
   - Nonce (4 bytes): from mailbox datum
   - Origin domain (4 bytes): from mailbox datum
   - Sender (32 bytes): `0x00000000` + verification key hash (28 bytes)
   - Destination domain (4 bytes)
   - Recipient (32 bytes)
   - Body (variable length)
   - Message ID = keccak256(encoded_message)

2. **Merkle Tree Update**: Incremental merkle tree algorithm
   - Uses 32 branches at each level
   - Updates affected branches based on leaf count
   - Matches on-chain validation logic

3. **Transaction Building**:
   - Spends current mailbox UTXO
   - Creates continuation UTXO with updated datum
   - Uses reference script for validation
   - Includes proper redeemer (Dispatch variant)

### Files Modified

| File | Changes |
|------|---------|
| `cardano/cli/src/commands/mailbox.rs` | Added Dispatch command with full implementation |
| `cardano/cli/src/utils/cbor.rs` | Added `build_mailbox_dispatch_redeemer` function |
| `cardano/cli/src/utils/tx_builder.rs` | Fixed change output threshold (`>` to `>=`) |
| `cardano/cli/src/commands/init.rs` | Increased mailbox minUTxO to 7 ADA for larger datum |

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

## Testing

### Tested on Preview Testnet

Successfully dispatched message:
- Transaction: `b100677d6a98680617feb1d7af70f064739b40d83fd748c77892052fa85aaa08`
- Message ID: `0x0a0550053cbb78c7bc427e8036689b514c0990f69fb960baecedf48714cc9152`
- Destination: 43113
- Explorer: https://preview.cardanoscan.io/transaction/b100677d6a98680617feb1d7af70f064739b40d83fd748c77892052fa85aaa08

## Definition of Done

- [x] `dispatch` command implemented
- [x] Message ID calculation correct (keccak256 of Hyperlane message format)
- [x] Transaction builds and submits successfully
- [x] Dry run mode works (shows message details without signing key)
- [x] Merkle tree update logic implemented
- [x] Integration test on testnet passes

## Acceptance Criteria

1. ✅ Command dispatches message successfully
2. ✅ Message ID calculation matches Hyperlane spec
3. ✅ Nonce correctly incremented
4. ✅ Merkle tree correctly updated
5. ✅ Works with string and hex body formats
6. ✅ Dry run shows expected values without submitting

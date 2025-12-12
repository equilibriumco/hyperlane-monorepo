[← Epic 1: Bidirectional Messaging](./EPIC.md) | [Epics Overview](../README.md)

# Task 1.1: MerkleTree Hook Implementation
**Status:** ⬜ Not Started
**Complexity:** Medium
**Depends On:** None

## Objective

Implement the `MerkleTreeHook` trait for Cardano to enable reading merkle tree state from the mailbox contract.

## Background

The merkle tree is stored in the mailbox datum and updated on each message dispatch. Validators need to read this state to sign checkpoints. The datum contains `local_domain`, `outbound_nonce`, `inbound_nonce`, `merkle_tree`, and `default_ism` fields.

## Current State

**File:** `rust/main/chains/hyperlane-cardano/src/merkle_tree_hook.rs`

The implementation exists but methods are stubs or incomplete.

## Requirements

### 1. Implement `tree()` method

Should fetch the mailbox UTXO, parse the datum, extract the merkle_tree field, and convert it to Hyperlane's `IncrementalMerkle` format.

### 2. Implement `count()` method

Should return the number of messages in the tree, which equals the `outbound_nonce` from the mailbox datum.

### 3. Implement `latest_checkpoint()` method

Should return the latest checkpoint containing the merkle root and current index.

### 4. Parse merkle tree from CBOR

The Aiken merkle tree stores 32 branch hashes (each 32 bytes) plus a count. Need proper CBOR parsing to extract this structure and convert to Hyperlane's format.

## Files to Modify

| File | Changes |
|------|---------|
| `rust/main/chains/hyperlane-cardano/src/merkle_tree_hook.rs` | Main implementation |
| `rust/main/chains/hyperlane-cardano/src/types.rs` | Add merkle tree types |
| `rust/main/chains/hyperlane-cardano/src/mailbox.rs` | Datum parsing updates |

## Technical Notes

- The Aiken MerkleTree type has `branches: List<ByteArray>` (32 branches, each 32 bytes) and `count: Int`
- Use Blockfrost to query the mailbox UTXO by script hash, then find the one with the state NFT
- The mailbox datum uses inline datum format

## Testing

### Unit Tests
- Parse merkle tree from known CBOR
- Verify branch extraction
- Test count retrieval
- Test checkpoint construction

### Integration Tests
- Fetch merkle tree from deployed testnet mailbox
- Verify root matches expected value
- Test with empty tree
- Test with multiple messages

## Definition of Done

- [ ] `tree()` returns correct `IncrementalMerkle`
- [ ] `count()` returns message count
- [ ] `latest_checkpoint()` returns valid checkpoint
- [ ] CBOR parsing handles all edge cases
- [ ] Unit tests pass
- [ ] Integration tests pass with testnet mailbox

## Acceptance Criteria

1. Merkle tree correctly parsed from mailbox datum
2. Tree state matches on-chain state
3. Works with empty tree (no messages dispatched)
4. Works with populated tree (after dispatches)
5. Proper error handling for malformed data

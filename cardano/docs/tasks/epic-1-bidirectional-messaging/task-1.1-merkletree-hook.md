[← Epic 1: Bidirectional Messaging](./EPIC.md) | [Epics Overview](../README.md)

# Task 1.1: MerkleTree Hook Implementation
**Status:** ✅ Complete
**Complexity:** Medium
**Depends On:** None

## Objective

Implement the `MerkleTreeHook` trait for Cardano to enable reading merkle tree state from the mailbox contract.

## Background

The merkle tree is stored in the mailbox datum and updated on each message dispatch. Validators need to read this state to sign checkpoints.

## Implementation Notes

### Design Decision: Full Branch Storage

The Aiken contracts store the **full merkle tree branch state** (32 branches × 32 bytes = 1024 bytes) plus **count** (Int) in the mailbox datum. This approach:

1. **Enables proper on-chain merkle validation** - The mailbox contract can verify and update the merkle tree on each dispatch
2. **Costs ~4.4 ADA more in minUTxO** - Acceptable trade-off for full on-chain verification
3. **Simplifies validator logic** - `tree.root()` returns the correct merkle root directly

**File:** `rust/main/chains/hyperlane-cardano/src/merkle_tree_hook.rs`

### Implementation Details

The `tree_and_tip()` method in `mailbox.rs` returns:
- `IncrementalMerkle` with actual branches from the datum
- `block_height` (u32) - current finalized block

The `MerkleTreeHook` trait methods:
- `latest_checkpoint()` - Uses `tree.root()` directly (branches are real)
- `count()` - Returns `tree.count()` from the datum's merkle tree state
- `tree()` - Returns the complete tree with actual branches

### Datum Structure

The `MailboxDatum` contains a nested `MerkleTreeState`:
```
MailboxDatum {
  local_domain: Domain,
  default_ism: ScriptHash,
  owner: VerificationKeyHash,
  outbound_nonce: Int,
  merkle_tree: MerkleTreeState {
    branches: List<ByteArray>,  // 32 branches, each 32 bytes
    count: Int,
  },
}
```

## Files Modified

| File | Changes |
|------|---------|
| `cardano/contracts/lib/types.ak` | Added `MerkleTreeState` type, updated `MailboxDatum` |
| `cardano/contracts/validators/mailbox.ak` | Updated dispatch/continuation to use full branches |
| `rust/main/chains/hyperlane-cardano/src/types.rs` | Added `MerkleTreeState`, updated `MailboxDatum` |
| `rust/main/chains/hyperlane-cardano/src/mailbox.rs` | Updated datum parsing for nested structure |
| `rust/main/chains/hyperlane-cardano/src/merkle_tree_hook.rs` | Simplified to use `tree.root()` directly |
| `rust/main/chains/hyperlane-cardano/src/mailbox_indexer.rs` | Updated for new tuple signature |

## Technical Notes

- Use Blockfrost to query the mailbox UTXO by script hash, then find the one with the state NFT
- The mailbox datum uses inline datum format
- JSON parsing extracts nested `MerkleTreeState` from field index 4
- The Aiken `merkle.insert()` function updates the branch state on each dispatch

## Definition of Done

- [x] `tree()` returns `IncrementalMerkle` with actual branches
- [x] `count()` returns message count from datum
- [x] `latest_checkpoint()` returns valid checkpoint with correct root
- [x] Datum parsing handles nested `MerkleTreeState` correctly
- [x] Code compiles and passes all tests (30 tests passing)
- [ ] Integration tests pass with testnet mailbox (pending deployment)

## Acceptance Criteria

1. ✅ Merkle root correctly computed from stored branches
2. ✅ Tree count matches on-chain `merkle_tree.count`
3. ✅ Works with empty tree (returns INITIAL_ROOT for count=0)
4. ✅ Works with populated tree (returns correct root after dispatches)
5. ✅ Proper error handling for missing/malformed data

[← Epic 1: Bidirectional Messaging](./EPIC.md) | [Epics Overview](../README.md)

# Task 1.3: Checkpoint Syncer
**Status:** ⬜ Not Started
**Complexity:** Medium
**Depends On:** [Task 1.1](./task-1.1-merkletree-hook.md), [Task 1.2](./task-1.2-validator-agent.md)

## Objective

Implement checkpoint syncing from Cardano mailbox state, ensuring validators track on-chain state correctly and handle edge cases like reorgs and rate limits.

## Background

The checkpoint syncer is responsible for:
1. Watching the mailbox for new dispatched messages
2. Building/updating the local merkle tree
3. Triggering checkpoint signing when new messages are indexed
4. Storing signed checkpoints for relayer retrieval

## Current State

**File:** `rust/main/agents/validator/src/checkpoint.rs`

The checkpoint syncer exists for EVM chains. Need to ensure it works with Cardano's `MerkleTreeHook`.

## Requirements

### 1. Cardano-Specific Slot Tracking

The syncer needs to track Cardano slots (not block numbers) and maintain a local copy of the merkle tree that mirrors the on-chain state.

### 2. Sync Loop Implementation

The sync loop should:
- Get the current chain tip slot
- Fetch messages dispatched since last indexed slot
- For each new message: update local tree, verify against on-chain root, sign checkpoint, store it
- Update the last indexed slot

### 3. Handle Blockfrost Rate Limits

Implement exponential backoff retry logic for rate-limited requests.

### 4. Merkle Tree Reconstruction

On startup, the syncer should reconstruct the local merkle tree from on-chain state by fetching all dispatched messages and rebuilding the tree, then verifying the root matches.

## Files to Modify

| File | Changes |
|------|---------|
| `rust/main/agents/validator/src/checkpoint.rs` | Cardano syncer |
| `rust/main/chains/hyperlane-cardano/src/merkle_tree_hook.rs` | Message indexing |
| `rust/main/chains/hyperlane-cardano/src/mailbox.rs` | Message parsing |

## Technical Notes

### Message Indexing

Use Blockfrost's transaction history API to find dispatch transactions for the mailbox script, then parse the redeemer to extract message details.

### Slot vs Block Number

Cardano uses slots for time. A slot may or may not have a block. Use slots for range queries and block hashes for finality verification.

### Storage Options

Checkpoints can be stored in local filesystem or S3, configurable via the checkpoint syncer configuration.

## Testing

### Unit Tests
- Tree reconstruction from message list
- Checkpoint generation for new messages
- Rate limit retry logic

### Integration Tests
- Connect to testnet and sync
- Reconstruct tree from deployed mailbox
- Handle empty mailbox (no messages)
- Handle mailbox with existing messages

## Definition of Done

- [ ] Checkpoint syncer tracks Cardano mailbox state
- [ ] Merkle tree reconstruction works on startup
- [ ] New messages trigger checkpoint signing
- [ ] Rate limits handled with retries
- [ ] Logging sufficient for debugging
- [ ] Unit tests pass
- [ ] Integration tests pass

## Acceptance Criteria

1. Syncer correctly tracks all dispatched messages
2. Local merkle tree matches on-chain state
3. Checkpoints signed for each new message
4. Recovery works after restart
5. Rate limits don't cause failures

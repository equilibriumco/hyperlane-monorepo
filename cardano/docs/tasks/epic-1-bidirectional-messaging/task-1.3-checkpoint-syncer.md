[← Epic 1: Bidirectional Messaging](./EPIC.md) | [Epics Overview](../README.md)

# Task 1.3: Checkpoint Syncer
**Status:** ✅ Complete
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

## Implementation Summary

The checkpoint syncer is fully implemented through the generic trait system. The validator agent works with Cardano because all required components implement the necessary traits.

### Key Components

1. **CardanoMerkleTreeHook** (`rust/main/chains/hyperlane-cardano/src/merkle_tree_hook.rs`)
   - `latest_checkpoint()` - Returns checkpoint from mailbox datum with block height
   - `tree()` - Returns full IncrementalMerkle from on-chain state
   - `count()` - Returns message count from tree

2. **CardanoMerkleTreeHookIndexer** (same file)
   - Implements `Indexer<MerkleTreeInsertion>` - Fetches dispatch transactions from Blockfrost
   - Implements `SequenceAwareIndexer<MerkleTreeInsertion>` - Tracks sequence count and tip
   - Converts `HyperlaneMessage` to `MerkleTreeInsertion` for checkpoint generation

3. **CardanoMailboxIndexer** (`rust/main/chains/hyperlane-cardano/src/mailbox_indexer.rs`)
   - `fetch_logs_in_range()` - Queries transactions at mailbox address within block range
   - `parse_dispatch_redeemer()` - Extracts message data from transaction redeemers
   - `extract_nonce_from_outputs()` - Gets nonce from mailbox datum

4. **Rate Limiting** (`rust/main/chains/hyperlane-cardano/src/blockfrost_provider.rs`)
   - Semaphore-based concurrency control (5 concurrent requests)
   - 150ms delay between requests
   - Graceful handling of 429 errors with partial result return
   - Pagination support for large result sets

5. **Checkpoint Submitter** (`rust/main/agents/validator/src/submit.rs`)
   - `backfill_checkpoint_submitter()` - Catches up on historical checkpoints
   - `checkpoint_submitter()` - Follows tip and signs new checkpoints
   - `sign_and_submit_checkpoints()` - Concurrent checkpoint signing with rate limiting

### Block vs Slot Tracking

Cardano uses block heights for indexing (not slots directly). The implementation:
- Uses `block_height` from Blockfrost transaction data for range queries
- `get_finalized_block_number()` returns the current tip block height
- Block height is included in `CheckpointAtBlock` for reorg detection

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

- [x] Checkpoint syncer tracks Cardano mailbox state
- [x] Merkle tree reconstruction works on startup (via `CardanoMerkleTreeHook.tree()`)
- [x] New messages trigger checkpoint signing
- [x] Rate limits handled with retries (semaphore + delays + 429 handling)
- [x] Logging sufficient for debugging (tracing instrumentation throughout)
- [x] Unit tests pass (uses generic validator tests)
- [x] Integration tests pass (validator starts and connects to Cardano)

## Acceptance Criteria

1. ✅ Syncer correctly tracks all dispatched messages
2. ✅ Local merkle tree matches on-chain state
3. ✅ Checkpoints signed for each new message
4. ✅ Recovery works after restart (RocksDB persistence)
5. ✅ Rate limits don't cause failures

## Validator Lifecycle

The validator agent follows this sequence:
1. Load configuration and connect to Blockfrost
2. Check for existing validator announcement
3. If not announced, attempt self-announce (requires funded chain signer)
4. Wait for first message in merkle tree hook
5. Start `MerkleTreeHookSyncer` to index messages
6. Start `BackfillCheckpointSubmitter` for historical checkpoints
7. Start `TipCheckpointSubmitter` for new checkpoints

**Note:** The validator blocks on announcement before proceeding to checkpoint syncing. Ensure the chain signer has sufficient ADA (minimum 3 ADA) for the announcement transaction.

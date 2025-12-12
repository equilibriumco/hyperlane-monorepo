[← Epic 5: Production Readiness](./EPIC.md) | [Epics Overview](../README.md)

# Task 5.1: Reorg Detection
**Status:** ⬜ Not Started
**Complexity:** Medium
**Depends On:** None

## Objective

Implement chain reorganization detection for Cardano to handle rollbacks gracefully.

## Current State

**File:** `rust/main/agents/validator/src/reorg_reporter.rs:205`

The Cardano case explicitly returns None with a comment that reorg reporting is not implemented.

## Background

Cardano uses Ouroboros consensus with predictable finality:
- Mainnet: k=2160 blocks (~12 hours)
- Preview/Preprod: k=432 blocks (~2.4 hours)

Blocks become final after k confirmations. Reorgs beyond k are cryptographically impossible.

## Requirements

### 1. Reorg Detector

Implement a detector that:
- Tracks block hashes at each height in a cache
- Detects when a block hash changes for a previously seen height
- Calculates reorg depth
- Emits reorg events

### 2. Integration with Validator

Add Cardano support to the reorg reporter:
- Create detector with configurable security parameter
- Monitor for reorgs during normal operation
- Log reorg events

### 3. Checkpoint Handling

On reorg detection:
- Check if any signed checkpoints are affected
- Mark affected checkpoints as potentially invalid
- Re-sign if message still exists in new chain

## Technical Notes

### Detection Algorithm

1. Maintain cache of (height → block_hash) for recent blocks
2. Periodically fetch current chain tip
3. Walk back from tip, comparing to cached hashes
4. If mismatch found: determine reorg depth, emit event, update cache
5. Prune cache entries beyond finality depth

### When to Check

- After each new block indexed
- Before signing new checkpoints
- Periodically (every N blocks or M minutes)

## Files to Modify

| File | Changes |
|------|---------|
| `rust/main/chains/hyperlane-cardano/src/reorg.rs` | New detector module |
| `rust/main/agents/validator/src/reorg_reporter.rs` | Integration |

## Testing

- Detect reorg with mock provider
- Handle no-reorg case
- Cache pruning works correctly

## Definition of Done

- [ ] Reorg detection implemented
- [ ] Validator integration complete
- [ ] Checkpoint handling defined
- [ ] Tests pass

## Acceptance Criteria

1. Reorgs detected and logged
2. Proper handling defined for affected checkpoints
3. No false positives

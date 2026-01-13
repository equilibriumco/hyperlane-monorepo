[← Epic 4: Advanced Features](./EPIC.md) | [Epics Overview](../README.md)

# Task 4.3: Parallel Queries
**Status:** ⬜ Not Started
**Complexity:** Low
**Depends On:** None

## Objective

Parallelize independent Blockfrost queries to reduce transaction building time.

## Problem

Current approach makes sequential queries that could run in parallel:
1. Get mailbox UTXO (~200ms)
2. Get registry UTXO (~200ms)
3. Get recipient state (~200ms)
4. Get ISM reference script (~200ms)

Total: ~800ms sequential

## Solution

Use async parallelization for independent queries. Since these four queries don't depend on each other's results, they can run simultaneously.

With parallel execution:
- All queries start at once
- Total time = max(individual times) ≈ 200ms

## Identified Opportunities

### Transaction Building

When building a process transaction, fetch mailbox, registry, recipient state, and ISM reference in parallel, then use the results to build the transaction.

### State Queries

When fetching overall state, query mailbox, registry, and IGP state in parallel.

## Files to Modify

| File | Changes |
|------|---------|
| `rust/main/chains/hyperlane-cardano/src/tx_builder.rs` | Parallel queries |
| `rust/main/chains/hyperlane-cardano/src/mailbox.rs` | Parallel fetches |

## Testing

- Parallel queries complete correctly
- Error handling works (any failure fails the operation)
- Performance improvement measurable

## Definition of Done

- [ ] Independent queries parallelized
- [ ] Transaction building faster
- [ ] No correctness issues

## Acceptance Criteria

1. Build time reduced from ~800ms to ~200ms
2. All queries complete correctly
3. Proper error handling maintained

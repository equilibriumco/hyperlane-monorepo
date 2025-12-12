[← Epic 4: Advanced Features](./EPIC.md) | [Epics Overview](../README.md)

# Task 4.3: Reference Script Caching
**Status:** ⬜ Not Started
**Complexity:** Low
**Depends On:** None

## Objective

Cache reference script UTXO locations to reduce Blockfrost API calls.

## Problem

Reference scripts are fetched from Blockfrost for each transaction:
- Mailbox reference script
- ISM reference script
- Recipient reference script

This adds latency (~200ms per lookup) and consumes API quota.

## Solution

Implement an in-memory cache with TTL and invalidation on failure.

### Cache Behavior

- Store mapping of script hash → UTXO reference
- Check cache before making API call
- Use configurable TTL (default 5-10 minutes)
- Invalidate entry on "UTxO not found" errors

### Cache Invalidation

Reference scripts rarely change (only on contract upgrades). Strategy:
- Use long TTL since scripts are stable
- Invalidate on transaction failure with UTXO error
- Force refresh via CLI command if needed

## Files to Modify

| File | Changes |
|------|---------|
| `rust/main/chains/hyperlane-cardano/src/tx_builder.rs` | Use cache |
| `rust/main/chains/hyperlane-cardano/src/cache.rs` | New cache module |

## Testing

- Cache hit avoids API call
- Cache miss fetches and stores
- Invalidation works on UTXO errors
- TTL expires correctly

## Definition of Done

- [ ] Cache implemented
- [ ] Integrated with tx builder
- [ ] Reduced API calls measurable
- [ ] No stale data issues

## Acceptance Criteria

1. Fewer Blockfrost API calls per transaction
2. Correct invalidation on UTXO consumption
3. Configurable TTL

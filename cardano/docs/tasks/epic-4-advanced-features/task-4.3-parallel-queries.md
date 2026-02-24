[← Epic 4: Advanced Features](./EPIC.md) | [Epics Overview](../README.md)

# Task 4.3: Parallel Queries
**Status:** ✅ Done
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

Used three parallelization strategies across all Cardano chain files:

1. **`tokio::try_join!`** for pairs of independent async calls returning `Result`
2. **`tokio::join!`** for pairs where errors are handled separately
3. **`FuturesUnordered`** for concurrent processing of multiple futures (TX and block hash batches)
4. **Sync computation** via `calculate_min_lovelace_sync()` — pre-fetch `coins_per_utxo_byte` once (cached via `OnceCell`), then compute all min_lovelace values synchronously

## Changes

### tx_builder.rs

- `build_process_tx`: `tokio::try_join!` for mailbox UTXO + recipient resolution
- `build_complete_process_tx`: pre-fetch `coins_per_utxo_byte` once, replace 5 async `calculate_min_lovelace()` calls with sync computation
- `estimate_process_cost`: same sync pattern for 2 min_lovelace calls
- `update_ism_validators`: `tokio::join!` for min_lovelace + ISM UTXOs

### mailbox.rs

- `tree_and_tip`: `tokio::try_join!` for mailbox UTXO + finalized block number

### interchain_gas.rs

- Block hash fetching: `stream::iter().buffer_unordered(5)` replaces sequential for-loop
- TX processing: extracted `process_igp_transaction()` helper, concurrent via `FuturesUnordered`

### mailbox_indexer.rs

- TX processing: extracted `process_dispatch_transaction()` helper, concurrent via `FuturesUnordered`

## Files Modified

| File | Changes |
|------|---------|
| `rust/main/chains/hyperlane-cardano/Cargo.toml` | Added `futures` dependency |
| `rust/main/chains/hyperlane-cardano/src/tx_builder.rs` | `tokio::try_join!`, `tokio::join!`, sync min_lovelace |
| `rust/main/chains/hyperlane-cardano/src/mailbox.rs` | `tokio::try_join!` in `tree_and_tip` |
| `rust/main/chains/hyperlane-cardano/src/interchain_gas.rs` | `buffer_unordered`, `FuturesUnordered` |
| `rust/main/chains/hyperlane-cardano/src/mailbox_indexer.rs` | `FuturesUnordered` |

## Testing

- All 61 unit tests pass
- Clippy clean with `-D warnings`
- E2E: 4/4 bidirectional tests passed (Sepolia↔Cardano, 5B + 100B bodies)

## Definition of Done

- [x] Independent queries parallelized
- [x] Transaction building faster
- [x] No correctness issues

## Acceptance Criteria

1. ~~Build time reduced from ~800ms to ~200ms~~ Independent queries now run concurrently, bounded by the rate limiter (`Semaphore(5)` + 150ms sleep)
2. All queries complete correctly — verified via E2E tests
3. Proper error handling maintained — `try_join!` propagates first error

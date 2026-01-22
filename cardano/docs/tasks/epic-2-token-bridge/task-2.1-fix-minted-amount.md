[← Epic 2: Token Bridge](./EPIC.md) | [Epics Overview](../README.md)

# Task 2.1: Fix get_minted_amount() Placeholder

**Status:** ✅ Complete
**Complexity:** Low
**Depends On:** None

## Objective

Fix the placeholder implementation of `get_minted_amount()` in the warp route contract.

## Background

The function currently returns a hardcoded value instead of calculating the actual minted amount from the transaction.

## Current State

**File:** `cardano/contracts/validators/warp_route.ak:484-488`

The function returns 0 instead of calculating actual minted tokens.

## Requirements

### Implementation

The function should:

1. Get the minted value from the transaction using `tx.mint`
2. Convert to value using `value.from_minted_value`
3. Extract tokens under the synthetic policy using `value.tokens`
4. Sum all minted amounts using `dict.foldl`

### Edge Cases to Handle

1. **No tokens minted:** Return 0
2. **Multiple assets:** Sum all (though typically only one synthetic token type)
3. **Negative values:** These represent burns, handle appropriately

## Files to Modify

| File                                         | Changes                 |
| -------------------------------------------- | ----------------------- |
| `cardano/contracts/validators/warp_route.ak` | Fix get_minted_amount() |

## Testing

### Unit Tests

- No minted tokens → returns 0
- Single token minted → returns correct amount
- Multiple tokens → returns sum
- Burn (negative) → returns negative value

### Integration Tests

- Deploy warp route with fix
- Execute synthetic mint
- Verify amount calculation

## Definition of Done

- [x] Function correctly calculates minted amount
- [x] Unit tests pass (94/94)
- [x] No regression in warp route functionality
- [x] Contract compiles and deploys

## Acceptance Criteria

1. `get_minted_amount()` returns actual minted value
2. Works for mint and burn operations
3. Handles edge cases correctly

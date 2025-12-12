[← Epic 2: Token Bridge](./EPIC.md) | [Epics Overview](../README.md)

# Task 2.3: Deploy Synthetic Warp Route
**Status:** ⬜ Not Started
**Complexity:** Medium
**Depends On:** [Task 2.1](./task-2.1-fix-minted-amount.md)

## Objective

Deploy and test a synthetic-type warp route for minting tokens representing remote assets.

## Background

Synthetic warp routes mint tokens when receiving transfers and burn them when sending out. Used for tokens whose native chain is elsewhere.

## Requirements

### 1. Deploy Synthetic Token Policy

Create a minting policy that only allows the warp route to mint/burn tokens. The policy should verify that the warp route script is present in the transaction inputs.

### 2. Deploy Warp Route

Initialize a synthetic-type warp route linked to the minting policy and mailbox.

### 3. Token Metadata (Optional)

Register token metadata for wallet display:
- Token name
- Symbol
- Decimals
- Logo URL

## Technical Notes

### Synthetic Token Policy

The minting policy must verify that only the warp route can mint/burn by checking that the warp route script is included in the transaction inputs.

### Warp Route Integration

When handling receive transfers, the warp route must verify that the correct amount of synthetic tokens are minted in the transaction.

## Files to Modify

| File | Changes |
|------|---------|
| `cardano/cli/src/commands/warp.rs` | Add synthetic init |
| `cardano/contracts/validators/synthetic_token.ak` | Verify minting logic |
| `cardano/contracts/validators/warp_route.ak` | Synthetic handling |

## Testing

### Deployment Tests
- Synthetic policy deploys
- Warp route deploys with synthetic config

### Minting Tests
- Can mint via warp route receive
- Cannot mint outside warp route
- Correct amount minted

### Burning Tests
- Can burn via warp route send
- Amount correctly deducted

## Definition of Done

- [ ] Synthetic token policy deployed
- [ ] Synthetic warp route deployed
- [ ] Minting/burning works correctly
- [ ] Access control enforced

## Acceptance Criteria

1. Synthetic tokens mintable only via warp route
2. Receive transfer mints correct amount
3. Send transfer burns tokens
4. Token metadata registered (if applicable)

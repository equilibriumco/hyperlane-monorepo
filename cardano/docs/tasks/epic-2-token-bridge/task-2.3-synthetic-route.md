[← Epic 2: Token Bridge](./EPIC.md) | [Epics Overview](../README.md)

# Task 2.3: Deploy Synthetic Warp Route

**Status:** ✅ Complete
**Complexity:** Medium
**Depends On:** [Task 2.1](./task-2.1-fix-minted-amount.md)

## Objective

Deploy and test a synthetic-type warp route for minting tokens representing remote assets.

## Background

Synthetic warp routes mint tokens when receiving transfers and burn them when sending out. Used for tokens whose native chain is elsewhere.

## Implementation Summary

### CLI Command Implemented

```bash
hyperlane-cardano warp deploy --token-type synthetic --decimals <DECIMALS>
```

### Deployment Flow

1. Load mailbox policy ID from deployment_info.json
2. Compute warp_route script hash (parameterized by mailbox policy)
3. Compute synthetic_token policy (parameterized by warp_route hash)
4. Deploy warp route with state NFT containing synthetic minting policy

### Key Differences from Collateral Route

- **No vault needed**: Synthetic routes don't lock tokens in a vault
- **Minting policy**: The synthetic_token.ak policy is parameterized with the warp route hash
- **Token creation**: Tokens are minted on-demand when receiving transfers from other chains

### Files Modified

| File                               | Changes                                       |
| ---------------------------------- | --------------------------------------------- |
| `cardano/cli/src/commands/warp.rs` | Implemented `deploy_synthetic_route` function |
| `cardano/cli/src/utils/cbor.rs`    | Added `build_warp_route_synthetic_datum`      |

## Technical Notes

### Synthetic Token Policy

The minting policy verifies that only the warp route can mint/burn by checking that the warp route script is included in the transaction inputs:

```aiken
validator synthetic_token(warp_route_hash: ScriptHash) {
  mint(_redeemer: Data, _policy_id: ByteArray, tx: Transaction) {
    let warp_route_involved =
      list.any(
        tx.inputs,
        fn(input) {
          when input.output.address.payment_credential is {
            Script(hash) -> hash == warp_route_hash
            _ -> False
          }
        },
      )
    warp_route_involved
  }
}
```

### Warp Route Integration

When handling receive transfers, the warp route validates that the correct amount of synthetic tokens are minted:

```aiken
fn validate_synthetic_mint(
  minting_policy: ScriptHash,
  recipient: ByteArray,
  amount: Int,
  tx: Transaction,
) -> Bool {
  let minted = get_minted_amount(tx, minting_policy)
  expect minted == amount
  expect recipient_receives_tokens(tx, recipient, minting_policy, "", amount)
  True
}
```

## Testing

### Deployment Tests

- ✅ Dry run shows correct script hash computation
- ✅ Warp route deploys with synthetic config

### Minting Tests

- ⬜ Can mint via warp route receive (requires Task 2.5)
- ⬜ Cannot mint outside warp route (requires Task 2.5)
- ⬜ Correct amount minted (requires Task 2.5)

### Burning Tests

- ⬜ Can burn via warp route send (requires Task 2.5)
- ⬜ Amount correctly deducted (requires Task 2.5)

## Definition of Done

- [x] Synthetic token policy parameterized by warp route hash
- [x] CLI command implemented for deployment
- [x] Synthetic warp route deployed on-chain
- [ ] Minting/burning works correctly (requires Task 2.5)
- [ ] Access control enforced (requires Task 2.5)

## Acceptance Criteria

1. ✅ Synthetic tokens mintable only via warp route (enforced by policy)
2. ⬜ Receive transfer mints correct amount (requires Task 2.5)
3. ⬜ Send transfer burns tokens (requires Task 2.5)
4. ⬜ Token metadata registered (if applicable)

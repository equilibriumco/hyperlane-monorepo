[← Epic 2: Token Bridge](./EPIC.md) | [Epics Overview](../README.md)

# Task 2.2: Deploy Collateral Warp Route

**Status:** ✅ Complete
**Complexity:** Medium
**Depends On:** [Task 2.1](./task-2.1-fix-minted-amount.md)

## Objective

Deploy and test a collateral-type warp route for bridging native Cardano tokens.

## Background

Collateral warp routes lock native tokens in a vault when transferring out and release them when receiving transfers in.

## Implementation Summary

### Test Token Created

- **Policy ID:** `908d51752e4c76fe1404a92b1276b1c1093dae0c7f302c5442f0177e`
- **Asset Name:** WARPTEST
- **Amount:** 1,000,000 tokens
- **TX:** `fc9c88d5d7de7cb997c993eaf727104e39c404bbff0d40604851b5088e1f6013`

### Vault Deployed

- **Script Hash:** `a3a296f04fc7387fef4c1740fa755f1b7ca42c57e42bf8c02532dae5`
- **Address:** `addr_test1wz3699hsflrnsll0fst5p7n4tudhefpv2ljzh7xqy5ed4egpdvw7c`
- **NFT Policy:** `48889376280aedb0cb9a457d061cc5c070a36ebb9b4769b795c4c342`
- **TX:** `101b1469a4c6faae8e9d50f7fbfcd9cb74bef85b6256bf2a6884c9f453cd3046`

### Warp Route Deployed

- **Script Hash:** `3d076cd4c8b5e8f66ae70f38aeae1fcfe5764183a1803725191e0b3c`
- **Address:** `addr_test1wq7swmx5ez673an2uu8n3t4wrl872ajpswscqde9ry0qk0qxrz9y6`
- **NFT Policy:** `0bb7cce341d209b951285392342ea6c4fa9b1801a3c9c957008eb757`
- **TX:** `9fd3a9dc8dc12adf4a8ad7e7d336572fe33e6c8a19e634b9e5f06bf23477eeae`

## CLI Commands Implemented

### Token Deployment

```bash
hyperlane-cardano token deploy --name <NAME> --amount <AMOUNT>
```

### Warp Route Deployment

```bash
hyperlane-cardano warp deploy \
  --token-type collateral \
  --token-policy <POLICY_ID> \
  --token-asset <ASSET_NAME> \
  --decimals <DECIMALS>
```

## Files Modified

| File                                         | Changes                                                           |
| -------------------------------------------- | ----------------------------------------------------------------- |
| `cardano/cli/src/commands/warp.rs`           | Implemented `deploy_collateral_route` function                    |
| `cardano/cli/src/commands/token.rs`          | Added test token minting command                                  |
| `cardano/cli/src/utils/cbor.rs`              | Added `build_vault_datum` and `build_warp_route_collateral_datum` |
| `cardano/cli/src/utils/plutus.rs`            | Added `get_script_hash` and `encode_script_hash_param`            |
| `cardano/contracts/validators/test_token.ak` | Created one-shot minting policy for test tokens                   |

## Testing

### Deployment Tests

- ✅ Vault deploys successfully
- ✅ Warp route deploys successfully
- ✅ Configuration saved to deployment_info.json

### Functional Tests

- ⬜ Can lock tokens in vault (requires Task 2.5)
- ⬜ Can release tokens from vault (requires Task 2.5)
- ⬜ Access control enforced (requires Task 2.5)

## Definition of Done

- [x] Vault deployed on Preview
- [x] Collateral warp route deployed
- [x] CLI commands work
- [x] Configuration documented
- [x] Ready for remote enrollment (Task 2.4)

## Acceptance Criteria

1. ✅ Vault holds test tokens securely
2. ⬜ Warp route can lock and release tokens (requires Task 2.5)
3. ⬜ Only authorized operations succeed (requires Task 2.5)

[← Epic 2: Token Bridge](./EPIC.md) | [Epics Overview](../README.md)

# Task 2.2: Deploy Collateral Warp Route
**Status:** ⬜ Not Started
**Complexity:** Medium
**Depends On:** [Task 2.1](./task-2.1-fix-minted-amount.md)

## Objective

Deploy and test a collateral-type warp route for bridging native Cardano tokens.

## Background

Collateral warp routes lock native tokens in a vault when transferring out and release them when receiving transfers in.

## Requirements

### 1. Choose Test Token

Options:
- Create a test token on Preview
- Use an existing Preview testnet token
- Use tADA (test ADA variant)

### 2. Deploy Vault

Use CLI to initialize a vault for the chosen token. The vault contract holds locked collateral.

### 3. Deploy Warp Route

Initialize a collateral-type warp route linked to the vault and mailbox.

### 4. CLI Implementation

Implement or verify `warp init` command with:
- `--type collateral` flag
- `--token <policy_id>.<asset_name>` for the token to bridge
- `--vault <vault_script_hash>` for the vault address
- `--mailbox <mailbox_script_hash>` for message dispatch

## Files to Modify

| File | Changes |
|------|---------|
| `cardano/cli/src/commands/warp.rs` | Add/verify init command |
| `cardano/contracts/validators/warp_route.ak` | Verify collateral logic |
| `cardano/contracts/validators/vault.ak` | Verify vault logic |

## Testing

### Deployment Tests
- Vault deploys successfully
- Warp route deploys successfully
- Configuration saved to deployment_info.json

### Functional Tests
- Can lock tokens in vault
- Can release tokens from vault
- Access control enforced

## Definition of Done

- [ ] Vault deployed on Preview
- [ ] Collateral warp route deployed
- [ ] CLI commands work
- [ ] Configuration documented
- [ ] Ready for remote enrollment (Task 2.4)

## Acceptance Criteria

1. Vault holds test tokens securely
2. Warp route can lock and release tokens
3. Only authorized operations succeed

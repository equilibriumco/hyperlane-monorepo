[← Back to Epics Overview](../README.md)

# Epic 2: Token Bridge (Warp Routes)

**Priority:** 🟡 High
**Status:** 🟡 In Progress
**Phase:** 2 - Feature Completion
**Depends On:** Epic 1 (for outgoing transfers)

## Summary

Deploy and test warp routes for cross-chain token transfers. The on-chain contracts are implemented but have not been deployed or tested end-to-end.

## Business Value

- Enables cross-chain token bridging
- Allows Cardano native tokens to be used on other chains
- Brings tokens from other chains to Cardano

## Current State

### Implemented

- Warp route contract (`contracts/validators/warp_route.ak`)
- Vault contract (`contracts/validators/vault.ak`)
- Synthetic token minting policy (`contracts/validators/synthetic_token.ak`)
- CLI commands structure (`cardano/cli/src/commands/warp.rs`)

### Known Issues

- `get_minted_amount()` in `warp_route.ak:484-488` returns placeholder value

### Token Types Supported

| Type       | Lock/Mint                  | Release/Burn         | Use Case                  |
| ---------- | -------------------------- | -------------------- | ------------------------- |
| Collateral | Lock native token in vault | Release from vault   | Bridge Cardano tokens out |
| Synthetic  | Mint synthetic token       | Burn synthetic token | Receive remote tokens     |
| Native     | Lock ADA in vault          | Release ADA          | Bridge ADA out            |

## Tasks

| #   | Task                                                 | Status | Depends On  | Description                      |
| --- | ---------------------------------------------------- | ------ | ----------- | -------------------------------- |
| 2.1 | [Fix Minted Amount](./task-2.1-fix-minted-amount.md) | ✅     | -           | Fix placeholder in warp_route.ak |
| 2.2 | [Collateral Route](./task-2.2-collateral-route.md)   | ✅     | 2.1         | Deploy collateral warp route     |
| 2.3 | [Synthetic Route](./task-2.3-synthetic-route.md)     | ✅     | 2.1         | Deploy synthetic warp route      |
| 2.4 | [Remote Enrollment](./task-2.4-remote-enrollment.md) | ⬜     | 2.2, 2.3    | Enroll remote routers            |
| 2.5 | [Transfer Testing](./task-2.5-transfer-testing.md)   | ⬜     | Epic 1, 2.4 | E2E transfer tests               |

## Task Dependency Graph

```
Task 2.1 (Fix Minted Amount)
    │
    ├──────────────────────┐
    ▼                      ▼
Task 2.2 (Collateral)  Task 2.3 (Synthetic)
    │                      │
    └──────────┬───────────┘
               ▼
        Task 2.4 (Enrollment)
               │
               ▼
        Task 2.5 (E2E Testing)
               │
               │ Depends on Epic 1 for
               │ outgoing transfers
               ▼
        ✅ Token Bridge Complete
```

## Technical Architecture

### Warp Route State

```aiken
type WarpRouteDatum {
  owner: Address,
  mailbox: ScriptHash,
  route_type: RouteType,
  enrolled_routes: Dict<Int, ByteArray>,  // domain -> router address
  vault: Option<ScriptHash>,
  token: Option<AssetClass>,
  synthetic_policy: Option<PolicyId>,
}
```

### Transfer Flow (Cardano → Remote)

```
1. User initiates transfer via CLI
2. Tokens locked in vault (collateral) or burned (synthetic)
3. Dispatch message sent via mailbox
4. [Epic 1 flow: Validator → Checkpoint → Relayer]
5. Remote warp route receives message
6. Remote tokens minted or released to recipient
```

### Transfer Flow (Remote → Cardano)

```
1. Remote chain initiates transfer
2. Message delivered to Cardano mailbox
3. Mailbox invokes warp route as recipient
4. Tokens released from vault (collateral) or minted (synthetic)
5. Tokens sent to recipient address
```

## Key Files

| File                                              | Purpose                    |
| ------------------------------------------------- | -------------------------- |
| `cardano/contracts/validators/warp_route.ak`      | Main warp route contract   |
| `cardano/contracts/validators/vault.ak`           | Token vault for collateral |
| `cardano/contracts/validators/synthetic_token.ak` | Synthetic token minting    |
| `cardano/cli/src/commands/warp.rs`                | CLI commands               |

## Definition of Done

- [x] `get_minted_amount()` correctly calculates minted tokens
- [x] Collateral warp route deployed and tested
- [x] Synthetic warp route deployed and tested
- [ ] Remote routes enrolled on both ends
- [ ] Cardano → Remote transfer succeeds
- [ ] Remote → Cardano transfer succeeds
- [ ] Round-trip transfer test passes
- [ ] Documentation complete

## Risks & Mitigations

| Risk                    | Impact   | Mitigation                   |
| ----------------------- | -------- | ---------------------------- |
| Token value discrepancy | Critical | Thorough amount validation   |
| Vault drainage          | Critical | Access control audit         |
| Synthetic inflation     | Critical | Minting authorization checks |

## Acceptance Criteria

1. Collateral route: Lock → Transfer → Release works
2. Synthetic route: Mint → Transfer → Burn works
3. Bidirectional transfers succeed
4. No token value loss in transfers
5. CLI commands documented and tested

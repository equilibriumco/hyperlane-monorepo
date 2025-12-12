[← Epic 2: Token Bridge](./EPIC.md) | [Epics Overview](../README.md)

# Task 2.5: Transfer Testing
**Status:** ⬜ Not Started
**Complexity:** High
**Depends On:** Epic 1, [Task 2.4](./task-2.4-remote-enrollment.md)

## Objective

Test end-to-end token transfers in both directions.

## Prerequisites

- Epic 1 complete (bidirectional messaging)
- Warp routes deployed (Tasks 2.2, 2.3)
- Remote routes enrolled (Task 2.4)
- Matching warp route on remote chain (Fuji)

## Test Scenarios

### 1. Cardano → Remote (Collateral)

Steps:
1. Lock tokens and send to Fuji using `warp transfer` command
2. Verify tokens locked in Cardano vault
3. Verify message dispatched via mailbox
4. Wait for validator signing and relayer delivery
5. Verify synthetic tokens minted on Fuji

### 2. Remote → Cardano (Collateral)

Steps:
1. On Fuji: burn synthetic and send to Cardano
2. Wait for message delivery to Cardano warp route
3. Verify collateral released from vault
4. Verify tokens sent to recipient

### 3. Round-Trip Transfer

1. Send 1000 tokens Cardano → Fuji
2. Verify receipt on Fuji
3. Send 500 tokens Fuji → Cardano
4. Verify receipt on Cardano
5. Verify remaining balances correct

## CLI Commands

### Transfer Command

```
hyperlane-cardano warp transfer \
  --route <script_hash> \
  --destination <domain_id> \
  --recipient <address> \
  --amount <amount>
```

### Balance Command

```
hyperlane-cardano warp balance --route <script_hash>
```

Should show vault balance (collateral) or synthetic supply depending on route type.

## Definition of Done

- [ ] Transfer CLI command implemented
- [ ] Cardano → Remote transfer works
- [ ] Remote → Cardano transfer works
- [ ] Balance verification works
- [ ] Round-trip test passes
- [ ] No token value loss

## Acceptance Criteria

1. Tokens correctly locked/released on Cardano
2. Correct amounts on both chains
3. All test scenarios pass
4. Documented test procedures

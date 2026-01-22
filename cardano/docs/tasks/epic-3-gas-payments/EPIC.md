[← Back to Epics Overview](../README.md)

# Epic 3: Gas Payments (IGP)

**Priority:** 🟡 High
**Status:** 🔄 In Progress
**Phase:** 2 - Feature Completion

## Summary

Complete the Interchain Gas Paymaster (IGP) integration to enable gas payment for cross-chain messages. This implementation must be fully compatible with the [Hyperlane IGP specification](https://docs.hyperlane.xyz/docs/protocol/core/interchain-gas-payment).

## Business Value

- Enables paid message delivery guarantees
- Allows relayers to be compensated for gas costs
- Required for production-grade message delivery
- Enables sustainable relayer economics

## Hyperlane IGP Specification Compliance

Per the official Hyperlane documentation, the IGP must support:

| Requirement | Status | Notes |
|-------------|--------|-------|
| `payForGas(messageId, destination, gasAmount)` | ✅ Implemented | CLI command in Task 3.1 |
| `quoteGasPayment(destination, gasAmount)` | ✅ Implemented | CLI command in Task 3.1 |
| Gas oracles per destination | ✅ Implemented | `GasOracleConfig` per domain |
| `GasPayment` event emission | ✅ Adapted | Redeemer serves as event on Cardano |
| Refund address support | ⬜ Deferred | Planned for [Task 4.6](../epic-4-advanced-features/task-4.6-igp-refund.md) |
| Post-dispatch hook integration | ❌ Missing | For automatic gas payment at dispatch |
| Relayer gas payment indexing | ✅ Implemented | Task 3.2 |

## Current State

### Implemented
- On-chain IGP contract (`contracts/validators/igp.ak`)
- IGP initialized on Preview testnet (Task 3.0 ✅)
- CLI commands: show, quote, set-oracle, pay-for-gas, claim (Task 3.1 ✅)
- Gas payment indexing via `fetch_logs_in_range` (Task 3.2 ✅)
- `parse_pay_for_gas_redeemer` and `calculate_igp_payment` functions
- `InterchainGasPaymaster` trait implementation (partial)
- Gas calculation logic
- Owner-only oracle configuration

### Missing
- Refund address handling (deferred to [Task 4.6](../epic-4-advanced-features/task-4.6-igp-refund.md))
- Post-dispatch hook integration (Task 3.5)
- End-to-end testing (Task 3.6)

## Tasks

| # | Task | Status | Depends On | Description |
|---|------|--------|------------|-------------|
| 3.0 | [Init IGP](./task-3.0-init-igp.md) | ✅ | - | **PREREQUISITE**: Initialize IGP contract on testnet |
| 3.1 | [CLI Commands](./task-3.1-cli-commands.md) | ✅ | 3.0 | pay-for-gas, quote, claim, set-oracle, show |
| 3.2 | [RPC Endpoint](./task-3.2-rpc-endpoint.md) | ✅ | 3.0 | Implement gas payment indexing and quote endpoint |
| 3.3 | [Contract Enhancements](./task-3.3-contract-enhancements.md) | ⏸️ | - | Refund handling moved to [Task 4.6](../epic-4-advanced-features/task-4.6-igp-refund.md) |
| 3.4 | [Relayer Integration](./task-3.4-relayer-integration.md) | ⬜ | 3.2 | Relayer queries and enforces gas payments |
| 3.5 | [Post-Dispatch Hook](./task-3.5-post-dispatch-hook.md) | ⬜ | 3.1, 3.2 | Automatic gas payment at dispatch time |
| 3.6 | [E2E Testing](./task-3.6-e2e-testing.md) | ⬜ | 3.1-3.5 | Test full payment flow |

## Task Dependency Graph

```
                    Task 3.0 (Init IGP)
                           │
           ┌───────────────┼───────────────┐
           │               │               │
           ▼               ▼               ▼
    Task 3.3         Task 3.1        Task 3.2
    (Contract)       (CLI)           (RPC)
           │               │               │
           └───────┬───────┴───────┬───────┘
                   │               │
                   ▼               ▼
           Task 3.5          Task 3.4
           (Post-Dispatch)   (Relayer)
                   │               │
                   └───────┬───────┘
                           │
                           ▼
                   Task 3.6 (E2E Testing)
```

**Implementation order (matches task numbers):**
1. **Task 3.0** - Init IGP (prerequisite for testing all others)
2. **Task 3.1** - CLI Commands (enables manual testing)
3. **Task 3.2** - RPC Endpoint (verify with payments from CLI)
4. **Task 3.3** - Contract Enhancements (can be done in parallel with 3.1/3.2)
5. **Task 3.4** - Relayer Integration
6. **Task 3.5** - Post-Dispatch Hook
7. **Task 3.6** - E2E Testing

## Technical Architecture

### IGP Contract Actions

```
PayForGas:
  - message_id: 32-byte message identifier
  - destination: destination domain ID
  - gas_amount: units of gas to pay for

Claim:
  - amount: lovelace amount to claim
  - (beneficiary verified from datum)

SetGasOracle:
  - domain: destination domain ID
  - config: GasOracleConfig with gas_price and token_exchange_rate
```

### Gas Oracle Configuration

Each destination domain has a `GasOracleConfig`:
- `gas_price`: The gas price on the destination chain (in destination native units)
- `token_exchange_rate`: Exchange rate between origin token (ADA) and destination token

### Gas Payment Flow

```
Standard Flow:
1. User dispatches message → receives message_id
2. User calls quoteGasPayment to get required ADA
3. User calls PayForGas with message_id, destination, gas_amount
4. IGP validates payment >= required
5. Relayer indexes GasPayment from transaction
6. Relayer delivers message to destination
7. Beneficiary claims accumulated fees

Note: Refund support planned for [Task 4.6](../epic-4-advanced-features/task-4.6-igp-refund.md)

Post-Dispatch Hook Flow (automatic):
1. User dispatches message with gas payment in single transaction
2. Post-dispatch hook automatically calls PayForGas
3. Relayer handles delivery
```

### Gas Calculation Formula

```
required_payment = (gas_amount * gas_price * token_exchange_rate) / PRECISION

Where:
- gas_amount: Requested gas units
- gas_price: Destination chain gas price
- token_exchange_rate: ADA to destination token rate
- PRECISION: 10^18 (to match EVM precision)
```

### Event Handling on Cardano

Unlike EVM which has explicit events, Cardano uses transaction redeemers as the event log. The relayer indexes transactions to the IGP script address and parses the `PayForGas` redeemer to extract:
- `message_id`
- `destination`
- `gas_amount`
- `payment` (calculated from UTXO value difference)

## Key Files

| File | Purpose |
|------|---------|
| `cardano/contracts/validators/igp.ak` | On-chain IGP contract |
| `rust/main/chains/hyperlane-cardano/src/interchain_gas.rs` | Rust IGP client |
| `cardano/cli/src/commands/igp.rs` | CLI commands (to create) |

## Definition of Done

- [x] IGP deployed and configured on testnet (Task 3.0)
- [x] CLI commands for all IGP operations work (Task 3.1)
- [x] `quoteGasPayment` CLI command implemented (Task 3.1)
- [x] Gas payments indexed correctly from chain (Task 3.2)
- [ ] Relayer queries and enforces Cardano IGP (Task 3.4)
- [ ] Post-dispatch hook enables automatic payment (Task 3.5)
- [ ] E2E test: quote → pay → deliver → claim (Task 3.6)
- [ ] Documentation complete

> **Note:** Refund address support deferred to [Task 4.6](../epic-4-advanced-features/task-4.6-igp-refund.md)

## CLI Interface

```bash
# Quote gas payment (returns required ADA)
hyperlane-cardano igp quote \
  --destination 43113 \
  --gas-amount 200000

# Pay for gas
hyperlane-cardano igp pay-for-gas \
  --message-id 0x1234...abcd \
  --destination 43113 \
  --gas-amount 200000

# Show IGP state
hyperlane-cardano igp show

# Set gas oracle (owner only)
hyperlane-cardano igp set-oracle \
  --domain 43113 \
  --token-exchange-rate 1000000 \
  --gas-price 25000000000

# Claim fees (beneficiary only)
hyperlane-cardano igp claim --amount 1000000
```

## Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Oracle price manipulation | High | Owner-only oracle updates, monitoring |
| Fee calculation errors | High | Thorough testing with EVM reference values |
| Relayer not checking IGP | Medium | Integration tests, enforcement config |
| Post-dispatch hook failures | Medium | Graceful fallback to manual payment |

## Acceptance Criteria

1. Hyperlane IGP specification compliance (refund deferred to Task 4.6)
2. `quoteGasPayment` returns accurate estimates ✅
3. Gas payments properly indexed by relayer
4. All CLI commands work on testnet ✅
5. Relayer enforces gas payments
6. Post-dispatch hook enables atomic dispatch+pay
7. Beneficiary can claim accumulated fees ✅
8. Oracle configuration works correctly ✅

[← Back to Epics Overview](../README.md)

# Epic 4: Advanced Features

**Priority:** 🟡 High
**Status:** ⬜ Not Started
**Phase:** 2 - Feature Completion

## Summary

Implement advanced features including performance optimizations, contract upgradeability, and parallel inbound processing. These enhance the system's scalability and maintainability.

## Business Value

- **Performance:** Reduces latency and API costs for high-volume usage
- **Upgradeability:** Allows bug fixes and improvements without redeploying entire contract suite
- **Scalability:** Increases inbound throughput from ~3 messages/minute to N messages/block
- **Per-recipient ISM:** Implemented as part of parallel processing (Task 4.5)

## Tasks

| #   | Task                                                             | Status | Depends On | Description                                                   |
| --- | ---------------------------------------------------------------- | ------ | ---------- | ------------------------------------------------------------- |
| 4.1 | [State NFT as Address](./task-4.1-nft-lookups.md)                | ⬜     | -          | Use state NFT policy as Hyperlane address                     |
| 4.2 | [Ref Script Cache](./task-4.2-ref-script-cache.md)               | ⬜     | -          | Cache reference script UTXOs                                  |
| 4.3 | [Parallel Queries](./task-4.3-parallel-queries.md)               | ⬜     | -          | Parallelize Blockfrost calls                                  |
| 4.4 | [NFT-Based Contract Identity](./task-4.4-nft-identity.md)        | ⬜     | -          | Stable identity across upgrades                               |
| 4.5 | [Parallel Inbound Processing](./task-4.5-parallel-processing.md) | ⬜     | 4.4        | Reference inputs for scalability (includes per-recipient ISM) |
| 4.6 | [IGP Refund Support](./task-4.6-igp-refund.md)                   | ⬜     | 3.1        | Refund unused gas payments to users                           |

## Task Details

### 4.1 State NFT Policy as Hyperlane Address

**Current State:** Script hash used as Hyperlane address. Warp routes need unused `_state_nft_policy_id` parameter for uniqueness. Registry lookups are O(n).

**Solution:** Use state NFT policy ID as the Hyperlane address instead of script hash:

```
┌─────────────────────────────────────────────────────────────────┐
│   Before: script_hash = Hyperlane address                       │
│     - Warp routes need _state_nft_policy_id for uniqueness      │
│     - Registry lookup: O(n) iteration by script_hash            │
│                                                                  │
│   After: state_nft_policy = Hyperlane address                   │
│     - Warp routes only need mailbox_policy_id parameter         │
│     - Registry lookup: O(1) via asset query                     │
│     - Consistent pattern for all recipient types                │
└─────────────────────────────────────────────────────────────────┘
```

Related to Task 4.4 - both establish NFT policies as stable identifiers.

### 4.2 Reference Script Cache

**Problem:** Reference scripts fetched from Blockfrost for each transaction.

**Solution:** In-memory cache with TTL, invalidated on UTXO consumption.

### 4.3 Parallel Queries

**Problem:** Sequential Blockfrost queries that could run in parallel.

**Solution:** Use `tokio::try_join!` for independent queries.

### 4.4 NFT-Based Contract Identity

**Problem:** Contracts parameterized by policy IDs create cascading upgrade dependencies.

**Solution:** Use stable identity NFTs instead of policy ID parameterization:

```
┌─────────────────────────────────────────────────────────────────┐
│   mailbox_identity_nft (minted once, never changes)             │
│         │                                                        │
│         ├──► mailbox v1 (holds the NFT)                         │
│         │         │                                              │
│         │         ▼ (upgrade: migrate NFT)                      │
│         └──► mailbox v2 (receives the NFT)                      │
│                                                                  │
│   Other contracts check for identity NFT, not policy ID         │
└─────────────────────────────────────────────────────────────────┘
```

### 4.5 Parallel Inbound Processing

**Problem:** Mailbox UTXO consumed for every `process` creates bottleneck (~3 msg/min).

**Solution:** Move validation to minting policy, use reference inputs:

```
┌─────────────────────────────────────────────────────────────────┐
│   Reference inputs (read-only, no contention):                  │
│     - mailbox_utxo     → local_domain, default_ism              │
│     - ism_utxo         → validator_set, threshold               │
│                                                                  │
│   Spent inputs (per message):                                   │
│     - recipient_utxo   → only this has contention               │
│                                                                  │
│   Different recipients → FULLY PARALLEL                         │
└─────────────────────────────────────────────────────────────────┘
```

## Performance Targets

| Metric                  | Current       | Target                            |
| ----------------------- | ------------- | --------------------------------- |
| Recipient lookup        | O(n)          | O(1)                              |
| Transaction build time  | ~2s           | ~500ms                            |
| Blockfrost calls per tx | ~8            | ~4                                |
| Inbound throughput      | ~3 msg/min    | N msg/block (N=unique recipients) |
| Upgrade impact          | All contracts | Single contract                   |

## Definition of Done

- [ ] State NFT policy used as Hyperlane address for all recipients
- [ ] Reference scripts cached in memory
- [ ] Independent queries parallelized
- [ ] Mailbox can be upgraded without redeploying recipients
- [ ] Multiple messages to different recipients processed in same block
- [ ] Per-recipient ISM honored (part of parallel processing)
- [ ] Benchmark shows measurable improvement
- [ ] No regression in correctness

## Risks & Mitigations

| Risk                                    | Impact | Mitigation                                     |
| --------------------------------------- | ------ | ---------------------------------------------- |
| Cache staleness                         | Medium | Conservative TTL, refresh on failure           |
| Address scheme change                   | Low    | Clean break, no mainnet deployments yet        |
| Parallel query race conditions          | Low    | Proper async handling                          |
| Migration complexity for upgradeability | High   | Phased rollout, backwards compatibility period |
| Minting policy size increase            | Medium | Optimize code, potentially split validation    |

## Acceptance Criteria

1. Custom ISM honored for recipients that set it
2. State NFT policy is the Hyperlane address (O(1) lookups)
3. Transaction building is measurably faster
4. Mailbox upgrade does not require recipient redeployment
5. Multiple inbound messages processed in parallel (different recipients)
6. All existing tests pass

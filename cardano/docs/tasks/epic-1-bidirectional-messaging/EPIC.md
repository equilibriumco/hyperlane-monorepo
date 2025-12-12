[← Back to Epics Overview](../README.md)

# Epic 1: Bidirectional Messaging

**Priority:** 🔴 Critical
**Status:** ⬜ Not Started
**Phase:** 1 - Core Functionality

## Summary

Enable outgoing messages from Cardano to remote chains. Currently, only incoming messages (Remote → Cardano) work. This epic completes the bidirectional messaging capability.

## Business Value

- Enables Cardano dApps to send cross-chain messages
- Required for token bridging (Epic 2) and all outgoing use cases
- Completes the core Hyperlane functionality on Cardano

## Current State

```
Working:
Fuji → Hyperlane Validators → Relayer → Cardano Mailbox.process() ✅

Missing:
Cardano Mailbox.dispatch() → ??? → Relayer → Fuji ❌
```

## Tasks

| # | Task | Status | Depends On | Description |
|---|------|--------|------------|-------------|
| 1.1 | [MerkleTree Hook](./task-1.1-merkletree-hook.md) | ⬜ | - | Implement merkle tree state retrieval |
| 1.2 | [Validator Agent](./task-1.2-validator-agent.md) | ⬜ | 1.1 | Add Cardano support to validator |
| 1.3 | [Checkpoint Syncer](./task-1.3-checkpoint-syncer.md) | ⬜ | 1.1, 1.2 | Sync checkpoints from Cardano |
| 1.4 | [Validator Config](./task-1.4-validator-config.md) | ⬜ | 1.2 | Configuration and CLI commands |
| 1.5 | [Dispatch CLI](./task-1.5-dispatch-cli.md) | ⬜ | - | CLI command to dispatch messages |
| 1.6 | [Validator Announce](./task-1.6-validator-announce.md) | ⬜ | 1.4 | Register validators on-chain for relayer discovery |
| 1.7 | [E2E Testing](./task-1.7-e2e-testing.md) | ⬜ | 1.1-1.6 | End-to-end and bidirectional tests |

## Task Dependency Graph

```
Task 1.1 (MerkleTree Hook)
    │
    ├──────────────────────┐
    ▼                      ▼
Task 1.2 (Validator)   Task 1.5 (Dispatch CLI)
    │                      │
    ▼                      │
Task 1.3 (Checkpoint)      │
    │                      │
    ▼                      │
Task 1.4 (Config)          │
    │                      │
    ▼                      │
Task 1.6 (Validator Announce)
    │                      │
    └──────────┬───────────┘
               ▼
        Task 1.7 (E2E Testing)
```

## Technical Architecture

### Message Dispatch Flow

```
1. User calls CLI: hyperlane-cardano mailbox dispatch
2. CLI builds transaction with Dispatch redeemer
3. Transaction updates mailbox:
   - Increments outbound_nonce
   - Adds message hash to merkle tree
4. Validator agent indexes dispatched message
5. Validator signs checkpoint (merkle_root + index + message_id)
6. Checkpoint stored in configured storage (S3/local)
7. Relayer fetches checkpoint and message metadata
8. Relayer delivers to destination chain
```

### Key Components

| Component | Location | Purpose |
|-----------|----------|---------|
| MerkleTreeHook | `rust/main/chains/hyperlane-cardano/src/merkle_tree_hook.rs` | Read merkle tree from mailbox |
| Validator Agent | `rust/main/agents/validator/` | Sign checkpoints |
| CLI | `cardano/cli/src/commands/mailbox.rs` | Dispatch command |
| Mailbox Contract | `cardano/contracts/validators/mailbox.ak` | On-chain dispatch logic |

## Definition of Done

- [ ] Validator agent runs with Cardano origin configured
- [ ] Messages dispatched from Cardano appear in checkpoint storage
- [ ] Relayer picks up and delivers messages to destination
- [ ] Bidirectional test (Fuji → Cardano → Fuji) passes
- [ ] Documentation updated

## Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Blockfrost rate limits | High | Implement caching, request batching |
| Merkle tree parsing complexity | Medium | Thorough unit testing with real data |
| Checkpoint format mismatch | High | Test against EVM reference implementation |

## Acceptance Criteria

1. `hyperlane-cardano mailbox dispatch` command works
2. Validator agent signs checkpoints for Cardano messages
3. Message delivered end-to-end (Cardano → Fuji)
4. Bidirectional round-trip test passes
5. No regression in incoming message flow

[← Epic 1: Bidirectional Messaging](./EPIC.md) | [Epics Overview](../README.md)

# Task 1.7: End-to-End Testing
**Status:** ⬜ Not Started
**Complexity:** Medium
**Depends On:** Tasks 1.1-1.6

## Objective

Create comprehensive end-to-end tests for bidirectional messaging between Cardano and other chains.

## Background

With all components in place, we need to verify the complete message flow works in both directions.

## Test Scenarios

- Cardano → Fuji (Outgoing)
- Fuji → Cardano (Incoming)

## Test Cases

### 1. Cardano → Fuji (Outgoing)

**Script:** `cardano/scripts/test-outgoing-message.sh`

Steps:
1. Dispatch message from Cardano using CLI
2. Capture message ID and transaction hash
3. Wait for Cardano confirmation (~60s)
4. Check validator checkpoint storage for signed checkpoint
5. Wait for relayer delivery (~120s)
6. Verify delivery on Fuji via Hyperlane explorer or RPC

### 2. Fuji → Cardano (Incoming)

**Script:** `cardano/scripts/test-incoming-message.sh`

Steps:
1. Dispatch message from Fuji
2. Wait for relayer (~120s)
3. Query Cardano recipient history
4. Verify message appears in recipient state

### 3. Bidirectional Round-Trip

**Script:** `cardano/scripts/test-bidirectional.sh`

Steps:
1. Run incoming message test (Fuji → Cardano)
2. Run outgoing message test (Cardano → Fuji)
3. Verify both directions complete successfully

## Test Environment Setup

### Prerequisites
- `BLOCKFROST_API_KEY` environment variable
- `CARDANO_SIGNING_KEY` path to signing key
- `VALIDATOR_KEY` for validator agent
- Cardano mailbox deployed on Preview
- Recipient registered
- ISM configured with validators
- Validator announced (Task 1.6)
- Validator agent running
- Relayer running with Cardano support

### Test Configuration

Create `cardano/tests/fixtures/test-config.json` with:
- Cardano domain (2003)
- Fuji domain (43113)
- Test recipient address
- Mailbox policy ID
- Timeout settings

## Rust Integration Tests

**File:** `rust/main/chains/hyperlane-cardano/tests/e2e.rs`

Create ignored tests (run with `--ignored` flag) that:
- Test outgoing message dispatch and nonce increment
- Test validator checkpoint signing
- Verify checkpoint signature

## Definition of Done

- [ ] Outgoing message test script works
- [ ] Incoming message test script works
- [ ] Bidirectional test passes
- [ ] Rust integration tests implemented
- [ ] Tests documented with setup instructions
- [ ] Test can be run on demand
- [ ] Failure modes properly detected

## Test Matrix

| Scenario | Origin | Destination | Expected Result |
|----------|--------|-------------|-----------------|
| Basic outgoing | Cardano | Fuji | Delivered in <5min |
| Basic incoming | Fuji | Cardano | Delivered in <5min |
| Round-trip | Fuji→Cardano→Fuji | | Both delivered |
| Large body | Cardano | Fuji | Delivered (test limits) |
| Empty body | Cardano | Fuji | Delivered |

## Acceptance Criteria

1. All test scripts pass on Preview testnet
2. Tests detect common failure modes
3. Clear output showing test progress
4. Tests can be run repeatedly
5. Documentation sufficient for others to run

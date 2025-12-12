[← Back to Epics Overview](../README.md)

# Epic 6: Security Audit

**Priority:** 🔴 Critical
**Status:** ⬜ Not Started
**Phase:** 4 - Security Gate (Before Mainnet)

## Summary

Comprehensive security audit of the Hyperlane-Cardano integration before mainnet deployment. This is the final gate - all other epics should be complete before this begins.

## Business Value

- Ensures security of user funds and messages
- Required for mainnet deployment
- Builds trust with users and integrators
- Identifies vulnerabilities before production

## Prerequisites

Before starting this epic:
- [ ] Epic 1 (Bidirectional Messaging) complete
- [ ] Epic 2 (Token Bridge) complete
- [ ] Epic 3 (Gas Payments) complete
- [ ] Epic 4 (Advanced Features) complete or deferred
- [ ] Epic 5 (Production Readiness) complete

## Tasks

| # | Task | Status | Depends On | Description |
|---|------|--------|------------|-------------|
| 6.1 | [Contract Audit](./task-6.1-contract-audit.md) | ⬜ | All Epics | Aiken smart contract audit |
| 6.2 | [Crypto Review](./task-6.2-crypto-review.md) | ⬜ | 6.1 | Cryptographic implementation review |
| 6.3 | [Off-chain Review](./task-6.3-offchain-review.md) | ⬜ | 6.1 | Rust off-chain code review |

## Audit Scope

### Aiken Contracts (Critical Priority)

| Contract | Lines | Risk Level |
|----------|-------|------------|
| `mailbox.ak` | 366 | Critical |
| `multisig_ism.ak` | 264 | Critical |
| `warp_route.ak` | 519 | High |
| `registry.ak` | 365 | Medium |
| `igp.ak` | 275 | Medium |
| `deferred_recipient.ak` | 336 | Medium |
| `vault.ak` | 286 | Medium |
| `merkle.ak` | 164 | High |

### Focus Areas

1. **Signature Verification** (`multisig_ism.ak`)
   - ECDSA secp256k1 implementation
   - Ethereum address derivation
   - Threshold enforcement
   - Replay protection

2. **Message Processing** (`mailbox.ak`)
   - Message ID calculation (keccak256)
   - Replay protection mechanism
   - ISM verification flow

3. **Token Handling** (`warp_route.ak`, `vault.ak`)
   - Collateral lock/unlock
   - Synthetic mint/burn authorization
   - Overflow/underflow checks

4. **Merkle Tree** (`merkle.ak`)
   - Insert operation correctness
   - Root calculation accuracy

### Off-chain Components

| Component | Location | Risk Level |
|-----------|----------|------------|
| Transaction Builder | `tx_builder.rs` | High |
| Blockfrost Provider | `blockfrost_provider.rs` | Medium |
| Mailbox Client | `mailbox.rs` | Medium |
| ISM Client | `multisig_ism.rs` | Medium |

## Audit Checklist

### Smart Contract Security

- [ ] Authentication: Only authorized parties can perform privileged operations
- [ ] Authorization: Access control on admin functions
- [ ] State Integrity: Nonce increments, merkle tree valid, UTXO continuation
- [ ] Replay Protection: Messages cannot be processed twice
- [ ] Economic Safety: No value extraction attacks

### Common Vulnerabilities

- [ ] Double Spend: UTXO model prevents by design, verify edge cases
- [ ] Integer Overflow: Check arithmetic, verify Aiken safety
- [ ] Denial of Service: Large input handling, script execution limits
- [ ] Front-Running: Sensitive ordering, MEV opportunities

### Cryptographic Verification

- [ ] keccak256 matches EVM implementation
- [ ] ECDSA verification matches Ethereum ecrecover
- [ ] Message encoding matches Hyperlane wire format
- [ ] Domain separator calculation correct

## Deliverables

1. **Audit Report**
   - Executive summary
   - Methodology
   - Findings by severity (Critical/High/Medium/Low/Info)
   - Recommendations

2. **Remediation Verification**
   - All critical/high findings fixed
   - Fixes verified by auditor

3. **Test Suite Additions**
   - Regression tests for findings
   - Edge case coverage

## Definition of Done

- [ ] All Aiken contracts audited by qualified auditor
- [ ] All cryptographic implementations verified
- [ ] Off-chain code reviewed
- [ ] No critical or high severity findings unresolved
- [ ] All medium findings documented with mitigation
- [ ] Audit report published
- [ ] Test suite expanded based on findings

## Timeline Considerations

- Audit engagement typically takes 2-4 weeks
- Allow time for remediation and re-review
- Schedule audit after feature freeze
- Do not deploy to mainnet until audit complete

## Acceptance Criteria

1. External auditor engaged and completed review
2. No unresolved critical or high severity findings
3. Medium findings have documented mitigation plan
4. Audit report publicly available
5. All fixes verified by auditor
6. Team confident in security posture

---

**Note:** This epic is the final gate before mainnet deployment. Do not proceed to mainnet until all tasks are complete and findings are resolved.

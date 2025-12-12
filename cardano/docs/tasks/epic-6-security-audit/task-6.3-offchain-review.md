[← Epic 6: Security Audit](./EPIC.md) | [Epics Overview](../README.md)

# Task 6.3: Off-Chain Code Review
**Status:** ⬜ Not Started
**Complexity:** Medium
**Depends On:** [Task 6.1](./task-6.1-contract-audit.md)

## Objective

Security review of Rust off-chain code handling Cardano transactions.

## Scope

| Component | Location | Risk Level |
|-----------|----------|------------|
| Transaction Builder | `tx_builder.rs` | High |
| Blockfrost Provider | `blockfrost_provider.rs` | Medium |
| Mailbox Client | `mailbox.rs` | Medium |
| ISM Client | `multisig_ism.rs` | Medium |
| Type Definitions | `types.rs` | Low |

## Review Areas

### Transaction Construction
- UTXO selection correctness
- Fee calculation accuracy
- Witness set construction
- Redeemer formatting
- Datum serialization

### Data Parsing
- CBOR datum parsing safety
- Error handling for malformed data
- Type coercion safety
- Bounds checking

### API Security
- Input validation on all public APIs
- Rate limiting handling
- Error message sanitization (no secrets leaked)
- Timeout handling

### Key Management
- Private keys not logged
- Key storage secure
- Key rotation supported
- No hardcoded credentials

### Error Handling
- All errors handled (no panics in production paths)
- Errors don't leak sensitive info
- Retries handled correctly
- Graceful degradation

## Review Checklist

### Memory Safety
- No buffer overflows in CBOR parsing
- Safe handling of external data
- Bounds checked array access

### Concurrency
- No data races
- Proper mutex usage if applicable
- Async cancellation safety

### Input Validation
- All external inputs validated
- Script hashes verified
- Amounts checked for overflow

## Definition of Done

- [ ] All high-risk components reviewed
- [ ] No security vulnerabilities found
- [ ] Error handling verified robust
- [ ] Documented findings

## Acceptance Criteria

1. No unhandled error paths
2. All inputs validated
3. No credential leaks possible
4. Robust error handling throughout

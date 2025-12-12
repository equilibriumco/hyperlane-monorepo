[← Epic 6: Security Audit](./EPIC.md) | [Epics Overview](../README.md)

# Task 6.2: Cryptographic Implementation Review
**Status:** ⬜ Not Started
**Complexity:** High
**Depends On:** [Task 6.1](./task-6.1-contract-audit.md)

## Objective

Verify all cryptographic implementations match their EVM reference implementations.

## Scope

### 1. keccak256 Implementation

**Location:** `cardano/contracts/lib/keccak.ak`

- Output matches EVM keccak256 for all inputs
- All input sizes handled correctly
- No edge case discrepancies

### 2. ECDSA Signature Verification

**Location:** `cardano/contracts/lib/ecdsa.ak`

- Matches Ethereum ecrecover behavior
- Public key recovery correct
- Signature malleability handled
- Invalid signatures properly rejected

### 3. Ethereum Address Derivation

**Location:** `cardano/contracts/lib/eth.ak`

- Address derived correctly from public key
- Matches EVM address(pubKey) output
- Checksum handling (if applicable)

### 4. Message Encoding

**Location:** Various contracts

- Message format matches Hyperlane wire format
- Encoding consistent with EVM implementation
- Decoding handles all valid formats

### 5. Domain Separator

**Location:** `cardano/contracts/lib/domain.ak`

- Calculation matches Hyperlane spec
- Chain ID handling correct

## Verification Approach

1. **Unit Tests:** Compare outputs with known values from EVM
2. **Cross-Chain Tests:** Same input on both chains, compare outputs
3. **Fuzz Testing:** Random inputs, compare outputs between implementations
4. **Edge Cases:** Empty input, max length, special byte patterns

## Test Vectors

Create comprehensive test vectors comparing:
- keccak256 outputs for various inputs
- ecrecover results for various signatures
- Message ID calculations
- Domain separator values

## Definition of Done

- [ ] All crypto operations verified against EVM reference
- [ ] Test vectors documented and automated
- [ ] No discrepancies found
- [ ] Fuzz tests pass

## Acceptance Criteria

1. 100% match with EVM reference for all test cases
2. Comprehensive test coverage
3. No edge case failures

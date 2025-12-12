[← Epic 3: Gas Payments](./EPIC.md) | [Epics Overview](../README.md)

# Task 3.4: IGP End-to-End Testing
**Status:** ⬜ Not Started
**Complexity:** Medium
**Depends On:** Tasks 3.1-3.3, 3.5, 3.6

## Objective

Comprehensive end-to-end testing of the complete IGP flow, including all new features (quote, refunds, hooks).

## Test Scenarios

### 1. Quote and Pay Flow

**Scenario:** User quotes gas, pays exact amount, message delivered

Steps:
1. Configure gas oracle for destination
2. Use `igp quote` to get required payment
3. Dispatch message from Cardano
4. Capture message ID
5. Pay for gas using `igp pay-for-gas` with quoted amount
6. Wait for relayer to process
7. Verify message delivered on destination

**Verification:**
- Quote amount matches actual required payment
- Gas payment indexed by relayer
- Message delivered successfully

### 2. Refund Flow

**Scenario:** User overpays, receives refund

Steps:
1. Get quote for gas payment
2. Dispatch message
3. Pay 2x the quoted amount with refund address
4. Verify refund output created
5. Verify correct refund amount (overpayment returned)
6. Verify message still delivered

**Verification:**
- Refund goes to specified address
- Refund amount is correct
- IGP retains only required payment

### 3. Post-Dispatch Hook Flow

**Scenario:** Atomic dispatch with gas payment

Steps:
1. Configure gas oracle
2. Dispatch with `--hook igp --gas-amount 200000`
3. Verify single transaction submitted
4. Verify message dispatched
5. Verify gas payment in IGP
6. Verify message delivered

**Verification:**
- Single transaction contains both dispatch and payment
- Both mailbox and IGP state updated
- Message ID in redeemer matches dispatched message

### 4. Unpaid Message (Enforcement Mode)

**Scenario:** Relayer enforces gas payment requirement

Steps:
1. Configure relayer with `allowUnpaid: false`
2. Dispatch message without payment
3. Wait for relayer poll cycle
4. Verify message NOT delivered
5. Pay for gas
6. Verify message now delivered

**Verification:**
- Unpaid messages not processed
- Paid messages processed
- Relayer logs show enforcement behavior

### 5. Fee Claiming

**Scenario:** Beneficiary claims accumulated fees

Steps:
1. Process several paid messages
2. Check accumulated fees using `igp show`
3. Record beneficiary balance
4. Claim fees using `igp claim`
5. Verify balance transfer to beneficiary
6. Verify IGP balance reduced

**Verification:**
- Claim amount matches requested
- Beneficiary receives funds
- Non-beneficiary cannot claim

### 6. Oracle Configuration

**Scenario:** Owner updates gas oracle

Steps:
1. Set initial oracle with `igp set-oracle`
2. Verify quote reflects oracle values
3. Update oracle with new values
4. Verify quote changes accordingly
5. Verify non-owner cannot update

**Verification:**
- Oracle updates work
- Quote reflects current oracle
- Access control enforced

### 7. Per-Destination Defaults

**Scenario:** Different defaults for different destinations

Steps:
1. Set default gas limit for domain A
2. Set different default for domain B
3. Pay with gas_amount=0 for domain A
4. Verify correct default used
5. Pay with gas_amount=0 for domain B
6. Verify different default used

**Verification:**
- Per-destination defaults apply
- Fallback default used when no specific default

### 8. Error Cases

**Scenario:** Graceful handling of error conditions

Test cases:
- Pay for non-existent message ID (should still work - IGP doesn't validate message existence)
- Pay for destination with no oracle (should fail with clear error)
- Claim more than available balance (should fail)
- Set oracle as non-owner (should fail)
- Hook dispatch with insufficient funds (should fail or fallback)

**Verification:**
- Clear error messages
- No partial state corruption
- Graceful fallback where appropriate

## Test Script

Create `cardano/scripts/test-igp-e2e.sh` that runs all scenarios:

```bash
#!/bin/bash
# IGP End-to-End Test Suite

# Setup
echo "Setting up IGP test environment..."
# Deploy IGP if needed
# Configure oracle
# Fund test accounts

# Test 1: Quote and Pay
echo "Test 1: Quote and Pay Flow"
# Run test, verify results

# Test 2: Refund
echo "Test 2: Refund Flow"
# Run test, verify refund

# Test 3: Post-Dispatch Hook
echo "Test 3: Hook Flow"
# Run test, verify atomic operation

# ... more tests

# Summary
echo "Test Results:"
# Report pass/fail for each test
```

## Test Environment

### Prerequisites
- Testnet deployment of all contracts (Mailbox, IGP, etc.)
- Funded test wallets
- Running relayer with Cardano support
- Destination chain (e.g., Fuji) accessible

### Test Data
- Known gas oracle values for consistent testing
- Pre-calculated expected payment amounts
- Test message payloads

## Definition of Done

- [ ] Quote flow tested end-to-end
- [ ] Refund flow tested
- [ ] Post-dispatch hook tested
- [ ] Enforcement mode tested
- [ ] Fee claiming tested
- [ ] Oracle configuration tested
- [ ] Per-destination defaults tested
- [ ] Error cases tested
- [ ] Test script automated and repeatable
- [ ] All tests pass on testnet

## Acceptance Criteria

1. All happy-path scenarios pass
2. Error cases handled gracefully
3. Tests are automated and repeatable
4. Test script runs in under 30 minutes
5. Clear pass/fail reporting
6. Documentation of test setup and teardown

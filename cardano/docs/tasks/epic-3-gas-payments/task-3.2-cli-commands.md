[← Epic 3: Gas Payments](./EPIC.md) | [Epics Overview](../README.md)

# Task 3.2: IGP CLI Commands
**Status:** ⬜ Not Started
**Complexity:** Medium
**Depends On:** None

## Objective

Implement CLI commands for IGP operations: quote, pay-for-gas, claim, set-oracle, show.

## Commands

### 1. Quote Gas Payment

```
hyperlane-cardano igp quote \
  --destination 43113 \
  --gas-amount 200000
```

Should:
- Fetch IGP state to get gas oracle for destination
- Calculate required ADA payment based on oracle
- Display the required payment in ADA and lovelace
- Show exchange rate and gas price used in calculation

Output example:
```
Gas Quote for destination 43113:
  Gas amount:     200,000 units
  Gas price:      25 gwei
  Exchange rate:  1,000,000 (ADA/ETH scaled)
  Required:       5.0 ADA (5,000,000 lovelace)
```

### 2. Pay for Gas

```
hyperlane-cardano igp pay-for-gas \
  --message-id 0x1234...abcd \
  --destination 43113 \
  --gas-amount 200000 \
  --refund-address addr1...  # Optional, defaults to sender
```

Should:
- Fetch IGP state to get gas oracle for destination
- Calculate required ADA payment based on oracle
- Build and submit transaction with PayForGas redeemer
- Include refund address for any overpayment
- Return transaction hash and payment amount

### 3. Claim Fees

```
hyperlane-cardano igp claim --amount 1000000
```

Should:
- Build transaction with Claim redeemer
- Caller must be the configured beneficiary
- Specify amount to claim in lovelace
- Submit and return transaction hash
- Display new IGP balance after claim

### 4. Set Gas Oracle

```
hyperlane-cardano igp set-oracle \
  --domain 43113 \
  --token-exchange-rate 1000000 \
  --gas-price 25000000000
```

Should:
- Build transaction with SetGasOracle redeemer
- Caller must be the owner
- Validate gas_price > 0 and token_exchange_rate > 0
- Submit and return transaction hash
- Display updated oracle configuration

### 5. Show IGP State

```
hyperlane-cardano igp show
```

Should display:
- IGP contract address and script hash
- Owner address
- Beneficiary address
- Default gas limit
- All configured gas oracles with:
  - Domain ID
  - Gas price
  - Token exchange rate
- Current balance (claimable fees)

Output example:
```
IGP State:
  Address:     addr1...
  Script Hash: 0x1234...
  Owner:       addr1q...
  Beneficiary: addr1q...
  Default Gas: 100,000 units
  Balance:     150.5 ADA (150,500,000 lovelace)

Gas Oracles:
  Domain 43113 (Avalanche Fuji):
    Gas Price:     25,000,000,000 wei
    Exchange Rate: 1,000,000
  Domain 11155111 (Sepolia):
    Gas Price:     30,000,000,000 wei
    Exchange Rate: 1,200,000
```

## Files to Create/Modify

| File | Changes |
|------|---------|
| `cardano/cli/src/commands/igp.rs` | New file with all commands |
| `cardano/cli/src/commands/mod.rs` | Export igp module |
| `cardano/cli/src/main.rs` | Wire up commands |

## Testing

- quote returns accurate payment estimate
- quote handles missing oracle gracefully
- pay-for-gas calculates correct payment
- pay-for-gas with refund address works
- claim transfers fees to beneficiary
- claim fails for non-beneficiary
- set-oracle updates state correctly
- set-oracle fails for non-owner
- show displays complete state

## Definition of Done

- [ ] All five CLI commands implemented
- [ ] Quote command returns accurate estimates
- [ ] Refund address supported in pay-for-gas
- [ ] Commands work on testnet
- [ ] Help text complete
- [ ] Error handling robust with clear messages

## Acceptance Criteria

1. All five commands work correctly
2. Quote matches actual required payment
3. Payment calculations match on-chain logic
4. Refund address handling works
5. Access control enforced (owner-only for set-oracle, beneficiary-only for claim)

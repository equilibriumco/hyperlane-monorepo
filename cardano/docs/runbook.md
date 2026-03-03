# Deployment & Test Runbook

Generated during execution of [preprod-test-plan.md](preprod-test-plan.md).

**Date:**
**Operator:**
**Network:** preview
**Wallet:** (address)

---

## Phase 1 — Deployment

### 1.1 deploy extract

```
# Command run:

# Output / deployed hashes:
mailbox_hash=
ism_hash=
igp_hash=
va_hash=
warp_route_hash=
processed_msg_nft_hash=
verified_msg_nft_hash=
canonical_config_nft_hash=
```

---

### 1.2 init all

```
# Command run:

# TX hashes:
mailbox_init_tx=
ism_init_tx=
igp_init_tx=
va_init_tx=

# Derived addresses & policies:
mailbox_address=
mailbox_script_hash=
mailbox_state_nft_policy=
ism_address=
ism_script_hash=
ism_state_nft_policy=
igp_address=
igp_script_hash=
igp_state_nft_policy=
processed_msg_nft_policy=
verified_msg_nft_policy=
va_hash=
```

---

### 1.3 deploy reference-scripts-all

```
# Command run:

# TX hash:
ref_scripts_tx=

# UTXOs:
mailbox_ref_utxo=
ism_ref_utxo=
```

---

### 1.4 igp set-oracle (Sepolia)

```
# Command run:

# TX hash:
igp_oracle_tx=

# Quote result:
igp_quote_11155111=  # lovelace
```

---

### 1.5 warp deploy

```
# Native ADA:
native_warp_tx=
native_nft_policy=
native_warp_address=
native_h256=  # 0x01000000 + nft_policy

# Collateral (TEST):
collateral_warp_tx=
collateral_nft_policy=
collateral_warp_address=
collateral_h256=

# Synthetic (wCTEST):
synthetic_warp_tx=
synthetic_nft_policy=
synthetic_warp_address=
synthetic_h256=
synthetic_minting_ref_tx=
```

---

### 1.6 warp enroll-router

```
# Native → Sepolia wADA collateral:
enroll_native_tx=

# Collateral → Sepolia FTEST:
enroll_collateral_tx=

# Synthetic → Sepolia wCTEST:
enroll_synthetic_tx=
```

---

### 1.7 Sepolia Contracts

```
# ISM (reused or new):
sepolia_ism=

# IGP:
sepolia_igp_deploy_tx=
sepolia_igp=
sepolia_storage_oracle=

# Oracle config TXs:
sepolia_oracle_config_tx=

# Warp route (re-enrolled or new):
sepolia_collateral_wada=
sepolia_collateral_ftest=
sepolia_synthetic_wctest=

# Router enrollment TXs:
enroll_sepolia_native_tx=
enroll_sepolia_collateral_tx=
enroll_sepolia_synthetic_tx=
```

---

### 1.8 Relayer Config

```
# .env updated: yes/no
# relayer-cardano-sepolia.json updated: yes/no
# gasPaymentEnforcement: set to minimum/gasFraction 1/1
# INDEX_FROM: (block height of mailbox init TX)
index_from_block=
```

---

### 1.9 Validator + Relayer Start

```
# Validator announce TX:
va_announce_tx=
va_storage_location=

# Relayer started at:
relayer_start_time=

# First log line showing indexing:
```

---

### 1.10 Greeting Deployment

```
# Command run:

# TX1 (fund init signal):
greeting_fund_tx=

# TX2 (canonical init):
greeting_init_tx=

# Results:
greeting_script_hash=
greeting_address=
greeting_state_nft_policy=
greeting_h256=  # 0x02000000 + script_hash
canonical_config_nft_utxo=
greeting_ref_script_utxo=
```

---

## Phase 2 — E2E Tests

### 2.1 Sepolia → Cardano: Native ADA

```
# Quote:
quote_wei=
quote_ada_equiv=

# Dispatch TX (Sepolia):
dispatch_tx=
message_id=
amount_wada_wei=

# Relayer delivery TX (Cardano):
delivery_tx=
delivery_fee_lovelace=
delivery_time_seconds=

# Process TX (warp, automatic):
process_tx=

# Cost analysis:
igp_paid_wei=
igp_paid_ada_equiv=
cardano_delivery_cost_lovelace=
relayer_profit_lovelace=
```

---

### 2.2 Sepolia → Cardano: Collateral TEST

```
# TX hashes: dispatch / delivery / process
dispatch_tx=
message_id=
delivery_tx=
process_tx=

# Cost analysis:
igp_paid_wei=
cardano_delivery_cost_lovelace=
relayer_profit_lovelace=
```

---

### 2.3 Sepolia → Cardano: Synthetic wCTEST

```
dispatch_tx=
message_id=
delivery_tx=
process_tx=
igp_paid_wei=
cardano_delivery_cost_lovelace=
relayer_profit_lovelace=
```

---

### 2.4 Cardano → Sepolia: Native ADA

```
# Quote:
igp_quote_lovelace=

# Dispatch TX:
dispatch_tx=
message_id=

# IGP pay TX:
igp_pay_tx=

# Relayer delivery TX (Sepolia):
delivery_tx=
delivery_gas_used=
delivery_gas_price=
delivery_cost_eth=

# Cost analysis:
igp_paid_lovelace=
sepolia_delivery_cost_lovelace_equiv=
relayer_profit_lovelace=
```

---

### 2.5 Cardano → Sepolia: Collateral TEST

```
dispatch_tx=
message_id=
igp_pay_tx=
delivery_tx=
igp_paid_lovelace=
sepolia_delivery_cost_lovelace_equiv=
relayer_profit_lovelace=
```

---

### 2.6 Cardano → Sepolia: Synthetic wCTEST

```
dispatch_tx=
message_id=
igp_pay_tx=
delivery_tx=
igp_paid_lovelace=
sepolia_delivery_cost_lovelace_equiv=
relayer_profit_lovelace=
```

---

### 2.7 Sepolia → Cardano: Greeting

```
dispatch_tx=
message_id=
igp_paid_wei=
delivery_tx=
delivery_fee_lovelace=
verified_msg_utxo_lovelace=
receive_tx=
greeting_count_after=
igp_paid_ada_equiv=
cardano_total_cost_lovelace=
relayer_profit_lovelace=
```

---

### 2.8 Cardano → Sepolia: Dispatch

```
dispatch_tx=
message_id=
igp_pay_tx=
igp_paid_lovelace=
delivery_tx=
delivery_gas_used=
delivered_on_sepolia=  # true/false
```

---

### 2.9 Per-Recipient ISM Override

```
# Second greeting deploy (custom ISM):
greeting2_script_hash=
greeting2_state_nft_policy=

# Test with wrong ISM (should fail):
test_fail_message_id=
relayer_rejection_log=

# Test with correct ISM (should succeed):
test_ok_message_id=
delivery_tx=
```

---

## Phase 3 — Stress Tests

### 3.1 Sepolia → Cardano: 100 Greeting Messages

```
# Multicall TX:
multicall_tx=
multicall_gas_used=
multicall_gas_cost_eth=
total_igp_paid_wei=

# Relayer processing:
start_time=
end_time=
total_duration_minutes=
avg_per_message_seconds=
messages_delivered=   # /100
messages_failed=

# Cost analysis:
total_cardano_delivery_cost_lovelace=
total_igp_received_lovelace=
net_relayer_profit_lovelace=

# Block range:
first_delivery_block=
last_delivery_block=
blocks_spanned=

# Greeting state after:
greeting_count=
```

---

### 3.2 Cardano → Sepolia: 100 Dispatches

```
# Dispatch period:
start_time=
end_time=
total_dispatch_duration_minutes=

# Delivery:
all_delivered=  # yes/no
messages_delivered=   # /100
messages_failed=
any_blockfrost_race_conditions=  # yes/no

# Cost:
total_igp_paid_lovelace=
total_sepolia_delivery_cost_eth=
```

---

## Phase 4 — Additional Tests

### 4.1 Relayer Restart Recovery

```
# Messages dispatched: 5
# Relayer killed after delivery 1 of 5
# Restart time:
# All 5 eventually delivered: yes/no
# Any duplicate deliveries: yes/no
```

---

### 4.2 Large Message Body

```
body_size_bytes=
dispatch_tx=
igp_paid_lovelace=
cardano_delivery_cost_lovelace=
verified_msg_utxo_lovelace=
delivery_success=  # yes/no
```

---

### 4.3 Zero-Body Message

```
dispatch_tx=
igp_paid_lovelace=
delivery_tx=
delivery_success=  # yes/no
```

---

## Summary

### Deployment Summary Table

| Contract | Address/Policy | Init TX |
|---|---|---|
| Mailbox | | |
| MultisigISM | | |
| IGP | | |
| ValidatorAnnounce | | |
| Native Warp Route | | |
| Collateral Warp Route | | |
| Synthetic Warp Route | | |
| Greeting | | |

### Cost Summary Table

| Test | Source paid | Source actual | Dest cost | IGP received | Net |
|---|---|---|---|---|---|
| Sep→ADA native | | | | | |
| Sep→ADA collateral | | | | | |
| Sep→ADA synthetic | | | | | |
| ADA→Sep native | | | | | |
| ADA→Sep collateral | | | | | |
| ADA→Sep synthetic | | | | | |
| Sep→ADA greeting | | | | | |
| ADA→Sep dispatch | | | | | |
| 100x greeting (avg) | | | | | |

### Release Decision

- [ ] All P0 issues resolved
- [ ] Phase 2 pass rate: ___/9
- [ ] Stress tests: Sep→ADA ___/100, ADA→Sep ___/100
- [ ] Relayer profitable on average: yes/no
- [ ] **Decision:** Proceed to preprod / Not yet (reasons: ...)

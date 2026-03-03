# Preprod Release Test Plan

**Network:** preview (validates readiness for preprod)
**Goal:** Full clean-room deployment + E2E validation of all features before promoting to preprod.

All commands use the `hyperlane-cardano` CLI. Record every TX hash in the [runbook](runbook.md).
Issues found → [FIXME.md](FIXME.md).

---

## Prerequisites

```bash
# Env setup - source before every session
source cardano/e2e-docker/.env

# Aliases used throughout (run all commands from monorepo root)
alias cli="cardano/cli/target/release/hyperlane-cardano \
  --network preview \
  --deployments-dir cardano/deployments \
  --contracts-dir cardano/contracts \
  --signing-key cardano/testnet-keys/payment.skey"

# Verify wallet has enough tADA (need ~200 ADA for full deployment)
# cli query utxo or check Blockfrost/explorer
```

**Keys and wallets:**

| Role | Key file | Address |
|---|---|---|
| Deployer / relayer | `cardano/testnet-keys/payment.skey` | `addr_test1vqfp9gpr8qqzp7x8h99cx8j90w0wvhcqnhuar4vggvxuezg4hvheh` |
| Recipient (test) | `cardano/testnet-keys/recipient/payment.skey` | `addr_test1vp4s2hc5ttr0syyj66x058wsh7e9wcq4vfq2l7fxr3qdqsqm5cdfx` |
| Cardano validator ECDSA | `CARDANO_VALIDATOR_KEY` (env) | `0x0A923108968Cf8427693679eeE7b98340Fe038ce` (derived) |
| Sepolia / EVM signer | `EVM_SIGNER_KEY` (env) | `0x1f26bfC6f52CbFad5c3fA8dABb71007b28bf4749` |

**Required before starting:**
- [ ] `aiken build` passes in `cardano/contracts/`
- [ ] Blockfrost preview API key set
- [ ] S3 bucket/folder ready for validator signatures
- [ ] Deployer wallet funded (~200 tADA)
- [ ] Sepolia wallet funded (~0.5 ETH)

---

## Phase 1 — Full Deployment from Scratch

### 0. Clean Slate

Remove all leftover deployment data so the CLI starts from zero:

```bash
rm -rf cardano/deployments/preview/

# The CLI will recreate the directory on first use.
# Verify it doesn't exist yet:
ls cardano/deployments/   # should show no preview/ subdirectory (or error if empty)
```

> **Important:** Back up or note the old addresses from the previous deployment before deleting, in case you need to reclaim any funds stuck in old contracts.

### 1.1 Extract Validators

```bash
cli deploy extract
```

**Verify:** `deployment_info.json` populated with unparameterized hashes for `mailbox`, `multisig_ism`, `igp`, `validator_announce`, `warp_route`, `processed_message_nft`, `verified_message_nft`, `canonical_config_nft`.

**Record in runbook:** mailbox hash, ISM hash, IGP hash.

---

### 1.2 Initialize Core Contracts (single command)

The **Cardano ISM** verifies Sepolia→Cardano messages using 3 official Hyperlane Sepolia validators (2-of-3):

```bash
cli init all \
  --domain 2003 \
  --origin-domains 11155111 \
  --validators "11155111:0xb22b65f202558adf86a8bb2847b76ae1036686a5,0x469f0940684d147defc44f3647146cb90dd0bc8e,0xd3c75dcf15056012a4d74c483a0c6ea11d8c2b83" \
  --thresholds "11155111:2" \
  --storage-location "s3://hyperlane-validator-signatures-cardanopreview/eu-north-1/<your-folder>" \
  --validator-key "$CARDANO_VALIDATOR_KEY"
```

**Verify:** `init status` shows all contracts initialized.

**Record in runbook:** mailbox address, ISM address, IGP address, VA address, all policy IDs.

---

### 1.3 Deploy Core Reference Scripts

```bash
cli deploy reference-scripts-all
```

**Record in runbook:** mailbox ref UTXO, ISM ref UTXO.

**Check:** `mailbox show` reports correct domain, ISM, ref UTXO.

---

### 1.4 Configure IGP Gas Oracles

```bash
# Cardano → Sepolia (domain 11155111)
cli igp set-oracle \
  --domain 11155111 \
  --gas-price 1000000000 \
  --exchange-rate 7171 \
  --gas-overhead 150000

# Verify quote
cli igp quote --domain 11155111
```

**Record in runbook:** quote output (lovelace), TX hash.

---

### 1.5 Deploy Warp Routes

```bash
# A) Native ADA warp route (Sepolia synthetic wADA)
cli warp deploy --token-type native --remote-decimals 18

# B) Collateral (TEST token, 6 decimals)
cli warp deploy \
  --token-type collateral \
  --token-policy $CARDANO_COLLATERAL_TOKEN_POLICY \
  --token-asset "" \
  --decimals 6 \
  --remote-decimals 18

# C) Synthetic (wCTEST, 6 decimals)
cli warp deploy \
  --token-type synthetic \
  --decimals 6 \
  --remote-decimals 18

# Deploy synthetic minting ref script (required for relayer)
cli warp deploy-minting-ref --warp-policy <synthetic_nft_policy>
```

**Record in runbook:** all 3 NFT policy IDs, all 3 warp addresses.

---

### 1.6 Enroll Remote Routers (Cardano side)

```bash
SEPOLIA_DOMAIN=11155111

cli warp enroll-router \
  --warp-policy <native_nft_policy> \
  --domain $SEPOLIA_DOMAIN \
  --router <0x000...SEPOLIA_WADA_COLLATERAL_32bytes>

cli warp enroll-router \
  --warp-policy <collateral_nft_policy> \
  --domain $SEPOLIA_DOMAIN \
  --router <0x000...SEPOLIA_FTEST_COLLATERAL_32bytes>

cli warp enroll-router \
  --warp-policy <synthetic_nft_policy> \
  --domain $SEPOLIA_DOMAIN \
  --router <0x000...SEPOLIA_WCTEST_SYNTHETIC_32bytes>
```

---

### 1.7 Deploy Sepolia Contracts

> These require forge scripts (`solidity/script/warp-e2e/`) because they are EVM contracts. Document all TX hashes in the runbook.

The **Sepolia ISM** verifies Cardano→Sepolia messages. It must be a `StaticMerkleRootMultisigIsm`
(same type as current deployment) configured with **our own Cardano validator** (1-of-1):

- Validator address: `0x0A923108968Cf8427693679eeE7b98340Fe038ce` (derived from `CARDANO_VALIDATOR_KEY`)
- Threshold: 1

```bash
cd solidity

# Deploy Sepolia ISM (StaticMerkleRootMultisigIsm, 1-of-1)
# Uses DeployCardanoISM.s.sol — sets CARDANO_VALIDATOR env to our validator address
CARDANO_VALIDATOR=0x0A923108968Cf8427693679eeE7b98340Fe038ce \
forge script script/warp-e2e/DeployCardanoISM.s.sol --broadcast \
  --rpc-url $SEPOLIA_RPC_URL --private-key $EVM_SIGNER_KEY

# Deploy IGP + StorageGasOracle
forge script script/warp-e2e/DeployCardanoIGP.s.sol --broadcast \
  --rpc-url $SEPOLIA_RPC_URL --private-key $EVM_SIGNER_KEY

# Configure Sepolia IGP gas oracle for Cardano (domain 2003)
# gasPrice=44 (lovelace/byte), tokenExchangeRate recalibrate at test time
cast send $SEPOLIA_STORAGE_GAS_ORACLE \
  "setRemoteGasDataConfigs((uint32,uint128,uint128)[])" \
  "[(2003,<gasPrice>,<tokenExchangeRate>)]" \
  --rpc-url $SEPOLIA_RPC_URL --private-key $EVM_SIGNER_KEY

# Deploy Sepolia warp routes (or reuse existing if Cardano router addresses unchanged)
# wADA collateral, FTEST collateral, wCTEST synthetic
forge script script/warp-e2e/DeployCardanoWarpRoutes.s.sol --broadcast \
  --rpc-url $SEPOLIA_RPC_URL --private-key $EVM_SIGNER_KEY

# Enroll Cardano routers on Sepolia side (addresses from step 1.5)
cast send $SEPOLIA_COLLATERAL_WADA \
  "enrollRemoteRouter(uint32,bytes32)" 2003 <cardano_native_h256> \
  --rpc-url $SEPOLIA_RPC_URL --private-key $EVM_SIGNER_KEY
# ... repeat for collateral and synthetic routes
```

**Record in runbook:** all Sepolia contract addresses, TX hashes.

---

### 1.8 Update Relayer Config

Edit `cardano/e2e-docker/config/relayer-cardano-sepolia.json`:
- New Cardano contract addresses (mailbox, ISM, IGP, all warp routes)
- New ref script UTXOs
- `gasPaymentEnforcement: [{ type: "minimum", payment: { gasLimit: "1", gasFraction: "1/1" } }]`
- `INDEX_FROM`: block height of mailbox deploy TX

Update `.env` with new addresses.

**Verify config:** `cli config generate` (or validate manually).

---

### 1.9 Start Validator + Relayer

```bash
# Validator (Docker or local)
docker compose up -d validator

# Relayer (local for easier debugging)
./cardano/e2e-docker/run-relayer-local.sh
```

**Check:**
- [ ] Validator announces on-chain (check `cli validator show`)
- [ ] Relayer indexes from correct block
- [ ] No startup errors in logs

---

### 1.10 Deploy Greeting Contract

```bash
cli init recipient \
  --custom-contracts cardano/contracts \
  --custom-module greeting \
  --custom-validator greeting \
  --datum-cbor d879824000
```

**Record in runbook:** greeting script hash, greeting address, state NFT policy.
**Update .env** with new greeting values.

---

### Phase 1 Checklist

| Contract | Initialized | Ref Script | Router Enrolled |
|---|---|---|---|
| Mailbox | | | N/A |
| MultisigISM | | N/A | N/A |
| IGP | | N/A | N/A |
| ValidatorAnnounce | N/A | N/A | N/A |
| Native warp route | | N/A | |
| Collateral warp route | | N/A | |
| Synthetic warp route | | N/A | |
| Greeting | | (per recipient) | N/A |

---

## Phase 2 — E2E Tests with Gas Enforcement

**Setup:** Relayer running with `gasFraction: 1/1`. All gas paid via `quoteDispatch`.

### Cost Tracking Template

Fill in for each message:

| Test | Direction | Source cost paid | Source actual gas | Dest delivery cost | IGP payment | Relayer profit/loss | TX hashes |
|---|---|---|---|---|---|---|---|
| ... | | ADA/ETH | | ADA/ETH | | | |

---

### 2.1 Sepolia → Cardano: Native ADA Release

**Setup:** Send wADA from Sepolia to Cardano native warp route.

```bash
# 1. Quote
QUOTE=$(cast call $SEPOLIA_WADA_COLLATERAL "quoteGasPayment(uint32,uint256)" 2003 69000)

# 2. Transfer + IGP payment (or use 5-arg dispatch with AggregationHook)
cast send $SEPOLIA_COLLATERAL_WADA \
  "transferRemote(uint32,bytes32,uint256)" \
  2003 $CARDANO_NATIVE_ROUTE_H256 <amount_wei> \
  --value $QUOTE --rpc-url $SEPOLIA_RPC_URL --private-key $EVM_SIGNER_KEY
```

**Expected:** Relayer delivers → native ADA released at Cardano address.

**Track:**
- Quote from `quoteGasPayment` vs actual Sepolia gas used
- Cardano delivery TX fee (from Blockfrost)
- Any remaining IGP balance

---

### 2.2 Sepolia → Cardano: Collateral Unlock

```bash
cast send $SEPOLIA_FTEST_COLLATERAL \
  "transferRemote(uint32,bytes32,uint256)" \
  2003 $CARDANO_COLLATERAL_ROUTE_H256 <amount_wei> \
  --value $QUOTE ...
```

**Expected:** Relayer delivers → TEST tokens unlocked at Cardano address.

---

### 2.3 Sepolia → Cardano: Synthetic Mint

```bash
cast send $SEPOLIA_WCTEST_SYNTHETIC \
  "transferRemote(uint32,bytes32,uint256)" \
  2003 $CARDANO_SYNTHETIC_ROUTE_H256 <amount_wei> \
  --value $QUOTE ...
```

**Expected:** Relayer delivers → wCTEST minted at Cardano address.

---

### 2.4 Cardano → Sepolia: Native ADA Lock

```bash
# Quote gas first
cli igp quote --domain 11155111

# Transfer
cli warp transfer \
  --warp-policy <native_nft_policy> \
  --destination 11155111 \
  --recipient <0x...evm_wallet_h256> \
  --amount <lovelace>

# Pay IGP
cli igp pay-for-gas \
  --message-id <msg_id> \
  --destination 11155111 \
  --gas-amount <quoted_amount>
```

**Expected:** wADA minted on Sepolia.

---

### 2.5 Cardano → Sepolia: Collateral Unlock

```bash
cli warp transfer \
  --warp-policy <collateral_nft_policy> \
  --destination 11155111 \
  --recipient <0x...h256> \
  --amount <tokens>
cli igp pay-for-gas ...
```

**Expected:** FTEST unlocked on Sepolia.

---

### 2.6 Cardano → Sepolia: Synthetic Burn

```bash
cli warp transfer \
  --warp-policy <synthetic_nft_policy> \
  --destination 11155111 \
  --recipient <0x...h256> \
  --amount <tokens>
cli igp pay-for-gas ...
```

**Expected:** wCTEST minted on Sepolia.

---

### 2.7 Sepolia → Cardano: Greeting (Single Message)

```bash
# Dispatch from Sepolia with gas
# RECIPIENT = 0x02000000 + greeting_script_hash (from step 1.10 runbook)
RECIPIENT=0x02000000<greeting_script_hash>
BODY=$(cast --to-hex "Hello preprod test" --no-0x 2>/dev/null || printf '%s' "Hello preprod test" | xxd -p)

QUOTE=$(cast call $SEPOLIA_MAILBOX "quoteDispatch(uint32,bytes32,bytes)" 2003 $RECIPIENT 0x$BODY)
cast send $SEPOLIA_MAILBOX \
  "dispatch(uint32,bytes32,bytes)" \
  2003 $RECIPIENT 0x$BODY \
  --value $QUOTE ...

# Receive (after relayer delivers)
cli greeting list
cli greeting receive
```

**Expected:** Greeting updated: `"Hello, Hello preprod test"`, count = 1.

---

### 2.8 Cardano → Sepolia: Dispatch Test

```bash
cli mailbox dispatch \
  --destination 11155111 \
  --recipient <SEPOLIA_LIGHT_TEST_RECIPIENT_H256> \
  --body "Hello from Cardano"

cli igp pay-for-gas \
  --message-id <msg_id> \
  --destination 11155111 \
  --gas-amount <quoted>
```

**Expected:** `delivered()` returns true on Sepolia TestRecipient.

---

### 2.9 Per-Recipient ISM Override (Canonical Config NFT)

Test that the security feature works end-to-end.

```bash
# 1. Deploy a second greeting recipient with a custom ISM
cli init recipient \
  --custom-contracts cardano/contracts \
  --custom-module greeting \
  --custom-validator greeting \
  --custom-ism <alternative_ism_script_hash> \
  --datum-cbor d879824000

# 2. Dispatch to the new recipient from Sepolia with standard validator
#    Should fail (validator not in custom ISM) → relayer cannot deliver

# 3. Dispatch again when custom ISM has correct validator
#    Should succeed
```

---

### Phase 2 Checklist

| Test | Pass | IGP covered delivery cost | Notes |
|---|---|---|---|
| Sep→ADA native | | | |
| Sep→ADA collateral | | | |
| Sep→ADA synthetic | | | |
| ADA→Sep native | | | |
| ADA→Sep collateral | | | |
| ADA→Sep synthetic | | | |
| Sep→ADA greeting | | | |
| ADA→Sep dispatch | | | |
| Custom ISM override | | | |

---

## Phase 3 — Stress Tests

### 3.1 Sepolia → Cardano: 100 Greeting Messages via Multicall

```bash
# Use the existing dispatch-multicall.sh or cast multicall
# Body: "Test message N" for N in 1..100

# Pre-calculate total gas needed
SINGLE_QUOTE=$(cast call $SEPOLIA_MAILBOX "quoteDispatch(...)")
TOTAL_ETH=$(python3 -c "print($SINGLE_QUOTE * 100 / 1e18, 'ETH')")

# Dispatch all 100 in a single multicall TX (saves gas vs 100 individual TXs)
./cardano/e2e-docker/dispatch-multicall.sh \
  --count 100 \
  --recipient $CARDANO_GREETING_H256 \
  --body-prefix "Stress test "
```

**Track:**
- Time from dispatch to all 100 delivered (minutes)
- Average processing time per message
- Any dropped/stuck messages
- Total Cardano fees paid by relayer across 100 deliveries
- Total IGP received by relayer
- Net profit/loss

**Expected:** All 100 delivered within ~15 minutes (batching at ~10 messages/block).

---

### 3.2 Cardano → Sepolia: 100 Dispatches

> Cardano has no multicall — messages must be sent one per TX (one per block ~20s).
> This test validates throughput at the Cardano mailbox dispatch level.

```bash
# Script: dispatch 100 messages sequentially with 1 block gap
for i in $(seq 1 100); do
  MSG_ID=$(cli mailbox dispatch \
    --destination 11155111 \
    --recipient <SEPOLIA_LIGHT_TEST_RECIPIENT_H256> \
    --body "Cardano stress $i" | grep "Message ID:" | awk '{print $3}')

  cli igp pay-for-gas \
    --message-id "$MSG_ID" \
    --destination 11155111 \
    --gas-amount <quoted>

  echo "$i: $MSG_ID"
  sleep 20  # one block
done
```

**Track:**
- Total dispatch time
- All 100 delivered on Sepolia
- Any Blockfrost race conditions (missed messages)

---

### 3.3 Parallel Inbound Processing Validation

After the 100-message stress test, confirm the Cardano state is consistent:

```bash
cli greeting show        # should show count = (Phase 2 count) + 100
cli mailbox show         # nonce counter
```

**Check:** No UTXOs stuck at greeting address without corresponding receive TX.

---

## Phase 4 — Additional Tests

### 4.1 Relayer Restart Recovery

1. Dispatch 5 messages from Sepolia
2. Kill relayer after first delivery
3. Restart relayer with same `--db` path
4. Verify remaining 4 messages delivered without re-delivery of first

**Validates:** DB persistence and deduplication.

---

### 4.2 Large Message Bodies

```bash
# Max body: ~5000 bytes (TX size limits)
LARGE_BODY=$(python3 -c "print('A' * 4950)")
cli mailbox dispatch \
  --destination 11155111 \
  --recipient <SEPOLIA_LIGHT_TEST_RECIPIENT_H256> \
  --body "$LARGE_BODY"
```

**Track:** TX fee vs body size. Verify IGP payment sufficient.

---

### 4.3 Zero-Body Message

```bash
cli mailbox dispatch \
  --destination 11155111 \
  --recipient <SEPOLIA_LIGHT_TEST_RECIPIENT_H256> \
  --body ""
```

---

### 4.4 Blockfrost Rate Limiting Under Load

During the 100-message stress test, observe relayer logs for:
- 429 rate limit errors
- Retry behavior
- Any permanent message failures due to rate limiting

---

### 4.5 Validator Announcement Re-announce

```bash
# Test re-announcing with same key (should be idempotent)
cli validator announce \
  --storage-location "s3://..." \
  --validator-key "$CARDANO_VALIDATOR_KEY"
```

**Expected:** CLI handles "already announced" gracefully.

---

### 4.6 IGP Claim

After delivering messages, relayer has accumulated IGP fees:

```bash
cli igp claim
```

**Verify:** ADA returned to beneficiary address.

---

### 4.7 Warp Route: Round Trip

Send native ADA Cardano→Sepolia→Cardano:
1. `warp transfer` native → Sepolia (mints wADA)
2. `cast send` wADA Sepolia → Cardano (releases ADA)

**Verify:** ADA balance roughly preserved (minus fees).

---

## Phase 5 — UX/DX Audit

While executing the above, note friction points for the FIXME.md:

### Deployment UX Checklist

- [ ] `init all` single command works without manual steps between contracts
- [ ] Error messages are actionable (not raw hex/CBOR dumps)
- [ ] Deployment_info.json auto-updated at each step (no manual editing needed)
- [ ] Reference script deployment is clearly linked to init (dependency explicit)
- [ ] `init status` shows what's missing at a glance
- [ ] `mailbox show` / `ism show` / `igp show` give complete picture

### Relayer Config UX Checklist

- [ ] Config can be auto-generated from deployment_info.json (`config generate` command)
- [ ] INDEX_FROM calculated automatically from mailbox init TX block
- [ ] No manual copy-paste of addresses from deployment_info to .env

### Operational UX Checklist

- [ ] `greeting list` / `greeting show` work without extra flags
- [ ] `igp quote` returns human-readable output (not just raw lovelace)
- [ ] All CLI commands produce consistent output format
- [ ] Dry-run mode (`--dry-run`) works on all write commands

---

## Phase 6 — Release Readiness Checklist

| Category | Pass | Notes |
|---|---|---|
| Clean deployment from scratch | | |
| All 6 E2E warp transfer tests pass | | |
| Greeting E2E (both directions) | | |
| 100-msg Sepolia→Cardano stress test | | |
| 100-msg Cardano→Sepolia stress test | | |
| Relayer restart recovery | | |
| IGP enforcement (gasFraction 1/1) | | |
| Per-recipient ISM override | | |
| Relayer profitable (net IGP > delivery cost) | | |
| No Blockfrost race conditions under load | | |
| All FIXME issues triaged (P0 fixed, P1 documented) | | |

**Release Decision:** Proceed to preprod only if all P0 issues resolved and at least 90% of checklist passes.

---

## Appendix: Cost Tracking Formulas

**Cardano delivery cost per message:**
```
fee = tx_fee + lovelace_locked_in_processed_marker(1.5 ADA) + lovelace_locked_in_verified_msg_nft
verified_msg_utxo ≈ 3.9 ADA (5B body) to 23.9 ADA (5000B body)
total_delivery_cost ≈ fee + 1.5 ADA + verified_msg_utxo
```

**Relayer profitability (Sepolia→Cardano):**
```
igp_received(lovelace) = igp_payment_wei * exchange_rate / 1e12
net = igp_received - total_delivery_cost
```

**Sepolia→Cardano gas quote accuracy:**
```
quoted_lovelace vs actual_delivery_cost
over/under-estimation %
```

**Cardano→Sepolia profitability:**
```
igp_paid(lovelace) = quoted_lovelace (from igp quote)
sepolia_delivery_cost_eth = eth_estimateGas * gasPrice
net_for_relayer_in_lovelace = igp_paid - (sepolia_delivery_cost_eth * exchange_rate)
```

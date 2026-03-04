# Cardano Integration — Fix Implementation Report

Date: 2026-03-04
Branch: `feat/cardano-per-recipient-ism`
Base: `72cf9dcdc` (docs: add preprod release test plan and runbook template)

---

## Summary

14 issues from `issues-and-fix-plan.md` addressed across 11 commits.
One review finding (Codex) required a correction to B3's cache logic.

### Bug Fixes

| ID  | Severity | Status | Commit |
|-----|----------|--------|--------|
| B1  | High     | Fixed  | `a0aa17b23` |
| B3  | High     | Fixed (corrected after Codex review) | `1067fe276` + `cdd35e6ac` |
| B4  | Medium   | Fixed  | `1067fe276` |
| B5  | Medium   | Fixed  | `a0aa17b23` |
| B6  | Medium   | Fixed  | `e818b92e7` |
| B7  | Low      | Fixed  | `2b346237d` |
| C1  | High     | Fixed  | `cadb5100e` |

### Quality & Ops Fixes

| ID  | Priority | Status | Commit |
|-----|----------|--------|--------|
| Q1  | High     | Fixed  | `1924449a4` |
| Q2  | Medium   | Fixed  | `1924449a4` |
| Q4  | Low      | Fixed  | `0723e8d9a` |
| Q6  | Low      | Fixed  | `8a7217d3c` |
| O1  | Medium   | Fixed  | `c3c6527f5` |
| O2  | Low      | Fixed  | `0ceb58343` |

---

## Bug Fix Details

### B1 — Delivered-message indexer uses wrong address (HIGH)

**File:** `rust/main/chains/hyperlane-cardano/src/mailbox_indexer.rs:498`

**Change:** `self.conf.mailbox_policy_id` → `self.conf.processed_messages_script_hash`

**Why:** The indexer queried the mailbox script address instead of the processed-message
NFT script address, so delivered messages were never found.

---

### B3 — Relayer stale TX cache not rebuilt on BadInputsUTxO (HIGH)

**File:** `rust/main/chains/hyperlane-cardano/src/tx_builder.rs`

**Changes:**
1. Reduced `SPENT_UTXO_CACHE_TTL_SECS` from 120 → 60 seconds
2. Added `mark_bad_inputs_as_spent()` method that inserts failed TX inputs into the
   `recently_spent` cache so Blockfrost's stale index doesn't cause reselection
3. Called at the single-message submit error path when `is_retryable_bad_inputs_error`

**Codex finding (P1):** Original implementation evicted inputs from cache — inverted.
When `BadInputsUTxO` fires, inputs are confirmed spent on-chain but Blockfrost may still
show them as available (25-40s lag). Corrected to insert instead of remove.

---

### B4 — U256::as_u64() silent truncation in amount conversion (MEDIUM)

**File:** `rust/main/chains/hyperlane-cardano/src/tx_builder.rs`

**Changes:**
1. `convert_wire_to_local_amount` return type: `u64` → `Result<u64, TxBuilderError>`
2. Bounds check: `if result > U256::from(u64::MAX)` → error
3. Updated 2 call sites to propagate with `?`
4. Added `test_convert_wire_overflow_returns_error` test

---

### B5 — get_balance masks RPC errors with zero (MEDIUM)

**File:** `rust/main/chains/hyperlane-cardano/src/provider.rs:104-112`

**Change:** Replaced `Ok(U256::zero())` error fallback with `.map_err(to_chain_err)?`.

---

### B6 — Blockfrost cursor race condition (MEDIUM)

**Files:** `mailbox_indexer.rs`, `trait_builder.rs`

**Changes:**
1. Added `BACKFILL_WINDOW = 5` — on each indexer tick, re-scan 5 blocks behind the cursor
   to catch TXs whose Blockfrost address-transaction index lagged behind
2. Fixed stale doc comment: `confirmation_block_delay` default was already 5, not 2

---

### B7 — Chain metrics return placeholder zeros (LOW)

**Files:** `provider.rs`, `blockfrost_provider.rs`

**Changes:**
1. `get_block_by_height` and `get_chain_metrics` now return real data from Blockfrost
2. Added `get_latest_block_info()` to `BlockfrostProvider`
3. Added `block_hash_to_h256()` with length validation

---

### C1 — ChainSigner returns placeholder address for Cardano Keypair (HIGH)

**File:** `rust/main/hyperlane-base/src/settings/signers.rs:343-351`

**Change:** Delegated to existing `self.address_bech32_testnet()` and `self.address_h256()`.

**Note:** `address_bech32_testnet()` hardcodes testnet. Needs network-aware variant for
mainnet — low priority.

---

## Quality & Ops Fix Details

### Q1 — Raw Plutus constructor tags → typed enums (HIGH)

**New file:** `rust/main/chains/hyperlane-cardano/src/redeemers.rs`

Created typed enums: `MailboxRedeemerTag`, `MultisigIsmRedeemerTag`, `WarpRouteRedeemerTag`
with `plutus_tag()` method. Generic `plutus_constr_tag(index)` for datum wrappers.
22 raw tag literals replaced in `tx_builder.rs`.

---

### Q2 — Magic numbers → consts module (MEDIUM)

**New file:** `rust/main/chains/hyperlane-cardano/src/consts.rs`

8 constants: `POLICY_ID_ADDR_PREFIX`, `SCRIPT_HASH_ADDR_PREFIX`, `MULTISIG_ISM_METADATA_MIN_LEN`,
`ECDSA_SIG_LEN`, `MESSAGE_ID_SIZE`, `SCRIPT_HASH_SIZE`, `CARDANO_SCRIPT_ADDR_TESTNET/MAINNET`.
~20 replacements across 4 files.

---

### Q4 — Address/script conversion duplication → consolidated (LOW)

Consolidated into `types.rs`:
- `script_hash_bytes_to_address` (was duplicated in 3 files)
- `extract_script_hash_from_address` (moved from `recipient_resolver.rs`)
- `extract_cardano_credential_from_bytes32` (moved from `tx_builder.rs`)

---

### Q6 — Duplicate domain ID mapping → centralized (LOW)

`mailbox_indexer.rs:get_local_domain()` now calls `self.conf.network.domain_id()` instead
of repeating the match. Domain ID method already existed on `CardanoNetwork`.

---

### O1 — Parallel CLI UTXO conflicts → file lock (MEDIUM)

**New file:** `cardano/cli/src/utils/wallet_lock.rs`

POSIX advisory lock (`flock(LOCK_EX)`) keyed on wallet address prefix. Acquired in
`main()` before command dispatch, released on drop. Added `libc` as explicit dependency.

---

### O2 — `init all` silently skips IGP → warning message (LOW)

Added warning at end of `init_all`: "Note: IGP not initialized by 'init all'. Run 'init
igp' separately."

---

## Remaining Issues

| ID  | Title | Status |
|-----|-------|--------|
| B2  | INDEX_FROM too high | Operational/config — not code |
| B8  | WADA scale=1e12 | Solidity redeploy — separate process |
| C2  | Testnet keys in git | Skipped per user decision |
| S2  | Replay protection depth-in-defense | Post-preprod, Aiken contract change |
| Q3  | unwrap/expect in production paths | Deferred — large effort (L) |
| Q5  | tx_builder.rs monolith split | Deferred — large effort (L), conflicts with other fixes |
| O3  | .env / deployment_info sync | New CLI command needed — separate effort |
| O4  | onChainFeeQuoting incompatibility | Config/docs fix |

---

## Verification

- `cargo check -p hyperlane-cardano -p hyperlane-base` — **clean**
- `cargo test -p hyperlane-cardano` — **71/71 passed**
- Codex review (Wave 1): 1 finding (B3 cache logic inversion) — **corrected**
- Manual review: 1 hardening (B7 hash length check) — **applied**
- Codex review (Wave 2+3): pending

---

## Commit Log

```
1924449a4 refactor(cardano): extract magic numbers to consts module
0723e8d9a refactor(cardano): consolidate address conversion helpers
c3c6527f5 fix(cardano): add file lock to prevent parallel CLI UTXO conflicts
e818b92e7 fix(cardano): mitigate Blockfrost index lag with backfill window
8a7217d3c refactor(cardano): centralize domain ID mapping
0ceb58343 fix(cardano): warn about IGP skip in init all command
cdd35e6ac fix(cardano): correct BadInputsUTxO cache logic and harden hash parsing
2b346237d fix(cardano): return real block hash and timestamp in chain metrics
1067fe276 fix(cardano): add overflow check in wire-to-local amount conversion
cadb5100e fix(cardano): derive real address in ChainSigner for Keypair
a0aa17b23 fix(cardano): use correct script hash for processed message indexer
```

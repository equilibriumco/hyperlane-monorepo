# Cardano Integration — Issues & Fix Plan

Sources: `FIXME.md`, `review-findings-2026-02-20.md`, multi-agent code analysis (2026-03-03).

---

## Priority Matrix

| ID  | Title                                            | Severity | Category  | Status |
|-----|--------------------------------------------------|----------|-----------|--------|
| S1  | ~~Multisig duplicate-signature bypass~~           | ~~Critical~~ | Security | ✅ Not vulnerable |
| S2  | ~~Replay protection on-chain (depth-in-defense)~~ | ~~Medium~~ | Security | ✅ `92c76b714` |
| B1  | ~~Delivered-message indexer uses wrong address~~  | ~~High~~ | Bug       | ✅ `a0aa17b23` |
| B2  | ~~`CARDANO_INDEX_FROM` too high → relayer stuck~~ | ~~High~~ | Bug       | ✅ `d39feaffa` + `63b3e2168` |
| B3  | ~~Relayer stale TX cache on `BadInputsUTxO`~~    | ~~High~~ | Bug       | ✅ `cdd35e6ac` |
| B4  | ~~`U256::as_u64()` silent truncation in amounts~~ | ~~Medium~~ | Bug      | ✅ `1067fe276` |
| B5  | ~~`get_balance` masks RPC errors with zero~~      | ~~Medium~~ | Bug      | ✅ `e890001f3` |
| B6  | ~~Blockfrost cursor race (address-TX index lag)~~ | ~~Medium~~ | Bug      | ✅ `e818b92e7` |
| B7  | ~~Chain metrics return placeholder hash/timestamp~~ | ~~Low~~ | Bug      | ✅ `2b346237d` |
| B8  | ~~`WADA` collateral deployed with `scale=1e12`~~ | ~~P0~~   | Config    | ✅ `3558fc5e2` |
| Q1  | ~~Raw Plutus constructor tags (121/122/123)~~     | ~~High~~ | Quality   | ✅ `1924449a4` |
| Q2  | ~~Magic numbers across Rust crate~~               | ~~Medium~~ | Quality  | ✅ `1924449a4` |
| Q3  | ~~`unwrap`/`expect` in production paths~~         | ~~Medium~~ | Quality  | ✅ `e890001f3` |
| Q4  | ~~Address/script conversion duplication~~         | ~~Low~~  | Quality   | ✅ `0723e8d9a` |
| Q5  | ~~`tx_builder.rs` monolith (multiple concerns)~~  | ~~Low~~  | Quality   | ✅ `de18465de` |
| Q6  | ~~Duplicate domain ID mapping~~                   | ~~Low~~  | Quality   | ✅ `8a7217d3c` |
| O1  | ~~Parallel CLI collateral UTXO conflict~~         | ~~Medium~~ | Ops      | ✅ `c3c6527f5` |
| O2  | ~~`init all` silently skips IGP~~                 | ~~Low~~  | UX        | ✅ `0ceb58343` |
| O3  | ~~`.env` / `deployment_info.json` sync gap~~      | ~~Low~~  | UX        | ✅ Already had `GenerateEnv` |
| O4  | ~~`onChainFeeQuoting` incompatible w/ official IGP~~ | ~~Low~~ | Config  | ✅ `391519687` |
| C1  | ~~`ChainSigner` returns zero/placeholder address~~ | ~~High~~ | Codex    | ✅ `cadb5100e` |
| C2  | Testnet signing keys committed to git            | Medium   | Codex     | ⏭ Skipped (user decision) |

> Effort: XS = minutes, S = hours, M = 1-2 days, L = 3+ days.

---

## Security Findings

### S1 — Multisig threshold bypass via duplicate signatures ✅ RESOLVED

**Finding:** The original report flagged `count_valid_signatures` in
`cardano/contracts/validators/multisig_ism.ak:107` for not tracking seen addresses.

**Code audit result:** NOT VULNERABLE. The implementation already uses a `seen` accumulator:

```aiken
// multisig_ism.ak
fn count_valid_signatures(...) -> Int {
  let (valid_count, _seen) =
    list.foldl(validator_signatures, (0, []), fn(val_sig, acc) {
      let (count, seen) = acc
      ...
      if sig_valid {
        let address = pubkey_to_eth_address(uncompressed_pubkey)
        if list.has(validator_addresses, address) && !list.has(seen, address) {
          (count + 1, [address, ..seen])  // dedup enforced here
        } else {
          (count, seen)
        }
      } else { (count, seen) }
    })
  valid_count
}
```

Both conditions must hold: signer in validator set AND not already counted.
**Action:** None. Close the finding.

---

### S2 — Replay protection (depth-in-defense gap)

**Severity:** Medium (mitigated by SMT; low practical exploitability)

**Where:** `cardano/contracts/validators/mailbox.ak:164-170`,
`cardano/contracts/validators/processed_message_nft.ak:35-71`

**Finding:** The original report said "`is_message_processed` only checks `tx.reference_inputs`" — that
description is of an older architecture. The current code uses an **SMT non-membership proof**:

```aiken
// mailbox.ak — validate_process
let smt_key = bytearray.take(message_id, 16)
let new_tree_root =
  smt.verify_non_membership_and_insert(
    datum.processed_tree_root,
    smt_key,
    smt_proof,
  )
// Mailbox datum is updated atomically: processed_tree_root = new_tree_root
```

Replay is cryptographically blocked: generating a valid non-membership proof for a key already in
the SMT is infeasible. Old proofs fail because the root has changed.

**Remaining gap:** `processed_message_nft` policy does not verify the minted asset name equals
`message_id`, and does not enforce asset-name uniqueness across TXs. The NFT is
informational/queryability only — not the actual replay guard. Two independent submissions could
each mint a processed NFT with the same asset name (though the SMT proof would reject the second TX
anyway).

**Plan (defense-in-depth, not blocking):**
Add an on-chain uniqueness check in the minting policy so the NFT independently proves
non-reprocessing:

```aiken
// processed_message_nft.ak — add inside mint validator:
// Require that the minted asset_name matches the message_id in the mailbox redeemer
// (currently no such check exists).
let mint_pairs = dict.to_pairs(own_mints)
expect [Pair(asset_name, 1)] = mint_pairs  // exactly one, quantity=1
// Add: verify asset_name == message_id extracted from the Mailbox Process redeemer
```

Full implementation requires reading the mailbox redeemer from `tx.redeemers` inside the NFT
policy, which is straightforward in Aiken.

**Priority:** Implement post-preprod if security posture requires it. Not blocking.

---

## Bug Findings

### B1 — Delivered-message indexer uses wrong address `mailbox_policy_id` ⚠️ HIGH

**Where:** `rust/main/chains/hyperlane-cardano/src/mailbox_indexer.rs:498`

**Code (buggy):**
```rust
let processed_script_address = self
    .provider
    .script_hash_to_address(&self.conf.mailbox_policy_id)  // ← wrong field
    .map_err(hyperlane_core::ChainCommunicationError::from_other)?;
```

`mailbox_policy_id` is the mailbox state-NFT minting policy hash. The address for querying
processed-message markers is derived from `processed_messages_script_hash`.

**Impact:** Scraper queries the wrong on-chain address → zero delivered messages indexed → metrics
and analytics broken.

**Fix (1 line):**
```rust
let processed_script_address = self
    .provider
    .script_hash_to_address(&self.conf.processed_messages_script_hash)  // ← correct
    .map_err(hyperlane_core::ChainCommunicationError::from_other)?;
```

---

### B2 — `CARDANO_INDEX_FROM` too high → relayer stuck forever ⚠️ HIGH

**Where:** `rust/main/agents/relayer/src/merkle_tree/db_loader.rs`, Cardano indexer config

**Root cause:** `MerkleTreeDbLoader` always starts at `leaf_index = 0`. If `INDEX_FROM` is set
to a block after the first mailbox dispatch, messages with early nonces never enter the DB.
The loader queries leaf 0 repeatedly, finds nothing, never increments — `highest_known_leaf_index()`
returns `None` → "Unable to reach quorum" forever.

**Operational fix (required now):** Set `CARDANO_INDEX_FROM` ≤ mailbox deployment block. Clear
relayer DB and restart.

**Code fix (validation + better error):** Add startup warning in `MerkleTreeDbLoader::tick()`:
```rust
async fn tick(&mut self) -> Result<()> {
    if let Some(insertion) = self.next_unprocessed_leaf().await? {
        // ...
        self.leaf_index += 1;
    } else {
        if self.leaf_index == 0 {
            warn!(
                "No merkle tree insertion found at leaf_index=0. \
                 Verify CARDANO_INDEX_FROM is <= the mailbox deployment block. \
                 Relayer will be stuck until leaf 0 is indexed."
            );
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    Ok(())
}
```

---

### B3 — Relayer stale TX cache not rebuilt on `BadInputsUTxO` ⚠️ HIGH

**Where:** `rust/main/chains/hyperlane-cardano/src/tx_builder.rs` (`recently_spent` HashMap)

**Root cause:** When a delivery TX is submitted, its inputs are added to `recently_spent` (120s TTL).
If the TX fails on-chain (or was never confirmed), those UTXOs remain blocked for the full TTL.
The next message build fails because it cannot find usable UTXOs (all filtered by `recently_spent`).

**Workaround:** Restart the relayer (clears in-memory cache).

**Fix:** After a TX submission returns `BadInputsUTxO`, identify which cached inputs are stale and
evict them:
```rust
Err(e) if is_retryable_bad_inputs_error(&e) => {
    // Parse spent input refs from error message
    let stale_refs = extract_bad_inputs_from_error(&e);
    self.evict_recently_spent(stale_refs).await;
    // Rebuild TX with fresh UTXO selection
    continue 'retry;
}
```

Additionally reduce `SPENT_UTXO_CACHE_TTL_SECS` from 120 to 60 as a conservative improvement.

---

### B4 — `U256::as_u64()` silent truncation in amount conversion ⚠️ MEDIUM

**Where:** `rust/main/chains/hyperlane-cardano/src/tx_builder.rs:5532-5546`
(`convert_wire_to_local_amount`)

**Code (buggy):**
```rust
fn convert_wire_to_local_amount(wire_amount: U256, remote_decimals: u8, local_decimals: u8) -> u64 {
    if local_decimals >= remote_decimals {
        let multiplier = U256::from(10u64).pow(U256::from(local_decimals - remote_decimals));
        let result = wire_amount.saturating_mul(multiplier);
        result.as_u64()  // ← truncates silently if result > u64::MAX
    } else {
        let divisor = U256::from(10u64).pow(U256::from(remote_decimals - local_decimals));
        let result = wire_amount / divisor;
        result.as_u64()  // ← truncates silently
    }
}
```

**Fix:** Return `Result<u64, TxBuilderError>` and add bounds check before cast:
```rust
fn convert_wire_to_local_amount(
    wire_amount: U256,
    remote_decimals: u8,
    local_decimals: u8,
) -> Result<u64, TxBuilderError> {
    let result = if local_decimals >= remote_decimals {
        let m = U256::from(10u64).pow(U256::from(local_decimals - remote_decimals));
        wire_amount.saturating_mul(m)
    } else {
        let d = U256::from(10u64).pow(U256::from(remote_decimals - local_decimals));
        wire_amount / d
    };
    if result > U256::from(u64::MAX) {
        return Err(TxBuilderError::Encoding(format!(
            "amount overflow: {result} exceeds u64::MAX"
        )));
    }
    Ok(result.as_u64())
}
```

Update all call sites to propagate the error with `?`.

---

### B5 — `get_balance` masks RPC errors with `U256::zero()` ⚠️ MEDIUM

**Where:** `rust/main/chains/hyperlane-cardano/src/provider.rs:91-107`

**Code (buggy):**
```rust
Err(e) => {
    tracing::warn!("Failed to get balance for Cardano address {}: {}", address, e);
    Ok(U256::zero())  // ← hides RPC outage
}
```

Callers cannot distinguish "address has 0 ADA" from "Blockfrost is down". Downstream TXs fail
with confusing "insufficient funds" errors.

**Fix (3 lines):**
```rust
Err(e) => {
    Err(hyperlane_core::ChainCommunicationError::from_other(e))
}
```

---

### B6 — Blockfrost cursor race condition (address-TX index lag) ⚠️ MEDIUM

**Where:** `rust/main/chains/hyperlane-cardano/src/blockfrost_provider.rs`
(cursor advance / `get_address_transactions`)

**Root cause:** Blockfrost block tip advances in real-time but address-transaction index lags 25-40s.
Current `confirmation_block_delay` defaults to 2 blocks (~40s on Preview). When tip spikes or block
time is short, the indexer scans blocks whose address index isn't populated yet → empty result →
cursor advances past those blocks → TXs permanently missed.

**Immediate fix:** Increase default `confirmation_block_delay` from 2 → 4 in
`rust/main/chains/hyperlane-cardano/src/types.rs` (trait builder defaults):
```rust
// confirmation_block_delay: 2  →  4
```

**Proper fix:** Implement a backfill re-scan window (check last N blocks on every pass, analogous
to EVM `reorg_period`):
```rust
// On each indexer tick, re-check max(0, cursor - BACKFILL_WINDOW) blocks
// to catch any TXs that Blockfrost indexed late.
const BACKFILL_WINDOW: u64 = 5;
let safe_from = from.saturating_sub(BACKFILL_WINDOW);
```

---

### B7 — Chain metrics return placeholder zeros (Low)

**Where:** `rust/main/chains/hyperlane-cardano/src/provider.rs:44-62` (`get_block_by_height`),
`provider.rs:109-124` (`get_chain_metrics`)

Both return `H256::zero()` and `timestamp: 0`. The Blockfrost API already provides this data via
`/blocks/{height}` (already used in `blockfrost_provider.rs`).

**Fix:** Call `self.provider.get_block_by_height(height)` inside `CardanoProvider` and populate:
```rust
Ok(BlockInfo {
    hash: H256::from_slice(&hex::decode(&block.hash)?),
    timestamp: block.time as u64,
    number: height,
})
```

---

### B8 — P0: `WADA` collateral deployed with `scale=1e12` (preprod blocker)

**Where:** Sepolia deployment — `SEPOLIA_COLLATERAL_WADA` at
`0xC437640D34671bdf461aE7Ea168cdDd47Baf7a5D`

**Root cause:** `scale=1e12` encodes `wire = amount * 1e12` → Cardano receives `1e18 * 1e12 / 1e12
= 1e18 lovelace` for 1 WADA → impossible to fund. Correct `scale=1`.

**Fix script:** `solidity/script/warp-e2e/FixWadaCollateral.s.sol` (already written).
Redeploy collateral contract with `scale=1` before preprod Phase 2.1/2.4.

---

## Code Quality Findings

### Q1 — Raw Plutus constructor tags (121/122/123) ⚠️ HIGH

**Where:** `rust/main/chains/hyperlane-cardano/src/tx_builder.rs` (5+ locations)

Raw numeric Plutus CBOR tags are sprinkled inline. Any Aiken schema reordering silently produces
wrong tags with no compiler warning.

```rust
// Current (fragile):
tag: 122,  // Constructor 1: Process

// Needed:
tag: MailboxRedeemer::Process.plutus_tag(),
```

**Plan:** Create `cardano/contracts/src/plutus_schema.rs` (or add to `types.rs`) with typed enums
mirroring every Aiken sum type that is encoded/decoded in Rust:

```rust
pub enum MailboxRedeemer { Dispatch = 0, Process = 1, SetDefaultIsm = 2, TransferOwnership = 3 }
pub enum WarpTokenType   { Collateral = 0, Synthetic = 1, Native = 2 }
pub enum GreetingRedeemer { Init = 0, HandleMessage = 1, Reclaim = 2 }

impl MailboxRedeemer {
    pub fn plutus_tag(self) -> u64 {
        let i = self as u64;
        if i <= 6 { 121 + i } else { 1280 + (i - 7) }
    }
}
```

Replace all raw tag literals with `SomeEnum::Variant.plutus_tag()`.

---

### Q2 — Magic numbers in Rust crate ⚠️ MEDIUM

**Where:** `tx_builder.rs`, `types.rs`, `mailbox_indexer.rs`

Most critical inline literals:

| Constant needed              | Value | Location                  |
|------------------------------|-------|---------------------------|
| `POLICY_ID_ADDR_PREFIX`      | `0x01`| `types.rs:330`, `tx_builder.rs` |
| `SCRIPT_HASH_ADDR_PREFIX`    | `0x02`| `types.rs:360`, `tx_builder.rs` |
| `MULTISIG_ISM_METADATA_MIN`  | `68`  | `tx_builder.rs:5638`      |
| `ECDSA_SIG_SIZE`             | `65`  | `tx_builder.rs:5699`      |
| `MESSAGE_ID_SIZE`            | `32`  | multiple locations        |
| `SCRIPT_HASH_SIZE`           | `28`  | multiple locations        |
| `CARDANO_SCRIPT_ADDR_TESTNET`| `0x70`| `tx_builder.rs:4222`      |
| `CARDANO_SCRIPT_ADDR_MAINNET`| `0x71`| `tx_builder.rs:4224`      |

**Plan:** Create `rust/main/chains/hyperlane-cardano/src/consts.rs`:
```rust
pub mod address_prefix { pub const POLICY_ID: u8 = 0x01; pub const SCRIPT_HASH: u8 = 0x02; }
pub mod metadata { pub const MULTISIG_ISM_MIN_LEN: usize = 68; pub const ECDSA_SIG_LEN: usize = 65; }
pub mod cardano_addr { pub const SCRIPT_TESTNET: u8 = 0x70; pub const SCRIPT_MAINNET: u8 = 0x71; }
```

---

### Q3 — `unwrap()`/`expect()` in production paths ⚠️ MEDIUM

**Where:** `mailbox.rs`, `tx_builder.rs`, `interchain_gas.rs`, `cardano/cli/src/*`

Replace panicking calls with typed `TxBuilderError` or `ChainCommunicationError` returns. Prioritize
paths in the relayer hot path (TX submission, UTXO selection).

---

### Q4 — Address/script conversion duplication (Low)

**Where:** `types.rs`, `recipient_resolver.rs`

`extract_script_hash_from_address` and `script_hash_to_address` exist in multiple files with
slightly different error types. Consolidate into `types.rs` and re-export; update call sites.

---

### Q5 — `tx_builder.rs` monolith (Low)

**Where:** `rust/main/chains/hyperlane-cardano/src/tx_builder.rs` (~6000 lines)

Responsibilities: metadata parsing, amount conversion, recipient resolution, TX construction,
crypto helpers, test vectors. Split into sub-modules:
`metadata.rs`, `amounts.rs`, `redeemers.rs`, `recipient.rs` — keep `tx_builder.rs` as
orchestration layer.

---

### Q6 — Duplicate domain ID mapping (Low)

**Where:** `mailbox_indexer.rs`, `blockfrost_provider.rs`, `tx_builder.rs`

Domain IDs `2001/2002/2003` repeated in multiple files. Centralize in `consts.rs` with a helper
`fn domain_id(network: CardanoNetwork) -> u32`.

---

## Codex Findings (second opinion)

### C1 — `ChainSigner` returns placeholder/zero address for Cardano keypair ⚠️ HIGH

**Where:** `rust/main/hyperlane-base/src/settings/signers.rs:344-351`

**Code (buggy):**
```rust
impl ChainSigner for hyperlane_cardano::Keypair {
    fn address_string(&self) -> String {
        // TODO: Implement proper Cardano address derivation
        "cardano_address_placeholder".to_string()  // ← wrong
    }

    fn address_h256(&self) -> H256 {
        // TODO: Implement proper Cardano address to H256 conversion
        H256::zero()  // ← wrong
    }
}
```

Any code path that depends on `chain_signer.address_h256()` or `address_string()` — including
the validator self-announce check — operates against the zero address instead of the actual signer.
This means validator identity checks and announcement flows are broken.

**Fix:** Derive the address from the loaded keypair. `Keypair` already holds `pallas_public`; use
it to compute the Blake2b-224 payment key hash and pad to H256:
```rust
fn address_h256(&self) -> H256 {
    let mut h = H256::zero();
    // Payment credential prefix (0x00000000) + 28-byte key hash
    h.0[4..].copy_from_slice(self.payment_key_hash().as_ref());
    h
}

fn address_string(&self) -> String {
    self.bech32_address()  // implemented on Keypair
}
```

---

### C2 — Testnet private signing keys committed to git ⚠️ MEDIUM

**Where:** `cardano/testnet-keys/payment.skey`, `cardano/testnet-keys/recipient/payment.skey`

Both files contain raw Ed25519 private keys in `cborHex`:
```json
{ "cborHex": "5820f5e5319e6f4403600791409af815d37996b5357b3f33905bba180a9783c7538e" }
```

Anyone with repo access can drain these wallets. Even on testnet this breaks shared environments.

**Fix:**
- Remove both files from git tracking: `git rm --cached cardano/testnet-keys/payment.skey ...`
- Add `cardano/testnet-keys/` to `.gitignore`
- Distribute keys via `CARDANO_OPERATOR_KEY` env var or a local secrets manager
- Rotate the exposed keys immediately

---

## Operational Findings

### O1 — Parallel CLI invocations conflict on collateral UTXO (P1)

**Where:** `cardano/cli/src/utils/tx_builder.rs` (collateral selection)

Two concurrent CLI processes query Blockfrost independently, select the same UTXO as collateral,
and one fails with `NoCollateralInputs` / `BadInputsUTxO`.

**Fix (minimal):** Add a process-level file lock keyed on wallet address:
```rust
let lock_path = format!("/tmp/hyperlane-cli-{}.lock", &wallet_addr[..16]);
let _lock = FileLock::exclusive(&lock_path)?;  // Released on drop
```

**Fix (config):** Accept `--collateral-utxo <txhash#idx>` CLI flag to let operators pre-select
and coordinate manually.

---

### O2 — `init all` silently skips IGP (P2)

**Where:** `cardano/cli/src/commands/init.rs` (`init_all`)

**Fix (XS):** Either include `init_igp` in the `init_all` flow, or print explicit message:
```
ℹ IGP: not initialized — run `cli init igp` separately
```

---

### O3 — `.env` / `deployment_info.json` manual sync gap (P2)

**Where:** CLI UX

After any contract redeploy, operators must update both files. Divergence causes silent wrong-address
usage.

**Fix:** Add `cli config export --format env` command to generate a `.env` from
`deployment_info.json`. No manual sync required.

---

### O4 — `onChainFeeQuoting` incompatible with official Sepolia mailbox (P2)

The official Sepolia mailbox default hook charges 1 wei via its own IGP, not our custom one.
`onChainFeeQuoting` rejects such messages as `GasPaymentNotFound`.

**Fix (XS):** Change enforcement to `type: none` for cross-chain tests, or always pass
`--hook <aggregation_hook>` when dispatching from official Sepolia mailbox. Document clearly in
relayer config example.

---

## Recommended Fix Order

### Blocking (do before preprod)

1. **C2** — Remove testnet private keys from git; rotate exposed keys; add to `.gitignore`.
2. **B8** — Redeploy `WADA` collateral with `scale=1` (`FixWadaCollateral.s.sol`).
3. **B1** — 1-line fix: `mailbox_indexer.rs:498` → use `processed_messages_script_hash`.
4. **B2** — Operational: set correct `CARDANO_INDEX_FROM`; add startup warning in `MerkleTreeDbLoader`.
5. **C1** — Implement real address derivation in `ChainSigner for Keypair`.
6. **B4** — Add bounds check in `convert_wire_to_local_amount`.
7. **B5** — Propagate error in `get_balance` instead of returning zero.
8. **O1** — Add file lock for parallel CLI invocations.

### High-value, pre-audit

9.  **Q1** — Replace raw Plutus tags with named enum helpers.
10. **B3** — Evict stale inputs from cache on `BadInputsUTxO`.
11. **B6** — Increase `confirmation_block_delay` to 4; add backfill window.
12. **Q2** — Extract magic numbers to `consts.rs`.

### Post-preprod

13. **B7** — Implement real chain metrics (block hash, timestamp).
14. **Q3** — Replace `unwrap`/`expect` in production paths.
15. **S2** — Add defense-in-depth uniqueness check to `processed_message_nft` policy.
16. **Q4–Q6** — Code quality cleanup (consolidate helpers, split modules, unify domain IDs).
17. **O2–O4** — UX improvements.

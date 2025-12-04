# Reference Script Locator Implementation Plan

This document tracks the implementation of `reference_script_locator` in the Hyperlane Cardano registry.

## Goal

Enable the relayer to discover reference script UTXOs via NFT locators, following the design in `HYPERLANE_CARDANO_REFERENCE_SCRIPTS.md`.

## Current State (Updated)

- **Recipient deployed** (two-UTXO pattern):
  - Script hash: `931e71c75bd0ac35ff9024b3c2a578e006bf3abca509c11734f7f9bc`
  - State UTXO: TX `b7e620e418b3f8d1a52297fe86fd8b62e05c929ad72f3add485f630026700c36#0`
  - Reference Script UTXO: TX `b7e620e418b3f8d1a52297fe86fd8b62e05c929ad72f3add485f630026700c36#1`
- **State NFT**: Policy `f03b07a75d1e5671cc463a97252c3a75320cc5b8d957afb8739b0385`, asset name empty
- **Ref Script NFT**: Policy `f03b07a75d1e5671cc463a97252c3a75320cc5b8d957afb8739b0385`, asset name "ref" (726566)
- **Registry** (NEW with reference_script_locator support):
  - Hash: `eb0ec4a2f9097c9cecdbb21091e1d5291b1f25af383ea1e73116f278`
  - Address: `addr_test1wr4sa39zlyyhe88vmweppy0p6553k8e94uurag08xyt0y7qagxpdg`
  - State NFT Policy: `41e7ac7a6d1552cb848da41d25bec4c47a17efbc83d79d23b20e8839`
  - Registration TX: `2c8f3cffbd48a9647a0dc7bc22ab921b3531ae8ef0b09c1f731fb017b66d3cdc`

## Implementation Tasks

### Phase 1: Update Aiken Types - COMPLETED

- [x] **1.1** Update `cardano/contracts/lib/types.ak`
  - Add `reference_script_locator: Option<UtxoLocator>` to `RecipientRegistration`

- [x] **1.2** Update `cardano/contracts/validators/registry.ak`
  - No changes needed (just stores registration)

- [x] **1.3** Rebuild contracts
  ```bash
  cd cardano/contracts && aiken build
  ```

### Phase 2: Update CLI - COMPLETED

- [x] **2.1** Update `cardano/cli/src/utils/cbor.rs`
  - Updated `RegistrationData` struct with `ref_script_policy_id`, `ref_script_asset_name`
  - Updated `build_registry_admin_register_redeemer()` and `build_registry_register_redeemer()`
  - Updated `build_registry_datum()` to encode `reference_script_locator`

- [x] **2.2** Update `cardano/cli/src/commands/registry.rs`
  - Added `--ref-script-policy` and `--ref-script-asset` options to `register` command
  - Updated parsing for 6-field registration structure

- [x] **2.3** Update `cardano/cli/src/utils/types.rs`
  - Added `ref_script_policy_id` and `ref_script_asset_name` to `RecipientInfo`

- [x] **2.4** Rebuild CLI
  ```bash
  cd cardano/cli && cargo build --release  # PASSES
  ```

### Phase 3: Update Relayer - COMPLETED

- [x] **3.1** Update `rust/main/chains/hyperlane-cardano/src/types.rs`
  - Added `reference_script_locator: Option<UtxoLocator>` to `RecipientRegistration`

- [x] **3.2** Update `rust/main/chains/hyperlane-cardano/src/registry.rs`
  - Updated `parse_registration_from_plutus()` for CBOR parsing (6 fields)
  - Updated `parse_registration_json()` for JSON parsing (6 fields)
  - Added `parse_optional_locator_from_plutus()` helper

- [x] **3.3** Update `rust/main/chains/hyperlane-cardano/src/tx_builder.rs`
  - Added `recipient_ref_script_utxo: Option<Utxo>` to `ProcessTxComponents`
  - In `build_process_tx`: look up reference script UTXO via locator from registry
  - In `build_complete_process_tx`: add reference script UTXO as reference input

- [x] **3.4** Update `rust/main/chains/hyperlane-cardano/src/bin/cardano_register.rs`
  - Added `--ref-script-policy` and `--ref-script-asset` command line options
  - Updated registration construction with `reference_script_locator`

- [x] **3.5** Build relayer
  ```bash
  cd rust/main && cargo check -p hyperlane-cardano  # PASSES
  ```

### Phase 4: Redeploy Recipient - COMPLETED

**CLI updated to support two-UTXO pattern automatically!**

The `init recipient` command now:
- Mints TWO NFTs in a single transaction (state NFT + ref NFT)
- Creates state UTXO at script address with empty-name NFT + datum
- Creates reference script UTXO at deployer address with "ref" NFT + script attached
- Outputs the correct `registry register` command with `--ref-script-policy` and `--ref-script-asset`

- [x] **4.1** Updated CLI `init recipient` command
  - Added `--ref-script-lovelace` parameter (default 20 ADA)
  - New `build_init_recipient_two_utxo_tx()` in tx_builder
  - Mints both NFTs with same policy (state="" + ref="ref")

- [x] **4.2** Deploy new recipient with two-UTXO pattern
  - TX: `b7e620e418b3f8d1a52297fe86fd8b62e05c929ad72f3add485f630026700c36`
  - NFT Policy: `f03b07a75d1e5671cc463a97252c3a75320cc5b8d957afb8739b0385`

- [x] **4.3** Updated state_nft validator to allow minting 1-2 tokens (for two-UTXO pattern)

- [x] **4.4** Re-extracted validators with updated types (`deploy extract`)
  - New registry hash: `eb0ec4a2f9097c9cecdbb21091e1d5291b1f25af383ea1e73116f278`

- [x] **4.5** Deployed new registry with updated types
  - TX: `81051632b6bf93c77213bee4870824b097edb88d7d1d50c1bfed57bcd1991666`
  - State NFT Policy: `41e7ac7a6d1552cb848da41d25bec4c47a17efbc83d79d23b20e8839`

- [x] **4.6** Registered recipient with `reference_script_locator`
  - TX: `2c8f3cffbd48a9647a0dc7bc22ab921b3531ae8ef0b09c1f731fb017b66d3cdc`

### Phase 5: Update Test Script - COMPLETED

- [x] **5.1** Updated `cardano/scripts/send-message-to-recipient.sh`
  - Updated examples with new recipient script hash

### Phase 6: End-to-End Test - IN PROGRESS

- [ ] **6.1** Send test message from Fuji to Cardano recipient
- [ ] **6.2** Verify relayer picks up message and delivers it
- [ ] **6.3** Verify recipient state is updated

---

## Type Changes

### Aiken (types.ak)

```aiken
// BEFORE
pub type RecipientRegistration {
  script_hash: ScriptHash,
  state_locator: UtxoLocator,
  additional_inputs: List<AdditionalInput>,
  recipient_type: RecipientType,
  custom_ism: Option<ScriptHash>,
}

// AFTER
pub type RecipientRegistration {
  script_hash: ScriptHash,
  state_locator: UtxoLocator,
  reference_script_locator: Option<UtxoLocator>,  // NEW
  additional_inputs: List<AdditionalInput>,
  recipient_type: RecipientType,
  custom_ism: Option<ScriptHash>,
}
```

### Rust (types.rs)

```rust
// BEFORE
pub struct RecipientRegistration {
    pub script_hash: ScriptHash,
    pub state_locator: UtxoLocator,
    pub additional_inputs: Vec<AdditionalInput>,
    pub recipient_type: RecipientType,
    pub custom_ism: Option<ScriptHash>,
}

// AFTER
pub struct RecipientRegistration {
    pub script_hash: ScriptHash,
    pub state_locator: UtxoLocator,
    pub reference_script_locator: Option<UtxoLocator>,  // NEW
    pub additional_inputs: Vec<AdditionalInput>,
    pub recipient_type: RecipientType,
    pub custom_ism: Option<ScriptHash>,
}
```

---

## CBOR Encoding

The new field is inserted at position 2 (after `state_locator`, before `additional_inputs`):

```
RecipientRegistration = Constr 0 [
  script_hash,           // field 0: ByteArray (28 bytes)
  state_locator,         // field 1: UtxoLocator
  reference_script_locator, // field 2: Option<UtxoLocator> (NEW)
  additional_inputs,     // field 3: List<AdditionalInput>
  recipient_type,        // field 4: RecipientType
  custom_ism,            // field 5: Option<ScriptHash>
]

Option<UtxoLocator> encoding:
  None = Constr 1 []
  Some(locator) = Constr 0 [locator]
```

---

## Notes

- Registry contract itself doesn't need to change logic (it just stores the registration)
- Existing registrations will break (field count mismatch) - need migration or fresh registry
- **DONE**: Deployed fresh registry with updated types to avoid migration complexity
- **DONE**: Updated state_nft validator to allow minting 1-2 tokens for two-UTXO pattern
- The relayer config needs to be updated with the new registry NFT policy: `41e7ac7a6d1552cb848da41d25bec4c47a17efbc83d79d23b20e8839`

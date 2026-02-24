[← Epic 4: Advanced Features](./EPIC.md) | [Epics Overview](../README.md)

# Task 4.4: NFT-Based Contract Identity

**Status:** 🟢 Complete
**Complexity:** High
**Depends On:** None

## Objective

Implement a stable identity system using NFTs that allows contracts to be upgraded independently without cascading redeployments.

## Problem Statement

Currently, contracts are parameterized by each other's policy IDs:

```
processed_msg_nft(mailbox_policy_id)    → tied to mailbox identity
recipient(mailbox_policy_id)             → tied to mailbox identity
```

When the mailbox code changes:
1. Mailbox policy ID changes (new code = new hash)
2. `processed_msg_nft` must be redeployed with new parameter
3. All recipients must be redeployed with new parameter
4. Registry entries must be updated
5. Old processed message NFTs are at wrong policy (replay protection breaks)

## Solution: State NFT as Identity

The existing **state NFT** already serves as a stable identity token. Its policy ID is derived from a one-shot minting policy (seed UTXO), so it never changes across upgrades. Other contracts are parameterized by this state NFT policy, not the validator script hash.

The `Migrate` redeemer moves the state UTXO (with datum + state NFT) from the old script address to a new one. Since the state NFT policy stays constant, all dependent contracts (processed_message_nft, verified_message_nft, warp routes) continue working without redeployment.

```
┌─────────────────────────────────────────────────────────────────┐
│                    STATE NFT AS IDENTITY                        │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│   1. State NFT minted at deployment (one-shot, stable policy)   │
│      - Policy ID is stable forever                              │
│      - Other contracts parameterized by this policy             │
│                                                                 │
│   2. Mailbox v1 holds the state NFT                             │
│      - Mailbox script hash can change                           │
│      - State NFT policy stays constant                          │
│                                                                 │
│   3. Upgrade: Migrate redeemer moves state to new address       │
│      - Owner signs migration TX                                 │
│      - State NFT + datum move to new script address             │
│      - All dependent contracts continue working                 │
│                                                                 │
│   4. All 4 contracts support migration:                         │
│      - Mailbox: MigrateMailbox { new_address }                  │
│      - ISM: MigrateIsm { new_address }                          │
│      - IGP: MigrateIgp { new_address }                          │
│      - Warp Route: MigrateWarp { new_address }                  │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

## Implementation

### 1. Shared Migration Utility — `utils.ak`

```aiken
pub fn validate_migrate(
  owner: VerificationKeyHash,
  new_address: Address,
  tx: Transaction,
  own_ref: OutputReference,
  expected_datum: Data,
) -> Bool {
  expect is_signed_by(tx, owner)
  expect Some(own_input) = find_input(tx, own_ref)
  let own_value = own_input.output.value
  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == new_address })
  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum_data == expected_datum
  value_preserved(own_value, continuation.value)
}
```

Checks:
- Owner signature required
- Continuation output at new address with identical datum
- Value preserved (state NFT + ADA)

### 2. Migrate Redeemers in All Validators

Each validator has a new redeemer variant that delegates to `validate_migrate`:

| Validator | Redeemer | Type in `types.ak` |
|-----------|----------|-------------------|
| `mailbox.ak` | `MigrateMailbox { new_address }` | `MailboxRedeemer` |
| `multisig_ism.ak` | `MigrateIsm { new_address }` | `MultisigIsmRedeemer` |
| `igp.ak` | `MigrateIgp { new_address }` | `IgpRedeemer` |
| `warp_route.ak` | `MigrateWarp { new_address }` | `WarpRouteRedeemer` |

### 3. CLI Migrate Commands

Each CLI module has a `migrate` subcommand:

```bash
# Auto-compute new hash from blueprint + deployment params
cardano-cli mailbox migrate
cardano-cli ism migrate
cardano-cli igp migrate
cardano-cli warp migrate --warp-policy <policy>

# Or specify hash explicitly
cardano-cli mailbox migrate --new-script-hash <hash>

# Dry run (show hashes, don't submit TX)
cardano-cli mailbox migrate --dry-run
```

For parameterized contracts (mailbox, warp), the CLI auto-applies deployment parameters from `deployment_info.json` to compute both the current and new script hashes.

### 4. Script Loading Fix

All mailbox operations (dispatch, set_default_ism, migrate) now correctly apply parameters from `deployment_info.json` when loading inline scripts. Previously, dispatch/set_default_ism would load the unparameterized blueprint code, causing `MissingScriptWitnessesUTXOW` errors when no reference script was available.

## Files Modified

| File | Changes |
|------|---------|
| `contracts/lib/types.ak` | Added `MigrateMailbox`, `MigrateIsm`, `MigrateIgp`, `MigrateWarp` redeemer variants |
| `contracts/lib/utils.ak` | Added `validate_migrate()` shared utility |
| `contracts/validators/mailbox.ak` | Added `MigrateMailbox` handler |
| `contracts/validators/multisig_ism.ak` | Added `MigrateIsm` handler |
| `contracts/validators/igp.ak` | Added `MigrateIgp` handler |
| `contracts/validators/warp_route.ak` | Added `MigrateWarp` handler |
| `contracts/plutus.json` | Rebuilt with new validators |
| `cli/src/commands/mailbox.rs` | Added `migrate` subcommand, fixed param application in dispatch/set_default_ism, fixed state NFT asset name |
| `cli/src/commands/ism.rs` | Added `migrate` subcommand with blueprint hash validation |
| `cli/src/commands/igp.rs` | Added `migrate` subcommand with blueprint hash validation |
| `cli/src/commands/warp.rs` | Added `migrate` subcommand with param auto-application |
| `cli/src/utils/cbor.rs` | Added `build_migrate_redeemer()` |
| `cli/src/utils/plutus.rs` | Added `compute_blueprint_hash()` helper |

## Testing

Tested end-to-end on preview testnet:

| Test Case | Result |
|-----------|--------|
| `mailbox migrate --dry-run` | Shows current vs new hash, no TX |
| Forward migration (v1→v2) | State moved to new address, all fields preserved |
| Reverse migration (v2→v1) | State moved back, all fields preserved |
| Dispatch after round-trip migration | TX succeeds |
| Relayer delivery after migration (Sepolia→Cardano) | Message delivered, greeting received |
| Relayer delivery after migration (Cardano→Sepolia) | Message delivered to TestRecipient |
| Greeting receive after migration | State updated correctly (count: 2) |

## Definition of Done

- [x] Shared `validate_migrate()` utility implemented
- [x] All 4 validators have Migrate redeemers
- [x] CLI `migrate` subcommands for all contracts
- [x] Auto-compute new script hash from blueprint + deployment params
- [x] Parameterized script loading fixed (dispatch, set_default_ism)
- [x] State NFT asset name handled correctly in all operations
- [x] Deployment info updated after migration
- [x] Migration tested end-to-end (round-trip v1→v2→v1)
- [x] Post-migration messaging verified bidirectionally

## Acceptance Criteria

1. ~~New deployments use identity NFT pattern~~ State NFT already serves as stable identity
2. Mailbox can be upgraded without redeploying dependent contracts ✅
3. Processed message NFTs remain valid across upgrades ✅
4. Existing security guarantees maintained ✅
5. All 4 contract types support migration ✅
6. Bidirectional messaging works after migration ✅

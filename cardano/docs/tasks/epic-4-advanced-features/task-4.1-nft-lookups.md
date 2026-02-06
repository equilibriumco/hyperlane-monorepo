[<- Epic 4: Advanced Features](./EPIC.md) | [Epics Overview](../README.md)

# Task 4.1: State NFT Policy as Hyperlane Address

**Status:** ✅ Complete
**Complexity:** High
**Depends On:** None
**Related:** [Task 4.4: NFT-Based Contract Identity](./task-4.4-nft-identity.md)

## Objective

Use the state NFT policy ID as the canonical Hyperlane address for all Cardano recipients, replacing script hash-based addressing. This provides O(1) lookups, removes the registry contract entirely, and aligns with Cardano's NFT-based identity patterns.

## Background

### Previous Approach

Cardano recipients used their **script hash** as their Hyperlane address:

1. Remote chains enrolled `script_hash` as the recipient address
2. Messages arrived with `recipient = script_hash`
3. Registry contract lookups iterated through entries to find `script_hash` (O(n))
4. Warp routes required an unused `_state_nft_policy_id` parameter to force unique script hashes

### Problems Solved

- **Script hash collision**: Warp routes with identical bytecode produced identical script hashes. An unused parameter was needed to differentiate them.
- **O(n) registry lookups**: The registry stored entries in a list, requiring iteration.
- **Registry as bottleneck**: A shared on-chain registry was a contention point for all recipients.

## What Was Implemented

### Addressing Scheme

**Hyperlane address = `0x01000000 || state_nft_policy_id`** (32 bytes total)

- `0x01` prefix byte distinguishes Cardano addresses from EVM (which use `0x000...` padding)
- 3 zero-padded bytes follow the prefix
- 28-byte state NFT policy ID (the natural unique identifier for each deployment)

### Registry Removal

The on-chain registry contract was **removed entirely**. Instead, the relayer uses a `RecipientResolver` that performs O(1) NFT queries directly against the chain indexer (Blockfrost). No registry contract is needed.

The resolver detects **recipient kind from the datum structure**:

- **WarpRoute**: Identified by warp route datum shape
- **Generic**: Identified by generic recipient datum shape

### Warp Route Parameter Simplification

Warp route validators went from 4 parameters to 3:

```aiken
// BEFORE
validator warp_route(
  mailbox_policy_id: PolicyId,
  _state_nft_policy_id: PolicyId,
  processed_messages_nft_policy: PolicyId,
  redemption_script: ScriptHash,
)

// AFTER
validator warp_route(
  mailbox_policy_id: PolicyId,
  processed_messages_nft_policy: PolicyId,
  redemption_script: ScriptHash,
)
```

All warp routes now share the **same script address**, identified by unique state NFTs.

### Shared Reference Script UTXO

All warp routes share a single reference script UTXO, stored in the relayer config as `warp_route_reference_script_utxo`. This eliminates the need for per-warp-route reference scripts.

### Custom ISM Support

An `ism: Option<ScriptHash>` field was added to `WarpRouteDatum`, allowing per-warp-route ISM configuration.

### Deferred Recipient Removal (Relayer)

The deferred recipient type was removed from the relayer. The contracts still exist, but the relayer no longer handles them as a distinct case.

## Files Changed

| File                                                           | Change                                                   |
| -------------------------------------------------------------- | -------------------------------------------------------- |
| `cardano/contracts/validators/warp_route.ak`                   | Removed `_state_nft_policy_id` parameter (4 -> 3 params) |
| `cardano/contracts/lib/types.ak`                               | Added `ism` field to datum, removed registry types       |
| `cardano/contracts/validators/registry.ak`                     | **Deleted**                                              |
| `rust/main/chains/hyperlane-cardano/src/recipient_resolver.rs` | **New** - O(1) NFT-based recipient resolution            |
| `rust/main/chains/hyperlane-cardano/src/registry.rs`           | **Deleted**                                              |
| `rust/main/chains/hyperlane-cardano/src/tx_builder.rs`         | Updated for new addressing and shared reference script   |
| `rust/main/chains/hyperlane-cardano/src/mailbox.rs`            | Uses `0x01` prefix addressing                            |
| `rust/main/chains/hyperlane-cardano/src/types.rs`              | Updated types for new scheme                             |
| `rust/main/chains/hyperlane-cardano/src/trait_builder.rs`      | Wired up `RecipientResolver`                             |
| `rust/main/chains/hyperlane-cardano/src/connection_parser.rs`  | Updated config parsing                                   |
| `cardano/cli/src/commands/warp.rs`                             | Deploy with 3 params, `0x01` prefix for sender address   |
| `cardano/cli/src/commands/registry.rs`                         | **Deleted**                                              |
| `cardano/cli/src/cbor.rs`                                      | Updated datum encoding with `ism` field                  |

## Definition of Done

- [x] `warp_route.ak` reduced to 3 parameters (`mailbox_policy_id`, `processed_messages_nft_policy`, `redemption_script`)
- [x] Registry contract removed; relayer uses `RecipientResolver` with O(1) NFT queries
- [x] Hyperlane address uses `0x01000000 || policy_id` format (32 bytes)
- [x] All warp routes share the same script address, differentiated by state NFTs
- [x] Shared reference script UTXO for all warp routes
- [x] Custom ISM support via `ism: Option<ScriptHash>` in datum
- [x] Recipient kind detection from datum structure (WarpRoute vs Generic)
- [x] CLI updated for new deploy params and address format

## Relationship to Task 4.4

This task and [Task 4.4 (NFT-Based Contract Identity)](./task-4.4-nft-identity.md) share a common philosophy: **NFT policies are stable identifiers, script hashes are implementation details.**

- **Task 4.1**: State NFT policy = recipient's Hyperlane address
- **Task 4.4**: Identity NFT policy = mailbox's stable reference for upgrades

Together, they establish a consistent "NFT-based identity" pattern for Cardano Hyperlane contracts.

[<- Epic 4: Advanced Features](./EPIC.md) | [Epics Overview](../README.md)

# Task 4.1: State NFT Policy as Hyperlane Address

**Status:** ✅ Complete
**Complexity:** High
**Depends On:** None
**Related:** [Task 4.4: NFT-Based Contract Identity](./task-4.4-nft-identity.md)

## Objective

Replace registry-based recipient lookups with direct addressing. Warp routes use their state NFT policy ID (`0x01` prefix) for O(1) lookups. Generic recipients use their script hash (`0x02` prefix). Removes the registry contract entirely.

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

Two address formats coexist, depending on recipient type:

**Warp routes (TokenReceiver):** `0x01000000 || state_nft_policy_id` (32 bytes)
- `0x01` prefix = NFT-policy addressing
- Warp routes are spent in the same TX as the mailbox
- No verified_message_nft needed (they validate by checking mailbox co-spending)

**Generic recipients (e.g., greeting):** `0x02000000 || script_hash` (32 bytes)
- `0x02` prefix = script-hash addressing
- Two-phase delivery: mailbox creates verified_message_nft UTXO at recipient script address
- Recipient processes message in separate TX, burning the NFT

The mailbox conditionally mints/delivers `verified_message_nft` only for `0x02` recipients.

### Registry Removal

The on-chain registry contract was **removed entirely**. Instead, the relayer uses a `RecipientResolver` that performs O(1) NFT queries directly against the chain indexer (Blockfrost). No registry contract is needed.

The resolver detects **recipient kind from the datum structure**:

- **WarpRoute**: Identified by warp route datum shape
- **Generic**: Identified by generic recipient datum shape

### Warp Route Parameter Simplification

Warp route validators went from 4 parameters to 2:

```aiken
// BEFORE (original)
validator warp_route(
  mailbox_policy_id: PolicyId,
  _state_nft_policy_id: PolicyId,
  processed_messages_nft_policy: PolicyId,
  redemption_script: ScriptHash,
)

// AFTER (current)
validator warp_route(
  mailbox_policy_id: PolicyId,
  processed_messages_nft_policy: PolicyId,
)
```

The `_state_nft_policy_id` was removed (unique identity via state NFTs doesn't need it as a parameter). The `redemption_script` was removed (tokens go directly to recipient wallets, no redemption pattern).

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

- [x] `warp_route.ak` reduced to 2 parameters (`mailbox_policy_id`, `processed_messages_nft_policy`)
- [x] Registry contract removed; relayer uses `RecipientResolver`
- [x] Warp routes use `0x01000000 || nft_policy_id` format (32 bytes)
- [x] Generic recipients use `0x02000000 || script_hash` format (32 bytes)
- [x] Mailbox conditionally mints verified_message_nft only for `0x02` recipients
- [x] All warp routes share the same script address, differentiated by state NFTs
- [x] Shared reference script UTXO for all warp routes
- [x] Custom ISM support via `ism: Option<ScriptHash>` in datum
- [x] Recipient kind detection from address prefix (`0x01` = warp route, `0x02` = generic)
- [x] CLI updated for new deploy params and address format
- [x] Greeting contract tested end-to-end (Fuji → Cardano → greeting receive)

## Relationship to Task 4.4

This task and [Task 4.4 (NFT-Based Contract Identity)](./task-4.4-nft-identity.md) share a common philosophy: **NFT policies are stable identifiers, script hashes are implementation details.**

- **Task 4.1**: State NFT policy = recipient's Hyperlane address
- **Task 4.4**: Identity NFT policy = mailbox's stable reference for upgrades

Together, they establish a consistent "NFT-based identity" pattern for Cardano Hyperlane contracts.

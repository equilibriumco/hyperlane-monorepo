[← Epic 4: Advanced Features](./EPIC.md) | [Epics Overview](../README.md)

# Task 4.1: State NFT Policy as Hyperlane Address

**Status:** ⬜ Not Started
**Complexity:** High
**Depends On:** None
**Related:** [Task 4.4: NFT-Based Contract Identity](./task-4.4-nft-identity.md)

## Objective

Use the state NFT policy ID as the canonical Hyperlane address for all Cardano recipients, replacing the current script hash-based addressing. This provides O(1) lookups, simplifies warp route parameterization, and aligns with Cardano's NFT-based identity patterns.

## Background

### Current Approach

Currently, Cardano recipients use their **script hash** as their Hyperlane address:

1. Remote chains enroll `script_hash` as the recipient address
2. Messages arrive with `recipient = script_hash`
3. Registry lookups iterate through entries to find `script_hash` (O(n))
4. Warp routes require an unused `_state_nft_policy_id` parameter to force unique script hashes

### The Problem

**Script Hash Collision for Warp Routes:**

In Cardano, validators with identical bytecode produce identical script hashes. Warp routes are a template deployed multiple times with the same code. Without parameterization tricks, all warp routes would share the same address.

Current workaround in `warp_route.ak`:

```aiken
// _state_nft_policy_id is UNUSED in logic - only exists to make script hash unique
validator warp_route(mailbox_policy_id: PolicyId, _state_nft_policy_id: PolicyId) {
  // ...
}
```

**O(n) Registry Lookups:**

The registry stores entries in a list, requiring iteration to find a recipient:

```aiken
list.find(datum.registrations, fn(r) { r.script_hash == script_hash })
```

**Inconsistent with Cardano Idioms:**

Cardano uses NFTs as unique instance identifiers. Using script hash as identity fights against this pattern.

## Solution: State NFT Policy as Address

Every Cardano recipient already has a **state NFT** to identify its state UTXO. The state NFT policy is:

- Unique per deployment (one-shot minting from a specific UTXO)
- Already required for registry registration
- The natural "instance identifier" in Cardano's model

### New Approach

1. **Hyperlane address = `state_locator.policy_id`** (not `script_hash`)
2. Remote chains enroll the state NFT policy as the recipient address
3. Messages arrive with `recipient = state_nft_policy`
4. Relayer queries UTXO by state NFT (O(1) via asset query)
5. Warp routes no longer need `_state_nft_policy_id` parameter

### Benefits

| Aspect            | Before (script_hash)                           | After (state_nft_policy) |
| ----------------- | ---------------------------------------------- | ------------------------ |
| Lookup complexity | O(n) iteration                                 | O(1) asset query         |
| Warp route params | Needs unused `_state_nft_policy_id`            | Only `mailbox_policy_id` |
| Index NFTs        | Need separate index NFT (Task 4.1 original)    | State NFT IS the index   |
| Cardano alignment | Fights UTXO model                              | Native pattern           |
| Consistency       | Different for warp routes vs custom recipients | Same for all recipients  |

### How It Works

**Deployment Flow (same for all recipient types):**

```
1. Deploy recipient validator (warp route, generic, deferred, etc.)
2. Mint state NFT (one-shot policy, unique per instance)
3. Register in registry with state_locator
4. Enroll on remote chains: address = state_nft_policy_id
```

**Message Delivery Flow:**

```
1. Message arrives: recipient = 0xabc123... (state NFT policy)
2. Relayer queries: "UTXO containing NFT with policy 0xabc123..."
3. Blockfrost/indexer returns the exact UTXO (O(1))
4. Relayer gets script_hash from registry or UTXO address
5. Relayer builds and submits transaction
```

## Implementation

### 1. Registry Contract Changes

Update `registry.ak` to use `state_locator.policy_id` as the lookup key:

```aiken
// BEFORE: lookup by script_hash
expect Some(existing) =
  list.find(datum.registrations, fn(r) { r.script_hash == script_hash })

// AFTER: lookup by state NFT policy (the Hyperlane address)
expect Some(existing) =
  list.find(datum.registrations, fn(r) {
    r.state_locator.policy_id == hyperlane_address
  })
```

The `RecipientRegistration` type stays the same - `script_hash` is still needed for transaction building, but `state_locator.policy_id` becomes the lookup key.

### 2. Warp Route Contract Changes

Remove the unused `_state_nft_policy_id` parameter from `warp_route.ak`:

```aiken
// BEFORE
validator warp_route(mailbox_policy_id: PolicyId, _state_nft_policy_id: PolicyId) {
  // ...
}

// AFTER
validator warp_route(mailbox_policy_id: PolicyId) {
  // ...
}
```

All warp routes now share the same script hash. Uniqueness comes from the state NFT, not the script address.

### 3. CLI Changes

Update warp route deployment in `cardano/cli/src/commands/warp.rs`:

```rust
// BEFORE: Apply two parameters
let warp_route_applied = apply_validator_params(
    &ctx.contracts_dir,
    "warp_route",
    "warp_route",
    &[
        &hex::encode(&mailbox_param_cbor),
        &hex::encode(&state_nft_param_cbor),  // Remove this
    ],
)?;

// AFTER: Apply only mailbox parameter
let warp_route_applied = apply_validator_params(
    &ctx.contracts_dir,
    "warp_route",
    "warp_route",
    &[&hex::encode(&mailbox_param_cbor)],
)?;
```

Update remote route enrollment to use state NFT policy:

```rust
// The Hyperlane address to enroll on remote chains
let hyperlane_address = state_nft_policy_id;  // Not script_hash
```

### 4. Relayer Changes

Update `rust/main/chains/hyperlane-cardano/src/registry.rs` for O(1) lookups:

```rust
// BEFORE: Iterate through registry entries
async fn get_recipient_info(&self, script_hash: &[u8]) -> Result<RecipientRegistration> {
    let registry = self.fetch_registry_datum().await?;
    registry.registrations
        .iter()
        .find(|r| r.script_hash == script_hash)
        .ok_or(Error::NotFound)
}

// AFTER: Query by state NFT directly
async fn get_recipient_utxo(&self, hyperlane_address: &[u8]) -> Result<Utxo> {
    // hyperlane_address IS the state NFT policy ID
    // Query Blockfrost for UTXO containing this NFT
    self.blockfrost
        .get_utxo_by_asset(hyperlane_address, "")  // Empty asset name for state NFT
        .await
}
```

### 5. Remote Route Enrollment

When enrolling Cardano warp routes on other chains (EVM, etc.), use the state NFT policy:

```solidity
// On EVM side, enrolling Cardano warp route
warpRoute.enrollRemoteRouter(
    cardanoDomain,
    bytes32(cardanoStateNftPolicyId)  // Not script hash
);
```

## Files to Modify

| File                                                    | Changes                                 |
| ------------------------------------------------------- | --------------------------------------- |
| `cardano/contracts/validators/warp_route.ak`            | Remove `_state_nft_policy_id` parameter |
| `cardano/contracts/validators/registry.ak`              | Lookup by `state_locator.policy_id`     |
| `cardano/cli/src/commands/warp.rs`                      | Update parameterization and enrollment  |
| `cardano/cli/src/commands/registry.rs`                  | Update registration flow                |
| `rust/main/chains/hyperlane-cardano/src/registry.rs`    | O(1) asset-based lookups                |
| `rust/main/chains/hyperlane-cardano/src/mailbox.rs`     | Use state NFT policy as address         |
| `rust/main/chains/hyperlane-cardano/src/trait_impls.rs` | Update address resolution               |

## Testing

| Test Case                  | Expected Result                                  |
| -------------------------- | ------------------------------------------------ |
| Deploy warp route (new)    | Only `mailbox_policy_id` parameter               |
| Register recipient         | `state_locator.policy_id` used as key            |
| Lookup by state NFT policy | O(1) - returns correct registration              |
| Message delivery           | Relayer finds UTXO via asset query               |
| Remote enrollment          | Uses state NFT policy, not script hash           |
| Multiple warp routes       | All share same script hash, different state NFTs |

## Definition of Done

- [ ] `warp_route.ak` has single parameter (`mailbox_policy_id` only)
- [ ] Registry lookups use `state_locator.policy_id` as key
- [ ] Relayer performs O(1) lookups via asset queries
- [ ] CLI uses state NFT policy for remote enrollments
- [ ] All recipient types use consistent addressing pattern
- [ ] E2E tests pass with new addressing scheme

## Acceptance Criteria

1. Warp routes deploy without `_state_nft_policy_id` parameter
2. Multiple warp routes can coexist (same script hash, different state NFTs)
3. Registry lookups are O(1) via state NFT policy
4. Remote chains enroll state NFT policy as Cardano address
5. Message delivery works end-to-end with new addressing
6. Pattern is consistent across warp routes, generic, and deferred recipients

## Relationship to Task 4.4

This task and [Task 4.4 (NFT-Based Contract Identity)](./task-4.4-nft-identity.md) share a common philosophy: **NFT policies are stable identifiers, script hashes are implementation details.**

- **Task 4.1**: State NFT policy = recipient's Hyperlane address
- **Task 4.4**: Identity NFT policy = mailbox's stable reference for upgrades

Together, they establish a consistent "NFT-based identity" pattern for Cardano Hyperlane contracts.

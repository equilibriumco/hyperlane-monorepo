[← Epic 4: Advanced Features](./EPIC.md) | [Epics Overview](../README.md)

# Task 4.1: NFT-Based O(1) Lookups
**Status:** ⬜ Not Started
**Complexity:** High
**Depends On:** None

## Objective

Implement NFT-based recipient lookups for O(1) query performance.

## Background

**File:** `rust/main/chains/hyperlane-cardano/src/blockfrost_provider.rs:51`

Contains a TODO comment about migrating to NFT-based tracking.

## Problem

Current approach requires O(n) iteration through the registry dictionary to find a recipient. With many recipients, this becomes slow.

## Solution

Mint an NFT with the script hash as the token name when registering. Then query by asset ID directly via Blockfrost API (O(1) lookup).

### On-Chain Changes

Modify registry registration to mint an index NFT:
- Token name = recipient's script hash
- Policy ID = registry's NFT policy
- Amount = 1

### Off-Chain Changes

Add fast lookup method:
- Build asset ID from registry policy + script hash
- Query Blockfrost asset API directly
- Parse recipient info from the UTXO containing the NFT

## Migration

For existing registrations without NFT:
- Fall back to dictionary lookup
- Provide CLI command to mint index NFTs for existing recipients

## Trade-offs

**Pros:**
- O(1) lookup time
- Works with Blockfrost asset API
- Scales to thousands of recipients

**Cons:**
- Additional NFT per registration
- Migration needed for existing recipients
- Slightly higher registration cost

## Files to Modify

| File | Changes |
|------|---------|
| `cardano/contracts/validators/registry.ak` | NFT minting on register |
| `rust/main/chains/hyperlane-cardano/src/registry.rs` | NFT-based lookup |
| `rust/main/chains/hyperlane-cardano/src/blockfrost_provider.rs` | Asset queries |

## Testing

- New registration mints NFT
- Lookup by NFT works
- Fallback for legacy recipients works

## Definition of Done

- [ ] Registry mints index NFTs on registration
- [ ] O(1) lookup implemented
- [ ] Migration path documented
- [ ] Performance improvement measurable

## Acceptance Criteria

1. New recipients get index NFT
2. Lookup time is constant regardless of registry size
3. Backwards compatible with existing recipients

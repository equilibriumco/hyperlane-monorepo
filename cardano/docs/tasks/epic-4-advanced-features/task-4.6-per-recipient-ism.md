[← Epic 4: Advanced Features](./EPIC.md) | [Epics Overview](../README.md)

# Task 4.7: Per-Recipient ISM Overrides

**Status:** ⬜ Not Started
**Complexity:** Medium
**Depends On:** Task 4.1 (NFT Lookups)

## Objective

Honor per-recipient ISM overrides on Cardano during `process`.

## Current State

- `mailbox.ak` always uses `default_ism`.
- Rust `RecipientResolver` already extracts `ism` from WarpRoute datum, but on-chain validation ignores it.

## Scope

- On-chain: use recipient-specific ISM when present.
- Off-chain: ensure relayer builds tx with correct ISM input/ref.

## Implementation

1. **Define ISM override source**
   - Warp routes: `WarpRouteDatum.ism` (already present) is the override.
   - Generic recipients: define a recipient registry datum or keep default only.

2. **Mailbox contract** (`cardano/contracts/validators/mailbox.ak`)
   - Update `get_recipient_ism` to:
     - For warp routes: read recipient state UTXO datum and use `ism` if set.
     - For generic recipients: fall back to `default_ism` (unless registry chosen).

3. **Tx builder** (`rust/main/chains/hyperlane-cardano/src/tx_builder.rs`)
   - When `ResolvedRecipient.ism` is Some, include that ISM UTXO as input (or reference input if Task 4.5 done).
   - Fail fast if override set but ISM UTXO missing.

4. **Recipient resolver** (`rust/main/chains/hyperlane-cardano/src/recipient_resolver.rs`)
   - Confirm `ism` extraction from WarpRoute datum is correct for all token types.

5. **Tests**
   - Aiken: mailbox uses override when present; default when absent.
   - Rust: builds tx with override ISM; errors when missing.

## Acceptance Criteria

- Warp route with `ism` override uses that ISM on-chain.
- Warp route without `ism` uses mailbox `default_ism`.
- Generic recipient uses `default_ism` (unless registry path implemented).
- Relayer fails if override set but ISM input not provided.

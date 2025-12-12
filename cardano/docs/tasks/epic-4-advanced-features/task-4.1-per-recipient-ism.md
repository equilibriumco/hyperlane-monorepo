[← Epic 4: Advanced Features](./EPIC.md) | [Epics Overview](../README.md)

# Task 4.1: Per-Recipient ISM
**Status:** ⬜ Not Started
**Complexity:** Medium
**Depends On:** None

## Objective

Enable recipients to specify their own ISM instead of using only the mailbox's default ISM.

## Current State

**File:** `cardano/contracts/validators/mailbox.ak:290-298`

The `get_recipient_ism` function always returns the default ISM, ignoring the recipient's custom ISM setting stored in the registry.

## Requirements

### 1. On-Chain Implementation

Modify `get_recipient_ism` to:
- Accept the registry as a reference input
- Look up the recipient in the registry
- Return the recipient's custom ISM if set
- Fall back to default ISM if not set or recipient not found

### 2. Update Process Validation

Modify `validate_process` to:
- Require registry as reference input
- Pass registry to `get_recipient_ism()`
- Verify the correct ISM script is in the transaction

### 3. Off-Chain Transaction Builder

Update the transaction builder to:
- Query recipient's custom ISM from registry
- Include the correct ISM reference script in the transaction
- Fall back to default ISM if not set

## Technical Notes

The registry already stores `custom_ism: Option<ScriptHash>` per recipient registration. The ISM selection flow is:

1. Message arrives for recipient
2. Mailbox looks up recipient in registry
3. If recipient has custom_ism set, use it
4. Otherwise use mailbox.default_ism
5. Verify ISM script approves message

The registry must be included as a reference input when processing messages (adds ~0.1 ADA cost).

## Files to Modify

| File | Changes |
|------|---------|
| `cardano/contracts/validators/mailbox.ak` | ISM lookup, process validation |
| `rust/main/chains/hyperlane-cardano/src/tx_builder.rs` | Transaction building |

## Testing

- Recipient with custom ISM → uses custom ISM
- Recipient without custom ISM → uses default ISM
- Recipient not in registry → uses default ISM
- Wrong ISM in transaction → rejected

## Definition of Done

- [ ] Mailbox looks up recipient's ISM from registry
- [ ] Transaction builder uses correct ISM
- [ ] All tests pass
- [ ] No regression in existing flow

## Acceptance Criteria

1. Custom ISM honored when set
2. Default ISM used as fallback
3. No regression in existing functionality

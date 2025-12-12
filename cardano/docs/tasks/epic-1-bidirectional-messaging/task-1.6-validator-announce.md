[← Epic 1: Bidirectional Messaging](./EPIC.md) | [Epics Overview](../README.md)

# Task 1.6: Validator Announce
**Status:** ⬜ Not Started
**Complexity:** Low-Medium
**Depends On:** [Task 1.4](./task-1.4-validator-config.md)

## Objective

Implement validator announcement functionality so that validators register their checkpoint storage locations on-chain, enabling relayers to discover where to fetch signed checkpoints.

## Background

Per the Hyperlane specification, validators must announce themselves by writing to the `ValidatorAnnounce` contract on their origin chain. This announcement includes:
- Validator's signing address (Ethereum address derived from their key)
- Storage location URL (e.g., S3 bucket URL where checkpoints are stored)

Relayers query this contract to discover all validators and their storage locations, then fetch checkpoints from those locations.

## Current State

**Implemented:**
- `ValidatorAnnounce` contract exists (`contracts/validators/validator_announce.ak`)
- `CardanoValidatorAnnounce` Rust implementation exists (`rust/main/chains/hyperlane-cardano/src/validator_announce.rs`)

**Missing:**
- CLI command to announce a validator
- Integration with validator agent startup
- Testing of announcement flow

## Requirements

### 1. CLI Command for Validator Announcement

```
hyperlane-cardano validator announce \
  --storage-location "s3://bucket-name/cardano-validator" \
  --signing-key /path/to/key.json
```

Should:
- Derive Ethereum address from signing key
- Build transaction with Announce redeemer
- Submit to ValidatorAnnounce contract
- Return transaction hash

### 2. Validator Agent Auto-Announce

When the validator agent starts with Cardano as origin:
- Check if already announced (query contract)
- If not announced, submit announcement transaction
- Log announcement status

This ensures validators are discoverable without manual intervention.

### 3. Relayer Discovery Integration

Verify that the relayer can:
- Query ValidatorAnnounce contract for announced validators
- Parse storage locations from announcements
- Fetch checkpoints from discovered locations

## ValidatorAnnounce Contract Interface

```
Announce redeemer:
  - validator: ByteArray (20-byte Ethereum address)
  - storage_location: ByteArray (URL as bytes)

Contract validates:
  - Transaction is signed by a key that derives to the validator address
  - Storage location is non-empty
  - Updates datum with new announcement
```

## Files to Modify

| File | Changes |
|------|---------|
| `cardano/cli/src/commands/validator.rs` | New file with announce command |
| `cardano/cli/src/commands/mod.rs` | Export validator module |
| `cardano/cli/src/main.rs` | Wire up command |
| `rust/main/agents/validator/src/` | Auto-announce on startup (optional) |

## Testing

### Unit Tests
- Ethereum address derivation from signing key
- Announcement transaction building
- Storage location parsing

### Integration Tests
- Announce validator on testnet
- Query announcements from contract
- Verify relayer can discover announced validators

## Definition of Done

- [ ] CLI `validator announce` command implemented
- [ ] Announcement transaction succeeds on testnet
- [ ] Relayer discovers announced validators
- [ ] Checkpoints fetched from announced storage location
- [ ] Documentation updated

## Acceptance Criteria

1. Validator can announce storage location via CLI
2. Announcement persists on-chain
3. Relayer discovers validators without manual configuration
4. Multiple validators can announce to same contract

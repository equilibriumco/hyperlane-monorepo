[← Epic 1: Bidirectional Messaging](./EPIC.md) | [Epics Overview](../README.md)

# Task 1.6: Validator Announce
**Status:** ✅ Complete
**Complexity:** Low-Medium
**Depends On:** [Task 1.4](./task-1.4-validator-config.md)

## Objective

Implement validator announcement functionality so that validators register their checkpoint storage locations on-chain, enabling relayers to discover where to fetch signed checkpoints.

## Background

Per the Hyperlane specification, validators must announce themselves by writing to the `ValidatorAnnounce` contract on their origin chain. This announcement includes:
- Validator's signing address (32-byte padded verification key hash)
- Storage location URL (e.g., S3 bucket URL where checkpoints are stored)

Relayers query this contract to discover all validators and their storage locations, then fetch checkpoints from those locations.

## Implementation Summary

### CLI Commands

Added `validator` command group with two subcommands:

```bash
# Announce validator storage location
./cli/target/release/hyperlane-cardano validator announce \
  --storage-location "s3://bucket-name/cardano-validator" \
  [--signing-key /path/to/key.skey] \
  [--dry-run]

# Show validator announcements
./cli/target/release/hyperlane-cardano validator show \
  [--validator <pubkey-hex>]
```

### Command Options

| Option | Description | Default |
|--------|-------------|---------|
| `--storage-location` | Storage location URL (e.g., S3 bucket) | Required |
| `--signing-key` | Path to signing key | From env CARDANO_SIGNING_KEY |
| `--dry-run` | Preview without submitting | false |
| `--validator` | Filter by validator pubkey (show command) | None |

### Implementation Details

1. **Script Parametrization**: The ValidatorAnnounce script is parametrized with:
   - `mailbox_policy_id`: The mailbox state NFT policy ID
   - `mailbox_domain`: The local Cardano domain ID (e.g., 2003)

2. **ValidatorAnnounceDatum Structure**:
   - `validator_pubkey`: 32 bytes (4 zero bytes + 28-byte verification key hash)
   - `mailbox_policy_id`: 28 bytes
   - `mailbox_domain`: Integer
   - `storage_location`: UTF-8 encoded URL as bytes

3. **Announce Redeemer**:
   - `Announce { storage_location: ByteArray }` - Constr 0

4. **Transaction Flow**:
   - For new announcements: Creates a seed UTXO at script address, then spends it with Announce redeemer
   - For updates: Spends existing announcement UTXO, creates continuation with updated storage location
   - Validator signature required (tx must include signer's verification key hash)

### Files Modified

| File | Changes |
|------|---------|
| `cardano/cli/src/commands/validator.rs` | New file with announce and show commands |
| `cardano/cli/src/commands/mod.rs` | Export validator module |
| `cardano/cli/src/main.rs` | Wire up validator command |

## Requirements

### 1. CLI Command for Validator Announcement

```bash
hyperlane-cardano validator announce \
  --storage-location "s3://bucket-name/cardano-validator" \
  --signing-key /path/to/key.skey
```

Should:
- Build validator pubkey (4 zero bytes + 28-byte verification key hash)
- Parametrize ValidatorAnnounce script with mailbox policy and domain
- Build transaction with Announce redeemer
- Submit to ValidatorAnnounce contract
- Return transaction hash

### 2. Validator Agent Auto-Announce (Future Enhancement)

When the validator agent starts with Cardano as origin:
- Check if already announced (query contract)
- If not announced, submit announcement transaction
- Log announcement status

This ensures validators are discoverable without manual intervention.

### 3. Relayer Discovery Integration

The relayer can:
- Query ValidatorAnnounce contract for announced validators via `CardanoValidatorAnnounce::get_announced_storage_locations()`
- Parse storage locations from announcement datums
- Fetch checkpoints from discovered locations

## ValidatorAnnounce Contract Interface

```
ValidatorAnnounceDatum:
  - validator_pubkey: ByteArray (32 bytes - padded verification key hash)
  - mailbox_policy_id: PolicyId (28 bytes)
  - mailbox_domain: Int
  - storage_location: ByteArray (URL as UTF-8 bytes)

Announce redeemer:
  - Announce { storage_location: ByteArray } - Creates or updates announcement

Contract validates:
  - Storage location is non-empty
  - For new: datum fields properly formed, validator signed tx
  - For update: only storage_location changed, validator signed tx
```

## Testing

### Tested on Preview Testnet

Successfully created and updated validator announcements:

**Create Announcement:**
- Transaction: `46bfd2caa51902460a23369e395898ed3f247eeacc185ba081d90ab760e27a97`
- Validator: `000000001212a023380020f8c7b94b831e457b9ee65f009df9d1d588430dcc89`
- Storage: `s3://test-bucket/cardano-validator`

**Update Announcement:**
- Transaction: `2e792decb62913fcf571165fea245b9fb70eae8208077358757c3c47cb5b2367`
- Storage: `s3://new-bucket/cardano-validator-v2`

**Script Address:** `addr_test1wpegc0kvdmayfy4ql5jt99ply8r4zvmndjc7kz9k7q5dyvg3zu67n`

## Definition of Done

- [x] CLI `validator announce` command implemented
- [x] Announcement transaction succeeds on testnet
- [x] `validator show` command displays announcements
- [x] Update flow works (spending existing announcement)
- [x] Documentation updated
- [ ] Relayer discovers announced validators (existing implementation in `CardanoValidatorAnnounce`)
- [ ] Validator agent auto-announce on startup (future enhancement)

## Acceptance Criteria

1. ✅ Validator can announce storage location via CLI
2. ✅ Announcement persists on-chain
3. ✅ Announcements can be updated with new storage location
4. ✅ Show command displays current announcements
5. ✅ Works with Cardano Preview testnet

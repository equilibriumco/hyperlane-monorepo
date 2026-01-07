[← Epic 1: Bidirectional Messaging](./EPIC.md) | [Epics Overview](../README.md)

# Task 1.6: Validator Announce
**Status:** ✅ Complete
**Complexity:** Low-Medium
**Depends On:** [Task 1.4](./task-1.4-validator-config.md)

## Objective

Implement validator announcement functionality so that validators register their checkpoint storage locations on-chain, enabling relayers to discover where to fetch signed checkpoints.

## Background

Per the Hyperlane specification, validators must announce themselves by writing to the `ValidatorAnnounce` contract on their origin chain. This announcement includes:
- Validator's Ethereum address (20 bytes derived from secp256k1 public key)
- Storage location URL (e.g., S3 bucket URL where checkpoints are stored)

Relayers query this contract to discover all validators and their storage locations, then fetch checkpoints from those locations.

**Cross-Chain Interoperability:**
Validators use secp256k1 keys (the same cryptography as Ethereum) to ensure their identity is consistent across all chains. This allows ISMs on any chain to verify checkpoints using the same validator public keys.

## Implementation Summary

### CLI Commands

Added `validator` command group with two subcommands:

```bash
# Announce validator storage location
./cli/target/release/hyperlane-cardano validator announce \
  --storage-location "s3://bucket-name/cardano-validator" \
  --validator-key "0x<secp256k1-private-key-hex>" \
  [--signing-key /path/to/cardano.skey] \
  [--dry-run]

# Show validator announcements
./cli/target/release/hyperlane-cardano validator show \
  [--validator <eth-address-hex>]
```

### Command Options

| Option | Description | Default |
|--------|-------------|---------|
| `--storage-location` | Storage location URL (e.g., S3 bucket) | Required |
| `--validator-key` | secp256k1 private key (hex with 0x prefix) | From env HYPERLANE_VALIDATOR_KEY |
| `--signing-key` | Cardano signing key (for tx fees) | From env CARDANO_SIGNING_KEY |
| `--dry-run` | Preview without submitting | false |
| `--validator` | Filter by validator address (show command) | None |

### Implementation Details

1. **Script Parametrization**: The ValidatorAnnounce script is parametrized with:
   - `mailbox_policy_id`: The mailbox state NFT policy ID
   - `mailbox_domain`: The local Cardano domain ID (e.g., 2003)

2. **ValidatorAnnounceDatum Structure**:
   - `validator_address`: 20 bytes (Ethereum address derived from secp256k1 pubkey)
   - `mailbox_policy_id`: 28 bytes
   - `mailbox_domain`: Integer
   - `storage_location`: UTF-8 encoded URL as bytes

3. **Announce Redeemer**:
   - `Announce { storage_location, compressed_pubkey, uncompressed_pubkey, signature }` - Constr 0
   - `Revoke { compressed_pubkey, uncompressed_pubkey, signature }` - Constr 1

4. **ECDSA Signature Verification**:
   - Contract verifies ECDSA secp256k1 signature using CIP-49 builtin
   - Announcement digest computed as: `keccak256(EIP-191 prefix || keccak256(domain_hash || storage_location))`
   - Domain hash: `keccak256(domain_bytes || mailbox_address || "HYPERLANE_ANNOUNCEMENT")`

5. **Transaction Flow**:
   - For new announcements: Creates a seed UTXO at script address, then spends it with Announce redeemer
   - For updates: Spends existing announcement UTXO, creates continuation with updated storage location
   - Signature verification proves validator authorization (no Cardano tx signer check needed)

### Files Modified

| File | Changes |
|------|---------|
| `cardano/contracts/lib/types.ak` | Updated ValidatorAnnounceDatum with 20-byte address |
| `cardano/contracts/validators/validator_announce.ak` | ECDSA signature verification |
| `cardano/cli/src/commands/validator.rs` | secp256k1 signing and verification |
| `cardano/cli/Cargo.toml` | Added k256 and tiny-keccak dependencies |

## Requirements

### 1. CLI Command for Validator Announcement

```bash
hyperlane-cardano validator announce \
  --storage-location "s3://bucket-name/cardano-validator" \
  --validator-key "0x0123456789abcdef..."
```

Should:
- Derive Ethereum address from secp256k1 public key
- Compute announcement digest matching EVM format
- Sign digest with ECDSA secp256k1
- Build transaction with Announce redeemer containing signature and public keys
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
  - validator_address: ByteArray (20 bytes - Ethereum address)
  - mailbox_policy_id: PolicyId (28 bytes)
  - mailbox_domain: Int
  - storage_location: ByteArray (URL as UTF-8 bytes)

Announce redeemer:
  - Announce { storage_location, compressed_pubkey, uncompressed_pubkey, signature }

Contract validates:
  - Storage location is non-empty
  - Public key formats match (compressed x == uncompressed x)
  - ECDSA signature verifies against announcement digest
  - Ethereum address derived from pubkey matches datum
  - For update: validator address matches existing datum
```

## Testing

### Tested on Preview Testnet

Successfully created and updated validator announcements with ECDSA secp256k1:

**Create Announcement:**
- Transaction: `79484fd7e370b4ffbd0861d4b92f1b3ac68d0cad5fd87f62fa33910d4d0ca035`
- Validator Address: `0xfcad0b19bb29d4674531d6f115237e16afce377c`
- Storage: `s3://test-bucket/cardano-validator`

**Update Announcement:**
- Transaction: `dbdd64a8b5d146c470b33dc41994f9b79f39d92205aeb9c7de7ef8eb7090834c`
- Storage: `s3://new-bucket/cardano-validator-v2`

**Script Address:** `addr_test1wryqqxgdvgugr6jlf2tttn3g43n2dwhte96p67lcslxc5mgkc23az`

## Definition of Done

- [x] CLI `validator announce` command implemented
- [x] Announcement transaction succeeds on testnet
- [x] `validator show` command displays announcements
- [x] Update flow works (spending existing announcement)
- [x] ECDSA secp256k1 signature verification working
- [x] Cross-chain compatible Ethereum addresses
- [x] Documentation updated
- [ ] Relayer discovers announced validators (existing implementation in `CardanoValidatorAnnounce`)
- [ ] Validator agent auto-announce on startup (future enhancement)

## Acceptance Criteria

1. ✅ Validator can announce storage location via CLI
2. ✅ Announcement persists on-chain
3. ✅ Announcements can be updated with new storage location
4. ✅ Show command displays current announcements
5. ✅ Works with Cardano Preview testnet
6. ✅ Uses Ethereum-compatible validator addresses for cross-chain interoperability
7. ✅ ECDSA secp256k1 signature verification on-chain

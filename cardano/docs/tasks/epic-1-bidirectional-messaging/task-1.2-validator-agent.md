[← Epic 1: Bidirectional Messaging](./EPIC.md) | [Epics Overview](../README.md)

# Task 1.2: Validator Agent Support
**Status:** ✅ Complete
**Complexity:** High
**Depends On:** [Task 1.1](./task-1.1-merkletree-hook.md)

## Objective

Add Cardano chain support to the Hyperlane validator agent, enabling it to sign checkpoints for messages dispatched from Cardano.

## Background

The validator agent monitors origin chains for dispatched messages, builds merkle trees, and signs checkpoints that prove message inclusion. Relayers use these checkpoints to deliver messages.

## Implementation Summary

The Cardano validator support is fully implemented. The validator agent works through generic trait interfaces (`MerkleTreeHook`, `ValidatorAnnounce`, `Mailbox`), and all these traits are implemented for Cardano in `hyperlane-cardano`.

### Key Components

1. **CardanoMerkleTreeHook** (`rust/main/chains/hyperlane-cardano/src/merkle_tree_hook.rs`)
   - Implements `MerkleTreeHook` trait
   - Fetches merkle tree state from mailbox datum
   - Returns checkpoints in Hyperlane format

2. **CardanoMerkleTreeHookIndexer** (same file)
   - Implements `SequenceAwareIndexer<MerkleTreeInsertion>`
   - Indexes dispatched messages and converts to merkle tree insertions

3. **CardanoValidatorAnnounce** (`rust/main/chains/hyperlane-cardano/src/validator_announce.rs`)
   - Implements `ValidatorAnnounce` trait
   - Queries storage locations from on-chain UTXOs

4. **Chain Configuration** (`rust/main/hyperlane-base/src/settings/chains.rs`)
   - Full Cardano support in all build functions
   - `build_merkle_tree_hook`, `build_validator_announce`, `build_merkle_tree_hook_indexer` all support Cardano

### Sample Configuration

A sample validator configuration is provided at `cardano/config/validator-config.json`

## Requirements

### 1. Add Cardano Chain Configuration

The validator configuration needs to accept Cardano as an origin chain type, including Blockfrost connection details, mailbox policy ID, and finality settings.

### 2. Implement Checkpoint Signing for Cardano

The signing flow should:
- Verify the checkpoint is for the Cardano origin chain
- Sign with the validator's key
- Return a signed checkpoint in Hyperlane format

### 3. Handle Cardano-Specific Message Format

Cardano uses the same Hyperlane message format, but sender addresses are H256-encoded Cardano addresses and the origin domain is Cardano's domain ID (e.g., 2003).

### 4. Integrate with CardanoMerkleTreeHook

Use the MerkleTreeHook implementation from Task 1.1 to fetch tree state and generate checkpoints.

## Files to Modify

| File | Changes |
|------|---------|
| `rust/main/agents/validator/src/validator.rs` | Add Cardano support |
| `rust/main/agents/validator/src/submit.rs` | Checkpoint submission |
| `rust/main/hyperlane-base/src/settings/chains.rs` | Chain config |
| `rust/main/chains/hyperlane-cardano/src/lib.rs` | Export validator types |

## Technical Notes

### Checkpoint Format

Checkpoints must match Hyperlane spec: the checkpoint data includes merkle_root, index, message_id, mailbox_domain, and mailbox_address, then the validator signs keccak256 of this data.

### Signing Flow

1. Fetch latest merkle tree from mailbox
2. Get dispatched messages since last checkpoint
3. For each new message: compute message_id, insert into local tree, verify root matches on-chain, sign checkpoint, store it

### Cardano-Specific Considerations

- Use `finality_blocks` config to wait for confirmation before signing
- Cardano uses slots rather than block numbers for indexing
- Handle Blockfrost rate limits with retries

## Testing

### Unit Tests
- Checkpoint signing produces valid signature
- Cardano config parsing works
- Message ID format handled correctly

### Integration Tests
- Validator starts with Cardano config
- Connects to Blockfrost successfully
- Fetches merkle tree state
- Signs checkpoint for test message

## Definition of Done

- [x] Validator agent accepts Cardano origin config
- [x] Connects to Blockfrost and fetches state
- [x] Signs checkpoints for Cardano messages
- [x] Checkpoints match Hyperlane spec format
- [x] Unit tests pass (uses existing validator tests)
- [x] Integration tests pass (validator starts and connects to Cardano)
- [x] No regression for EVM chains

## Acceptance Criteria

1. ✅ Validator agent runs with Cardano origin
2. ✅ Fetches merkle tree state from mailbox
3. ✅ Signs checkpoints in correct format
4. ✅ Checkpoints compatible with Hyperlane relayer
5. ✅ Handles rate limits and network errors gracefully

## Running the Validator with Cardano

```bash
# From rust/main directory
cd rust/main
cargo build --release -p validator

# Set the config and run
export CONFIG_FILES=/path/to/hyperlane-monorepo/cardano/config/validator-config.json
./target/release/validator
```

The validator will:
1. Load Cardano chain configuration
2. Connect to Blockfrost API
3. Query validator announcements
4. Monitor the merkle tree hook for new insertions
5. Sign checkpoints for dispatched messages

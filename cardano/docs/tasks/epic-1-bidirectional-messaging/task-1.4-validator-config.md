[← Epic 1: Bidirectional Messaging](./EPIC.md) | [Epics Overview](../README.md)

# Task 1.4: Validator Configuration
**Status:** ✅ Complete
**Complexity:** Low
**Depends On:** [Task 1.2](./task-1.2-validator-agent.md)

## Objective

Create configuration templates and CLI commands for running a Cardano validator agent.

## Background

Validators need proper configuration to connect to Cardano via Blockfrost, access mailbox contract state, store signed checkpoints, and manage signing keys.

## Implementation Summary

### CLI Command

Added `config update-validator` command to generate complete validator configuration from deployment info:

```bash
./cli/target/release/hyperlane-cardano config update-validator \
  --validator-key 0x<your-64-char-hex-key> \
  --checkpoint-path ./signatures \
  --db-path /tmp/hyperlane-validator-db
```

### Command Options

| Option | Description | Default |
|--------|-------------|---------|
| `--config-path` | Output config file path | `./config/validator-config.json` |
| `--chain-name` | Chain name in config | `cardano<network>` |
| `--validator-key` | Validator signing key (hex) | Required |
| `--checkpoint-path` | Checkpoint storage directory | `./signatures` |
| `--db-path` | Validator database path | `/tmp/hyperlane-validator-db` |
| `--metrics-port` | Prometheus metrics port | `9091` |
| `--index-from` | Block to start indexing from | Auto-detected |
| `--dry-run` | Preview config without writing | `false` |

### Documentation

Complete validator guide created at `cardano/docs/VALIDATOR_GUIDE.md` covering:
- Quick start instructions
- Configuration options
- Environment variables
- Validator lifecycle
- Checkpoint storage (local and S3)
- Validator announcement
- Monitoring and troubleshooting

## Requirements

### 1. Configuration File Template

Create `cardano/config/validator-config.json` with fields for:
- Origin chain name (cardano)
- Validator key configuration
- Checkpoint syncer settings (type: localStorage or S3, path)
- Reorg period and sync interval
- Chain-specific settings: Blockfrost URL, API key, mailbox policy ID and script hash, merkle tree hook config

### 2. CLI Command: Update Validator Config

Add a command to generate validator configuration from deployment info:

```
hyperlane-cardano config update-validator \
  --output ./validator-config.json \
  --checkpoint-path ./signatures \
  --validator-key ${VALIDATOR_KEY}
```

This should read `deployment_info.json` and produce a complete validator config.

### 3. Environment Variable Documentation

Document required environment variables:
- `BLOCKFROST_API_KEY` - Required for Cardano access
- `VALIDATOR_HEX_KEY` - Required for signing
- `CHECKPOINT_PATH` - Optional, defaults to ./signatures

### 4. Startup Validation

Validate configuration on startup:
- Check Blockfrost API key is set and valid format
- Verify mailbox policy ID is 56 hex characters
- Ensure checkpoint directory is writable

## Files to Create/Modify

| File | Changes |
|------|---------|
| `cardano/config/validator-config.json` | Template file |
| `cardano/cli/src/commands/config.rs` | Add update-validator command |
| `cardano/docs/VALIDATOR_GUIDE.md` | New documentation |

## Testing

### Unit Tests
- Config serialization/deserialization
- Environment variable substitution
- Validation logic

### Integration Tests
- Generate config from deployment info
- Validator starts with generated config

## Definition of Done

- [x] Configuration template created (`cardano/config/validator-config.json`)
- [x] CLI command generates valid config (`config update-validator`)
- [x] Environment variable documentation complete (`VALIDATOR_GUIDE.md`)
- [x] Validation catches common errors (key format, required fields)
- [x] Integration with validator agent works (tested with dry run and actual generation)

## Acceptance Criteria

1. ✅ Template config file works with validator agent
2. ✅ CLI generates config from deployment info
3. ✅ Environment variables properly substituted (VALIDATOR_HEX_KEY, BLOCKFROST_API_KEY)
4. ✅ Invalid configs rejected with clear error messages
5. ✅ Documentation complete for operators (`cardano/docs/VALIDATOR_GUIDE.md`)

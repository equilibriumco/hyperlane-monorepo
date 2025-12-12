[← Epic 1: Bidirectional Messaging](./EPIC.md) | [Epics Overview](../README.md)

# Task 1.4: Validator Configuration
**Status:** ⬜ Not Started
**Complexity:** Low
**Depends On:** [Task 1.2](./task-1.2-validator-agent.md)

## Objective

Create configuration templates and CLI commands for running a Cardano validator agent.

## Background

Validators need proper configuration to connect to Cardano via Blockfrost, access mailbox contract state, store signed checkpoints, and manage signing keys.

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

- [ ] Configuration template created
- [ ] CLI command generates valid config
- [ ] Environment variable documentation complete
- [ ] Validation catches common errors
- [ ] Integration with validator agent works

## Acceptance Criteria

1. Template config file works with validator agent
2. CLI generates config from deployment info
3. Environment variables properly substituted
4. Invalid configs rejected with clear error messages
5. Documentation complete for operators

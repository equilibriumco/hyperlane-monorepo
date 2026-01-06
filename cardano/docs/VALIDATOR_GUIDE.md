# Cardano Validator Guide

This guide explains how to run a Hyperlane validator for the Cardano chain.

## Overview

A Hyperlane validator monitors the Cardano mailbox for dispatched messages, signs checkpoints proving message inclusion in the merkle tree, and stores these checkpoints for relayers to fetch.

## Prerequisites

1. **Blockfrost API Key** - Get one from [blockfrost.io](https://blockfrost.io)
2. **Validator Signing Key** - A 32-byte hex-encoded private key (used for checkpoint signing)
3. **Funded Cardano Address** - For announcing the validator on-chain (minimum 3 ADA)

## Quick Start

### 1. Generate Configuration

Use the CLI to generate a validator configuration from deployment info:

```bash
cd cardano

# Generate config (replace with your validator key)
./cli/target/release/hyperlane-cardano config update-validator \
  --validator-key 0x<your-64-char-hex-key> \
  --checkpoint-path ./signatures \
  --db-path /tmp/hyperlane-validator-db
```

This reads `deployments/preview/deployment_info.json` and generates `config/validator-config.json`.

### 2. Set Environment Variables

```bash
# Required: Blockfrost API key
export BLOCKFROST_API_KEY=preview<your-api-key>

# Optional: Override validator key from config
export VALIDATOR_HEX_KEY=0x<your-64-char-hex-key>
```

### 3. Create Checkpoint Directory

```bash
mkdir -p ./signatures
```

### 4. Run the Validator

```bash
cd rust/main
cargo build --release -p validator

export CONFIG_FILES=/path/to/cardano/config/validator-config.json
./target/release/validator
```

## Configuration Options

### Command Line Options

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

### Environment Variables

| Variable | Description | Required |
|----------|-------------|----------|
| `BLOCKFROST_API_KEY` | Blockfrost API key for Cardano access | Yes |
| `VALIDATOR_HEX_KEY` | Validator signing key (alternative to CLI arg) | No |
| `CONFIG_FILES` | Path(s) to config file(s) | Yes (at runtime) |

### Configuration File Structure

```json
{
  "originChainName": "cardanopreview",
  "db": "/tmp/hyperlane-validator-db",
  "interval": 5,
  "maxSignConcurrency": 50,
  "validator": {
    "type": "hexKey",
    "key": "0x..."
  },
  "checkpointSyncer": {
    "type": "localStorage",
    "path": "./signatures"
  },
  "chains": {
    "cardanopreview": {
      "name": "cardanopreview",
      "domainId": 2003,
      "protocol": "cardano",
      "connection": {
        "url": "https://cardano-preview.blockfrost.io/api/v0",
        "apiKey": "",
        "network": "preview",
        "mailboxScriptHash": "...",
        "mailboxPolicyId": "...",
        ...
      },
      "mailbox": "0x02000000...",
      "signer": {
        "type": "hexKey",
        "key": "0x..."
      }
    }
  },
  "metricsPort": 9091
}
```

## Validator Lifecycle

1. **Startup**: Load configuration, connect to Blockfrost
2. **Announcement Check**: Query ValidatorAnnounce contract for existing announcement
3. **Self-Announce**: If not announced, submit announcement transaction (requires funded signer)
4. **Wait for Messages**: Poll merkle tree hook until messages exist
5. **Sync Messages**: Index dispatched messages from the mailbox
6. **Sign Checkpoints**: For each new message, sign checkpoint and store it
7. **Serve Checkpoints**: Checkpoints available for relayers to fetch

## Checkpoint Storage

### Local Storage

For testing and development:

```json
"checkpointSyncer": {
  "type": "localStorage",
  "path": "./signatures"
}
```

### S3 Storage

For production use:

```json
"checkpointSyncer": {
  "type": "s3",
  "bucket": "your-bucket-name",
  "region": "us-east-1"
}
```

## Validator Announcement

Before the validator can begin signing checkpoints, it must announce its storage location on-chain. This allows relayers to discover where to fetch checkpoints.

The validator agent attempts self-announcement automatically, but requires:
1. A `signer` configured for the origin chain
2. Sufficient ADA in the signer address (minimum 3 ADA)

If auto-announce fails, you'll see:
```
Cannot announce validator without a signer; make sure a signer is set for the origin chain
```

Or:
```
Please send tokens to your chain signer address to announce
```

## Monitoring

### Metrics

The validator exposes Prometheus metrics on the configured port (default: 9091):

- `hyperlane_latest_checkpoint` - Latest checkpoint index observed/processed
- `hyperlane_backfill_complete` - Whether historical checkpoint backfill is done
- `hyperlane_reached_initial_consistency` - Whether initial sync is complete

### Logs

Key log messages to watch:

```
INFO validator::validator: Checking for validator announcement
INFO hyperlane_cardano::validator_announce: Found N UTXOs at validator announce address
INFO validator::validator: Waiting for first message in merkle tree hook
INFO validator::submit: Signed and submitted checkpoint
```

## Troubleshooting

### "Mailbox not deployed"

Run the deployment commands first:
```bash
./cli/target/release/hyperlane-cardano deploy mailbox
./cli/target/release/hyperlane-cardano init mailbox
```

### "Invalid validator key format"

Ensure your key is:
- 32 bytes (64 hex characters)
- Has `0x` prefix
- Contains only valid hex characters (0-9, a-f)

### Rate Limiting

Blockfrost free tier allows 10 requests/second. The validator includes built-in rate limiting, but if you see 429 errors, consider:
- Using a paid Blockfrost plan
- Increasing the `interval` setting
- Reducing `maxSignConcurrency`

### Reorg Handling

The validator includes reorg detection. If a reorg is detected:
1. The validator will panic with a detailed error message
2. A reorg status file is written to checkpoint storage
3. Do NOT forcefully restart - investigate the cause first

## Security Considerations

1. **Protect your validator key** - This key signs checkpoints. Compromise could lead to invalid proofs.
2. **Secure checkpoint storage** - Use appropriate access controls for S3 buckets.
3. **Monitor validator uptime** - Offline validators delay message delivery.
4. **Use private RPCs** - Public RPCs may be unreliable or rate-limited.

## Network-Specific Settings

| Network | Domain ID | Blockfrost URL |
|---------|-----------|----------------|
| Preview | 2003 | https://cardano-preview.blockfrost.io/api/v0 |
| Preprod | 2002 | https://cardano-preprod.blockfrost.io/api/v0 |
| Mainnet | 2001 | https://cardano-mainnet.blockfrost.io/api/v0 |

# Cardano E2E Docker Setup

Docker Compose configuration for end-to-end testing of Hyperlane message relay between Cardano Preview testnet and Avalanche Fuji testnet.

## Overview

This setup runs two Hyperlane agents:
1. **Validator** - Signs checkpoints for messages dispatched from Cardano, stores them in AWS S3
2. **Relayer** - Bidirectional relay between Cardano Preview and Avalanche Fuji

## Prerequisites

- Docker and Docker Compose installed
- AWS account with S3 bucket configured for checkpoint storage
- Blockfrost API key for Cardano Preview
- Funded wallets on both chains for transaction fees
- Deployed Hyperlane contracts on both chains

## AWS S3 Setup

Follow the [Hyperlane AWS Validator Signatures guide](https://docs.hyperlane.xyz/docs/operate/validators/validator-signatures-aws) to set up your S3 bucket:

1. Create an S3 bucket (e.g., `hyperlane-validator-signatures-cardano-preview`)
2. Configure public read access for the bucket
3. Create an IAM user with write access to the bucket
4. Note your access key ID and secret access key

## Quick Start

1. Copy the example environment file:
   ```bash
   cp .env.example .env
   ```

2. Fill in your configuration values in `.env`:
   - AWS credentials and bucket name
   - `CARDANO_VALIDATOR_KEY` - ECDSA secp256k1 private key for validator signing
   - `CARDANO_SIGNER_KEY` - Ed25519 private key for Cardano transactions (Fuji->Cardano relay)
   - `BLOCKFROST_API_KEY` - Your Blockfrost Preview API key
   - `FUJI_SIGNER_KEY` - Private key for Fuji transaction signing
   - Contract addresses from your deployment

3. Build and start the services:
   ```bash
   docker compose build
   docker compose up -d
   ```

4. View logs:
   ```bash
   docker compose logs -f validator-cardano
   docker compose logs -f relayer
   ```

## Configuration

### Environment Variables

See `.env.example` for all required variables. Key configurations:

| Variable | Description |
|----------|-------------|
| `AWS_ACCESS_KEY_ID` | AWS access key for S3 |
| `AWS_SECRET_ACCESS_KEY` | AWS secret key for S3 |
| `AWS_REGION` | AWS region (e.g., us-east-1) |
| `AWS_S3_BUCKET` | S3 bucket name for checkpoints |
| `CARDANO_VALIDATOR_KEY` | Validator signing key (ECDSA secp256k1) |
| `CARDANO_SIGNER_KEY` | Cardano transaction signer (for Fuji->Cardano relay) |
| `BLOCKFROST_API_KEY` | Blockfrost API key for Cardano |
| `FUJI_SIGNER_KEY` | Key for signing Fuji transactions |
| `CARDANO_INDEX_FROM` | Starting block for Cardano indexing |
| `FUJI_INDEX_FROM` | Starting block for Fuji indexing |

### Contract Addresses

Cardano addresses use H256 format with `0x00000000` prefix:
```
0x00000000<28-byte-policy-id-hex>
```

For example, if your mailbox policy ID is `789ca889...`, the H256 address is:
```
0x00000000789ca889...
```

## Services

### Validator

- Port: 9090 (metrics)
- Data: `validator-cardano-data` volume
- Stores checkpoints in AWS S3

### Relayer

- Port: 9091 (metrics)
- Data: `relayer-data` volume
- Bidirectional: Cardano <-> Fuji

## Monitoring

Access Prometheus metrics:
- Validator: http://localhost:9090/metrics
- Relayer: http://localhost:9091/metrics

## Dispatching Messages

Once the services are running, you can dispatch messages from your terminal:

### Cardano -> Fuji
Use the Cardano CLI dispatch command:
```bash
cardano-cli dispatch --destination 43113 --recipient <fuji-recipient> --body "Hello Fuji"
```

### Fuji -> Cardano
Use Foundry cast or ethers to call the Fuji mailbox:
```bash
cast send $FUJI_MAILBOX "dispatch(uint32,bytes32,bytes)" 2003 <cardano-recipient> <message-body> --rpc-url $FUJI_RPC_URL --private-key $FUJI_SIGNER_KEY
```

## Troubleshooting

### View service status
```bash
docker compose ps
```

### Check logs
```bash
docker compose logs validator-cardano
docker compose logs relayer
```

### Restart services
```bash
docker compose restart
```

### Clean up and rebuild
```bash
docker compose down -v
docker compose build --no-cache
docker compose up -d
```

## E2E Test Flow

1. **Deploy contracts** on Cardano Preview and Fuji
2. **Configure AWS S3** bucket for checkpoint storage
3. **Announce validator** on Cardano with S3 bucket URL
4. **Start services** with docker compose
5. **Dispatch message** from either chain
6. **Monitor relayer** logs for message delivery
7. **Verify delivery** on destination chain

## Related Documentation

- [Hyperlane Documentation](https://docs.hyperlane.xyz/)
- [AWS Validator Signatures](https://docs.hyperlane.xyz/docs/operate/validators/validator-signatures-aws)
- [Cardano CLI Commands](../../cli/README.md)
- [Contract Deployment Guide](../../contracts/README.md)

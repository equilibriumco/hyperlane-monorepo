# Cardano E2E Docker Setup

Docker Compose configuration for end-to-end testing of Hyperlane message relay between Cardano Preview testnet and Avalanche Fuji testnet.

## Overview

This setup runs two Hyperlane agents:
1. **Validator** - Signs checkpoints for messages dispatched from Cardano
2. **Relayer** - Relays messages from Cardano to Fuji

## Prerequisites

- Docker and Docker Compose installed
- Blockfrost API key for Cardano Preview
- Funded Fuji wallet for gas fees
- Deployed Hyperlane contracts on both chains

## Quick Start

1. Copy the example environment file:
   ```bash
   cp .env.example .env
   ```

2. Fill in your configuration values in `.env`:
   - `VALIDATOR_KEY` - ECDSA secp256k1 private key for validator signing
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
   docker compose logs -f validator
   docker compose logs -f relayer
   ```

## Configuration

### Environment Variables

See `.env.example` for all required variables. Key configurations:

| Variable | Description |
|----------|-------------|
| `VALIDATOR_KEY` | Validator signing key (ECDSA secp256k1) |
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

- Port: 9093 (metrics)
- Data: `validator-data` volume
- Stores checkpoints in `/data/checkpoints`

### Relayer

- Port: 9094 (metrics)
- Data: `relayer-data` volume
- Reads validator checkpoints (mounted read-only)

## Monitoring

Access Prometheus metrics:
- Validator: http://localhost:9093/metrics
- Relayer: http://localhost:9094/metrics

## Troubleshooting

### View service status
```bash
docker compose ps
```

### Check logs
```bash
docker compose logs validator
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
2. **Announce validator** using Cardano CLI
3. **Start services** with docker compose
4. **Dispatch message** from Cardano using CLI
5. **Monitor relayer** logs for message delivery
6. **Verify delivery** on Fuji using explorer

## Related Documentation

- [Hyperlane Documentation](https://docs.hyperlane.xyz/)
- [Cardano CLI Commands](../../cli/README.md)
- [Contract Deployment Guide](../../contracts/README.md)

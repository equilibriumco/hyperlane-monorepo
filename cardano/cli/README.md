# Hyperlane Cardano CLI

A comprehensive CLI for deploying, initializing, and managing Hyperlane smart contracts on Cardano.

## Installation

```bash
cd cardano/cli
cargo build --release
```

The binary will be available at `target/release/hyperlane-cardano`.

## Prerequisites

- [Blockfrost API Key](https://blockfrost.io/) - Required for chain queries and transaction submission
- Cardano signing key (for transactions)
- Aiken-compiled contracts (`plutus.json`) - Built with `aiken build` in `cardano/contracts/`

## Configuration

The CLI can be configured via environment variables or command-line flags:

```bash
# Environment variables
export BLOCKFROST_API_KEY="your-api-key"
export CARDANO_NETWORK="preview"  # mainnet, preprod, preview
export CARDANO_SIGNING_KEY="./testnet-keys/payment.skey"

# Or use command-line flags
hyperlane-cardano --network preview --api-key <key> --signing-key <path> <command>
```

## Commands

### Deploy

Extract and prepare contract validators for deployment.

```bash
# Show all validators in plutus.json
hyperlane-cardano deploy info

# Extract validators and generate deployment artifacts
hyperlane-cardano deploy extract

# Extract specific validators only
hyperlane-cardano deploy extract --only mailbox,multisig_ism

# Generate deployment config
hyperlane-cardano deploy generate-config
```

### Initialize

Initialize contracts with state NFTs and initial datums.

```bash
# Check initialization status
hyperlane-cardano init status

# Generate datums for manual initialization
hyperlane-cardano init generate-datums --domain 2003 --ism-hash <hash>

# Initialize mailbox (dry run)
hyperlane-cardano init mailbox --domain 2003 --ism-hash <hash> --dry-run

# Initialize ISM
hyperlane-cardano init ism --domains "43113,421614" --dry-run

# Initialize all core contracts
hyperlane-cardano init all --domain 2003 --origin-domains "43113,421614" --dry-run
```

### ISM (Interchain Security Module)

Manage validator sets and thresholds.

```bash
# Show ISM configuration
hyperlane-cardano ism show --ism-policy <policy_id>

# Set validators for a domain
hyperlane-cardano ism set-validators \
  --domain 43113 \
  --validators 0xd8154f73d04cc7f7f0c332793692e6e6f6b2402e \
  --ism-policy <policy_id> \
  --signing-key payment.skey

# Set threshold for a domain
hyperlane-cardano ism set-threshold --domain 43113 --threshold 1

# Add a single validator
hyperlane-cardano ism add-validator \
  --domain 43113 \
  --validator 0x1234...

# Remove a validator
hyperlane-cardano ism remove-validator \
  --domain 43113 \
  --validator 0x1234...
```

### Registry

Manage recipient registrations.

```bash
# List registered recipients
hyperlane-cardano registry list

# Show recipient details
hyperlane-cardano registry show --script-hash <hash>

# Register a new recipient (dry run)
hyperlane-cardano registry register \
  --script-hash <hash> \
  --state-policy <policy_id> \
  --recipient-type generic \
  --dry-run

# Generate registration JSON
hyperlane-cardano registry generate-json \
  --script-hash <hash> \
  --state-policy <policy_id>
```

### Warp (Token Bridges)

Manage warp routes for cross-chain token transfers.

```bash
# Show warp route configuration
hyperlane-cardano warp show --warp-policy <policy_id>

# List enrolled remote routers
hyperlane-cardano warp routers

# Enroll a remote router
hyperlane-cardano warp enroll-router \
  --domain 43113 \
  --router 0x1234...5678 \
  --dry-run

# Initiate a transfer (dry run)
hyperlane-cardano warp transfer \
  --domain 43113 \
  --recipient 0x1234...5678 \
  --amount 1000000 \
  --dry-run

# Check vault balance
hyperlane-cardano warp vault-balance
```

### Query

Query chain state and contract data.

```bash
# Query mailbox state
hyperlane-cardano query mailbox

# Query ISM configuration
hyperlane-cardano query ism

# Query UTXOs at address
hyperlane-cardano query utxos <address>

# Query protocol parameters
hyperlane-cardano query params

# Query latest slot
hyperlane-cardano query tip

# Query transaction
hyperlane-cardano query tx <tx_hash>

# Query message processing status
hyperlane-cardano query message --message-id 0x1234...
```

### UTXO

UTXO management utilities.

```bash
# List wallet UTXOs
hyperlane-cardano utxo list

# List collateral-suitable UTXOs
hyperlane-cardano utxo list --collateral

# Find suitable UTXO
hyperlane-cardano utxo find --min-lovelace 10000000 --no-assets

# Split UTXO (dry run)
hyperlane-cardano utxo split --utxo "txhash#0" --count 5 --dry-run

# Consolidate UTXOs (dry run)
hyperlane-cardano utxo consolidate --max 10 --dry-run
```

### Transaction

Transaction building and submission.

```bash
# Submit a signed transaction
hyperlane-cardano tx submit tx.signed

# Submit and wait for confirmation
hyperlane-cardano tx submit tx.signed --wait --timeout 120

# Check transaction status
hyperlane-cardano tx status <tx_hash>

# Wait for confirmation
hyperlane-cardano tx wait <tx_hash> --timeout 300

# Evaluate transaction (get execution units)
hyperlane-cardano tx evaluate tx.raw

# Decode transaction
hyperlane-cardano tx decode tx.raw
```

### Shell Completions

Generate shell completions for better CLI experience.

```bash
# Bash
hyperlane-cardano completions bash > ~/.local/share/bash-completion/completions/hyperlane-cardano

# Zsh
hyperlane-cardano completions zsh > ~/.zfunc/_hyperlane-cardano

# Fish
hyperlane-cardano completions fish > ~/.config/fish/completions/hyperlane-cardano.fish
```

## Workflow Example

### 1. Deploy Contracts

```bash
# Build contracts first
cd cardano/contracts && aiken build && cd ../cli

# Extract validators
hyperlane-cardano deploy extract

# Generate config
hyperlane-cardano deploy generate-config
```

### 2. Initialize Contracts

```bash
# Generate datums
hyperlane-cardano init generate-datums \
  --domain 2003 \
  --ism-hash $(cat ../deployments/preview/multisig_ism.hash)

# Follow manual steps for initialization
# (Uses aiken blueprint apply + cardano-cli)
```

### 3. Configure ISM

```bash
# Set validators for Fuji (domain 43113)
hyperlane-cardano ism set-validators \
  --domain 43113 \
  --validators 0xd8154f73d04cc7f7f0c332793692e6e6f6b2402e \
  --threshold 1 \
  --ism-policy <ism_policy_id> \
  --signing-key ../testnet-keys/payment.skey
```

### 4. Verify Deployment

```bash
# Check initialization status
hyperlane-cardano init status

# Query mailbox state
hyperlane-cardano query mailbox

# Query ISM configuration
hyperlane-cardano ism show
```

## Output Files

The CLI generates artifacts in the deployments directory:

```
cardano/deployments/<network>/
├── deployment_info.json    # Contract addresses and policies
├── mailbox.plutus         # Mailbox validator script
├── mailbox.hash           # Mailbox script hash
├── mailbox.addr           # Mailbox script address
├── multisig_ism.plutus    # ISM validator script
├── multisig_ism.hash      # ISM script hash
├── multisig_ism.addr      # ISM script address
├── registry.plutus        # Registry validator script
├── mailbox_datum.json     # Initial mailbox datum
├── ism_datum.json         # Initial ISM datum
└── mint_redeemer.json     # State NFT mint redeemer
```

## Integration with Existing Tools

For operations that require full transaction building, the CLI provides:

1. CBOR-encoded redeemers and datums
2. Manual steps for cardano-cli integration
3. References to existing shell scripts

See `cardano/scripts/` for complete transaction examples.

## Troubleshooting

### "Blockfrost API key required"
Set the `BLOCKFROST_API_KEY` environment variable or use `--api-key`.

### "Signing key required"
Set the `CARDANO_SIGNING_KEY` environment variable or use `--signing-key`.

### "plutus.json not found"
Build the contracts first: `cd cardano/contracts && aiken build`

### "UTXO not found"
Ensure your wallet has funds. For testnet, use the Cardano faucet.

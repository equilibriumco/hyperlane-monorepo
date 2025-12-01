# Hyperlane Cardano Testing Guide

This guide covers end-to-end testing of cross-chain messaging between Cardano (Preprod/Preview) and EVM testnets (Sepolia or Avalanche Fuji).

## Prerequisites

### Tools Required

```bash
# Cardano tools
- Aiken CLI (v1.0+): https://aiken-lang.org/installation
- cardano-cli (for key generation and transaction building)
- A Blockfrost API key: https://blockfrost.io/

# EVM tools
- Foundry (forge, cast): https://getfoundry.sh/
- Node.js 18+ and yarn

# Rust
- Rust 1.75+ with cargo
```

### Testnet Tokens

| Network | Faucet |
|---------|--------|
| Cardano Preprod | https://docs.cardano.org/cardano-testnets/tools/faucet/ |
| Cardano Preview | https://docs.cardano.org/cardano-testnets/tools/faucet/ |
| Sepolia ETH | https://sepoliafaucet.com/ or https://www.alchemy.com/faucets/ethereum-sepolia |
| Avalanche Fuji | https://faucet.avax.network/ |

### Environment Variables

Create a `.env` file:

```bash
# Blockfrost API keys (get from blockfrost.io)
BLOCKFROST_API_KEY_PREPROD=preprodXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX
BLOCKFROST_API_KEY_PREVIEW=previewXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX

# EVM RPC endpoints
SEPOLIA_RPC_URL=https://eth-sepolia.g.alchemy.com/v2/YOUR_KEY
FUJI_RPC_URL=https://api.avax-test.network/ext/bc/C/rpc

# Private keys (NEVER commit these!)
CARDANO_SIGNING_KEY_PATH=/path/to/payment.skey
EVM_PRIVATE_KEY=0x...

# Validator keys (for signing checkpoints)
VALIDATOR_PRIVATE_KEY=0x...
```

---

## Part 1: Deploy Cardano Contracts

### 1.1 Build Aiken Contracts

```bash
cd /home/guilherme/workspace/eiger/hyperlane-cardano/contracts

# Build all validators
aiken build

# This generates:
# - plutus.json (contains all compiled validators)
# - artifacts/ (individual validator scripts)
```

### 1.2 Generate Cardano Keys

```bash
# Generate payment keys
cardano-cli address key-gen \
  --verification-key-file payment.vkey \
  --signing-key-file payment.skey

# Generate stake keys (optional, for staking addresses)
cardano-cli stake-address key-gen \
  --verification-key-file stake.vkey \
  --signing-key-file stake.skey

# Build address (preprod)
cardano-cli address build \
  --payment-verification-key-file payment.vkey \
  --testnet-magic 1 \
  --out-file payment.addr

# Get your address
cat payment.addr
# addr_test1qz...
```

### 1.3 Deploy Mailbox Contract

The mailbox is deployed by creating a UTXO at the script address with initial datum:

```bash
# Extract script hash from plutus.json
MAILBOX_HASH=$(jq -r '.validators[] | select(.title == "mailbox.mailbox") | .hash' plutus.json)

# Build script address
cardano-cli address build \
  --payment-script-file artifacts/mailbox.plutus \
  --testnet-magic 1 \
  --out-file mailbox.addr

# Initial datum (JSON format):
# {
#   "constructor": 0,
#   "fields": [
#     {"int": 2002},                    # local_domain (preprod)
#     {"bytes": "<ism_hash>"},          # default_ism (28 bytes hex)
#     {"bytes": "<owner_pkh>"},         # owner pubkey hash
#     {"int": 0},                        # outbound_nonce
#     {"bytes": "0000...0000"},         # merkle_root (32 bytes)
#     {"int": 0}                         # merkle_count
#   ]
# }

# Create the initial UTXO (using cardano-cli transaction build)
# Include a state NFT to mark the mailbox UTXO
```

### 1.4 Deploy IGP Contract

```bash
IGP_HASH=$(jq -r '.validators[] | select(.title == "igp.igp") | .hash' plutus.json)

# Initial datum:
# {
#   "constructor": 0,
#   "fields": [
#     {"bytes": "<owner_pkh>"},
#     {"bytes": "<beneficiary_addr>"},
#     {"list": []},                      # gas_oracles (empty initially)
#     {"int": 200000}                    # default_gas_limit
#   ]
# }
```

### 1.5 Deploy Validator Announce Contract

```bash
VA_HASH=$(jq -r '.validators[] | select(.title == "validator_announce.validator_announce") | .hash' plutus.json)

# This contract is parameterized with mailbox_policy_id and mailbox_domain
# Validators will create UTXOs at this address with their announcements
```

### 1.6 Record Deployed Addresses

Create `cardano-deployment.json`:

```json
{
  "network": "preprod",
  "domain": 2002,
  "contracts": {
    "mailbox": {
      "policyId": "abc123...",
      "address": "addr_test1..."
    },
    "ism": {
      "policyId": "def456...",
      "address": "addr_test1..."
    },
    "igp": {
      "policyId": "789abc...",
      "address": "addr_test1..."
    },
    "validatorAnnounce": {
      "policyId": "fedcba...",
      "address": "addr_test1..."
    }
  }
}
```

---

## Part 2: Deploy EVM Contracts (Sepolia/Fuji)

### 2.1 Clone and Build

```bash
cd /home/guilherme/workspace/eiger/hyperlane-monorepo/solidity

# Install dependencies
yarn install

# Build contracts
yarn build
```

### 2.2 Deploy Core Contracts

```bash
# Using Foundry
cd solidity

# Deploy Mailbox
forge script script/DeployMailbox.s.sol \
  --rpc-url $SEPOLIA_RPC_URL \
  --private-key $EVM_PRIVATE_KEY \
  --broadcast

# Or use Hyperlane CLI
npx @hyperlane-xyz/cli deploy core \
  --chain sepolia \
  --key $EVM_PRIVATE_KEY
```

### 2.3 Configure ISM for Cardano Origin

The ISM on Sepolia needs to know about Cardano validators:

```bash
# Deploy MultisigIsm with Cardano validators
forge script script/DeployMultisigIsm.s.sol \
  --rpc-url $SEPOLIA_RPC_URL \
  --private-key $EVM_PRIVATE_KEY \
  --broadcast \
  --sig "run(uint32,address[],uint8)" \
  2002 \                           # Cardano domain
  "[0x1234...,0x5678...]" \        # Validator addresses
  2                                 # Threshold
```

### 2.4 Record EVM Deployment

Create `sepolia-deployment.json`:

```json
{
  "network": "sepolia",
  "chainId": 11155111,
  "domain": 11155111,
  "contracts": {
    "mailbox": "0x...",
    "defaultIsm": "0x...",
    "igp": "0x...",
    "validatorAnnounce": "0x..."
  }
}
```

---

## Part 3: Configure and Run Validators

Validators sign checkpoints for messages. You need at least one validator for each origin chain.

### 3.1 Create Validator Config

Create `validator-cardano.json`:

```json
{
  "originChainName": "cardanopreprod",
  "checkpointSyncer": {
    "type": "localStorage",
    "path": "/tmp/cardano-validator-signatures"
  },
  "validator": {
    "type": "hexKey",
    "key": "0x..."
  },
  "reorgPeriod": 20,
  "interval": 30,
  "chains": {
    "cardanopreprod": {
      "name": "cardanopreprod",
      "domain": 2002,
      "protocol": "cardano",
      "rpcUrls": [{
        "http": "https://cardano-preprod.blockfrost.io/api/v0"
      }],
      "connection": {
        "type": "blockfrost",
        "apiKey": "${BLOCKFROST_API_KEY_PREPROD}"
      },
      "mailbox": {
        "policyId": "abc123..."
      }
    }
  }
}
```

### 3.2 Run Cardano Validator

```bash
cd /home/guilherme/workspace/eiger/hyperlane-monorepo/rust/main

# Build the validator binary
cargo build --release --bin validator

# Run validator
./target/release/validator \
  --config validator-cardano.json \
  --originChainName cardanopreprod
```

### 3.3 Announce Validator Storage Location

After starting the validator, announce its storage location:

```bash
# For Cardano, create a transaction that creates a UTXO at validator announce address
# The datum contains:
# - validator pubkey (32 bytes)
# - mailbox policy ID
# - domain (2002)
# - storage location URL (e.g., "file:///tmp/cardano-validator-signatures")

# Using the cardano_register tool:
cargo run --bin cardano_register -- \
  --script-hash $VALIDATOR_PUBKEY_HASH \
  --storage-location "file:///tmp/cardano-validator-signatures" \
  --network preprod
```

---

## Part 4: Configure and Run Relayer

The relayer watches for messages on origin chains and delivers them to destination chains.

### 4.1 Create Relayer Config

Create `relayer-config.json`:

```json
{
  "relaychains": "cardanopreprod,sepolia",
  "allowLocalCheckpointSyncers": true,
  "gasPaymentEnforcement": [{
    "type": "none"
  }],
  "chains": {
    "cardanopreprod": {
      "name": "cardanopreprod",
      "domain": 2002,
      "protocol": "cardano",
      "connection": {
        "type": "blockfrost",
        "url": "https://cardano-preprod.blockfrost.io/api/v0",
        "apiKey": "${BLOCKFROST_API_KEY_PREPROD}"
      },
      "addresses": {
        "mailbox": "abc123...",
        "ism": "def456...",
        "igp": "789abc...",
        "validatorAnnounce": "fedcba..."
      },
      "index": {
        "from": 0
      }
    },
    "sepolia": {
      "name": "sepolia",
      "domain": 11155111,
      "protocol": "ethereum",
      "rpcUrls": [{
        "http": "${SEPOLIA_RPC_URL}"
      }],
      "addresses": {
        "mailbox": "0x...",
        "defaultIsm": "0x...",
        "igp": "0x...",
        "validatorAnnounce": "0x..."
      },
      "index": {
        "from": 0
      }
    }
  }
}
```

### 4.2 Run Relayer

```bash
cd /home/guilherme/workspace/eiger/hyperlane-monorepo/rust/main

# Build relayer
cargo build --release --bin relayer

# Run relayer
./target/release/relayer \
  --config relayer-config.json \
  --db /tmp/relayer-db
```

---

## Part 5: Send Test Messages

### 5.1 Cardano → Sepolia (Outbound from Cardano)

Create a dispatch transaction on Cardano:

```bash
# The transaction must:
# 1. Spend the mailbox UTXO with Dispatch redeemer
# 2. Include new mailbox UTXO with updated nonce and merkle tree
# 3. Optionally pay for gas at IGP address

# Dispatch redeemer format:
# {
#   "constructor": 0,
#   "fields": [
#     {"int": 11155111},                           # destination (Sepolia)
#     {"bytes": "000000000000000000000000<recipient_addr>"},  # recipient (32 bytes)
#     {"bytes": "48656c6c6f"}                      # body ("Hello" in hex)
#   ]
# }

# Build and submit transaction using cardano-cli or a wallet
```

### 5.2 Sepolia → Cardano (Inbound to Cardano)

```bash
# Using cast (Foundry)
cast send $MAILBOX_ADDRESS \
  "dispatch(uint32,bytes32,bytes)" \
  2002 \                                          # destination (Cardano)
  0x000000000000000000000000<recipient_script_hash> \
  0x48656c6c6f \                                  # "Hello"
  --rpc-url $SEPOLIA_RPC_URL \
  --private-key $EVM_PRIVATE_KEY
```

### 5.3 Monitor Message Delivery

```bash
# Check relayer logs for:
# - "Indexed message" (origin chain)
# - "Fetching metadata" (getting validator signatures)
# - "Delivering message" (destination chain)

# On Cardano, check for Process transactions at mailbox address
# On Sepolia, check for MessageProcessed events
```

---

## Part 6: Troubleshooting

### Common Issues

#### 1. Validator Not Signing

```bash
# Check validator is running and syncing
# Verify checkpoint syncer path exists and is writable
# Check validator key matches announced address
```

#### 2. Relayer Not Delivering

```bash
# Check relayer has indexed the message:
# Look for "Indexed dispatched message" in logs

# Check ISM verification:
# Ensure validators are configured in destination ISM
# Verify threshold is met (enough signatures)

# Check gas:
# Relayer needs funds on destination chain
```

#### 3. Cardano Transaction Fails

```bash
# Common errors:
# - "InsufficientCollateral": Add more collateral UTXOs
# - "ScriptFailure": Check redeemer format and datum updates
# - "TxBodyScriptExecutionError": Check Plutus script logic
```

#### 4. Message ID Mismatch

```bash
# Message ID must be computed identically on both chains:
# id = keccak256(version || nonce || origin || sender || destination || recipient || body)

# Verify:
# - Version = 3
# - Nonce matches mailbox state
# - Domain IDs are correct (Cardano preprod = 2002)
# - Addresses are properly padded to 32 bytes
```

### Useful Commands

```bash
# Check Cardano mailbox state
curl -H "project_id: $BLOCKFROST_API_KEY_PREPROD" \
  "https://cardano-preprod.blockfrost.io/api/v0/addresses/<mailbox_addr>/utxos"

# Check Sepolia mailbox
cast call $MAILBOX_ADDRESS "nonce()(uint32)" --rpc-url $SEPOLIA_RPC_URL

# Query validator announcements on Cardano
curl -H "project_id: $BLOCKFROST_API_KEY_PREPROD" \
  "https://cardano-preprod.blockfrost.io/api/v0/addresses/<va_addr>/utxos"

# Check message delivery status on Sepolia
cast call $MAILBOX_ADDRESS \
  "delivered(bytes32)(bool)" \
  <message_id> \
  --rpc-url $SEPOLIA_RPC_URL
```

---

## Domain IDs Reference

| Network | Domain ID |
|---------|-----------|
| Cardano Mainnet | 2001 |
| Cardano Preprod | 2002 |
| Cardano Preview | 2003 |
| Ethereum Mainnet | 1 |
| Sepolia | 11155111 |
| Avalanche C-Chain | 43114 |
| Avalanche Fuji | 43113 |

---

## Next Steps

1. **Deploy a test recipient** on each chain to receive and log messages
2. **Set up monitoring** using Grafana/Prometheus for relayer metrics
3. **Configure gas payment enforcement** for production use
4. **Add more validators** for increased security

For production deployments, refer to the main Hyperlane documentation at https://docs.hyperlane.xyz/

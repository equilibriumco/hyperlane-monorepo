# Quick Start: Cardano ↔ Sepolia Testing

This is a condensed guide to test cross-chain messaging between Cardano Preprod and Sepolia.

## 1. Setup (10 minutes)

```bash
# Clone repos if not done
cd ~/workspace
git clone <hyperlane-monorepo>
git clone <hyperlane-cardano>  # Aiken contracts

# Install tools
curl -sSfL https://aiken-lang.org/install.sh | bash
curl -L https://foundry.paradigm.xyz | bash
foundryup

# Get testnet tokens
# - Cardano Preprod: https://docs.cardano.org/cardano-testnets/tools/faucet/
# - Sepolia: https://sepoliafaucet.com/
```

## 2. Environment

Create `.env`:

```bash
export BLOCKFROST_API_KEY=preprodXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX
export SEPOLIA_RPC_URL=https://eth-sepolia.g.alchemy.com/v2/YOUR_KEY
export EVM_PRIVATE_KEY=0x...
export CARDANO_SKEY_PATH=./payment.skey
```

## 3. Build Contracts

```bash
# Aiken (Cardano)
cd hyperlane-cardano/contracts
aiken build

# Solidity (Sepolia)
cd hyperlane-monorepo/solidity
yarn && yarn build
```

## 4. Deploy (Simplified)

### Cardano Side

For testing, you can use placeholder policy IDs initially. The actual deployment requires:

1. Mint state NFTs for each contract (mailbox, ISM, IGP)
2. Create initial UTXOs with correct datums
3. Record the policy IDs

### Sepolia Side

```bash
# Use Hyperlane CLI for quick deployment
npx @hyperlane-xyz/cli deploy core --chain sepolia
```

## 5. Run Infrastructure

### Terminal 1: Validator

```bash
cd hyperlane-monorepo/rust/main
cargo run --release --bin validator -- \
  --originChainName cardanopreprod \
  --checkpointSyncer.type localStorage \
  --checkpointSyncer.path /tmp/cardano-sigs
```

### Terminal 2: Relayer

```bash
cd hyperlane-monorepo/rust/main
cargo run --release --bin relayer -- \
  --relayChains cardanopreprod,sepolia \
  --db /tmp/relayer-db
```

## 6. Test Message: Sepolia → Cardano

```bash
# Send from Sepolia
cast send $SEPOLIA_MAILBOX \
  "dispatch(uint32,bytes32,bytes)" \
  2002 \
  0x0000000000000000000000000000000000000000000000000000000000000001 \
  0x48656c6c6f \
  --rpc-url $SEPOLIA_RPC_URL \
  --private-key $EVM_PRIVATE_KEY

# Watch relayer logs for:
# "Indexed message from sepolia"
# "Delivering to cardanopreprod"
```

## 7. Test Message: Cardano → Sepolia

This requires building a Cardano transaction with:
- Spending the mailbox UTXO
- Dispatch redeemer with (destination=11155111, recipient, body)
- New mailbox UTXO with incremented nonce

See full guide for transaction building details.

## Key Domain IDs

| Chain | Domain |
|-------|--------|
| Cardano Preprod | 2002 |
| Sepolia | 11155111 |
| Fuji | 43113 |

## Troubleshooting

```bash
# Check Cardano UTXOs
curl -H "project_id: $BLOCKFROST_API_KEY" \
  "https://cardano-preprod.blockfrost.io/api/v0/addresses/$MAILBOX_ADDR/utxos"

# Check Sepolia message status
cast call $SEPOLIA_MAILBOX "delivered(bytes32)(bool)" $MSG_ID --rpc-url $SEPOLIA_RPC_URL

# Relayer not delivering? Check:
# 1. Validator is running and signing checkpoints
# 2. ISM on destination is configured with validator addresses
# 3. Relayer has funds on destination chain
```

For detailed instructions, see [TESTING_GUIDE.md](./TESTING_GUIDE.md).

# Hyperlane Cardano Recipient Deployment Guide

This guide explains how to build, deploy, and register Hyperlane-compatible recipient scripts on Cardano.

## Prerequisites

- [Aiken](https://aiken-lang.org/) installed (`aiken --version`)
- Cardano signing key (Ed25519 extended or normal)
- Blockfrost API key for the target network
- Funded wallet address (at least 50 ADA recommended)
- Hyperlane CLI built (`cd cli && cargo build --release`)

## Overview

Deploying a recipient involves three steps:

1. **Build** - Compile and parameterize the Aiken contract
2. **Deploy** - Create on-chain UTXOs with the script and initial state
3. **Register** - Add the recipient to the Hyperlane registry

## 1. Building the Recipient Contract

### 1.1 Write Your Recipient Contract

Create a new validator in `contracts/validators/`. Recipients must follow this pattern:

```aiken
use types.{
  Domain, HyperlaneAddress, HyperlaneRecipientDatum, HyperlaneRecipientRedeemer,
  ScriptHash, HandleMessage, ContractAction,
}

/// Your contract-specific state
pub type MyRecipientInner {
  // Add your fields here
  counter: Int,
}

/// Recipient validator - MUST be parameterized by mailbox_hash
validator my_recipient(mailbox_hash: ScriptHash) {
  spend(
    datum: Option<HyperlaneRecipientDatum<MyRecipientInner>>,
    redeemer: HyperlaneRecipientRedeemer<Void>,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    expect Some(recipient_datum) = datum

    when redeemer is {
      HandleMessage { origin, sender, body } ->
        // Handle incoming Hyperlane message
        handle_message(recipient_datum, origin, sender, body, tx, own_ref, mailbox_hash)

      ContractAction { action } ->
        // Handle contract-specific actions
        False
    }
  }

  else(_) {
    fail
  }
}
```

Key requirements:
- Use `HyperlaneRecipientDatum<YourInner>` wrapper for state
- Use `HyperlaneRecipientRedeemer<YourActions>` for redeemers
- Parameterize by `mailbox_hash` to verify caller
- Verify mailbox is spending its UTXO when handling messages

### 1.2 Compile the Contract

```bash
cd contracts
aiken build
```

This generates `plutus.json` with unparameterized validators.

### 1.3 Apply Parameters

The recipient validator requires the mailbox script hash as a parameter. You also need a state NFT minting policy parameterized by a UTXO.

```bash
# Get the mailbox script hash
MAILBOX_HASH=$(cat ../deployments/preview/mailbox.hash)

# Choose a UTXO to consume for the state NFT (ensures uniqueness)
# This UTXO will be spent in the init transaction
UTXO="your_tx_hash#output_index"

# Apply state_nft parameter (UTXO reference)
aiken blueprint apply -v state_nft.mint \
  --arg "$(echo "{\"constructor\":0,\"fields\":[{\"bytes\":\"$(echo $UTXO | cut -d'#' -f1)\"},{\"int\":$(echo $UTXO | cut -d'#' -f2)}]}")" \
  -o ../deployments/preview/recipient_state_nft.plutus

# Apply generic_recipient parameter (mailbox hash)
aiken blueprint apply -v generic_recipient.spend \
  --arg "{\"bytes\":\"$MAILBOX_HASH\"}" \
  -o ../deployments/preview/generic_recipient.plutus
```

## 2. Deploying the Recipient

### 2.1 Using the CLI (Recommended)

The CLI handles parameterization, NFT minting, and initial state creation:

```bash
cd /path/to/cardano

# Deploy with automatic parameterization
BLOCKFROST_API_KEY=your_api_key ./cli/target/release/hyperlane-cardano \
  --signing-key path/to/payment.skey \
  --network preview \
  init recipient \
  --mailbox-hash $(cat deployments/preview/mailbox.hash)

# Or with pre-applied scripts (if aiken isn't available)
BLOCKFROST_API_KEY=your_api_key ./cli/target/release/hyperlane-cardano \
  --signing-key path/to/payment.skey \
  --network preview \
  init recipient \
  --nft-script deployments/preview/recipient_state_nft.plutus \
  --recipient-script deployments/preview/generic_recipient.plutus
```

The CLI will output:
- Recipient script hash
- State NFT policy ID
- Recipient script address
- Transaction hash

### 2.2 Manual Deployment

If you need more control, deploy manually:

```bash
# 1. Calculate script hashes
RECIPIENT_HASH=$(cardano-cli hash script \
  --script-file deployments/preview/generic_recipient.plutus)

NFT_POLICY=$(cardano-cli hash script \
  --script-file deployments/preview/recipient_state_nft.plutus)

# 2. Build recipient address
cardano-cli address build \
  --payment-script-file deployments/preview/generic_recipient.plutus \
  --testnet-magic 2 \
  --out-file deployments/preview/generic_recipient.addr

# 3. Create initial datum (HyperlaneRecipientDatum)
# Structure: { ism: Option<ScriptHash>, last_processed_nonce: Option<Int>, inner: { messages_received: Int, last_message: Option<ByteArray> } }
cat > deployments/preview/recipient_datum.json << 'EOF'
{
  "constructor": 0,
  "fields": [
    { "constructor": 1, "fields": [] },
    { "constructor": 1, "fields": [] },
    {
      "constructor": 0,
      "fields": [
        { "int": 0 },
        { "constructor": 1, "fields": [] }
      ]
    }
  ]
}
EOF

# 4. Build and submit transaction that:
#    - Mints the state NFT
#    - Creates UTXO at recipient address with datum and NFT
```

## 3. Deploying as Reference Script (Optional)

Reference scripts reduce transaction costs by storing the script on-chain once.

```bash
BLOCKFROST_API_KEY=your_api_key ./cli/target/release/hyperlane-cardano \
  --signing-key path/to/payment.skey \
  --network preview \
  deploy reference-script \
  --script deployments/preview/generic_recipient.plutus \
  --name "generic_recipient"
```

This creates a UTXO with the script in its `reference_script` field. Note the output UTXO reference for use in transactions.

## 4. Registering the Recipient

Registration tells the relayer how to construct transactions for your recipient.

### 4.1 Using the CLI

```bash
BLOCKFROST_API_KEY=your_api_key ./cli/target/release/hyperlane-cardano \
  --signing-key path/to/payment.skey \
  --network preview \
  registry register \
  --script-hash <recipient_script_hash> \
  --state-policy <state_nft_policy_id> \
  --state-asset "" \
  --recipient-type generic
```

Parameters:
- `--script-hash`: 28-byte recipient validator hash (56 hex chars)
- `--state-policy`: State NFT policy ID (28 bytes)
- `--state-asset`: Asset name within policy (empty for unit token)
- `--recipient-type`: One of `generic`, `token-receiver`, `contract-caller`
- `--custom-ism`: (Optional) ISM script hash to override default

### 4.2 Verify Registration

```bash
BLOCKFROST_API_KEY=your_api_key ./cli/target/release/hyperlane-cardano \
  --network preview \
  registry list
```

Expected output:
```
Registry UTXO:
  abc123...#0

Registered Recipients:
--------------------------------------------------------------------------------
Script Hash                                                      Type
--------------------------------------------------------------------------------
931e71c75bd0ac35ff9024b3c2a578e006bf3abca509c11734f7f9bc         GenericHandler
...
```

## Recipient Types

| Type | Description | Use Case |
|------|-------------|----------|
| `GenericHandler` | Simple state update | Message logging, counters |
| `TokenReceiver` | Mints/releases tokens | Warp routes, token bridges |
| `ContractCaller` | Calls other contracts | Complex DeFi interactions |

## Example: Full Deployment Flow

```bash
# Set environment
export BLOCKFROST_API_KEY=previewXXXXXXXXXXXXXXXXXXXXXXXX
export SIGNING_KEY=./testnet-keys/payment.skey
export NETWORK=preview

cd /path/to/hyperlane-monorepo/cardano

# 1. Build contracts
cd contracts && aiken build && cd ..

# 2. Deploy recipient (mints NFT, creates state UTXO)
./cli/target/release/hyperlane-cardano \
  --signing-key $SIGNING_KEY \
  --network $NETWORK \
  init recipient \
  --mailbox-hash $(cat deployments/$NETWORK/mailbox.hash)

# Note the output:
# Recipient Script Hash: 931e71c75bd0ac35ff9024b3c2a578e006bf3abca509c11734f7f9bc
# State NFT Policy: f2e541ac484fc08eb2c0d8240a126d33a38316594a98343c768b0ab7

# 3. Register with registry
./cli/target/release/hyperlane-cardano \
  --signing-key $SIGNING_KEY \
  --network $NETWORK \
  registry register \
  --script-hash 931e71c75bd0ac35ff9024b3c2a578e006bf3abca509c11734f7f9bc \
  --state-policy f2e541ac484fc08eb2c0d8240a126d33a38316594a98343c768b0ab7 \
  --state-asset "" \
  --recipient-type generic

# 4. Verify
./cli/target/release/hyperlane-cardano --network $NETWORK registry list
```

## Troubleshooting

### "Registry UTXO not found"
The registry hasn't been initialized. Deploy core contracts first:
```bash
./cli/target/release/hyperlane-cardano init registry
```

### "PlutusFailure" on registration
Common causes:
- **Wrong owner**: Signing key must match registry datum owner
- **Already registered**: Script hash is already in registry
- **Invalid hashes**: Script hash or policy ID is wrong length (must be 28 bytes / 56 hex)

### "BadInputsUTxO" error
Blockfrost cache may be stale. Wait 30 seconds and retry.

### Script hash mismatch
Ensure you're using the parameterized script, not the raw blueprint validator.

## Contract Addresses (Preview Testnet)

| Contract | Hash |
|----------|------|
| Mailbox | `f01158af16d6f625eae141c3d495d0f57913847ca87ebd6bfdc4a719` |
| Multisig ISM | `02993c46cdcf8eb56ada209e277acc288dc0263b6a502d17b8cbfa56` |
| Registry | `b46f18719b2d20b87474eb9cd761d82f1d7f750548eed38e775d2caf` |

## Reference Scripts (Preview Testnet)

| Script | UTXO Reference |
|--------|----------------|
| Mailbox | `3081333c4d7becb16186fb9dfb29af70c4a309bdc0a53436b9ed8e6d01793994#0` |
| Multisig ISM | `1b03aac93a0dfd797fe52256b5d121fdb3d7f8fdbda411e74a01d10c5f37455d#0` |
| Registry | `26f3d562cbacecdcc13dd8b0b7da7477569d49a4a877968717c7a59afc2a22aa#0` |

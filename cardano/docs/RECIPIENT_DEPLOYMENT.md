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
931e71c75bd0ac35ff9024b3c2a578e006bf3abca509c11734f7f9bc         Generic
...
```

## Recipient Types

| Type | Description | Use Case |
|------|-------------|----------|
| `Generic` | Simple state update | Message logging, counters |
| `TokenReceiver` | Mints/releases tokens | Warp routes, token bridges |
| `Deferred` | Stores messages for later | Complex DeFi interactions |

## 5. Deploying a Deferred Recipient

Deferred recipients require additional components compared to generic recipients. The relayer stores messages on-chain with NFT markers, and they are processed separately later.

### 5.1 Components Required

A deferred recipient deployment requires:
1. **Deferred Recipient Validator** - Parameterized by mailbox hash and message NFT policy
2. **Message NFT Minting Policy** - Parameterized by mailbox policy (for security)
3. **State NFT Minting Policy** - Standard one-shot policy for state UTXO identification

### 5.2 Build the Contracts

```bash
cd contracts
aiken build
```

The `plutus.json` will contain:
- `example_deferred_recipient.spend` - The deferred recipient validator
- `stored_message_nft.mint` - Message NFT policy (for message storage)
- `state_nft.mint` - State NFT policy

### 5.3 Apply Parameters

Deferred recipients require multiple parameterized scripts:

```bash
# Environment setup
export MAILBOX_HASH=$(cat ../deployments/preview/mailbox.hash)
export MAILBOX_POLICY=$(cat ../deployments/preview/mailbox_nft.policy)

# 1. Apply message NFT policy parameter (mailbox policy ID)
aiken blueprint apply -v stored_message_nft.mint \
  --arg "{\"bytes\":\"$MAILBOX_POLICY\"}" \
  -o ../deployments/preview/message_nft.plutus

# Get the message NFT policy ID
MESSAGE_NFT_POLICY=$(cardano-cli hash script \
  --script-file ../deployments/preview/message_nft.plutus)

# 2. Choose a UTXO for state NFT uniqueness
UTXO="your_tx_hash#output_index"

# Apply state_nft parameter
aiken blueprint apply -v state_nft.mint \
  --arg "$(echo "{\"constructor\":0,\"fields\":[{\"bytes\":\"$(echo $UTXO | cut -d'#' -f1)\"},{\"int\":$(echo $UTXO | cut -d'#' -f2)}]}")" \
  -o ../deployments/preview/deferred_state_nft.plutus

# 3. Apply deferred recipient parameters (mailbox hash + message NFT policy)
aiken blueprint apply -v example_deferred_recipient.spend \
  --arg "{\"bytes\":\"$MAILBOX_HASH\"}" \
  --arg "{\"bytes\":\"$MESSAGE_NFT_POLICY\"}" \
  -o ../deployments/preview/deferred_recipient.plutus
```

### 5.4 Initialize the Deferred Recipient

```bash
# Deploy deferred recipient (mints state NFT, creates state UTXO, deploys reference scripts)
BLOCKFROST_API_KEY=your_api_key ./cli/target/release/hyperlane-cardano \
  --signing-key path/to/payment.skey \
  --network preview \
  init recipient \
  --recipient-type deferred \
  --mailbox-hash $MAILBOX_HASH \
  --message-nft-policy $MESSAGE_NFT_POLICY
```

Or with pre-applied scripts:

```bash
BLOCKFROST_API_KEY=your_api_key ./cli/target/release/hyperlane-cardano \
  --signing-key path/to/payment.skey \
  --network preview \
  init recipient \
  --nft-script deployments/preview/deferred_state_nft.plutus \
  --recipient-script deployments/preview/deferred_recipient.plutus \
  --recipient-type deferred \
  --message-nft-policy $MESSAGE_NFT_POLICY
```

**Output:**
```
Deferred Recipient Deployment:
  Recipient Script Hash: 931e71c75bd0ac35ff9024b3c2a578e006bf3abca509c11734f7f9bc
  Recipient Address: addr_test1wz...
  State NFT Policy: f2e541ac484fc08eb2c0d8240a126d33a38316594a98343c768b0ab7
  Message NFT Policy: abc123...
  Init TX Hash: 3081333c4d7becb16186fb9dfb29af70c4a309bdc0a53436b9ed8e6d01793994

Reference Scripts:
  Recipient Ref Script: 3081333c4d7becb16186fb9dfb29af70c4a309bdc0a53436b9ed8e6d01793994#1
  Message NFT Ref Script: 3081333c4d7becb16186fb9dfb29af70c4a309bdc0a53436b9ed8e6d01793994#2
```

### 5.5 Deploy Message NFT Reference Script

For gas efficiency, deploy the message NFT policy as a reference script:

```bash
BLOCKFROST_API_KEY=your_api_key ./cli/target/release/hyperlane-cardano \
  --signing-key path/to/payment.skey \
  --network preview \
  deploy reference-script \
  --script deployments/preview/message_nft.plutus \
  --name "message_nft"
```

### 5.6 Register the Deferred Recipient

```bash
BLOCKFROST_API_KEY=your_api_key ./cli/target/release/hyperlane-cardano \
  --signing-key path/to/payment.skey \
  --network preview \
  registry register \
  --script-hash 931e71c75bd0ac35ff9024b3c2a578e006bf3abca509c11734f7f9bc \
  --state-policy f2e541ac484fc08eb2c0d8240a126d33a38316594a98343c768b0ab7 \
  --state-asset "" \
  --recipient-type deferred \
  --message-policy abc123... \
  --ref-script-policy f2e541ac484fc08eb2c0d8240a126d33a38316594a98343c768b0ab7 \
  --ref-script-asset "726566"
```

**Key parameters for deferred registration:**
- `--recipient-type deferred`: Tells the relayer to use the deferred message pattern
- `--message-policy`: The message NFT policy ID (relayer will mint NFTs with this policy)
- `--ref-script-policy` / `--ref-script-asset`: Locator for the reference script UTXO

### 5.7 Verify Deployment

```bash
# Check registration
./cli/target/release/hyperlane-cardano --network preview registry list

# Check state UTXO
./cli/target/release/hyperlane-cardano --network preview query utxo \
  --policy f2e541ac484fc08eb2c0d8240a126d33a38316594a98343c768b0ab7
```

## 6. Operating a Deferred Recipient

Once deployed, you need to process the stored messages. This can be done manually, via an automated service, through a dApp, or any other mechanism.

### 6.1 Monitor for Pending Messages

```bash
# Using the CLI
hyperlane-cardano deferred list \
  --recipient-address addr_test1wz... \
  --message-nft-policy abc123...
```

### 6.2 View Message Details

```bash
hyperlane-cardano deferred show \
  --message-utxo "txhash#0"
```

### 6.3 Process Messages

For the example_deferred_recipient (simple counter pattern):

```bash
# Dry run first
hyperlane-cardano deferred process \
  --message-utxo "message_tx_hash#0" \
  --recipient-state-policy f2e541ac... \
  --message-nft-policy abc123... \
  --recipient-ref-script "ref_script_tx#1" \
  --nft-ref-script "nft_ref_script_tx#2" \
  --dry-run

# Submit if dry run looks good
hyperlane-cardano deferred process \
  --message-utxo "message_tx_hash#0" \
  --recipient-state-policy f2e541ac... \
  --message-nft-policy abc123... \
  --recipient-ref-script "ref_script_tx#1" \
  --nft-ref-script "nft_ref_script_tx#2"
```

### 6.4 Automated Processing

For production, you can set up automated processing with a cron job or daemon:

```bash
#!/bin/bash
# process_deferred_messages.sh

RECIPIENT_ADDRESS="addr_test1wz..."
STATE_POLICY="f2e541ac..."
MESSAGE_POLICY="abc123..."
RECIPIENT_REF="ref_tx#1"
NFT_REF="nft_tx#2"

# List pending messages (JSON format)
MESSAGES=$(hyperlane-cardano deferred list \
  --recipient-address $RECIPIENT_ADDRESS \
  --message-nft-policy $MESSAGE_POLICY \
  --format json)

# Process each message
echo "$MESSAGES" | jq -r '.[].utxo' | while read UTXO; do
  echo "Processing: $UTXO"
  hyperlane-cardano deferred process \
    --message-utxo "$UTXO" \
    --recipient-state-policy $STATE_POLICY \
    --message-nft-policy $MESSAGE_POLICY \
    --recipient-ref-script "$RECIPIENT_REF" \
    --nft-ref-script "$NFT_REF"

  # Wait between transactions to avoid contention
  sleep 30
done
```

Run with cron:
```bash
# Process every 5 minutes
*/5 * * * * /path/to/process_deferred_messages.sh >> /var/log/deferred_processor.log 2>&1
```

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

## Contract Addresses

Contract addresses and reference script UTXOs change with each deployment. After deployment, check:

```bash
# View current deployment info
cat deployments/$NETWORK/deployment_info.json

# Or use CLI
./cli/target/release/hyperlane-cardano --network $NETWORK init status
```

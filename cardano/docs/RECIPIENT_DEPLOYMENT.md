# Hyperlane Cardano Recipient Deployment Guide

This guide explains how to build and deploy Hyperlane-compatible recipient scripts on Cardano.

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
3. **Enroll** - Remote chains enroll the state NFT policy as the Cardano recipient address

## 1. Building the Recipient Contract

### 1.1 Write Your Recipient Contract

Create a new validator in `contracts/validators/`. Recipients must follow this pattern:

```aiken
use types.{
  Domain, HyperlaneAddress, HyperlaneRecipientDatum, HyperlaneRecipientRedeemer,
  PolicyId, HandleMessage, ContractAction,
}

/// Your contract-specific state
pub type MyRecipientInner {
  // Add your fields here
  counter: Int,
}

/// Recipient validator - MUST be parameterized by verified_message_nft_policy
validator my_recipient(verified_message_nft_policy: PolicyId) {
  spend(
    datum: Option<HyperlaneRecipientDatum<MyRecipientInner>>,
    redeemer: HyperlaneRecipientRedeemer<Void>,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    expect Some(recipient_datum) = datum

    when redeemer is {
      HandleMessage { message, message_id } ->
        // Handle incoming Hyperlane message
        // Verify that a verified_message_nft is burned in this TX
        handle_message(recipient_datum, message, message_id, tx, own_ref, verified_message_nft_policy)

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
- Parameterize by `verified_message_nft_policy` to verify message authenticity
- Verify a `verified_message_nft` is burned when handling messages (the mailbox mints this NFT at the recipient's script address during `Process`, and the recipient burns it during message handling)

### 1.2 Compile the Contract

```bash
cd contracts
aiken build
```

This generates `plutus.json` with unparameterized validators.

### 1.3 Apply Parameters

The recipient validator requires the `verified_message_nft_policy` as a parameter. This is the policy ID of the verified message NFT that the mailbox mints during `Process`. You also need a state NFT minting policy parameterized by a UTXO.

```bash
# Get the verified message NFT policy ID
VERIFIED_MESSAGE_NFT_POLICY=$(cat ../deployments/preview/verified_message_nft.policy)

# Choose a UTXO to consume for the state NFT (ensures uniqueness)
# This UTXO will be spent in the init transaction
UTXO="your_tx_hash#output_index"

# Apply state_nft parameter (UTXO reference)
aiken blueprint apply -v state_nft.mint \
  --arg "$(echo "{\"constructor\":0,\"fields\":[{\"bytes\":\"$(echo $UTXO | cut -d'#' -f1)\"},{\"int\":$(echo $UTXO | cut -d'#' -f2)}]}")" \
  -o ../deployments/preview/recipient_state_nft.plutus

# Apply generic_recipient parameter (verified message NFT policy)
aiken blueprint apply -v generic_recipient.spend \
  --arg "{\"bytes\":\"$VERIFIED_MESSAGE_NFT_POLICY\"}" \
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
  --verified-message-nft-policy $(cat deployments/preview/verified_message_nft.policy)

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

## 4. Enrolling the Recipient on Remote Chains

No on-chain registration is needed on Cardano. The relayer discovers recipients via O(1) NFT queries using the state NFT policy ID.

### 4.1 Determine the Hyperlane Address

The addressing scheme depends on the recipient type:

**Warp routes** (TokenReceiver): Use NFT-policy addressing
```
Hyperlane address = 0x01000000{state_nft_policy_id}
```

**Generic recipients** (e.g., greeting): Use script-hash addressing
```
Hyperlane address = 0x02000000{script_hash}
```

For example, if your generic recipient script hash is `7fb8e3ae915c4c3759ffa6e98ce31a10024c775f300efb0ede58472c`, the Hyperlane address is:

```
0x020000007fb8e3ae915c4c3759ffa6e98ce31a10024c775f300efb0ede58472c
```

### 4.2 Enroll on Remote Chains

Remote chains must enroll this address as the Cardano recipient. For example, on an EVM chain, the router contract should store this address as the enrolled remote for the Cardano domain.

### 4.3 Verify State UTXO

```bash
./cli/target/release/hyperlane-cardano --network preview query utxo \
  --policy f2e541ac484fc08eb2c0d8240a126d33a38316594a98343c768b0ab7
```

## Recipient Types

| Type                          | Description                                                                 | Use Case                   |
| ----------------------------- | --------------------------------------------------------------------------- | -------------------------- |
| `Generic (Verified Message)`  | Receives verified message NFT from mailbox, processes in separate TX        | Message logging, counters, DeFi interactions |
| `TokenReceiver`               | Mints/releases tokens                                                       | Warp routes, token bridges |

## 5. Deploying a Generic Recipient (Verified Message Pattern)

Generic recipients on Cardano use a two-step message delivery pattern:

1. **Mailbox Process**: The mailbox creates a `verified_message_nft` UTXO at the recipient's script address during the `Process` transaction.
2. **Message Receive**: A separate transaction spends the verified message NFT UTXO, burns the NFT, and delivers the message to the recipient contract.

This is the standard pattern for all generic (non-warp-route) recipients.

### 5.1 Components Required

A generic recipient deployment requires:

1. **Recipient Validator** - Parameterized by `verified_message_nft_policy`
2. **Verified Message NFT Minting Policy** - Parameterized by mailbox policy (for security)
3. **State NFT Minting Policy** - Standard one-shot policy for state UTXO identification

### 5.2 Build the Contracts

```bash
cd contracts
aiken build
```

The `plutus.json` will contain:

- `generic_recipient.spend` (or your custom recipient) - The recipient validator
- `verified_message_nft.mint` - Verified message NFT policy (minted by mailbox, burned by recipient)
- `state_nft.mint` - State NFT policy

### 5.3 Apply Parameters

Generic recipients require multiple parameterized scripts:

```bash
# Environment setup
export MAILBOX_POLICY=$(cat ../deployments/preview/mailbox_nft.policy)

# 1. Apply verified message NFT policy parameter (mailbox policy ID)
aiken blueprint apply -v verified_message_nft.mint \
  --arg "{\"bytes\":\"$MAILBOX_POLICY\"}" \
  -o ../deployments/preview/verified_message_nft.plutus

# Get the verified message NFT policy ID
VERIFIED_MESSAGE_NFT_POLICY=$(cardano-cli hash script \
  --script-file ../deployments/preview/verified_message_nft.plutus)

# 2. Choose a UTXO for state NFT uniqueness
UTXO="your_tx_hash#output_index"

# Apply state_nft parameter
aiken blueprint apply -v state_nft.mint \
  --arg "$(echo "{\"constructor\":0,\"fields\":[{\"bytes\":\"$(echo $UTXO | cut -d'#' -f1)\"},{\"int\":$(echo $UTXO | cut -d'#' -f2)}]}")" \
  -o ../deployments/preview/recipient_state_nft.plutus

# 3. Apply recipient parameter (verified message NFT policy)
aiken blueprint apply -v generic_recipient.spend \
  --arg "{\"bytes\":\"$VERIFIED_MESSAGE_NFT_POLICY\"}" \
  -o ../deployments/preview/generic_recipient.plutus
```

### 5.4 Initialize the Recipient

```bash
# Deploy recipient (mints state NFT, creates state UTXO, deploys reference scripts)
BLOCKFROST_API_KEY=your_api_key ./cli/target/release/hyperlane-cardano \
  --signing-key path/to/payment.skey \
  --network preview \
  init recipient \
  --verified-message-nft-policy $VERIFIED_MESSAGE_NFT_POLICY
```

Or with pre-applied scripts:

```bash
BLOCKFROST_API_KEY=your_api_key ./cli/target/release/hyperlane-cardano \
  --signing-key path/to/payment.skey \
  --network preview \
  init recipient \
  --nft-script deployments/preview/recipient_state_nft.plutus \
  --recipient-script deployments/preview/generic_recipient.plutus \
  --verified-message-nft-policy $VERIFIED_MESSAGE_NFT_POLICY
```

**Output:**

```
Generic Recipient Deployment:
  Recipient Script Hash: 931e71c75bd0ac35ff9024b3c2a578e006bf3abca509c11734f7f9bc
  Recipient Address: addr_test1wz...
  State NFT Policy: f2e541ac484fc08eb2c0d8240a126d33a38316594a98343c768b0ab7
  Verified Message NFT Policy: abc123...
  Init TX Hash: 3081333c4d7becb16186fb9dfb29af70c4a309bdc0a53436b9ed8e6d01793994

Reference Scripts:
  Recipient Ref Script: 3081333c4d7becb16186fb9dfb29af70c4a309bdc0a53436b9ed8e6d01793994#1
  Verified Message NFT Ref Script: 3081333c4d7becb16186fb9dfb29af70c4a309bdc0a53436b9ed8e6d01793994#2
```

### 5.5 Deploy Verified Message NFT Reference Script

For gas efficiency, deploy the verified message NFT policy as a reference script:

```bash
BLOCKFROST_API_KEY=your_api_key ./cli/target/release/hyperlane-cardano \
  --signing-key path/to/payment.skey \
  --network preview \
  deploy reference-script \
  --script deployments/preview/verified_message_nft.plutus \
  --name "verified_message_nft"
```

### 5.6 Enrolling the Recipient

No on-chain registration is needed. The recipient's Hyperlane address is `0x02000000{script_hash}` for generic recipients, or `0x01000000{state_nft_policy_id}` for warp routes. Remote chains should enroll the appropriate address.

The relayer automatically discovers generic recipients and creates verified message NFT UTXOs at the recipient's script address during the `Process` step.

### 5.7 Verify Deployment

```bash
# Check state UTXO
./cli/target/release/hyperlane-cardano --network preview query utxo \
  --policy f2e541ac484fc08eb2c0d8240a126d33a38316594a98343c768b0ab7
```

## 6. Operating a Generic Recipient

Once deployed, you need to process verified messages delivered by the mailbox. The mailbox creates `verified_message_nft` UTXOs at the recipient's script address during `Process`. These contain a `VerifiedMessageDatum` with the message data. A separate `message receive` transaction delivers each message to the recipient contract by spending the verified message NFT UTXO and burning the NFT.

This can be done manually, via an automated service, through a dApp, or any other mechanism.

### 6.1 Monitor for Pending Messages

```bash
# Using the CLI
hyperlane-cardano message list \
  --recipient-address addr_test1wz...
```

### 6.2 View Message Details

```bash
hyperlane-cardano message show \
  --message-utxo "txhash#0"
```

### 6.3 Process Messages (Message Receive)

For a generic recipient (e.g., the greeting contract):

```bash
# Dry run first
hyperlane-cardano message receive \
  --message-utxo "message_tx_hash#0" \
  --recipient-state-policy f2e541ac... \
  --verified-message-nft-policy abc123... \
  --recipient-ref-script "ref_script_tx#1" \
  --nft-ref-script "nft_ref_script_tx#2" \
  --dry-run

# Submit if dry run looks good
hyperlane-cardano message receive \
  --message-utxo "message_tx_hash#0" \
  --recipient-state-policy f2e541ac... \
  --verified-message-nft-policy abc123... \
  --recipient-ref-script "ref_script_tx#1" \
  --nft-ref-script "nft_ref_script_tx#2"
```

### 6.4 Automated Processing

For production, you can set up automated processing with a cron job or daemon:

```bash
#!/bin/bash
# process_verified_messages.sh

RECIPIENT_ADDRESS="addr_test1wz..."
STATE_POLICY="f2e541ac..."
VERIFIED_MESSAGE_POLICY="abc123..."
RECIPIENT_REF="ref_tx#1"
NFT_REF="nft_tx#2"

# List pending messages (JSON format)
MESSAGES=$(hyperlane-cardano message list \
  --recipient-address $RECIPIENT_ADDRESS \
  --format json)

# Process each message
echo "$MESSAGES" | jq -r '.[].utxo' | while read UTXO; do
  echo "Processing: $UTXO"
  hyperlane-cardano message receive \
    --message-utxo "$UTXO" \
    --recipient-state-policy $STATE_POLICY \
    --verified-message-nft-policy $VERIFIED_MESSAGE_POLICY \
    --recipient-ref-script "$RECIPIENT_REF" \
    --nft-ref-script "$NFT_REF"

  # Wait between transactions to avoid contention
  sleep 30
done
```

Run with cron:

```bash
# Process every 5 minutes
*/5 * * * * /path/to/process_verified_messages.sh >> /var/log/message_processor.log 2>&1
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
  --verified-message-nft-policy $(cat deployments/$NETWORK/verified_message_nft.policy)

# Note the output:
# Recipient Script Hash: 931e71c75bd0ac35ff9024b3c2a578e006bf3abca509c11734f7f9bc
# State NFT Policy: f2e541ac484fc08eb2c0d8240a126d33a38316594a98343c768b0ab7

# 3. Enroll on remote chains
# For generic recipients: 0x02000000{script_hash}
# For warp routes: 0x01000000{state_nft_policy_id}
# Enroll the appropriate address on your remote chain's router contract.

# 4. Verify state UTXO
./cli/target/release/hyperlane-cardano --network $NETWORK query utxo \
  --policy f2e541ac484fc08eb2c0d8240a126d33a38316594a98343c768b0ab7
```

## Troubleshooting

### "BadInputsUTxO" error

Blockfrost cache may be stale. Wait 30 seconds and retry.

### Script hash mismatch

Ensure you're using the parameterized script, not the raw blueprint validator.

### State UTXO not found by relayer

- Verify the state NFT policy ID matches the Hyperlane address enrolled on the remote chain
- Confirm the state UTXO exists on-chain and contains the NFT

## Contract Addresses

Contract addresses and reference script UTXOs change with each deployment. After deployment, check:

```bash
# View current deployment info
cat deployments/$NETWORK/deployment_info.json

# Or use CLI
./cli/target/release/hyperlane-cardano --network $NETWORK init status
```

# Hyperlane Cardano Deployment Guide

This comprehensive guide explains how to deploy all Hyperlane contracts on Cardano using the CLI. It covers the complete deployment process, including contract dependencies, parametrization, and reference script deployment.

## Table of Contents

1. [Prerequisites](#prerequisites)
2. [Contract Overview & Dependencies](#contract-overview--dependencies)
3. [Phase 1: Build Contracts](#phase-1-build-contracts)
4. [Phase 2: Extract Validators](#phase-2-extract-validators)
5. [Phase 3: Initialize Core Contracts](#phase-3-initialize-core-contracts)
6. [Phase 4: Deploy Reference Scripts](#phase-4-deploy-reference-scripts)
7. [Phase 5: Configure Contracts](#phase-5-configure-contracts)
8. [Phase 6: Deploy Recipients](#phase-6-deploy-recipients)
9. [Verification & Troubleshooting](#verification--troubleshooting)
10. [Complete Deployment Script](#complete-deployment-script)

---

## Prerequisites

### Required Tools

```bash
# 1. Aiken compiler (for building contracts)
curl -sSfL https://install.aiken-lang.org | bash
aiken --version  # Should show v1.0.0 or later

# 2. Rust toolchain (for CLI)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup default stable

# 3. Build the Hyperlane CLI
cd cardano/cli
cargo build --release
```

### Required Credentials

```bash
# Set environment variables
export BLOCKFROST_API_KEY="your_blockfrost_api_key"  # Get from https://blockfrost.io
export CARDANO_SIGNING_KEY="/path/to/payment.skey"   # Ed25519 signing key
export NETWORK="preview"                              # preview, preprod, or mainnet
```

### Funded Wallet

Your signing key must control a wallet with sufficient ADA:

| Operation | Minimum ADA Required |
|-----------|---------------------|
| Reference script deployment | ~15 ADA per script |
| Contract initialization | ~10 ADA per contract |
| Total recommended | ~100 ADA |

---

## Contract Overview & Dependencies

### Core Contracts

| Contract | Purpose | Parameters | Dependencies |
|----------|---------|------------|--------------|
| **state_nft** | Unique NFT minting policy | UTXO reference | None |
| **mailbox** | Message dispatch/process hub | processed_messages_script | registry (default) |
| **multisig_ism** | Signature verification | None | None |
| **registry** | Recipient metadata store | None | None |
| **processed_message_nft** | Replay prevention | mailbox_script_hash | mailbox |

> **Note**: The mailbox validator is parameterized with `processed_messages_script`, which is the script address where processed message markers are stored. By default, this is set to the registry script hash, which allows the registry to double as processed message storage.

### Recipient Contracts

| Contract | Purpose | Parameters | Dependencies |
|----------|---------|------------|--------------|
| **example_generic_recipient** | Example message handler | mailbox_policy_id | mailbox |
| **warp_route** | Token bridge | mailbox_policy_id | mailbox, vault |
| **vault** | Token custody | None | warp_route |

### Dependency Graph

```
                 ┌─────────────────────────────────────────────┐
                 │           STATE NFT MINTING POLICY           │
                 │   (Parameterized per contract instance)      │
                 └─────────────────────────────────────────────┘
                                      │
          ┌───────────────────────────┼───────────────────────────┐
          │                           │                           │
          ▼                           ▼                           ▼
┌─────────────────┐         ┌─────────────────┐         ┌─────────────────┐
│     MAILBOX     │         │   MULTISIG_ISM  │         │    REGISTRY     │
│   (Validator)   │         │   (Validator)   │         │   (Validator)   │
└────────┬────────┘         └─────────────────┘         └─────────────────┘
         │                                                        │
         │ mailbox_policy_id                                      │
         ▼                                                        ▼
┌─────────────────┐                                    ┌─────────────────────┐
│ GENERIC_RECIPIENT│◄───────────────────────────────────│  REGISTRATION       │
│ (Parameterized) │                                    │  (Recipient entry)  │
└─────────────────┘                                    └─────────────────────┘
```

### Deployment Order

The contracts must be deployed in this order due to dependencies:

1. **Extract all validators** from plutus.json
2. **Initialize Core Contracts** - applies parameters, creates state NFTs, produces parameterized scripts
3. **Deploy Reference Scripts** - deploy the parameterized scripts as on-chain reference scripts
4. **Configure Mailbox** - set default ISM using ism_policy_id
5. **Configure ISM** - set validators and thresholds for each origin domain
6. **Deploy Recipients** - parameterized with mailbox_policy_id
7. **Register Recipients** - add to registry

> **Important**: Reference scripts can only be deployed AFTER initialization because the core contracts (mailbox, ISM, registry) are parameterized. The initialization step applies the required parameters and produces the final script bytecode.

---

## Phase 1: Build Contracts

### 1.1 Navigate to Contracts Directory

```bash
cd cardano/contracts
```

### 1.2 Build with Aiken

```bash
aiken build
```

This generates `plutus.json` containing all compiled validators:

```bash
# Verify output
cat plutus.json | jq '.validators[].title'
```

Expected output:
```
"mailbox.mailbox.spend"
"multisig_ism.multisig_ism.spend"
"registry.registry.spend"
"state_nft.state_nft.mint"
"example_generic_recipient.example_generic_recipient.spend"
"processed_message_nft.processed_message_nft.mint"
"warp_route.warp_route.spend"
"vault.vault.spend"
...
```

---

## Phase 2: Extract Validators

### 2.1 Extract All Validators

```bash
cd cardano

./cli/target/release/hyperlane-cardano \
  --network $NETWORK \
  deploy extract \
  --output deployments/$NETWORK
```

This creates:
- `deployments/$NETWORK/*.plutus` - Cardano CLI compatible script files
- `deployments/$NETWORK/*.hash` - Script hash files
- `deployments/$NETWORK/*.addr` - Bech32 script addresses
- `deployments/$NETWORK/deployment_info.json` - Deployment metadata

### 2.2 View Validator Information

```bash
./cli/target/release/hyperlane-cardano \
  --network $NETWORK \
  deploy info
```

### 2.3 Verify Extraction

```bash
ls deployments/$NETWORK/

# Expected files:
# mailbox.plutus, mailbox.hash, mailbox.addr
# multisig_ism.plutus, multisig_ism.hash, multisig_ism.addr
# registry.plutus, registry.hash, registry.addr
# state_nft.plutus (base, not parameterized)
# ...
```

---

## Phase 3: Initialize Core Contracts

Initialization applies parameters to the contracts, creates state NFTs, and sets up initial datums. This step is required before deploying reference scripts because the core contracts are parameterized.

### 3.1 Initialize All Core Contracts (Recommended)

```bash
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  init all \
  --domain 2003 \
  --origin-domains "43113,11155111"
```

Parameters:
- `--domain`: Local Cardano domain ID (2003 for preview, 2002 for preprod)
- `--origin-domains`: Comma-separated list of origin chain domain IDs to configure

### 3.2 Initialize Individually (Alternative)

#### Initialize Mailbox

```bash
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  init mailbox \
  --domain 2003 \
  --ism-hash "0000000000000000000000000000000000000000000000000000000000"
```

Note: We use a placeholder ISM hash initially; it will be updated after ISM initialization.

Output:
```
Mailbox initialized!
  State NFT Policy: f01158af16d6f625eae141c3d495d0f57913847ca87ebd6bfdc4a719
  UTXO: abc123...#0
```

#### Initialize Multisig ISM

```bash
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  init ism \
  --domains "43113,11155111" \
  --validators "43113:ab8cc5ae0dcce3d0dff1925a70cda0250f06ba21" \
  --thresholds "43113:1,11155111:1"
```

Parameters:
- `--domains`: Origin domain IDs (comma-separated)
- `--validators`: Format: "domain:addr1,addr2;domain2:addr3"
- `--thresholds`: Format: "domain:threshold,domain2:threshold"

Output:
```
ISM initialized!
  State NFT Policy: 02993c46cdcf8eb56ada209e277acc288dc0263b6a502d17b8cbfa56
  UTXO: def456...#0
```

#### Initialize Registry

```bash
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  init registry
```

Output:
```
Registry initialized!
  State NFT Policy: b46f18719b2d20b87474eb9cd761d82f1d7f750548eed38e775d2caf
  UTXO: ghi789...#0
```

### 3.3 Verify Initialization

```bash
# Check status
./cli/target/release/hyperlane-cardano \
  --network $NETWORK \
  init status
```

---

## Phase 4: Deploy Reference Scripts

Reference scripts are deployed on-chain to reduce transaction costs. Each script is stored in a UTXO that can be referenced by future transactions.

> **Note**: This step must be done AFTER initialization because the contracts are parameterized. The `init` commands apply the required parameters and save the parameterized scripts to the deployments directory.

### 4.1 Deploy All Core Reference Scripts

```bash
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  deploy reference-scripts-all
```

This deploys:
- Mailbox validator (15 ADA minimum UTXO)
- Multisig ISM validator (15 ADA minimum UTXO)
- Registry validator (15 ADA minimum UTXO)

### 4.2 Deploy Individual Reference Script (Alternative)

```bash
# Deploy a specific script by name (uses applied script automatically)
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  deploy reference-script \
  --script mailbox

# Or deploy from a specific .plutus file
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  deploy reference-script \
  --script deployments/$NETWORK/mailbox_applied.plutus \
  --lovelace 15000000
```

### 4.3 Verify Reference Script Deployment

The CLI automatically saves the reference script UTXOs to `deployment_info.json`. You can verify the deployment:

```bash
# Check deployment_info.json for referenceScriptUtxo fields
cat deployments/$NETWORK/deployment_info.json | jq '.mailbox.referenceScriptUtxo'
cat deployments/$NETWORK/deployment_info.json | jq '.ism.referenceScriptUtxo'
cat deployments/$NETWORK/deployment_info.json | jq '.registry.referenceScriptUtxo'
```

When configuring the relayer, use these UTXO references in your agent configuration:

```yaml
chains:
  cardano:
    mailboxReferenceScriptUtxo: "<tx_hash>#0"
    ismReferenceScriptUtxo: "<tx_hash>#0"
```

---

## Phase 5: Configure Contracts

### 5.1 Update Mailbox Default ISM

After ISM is initialized, update the mailbox to use the correct ISM:

```bash
# Get the ISM script hash from deployment info
ISM_HASH=$(cat deployments/$NETWORK/multisig_ism.hash)

BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  mailbox set-default-ism \
  --ism-hash $ISM_HASH
```

### 5.2 Configure ISM Validators

Set validators for each origin domain:

```bash
# For Avalanche Fuji testnet (domain 43113)
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  ism set-validators \
  --domain 43113 \
  --validators "d8154f73d04cc7f7f0c332793692e6e6f6b2402e,895ae30bc83ff1493b9cf7781b0b813d23659857,43e915573d9f1383cbf482049e4a012290759e7f"
```

### 5.3 Set ISM Threshold

```bash
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  ism set-threshold \
  --domain 43113 \
  --threshold 2
```

### 5.4 Verify Configuration

```bash
# Show mailbox configuration
./cli/target/release/hyperlane-cardano \
  --network $NETWORK \
  mailbox show

# Show ISM configuration
./cli/target/release/hyperlane-cardano \
  --network $NETWORK \
  ism show
```

---

## Phase 6: Deploy Recipients

Recipients are contracts that receive Hyperlane messages. They must be parameterized with the mailbox policy ID. The CLI supports deploying both the built-in example recipient and custom recipient contracts.

### 6.1 Deploy Built-in Example Recipient

The CLI includes a generic recipient for testing. Deploy it with:

```bash
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  init recipient
```

The CLI automatically reads the mailbox NFT policy ID from `deployment_info.json`. If you need to specify it manually:

```bash
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  init recipient \
  --mailbox-hash <mailbox_nft_policy_id>
```

This:
1. Applies the mailbox NFT policy ID parameter to the `example_generic_recipient` script
2. Creates a state NFT for the recipient
3. Creates two UTXOs:
   - State UTXO at recipient address with datum
   - Reference script UTXO for transaction efficiency

Output:
```
Recipient deployed!
  Script Hash: 931e71c75bd0ac35ff9024b3c2a578e006bf3abca509c11734f7f9bc
  State NFT Policy: f2e541ac484fc08eb2c0d8240a126d33a38316594a98343c768b0ab7
  State UTXO: xyz123...#0
  Reference Script UTXO: xyz123...#1
```

### 6.2 Deploy Custom Recipient

To deploy your own recipient contract, use the `--custom-contracts`, `--custom-module`, and `--custom-validator` options:

```bash
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  init recipient \
  --custom-contracts ./path/to/your/contracts \
  --custom-module my_recipient \
  --custom-validator my_recipient
```

Requirements for custom recipients:
- Your contract must be an Aiken project with a compiled `plutus.json` blueprint
- The validator must accept `mailbox_policy_id: PolicyId` as its first parameter
- The CLI will automatically apply the mailbox policy ID parameter using `aiken blueprint apply`

Example custom recipient structure:
```aiken
validator my_recipient(mailbox_policy_id: PolicyId) {
  spend(datum, redeemer, own_ref, tx) {
    // Verify mailbox is calling by checking for mailbox NFT in inputs
    expect mailbox_is_caller(tx, mailbox_policy_id)
    // Your custom logic here
    True
  }
}
```

### 6.3 Deploy Deferred Recipient (Optional)

The deferred recipient pattern allows messages to be stored first and processed later. This is useful for:
- Rate limiting or batching message processing
- Allowing users to trigger message processing at their convenience
- Separating message reception from execution

```bash
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  init recipient --deferred
```

This command:
1. Applies the mailbox NFT policy ID to `stored_message_nft` (minting policy for message storage)
2. Applies both `mailbox_policy_id` and `message_nft_policy` to `example_deferred_recipient`
3. Creates the **three-UTXO pattern** with:
   - State UTXO: holds the recipient state datum with state NFT (empty asset name)
   - Recipient Reference Script UTXO: holds the recipient validator script with "ref" NFT
   - Message NFT Reference Script UTXO: holds the `stored_message_nft` minting policy with "msg_ref" NFT

#### Three-UTXO Pattern (Deferred Recipients)

Deferred recipients require an additional reference script UTXO to provide the `stored_message_nft` minting policy. This allows the relayer to discover everything it needs from the registry without any additional configuration:

```
┌─────────────────────────────────────────────────────────────────────────┐
│                    DEFERRED RECIPIENT DEPLOYMENT                        │
└─────────────────────────────────────────────────────────────────────────┘
                                  │
     ┌────────────────────────────┼────────────────────────────┐
     │                            │                            │
     ▼                            ▼                            ▼
┌────────────────┐     ┌──────────────────────┐     ┌──────────────────────┐
│  State UTXO    │     │  Ref Script UTXO     │     │  Msg Ref Script UTXO │
│  (output #0)   │     │  (output #1)         │     │  (output #2)         │
├────────────────┤     ├──────────────────────┤     ├──────────────────────┤
│ NFT: "" (empty)│     │ NFT: "ref" (726566)  │     │ NFT: "msg_ref"       │
│ Datum: state   │     │ Script: recipient    │     │      (6d73675f726566)│
│ Location:      │     │ Location: deployer   │     │ Script: stored_      │
│   script addr  │     │   address            │     │   message_nft        │
└────────────────┘     └──────────────────────┘     │ Location: deployer   │
                                                    │   address            │
                                                    └──────────────────────┘
```

All three NFTs share the same policy ID, which is the `reference_script_locator` stored in the registry. The relayer:
1. Looks up the "ref" NFT UTXO for the recipient script
2. Looks up the "msg_ref" NFT UTXO for the `stored_message_nft` script
3. Uses both as reference inputs when processing messages

Output includes:
```
Stored Message NFT Policy: abc123...
Recipient Script Hash: def456...
Message NFT Policy: abc123...

State UTXO (output #0):
  NFT Policy: xyz789...
  NFT Asset Name: (empty)

Recipient Reference Script UTXO (output #1):
  NFT Policy: xyz789...
  NFT Asset Name: 726566 ("ref")

Message NFT Reference Script UTXO (output #2):
  NFT Policy: xyz789...
  NFT Asset Name: 6d73675f726566 ("msg_ref")
  Contains: stored_message_nft minting policy script

To register this recipient with the Hyperlane registry, run:
  hyperlane-cardano registry register \
    --script-hash def456... \
    --recipient-type deferred \
    --message-policy abc123... \
    --state-policy xyz789... \
    --state-asset "" \
    --ref-script-policy xyz789... \
    --ref-script-asset 726566 \
    --signing-key <path-to-owner-key>
```

> **Important**: The `--message-policy` is required when registering deferred recipients. This is the `stored_message_nft` policy ID shown during deployment.

> **Note**: The "msg_ref" NFT UTXO is automatically discovered by the relayer using the same policy ID as the reference_script_locator. No additional configuration is needed in the relayer config.

After deployment, you can:
- List pending deferred messages: `hyperlane-cardano deferred list --recipient <address>`
- Process a deferred message: `hyperlane-cardano deferred process --recipient <address> --message-utxo <utxo>`

### 6.4 Register Recipient

After deploying a recipient (built-in or custom), register it in the registry:

**For generic recipients:**

```bash
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  registry register \
  --script-hash 931e71c75bd0ac35ff9024b3c2a578e006bf3abca509c11734f7f9bc \
  --state-policy f2e541ac484fc08eb2c0d8240a126d33a38316594a98343c768b0ab7 \
  --state-asset "" \
  --ref-script-policy f2e541ac484fc08eb2c0d8240a126d33a38316594a98343c768b0ab7 \
  --ref-script-asset 726566 \
  --recipient-type generic
```

**For deferred recipients (requires `--message-policy`):**

```bash
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  registry register \
  --script-hash d00c07baf0e1aa1d5b2362ad6d4acbd443367167517781e4d12ff6f4 \
  --state-policy 90440110a4ff0daf3d8ba1fbe3178d6f5af03b8b09ebc144f6a10f52 \
  --state-asset "" \
  --ref-script-policy 90440110a4ff0daf3d8ba1fbe3178d6f5af03b8b09ebc144f6a10f52 \
  --ref-script-asset 726566 \
  --recipient-type deferred \
  --message-policy 0a289423f18f05d5d0bc46176c3c09a4a626a81078f0ba5c59bbb47c
```

Parameters:
- `--script-hash`: Recipient validator hash (28 bytes)
- `--state-policy`: State NFT policy ID for finding the state UTXO
- `--state-asset`: Asset name within policy (empty for unit token)
- `--ref-script-policy`: Reference script NFT policy (optional)
- `--ref-script-asset`: Reference script NFT asset name (optional)
- `--recipient-type`: One of `generic`, `token-receiver`, `deferred`
- `--message-policy`: Message NFT minting policy (**required for deferred recipients**)

### 6.5 Verify Registration

```bash
./cli/target/release/hyperlane-cardano \
  --network $NETWORK \
  registry list
```

---

## Verification & Troubleshooting

### Query Commands

```bash
# Query mailbox state
./cli/target/release/hyperlane-cardano --network $NETWORK query mailbox

# Query ISM configuration
./cli/target/release/hyperlane-cardano --network $NETWORK query ism

# Query UTXOs at an address
./cli/target/release/hyperlane-cardano --network $NETWORK query utxos <address>

# Query specific UTXO
./cli/target/release/hyperlane-cardano --network $NETWORK query utxo <tx_hash>#<index>

# Get protocol parameters
./cli/target/release/hyperlane-cardano --network $NETWORK query params
```

### Common Issues

#### "UTXO not found"

**Cause**: Blockfrost cache may be stale after recent transactions.

**Solution**: Wait 30-60 seconds and retry.

#### "PlutusFailure" on transaction

**Causes**:
- Wrong owner: Signing key doesn't match contract owner
- Invalid datum: Datum structure doesn't match expected
- Script hash mismatch: Using wrong script version

**Solution**: Check signing key matches owner in datum, verify script hashes.

#### "BadInputsUTxO" error

**Cause**: UTXO was already spent in another transaction.

**Solution**: Query current UTXOs and retry with updated references.

#### "InsufficientCollateral"

**Cause**: Collateral UTXO doesn't have enough ADA.

**Solution**: Ensure collateral UTXO has at least 5 ADA and no other tokens.

#### Parameter application fails

**Cause**: Aiken not installed or wrong version.

**Solution**:
```bash
# Check aiken is in PATH
which aiken

# Verify version
aiken --version  # Should be v1.0.0+

# If not found, reinstall
curl -sSfL https://install.aiken-lang.org | bash
```

---

## Complete Deployment Script

Here's a complete script for deploying all contracts:

```bash
#!/bin/bash
set -e

# Configuration
export NETWORK="preview"
export BLOCKFROST_API_KEY="your_api_key_here"
export CARDANO_SIGNING_KEY="./keys/payment.skey"
export LOCAL_DOMAIN=2003
export ORIGIN_DOMAINS="43113,11155111"  # Fuji, Sepolia

CLI="./cli/target/release/hyperlane-cardano"
DEPLOY_DIR="./deployments/$NETWORK"

echo "=== Hyperlane Cardano Deployment ==="
echo "Network: $NETWORK"
echo "Domain: $LOCAL_DOMAIN"
echo ""

# Step 1: Build contracts
echo "Step 1: Building contracts..."
cd contracts && aiken build && cd ..

# Step 2: Extract validators
echo "Step 2: Extracting validators..."
$CLI --network $NETWORK deploy extract --output $DEPLOY_DIR

# Step 3: Initialize core contracts (applies parameters)
echo "Step 3: Initializing core contracts..."
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
$CLI --signing-key $CARDANO_SIGNING_KEY --network $NETWORK \
  init all \
  --domain $LOCAL_DOMAIN \
  --origin-domains "$ORIGIN_DOMAINS"

echo "Waiting for confirmation..."
sleep 30

# Step 4: Deploy reference scripts (must be after init to use parameterized scripts)
echo "Step 4: Deploying reference scripts..."
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
$CLI --signing-key $CARDANO_SIGNING_KEY --network $NETWORK \
  deploy reference-scripts-all

echo "Waiting for confirmation..."
sleep 30

# Step 5: Configure mailbox with ISM
echo "Step 5: Configuring mailbox..."
ISM_HASH=$(cat $DEPLOY_DIR/multisig_ism.hash)
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
$CLI --signing-key $CARDANO_SIGNING_KEY --network $NETWORK \
  mailbox set-default-ism --ism-hash $ISM_HASH

sleep 30

# Step 6: Verify deployment
echo "Step 6: Verifying deployment..."
$CLI --network $NETWORK init status
$CLI --network $NETWORK mailbox show
$CLI --network $NETWORK ism show

echo ""
echo "=== Deployment Complete ==="
echo "Deployment info saved to: $DEPLOY_DIR/deployment_info.json"
```

---

## Appendix: Contract Addresses (Preview Testnet)

After deployment, your `deployment_info.json` will contain addresses like:

```json
{
  "network": "preview",
  "mailbox": {
    "hash": "f01158af16d6f625eae141c3d495d0f57913847ca87ebd6bfdc4a719",
    "address": "addr_test1wrsyg4dutwkky0jhzp8pa4943at0yncepugm0hdhlsg2wvq8xs6nu",
    "state_nft_policy": "...",
    "utxo": "..."
  },
  "ism": {
    "hash": "02993c46cdcf8eb56ada209e277acc288dc0263b6a502d17b8cbfa56",
    "address": "addr_test1wp5n85yxm8u3addtdsn8n8hevcfzxcpxmd492z4hmzl7jkstj8kld",
    "state_nft_policy": "..."
  },
  "registry": {
    "hash": "b46f18719b2d20b87474eb9cd761d82f1d7f750548eed38e775d2caf",
    "address": "addr_test1wrg0vpes5mty9cup6wh8x6mpmpght0aw92fwda384za9e0sj95vw5",
    "state_nft_policy": "..."
  }
}
```

---

## Appendix: Domain IDs

| Chain | Domain ID |
|-------|-----------|
| Cardano Mainnet | 2001 |
| Cardano Preprod | 2002 |
| Cardano Preview | 2003 |
| Ethereum Mainnet | 1 |
| Ethereum Sepolia | 11155111 |
| Avalanche Fuji | 43113 |
| Polygon Mumbai | 80001 |

---

## Appendix: CLI Command Reference

### Deploy Commands

| Command | Description |
|---------|-------------|
| `deploy extract` | Extract validators from plutus.json |
| `deploy info` | Show validator information |
| `deploy generate-config` | Generate deployment configuration |
| `deploy reference-script` | Deploy single reference script |
| `deploy reference-scripts-all` | Deploy all core reference scripts |

### Init Commands

| Command | Description |
|---------|-------------|
| `init mailbox` | Initialize mailbox contract |
| `init ism` | Initialize multisig ISM |
| `init registry` | Initialize registry |
| `init recipient` | Initialize a recipient contract |
| `init all` | Initialize all core contracts |
| `init status` | Show initialization status |

### Mailbox Commands

| Command | Description |
|---------|-------------|
| `mailbox set-default-ism` | Update default ISM |
| `mailbox show` | Display current configuration |

### ISM Commands

| Command | Description |
|---------|-------------|
| `ism set-validators` | Set validators for a domain |
| `ism set-threshold` | Set threshold for a domain |
| `ism show` | Display configuration |
| `ism add-validator` | Add a single validator |
| `ism remove-validator` | Remove a validator |

### Registry Commands

| Command | Description |
|---------|-------------|
| `registry register` | Register a new recipient |
| `registry list` | List all registered recipients |
| `registry show` | Show specific recipient details |
| `registry remove` | Remove a registration |

### Query Commands

| Command | Description |
|---------|-------------|
| `query mailbox` | Query mailbox state |
| `query ism` | Query ISM configuration |
| `query utxos` | List UTXOs at an address |
| `query utxo` | Query specific UTXO |
| `query params` | Get protocol parameters |
| `query tip` | Get latest slot |

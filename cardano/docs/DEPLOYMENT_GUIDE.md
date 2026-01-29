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
9. [Phase 7: Deploy Warp Routes](#phase-7-deploy-warp-routes)
10. [Verification & Troubleshooting](#verification--troubleshooting)
11. [Complete Deployment Script](#complete-deployment-script)
12. [Appendix: Script Parameterization](#appendix-script-parameterization)
13. [Appendix: Warp Route Architecture](#appendix-warp-route-architecture)

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

| Operation                   | Minimum ADA Required |
| --------------------------- | -------------------- |
| Reference script deployment | ~15 ADA per script   |
| Contract initialization     | ~10 ADA per contract |
| Total recommended           | ~100 ADA             |

---

## Contract Overview & Dependencies

### Core Contracts

| Contract                  | Purpose                      | Parameters                    | Dependencies          |
| ------------------------- | ---------------------------- | ----------------------------- | --------------------- |
| **state_nft**             | Unique NFT minting policy    | UTXO reference                | None                  |
| **mailbox**               | Message dispatch/process hub | processed_messages_nft_policy | processed_message_nft |
| **multisig_ism**          | Signature verification       | None                          | None                  |
| **registry**              | Recipient metadata store     | None                          | None                  |
| **processed_message_nft** | Replay prevention            | mailbox_policy_id             | mailbox (state NFT)   |

> **Note**: The mailbox validator is parameterized with `processed_messages_nft_policy`, which is the minting policy for processed message NFTs. These NFTs provide replay protection by marking each message_id as processed. The `processed_message_nft` policy is parameterized by `mailbox_policy_id` (stable across upgrades) to ensure replay protection persists even when the mailbox code is updated. See [Appendix: Script Parameterization](#appendix-script-parameterization) for details.

### Recipient Contracts

| Contract                      | Purpose                 | Parameters        | Dependencies |
| ----------------------------- | ----------------------- | ----------------- | ------------ |
| **example_generic_recipient** | Example message handler | mailbox_policy_id | mailbox      |
| **warp_route**                | Token bridge            | mailbox_policy_id | mailbox      |

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
"synthetic_minting.synthetic_minting.mint"
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
    mailboxReferenceScriptUtxo: '<tx_hash>#0'
    ismReferenceScriptUtxo: '<tx_hash>#0'
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

## Phase 7: Deploy Warp Routes

Warp routes are token bridge contracts that enable cross-chain token transfers via Hyperlane. Cardano supports three types of warp routes, each serving different use cases.

### 7.1 Warp Route Types Overview

| Type           | Use Case                         | Cardano Outbound           | Cardano Inbound                 |
| -------------- | -------------------------------- | -------------------------- | ------------------------------- |
| **Native**     | Bridge native ADA                | Locks ADA in state UTXO    | Releases ADA from state UTXO    |
| **Collateral** | Bridge existing Cardano tokens   | Locks tokens in state UTXO | Releases tokens from state UTXO |
| **Synthetic**  | Receive tokens from other chains | Burns synthetic tokens     | Mints synthetic tokens          |

#### Token Type Decision Matrix

```
Do you want to bridge...

┌─ Native ADA (lovelace)?
│  └─ YES → Use NATIVE warp route
│
├─ An existing Cardano token (e.g., HOSKY, MIN)?
│  └─ YES → Use COLLATERAL warp route
│
└─ Tokens from another chain (e.g., USDC from Ethereum)?
   └─ YES → Use SYNTHETIC warp route
```

### 7.2 Deploy Native Warp Route

The native warp route locks/releases ADA for cross-chain transfers.

```bash
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  warp deploy \
  --token-type native \
  --decimals 6 \
  --remote-decimals 18
```

**Parameters:**

- `--token-type native`: Specifies this is a native ADA warp route
- `--decimals 6`: ADA has 6 decimal places (1 ADA = 1,000,000 lovelace)
- `--remote-decimals 18`: EVM chains typically use 18 decimals

**Output:**

```
Warp route deployed!
  Type: Native
  Script Hash: a09ef754bfd03a4b8c48576718c30bbdc140ed45ff467cbc05924920
  NFT Policy: 7c90fa689949238c5cb56c20caa92d50ae05074837e5006314e8a849
  Address: addr_test1wzsfaa65hlgr5juvfptkwxxrpw7uzs8dghl5vl9uqkfyjgq065p09
  Reference Script UTXO: 0c943c58891bc22680b3003d7d152757562aafb8df51de458085c70e9c0b8130#1
  Hyperlane Address: 0x02000000a09ef754bfd03a4b8c48576718c30bbdc140ed45ff467cbc05924920

Deployment saved to: deployments/preview/native_warp_route.json
```

The Hyperlane address (H256 format) is used when enrolling this route on remote chains.

### 7.3 Deploy Collateral Warp Route

The collateral warp route locks existing Cardano tokens for cross-chain transfers. Tokens are held directly in the warp route's state UTXO (no separate vault needed).

```bash
# Replace with your token's policy ID and asset name
TOKEN_POLICY="908d51752e4c76fe1404a92b1276b1c1093dae0c7f302c5442f0177e"
TOKEN_ASSET="WARPTEST"  # ASCII or hex-encoded

BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  warp deploy \
  --token-type collateral \
  --token-policy $TOKEN_POLICY \
  --token-asset $TOKEN_ASSET \
  --decimals 6 \
  --remote-decimals 18
```

**Output:**

```
Warp route deployed!
  Type: Collateral
  Token: 908d51752e4c76fe1404a92b1276b1c1093dae0c7f302c5442f0177e.WARPTEST
  Script Hash: a51328c262339f2860854c1f704ed7c43053587bb4d65393b4e468f8
  NFT Policy: b6a3f69a99b75d852f689b5d1405c7cd76b298fc5ff7db36941b1dc1
  Reference Script UTXO: 476a73b0a697dadf13ddd0dd8139b19694bae4e8a0984ede7780201623940921#1
  Hyperlane Address: 0x02000000a51328c262339f2860854c1f704ed7c43053587bb4d65393b4e468f8

Deployment saved to: deployments/preview/collateral_warp_route.json
```

#### Fund the Warp Route (Optional but Recommended)

For the warp route to release tokens on inbound transfers, its state UTXO must have liquidity. Send tokens directly to the warp route address:

```bash
# The tokens will be held in the warp route's state UTXO
# Use cardano-cli or another wallet to send tokens to the warp route address
```

> **Note**: Unlike EVM warp routes that use separate vault contracts, Cardano collateral warp routes hold locked tokens directly in the state UTXO. This simplifies the architecture and reduces transaction costs.

### 7.4 Deploy Synthetic Warp Route

The synthetic warp route mints/burns synthetic tokens representing assets from other chains.

#### Step 1: Deploy the Warp Route

```bash
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  warp deploy \
  --token-type synthetic \
  --decimals 18 \
  --remote-decimals 18
```

**Parameters:**

- `--decimals 18`: Use the same decimals as the source chain for synthetic tokens
- No token policy needed - the synthetic minting policy is generated automatically

**Output:**

```
Warp route deployed!
  Type: Synthetic
  Script Hash: 2bc528ef916747a2f320107be4bade841fc114dfa8aa9ab473f8f9d9
  NFT Policy: fc0d436644772ca43b9374f9e7a3dd298609099b4af7309f49bf60c1
  Synthetic Minting Policy: 91d297366830695e0688f01f3f704c9e45a2356574f3827e26768032
  Reference Script UTXO: eca38472b3d7f97201dfe62df753b1ac47a4fc6b31ae81dd139e4e8bdb35844d#1
  Hyperlane Address: 0x020000002bc528ef916747a2f320107be4bade841fc114dfa8aa9ab473f8f9d9

Deployment saved to: deployments/preview/synthetic_warp_route.json
```

#### Step 2: Deploy Synthetic Minting Reference Script

For the relayer to mint synthetic tokens when processing inbound messages, the minting policy must be deployed as a reference script:

```bash
WARP_POLICY="fc0d436644772ca43b9374f9e7a3dd298609099b4af7309f49bf60c1"

BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  warp deploy-minting-ref \
  --warp-policy $WARP_POLICY
```

**Output:**

```
Minting policy reference script deployed!
  UTXO: 5678efgh...#0
  NFT: fc0d436644772ca43b9374f9e7a3dd298609099b4af7309f49bf60c1.6d696e745f726566
```

> **Important**: This step is required for inbound synthetic token transfers to work. Without the minting reference script, the relayer cannot mint synthetic tokens.

### 7.5 Register Warp Route in Registry

After deployment, register the warp route in the Hyperlane registry so the relayer can discover it.

#### Register Native Warp Route

```bash
# Values from native_warp_route.json
SCRIPT_HASH="a09ef754bfd03a4b8c48576718c30bbdc140ed45ff467cbc05924920"
NFT_POLICY="7c90fa689949238c5cb56c20caa92d50ae05074837e5006314e8a849"

BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  registry register \
  --script-hash $SCRIPT_HASH \
  --state-policy $NFT_POLICY \
  --state-asset "" \
  --ref-script-policy $NFT_POLICY \
  --ref-script-asset 726566 \
  --recipient-type token-receiver
```

**Parameters:**

- `--state-asset ""`: Empty string for the state UTXO NFT
- `--ref-script-asset 726566`: Hex encoding of "ref" for the reference script UTXO
- `--recipient-type token-receiver`: Indicates this is a warp route (not a generic recipient)

#### Register Collateral Warp Route

```bash
# Values from collateral_warp_route.json
SCRIPT_HASH="a51328c262339f2860854c1f704ed7c43053587bb4d65393b4e468f8"
NFT_POLICY="b6a3f69a99b75d852f689b5d1405c7cd76b298fc5ff7db36941b1dc1"

BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  registry register \
  --script-hash $SCRIPT_HASH \
  --state-policy $NFT_POLICY \
  --state-asset "" \
  --ref-script-policy $NFT_POLICY \
  --ref-script-asset 726566 \
  --recipient-type token-receiver
```

> **Note**: Collateral warp routes don't need additional inputs - tokens are held directly in the state UTXO.

#### Register Synthetic Warp Route

```bash
# Values from synthetic_warp_route.json
SCRIPT_HASH="2bc528ef916747a2f320107be4bade841fc114dfa8aa9ab473f8f9d9"
NFT_POLICY="fc0d436644772ca43b9374f9e7a3dd298609099b4af7309f49bf60c1"

BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  registry register \
  --script-hash $SCRIPT_HASH \
  --state-policy $NFT_POLICY \
  --state-asset "" \
  --ref-script-policy $NFT_POLICY \
  --ref-script-asset 726566 \
  --recipient-type token-receiver
```

### 7.6 Enroll Remote Routers

For bidirectional transfers, you must enroll the remote chain's warp route address on the Cardano side.

```bash
# Enroll Fuji warp route on Cardano
REMOTE_DOMAIN=43113  # Fuji domain ID
REMOTE_ROUTER="0x0000000000000000000000001ac0c9eeb284b7ddf83c973662abc0d20e3ae868"  # Fuji warp route address (H256)
WARP_POLICY="7c90fa689949238c5cb56c20caa92d50ae05074837e5006314e8a849"  # Cardano warp route NFT policy

BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  warp enroll-router \
  --domain $REMOTE_DOMAIN \
  --router $REMOTE_ROUTER \
  --warp-policy $WARP_POLICY
```

**Parameters:**

- `--domain`: The remote chain's domain ID
- `--router`: The remote warp route contract address in H256 format (32 bytes, padded)
- `--warp-policy`: The local Cardano warp route's NFT policy ID

> **Important**: You must also enroll the Cardano warp route on the remote chain. Use the Hyperlane address from the deployment output (e.g., `0x02000000a09ef754...`).

### 7.7 Verify Warp Route Deployment

```bash
# Show warp route configuration
./cli/target/release/hyperlane-cardano \
  --network $NETWORK \
  warp show \
  --warp-policy $WARP_POLICY

# List enrolled routers
./cli/target/release/hyperlane-cardano \
  --network $NETWORK \
  warp routers \
  --warp-policy $WARP_POLICY
```

### 7.8 Test Warp Route Transfer (E2E Testing)

> **Prerequisites**: Before testing transfers, ensure the Hyperlane validator and relayer agents are running and properly configured. See [Appendix: Agent Configuration Requirements](#appendix-agent-configuration-requirements) for setup instructions, including how to extract required addresses from your deployment files.

This section provides comprehensive end-to-end testing procedures for all warp route types. We cover three main test scenarios:

| Test | Cardano Type | Remote Type | Description                                 |
| ---- | ------------ | ----------- | ------------------------------------------- |
| 1    | Native (ADA) | Synthetic   | Lock ADA on Cardano, mint wADA on remote    |
| 2    | Native (ADA) | Synthetic   | Burn wADA on remote, release ADA on Cardano |
| 3    | Synthetic    | Collateral  | Bidirectional synthetic token transfers     |

#### Common Setup

Set up environment variables for testing:

```bash
# CLI path
CLI="./cli/target/release/hyperlane-cardano"

# Cardano configuration
export NETWORK="preview"
export BLOCKFROST_API_KEY="your_blockfrost_api_key"
export CARDANO_SIGNING_KEY="/path/to/payment.skey"

# Remote chain configuration (Fuji example)
export FUJI_RPC_URL="https://api.avax-test.network/ext/bc/C/rpc"
export FUJI_SIGNER_KEY="0xyour_private_key"
export FUJI_SIGNER_ADDRESS="0xYourAddress"

# Domain IDs
CARDANO_DOMAIN=2003       # Cardano Preview
FUJI_DOMAIN=43113         # Avalanche Fuji
```

#### Get Cardano Recipient Address (H256 Format)

For inbound transfers to Cardano, you need your address in H256 format:

```bash
# 1. Get your wallet's bech32 address
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
$CLI --signing-key $CARDANO_SIGNING_KEY --network $NETWORK utxo list
# Example output: addr_test1vqfp9gpr8qqzp7x8h99cx8j90w0wvhcqnhuar4vggvxuezg4hvheh

# 2. Extract the payment credential using cardano-cli
cardano-cli address info --address addr_test1vqfp9gpr8qqzp7x8h99cx8j90w0wvhcqnhuar4vggvxuezg4hvheh
# Output: { "base16": "601212a023380020f8c7b94b831e457b9ee65f009df9d1d588430dcc89", ... }

# 3. Convert to H256 format:
#    - Remove first byte (2 hex chars) = network/type indicator
#    - Add "0x00000000" prefix (4 zero bytes for pubkey hash addresses)
CARDANO_RECIPIENT="0x000000001212a023380020f8c7b94b831e457b9ee65f009df9d1d588430dcc89"
```

---

#### Test 1: Native Warp Route — Cardano → Remote (Outbound)

This test locks ADA on Cardano and mints synthetic wADA on the remote chain.

**Prerequisites:**

- Native warp route deployed on Cardano (see section 7.2)
- Synthetic warp route deployed on remote chain
- Both routes enrolled with each other's router addresses
- Relayer and validator agents running

**Step 1: Load warp route configuration**

```bash
# Read deployment info
NATIVE_WARP=$(cat deployments/$NETWORK/native_warp_route.json)
WARP_POLICY=$(echo $NATIVE_WARP | jq -r '.warp_route.nft_policy')
echo "Warp Policy: $WARP_POLICY"
```

**Step 2: Check your ADA balance**

```bash
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
$CLI --signing-key $CARDANO_SIGNING_KEY --network $NETWORK utxo list
```

**Step 3: Execute the transfer**

```bash
# Transfer 10 ADA to Fuji
# Note: Amount is in lovelace (1 ADA = 1,000,000 lovelace)
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
$CLI --signing-key $CARDANO_SIGNING_KEY --network $NETWORK \
  warp transfer \
  --warp-policy $WARP_POLICY \
  --domain $FUJI_DOMAIN \
  --recipient "0x000000000000000000000000$FUJI_SIGNER_ADDRESS" \
  --amount 10000000
```

**Expected output:**

```
Transfer initiated!
  Transaction: abc123...
  Message ID: 0x1234567890abcdef...
  Sender: 0x020000001212a023380020f8c7b94b831e457b9ee65f009df9d1d588430dcc89
  Recipient: 0x0000000000000000000000001f26bfc6f52cbfad5c3fa8dabb71007b28bf4749
  Amount: 10000000 (10.000000 local units → 10.000000000000000000 remote units)
```

**Step 4: Monitor the relayer**

```bash
docker compose -f e2e-docker/docker-compose.yml logs -f relayer
```

Look for:

- `Dispatched message to destination` - Message indexed on Cardano
- `Message successfully processed` - Delivery confirmed on Fuji

**Step 5: Verify receipt on Fuji**

```bash
# Check wADA balance on Fuji (should show 10 * 10^18 = 10000000000000000000)
cast call $FUJI_WARP_ROUTE "balanceOf(address)(uint256)" $FUJI_SIGNER_ADDRESS --rpc-url $FUJI_RPC_URL
```

---

#### Test 2: Native Warp Route — Remote → Cardano (Inbound)

This test burns synthetic wADA on the remote chain and releases ADA on Cardano.

**Prerequisites:**

- Completed Test 1 (have wADA tokens on Fuji)
- Same infrastructure running

**Step 1: Check wADA balance on Fuji**

```bash
# Get token info
cast call $FUJI_WARP_ROUTE "symbol()(string)" --rpc-url $FUJI_RPC_URL
cast call $FUJI_WARP_ROUTE "decimals()(uint8)" --rpc-url $FUJI_RPC_URL

# Check balance (should have tokens from Test 1)
cast call $FUJI_WARP_ROUTE "balanceOf(address)(uint256)" $FUJI_SIGNER_ADDRESS --rpc-url $FUJI_RPC_URL
```

**Step 2: Get interchain gas quote**

```bash
GAS_QUOTE=$(cast call $FUJI_WARP_ROUTE \
  "quoteGasPayment(uint32)(uint256)" $CARDANO_DOMAIN \
  --rpc-url $FUJI_RPC_URL)
echo "Gas quote: $GAS_QUOTE wei ($(echo "scale=6; $GAS_QUOTE / 1000000000000000000" | bc) AVAX)"
```

**Step 3: Execute the transfer**

```bash
# Transfer 5 wADA back to Cardano
# Amount is in wei (5 wADA with 18 decimals = 5 * 10^18)
cast send $FUJI_WARP_ROUTE \
  "transferRemote(uint32,bytes32,uint256)" \
  $CARDANO_DOMAIN \
  $CARDANO_RECIPIENT \
  5000000000000000000 \
  --value $GAS_QUOTE \
  --rpc-url $FUJI_RPC_URL \
  --private-key $FUJI_SIGNER_KEY
```

**Step 4: Monitor the relayer**

```bash
docker compose -f e2e-docker/docker-compose.yml logs -f relayer
```

Look for:

- `Fetching metadata for message` - Relayer detected the Fuji message
- `Transaction is finalized` - Cardano transaction confirmed

**Step 5: Verify receipt on Cardano**

```bash
# Check UTXOs - should see 5 ADA returned to your wallet
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
$CLI --signing-key $CARDANO_SIGNING_KEY --network $NETWORK utxo list
```

---

#### Test 3: Synthetic Warp Route — Bidirectional Testing

This test demonstrates the synthetic warp route where Cardano mints/burns synthetic tokens backed by collateral locked on the remote chain.

**Route configuration:**

- **Fuji**: Collateral warp route (locks ERC20 tokens)
- **Cardano**: Synthetic warp route (mints/burns synthetic tokens)

**Prerequisites:**

- Collateral warp route deployed on Fuji with an ERC20 token
- Synthetic warp route deployed on Cardano (max 6 decimals)
- Both routes enrolled with each other's router addresses
- Minting policy reference script deployed and registered

##### Test 3a: Remote → Cardano (Mint Synthetic)

**Step 1: Load configuration**

```bash
# Read Cardano synthetic warp route config
SYNTH_WARP=$(cat deployments/$NETWORK/synthetic_warp_route.json)
SYNTH_POLICY=$(echo $SYNTH_WARP | jq -r '.synthetic_policy')
SYNTH_DECIMALS=$(echo $SYNTH_WARP | jq -r '.decimals')
echo "Synthetic Policy: $SYNTH_POLICY"
echo "Decimals: $SYNTH_DECIMALS"

# Fuji collateral warp route address
FUJI_COLLATERAL_WARP="0xYourCollateralWarpRouteAddress"
```

**Step 2: Approve token spending on Fuji**

```bash
# Get the ERC20 token address from the collateral warp route
TOKEN_ADDRESS=$(cast call $FUJI_COLLATERAL_WARP "wrappedToken()(address)" --rpc-url $FUJI_RPC_URL)

# Approve the warp route to spend tokens
cast send $TOKEN_ADDRESS \
  "approve(address,uint256)" \
  $FUJI_COLLATERAL_WARP \
  1000000000000000000000 \
  --rpc-url $FUJI_RPC_URL \
  --private-key $FUJI_SIGNER_KEY
```

**Step 3: Transfer tokens to Cardano**

```bash
# Get gas quote
GAS_QUOTE=$(cast call $FUJI_COLLATERAL_WARP \
  "quoteGasPayment(uint32)(uint256)" $CARDANO_DOMAIN \
  --rpc-url $FUJI_RPC_URL)

# Transfer 10 tokens to Cardano
# Note: If Fuji token has 18 decimals and Cardano has 6, relayer handles conversion
cast send $FUJI_COLLATERAL_WARP \
  "transferRemote(uint32,bytes32,uint256)" \
  $CARDANO_DOMAIN \
  $CARDANO_RECIPIENT \
  10000000000000000000 \
  --value $GAS_QUOTE \
  --rpc-url $FUJI_RPC_URL \
  --private-key $FUJI_SIGNER_KEY
```

**Step 4: Monitor and verify**

```bash
# Monitor relayer
docker compose -f e2e-docker/docker-compose.yml logs -f relayer

# After delivery, check synthetic tokens on Cardano
# The amount should be 10,000,000 (10 tokens with 6 decimals)
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
$CLI --signing-key $CARDANO_SIGNING_KEY --network $NETWORK utxo list
# Look for tokens with policy ID matching $SYNTH_POLICY
```

##### Test 3b: Cardano → Remote (Burn Synthetic)

**Step 1: Load warp route configuration**

```bash
SYNTH_WARP=$(cat deployments/$NETWORK/synthetic_warp_route.json)
WARP_POLICY=$(echo $SYNTH_WARP | jq -r '.warp_route.nft_policy')
```

**Step 2: Execute the transfer**

```bash
# Transfer 5 synthetic tokens back to Fuji
# Amount is in Cardano units (5 tokens with 6 decimals = 5,000,000)
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
$CLI --signing-key $CARDANO_SIGNING_KEY --network $NETWORK \
  warp transfer \
  --warp-policy $WARP_POLICY \
  --domain $FUJI_DOMAIN \
  --recipient "0x000000000000000000000000$FUJI_SIGNER_ADDRESS" \
  --amount 5000000
```

**Expected output:**

```
Transfer initiated!
  Transaction: def456...
  Message ID: 0xabcdef123456...
  Amount: 5000000 (5.000000 local units → 5.000000000000000000 remote units)
```

**Step 3: Monitor and verify**

```bash
# Monitor relayer
docker compose -f e2e-docker/docker-compose.yml logs -f relayer

# After delivery, check collateral tokens released on Fuji
cast call $TOKEN_ADDRESS "balanceOf(address)(uint256)" $FUJI_SIGNER_ADDRESS --rpc-url $FUJI_RPC_URL
```

---

#### Troubleshooting E2E Tests

**Common Issues:**

| Error                         | Cause                                          | Solution                                                                      |
| ----------------------------- | ---------------------------------------------- | ----------------------------------------------------------------------------- |
| `MissingScriptWitnessesUTXOW` | Reference scripts not found                    | Deploy minting ref script (for synthetic) and verify registry entries         |
| `BabbageNonDisjointRefInputs` | Same UTXO used as reference and spending input | Check registry `--ref-script-asset` is correct (should be `726566` for "ref") |
| `RecipientNotFound`           | Registry entry not indexed yet                 | Wait for Blockfrost to index the registration transaction                     |
| `InsufficientBalance`         | Not enough tokens/ADA                          | Check UTXO balances before transfer                                           |
| `NoRelayerActivity`           | Relayer not detecting messages                 | Check relayer logs, verify domain configuration                               |
| `GasPaymentFailed`            | Insufficient AVAX for gas                      | Ensure adequate AVAX balance for `--value` parameter                          |

**Checking Message Status:**

1. **On Cardano (outbound):** Check that the dispatch transaction was confirmed and note the message ID
2. **On Fuji (outbound delivery):** Use Hyperlane Explorer or query the mailbox contract
3. **On relayer:** Look for message indexing and delivery logs

**Verifying Relayer Configuration:**

```bash
# Check relayer is properly configured for both domains
docker compose -f e2e-docker/docker-compose.yml exec relayer cat /config/relayer.json

# Verify required fields:
# - Cardano chain with correct mailbox, ISM, and warp route addresses
# - Fuji chain with correct RPC URL and contract addresses
# - Signing keys for both chains
```

**Decimal Handling:**

Cardano warp routes support a maximum of 6 decimals due to the i64 token amount limit. When bridging to/from chains with higher decimals (e.g., 18 on EVM):

- **Outbound (Cardano → EVM):** Amount is scaled up (e.g., 1,000,000 → 1,000,000,000,000,000,000)
- **Inbound (EVM → Cardano):** Amount is scaled down (e.g., 1,000,000,000,000,000,000 → 1,000,000)

Ensure your warp route is configured with correct `decimals` and `remote_decimals` values during deployment.

### 7.9 Complete Warp Route Deployment Script

```bash
#!/bin/bash
set -e

# Configuration
export NETWORK="preview"
export BLOCKFROST_API_KEY="your_api_key_here"
export CARDANO_SIGNING_KEY="./keys/payment.skey"

CLI="./cli/target/release/hyperlane-cardano"

# Fuji configuration (example remote chain)
FUJI_DOMAIN=43113
FUJI_WARP_ROUTE="0x0000000000000000000000001ac0c9eeb284b7ddf83c973662abc0d20e3ae868"

echo "=== Warp Route Deployment ==="

# 1. Deploy Native ADA warp route
echo "Deploying native ADA warp route..."
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
$CLI --signing-key $CARDANO_SIGNING_KEY --network $NETWORK \
  warp deploy \
  --token-type native \
  --decimals 6 \
  --remote-decimals 18

sleep 30

# 2. Get deployed warp route info
NATIVE_WARP=$(cat deployments/$NETWORK/native_warp_route.json)
NATIVE_SCRIPT_HASH=$(echo $NATIVE_WARP | jq -r '.warp_route.script_hash')
NATIVE_NFT_POLICY=$(echo $NATIVE_WARP | jq -r '.warp_route.nft_policy')

# 3. Register in registry
echo "Registering warp route..."
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
$CLI --signing-key $CARDANO_SIGNING_KEY --network $NETWORK \
  registry register \
  --script-hash $NATIVE_SCRIPT_HASH \
  --state-policy $NATIVE_NFT_POLICY \
  --state-asset "" \
  --ref-script-policy $NATIVE_NFT_POLICY \
  --ref-script-asset 726566 \
  --recipient-type token-receiver

sleep 30

# 4. Enroll remote router
echo "Enrolling remote router..."
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
$CLI --signing-key $CARDANO_SIGNING_KEY --network $NETWORK \
  warp enroll-router \
  --domain $FUJI_DOMAIN \
  --router $FUJI_WARP_ROUTE \
  --warp-policy $NATIVE_NFT_POLICY

echo ""
echo "=== Deployment Complete ==="
echo "Cardano Warp Route Address: 0x02000000$NATIVE_SCRIPT_HASH"
echo ""
echo "Next steps:"
echo "1. Enroll the Cardano warp route on Fuji using the address above"
echo "2. Start the relayer with the updated configuration"
echo "3. Test a transfer using: warp transfer --domain $FUJI_DOMAIN ..."
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

| Chain            | Domain ID |
| ---------------- | --------- |
| Cardano Mainnet  | 2001      |
| Cardano Preprod  | 2002      |
| Cardano Preview  | 2003      |
| Ethereum Mainnet | 1         |
| Ethereum Sepolia | 11155111  |
| Avalanche Fuji   | 43113     |
| Polygon Mumbai   | 80001     |

---

## Appendix: CLI Command Reference

### Deploy Commands

| Command                        | Description                         |
| ------------------------------ | ----------------------------------- |
| `deploy extract`               | Extract validators from plutus.json |
| `deploy info`                  | Show validator information          |
| `deploy generate-config`       | Generate deployment configuration   |
| `deploy reference-script`      | Deploy single reference script      |
| `deploy reference-scripts-all` | Deploy all core reference scripts   |

### Init Commands

| Command          | Description                     |
| ---------------- | ------------------------------- |
| `init mailbox`   | Initialize mailbox contract     |
| `init ism`       | Initialize multisig ISM         |
| `init registry`  | Initialize registry             |
| `init recipient` | Initialize a recipient contract |
| `init all`       | Initialize all core contracts   |
| `init status`    | Show initialization status      |

### Mailbox Commands

| Command                   | Description                   |
| ------------------------- | ----------------------------- |
| `mailbox set-default-ism` | Update default ISM            |
| `mailbox show`            | Display current configuration |

### ISM Commands

| Command                | Description                 |
| ---------------------- | --------------------------- |
| `ism set-validators`   | Set validators for a domain |
| `ism set-threshold`    | Set threshold for a domain  |
| `ism show`             | Display configuration       |
| `ism add-validator`    | Add a single validator      |
| `ism remove-validator` | Remove a validator          |

### Registry Commands

| Command             | Description                     |
| ------------------- | ------------------------------- |
| `registry register` | Register a new recipient        |
| `registry list`     | List all registered recipients  |
| `registry show`     | Show specific recipient details |
| `registry remove`   | Remove a registration           |

### Query Commands

| Command         | Description              |
| --------------- | ------------------------ |
| `query mailbox` | Query mailbox state      |
| `query ism`     | Query ISM configuration  |
| `query utxos`   | List UTXOs at an address |
| `query utxo`    | Query specific UTXO      |
| `query params`  | Get protocol parameters  |
| `query tip`     | Get latest slot          |

---

## Appendix: Script Parameterization

### What is Parameterization?

In Aiken (Cardano's smart contract language), validators can be **parameterized** - they accept compile-time parameters that are "baked into" the script bytecode. This is similar to constructor arguments in Solidity, but the parameters become part of the script hash itself.

```aiken
// Example: A validator parameterized by a policy ID
validator my_validator(some_policy_id: PolicyId) {
  spend(datum, redeemer, own_ref, tx) {
    // Can use some_policy_id in validation logic
    ...
  }
}
```

**Key implications:**

- Different parameter values → different script hashes → different addresses
- Parameters are immutable once applied
- The `aiken blueprint apply` command applies parameters to create the final script

### How Parameterization Works

1. **Build contracts**: `aiken build` compiles validators to `plutus.json` with parameters as placeholders
2. **Apply parameters**: `aiken blueprint apply` fills in parameter values, producing the final CBOR bytecode
3. **Deploy**: The parameterized script is deployed as a reference script or used directly

```bash
# Example: Apply mailbox_policy_id to the example_generic_recipient validator
aiken blueprint apply \
  -v example_generic_recipient.example_generic_recipient \
  -o recipient_applied.plutus \
  "6421905a7b782eda294774816c944d1707d0091c3fb84bc71cbf46e7"
```

### Parameterization Dependency Graph

The scripts in Hyperlane-Cardano have dependencies that must be resolved in a specific order:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                      PARAMETERIZATION DEPENDENCY GRAPH                       │
└─────────────────────────────────────────────────────────────────────────────┘

                         ┌─────────────────────┐
                         │   UTXO Reference    │
                         │ (consumed at init)  │
                         └──────────┬──────────┘
                                    │
                                    ▼
                         ┌─────────────────────┐
                         │   state_nft (mint)  │
                         │   One-shot policy   │
                         └──────────┬──────────┘
                                    │
               Creates unique NFT policy IDs for each contract
                                    │
          ┌─────────────────────────┼─────────────────────────┐
          │                         │                         │
          ▼                         ▼                         ▼
┌─────────────────────┐  ┌─────────────────────┐  ┌─────────────────────┐
│  mailbox_policy_id  │  │   ism_policy_id     │  │ registry_policy_id  │
│                     │  │                     │  │                     │
│ Identifies mailbox  │  │ Identifies ISM      │  │ Identifies registry │
│ state UTXO          │  │ state UTXO          │  │ state UTXO          │
└──────────┬──────────┘  └─────────────────────┘  └─────────────────────┘
           │
           │ Used as parameter for:
           │
           ├─────────────────────────────────────────────────────────────┐
           │                                                             │
           ▼                                                             ▼
┌─────────────────────────────┐                    ┌─────────────────────────────┐
│  processed_message_nft      │                    │  stored_message_nft         │
│  (mint)                     │                    │  (mint)                     │
│                             │                    │                             │
│  Parameter: mailbox_policy  │                    │  Parameter: mailbox_policy  │
│                             │                    │                             │
│  Used for: Replay protection│                    │  Used for: Deferred message │
│  (one NFT per message_id)   │                    │  authentication             │
└──────────┬──────────────────┘                    └──────────┬──────────────────┘
           │                                                  │
           │ processed_message_nft_policy                     │ stored_message_nft_policy
           │                                                  │
           ▼                                                  ▼
┌─────────────────────────────┐                    ┌─────────────────────────────┐
│  mailbox (spend)            │                    │  example_deferred_recipient │
│                             │                    │  (spend)                    │
│  Parameter:                 │                    │                             │
│  processed_messages_nft_    │                    │  Parameters:                │
│  policy                     │                    │  - mailbox_policy_id        │
│                             │                    │  - stored_message_nft_policy│
└─────────────────────────────┘                    └─────────────────────────────┘

           │
           │ mailbox_policy_id
           │
           ▼
┌─────────────────────────────┐
│  example_generic_recipient  │
│  (spend)                    │
│                             │
│  Parameter: mailbox_policy  │
│                             │
│  Verifies mailbox is caller │
│  by checking for NFT        │
└─────────────────────────────┘
```

### Script Parameterization Table

| Script                       | Type  | Parameter(s)                                                  | Parameter Source                                     | Purpose                                    |
| ---------------------------- | ----- | ------------------------------------------------------------- | ---------------------------------------------------- | ------------------------------------------ |
| `state_nft`                  | Mint  | `utxo_ref: OutputReference`                                   | Any unspent UTXO                                     | One-shot minting, ensures unique NFT       |
| `mailbox`                    | Spend | `processed_messages_nft_policy: PolicyId`                     | Derived from `processed_message_nft`                 | Replay protection via NFT minting          |
| `multisig_ism`               | Spend | (none)                                                        | -                                                    | No parameters needed                       |
| `registry`                   | Spend | (none)                                                        | -                                                    | No parameters needed                       |
| `processed_message_nft`      | Mint  | `mailbox_policy_id: PolicyId`                                 | `state_nft` policy for mailbox                       | Ensures only mailbox can trigger minting   |
| `stored_message_nft`         | Mint  | `mailbox_policy_id: PolicyId`                                 | `state_nft` policy for mailbox                       | Ensures only mailbox can mint message NFTs |
| `example_generic_recipient`  | Spend | `mailbox_policy_id: PolicyId`                                 | `state_nft` policy for mailbox                       | Verifies mailbox is calling                |
| `example_deferred_recipient` | Spend | `mailbox_policy_id: PolicyId`, `message_nft_policy: PolicyId` | `state_nft` for mailbox, `stored_message_nft` policy | Verifies mailbox and message authenticity  |
| `warp_route`                 | Spend | `mailbox_policy_id: PolicyId`                                 | `state_nft` policy for mailbox                       | Verifies mailbox is calling                |

### Why Stable vs Changing Parameters Matter

**Stable parameters** (like `mailbox_policy_id`) allow contracts to be upgraded without breaking dependencies:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                     STABLE vs CHANGING PARAMETERS                            │
└─────────────────────────────────────────────────────────────────────────────┘

  STABLE: mailbox_policy_id                    CHANGING: mailbox_script_hash
  ───────────────────────────                  ─────────────────────────────

  Initialization:                              Initialization:
  ┌─────────────────────┐                      ┌─────────────────────┐
  │ mailbox_policy_id = │                      │ mailbox_script_hash │
  │ 6421905a7b782eda... │                      │ = a1d95abf5b095036..│
  │ (one-shot, fixed)   │                      │ (from script code)  │
  └──────────┬──────────┘                      └──────────┬──────────┘
             │                                            │
             │ After mailbox upgrade:                     │ After mailbox upgrade:
             │                                            │
             ▼                                            ▼
  ┌─────────────────────┐                      ┌─────────────────────┐
  │ mailbox_policy_id = │                      │ mailbox_script_hash │
  │ 6421905a7b782eda... │  ◄── SAME!           │ = NEW_HASH_xyz...   │  ◄── CHANGED!
  │ (still the same)    │                      │ (code changed)      │
  └─────────────────────┘                      └─────────────────────┘

  Result: Recipients and                       Result: Recipients and
  processed_message_nft                        processed_message_nft
  continue working                             would need redeployment
```

**Critical insight**: `processed_message_nft` is parameterized by `mailbox_policy_id` (stable) rather than `mailbox_script_hash` (changes with code). This ensures:

1. **Replay protection persists across upgrades**: Old processed message NFTs are still recognized
2. **No double-processing**: A message processed before an upgrade cannot be replayed after
3. **No recipient redeployment**: Recipients don't need updating when mailbox code changes

### Deployment Order (Parameterization-Aware)

Due to parameterization dependencies, contracts must be deployed in this specific order:

```
Step 1: Build all contracts
        └─ aiken build → plutus.json

Step 2: Initialize mailbox (creates mailbox_policy_id)
        ├─ Consumes a UTXO → creates unique state_nft policy
        └─ mailbox_policy_id = state_nft policy ID

Step 3: Apply mailbox_policy_id to processed_message_nft
        └─ aiken blueprint apply -v processed_message_nft ... "mailbox_policy_id"
           → processed_message_nft_policy

Step 4: Apply processed_message_nft_policy to mailbox
        └─ aiken blueprint apply -v mailbox ... "processed_message_nft_policy"
           → mailbox_applied.plutus (final mailbox script)

Step 5: Deploy mailbox reference script
        └─ Uses mailbox_applied.plutus

Step 6: Initialize other core contracts (ISM, Registry)
        └─ Each gets its own state_nft policy

Step 7: Deploy recipients
        ├─ Generic: Apply mailbox_policy_id → recipient_applied.plutus
        └─ Deferred: Apply mailbox_policy_id to stored_message_nft
                     Apply both to deferred_recipient
```

### CLI Automation

The Hyperlane CLI automates most parameterization steps. When you run:

```bash
./cli/target/release/hyperlane-cardano init all --domain 2003
```

The CLI internally:

1. Creates state NFT policies for mailbox, ISM, and registry
2. Applies `mailbox_policy_id` to `processed_message_nft`
3. Applies the resulting policy to `mailbox`
4. Saves all parameterized scripts to `deployments/<network>/`

For recipients:

```bash
./cli/target/release/hyperlane-cardano init recipient
```

The CLI:

1. Reads `mailbox_policy_id` from `deployment_info.json`
2. Applies it to the recipient validator
3. Saves the parameterized script

### Manual Parameterization Example

If you need to manually apply parameters (e.g., for custom contracts):

```bash
# 1. Get the mailbox_policy_id from deployment info
MAILBOX_POLICY=$(cat deployments/preview/deployment_info.json | jq -r '.mailbox.state_nft_policy')

# 2. Apply parameter to your custom recipient
cd contracts
aiken blueprint apply \
  -v my_custom_recipient.my_custom_recipient \
  -o ../deployments/preview/my_custom_recipient_applied.plutus \
  "$MAILBOX_POLICY"

# 3. The resulting script hash will differ from the base script
# because the parameter is now embedded in the bytecode
```

---

## Appendix: Agent Configuration Requirements

When configuring the Hyperlane agents (validator and relayer) for Cardano, several environment variables must be set correctly. This section documents all required variables and how to extract them from your deployment.

### Environment Variables Overview

#### Variables Used by Both Validator and Relayer

| Variable                          | Description                           | Source                                    |
| --------------------------------- | ------------------------------------- | ----------------------------------------- |
| `BLOCKFROST_API_KEY`              | Blockfrost API key for Cardano access | Blockfrost dashboard                      |
| `CARDANO_MAILBOX`                 | Mailbox identifier (H256 format)      | `0x00000000` + `.mailbox.stateNftPolicy`  |
| `CARDANO_VALIDATOR_ANNOUNCE`      | Validator announce (H256 format)      | `0x00000000` + `.validator_announce.hash` |
| `CARDANO_MERKLE_TREE_HOOK`        | Merkle tree hook (H256 format)        | `0x00000000` + `.mailbox.hash`            |
| `CARDANO_ISM`                     | ISM identifier (H256 format)          | `0x00000000` + `.ism.stateNftPolicy`      |
| `CARDANO_MAILBOX_POLICY_ID`       | Mailbox state NFT policy              | `.mailbox.stateNftPolicy`                 |
| `CARDANO_MAILBOX_SCRIPT_HASH`     | Mailbox validator script hash         | `.mailbox.hash`                           |
| `CARDANO_MAILBOX_REF_UTXO`        | Mailbox reference script UTXO         | `.mailbox.referenceScriptUtxo`            |
| `CARDANO_ISM_SCRIPT_HASH`         | ISM validator script hash             | `.ism.hash`                               |
| `CARDANO_ISM_STATE_NFT_POLICY_ID` | ISM state NFT policy                  | `.ism.stateNftPolicy`                     |
| `CARDANO_ISM_REF_UTXO`            | ISM reference script UTXO             | `.ism.referenceScriptUtxo`                |
| `CARDANO_REGISTRY_POLICY_ID`      | Registry state NFT policy             | `.registry.stateNftPolicy`                |
| `CARDANO_PROCESSED_MSG_POLICY_ID` | Processed messages NFT policy         | `.mailbox.appliedParameters[0].value`     |
| `CARDANO_VA_POLICY_ID`            | Validator announce policy ID          | `.validator_announce.hash`                |
| `CARDANO_INDEX_FROM`              | Block height to start indexing        | See note below                            |

#### Validator-Only Variables

| Variable                | Description                                 |
| ----------------------- | ------------------------------------------- |
| `AWS_ACCESS_KEY_ID`     | AWS credentials for S3 checkpoint storage   |
| `AWS_SECRET_ACCESS_KEY` | AWS credentials for S3 checkpoint storage   |
| `AWS_REGION`            | AWS region for S3 bucket                    |
| `AWS_S3_BUCKET`         | S3 bucket name for checkpoints              |
| `CARDANO_VALIDATOR_KEY` | ECDSA secp256k1 key for signing checkpoints |

#### Relayer-Only Variables

| Variable                                | Description                                  |
| --------------------------------------- | -------------------------------------------- |
| `CARDANO_SIGNER_KEY`                    | Ed25519 key for Cardano transactions         |
| `CARDANO_PROCESSED_MSG_NFT_SCRIPT_CBOR` | CBOR-encoded processed message NFT script    |
| `FUJI_*`                                | Fuji chain configuration (see Fuji appendix) |

---

### Extracting Variables from deployment_info.json

After deploying Cardano contracts, extract the required values:

```bash
cd cardano/deployments/preview

# H256 Contract Addresses (with 0x00000000 prefix)
export CARDANO_MAILBOX=0x00000000$(jq -r '.mailbox.stateNftPolicy' deployment_info.json)
export CARDANO_VALIDATOR_ANNOUNCE=0x00000000$(jq -r '.validator_announce.hash' deployment_info.json)
export CARDANO_MERKLE_TREE_HOOK=0x00000000$(jq -r '.mailbox.hash' deployment_info.json)
export CARDANO_ISM=0x00000000$(jq -r '.ism.stateNftPolicy' deployment_info.json)

# Policy IDs and Script Hashes
export CARDANO_MAILBOX_POLICY_ID=$(jq -r '.mailbox.stateNftPolicy' deployment_info.json)
export CARDANO_MAILBOX_SCRIPT_HASH=$(jq -r '.mailbox.hash' deployment_info.json)
export CARDANO_ISM_SCRIPT_HASH=$(jq -r '.ism.hash' deployment_info.json)
export CARDANO_ISM_STATE_NFT_POLICY_ID=$(jq -r '.ism.stateNftPolicy' deployment_info.json)
export CARDANO_REGISTRY_POLICY_ID=$(jq -r '.registry.stateNftPolicy' deployment_info.json)
export CARDANO_PROCESSED_MSG_POLICY_ID=$(jq -r '.mailbox.appliedParameters[0].value' deployment_info.json)
export CARDANO_VA_POLICY_ID=$(jq -r '.validator_announce.hash' deployment_info.json)

# Reference Script UTXOs
export CARDANO_MAILBOX_REF_UTXO=$(jq -r '.mailbox.referenceScriptUtxo | "\(.txHash)#\(.outputIndex)"' deployment_info.json)
export CARDANO_ISM_REF_UTXO=$(jq -r '.ism.referenceScriptUtxo | "\(.txHash)#\(.outputIndex)"' deployment_info.json)

# Processed Message NFT Script CBOR (for relayer)
export CARDANO_PROCESSED_MSG_NFT_SCRIPT_CBOR=$(jq -r '.cborHex' ../processed_message_nft_applied.plutus)
```

#### One-liner Export Script

```bash
cd cardano/deployments/preview
eval $(jq -r '
  "export CARDANO_MAILBOX=0x00000000" + .mailbox.stateNftPolicy,
  "export CARDANO_VALIDATOR_ANNOUNCE=0x00000000" + .validator_announce.hash,
  "export CARDANO_MERKLE_TREE_HOOK=0x00000000" + .mailbox.hash,
  "export CARDANO_ISM=0x00000000" + .ism.stateNftPolicy,
  "export CARDANO_MAILBOX_POLICY_ID=" + .mailbox.stateNftPolicy,
  "export CARDANO_MAILBOX_SCRIPT_HASH=" + .mailbox.hash,
  "export CARDANO_ISM_SCRIPT_HASH=" + .ism.hash,
  "export CARDANO_ISM_STATE_NFT_POLICY_ID=" + .ism.stateNftPolicy,
  "export CARDANO_REGISTRY_POLICY_ID=" + .registry.stateNftPolicy,
  "export CARDANO_PROCESSED_MSG_POLICY_ID=" + .mailbox.appliedParameters[0].value,
  "export CARDANO_VA_POLICY_ID=" + .validator_announce.hash,
  "export CARDANO_MAILBOX_REF_UTXO=" + (.mailbox.referenceScriptUtxo | "\(.txHash)#\(.outputIndex)"),
  "export CARDANO_ISM_REF_UTXO=" + (.ism.referenceScriptUtxo | "\(.txHash)#\(.outputIndex)")
' deployment_info.json)

# Also export the script CBOR
export CARDANO_PROCESSED_MSG_NFT_SCRIPT_CBOR=$(jq -r '.cborHex' processed_message_nft_applied.plutus)
```

---

### CARDANO_INDEX_FROM Configuration

**Critical**: The `CARDANO_INDEX_FROM` setting must be a **block height**, not a slot number.

```bash
# WRONG - This is a slot number (will cause indexing to skip messages)
CARDANO_INDEX_FROM=101311900

# CORRECT - This is a block height
CARDANO_INDEX_FROM=3936000
```

To find the correct block height for your mailbox initialization transaction:

```bash
# Get the block height (not slot) for the mailbox init transaction
INIT_TX=$(jq -r '.mailbox.initTxHash' deployment_info.json)
curl -s -H "project_id: $BLOCKFROST_API_KEY" \
  "https://cardano-preview.blockfrost.io/api/v0/txs/$INIT_TX" \
  | jq '.block_height'
```

**Symptoms of incorrect CARDANO_INDEX_FROM**:

- Validator logs show: "Current indexing snapshot's block height is less than or equal to the lowest block height, not indexing anything below"
- Validator doesn't sign checkpoints for existing messages
- Relayer shows "Operation not ready" indefinitely

---

### Processed Messages NFT Script CBOR

**Critical**: The `processedMessagesNftScriptCbor` field is **required** for the Fuji → Cardano direction. Without it, the relayer cannot mint the processed message NFT, and message processing will fail with a Plutus validation error.

To get this value after deployment:

```bash
# The CBOR is in the applied script file
cat deployments/$NETWORK/processed_message_nft_applied.plutus | jq -r '.cborHex'
```

Or from deployment_info.json if your CLI version exports it:

```bash
cat deployments/$NETWORK/deployment_info.json | jq -r '.processed_message_nft.cbor'
```

---

### Validator Announcement S3 URL Format

The validator announces its storage location on-chain, and this URL must exactly match what the validator generates internally.

**S3 URL format**: `s3://<bucket>/<region>/<folder>`

Example:

```
s3://hyperlane-validator-signatures-cardanopreview/eu-north-1/cardano-preview
```

The validator config must include the folder:

```json
{
  "checkpointSyncer": {
    "type": "s3",
    "bucket": "hyperlane-validator-signatures-cardanopreview",
    "region": "eu-north-1",
    "folder": "cardano-preview"
  }
}
```

To announce with the correct format:

```bash
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  validator announce \
  --storage-location "s3://your-bucket/your-region/your-folder"
```

**Symptoms of mismatched announcement**:

- Validator logs show: "Validator has not announced signature storage location"
- Validator keeps trying to re-announce but "Cannot announce validator without a signer"
- Relayer shows "Unable to reach quorum" even though checkpoints exist in S3

### Example Complete Relayer Config for Cardano

```json
{
  "chains": {
    "cardanopreview": {
      "name": "cardanopreview",
      "domainId": 2003,
      "protocol": "cardano",
      "chainId": 2003,
      "connection": {
        "type": "blockfrost",
        "url": "https://cardano-preview.blockfrost.io/api/v0",
        "apiKey": "${BLOCKFROST_API_KEY}",
        "network": "preview",
        "mailboxPolicyId": "<mailbox_state_nft_policy_id>",
        "mailboxScriptHash": "<mailbox_script_hash>",
        "mailboxReferenceScriptUtxo": "<tx_hash>#0",
        "processedMessagesNftPolicyId": "<processed_msg_nft_policy_id>",
        "processedMessagesNftScriptCbor": "<cbor_hex_from_applied_script>",
        "processedMessagesScriptHash": "<mailbox_script_hash>",
        "ismPolicyId": "<ism_state_nft_policy_id>",
        "ismScriptHash": "<ism_script_hash>",
        "ismReferenceScriptUtxo": "<tx_hash>#0",
        "registryPolicyId": "<registry_state_nft_policy_id>",
        "validatorAnnouncePolicyId": "<va_state_nft_policy_id>"
      },
      "index": {
        "from": 3936000
      },
      "mailbox": "0x00000000<mailbox_state_nft_policy_id>",
      "validatorAnnounce": "0x00000000<va_state_nft_policy_id>",
      "merkleTreeHook": "0x00000000<mailbox_state_nft_policy_id>",
      "interchainSecurityModule": "0x00000000<ism_script_hash>"
    }
  }
}
```

---

### Troubleshooting Parameterization Issues

**Error: "Parameter type mismatch"**

Ensure the parameter value matches the expected type. Policy IDs and script hashes are 28-byte hex strings:

```bash
# Correct: 56 hex characters (28 bytes)
aiken blueprint apply -v validator ... "6421905a7b782eda294774816c944d1707d0091c3fb84bc71cbf46e7"

# Wrong: 64 hex characters (32 bytes) - this is a Hyperlane address, not a policy ID
aiken blueprint apply -v validator ... "020000006421905a7b782eda294774816c944d1707d0091c3fb84bc71cbf46e7"
```

**Error: "Script hash mismatch after upgrade"**

If you upgrade a contract and the script hash changes, that's expected. However, ensure:

1. Recipients use `mailbox_policy_id` (stable), not `mailbox_script_hash` (changes)
2. Update the relayer config with the new script hash and reference script UTXO
3. The mailbox state UTXO is migrated to the new script address (if address changed)

**Error: "Processed message NFT not found"**

After upgrading, ensure `processed_message_nft` still uses the same `mailbox_policy_id`. If it was accidentally parameterized with a different value:

1. Previous processed message NFTs are under a different policy
2. Replay attacks become possible
3. Redeploy with correct `mailbox_policy_id` and migrate state

---

## Appendix: Warp Route Architecture

### Overview

Warp routes are Hyperlane token bridge contracts that enable cross-chain token transfers. On Cardano, warp routes use a UTXO-based design where each route has:

- **State UTXO**: Contains the route's configuration (routers, token info, decimals)
- **State NFT**: Unique identifier for the warp route instance
- **Reference Script UTXO**: Contains the validator script for transaction efficiency

### Token Types Explained

#### Native Warp Route

Bridges native ADA to other chains:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                        NATIVE WARP ROUTE FLOW                                │
└─────────────────────────────────────────────────────────────────────────────┘

  CARDANO → REMOTE (Outbound)              REMOTE → CARDANO (Inbound)
  ─────────────────────────                ─────────────────────────

  User sends ADA to                        Relayer calls process:
  warp route:

  ┌─────────────────┐                      ┌─────────────────┐
  │   User Wallet   │                      │   Warp Route    │
  │   (-10 ADA)     │                      │  UTXO (locked)  │
  └────────┬────────┘                      │   30 ADA        │
           │                               └────────┬────────┘
           │ transfer(10 ADA)                       │
           ▼                                        │ release(10 ADA)
  ┌─────────────────┐                               ▼
  │   Warp Route    │                      ┌─────────────────┐
  │  UTXO (locked)  │                      │ Warp Route UTXO │
  │   +10 ADA       │                      │   20 ADA        │
  └────────┬────────┘                      └────────┬────────┘
           │                                        │
           │ Mailbox dispatch                       │
           ▼                                        ▼
  ┌─────────────────┐                      ┌─────────────────┐
  │  Message to     │                      │   Recipient     │
  │  destination    │                      │   (+10 ADA)     │
  └─────────────────┘                      └─────────────────┘
```

**State Datum for Native:**

```
WarpRouteState {
  token_type: Native,           // Constructor tag: 123
  decimals: 6,
  remote_decimals: 18,
  routers: [(43113, 0x000...Fuji_Router)],
  owner: owner_credential
}
```

#### Collateral Warp Route

Bridges existing Cardano tokens by locking them in the warp route's state UTXO:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                      COLLATERAL WARP ROUTE FLOW                              │
└─────────────────────────────────────────────────────────────────────────────┘

  CARDANO → REMOTE (Outbound)              REMOTE → CARDANO (Inbound)
  ─────────────────────────                ─────────────────────────

  User locks tokens in                     Relayer releases tokens:
  warp route:

  ┌─────────────────┐                      ┌─────────────────┐
  │   User Wallet   │                      │   Warp Route    │
  │  (-100 TOKENS)  │                      │   State UTXO    │
  └────────┬────────┘                      │  (500 TOKENS)   │
           │                               └────────┬────────┘
           │ transfer(100 TOKENS)                   │
           ▼                                        │ release(100 TOKENS)
  ┌─────────────────┐                               ▼
  │   Warp Route    │                      ┌─────────────────┐
  │   State UTXO    │                      │   Warp Route    │
  │  (+100 TOKENS)  │                      │   State UTXO    │
  └────────┬────────┘                      │  (400 TOKENS)   │
           │                               └────────┬────────┘
           │ Mailbox dispatch                       │
           ▼                                        ▼
  ┌─────────────────┐                      ┌─────────────────┐
  │  Message to     │                      │   Recipient     │
  │  destination    │                      │ (+100 TOKENS)   │
  └─────────────────┘                      └─────────────────┘
```

**State Datum for Collateral:**

```
WarpRouteState {
  token_type: Collateral {       // Constructor tag: 121
    policy_id: "908d5175...",
    asset_name: "WARPTEST"
  },
  decimals: 6,
  remote_decimals: 18,
  routers: [(43113, 0x000...Fuji_Router)],
  owner: owner_credential
}
```

> **Note**: Unlike EVM warp routes that use separate vault contracts, Cardano collateral routes hold locked tokens directly in the state UTXO. This is more efficient on Cardano's UTXO model.

#### Synthetic Warp Route

Mints/burns synthetic tokens representing assets from other chains:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                       SYNTHETIC WARP ROUTE FLOW                              │
└─────────────────────────────────────────────────────────────────────────────┘

  CARDANO → REMOTE (Outbound)              REMOTE → CARDANO (Inbound)
  ─────────────────────────                ─────────────────────────

  User burns synthetic tokens:             Relayer mints synthetic tokens:

  ┌─────────────────┐                      ┌─────────────────┐
  │   User Wallet   │                      │    Minting      │
  │  (100 wFTEST)   │                      │    Policy       │
  └────────┬────────┘                      └────────┬────────┘
           │                                        │
           │ burn(100 wFTEST)                       │ mint(100 wFTEST)
           ▼                                        ▼
  ┌─────────────────┐                      ┌─────────────────┐
  │   BURN 100      │                      │   MINT 100      │
  │   wFTEST        │                      │   wFTEST        │
  │   (supply -= )  │                      │   (supply += )  │
  └────────┬────────┘                      └────────┬────────┘
           │                                        │
           │ Mailbox dispatch                       │
           ▼                                        ▼
  ┌─────────────────┐                      ┌─────────────────┐
  │  Message to     │                      │   Recipient     │
  │  destination    │                      │ (+100 wFTEST)   │
  └─────────────────┘                      └─────────────────┘
```

**State Datum for Synthetic:**

```
WarpRouteState {
  token_type: Synthetic {        // Constructor tag: 122
    minting_policy: "91d29736..."
  },
  decimals: 18,
  remote_decimals: 18,
  routers: [(43113, 0x000...FTEST_Collateral)],
  owner: owner_credential
}
```

**Synthetic Minting Policy:**

- Parameterized with warp route NFT policy
- Only warp route can authorize minting/burning
- Asset name derived from message content (domain + sender)

### UTXO Structure

Each warp route creates UTXOs based on token type:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                      WARP ROUTE UTXO STRUCTURE                               │
└─────────────────────────────────────────────────────────────────────────────┘

  State UTXO (at warp route address) - ALL types
  ┌──────────────────────────────────────────────────────┐
  │ Location: addr_test1wz...                            │
  │ Value: 2,000,000+ lovelace + locked tokens*          │
  │ NFT: {nft_policy}."" (empty asset name)              │
  │ Datum: WarpRouteState { ... }                        │
  │ Script: None (spent via reference)                   │
  │                                                      │
  │ * Native: holds locked ADA                           │
  │ * Collateral: holds locked tokens                    │
  │ * Synthetic: only MIN_UTXO lovelace                  │
  └──────────────────────────────────────────────────────┘

  Reference Script UTXO (at deployer address) - ALL types
  ┌──────────────────────────────────────────────────────┐
  │ Location: addr_test1qz... (deployer)                 │
  │ Value: ~15,000,000 lovelace                          │
  │ NFT: {nft_policy}.726566 ("ref")                     │
  │ Script: warp_route validator                         │
  └──────────────────────────────────────────────────────┘

  Minting Ref UTXO (for Synthetic routes only)
  ┌──────────────────────────────────────────────────────┐
  │ Location: addr_test1qz... (deployer)                 │
  │ Value: ~10,000,000 lovelace                          │
  │ NFT: {nft_policy}.6d696e745f726566 ("mint_ref")      │
  │ Script: synthetic_minting_policy                     │
  └──────────────────────────────────────────────────────┘
```

### Hyperlane Address Format

Cardano warp routes use a special H256 address format for Hyperlane:

```
Format: 0x02000000 + script_hash (28 bytes)

Example:
  Script Hash: a09ef754bfd03a4b8c48576718c30bbdc140ed45ff467cbc05924920
  H256 Address: 0x02000000a09ef754bfd03a4b8c48576718c30bbdc140ed45ff467cbc05924920
                ^^^^^^^^ Protocol prefix (Cardano = 0x02)
                        ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
                        28-byte script hash (zero-padded on left to 32 bytes)
```

This address is used:

- When enrolling the Cardano route on remote chains
- As the sender address in outbound messages
- As the recipient address for inbound messages

### Decimal Conversion

Cardano and EVM chains use different decimal places:

| Asset  | Cardano Decimals | EVM Decimals | Conversion Factor |
| ------ | ---------------- | ------------ | ----------------- |
| ADA    | 6                | 18           | 10^12             |
| HOSKY  | 0                | 18           | 10^18             |
| Custom | Varies           | 18           | 10^(18-local)     |

**Wire Amount Calculation:**

```
wire_amount = local_amount * 10^(remote_decimals - local_decimals)

Example: Sending 10 ADA to Fuji
  local_amount = 10,000,000 lovelace (10 ADA)
  local_decimals = 6
  remote_decimals = 18
  wire_amount = 10,000,000 * 10^(18-6) = 10,000,000,000,000,000,000
              = 10.0 with 18 decimals
```

### Registry Integration

The registry stores metadata for warp route discovery:

```
RegistryEntry {
  script_hash: "a09ef754...",         // Warp route validator hash
  recipient_type: TokenReceiver,       // Indicates token bridge
  state_locator: {                     // How to find state UTXO
    policy_id: "7c90fa68...",
    asset_name: ""
  },
  reference_script_locator: Some({     // How to find ref script
    policy_id: "7c90fa68...",
    asset_name: "726566"               // "ref" in hex
  }),
  additional_inputs: [],               // No additional inputs needed
  ism: None                            // Uses default ISM
}
```

All warp route types (Native, Collateral, Synthetic) use the same registry entry format. Locked assets are held directly in the state UTXO, so no additional inputs are required.

### E2E Testing Scenarios

The following scenarios test all warp route types bidirectionally:

| Scenario | Direction      | Type       | Action                                   |
| -------- | -------------- | ---------- | ---------------------------------------- |
| 1        | Cardano → Fuji | Collateral | Lock WARPTEST, mint wCTEST on Fuji       |
| 2        | Fuji → Cardano | Synthetic  | Lock FTEST, mint wFTEST on Cardano       |
| 3        | Cardano → Fuji | Native     | Lock ADA, mint wADA on Fuji              |
| 4        | Fuji → Cardano | Synthetic  | Lock AVAX, mint wAVAX on Cardano         |
| 5        | Fuji → Cardano | Native     | Burn wADA, release ADA on Cardano        |
| 6        | Fuji → Cardano | Collateral | Burn wCTEST, release WARPTEST on Cardano |

> **Note**: For detailed step-by-step E2E testing instructions with Avalanche Fuji, see [Appendix: Fuji (Avalanche Testnet) Deployment Guide](#appendix-fuji-avalanche-testnet-deployment-guide).

---

## Appendix: Fuji (Avalanche Testnet) Deployment Guide

This appendix provides step-by-step instructions for deploying Hyperlane warp route infrastructure on Avalanche Fuji testnet for E2E testing with Cardano.

### Prerequisites

#### 1. Install Foundry

```bash
curl -L https://foundry.paradigm.xyz | bash
foundryup
```

#### 2. Get Fuji Test AVAX

- Use the [Avalanche Fuji Faucet](https://faucet.avax.network/) to get test AVAX
- You'll need at least 1 AVAX for deployments

#### 3. Set Up Base Environment Variables

Create a `.env` file or export these variables. These are required for all subsequent steps:

```bash
# ============================================================
# BASE CONFIGURATION (Required for all steps)
# ============================================================

# Fuji RPC endpoint
export FUJI_RPC_URL="https://api.avax-test.network/ext/bc/C/rpc"

# Your Fuji private key (with 0x prefix) - must have AVAX for gas
export FUJI_SIGNER_KEY="0x..."

# Fuji Hyperlane infrastructure (already deployed on Fuji testnet)
export FUJI_MAILBOX="0x5b6CFf85442B851A8e6eaBd2A4E4507B5135B3B0"

# Domain IDs
export CARDANO_DOMAIN=2003  # Cardano Preview testnet
export FUJI_DOMAIN=43113    # Avalanche Fuji testnet
```

### Deployment Flow Overview

The deployment follows this order, with each step producing outputs needed by subsequent steps:

```
Step 1: Deploy ISM ──────────────────► FUJI_CARDANO_ISM
                                              │
Step 2: Deploy Warp Routes ──────────► FUJI_SYNTHETIC_*, FUJI_COLLATERAL_*, FUJI_*
                                              │
Step 3: Set ISM on Routes ◄──────────────────┘
                                              │
Step 4: Mint Test Tokens ◄───────────────────┘
                                              │
Step 5: Pre-deposit Collateral ◄─────────────┘
                                              │
Step 6: Enroll Cardano Routers ◄─────── CARDANO_NATIVE_ADA, CARDANO_COLLATERAL_*, etc.
```

---

### Step 1: Deploy Cardano MultisigISM on Fuji

The ISM (Interchain Security Module) validates messages coming from Cardano. It needs the Cardano validator's EVM address.

#### Required Environment Variables

| Variable            | Description                       | Example                                      |
| ------------------- | --------------------------------- | -------------------------------------------- |
| `FUJI_SIGNER_KEY`   | Private key for Fuji transactions | `0x...`                                      |
| `CARDANO_VALIDATOR` | Cardano validator's EVM address   | `0x0A923108968Cf8427693679eeE7b98340Fe038ce` |

#### Optional Environment Variables

| Variable                | Description                   | Default |
| ----------------------- | ----------------------------- | ------- |
| `CARDANO_ISM_THRESHOLD` | Number of validators required | `1`     |

#### 1.1 Get Cardano Validator Address

The validator address is derived from the validator's ECDSA private key (the same key used by the Cardano validator agent for checkpoint signing):

```bash
# If you have the validator key from cardano/e2e-docker/.env
CARDANO_VALIDATOR_KEY="0x2e0afff1080232cd5fc8fe769dd72f5766e4e0b66e5528fa93f80e75aca9e764"

# Derive the EVM address
export CARDANO_VALIDATOR=$(cast wallet address --private-key $CARDANO_VALIDATOR_KEY)
echo "Cardano Validator Address: $CARDANO_VALIDATOR"
# Output: 0x0A923108968Cf8427693679eeE7b98340Fe038ce
```

#### 1.2 Deploy the ISM

```bash
cd solidity

# Ensure CARDANO_VALIDATOR is set
echo "Deploying ISM with validator: $CARDANO_VALIDATOR"

# Deploy
forge script script/warp-e2e/DeployCardanoISM.s.sol:DeployCardanoISM \
  --rpc-url $FUJI_RPC_URL \
  --broadcast \
  --private-key $FUJI_SIGNER_KEY

# ⚠️ IMPORTANT: Save the ISM address from the output
export FUJI_CARDANO_ISM="0x..."  # Copy from "MultisigISM deployed:" line
```

---

### Step 2: Deploy Fuji Warp Routes

This deploys all the test ERC20 tokens and warp routes needed for E2E testing.

#### Required Environment Variables

| Variable          | Description                       | Example |
| ----------------- | --------------------------------- | ------- |
| `FUJI_SIGNER_KEY` | Private key for Fuji transactions | `0x...` |

#### Optional Environment Variables for Token Customization

You can customize token names, symbols, and decimals via environment variables:

**Test ERC20 Tokens:**

| Variable          | Description              | Default           |
| ----------------- | ------------------------ | ----------------- |
| `FTEST_NAME`      | Name for FTEST token     | `Fuji Test Token` |
| `FTEST_SYMBOL`    | Symbol for FTEST token   | `FTEST`           |
| `FTEST_DECIMALS`  | Decimals for FTEST token | `18`              |
| `WADA_NAME`       | Name for WADA token      | `Wrapped ADA`     |
| `WADA_SYMBOL`     | Symbol for WADA token    | `WADA`            |
| `WADA_DECIMALS`   | Decimals for WADA token  | `18`              |
| `TOKENA_NAME`     | Name for TokenA          | `Token A`         |
| `TOKENA_SYMBOL`   | Symbol for TokenA        | `TOKA`            |
| `TOKENA_DECIMALS` | Decimals for TokenA      | `18`              |

**Synthetic Warp Routes:**

| Variable                  | Description                   | Default         |
| ------------------------- | ----------------------------- | --------------- |
| `WCTEST_NAME`             | Name for wCTEST synthetic     | `Wrapped CTEST` |
| `WCTEST_SYMBOL`           | Symbol for wCTEST synthetic   | `wCTEST`        |
| `WCTEST_DECIMALS`         | Decimals for wCTEST synthetic | `6`             |
| `SYNTHETIC_WADA_NAME`     | Name for wADA synthetic       | `Wrapped ADA`   |
| `SYNTHETIC_WADA_SYMBOL`   | Symbol for wADA synthetic     | `wADA`          |
| `SYNTHETIC_WADA_DECIMALS` | Decimals for wADA synthetic   | `6`             |

#### What Gets Deployed

The script deploys these contracts (with default configurations shown):

| Contract Type        | Name            | Symbol | Decimals | Purpose                           |
| -------------------- | --------------- | ------ | -------- | --------------------------------- |
| TestERC20            | Fuji Test Token | FTEST  | 18       | Test token for Fuji → Cardano     |
| TestERC20            | Wrapped ADA     | WADA   | 18       | Wrapped ADA for collateral route  |
| TestERC20            | Token A         | TOKA   | 18       | For collateral-collateral tests   |
| HypERC20 (Synthetic) | Wrapped CTEST   | wCTEST | 6        | Receives from Cardano collateral  |
| HypERC20 (Synthetic) | Wrapped ADA     | wADA   | 6        | Receives from Cardano native      |
| HypERC20Collateral   | -               | -      | -        | Locks FTEST for Cardano synthetic |
| HypERC20Collateral   | -               | -      | -        | Releases WADA for Cardano native  |
| HypNative            | -               | -      | -        | Locks native AVAX                 |

#### 2.1 Deploy Warp Routes (Default Configuration)

```bash
cd solidity

forge script script/warp-e2e/DeployFujiWarp.s.sol:DeployFujiWarp \
  --rpc-url $FUJI_RPC_URL \
  --broadcast \
  --private-key $FUJI_SIGNER_KEY
```

#### 2.1b Deploy Warp Routes (Custom Token Names)

To deploy with custom token names, set the environment variables before running:

```bash
cd solidity

# Example: Custom token names for a specific test scenario
export FTEST_NAME="My Test Token"
export FTEST_SYMBOL="MTT"
export WCTEST_NAME="Wrapped My Test Token"
export WCTEST_SYMBOL="wMTT"

forge script script/warp-e2e/DeployFujiWarp.s.sol:DeployFujiWarp \
  --rpc-url $FUJI_RPC_URL \
  --broadcast \
  --private-key $FUJI_SIGNER_KEY
```

#### 2.2 Save Output Addresses

The script outputs environment variables at the end. **Copy and export all of them**:

```bash
# ⚠️ IMPORTANT: Export ALL addresses from the deployment output

# Test ERC20 Tokens
export FUJI_FTEST="0x..."           # Fuji Test Token
export FUJI_WADA="0x..."            # Wrapped ADA ERC20
export FUJI_TOKENA="0x..."          # Token A

# Synthetic Warp Routes (mint tokens when receiving from Cardano)
export FUJI_SYNTHETIC_WCTEST="0x..."   # Receives CTEST from Cardano, mints wCTEST
export FUJI_SYNTHETIC_WADA="0x..."     # Receives ADA from Cardano, mints wADA

# Collateral Warp Routes (lock/release tokens)
export FUJI_COLLATERAL_FTEST="0x..."   # Locks FTEST, Cardano receives synthetic wFTEST
export FUJI_COLLATERAL_WADA="0x..."    # Releases WADA when Cardano sends ADA
export FUJI_COLLATERAL_TOKENA="0x..."  # For collateral-collateral tests

# Native Warp Route
export FUJI_NATIVE_AVAX="0x..."        # Locks native AVAX
```

---

### Step 3: Set Cardano ISM on Warp Routes

Configure the warp routes to use the Cardano ISM for validating inbound messages from Cardano.

#### Required Environment Variables

| Variable                | Description                       | Set In        |
| ----------------------- | --------------------------------- | ------------- |
| `FUJI_SIGNER_KEY`       | Private key for Fuji transactions | Prerequisites |
| `FUJI_CARDANO_ISM`      | Cardano MultisigISM address       | Step 1        |
| `FUJI_SYNTHETIC_WCTEST` | wCTEST synthetic route            | Step 2        |
| `FUJI_SYNTHETIC_WADA`   | wADA synthetic route              | Step 2        |
| `FUJI_COLLATERAL_FTEST` | FTEST collateral route            | Step 2        |
| `FUJI_COLLATERAL_WADA`  | WADA collateral route             | Step 2        |

#### 3.1 Verify Variables Are Set

```bash
# Check all required variables are set
echo "ISM: $FUJI_CARDANO_ISM"
echo "Synthetic wCTEST: $FUJI_SYNTHETIC_WCTEST"
echo "Synthetic wADA: $FUJI_SYNTHETIC_WADA"
echo "Collateral FTEST: $FUJI_COLLATERAL_FTEST"
echo "Collateral WADA: $FUJI_COLLATERAL_WADA"
```

#### 3.2 Set ISM on All Routes

```bash
cd solidity

forge script script/warp-e2e/DeployCardanoISM.s.sol:DeployCardanoISM \
  --sig "setISMOnWarpRoutes()" \
  --rpc-url $FUJI_RPC_URL \
  --broadcast \
  --private-key $FUJI_SIGNER_KEY
```

#### 3.3 Alternative: Set ISM on Single Route Using Cast

```bash
# Set ISM on a specific warp route manually
cast send $FUJI_SYNTHETIC_WCTEST \
  "setInterchainSecurityModule(address)" \
  $FUJI_CARDANO_ISM \
  --rpc-url $FUJI_RPC_URL \
  --private-key $FUJI_SIGNER_KEY
```

---

### Step 4: Mint Test Tokens

Mint test tokens to your wallet for testing transfers from Fuji to Cardano.

#### Required Environment Variables

| Variable          | Description                       | Set In        |
| ----------------- | --------------------------------- | ------------- |
| `FUJI_SIGNER_KEY` | Private key for Fuji transactions | Prerequisites |
| `FUJI_FTEST`      | FTEST token address               | Step 2        |
| `FUJI_WADA`       | WADA token address                | Step 2        |
| `FUJI_TOKENA`     | TokenA address                    | Step 2        |

#### 4.1 Mint Using Script

```bash
cd solidity

forge script script/warp-e2e/DeployFujiWarp.s.sol:DeployFujiWarp \
  --sig "mintTestTokens()" \
  --rpc-url $FUJI_RPC_URL \
  --broadcast \
  --private-key $FUJI_SIGNER_KEY
```

This mints 1,000,000 tokens (with 18 decimals) of each test token to your wallet.

#### 4.2 Alternative: Mint Using Cast

```bash
# Mint 1000 FTEST to your wallet
WALLET=$(cast wallet address --private-key $FUJI_SIGNER_KEY)

cast send $FUJI_FTEST \
  "mint(address,uint256)" \
  $WALLET \
  "1000000000000000000000" \
  --rpc-url $FUJI_RPC_URL \
  --private-key $FUJI_SIGNER_KEY
```

---

### Step 5: Pre-deposit Collateral (for Cardano → Fuji)

For collateral routes that **release** tokens (like WADA when receiving ADA from Cardano), you must pre-deposit tokens into the collateral contract.

#### Required Environment Variables

| Variable                 | Description                       | Set In        |
| ------------------------ | --------------------------------- | ------------- |
| `FUJI_SIGNER_KEY`        | Private key for Fuji transactions | Prerequisites |
| `FUJI_WADA`              | WADA token address                | Step 2        |
| `FUJI_TOKENA`            | TokenA address                    | Step 2        |
| `FUJI_COLLATERAL_WADA`   | WADA collateral route             | Step 2        |
| `FUJI_COLLATERAL_TOKENA` | TokenA collateral route           | Step 2        |

#### 5.1 Pre-deposit Using Script

```bash
cd solidity

forge script script/warp-e2e/DeployFujiWarp.s.sol:DeployFujiWarp \
  --sig "preDepositCollateral()" \
  --rpc-url $FUJI_RPC_URL \
  --broadcast \
  --private-key $FUJI_SIGNER_KEY
```

This deposits 100,000 tokens to each collateral contract.

#### 5.2 Alternative: Pre-deposit Using Cast

```bash
# Transfer WADA directly to collateral contract
cast send $FUJI_WADA \
  "transfer(address,uint256)" \
  $FUJI_COLLATERAL_WADA \
  "100000000000000000000000" \
  --rpc-url $FUJI_RPC_URL \
  --private-key $FUJI_SIGNER_KEY
```

---

### Step 6: Enroll Cardano Routers on Fuji

After deploying Cardano warp routes, enroll them as remote routers on the Fuji warp routes.

#### Required Environment Variables

| Variable                   | Description                       | Format                     |
| -------------------------- | --------------------------------- | -------------------------- |
| `FUJI_SIGNER_KEY`          | Private key for Fuji transactions | `0x...`                    |
| `FUJI_SYNTHETIC_WCTEST`    | Fuji wCTEST synthetic             | `0x...` (20 bytes)         |
| `FUJI_SYNTHETIC_WADA`      | Fuji wADA synthetic               | `0x...` (20 bytes)         |
| `FUJI_COLLATERAL_FTEST`    | Fuji FTEST collateral             | `0x...` (20 bytes)         |
| `FUJI_COLLATERAL_WADA`     | Fuji WADA collateral              | `0x...` (20 bytes)         |
| `CARDANO_NATIVE_ADA`       | Cardano Native ADA route          | `0x02000000...` (32 bytes) |
| `CARDANO_COLLATERAL_CTEST` | Cardano Collateral CTEST route    | `0x02000000...` (32 bytes) |
| `CARDANO_SYNTHETIC_FTEST`  | Cardano Synthetic FTEST route     | `0x02000000...` (32 bytes) |

#### Optional Environment Variables

| Variable         | Description       | Default          |
| ---------------- | ----------------- | ---------------- |
| `CARDANO_DOMAIN` | Cardano domain ID | `2003` (Preview) |

#### 6.1 Get Cardano Warp Route Addresses

From your Cardano deployment, get the script hashes and convert to H256 format:

```bash
# Cardano addresses use H256 format: 0x02000000 + 28-byte script hash
# The "02000000" prefix indicates a script address

# Example: From Cardano CLI output or deployment artifacts
# If warp show --warp-policy returns script hash: 0ea635a9db202792c36ceec3a6c9d4bea53a15eb481eb545b6976ddb

# Native ADA warp route
export CARDANO_NATIVE_ADA="0x020000000ea635a9db202792c36ceec3a6c9d4bea53a15eb481eb545b6976ddb"

# Collateral CTEST warp route (for bridging Cardano native tokens)
export CARDANO_COLLATERAL_CTEST="0x02000000b72f2aeeddc9d0203429ecdb0fb1d65129592a9da62757a6bee7e472"

# Synthetic wFTEST warp route (receives FTEST from Fuji)
export CARDANO_SYNTHETIC_FTEST="0x02000000503a80b8f25f64f5375f7b1cac6e862dd333ec3dace7dc9544e9040c"
```

> **Tip**: You can find script hashes in `cardano/deployments/preview/*.json` files or by running `hyperlane-cardano warp show --warp-policy <NFT_POLICY>`.

#### 6.2 Run Enrollment Script

```bash
cd solidity

# Verify Cardano addresses are set
echo "Cardano Native ADA: $CARDANO_NATIVE_ADA"
echo "Cardano Collateral CTEST: $CARDANO_COLLATERAL_CTEST"
echo "Cardano Synthetic FTEST: $CARDANO_SYNTHETIC_FTEST"

forge script script/warp-e2e/EnrollCardanoRouters.s.sol:EnrollCardanoRouters \
  --rpc-url $FUJI_RPC_URL \
  --broadcast \
  --private-key $FUJI_SIGNER_KEY
```

#### 6.3 Alternative: Enroll Single Router

To enroll a single Cardano router on a specific Fuji warp route:

```bash
# Set the specific route pair
export FUJI_WARP_ROUTE="$FUJI_SYNTHETIC_WADA"  # Fuji route to configure
export CARDANO_ROUTER="$CARDANO_NATIVE_ADA"    # Cardano route to enroll

forge script script/warp-e2e/EnrollCardanoRouters.s.sol:EnrollCardanoRouters \
  --sig "enrollSingle()" \
  --rpc-url $FUJI_RPC_URL \
  --broadcast \
  --private-key $FUJI_SIGNER_KEY
```

#### 6.4 Alternative: Enroll Using Cast

```bash
# Enroll Cardano native ADA on Fuji wADA synthetic
cast send $FUJI_SYNTHETIC_WADA \
  "enrollRemoteRouter(uint32,bytes32)" \
  $CARDANO_DOMAIN \
  $CARDANO_NATIVE_ADA \
  --rpc-url $FUJI_RPC_URL \
  --private-key $FUJI_SIGNER_KEY
```

---

### Step 7: Verify Deployments

#### Check ISM Configuration

```bash
# Check ISM on a warp route (should return FUJI_CARDANO_ISM address)
cast call $FUJI_SYNTHETIC_WCTEST \
  "interchainSecurityModule()(address)" \
  --rpc-url $FUJI_RPC_URL
```

#### Check Enrolled Routers

```bash
# Check if Cardano router is enrolled (should return non-zero bytes32)
cast call $FUJI_SYNTHETIC_WCTEST \
  "routers(uint32)(bytes32)" \
  $CARDANO_DOMAIN \
  --rpc-url $FUJI_RPC_URL

# Expected output: 0x02000000... (Cardano warp route address)
# If 0x0000...0000, enrollment failed or wasn't done
```

#### Check Token Balances

```bash
# Your wallet address
WALLET=$(cast wallet address --private-key $FUJI_SIGNER_KEY)

# Check FTEST balance in your wallet
cast call $FUJI_FTEST \
  "balanceOf(address)(uint256)" \
  $WALLET \
  --rpc-url $FUJI_RPC_URL

# Check WADA balance in collateral contract (for Cardano → Fuji releases)
cast call $FUJI_WADA \
  "balanceOf(address)(uint256)" \
  $FUJI_COLLATERAL_WADA \
  --rpc-url $FUJI_RPC_URL
```

---

### Step 8: Test Transfer (Fuji → Cardano)

> **Prerequisites**: Before testing transfers, ensure the Hyperlane validator and relayer agents are running and properly configured. See [Appendix: Agent Configuration Requirements](#appendix-agent-configuration-requirements) for setup instructions.

#### Required Environment Variables

| Variable                | Description                       |
| ----------------------- | --------------------------------- |
| `FUJI_SIGNER_KEY`       | Private key for Fuji transactions |
| `FUJI_FTEST`            | FTEST token address               |
| `FUJI_COLLATERAL_FTEST` | FTEST collateral warp route       |
| `CARDANO_DOMAIN`        | Cardano domain ID (2003)          |

#### 8.1 Approve Token Spending

```bash
# Approve FTEST collateral to spend your tokens
cast send $FUJI_FTEST \
  "approve(address,uint256)" \
  $FUJI_COLLATERAL_FTEST \
  "1000000000000000000000" \
  --rpc-url $FUJI_RPC_URL \
  --private-key $FUJI_SIGNER_KEY
```

#### 8.2 Prepare Cardano Recipient Address

```bash
# Cardano recipient in H256 format:
# - Pubkey addresses: 0x01000000 + 28-byte payment credential
# - Script addresses: 0x02000000 + 28-byte script hash

# Example: From your Cardano wallet/deployment
CARDANO_RECIPIENT="0x010000001212a023380020f8c7b94b831e457b9ee65f009df9d1d588430dcc89"
```

#### 8.3 Initiate Transfer

```bash
# Transfer 5 FTEST (18 decimals) to Cardano
cast send $FUJI_COLLATERAL_FTEST \
  "transferRemote(uint32,bytes32,uint256)" \
  $CARDANO_DOMAIN \
  $CARDANO_RECIPIENT \
  "5000000000000000000" \
  --rpc-url $FUJI_RPC_URL \
  --private-key $FUJI_SIGNER_KEY

# Save the transaction hash for tracking
```

#### 8.4 Monitor the Transfer

```bash
# Check Hyperlane Explorer or relayer logs for message delivery
# The relayer will pick up the message and deliver it to Cardano
```

---

### Complete Environment Variables Reference

Here's a template with all environment variables organized by when they're set:

```bash
#!/bin/bash
# Fuji E2E Deployment Environment Variables

# ============================================================
# PREREQUISITES (Set before starting)
# ============================================================
export FUJI_RPC_URL="https://api.avax-test.network/ext/bc/C/rpc"
export FUJI_SIGNER_KEY="0x..."  # Your Fuji private key

# Fuji Hyperlane Infrastructure (pre-deployed)
export FUJI_MAILBOX="0x5b6CFf85442B851A8e6eaBd2A4E4507B5135B3B0"

# Domain IDs
export CARDANO_DOMAIN=2003
export FUJI_DOMAIN=43113

# ============================================================
# STEP 1: ISM Deployment Inputs
# ============================================================
export CARDANO_VALIDATOR="0x..."  # From: cast wallet address --private-key $CARDANO_VALIDATOR_KEY

# Optional
export CARDANO_ISM_THRESHOLD=1

# ============================================================
# STEP 1: ISM Deployment Outputs (set after deployment)
# ============================================================
export FUJI_CARDANO_ISM="0x..."

# ============================================================
# STEP 2: Warp Route Deployment Outputs (set after deployment)
# ============================================================
# Test Tokens
export FUJI_FTEST="0x..."
export FUJI_WADA="0x..."
export FUJI_TOKENA="0x..."

# Synthetic Routes
export FUJI_SYNTHETIC_WCTEST="0x..."
export FUJI_SYNTHETIC_WADA="0x..."

# Collateral Routes
export FUJI_COLLATERAL_FTEST="0x..."
export FUJI_COLLATERAL_WADA="0x..."
export FUJI_COLLATERAL_TOKENA="0x..."

# Native Route
export FUJI_NATIVE_AVAX="0x..."

# ============================================================
# STEP 6: Cardano Router Enrollment Inputs
# (Get from Cardano deployment artifacts)
# ============================================================
export CARDANO_NATIVE_ADA="0x02000000..."
export CARDANO_COLLATERAL_CTEST="0x02000000..."
export CARDANO_SYNTHETIC_FTEST="0x02000000..."
```

---

### Warp Route Pairing Reference

| Scenario | Cardano Route    | Fuji Route       | Direction      | Token Flow               |
| -------- | ---------------- | ---------------- | -------------- | ------------------------ |
| 1        | Collateral CTEST | Synthetic wCTEST | Cardano → Fuji | Lock CTEST → Mint wCTEST |
| 2        | Synthetic wFTEST | Collateral FTEST | Fuji → Cardano | Lock FTEST → Mint wFTEST |
| 3        | Native ADA       | Synthetic wADA   | Cardano → Fuji | Lock ADA → Mint wADA     |
| 4        | Synthetic wAVAX  | Native AVAX      | Fuji → Cardano | Lock AVAX → Mint wAVAX   |
| 5        | Native ADA       | Collateral WADA  | Cardano → Fuji | Lock ADA → Release WADA  |

---

### Customizing Token Deployment

The `DeployFujiWarp.s.sol` script supports customization via environment variables.

#### Option 1: Environment Variables (Recommended)

Set environment variables before running the deployment script:

```bash
# Custom token names and symbols
export FTEST_NAME="My Test Token"
export FTEST_SYMBOL="MTT"
export FTEST_DECIMALS=18

export WCTEST_NAME="Wrapped My Test Token"
export WCTEST_SYMBOL="wMTT"
export WCTEST_DECIMALS=6

export SYNTHETIC_WADA_NAME="Synthetic ADA"
export SYNTHETIC_WADA_SYMBOL="sADA"

# Then deploy
forge script script/warp-e2e/DeployFujiWarp.s.sol:DeployFujiWarp \
  --rpc-url $FUJI_RPC_URL \
  --broadcast \
  --private-key $FUJI_SIGNER_KEY
```

See [Step 2](#step-2-deploy-fuji-warp-routes) for the full list of customizable environment variables.

#### Option 2: Deploy Individual Contracts Manually

For complete control, deploy contracts individually using `forge create`:

```bash
# Deploy custom TestERC20
forge create script/warp-e2e/TestERC20.sol:TestERC20 \
  --constructor-args "My Token" "MTK" 18 \
  --rpc-url $FUJI_RPC_URL \
  --private-key $FUJI_SIGNER_KEY

# Deploy HypERC20 synthetic
forge create contracts/token/HypERC20.sol:HypERC20 \
  --constructor-args 6 1000000000000 $FUJI_MAILBOX \
  --rpc-url $FUJI_RPC_URL \
  --private-key $FUJI_SIGNER_KEY

# Initialize the synthetic
WALLET=$(cast wallet address --private-key $FUJI_SIGNER_KEY)
cast send $DEPLOYED_ADDRESS \
  "initialize(uint256,string,string,address,address,address)" \
  0 "Wrapped Token" "wTKN" "0x0000000000000000000000000000000000000000" $FUJI_CARDANO_ISM $WALLET \
  --rpc-url $FUJI_RPC_URL \
  --private-key $FUJI_SIGNER_KEY
```

---

### Troubleshooting Fuji Deployments

#### "Environment variable not set" Error

```bash
# Check which variables are missing
env | grep -E "^FUJI_|^CARDANO_"

# Make sure to export (not just set) variables
export FUJI_CARDANO_ISM="0x..."  # ✓ Correct
FUJI_CARDANO_ISM="0x..."         # ✗ Won't be available to forge
```

#### "Execution reverted" on Transfer

1. **Check ISM is set correctly:**

   ```bash
   cast call $FUJI_SYNTHETIC_WCTEST "interchainSecurityModule()(address)" --rpc-url $FUJI_RPC_URL
   # Should return $FUJI_CARDANO_ISM
   ```

2. **Verify router enrollment:**

   ```bash
   cast call $FUJI_SYNTHETIC_WCTEST "routers(uint32)(bytes32)" $CARDANO_DOMAIN --rpc-url $FUJI_RPC_URL
   # Should return non-zero (Cardano address)
   ```

3. **Ensure token approval for collateral routes:**
   ```bash
   WALLET=$(cast wallet address --private-key $FUJI_SIGNER_KEY)
   cast call $FUJI_FTEST "allowance(address,address)(uint256)" $WALLET $FUJI_COLLATERAL_FTEST --rpc-url $FUJI_RPC_URL
   ```

#### "Router not enrolled"

```bash
# Check current enrolled router
cast call $FUJI_WARP_ROUTE "routers(uint32)(bytes32)" $CARDANO_DOMAIN --rpc-url $FUJI_RPC_URL

# If returns 0x000...000, enroll the router
cast send $FUJI_WARP_ROUTE \
  "enrollRemoteRouter(uint32,bytes32)" \
  $CARDANO_DOMAIN \
  $CARDANO_ROUTER \
  --rpc-url $FUJI_RPC_URL \
  --private-key $FUJI_SIGNER_KEY
```

#### "Insufficient balance" in Collateral

Pre-deposit more tokens to the collateral contract:

```bash
# First mint more tokens if needed
cast send $FUJI_WADA "mint(address,uint256)" $WALLET "1000000000000000000000000" \
  --rpc-url $FUJI_RPC_URL --private-key $FUJI_SIGNER_KEY

# Then transfer to collateral
cast send $FUJI_WADA "transfer(address,uint256)" $FUJI_COLLATERAL_WADA "500000000000000000000000" \
  --rpc-url $FUJI_RPC_URL --private-key $FUJI_SIGNER_KEY
```

#### Message Not Delivered to Cardano

1. Check the [Hyperlane Explorer](https://explorer.hyperlane.xyz/) for message status
2. Verify Cardano relayer is running and configured for Fuji (domain 43113) as origin
3. Check relayer logs: `docker logs -f hyperlane-relayer 2>&1 | grep -E "(message|error)"`
4. Verify Cardano ISM has the correct Fuji validators configured

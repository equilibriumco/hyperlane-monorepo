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
8. [Phase 6: Deploy Recipients (Optional)](#phase-6-deploy-recipients-optional)
9. [Phase 7: Deploy Warp Routes](#phase-7-deploy-warp-routes)
10. [Verification & Troubleshooting](#verification--troubleshooting)
11. [Complete Deployment Script](#complete-deployment-script)
12. [Appendix: Script Parameterization](#appendix-script-parameterization)
13. [Appendix: Agent Configuration Requirements](#appendix-agent-configuration-requirements)
14. [Appendix: Warp Route Architecture](#appendix-warp-route-architecture)
15. [Appendix: Sepolia (Ethereum Testnet) Deployment Guide](#appendix-sepolia-ethereum-testnet-deployment-guide)
16. [Appendix: Gas Payment (IGP) Configuration & Enforcement](#appendix-gas-payment-igp-configuration--enforcement)
17. [Appendix: EVM-Side Hook Configuration (AggregationHook)](#appendix-evm-side-hook-configuration-aggregationhook)

> The design rationale for the gas model lives in
> [`cardano/docs/design/igp-gas-model.md`](design/igp-gas-model.md).

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

### Transaction Confirmation

By default, the CLI waits for transaction confirmation before returning. This prevents errors from consumed UTXOs when chaining commands. Use `--no-wait` to skip confirmation waiting:

```bash
# Default: waits for confirmation (recommended for scripted deployments)
./cli/target/release/hyperlane-cardano --network $NETWORK init mailbox --domain 2003

# Skip waiting (faster for exploratory use)
./cli/target/release/hyperlane-cardano --no-wait --network $NETWORK init mailbox --domain 2003
```

---

## Contract Overview & Dependencies

### Core Contracts

| Contract                        | Purpose                       | Parameters                                                  | Dependencies                             |
| -------------------------------- | ------------------------------ | ----------------------------------------------------------- | ---------------------------------------- |
| **state_nft**                    | Unique NFT minting policy      | UTXO reference                                              | None                                     |
| **mailbox**                      | Message dispatch/process hub   | verified_message_nft_policy, ism_nft_policy                 | verified_message_nft                        |
| **message_id_multisig_ism**      | MessageId signature verification | None                                                       | None                                     |
| **merkle_root_multisig_ism**     | MerkleRoot signature verification | None                                                      | None                                     |
| **verified_message_nft**         | Verified message token         | mailbox_policy_id                                           | mailbox (state NFT)                      |

> **Note**: The mailbox validator is parameterized with `verified_message_nft_policy` (for verified message tokens) and `ism_nft_policy`. Replay protection uses a sparse Merkle tree (SMT) in the mailbox datum instead of a separate NFT policy. The `verified_message_nft` policy is parameterized by `mailbox_policy_id` (stable across upgrades) to ensure it persists even when the mailbox code is updated. Warp routes are identified by their state NFT policy ID (Hyperlane address = `0x01000000 || nft_policy_id`). Generic script recipients use `0x02000000 || script_hash`. See [Appendix: Script Parameterization](#appendix-script-parameterization) for details.
>
> **ISM naming**: there are two ISM validators — `message_id_multisig_ism` (verifies a MessageId multisig proof, no merkle tree needed) and `merkle_root_multisig_ism` (verifies a merkle inclusion proof against a signed root). A deployment always has one **default** ISM (chosen at `deploy extract --ism-module-type`) plus optionally one or more **alt** ISM instances of the other flavour, recorded in `deployment_info.json` under `ism` (default) and `alt_isms[]`. See [Deploying Both ISM Flavours (Dual-ISM)](#34-deploying-both-ism-flavours-dual-ism).

### Recipient Contracts

| Contract                      | Purpose                 | Parameters                                  | Dependencies                    |
| ----------------------------- | ----------------------- | ------------------------------------------- | ------------------------------- |
| **greeting**                  | Example message handler | verified_message_nft_policy, owner          | mailbox (verified_message_nft)  |
| **warp_route**                | Token bridge            | mailbox_policy_id                                | mailbox                    |

### Dependency Graph

```
                 ┌─────────────────────────────────────────────┐
                 │           STATE NFT MINTING POLICY           │
                 │   (Parameterized per contract instance)      │
                 └─────────────────────────────────────────────┘
                                      │
                    ┌─────────────────┼─────────────────┐
                    │                                   │
                    ▼                                   ▼
          ┌─────────────────┐                 ┌───────────────────────────┐
          │     MAILBOX     │                 │  MESSAGE_ID_MULTISIG_ISM  │
          │   (Validator)   │                 │  MERKLE_ROOT_MULTISIG_ISM │
          └────────┬────────┘                 │  (default + alt_isms[])  │
                   │                          └───────────────────────────┘
                   │
                   │ mailbox_policy_id
                   │
          ┌────────┴────────────────────┐
          │                            │
          ▼                            ▼
 ┌──────────────┐      ┌────────────────────────────┐
 │ VERIFIED_    │      │  WARP_ROUTE (mailbox only)  │
 │ MESSAGE_NFT  │      └────────────────────────────┘
 └──────┬───────┘
        │
        │ verified_message_nft_policy
        │
        ├────────────────┐
        │                │
        ▼                ▼
 ┌──────────────┐ ┌──────────────────────┐
 │   GREETING   │ │   CUSTOM RECIPIENT   │
 │              │ │   (Optional)         │
 └──────────────┘ └──────────────────────┘
```

### Deployment Order

The contracts must be deployed in this order due to dependencies:

1. **Extract all validators** from plutus.json (writes both `message_id_multisig_ism.*` and `merkle_root_multisig_ism.*`; `--ism-module-type` picks the default)
2. **Initialize Core Contracts** (mailbox, default ISM) - applies parameters, creates state NFTs, produces parameterized scripts
3. **Deploy Reference Scripts** - deploy the parameterized scripts (mailbox + default ISM + any `alt_isms[]`) as on-chain reference scripts
4. **Configure Mailbox** - set default ISM using its **script hash** (`ism.hash`, not the state NFT policy)
5. **Configure ISM** - set validators and thresholds for each origin domain
6. **Deploy Recipients/Warp Routes** - recipients parameterized with verified_message_nft_policy; warp routes with mailbox_policy_id
7. **(Optional) Initialize IGP** - `init all` does not do this; run `init igp` separately if gas payments are needed

> **Important**: Reference scripts can only be deployed AFTER initialization because the core contracts (mailbox, ISM) are parameterized. The initialization step applies the required parameters and produces the final script bytecode.

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
"message_id_multisig_ism.message_id_multisig_ism.spend"
"merkle_root_multisig_ism.merkle_root_multisig_ism.spend"
"state_nft.state_nft.mint"
"greeting.greeting.spend"
"verified_message_nft.verified_message_nft.mint"
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
  --output deployments/$NETWORK \
  --ism-module-type messageid
```

`--ism-module-type` selects which ISM flavour fills the mailbox's **default** `ism` slot in `deployment_info.json` (`messageid` or `merkleroot`; defaults to `messageid` if omitted). Regardless of which flavour is chosen as default, `deploy extract` always writes **both** ISM validators' files — `message_id_multisig_ism.*` and `merkle_root_multisig_ism.*` — so either flavour can be initialized later (see [Deploying Both ISM Flavours (Dual-ISM)](#34-deploying-both-ism-flavours-dual-ism)).

This creates:

- `deployments/$NETWORK/*.plutus` - Cardano CLI compatible script files
- `deployments/$NETWORK/*.hash` - Script hash files
- `deployments/$NETWORK/*.addr` - Bech32 script addresses
- `deployments/$NETWORK/deployment_info.json` - Deployment metadata (records the chosen flavour under `ism.moduleType`)

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
# message_id_multisig_ism.plutus, message_id_multisig_ism.hash, message_id_multisig_ism.addr
# merkle_root_multisig_ism.plutus, merkle_root_multisig_ism.hash, merkle_root_multisig_ism.addr
# state_nft.plutus (base, not parameterized)
# ...
```

---

## Phase 3: Initialize Core Contracts

Initialization applies parameters to the contracts, creates state NFTs, and sets up initial datums. This step is required before deploying reference scripts because the core contracts are parameterized.

### 3.1 Initialize All Core Contracts (Recommended)

`init all` initializes the mailbox and the **default** ISM (the flavour chosen at `deploy extract --ism-module-type`), and can also configure ISM validators/thresholds and announce the validator, all in one command. `--validators`, `--thresholds`, `--storage-location`, and `--validator-key` are technically optional at the CLI level, but skipping them leaves the ISM with **no validators and threshold 0** — the on-chain ISM rejects every checkpoint at threshold 0 (see [5.3](#53-set-ism-threshold)), so no message will ever verify until you configure them. In practice always pass the full set:

```bash
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  init all \
  --domain 2003 \
  --origin-domains 11155111 \
  --validators "11155111:b22b65f202558adf86a8bb2847b76ae1036686a5,469f0940684d147defc44f3647146cb90dd0bc8e,d3c75dcf15056012a4d74c483a0c6ea11d8c2b83" \
  --thresholds "11155111:1" \
  --storage-location "s3://my-bucket/my-region/my-folder" \
  --validator-key "0x2e0afff1080232cd5fc8fe769dd72f5766e4e0b66e5528fa93f80e75aca9e764"
```

Parameters:

- `--domain`: Local Cardano domain ID (2003 for preview, 2002 for preprod). This is the domain the ISM/validator-announce identify Cardano as, on the *remote* chains.
- `--origin-domains`: Comma-separated list of remote origin domain IDs this ISM will verify messages from (e.g. `11155111` for Sepolia).
- `--validators`: ISM validators per origin domain. Format: `"domain:addr1,addr2;domain2:addr3"` (20-byte / 40-hex-char EVM addresses, no `0x` prefix).
- `--thresholds`: ISM threshold per origin domain. Format: `"domain:threshold;domain2:threshold"`.
- `--storage-location`: S3 URL announcing where the **Cardano validator's** checkpoints live, for the outbound (Cardano → remote) direction. Format is `s3://<bucket>/<region>/<folder>` — the region is part of the path, not a separate field.
- `--validator-key`: ECDSA secp256k1 hex key used to sign the validator announcement (the same key the Cardano validator agent uses for checkpoint signing).

Each step waits for on-chain confirmation before proceeding to the next.

> **Note**: `init all` does **not** initialize the IGP (Interchain Gas Paymaster). Run `init igp` separately — see [3.5](#35-initialize-the-igp).

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

#### Initialize the Default ISM

```bash
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  init ism \
  --domains 11155111 \
  --validators "11155111:ab8cc5ae0dcce3d0dff1925a70cda0250f06ba21" \
  --thresholds "11155111:1"
```

Parameters:

- `--domains`: Origin domain IDs (comma-separated)
- `--validators`: Format: "domain:addr1,addr2;domain2:addr3"
- `--thresholds`: Format: "domain:threshold;domain2:threshold"
- `--module-type`: ISM flavour to (re-)initialize — `messageid` or `merkleroot`. Defaults to the deployment's default flavour (from `deploy extract --ism-module-type`); this command with no `--module-type` re-initializes that same default ISM. Passing the *other* flavour instead initializes a new, additional ISM instance and appends it to `deployment_info.json`'s `alt_isms[]` — see [3.4](#34-deploying-both-ism-flavours-dual-ism).

Output:

```
ISM initialized!
  State NFT Policy: 02993c46cdcf8eb56ada209e277acc288dc0263b6a502d17b8cbfa56
  UTXO: def456...#0
```

### 3.3 Verify Initialization

```bash
# Check status
./cli/target/release/hyperlane-cardano \
  --network $NETWORK \
  init status
```

### 3.4 Deploying Both ISM Flavours (Dual-ISM)

A deployment is not limited to a single ISM instance. It always has one **default** ISM (the flavour chosen at `deploy extract --ism-module-type`, initialized by `init all` / `init ism`) and can additionally have one or more **alt** ISM instances of the other flavour. This lets the mailbox's default stay MessageId (cheap, no tree backfill) while a specific recipient opts into MerkleRoot (or vice versa).

After `init all` has initialized the default flavour, stand up the other flavour as an alt ISM by passing `--module-type` with the flavour that differs from the default:

```bash
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  init ism \
  --domains 11155111 \
  --validators "11155111:ab8cc5ae0dcce3d0dff1925a70cda0250f06ba21" \
  --thresholds "11155111:1" \
  --module-type merkleroot
```

- `--module-type` defaults to the deployment's default flavour — passing that same value re-initializes `deployment_info.json`'s `ism` entry.
- Passing the **other** flavour deploys a brand-new ISM instance (its own state NFT, its own validator set) and appends it to `deployment_info.json`'s `alt_isms[]` array; the mailbox's default ISM is untouched.
- Recipients opt a specific ISM in via `init recipient --custom-ism <script_hash> --custom-ism-policy <state_nft_policy>`, pointing at either the default ISM or any entry in `alt_isms[]` (see [Selecting a per-recipient ISM override](#selecting-a-per-recipient-ism-override)).
- `deploy reference-scripts-all` (Phase 4) automatically deploys a reference script for the default ISM **and every entry in `alt_isms[]`**, so no extra step is needed to make an alt ISM referenceable.

### 3.5 Initialize the IGP

`init all` does **not** set up the Interchain Gas Paymaster — it only covers the mailbox, default ISM, and (optionally) the validator announcement. Run `init igp` separately if you need gas payments:

```bash
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  init igp \
  --oracle "11155111:1000000000:7171:211000"
```

- `--oracle`: repeatable gas oracle config per remote domain, format `"domain:gas_price:exchange_rate:gas_overhead"`. The `211000` overhead here targets a ~1.5× relayer margin for Sepolia delivery — see the **Gas Payment (IGP) Configuration & Enforcement** appendix for how the values are derived, the matching Sepolia-side oracle setup, and enabling relayer enforcement.
- `--beneficiary`: address that can claim collected fees (defaults to the signing key's public key hash).

> This only sets the **Cardano → Sepolia** oracle. The **Sepolia → Cardano**
> oracle, per-route `destinationGas`, and relayer `onChainFeeQuoting` enforcement
> are configured after warp routes exist — see the Gas Payment appendix.

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

This deploys a reference script (15 ADA minimum UTXO each) for:

- The mailbox validator
- The **default** ISM (whichever flavour is recorded in `deployment_info.json`'s `ism`)
- Every ISM flavour recorded under `deployment_info.json`'s `alt_isms[]` (see [Dual-ISM](#34-deploying-both-ism-flavours-dual-ism)) — so if you initialized both a MessageId and a MerkleRoot ISM, this single command deploys reference scripts for both.

### 4.2 Deploy Individual Reference Script (Alternative)

```bash
# Deploy a specific script by name (uses applied script automatically)
# Valid names: mailbox, message_id_multisig_ism, merkle_root_multisig_ism
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

> **Note**: If you used `init all` with `--validators` and `--thresholds` flags in Phase 3, the ISM validators and thresholds are already configured. You only need to perform Phase 5 if you initialized contracts individually or need to reconfigure.

### 5.1 Update Mailbox Default ISM

After ISM is initialized, update the mailbox to use the correct ISM:

```bash
# Get the ISM script hash from deployment info (default flavour — see deploy extract --ism-module-type)
ISM_HASH=$(cat deployments/$NETWORK/message_id_multisig_ism.hash)  # or merkle_root_multisig_ism.hash

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
# For Ethereum Sepolia testnet (domain 11155111)
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  ism set-validators \
  --domain 11155111 \
  --validators "b22b65f202558adf86a8bb2847b76ae1036686a5,469f0940684d147defc44f3647146cb90dd0bc8e,d3c75dcf15056012a4d74c483a0c6ea11d8c2b83"
```

### 5.3 Set ISM Threshold

```bash
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  ism set-threshold \
  --domain 11155111 \
  --threshold 2
```

> **Note**: The threshold must be at least `1`. `set-threshold` rejects `0`, and even if a `0` threshold were placed in the datum some other way, the ISM's `verify_checkpoint` rejects it on-chain — otherwise an empty signature set would satisfy the domain.

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

## Phase 6: Deploy Recipients (Optional)

Recipients are contracts that receive Hyperlane messages on Cardano. Generic recipients (like the greeting contract) are parameterized with `verified_message_nft_policy` and use the verified message pattern. This phase is only needed if you want to receive generic (non-warp-route) messages on Cardano.

### 6.1 Deploy the Greeting Contract (Example)

The greeting contract is a simple recipient that stores "Hello, {name}" messages on-chain. It's useful for testing the full message flow end-to-end.

```bash
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  init recipient \
  --custom-contracts ./contracts \
  --custom-module greeting \
  --custom-validator greeting
```

This applies two parameters — `verified_message_nft_policy` and `owner` (the owner defaults to the signing key's public key hash; override with `--owner <pkh>`) — to the `greeting` validator, then deploys the **three-UTXO canonical-config pattern** over **two transactions**:

- **TX1** funds an ADA-only "init-signal" UTXO at the recipient's script address.
- **TX2** spends that UTXO with the `Init` redeemer (owner-signed), mints the canonical config NFT and the state NFT, and creates the three outputs.

The three outputs are:

- **#0 Config UTXO** — script address, canonical config NFT + ISM config datum (holds the per-recipient ISM override, or `None` for the default ISM)
- **#1 State UTXO** — script address, state NFT + initial state datum
- **#2 Reference Script UTXO** — deployer address, "ref" NFT + validator script

Output:

```
Recipient Deployment Summary (Canonical Config NFT Pattern)
  Script Hash: e4edab59ad48a709b58318c714142f6ceb5a3c87bd2f983054e64bec
  State NFT Policy: cda0f0a48a73a90c06ac73f21f29f94a1377d5dbcbc346bab2ce93df
  #0 config UTXO  — canonical NFT + ISM config datum  @ script address
  #1 state UTXO   — state NFT + initial datum          @ script address
  #2 ref script   — ref NFT + recipient script CBOR    @ deployer address
```

The greeting contract's datum tracks the last greeting and a counter:

```aiken
pub type GreetingDatum {
  last_greeting: ByteArray,
  greeting_count: Int,
}
```

After deployment, sending a message with body `"Alice"` from another chain will update the datum to `last_greeting: "Hello, Alice"` and increment `greeting_count`.

### 6.2 Deploy a Custom Recipient

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
- The validator must accept two parameters, in order: `verified_message_nft_policy: PolicyId` and `owner: VerificationKeyHash`. The CLI applies both with `aiken blueprint apply` (the owner defaults to the signing key; override with `--owner <pkh>`).
- Constructor 0 (`Init`) **must require the recipient owner's signature**. The canonical config NFT policy relies on this invariant; without it, an attacker can install an ISM override for the recipient.

There are two parameterization patterns for recipients:

**Pattern 1: Verified message pattern** (recommended for generic recipients like greeting):

The contract is parameterized by `verified_message_nft_policy` and `owner`. The mailbox creates a verified message NFT when processing inbound messages, and the recipient verifies its presence; the owner gates `Init` (and typically message handling).

```aiken
validator my_recipient(verified_message_nft_policy: PolicyId, owner: VerificationKeyHash) {
  spend(datum, redeemer, own_ref, tx) {
    // Verify that a verified message NFT is present in the transaction
    expect has_verified_message_nft(tx, verified_message_nft_policy)
    // Your custom logic here
    True
  }
}
```

#### Selecting a per-recipient ISM override

By default a recipient is verified by the mailbox's default ISM. To route a recipient through its own ISM, pass **both** the ISM script hash and its state-NFT policy at init:

```bash
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  init recipient \
  --custom-contracts ./path/to/your/contracts \
  --custom-module my_recipient \
  --custom-validator my_recipient \
  --custom-ism <ism_script_hash> \
  --custom-ism-policy <ism_state_nft_policy>
```

- `--custom-ism-policy` is **required whenever `--custom-ism` is given** (the CLI rejects one without the other). Both identifiers are written into the config UTXO's `IsmConfig { script_hash, state_nft_policy }` datum, which the mailbox reads to authenticate the override.
- The override ISM must hold its own `ISM State` NFT under the given policy — the script hash alone is not accepted. This authenticates the ISM's validator set, not just its code.
- **Security requirement**: only enable an override for a recipient whose constructor-0 `Init` is owner-gated (see above). Because the canonical config NFT is minted purely on a constructor-0 spend of the recipient script, a recipient that does not owner-gate `Init` lets an attacker install their own `IsmConfig` and bypass verification. Do not enable overrides for external recipients until this is audited.

> **Note**: Warp routes use a different co-spending pattern internally (parameterized by `mailbox_policy_id`), but this is handled by the built-in warp route contracts. Custom recipients should always use the verified message NFT pattern above.

### 6.3 Test the Greeting End-to-End

This exercises the full inbound path for a **generic (non-warp) recipient**:
Sepolia dispatch → relayer delivers a `verified_message_nft` to the greeting
address → you consume it with `greeting receive` → the datum updates. Requires
the Sepolia infrastructure (see the Sepolia appendix) and gas configured/enforced
(see the Gas Payment appendix).

```bash
export ETH_RPC_URL=$SEPOLIA_RPC_URL

# 1) Greeting script hash from deployment (recipients[].script_hash)
GREETING_HASH=$(jq -r '.recipients[0].script_hash' deployments/$NETWORK/deployment_info.json)
# Script recipients use the 0x02 prefix + 28-byte script hash:
RECIPIENT="0x02000000$GREETING_HASH"

# 2) Dispatch from Sepolia. The mailbox's required hook is a StaticProtocolFee
#    needing --value 1; interchain gas is paid separately (next step).
#    Body is arbitrary bytes; the greeting prepends "Hello, " to it. "Alice" = 0x416c696365
MSG=$(cast send $SEPOLIA_MAILBOX "dispatch(uint32,bytes32,bytes)(bytes32)" \
  2003 $RECIPIENT 0x416c696365 --value 1 --private-key $EVM_SIGNER_KEY --json \
  | jq -r '.logs[] | select(.topics[0]=="0x788dbc1b7152732178210e7f4d9d010ef016f9eafbe66786bd7169f56e0c353a") | .topics[1]')
echo "Message: $MSG"

# 3) Pay the Sepolia IGP for delivery. Script recipients cost more than warp
#    (they carry the whole message body in a verified-message UTXO):
#    gasAmount ≈ 1.5 × (1_720_800 + 4_400 × body_bytes) lovelace. 200000 covers a short body.
cast send $IGP "payForGas(bytes32,uint32,uint256,address)" \
  $MSG 2003 200000 $EVM_SIGNER_ADDRESS --value <wei ≥ quote> --private-key $EVM_SIGNER_KEY

# 4) Wait for the relayer to mint the verified_message_nft at the greeting address
$CLI --network $NETWORK greeting list        # shows the pending message

# 5) Consume it. MUST be signed by the greeting OWNER key (the --owner from
#    init recipient; defaults to the init signer). Auto-discovers the message UTXO.
$CLI --signing-key $GREETING_OWNER_KEY --network $NETWORK greeting receive

# 6) Verify the datum updated
$CLI --network $NETWORK greeting show
# Expected: last_greeting = "Hello, Alice", greeting_count incremented
```

If `greeting receive` fails the on-chain owner check, you signed with the wrong
key — see Troubleshooting. If it never appears in `greeting list`, the message
was rejected for insufficient gas (check the relayer for
`GasPaymentRequirementNotMet`) — top it up per the Gas Payment appendix.

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
  Hyperlane Address: 0x010000007c90fa689949238c5cb56c20caa92d50ae05074837e5006314e8a849

Deployment saved to: deployments/preview/native_warp_route.json
```

The Hyperlane address (`0x01000000 || nft_policy_id`) is used when enrolling this route on remote chains.

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
  Hyperlane Address: 0x01000000b6a3f69a99b75d852f689b5d1405c7cd76b298fc5ff7db36941b1dc1

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

```bash
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  warp deploy \
  --token-type synthetic \
  --decimals 6 \
  --remote-decimals 18
```

**Parameters:**

- `--decimals 6`: Cardano-side decimals (max 6 due to i64 token amount constraint)
- No token policy needed - the synthetic minting policy is generated automatically

**Output:**

```
Warp route deployed!
  Type: Synthetic
  Script Hash: 2bc528ef916747a2f320107be4bade841fc114dfa8aa9ab473f8f9d9
  NFT Policy: fc0d436644772ca43b9374f9e7a3dd298609099b4af7309f49bf60c1
  Synthetic Minting Policy: 91d297366830695e0688f01f3f704c9e45a2356574f3827e26768032
  Reference Script UTXO: eca38472b3d7f97201dfe62df753b1ac47a4fc6b31ae81dd139e4e8bdb35844d#1
  Hyperlane Address: 0x01000000fc0d436644772ca43b9374f9e7a3dd298609099b4af7309f49bf60c1

Auto-deploying minting policy reference script...
  Minting ref UTXO: 5678efgh...#0

Deployment saved to: deployments/preview/synthetic_warp_route.json
```

The CLI automatically deploys the minting policy reference script after the synthetic warp route. This reference script is required for the relayer to mint synthetic tokens on inbound transfers.

#### Manual Minting Ref Deployment (if needed)

If you need to redeploy the minting reference script separately:

```bash
WARP_POLICY="fc0d436644772ca43b9374f9e7a3dd298609099b4af7309f49bf60c1"

BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY \
  --network $NETWORK \
  warp deploy-minting-ref \
  --warp-policy $WARP_POLICY
```

### 7.5 Enroll Remote Routers

For bidirectional transfers, you must enroll the remote chain's warp route address on the Cardano side.

```bash
# Enroll Sepolia warp route on Cardano
REMOTE_DOMAIN=11155111  # Sepolia domain ID
REMOTE_ROUTER="0x000000000000000000000000d74122654d6be10ac086ff6764bd9edc651d36e0"  # Sepolia warp route address (H256)
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

> **Important**: You must also enroll the Cardano warp route on the remote chain. Use the Hyperlane address from the deployment output (e.g., `0x010000007c90fa68...` — the `0x01000000` prefix + the NFT policy ID, **not** the script hash).

### 7.6 Verify Warp Route Deployment

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

### 7.7 Test Warp Route Transfer (E2E Testing)

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

# Remote chain configuration (Sepolia example)
export EVM_RPC_URL="https://sepolia.drpc.org"
export EVM_SIGNER_KEY="0xyour_private_key"
export EVM_SIGNER_ADDRESS="0xYourAddress"

# Domain IDs
CARDANO_DOMAIN=2003       # Cardano Preview
EVM_DOMAIN=11155111         # Ethereum Sepolia
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
# Transfer 10 ADA to Sepolia
# Note: Amount is in lovelace (1 ADA = 1,000,000 lovelace)
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
$CLI --signing-key $CARDANO_SIGNING_KEY --network $NETWORK \
  warp transfer \
  --warp-policy $WARP_POLICY \
  --domain $EVM_DOMAIN \
  --recipient "0x000000000000000000000000$EVM_SIGNER_ADDRESS" \
  --amount 10000000 \
  --gas-limit 0
```

> **Gas:** `--gas-limit` bundles the IGP payment; with the Cardano overhead
> configured (Gas Payment appendix), `--gas-limit 0` pays `0 + overhead` ≈ 1.5×
> the delivery cost. **Without a gas payment the relayer rejects the message**
> (`GasPaymentRequirementNotMet`) once `onChainFeeQuoting` enforcement is on.

**Expected output:**

```
Transfer initiated!
  Transaction: abc123...
  Message ID: 0x1234567890abcdef...
  Sender: 0x010000007c90fa689949238c5cb56c20caa92d50ae05074837e5006314e8a849
  Recipient: 0x0000000000000000000000001f26bfc6f52cbfad5c3fa8dabb71007b28bf4749
  Amount: 10000000 (10.000000 local units → 10.000000000000000000 remote units)
```

**Step 4: Monitor the relayer**

```bash
docker compose -f e2e-docker/docker-compose.yml logs -f relayer
```

Look for:

- `Dispatched message to destination` - Message indexed on Cardano
- `Message successfully processed` - Delivery confirmed on Sepolia

**Step 5: Verify receipt on Sepolia**

```bash
# Check wADA balance on Sepolia (should show 10 * 10^18 = 10000000000000000000)
cast call $EVM_WARP_ROUTE "balanceOf(address)(uint256)" $EVM_SIGNER_ADDRESS --rpc-url $EVM_RPC_URL
```

---

#### Test 2: Native Warp Route — Remote → Cardano (Inbound)

This test burns synthetic wADA on the remote chain and releases ADA on Cardano.

**Prerequisites:**

- Completed Test 1 (have wADA tokens on Sepolia)
- Same infrastructure running

**Step 1: Check wADA balance on Sepolia**

```bash
# Get token info
cast call $EVM_WARP_ROUTE "symbol()(string)" --rpc-url $EVM_RPC_URL
cast call $EVM_WARP_ROUTE "decimals()(uint8)" --rpc-url $EVM_RPC_URL

# Check balance (should have tokens from Test 1)
cast call $EVM_WARP_ROUTE "balanceOf(address)(uint256)" $EVM_SIGNER_ADDRESS --rpc-url $EVM_RPC_URL
```

**Step 2: Get interchain gas quote**

```bash
GAS_QUOTE=$(cast call $EVM_WARP_ROUTE \
  "quoteGasPayment(uint32)(uint256)" $CARDANO_DOMAIN \
  --rpc-url $EVM_RPC_URL)
echo "Gas quote: $GAS_QUOTE wei ($(echo "scale=6; $GAS_QUOTE / 1000000000000000000" | bc) ETH)"
```

**Step 3: Execute the transfer**

```bash
# Transfer 5 wADA back to Cardano
# Amount is in wei (5 wADA with 18 decimals = 5 * 10^18)
cast send $EVM_WARP_ROUTE \
  "transferRemote(uint32,bytes32,uint256)" \
  $CARDANO_DOMAIN \
  $CARDANO_RECIPIENT \
  5000000000000000000 \
  --value $GAS_QUOTE \
  --rpc-url $EVM_RPC_URL \
  --private-key $EVM_SIGNER_KEY
```

**Step 4: Monitor the relayer**

```bash
docker compose -f e2e-docker/docker-compose.yml logs -f relayer
```

Look for:

- `Fetching metadata for message` - Relayer detected the Sepolia message
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

- **Sepolia**: Collateral warp route (locks ERC20 tokens)
- **Cardano**: Synthetic warp route (mints/burns synthetic tokens)

**Prerequisites:**

- Collateral warp route deployed on Sepolia with an ERC20 token
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

# Sepolia collateral warp route address
EVM_COLLATERAL_WARP="0xYourCollateralWarpRouteAddress"
```

**Step 2: Approve token spending on Sepolia**

```bash
# Get the ERC20 token address from the collateral warp route
TOKEN_ADDRESS=$(cast call $EVM_COLLATERAL_WARP "wrappedToken()(address)" --rpc-url $EVM_RPC_URL)

# Approve the warp route to spend tokens
cast send $TOKEN_ADDRESS \
  "approve(address,uint256)" \
  $EVM_COLLATERAL_WARP \
  1000000000000000000000 \
  --rpc-url $EVM_RPC_URL \
  --private-key $EVM_SIGNER_KEY
```

**Step 3: Transfer tokens to Cardano**

```bash
# Get gas quote
GAS_QUOTE=$(cast call $EVM_COLLATERAL_WARP \
  "quoteGasPayment(uint32)(uint256)" $CARDANO_DOMAIN \
  --rpc-url $EVM_RPC_URL)

# Transfer 10 tokens to Cardano
# Note: If Sepolia token has 18 decimals and Cardano has 6, relayer handles conversion
cast send $EVM_COLLATERAL_WARP \
  "transferRemote(uint32,bytes32,uint256)" \
  $CARDANO_DOMAIN \
  $CARDANO_RECIPIENT \
  10000000000000000000 \
  --value $GAS_QUOTE \
  --rpc-url $EVM_RPC_URL \
  --private-key $EVM_SIGNER_KEY
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
# Transfer 5 synthetic tokens back to Sepolia
# Amount is in Cardano units (5 tokens with 6 decimals = 5,000,000)
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
$CLI --signing-key $CARDANO_SIGNING_KEY --network $NETWORK \
  warp transfer \
  --warp-policy $WARP_POLICY \
  --domain $EVM_DOMAIN \
  --recipient "0x000000000000000000000000$EVM_SIGNER_ADDRESS" \
  --amount 5000000 \
  --gas-limit 0
```

> **Gas:** as above — `--gas-limit` bundles the IGP payment; omit it and the
> relayer rejects the message under gas enforcement. See the Gas Payment appendix.

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

# After delivery, check collateral tokens released on Sepolia
cast call $TOKEN_ADDRESS "balanceOf(address)(uint256)" $EVM_SIGNER_ADDRESS --rpc-url $EVM_RPC_URL
```

---

#### Troubleshooting E2E Tests

**Common Issues:**

| Error                         | Cause                                          | Solution                                                                  |
| ----------------------------- | ---------------------------------------------- | ------------------------------------------------------------------------- |
| `MissingScriptWitnessesUTXOW` | Reference scripts not found                    | Deploy minting ref script (for synthetic) and verify NFT policy IDs       |
| `BabbageNonDisjointRefInputs` | Same UTXO used as reference and spending input | Check ref script NFT asset name is correct (should be `726566` for "ref") |
| `RecipientNotFound`           | Recipient UTXO not indexed yet                 | Wait for Blockfrost to index the deployment transaction                   |
| `InsufficientBalance`         | Not enough tokens/ADA                          | Check UTXO balances before transfer                                       |
| `NoRelayerActivity`           | Relayer not detecting messages                 | Check relayer logs, verify domain configuration                           |
| `GasPaymentFailed`            | Insufficient ETH for gas                      | Ensure adequate ETH balance for `--value` parameter                      |

**Checking Message Status:**

1. **On Cardano (outbound):** Check that the dispatch transaction was confirmed and note the message ID
2. **On Sepolia (outbound delivery):** Use Hyperlane Explorer or query the mailbox contract
3. **On relayer:** Look for message indexing and delivery logs

**Verifying Relayer Configuration:**

```bash
# Check relayer is properly configured for both domains
docker compose -f e2e-docker/docker-compose.yml exec relayer cat /config/relayer.json

# Verify required fields:
# - Cardano chain with correct mailbox, ISM, and warp route addresses
# - Sepolia chain with correct RPC URL and contract addresses
# - Signing keys for both chains
```

**Decimal Handling:**

Cardano warp routes support a maximum of 6 decimals due to the i64 token amount limit. When bridging to/from chains with higher decimals (e.g., 18 on EVM):

- **Outbound (Cardano → EVM):** Amount is scaled up (e.g., 1,000,000 → 1,000,000,000,000,000,000)
- **Inbound (EVM → Cardano):** Amount is scaled down (e.g., 1,000,000,000,000,000,000 → 1,000,000)

Ensure your warp route is configured with correct `decimals` and `remote_decimals` values during deployment.

### 7.8 Complete Warp Route Deployment Script

```bash
#!/bin/bash
set -e

# Configuration
export NETWORK="preview"
export BLOCKFROST_API_KEY="your_api_key_here"
export CARDANO_SIGNING_KEY="./keys/payment.skey"

CLI="./cli/target/release/hyperlane-cardano"

# Sepolia configuration (example remote chain)
EVM_DOMAIN=11155111
EVM_WARP_ROUTE="0x000000000000000000000000d74122654d6be10ac086ff6764bd9edc651d36e0"

echo "=== Warp Route Deployment ==="

# 1. Deploy Native ADA warp route (waits for confirmation automatically)
echo "Deploying native ADA warp route..."
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
$CLI --signing-key $CARDANO_SIGNING_KEY --network $NETWORK \
  warp deploy \
  --token-type native \
  --decimals 6 \
  --remote-decimals 18

# 2. Get deployed warp route info
NATIVE_WARP=$(cat deployments/$NETWORK/native_warp_route.json)
NATIVE_SCRIPT_HASH=$(echo $NATIVE_WARP | jq -r '.warp_route.script_hash')
NATIVE_NFT_POLICY=$(echo $NATIVE_WARP | jq -r '.warp_route.nft_policy')

# 3. Enroll remote router
echo "Enrolling remote router..."
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
$CLI --signing-key $CARDANO_SIGNING_KEY --network $NETWORK \
  warp enroll-router \
  --domain $EVM_DOMAIN \
  --router $EVM_WARP_ROUTE \
  --warp-policy $NATIVE_NFT_POLICY

echo ""
echo "=== Deployment Complete ==="
echo "Cardano Warp Route Address: 0x01000000$NATIVE_NFT_POLICY"
echo ""
echo "Next steps:"
echo "1. Enroll the Cardano warp route on Sepolia using the address above"
echo "2. Start the relayer with the updated configuration"
echo "3. Test a transfer using: warp transfer --domain $EVM_DOMAIN ..."
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

#### "ScriptIntegrityHashMismatch" / cost model errors

**Cause**: The PlutusV3 cost model used to build the script-data hash must match the chain's exactly. The CLI and relayer read it live from `cost_models_raw` in the current protocol parameters (it is not hardcoded, because the parameter set grows at hard forks). If Blockfrost returns parameters without that field, transaction building fails rather than falling back to a stale table.

**Solution**: Confirm `query params` returns a populated `cost_models_raw.PlutusV3`, and that `BLOCKFROST_API_KEY` points at the same network you are deploying to. Retry once the parameters endpoint responds normally.

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

#### Deployer/init operations fail with UTXO selection errors (`NoCollateralInputs`, `BadInputsUTxO`, or seemingly-random init failures)

**Cause**: `init`/seed operations need clean, ADA-only UTXOs to pick from. A wallet whose UTXOs are mostly token-bearing (leftover NFTs/tokens from prior deployments) can fail to find a suitable collateral or seed UTXO.

**Solution**: Consolidate and then split out clean ADA-only outputs before a deployment run:

```bash
./cli/target/release/hyperlane-cardano --signing-key $CARDANO_SIGNING_KEY --network $NETWORK \
  utxo consolidate --max 20

./cli/target/release/hyperlane-cardano --signing-key $CARDANO_SIGNING_KEY --network $NETWORK \
  utxo split --utxo <tx_hash>#<index> --count 10 --amount 20000000
```

Also do not run the CLI **concurrently** against the same signing key (e.g. in parallel scripts) — both invocations can pick the same collateral/input UTXO and one will fail with `NoCollateralInputs` / `BadInputsUTxO`. Run deployment commands sequentially.

#### Validator never publishes `checkpoint_0` after a mailbox redeploy

**Symptom**: after redeploying the mailbox (new state NFT policy) and clearing the validator's S3 checkpoints/DB, the validator ingests the first leaf but `checkpoint_queue_len` stays `0` and it never signs.

**Cause**: the validator only captures and signs the tip as part of its *startup* routine. A validator process that was already running through the mailbox redeploy keeps operating against its old in-memory state.

**Solution**: after redeploying the mailbox — and after clearing the validator's S3 checkpoints, reorg flag, and local DB volume for that chain — **restart** the validator process/container (`docker compose restart validator-cardano` or equivalent). A stale `local_merkle_root` left in the DB will otherwise also trigger a reorg flag and the validator will refuse to sign at all.

#### `greeting receive` fails despite a valid verified message NFT

**Cause**: `greeting receive` (and any recipient action gated by the `owner` parameter) must be signed by the **recipient owner key** — the key whose public key hash was passed as `--owner` at `init recipient` (defaults to the init signer's key if `--owner` was omitted). Signing with a different key fails the on-chain owner check even if the message itself is valid.

**Solution**: use the same signing key that deployed/owns the recipient, or re-derive the owner pkh with `cat deployments/$NETWORK/owner.pkh` and confirm it matches your signing key's hash.

---

## Complete Deployment Script

Here's a complete script for deploying all contracts. The CLI waits for TX confirmation by default, so no manual sleep/polling is needed between steps:

```bash
#!/bin/bash
set -e

# Configuration
export NETWORK="preview"
export BLOCKFROST_API_KEY="your_api_key_here"
export CARDANO_SIGNING_KEY="./keys/payment.skey"
export LOCAL_DOMAIN=2003
export ORIGIN_DOMAINS="11155111"  # Sepolia
export ISM_MODULE_TYPE="messageid"  # or "merkleroot" - see Phase 3.4 for running both

# ISM configuration (required for a working ISM - threshold 0 rejects everything on-chain)
export VALIDATORS="11155111:b22b65f202558adf86a8bb2847b76ae1036686a5,469f0940684d147defc44f3647146cb90dd0bc8e,d3c75dcf15056012a4d74c483a0c6ea11d8c2b83"
export THRESHOLDS="11155111:2"

# Validator announcement (outbound direction: Cardano validator -> remote chains)
export STORAGE_LOCATION="s3://my-bucket/my-region/my-folder"
export VALIDATOR_KEY="0x2e0afff1080232cd5fc8fe769dd72f5766e4e0b66e5528fa93f80e75aca9e764"

CLI="./cli/target/release/hyperlane-cardano"
DEPLOY_DIR="./deployments/$NETWORK"

echo "=== Hyperlane Cardano Deployment ==="
echo "Network: $NETWORK"
echo "Domain: $LOCAL_DOMAIN"
echo ""

# Step 1: Build contracts
echo "Step 1: Building contracts..."
cd contracts && aiken build && cd ..

# Step 2: Extract validators (writes both message_id_multisig_ism.* and
# merkle_root_multisig_ism.*; --ism-module-type picks the *default* one)
echo "Step 2: Extracting validators..."
$CLI --network $NETWORK deploy extract --output $DEPLOY_DIR --ism-module-type $ISM_MODULE_TYPE

# Step 3: Initialize core contracts + configure ISM + announce validator
echo "Step 3: Initializing core contracts..."
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
$CLI --signing-key $CARDANO_SIGNING_KEY --network $NETWORK \
  init all \
  --domain $LOCAL_DOMAIN \
  --origin-domains "$ORIGIN_DOMAINS" \
  --validators "$VALIDATORS" \
  --thresholds "$THRESHOLDS" \
  --storage-location "$STORAGE_LOCATION" \
  --validator-key "$VALIDATOR_KEY"

# Step 3b: Initialize the IGP (init all does NOT do this)
echo "Step 3b: Initializing IGP..."
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
$CLI --signing-key $CARDANO_SIGNING_KEY --network $NETWORK \
  init igp --oracle "11155111:1000000000:7171:150000"

# Step 4: Deploy reference scripts (mailbox + default ISM + every alt_isms[] entry)
echo "Step 4: Deploying reference scripts..."
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
$CLI --signing-key $CARDANO_SIGNING_KEY --network $NETWORK \
  deploy reference-scripts-all

# Step 5: Configure mailbox with default ISM
echo "Step 5: Configuring mailbox..."
if [ "$ISM_MODULE_TYPE" = "merkleroot" ]; then
  ISM_HASH=$(cat $DEPLOY_DIR/merkle_root_multisig_ism.hash)
else
  ISM_HASH=$(cat $DEPLOY_DIR/message_id_multisig_ism.hash)
fi
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
$CLI --signing-key $CARDANO_SIGNING_KEY --network $NETWORK \
  mailbox set-default-ism --ism-hash $ISM_HASH

# Step 6: Generate relayer configuration
echo "Step 6: Generating agent configs..."
$CLI --network $NETWORK \
  config update-relayer --dry-run

# Step 7: Generate .env file (starting point only - variable names drift from
# what e2e-docker/.env expects, and verifiedMessageNftScriptCbor/warp entries
# can be stale; reconcile by hand against "Appendix: Agent Configuration
# Requirements" below)
echo "Step 7: Generating .env file..."
$CLI --network $NETWORK \
  config generate-env --output $DEPLOY_DIR/.env.generated

# Step 8: Verify deployment
echo "Step 8: Verifying deployment..."
$CLI --network $NETWORK init status
$CLI --network $NETWORK mailbox show
$CLI --network $NETWORK ism show

echo ""
echo "=== Deployment Complete ==="
echo "Deployment info saved to: $DEPLOY_DIR/deployment_info.json"
echo "Environment file: $DEPLOY_DIR/.env.generated (reconcile by hand, see caveat above)"
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
  }
}
```

---

## Appendix: CLI Command Reference

### Global Flags

| Flag             | Description                                    |
| ---------------- | ---------------------------------------------- |
| `--network`      | Cardano network (preview, preprod, mainnet)    |
| `--signing-key`  | Path to Ed25519 signing key                    |
| `--no-wait`      | Skip TX confirmation waiting (default: waits)  |

### Deploy Commands

| Command                        | Description                                                                    |
| ------------------------------ | -------------------------------------------------------------------------------- |
| `deploy extract`               | Extract validators from plutus.json (`--ism-module-type messageid\|merkleroot` picks the default ISM; writes both flavours' files regardless) |
| `deploy info`                  | Show validator information                                                       |
| `deploy generate-config`       | Generate deployment configuration                                                |
| `deploy reference-script`      | Deploy single reference script                                                   |
| `deploy reference-scripts-all` | Deploy reference scripts for mailbox + default ISM + every `alt_isms[]` entry     |

### Init Commands

| Command          | Description                                                                                             |
| ---------------- | ---------------------------------------------------------------------------------------------------------- |
| `init mailbox`   | Initialize mailbox contract                                                                                 |
| `init ism`       | Initialize an ISM instance (`--module-type` selects/adds a flavour — see [Dual-ISM](#34-deploying-both-ism-flavours-dual-ism)) |
| `init igp`       | Initialize the Interchain Gas Paymaster (not covered by `init all`)                                          |
| `init recipient` | Initialize a recipient contract                                                                              |
| `init all`       | Initialize mailbox + default ISM (optionally configure ISM validators + validator announce too); does NOT init IGP |
| `init status`    | Show initialization status                                                                                   |

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

### Config Commands

| Command                 | Description                                              |
| ----------------------- | -------------------------------------------------------- |
| `config update-relayer` | Generate relayer config with all fields from deployment   |
| `config update-validator` | Generate validator config from deployment               |
| `config generate-env`   | Generate .env file with all deployment values            |
| `config show`           | Show current Cardano config from relayer config          |

### Query Commands

| Command         | Description              |
| --------------- | ------------------------ |
| `query mailbox` | Query mailbox state                          |
| `query ism`     | Query ISM configuration                      |
| `query message` | Check if a message has been delivered (by ID) |
| `query utxos`   | List UTXOs at an address                     |
| `query utxo`    | Query specific UTXO                          |
| `query params`  | Get protocol parameters                      |
| `query tip`     | Get latest slot                              |

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
# Example: Apply verified_message_nft_policy to the greeting validator
aiken blueprint apply \
  -v greeting.greeting \
  -o greeting_applied.plutus \
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
                    ┌─────────────────┼─────────────────┐
                    │                                   │
                    ▼                                   ▼
          ┌─────────────────────┐            ┌─────────────────────┐
          │  mailbox_policy_id  │            │   ism_policy_id     │
          │                     │            │                     │
          │ Identifies mailbox  │            │ Identifies ISM      │
          │ state UTXO          │            │ state UTXO          │
          └──────────┬──────────┘            └─────────────────────┘
           │
           │ Used as parameter for:
           │
           ▼
┌─────────────────────────────┐
│  verified_message_nft       │
│  (mint)                     │
│                             │
│  Parameter: mailbox_policy  │
│                             │
│  Used for: Verified message │
│  tokens for recipients      │
└──────────┬──────────────────┘
           │
           │ verified_message_nft_policy
           │
           ▼
┌─────────────────────────────┐
│  mailbox (spend)            │
│                             │
│  Parameters:                │
│  verified_message_nft_      │
│  policy, ism_nft_policy     │
│                             │
│  Replay protection: SMT     │
│  in mailbox datum           │
└─────────────────────────────┘               │
                                              │
                                              ▼
                               ┌─────────────────────────────┐
                               │  greeting (spend)           │
                               │                             │
                               │  Parameter:                 │
                               │  verified_message_nft_policy│
                               │                             │
                               │  Example recipient that     │
                               │  stores greeting messages   │
                               └─────────────────────────────┘
```

### Script Parameterization Table

| Script                       | Type  | Parameter(s)                                                  | Parameter Source                                     | Purpose                                    |
| ---------------------------- | ----- | ------------------------------------------------------------- | ---------------------------------------------------- | ------------------------------------------ |
| `state_nft`                  | Mint  | `utxo_ref: OutputReference`                                   | Any unspent UTXO                                     | One-shot minting, ensures unique NFT       |
| `mailbox`                    | Spend | `verified_message_nft_policy: PolicyId, ism_nft_policy: PolicyId` | Derived from `verified_message_nft` and ISM state NFT | Verified message minting + ISM verification |
| `message_id_multisig_ism`    | Spend | (none)                                                        | -                                                    | No parameters needed; MessageId flavour    |
| `merkle_root_multisig_ism`   | Spend | (none)                                                        | -                                                    | No parameters needed; MerkleRoot flavour   |
| `verified_message_nft`       | Mint  | `mailbox_policy_id: PolicyId`                                 | `state_nft` policy for mailbox                       | Ensures only mailbox can mint verified message NFTs |
| `greeting`                   | Spend | `verified_message_nft_policy: PolicyId, owner: VerificationKeyHash` | Derived from `verified_message_nft`; owner defaults to signing key | Example recipient, verifies message NFT    |
| `warp_route`                 | Spend | `mailbox_policy_id: PolicyId`                                 | `state_nft` policy for mailbox                       | Co-spends with mailbox                     |

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
  verified_message_nft                         verified_message_nft
  continue working                             would need redeployment
```

**Critical insight**: `verified_message_nft` is parameterized by `mailbox_policy_id` (stable) rather than `mailbox_script_hash` (changes with code). This ensures:

1. **Verified message tokens persist across upgrades**: Recipients continue recognizing message NFTs
2. **No recipient redeployment**: Recipients don't need updating when mailbox code changes
3. **Replay protection**: Handled by the sparse Merkle tree (SMT) in the mailbox datum

### Deployment Order (Parameterization-Aware)

Due to parameterization dependencies, contracts must be deployed in this specific order:

```
Step 1: Build all contracts
        └─ aiken build → plutus.json

Step 2: Initialize mailbox (creates mailbox_policy_id)
        ├─ Consumes a UTXO → creates unique state_nft policy
        └─ mailbox_policy_id = state_nft policy ID

Step 3: Apply mailbox_policy_id to verified_message_nft
        └─ aiken blueprint apply -v verified_message_nft ... "mailbox_policy_id"
           → verified_message_nft_policy

Step 4: Apply verified_message_nft_policy + ism_nft_policy to mailbox
        └─ aiken blueprint apply -v mailbox ... "verified_message_nft_policy" "ism_nft_policy"
           → mailbox_applied.plutus (final mailbox script)

Step 5: Deploy mailbox reference script
        └─ Uses mailbox_applied.plutus

Step 6: Initialize other core contracts (ISM)
        └─ Each gets its own state_nft policy

Step 7: Deploy recipients (optional)
        └─ Apply verified_message_nft_policy → greeting_applied.plutus
```

### CLI Automation

The Hyperlane CLI automates most parameterization steps. When you run:

```bash
./cli/target/release/hyperlane-cardano init all --domain 2003
```

The CLI internally:

1. Creates state NFT policies for mailbox and ISM
2. Applies `mailbox_policy_id` to `verified_message_nft`
3. Applies the resulting policies to `mailbox`
4. Saves all parameterized scripts to `deployments/<network>/`

For recipients (e.g., the greeting contract):

```bash
./cli/target/release/hyperlane-cardano init recipient \
  --custom-contracts ./contracts \
  --custom-module greeting \
  --custom-validator greeting
```

The CLI:

1. Reads `verified_message_nft_policy` from the mailbox's applied parameters in `deployment_info.json`
2. Applies it, together with the owner key hash, to the specified validator from the custom contracts' `plutus.json`
3. Mints the canonical config and state NFTs and deploys the three-UTXO canonical-config pattern

### Manual Parameterization Example

If you need to manually apply parameters (e.g., for custom contracts):

```bash
# 1. Get the verified_message_nft_policy (recipients are parameterized by this,
#    NOT by the mailbox state NFT policy) and your owner key hash
VERIFIED_MSG_NFT_POLICY=$(jq -r '.verified_message_nft.policy_id' deployments/preview/deployment_info.json)
OWNER_PKH=$(cat deployments/preview/owner.pkh)   # 28-byte hex of the recipient owner

# 2. Apply both parameters to your custom recipient (order matters)
cd contracts
aiken blueprint apply \
  -v my_custom_recipient.my_custom_recipient \
  -o ../deployments/preview/my_custom_recipient_applied.plutus \
  "$VERIFIED_MSG_NFT_POLICY" \
  "$OWNER_PKH"

# 3. The resulting script hash will differ from the base script
# because the parameters are now embedded in the bytecode
```

---

## Appendix: Agent Configuration Requirements

When configuring the Hyperlane agents (validator and relayer) for Cardano, several environment variables must be set correctly. This section documents all required variables and how to extract them from your deployment.

### Automated Configuration (Recommended)

The CLI can generate all agent configuration files and environment variables automatically:

```bash
cd cardano

# Generate relayer config (derives all parameterized values automatically)
./cli/target/release/hyperlane-cardano --network $NETWORK \
  config update-relayer --config-path config/relayer-config.json

# Generate validator config
./cli/target/release/hyperlane-cardano --network $NETWORK \
  config update-validator \
  --validator-key 0x2e0afff1080232cd5fc8fe769dd72f5766e4e0b66e5528fa93f80e75aca9e764

# Generate .env file with all deployment values
./cli/target/release/hyperlane-cardano --network $NETWORK \
  config generate-env --output deployments/$NETWORK/.env.generated
```

`config update-relayer` derives and sets all required fields including:
- Mailbox, ISM, IGP, VA policy IDs and script hashes
- Reference script UTXOs
- Verified message NFT policy + script CBOR
- Warp route reference script UTXO

Use `--dry-run` to preview without writing changes.

> **`config generate-env` caveat**: treat its output as a *starting point*, not a drop-in `.env`. Its variable names drift from what the `e2e-docker` stack's `.env` actually expects — e.g. it emits `CARDANO_VERIFIED_MSG_POLICY_ID` with no accompanying CBOR field, it can carry stale/duplicate warp route entries from earlier deployments, and it leaves placeholders for `CARDANO_SIGNER_KEY` / `CARDANO_INDEX_FROM`. Reconcile the generated file by hand against the **Environment Variables Overview** below and `e2e-docker/.env.example` before using it. In particular, the H256 address forms the agents actually read are:
>
> - `CARDANO_MAILBOX` / `CARDANO_ISM` / `CARDANO_IGP` = `0x00000000` + the contract's **script hash** (`.mailbox.hash` / `.ism.hash` / `.igp.hash` — *not* the state NFT policy)
> - `CARDANO_VALIDATOR_ANNOUNCE` = `0x00000000` + the VA **policy id** (`.validator_announce.hash` — for the VA this field holds the policy id, since VA has no separate spend-script/state-NFT split)
>
> See [Extracting Variables from deployment_info.json](#extracting-variables-from-deployment_infojson) below for the corrected extraction commands.

### Manual Configuration

If you need to extract values manually, see the sections below.

### Environment Variables Overview

#### Variables Used by Both Validator and Relayer

| Variable                          | Description                           | Source                                    |
| --------------------------------- | ------------------------------------- | ----------------------------------------- |
| `BLOCKFROST_API_KEY`              | Blockfrost API key for Cardano access | Blockfrost dashboard                      |
| `CARDANO_MAILBOX`                 | Mailbox identifier (H256 format)      | `0x00000000` + `.mailbox.hash` (script hash — **not** the state NFT policy) |
| `CARDANO_VALIDATOR_ANNOUNCE`      | Validator announce (H256 format)      | `0x00000000` + `.validator_announce.hash` (VA's policy id) |
| `CARDANO_MERKLE_TREE_HOOK`        | Merkle tree hook (H256 format)        | `0x00000000` + `.mailbox.hash`            |
| `CARDANO_ISM`                     | Default ISM identifier (H256 format)  | `0x00000000` + `.ism.hash` (script hash — **not** the state NFT policy) |
| `CARDANO_IGP`                     | IGP identifier (H256 format)          | `0x00000000` + `.igp.hash` (script hash)  |
| `CARDANO_MAILBOX_POLICY_ID`       | Mailbox state NFT policy              | `.mailbox.stateNftPolicy`                 |
| `CARDANO_MAILBOX_SCRIPT_HASH`     | Mailbox validator script hash         | `.mailbox.hash`                           |
| `CARDANO_MAILBOX_REF_UTXO`        | Mailbox reference script UTXO         | `.mailbox.referenceScriptUtxo`            |
| `CARDANO_ISM_SCRIPT_HASH`         | Default ISM validator script hash     | `.ism.hash`                               |
| `CARDANO_ISM_STATE_NFT_POLICY_ID` | Default ISM state NFT policy          | `.ism.stateNftPolicy`                     |
| `CARDANO_ISM_REF_UTXO`            | Default ISM reference script UTXO     | `.ism.referenceScriptUtxo`                |
| `CARDANO_IGP_SCRIPT_HASH`         | IGP validator script hash             | `.igp.hash`                               |
| `CARDANO_IGP_STATE_NFT_POLICY_ID` | IGP state NFT policy                  | `.igp.stateNftPolicy`                     |
| `CARDANO_VA_POLICY_ID`            | Validator announce policy ID          | `.validator_announce.hash`                |
| `CARDANO_VERIFIED_MSG_NFT_POLICY_ID`   | Verified message NFT policy      | `.verified_message_nft.policy_id`         |
| `CARDANO_VERIFIED_MSG_NFT_SCRIPT_CBOR` | Verified message NFT applied CBOR (mailbox-parameterized — see [critical gotcha](#troubleshooting-parameterization-issues) below) | `deployments/<net>/verified_message_nft_applied.plutus` `.cborHex` |
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
| `EVM_*`                                | Sepolia chain configuration (see Sepolia appendix) |

---

### Extracting Variables from deployment_info.json

> **Tip**: Use `config generate-env` as a starting point, but its variable names drift from what `e2e-docker/.env` expects — see the caveat above. The manual extraction below produces the values the agents actually need.

After deploying Cardano contracts, extract the required values:

```bash
cd cardano/deployments/preview

# H256 Contract Addresses (with 0x00000000 prefix) - use .hash (script hash), NOT .stateNftPolicy
export CARDANO_MAILBOX=0x00000000$(jq -r '.mailbox.hash' deployment_info.json)
export CARDANO_VALIDATOR_ANNOUNCE=0x00000000$(jq -r '.validator_announce.hash' deployment_info.json)
export CARDANO_MERKLE_TREE_HOOK=0x00000000$(jq -r '.mailbox.hash' deployment_info.json)
export CARDANO_ISM=0x00000000$(jq -r '.ism.hash' deployment_info.json)
export CARDANO_IGP=0x00000000$(jq -r '.igp.hash' deployment_info.json)

# Policy IDs and Script Hashes
export CARDANO_MAILBOX_POLICY_ID=$(jq -r '.mailbox.stateNftPolicy' deployment_info.json)
export CARDANO_MAILBOX_SCRIPT_HASH=$(jq -r '.mailbox.hash' deployment_info.json)
export CARDANO_ISM_SCRIPT_HASH=$(jq -r '.ism.hash' deployment_info.json)
export CARDANO_ISM_STATE_NFT_POLICY_ID=$(jq -r '.ism.stateNftPolicy' deployment_info.json)
export CARDANO_IGP_SCRIPT_HASH=$(jq -r '.igp.hash' deployment_info.json)
export CARDANO_IGP_STATE_NFT_POLICY_ID=$(jq -r '.igp.stateNftPolicy' deployment_info.json)
export CARDANO_VA_POLICY_ID=$(jq -r '.validator_announce.hash' deployment_info.json)

# Reference Script UTXOs
export CARDANO_MAILBOX_REF_UTXO=$(jq -r '.mailbox.referenceScriptUtxo | "\(.txHash)#\(.outputIndex)"' deployment_info.json)
export CARDANO_ISM_REF_UTXO=$(jq -r '.ism.referenceScriptUtxo | "\(.txHash)#\(.outputIndex)"' deployment_info.json)

# Verified Message NFT (mailbox-parameterized - regenerate after every mailbox redeploy)
export CARDANO_VERIFIED_MSG_NFT_POLICY_ID=$(jq -r '.verified_message_nft.policy_id' deployment_info.json)
export CARDANO_VERIFIED_MSG_NFT_SCRIPT_CBOR=$(python3 -c "import json;print(json.load(open('verified_message_nft_applied.plutus'))['cborHex'])")
```

#### One-liner Export Script

```bash
cd cardano/deployments/preview
eval $(jq -r '
  "export CARDANO_MAILBOX=0x00000000" + .mailbox.hash,
  "export CARDANO_VALIDATOR_ANNOUNCE=0x00000000" + .validator_announce.hash,
  "export CARDANO_MERKLE_TREE_HOOK=0x00000000" + .mailbox.hash,
  "export CARDANO_ISM=0x00000000" + .ism.hash,
  "export CARDANO_IGP=0x00000000" + .igp.hash,
  "export CARDANO_MAILBOX_POLICY_ID=" + .mailbox.stateNftPolicy,
  "export CARDANO_MAILBOX_SCRIPT_HASH=" + .mailbox.hash,
  "export CARDANO_ISM_SCRIPT_HASH=" + .ism.hash,
  "export CARDANO_ISM_STATE_NFT_POLICY_ID=" + .ism.stateNftPolicy,
  "export CARDANO_IGP_SCRIPT_HASH=" + .igp.hash,
  "export CARDANO_IGP_STATE_NFT_POLICY_ID=" + .igp.stateNftPolicy,
  "export CARDANO_VA_POLICY_ID=" + .validator_announce.hash,
  "export CARDANO_MAILBOX_REF_UTXO=" + (.mailbox.referenceScriptUtxo | "\(.txHash)#\(.outputIndex)"),
  "export CARDANO_ISM_REF_UTXO=" + (.ism.referenceScriptUtxo | "\(.txHash)#\(.outputIndex)")
' deployment_info.json)

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

> **Tip**: Use `config update-relayer` to generate this automatically. The command derives all parameterized values (verified message NFT) from the Plutus blueprint. This example mirrors the actual working config template at `cardano/e2e-docker/config/relayer-cardano-sepolia.json` — note the top-level `mailbox` / `interchainSecurityModule` / `merkleTreeHook` / `validatorAnnounce` / `interchainGasPaymaster` fields all use the **`0x00000000` prefix** (not `0x02000000` — that prefix is only for generic script recipients, not the mailbox/ISM/IGP themselves), and `mailbox`/`interchainSecurityModule`/`interchainGasPaymaster` are built from the **script hash**, not the state NFT policy (`connection.mailboxPolicyId` etc. hold the policy separately, for a different purpose):

```json
{
  "chains": {
    "cardanopreview": {
      "name": "cardanopreview",
      "domainId": 2003,
      "protocol": "cardano",
      "chainId": 2003,
      "blocks": {
        "confirmations": 1,
        "estimateBlockTime": 20,
        "reorgPeriod": 5
      },
      "connection": {
        "url": "https://cardano-preview.blockfrost.io/api/v0",
        "apiKey": "${BLOCKFROST_API_KEY}",
        "network": "preview",
        "mailboxPolicyId": "<mailbox_state_nft_policy_id>",
        "mailboxScriptHash": "<mailbox_script_hash>",
        "mailboxAssetNameHex": "<mailbox_nft_asset_name>",
        "mailboxReferenceScriptUtxo": "<tx_hash>#0",
        "ismPolicyId": "<ism_state_nft_policy_id>",
        "ismScriptHash": "<ism_script_hash>",
        "ismAssetNameHex": "<ism_nft_asset_name>",
        "ismReferenceScriptUtxo": "<tx_hash>#0",
        "igpScriptHash": "<igp_script_hash>",
        "validatorAnnouncePolicyId": "<va_policy_id>",
        "verifiedMessageNftPolicyId": "<verified_msg_nft_policy_id>",
        "verifiedMessageNftScriptCbor": "<cbor_hex_from_verified_message_nft_applied.plutus>",
        "warpRouteReferenceScriptUtxo": "<tx_hash>#1",
        "confirmationBlockDelay": 5
      },
      "index": {
        "from": 3936000
      },
      "rpcUrls": [{ "http": "https://cardano-preview.blockfrost.io/api/v0" }],
      "signer": { "type": "hexKey", "key": "${CARDANO_SIGNER_KEY}" },
      "mailbox": "0x00000000<mailbox_script_hash>",
      "validatorAnnounce": "0x00000000<va_policy_id>",
      "merkleTreeHook": "0x00000000<mailbox_script_hash>",
      "interchainSecurityModule": "0x00000000<ism_script_hash>",
      "interchainGasPaymaster": "0x00000000<igp_script_hash>"
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

**Every inbound delivery fails with a bare Plutus `error` (`ValidationTagMismatch … PlutusFailure`)**

The most common cause is a **stale `verifiedMessageNftScriptCbor` / `verifiedMessageNftPolicyId`** in the agent config. `verified_message_nft` is parameterized with the **mailbox state-NFT policy**, so a mailbox redeploy changes its applied CBOR *and* policy id. If the config still holds the old script, the relayer mints under the wrong policy and that policy — searching the tx inputs for a mailbox it no longer matches — throws a bare `error`. Fix: re-read both values from `deployments/<net>/verified_message_nft_applied.plutus`:

```bash
# CBOR: the applied script's cborHex
python3 -c "import json;print(json.load(open('deployments/preview/verified_message_nft_applied.plutus'))['cborHex'])"
# Policy id: blake2b-224 of (0x03 language tag || cbor bytes) for PlutusV3
python3 -c "import json,hashlib;c=json.load(open('deployments/preview/verified_message_nft_applied.plutus'))['cborHex'];print(hashlib.blake2b(bytes([3])+bytes.fromhex(c),digest_size=28).hexdigest())"
```

**Recipients bake in the same policy — redeploy them too.** `verified_message_nft` isn't only read by the relayer; every recipient contract (including `greeting`) is itself parameterized by `verified_message_nft_policy` (see [Script Parameterization Table](#script-parameterization-table)). After a mailbox redeploy, a recipient deployed against the *old* policy can never recognize the NFT the relayer delivers — there is no config value to patch, the policy is baked into the recipient's own script hash and address. Redeploy the recipient (`init recipient` again — it gets a new script address) whenever the mailbox is redeployed. See `design/merkleroot-ism-binding.md` §12 for the full mailbox-parameterization chain.

**MerkleRoot ISM: inbound messages never leave `CouldNotFetchMetadata` / "Unable to reach quorum"**

A MerkleRoot ISM needs a merkle **inclusion proof**, so the relayer must index the origin's tree from **leaf 0** (`highest_known_leaf_index()` is `None` until leaf 0 is stored — there is no snapshot import). For a **fresh** origin mailbox this is automatic. For a **busy shared** origin mailbox (e.g. the official Sepolia mailbox, ~870k leaves) you must backfill the whole tree once per fresh `relayer_db`:

1. Set the origin chain's `index.from` to the origin **MerkleTreeHook deploy block**.
2. Use an RPC with a large `eth_getLogs` range and set `index.chunk` near its cap. dRPC free tier caps at 10 000 blocks; **Tenderly allows ~500 000** — put it first in `rpcUrls` and set `"chunk": 99999`. The backfill then finishes in minutes.
3. Optionally add a relayer `whitelist` (`{"origindomain": [<origin>]}` or a specific `messageid`) so the message cursor doesn't hammer the destination RPC for undeliverable historical messages while the tree builds.

If the origin is a busy shared mailbox and you don't want a backfill, use a **MessageId** ISM there instead (no tree required) — this is also why the mailbox's *default* ISM should generally stay MessageId, with MerkleRoot only opted into per-recipient via [Dual-ISM](#34-deploying-both-ism-flavours-dual-ism) when a proof is actually needed. See `design/merkleroot-ism-binding.md` §11 and §13 for the full operational rationale.

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
  routers: [(11155111, 0x000...Sepolia_Router)],
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
  routers: [(11155111, 0x000...Sepolia_Router)],
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
  routers: [(11155111, 0x000...FTEST_Collateral)],
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
Format: 0x01000000 + nft_policy_id (28 bytes)

Example:
  NFT Policy:  7c90fa689949238c5cb56c20caa92d50ae05074837e5006314e8a849
  H256 Address: 0x010000007c90fa689949238c5cb56c20caa92d50ae05074837e5006314e8a849
                ^^^^^^^^ Policy-based prefix (0x01 = resolved by state NFT)
                        ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
                        28-byte NFT policy ID (zero-padded on left to 32 bytes)
```

Generic script recipients (without state NFTs) use `0x02000000 + script_hash` instead.

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

Example: Sending 10 ADA to Sepolia
  local_amount = 10,000,000 lovelace (10 ADA)
  local_decimals = 6
  remote_decimals = 18
  wire_amount = 10,000,000 * 10^(18-6) = 10,000,000,000,000,000,000
              = 10.0 with 18 decimals
```

> **Note**: When the remote chain has fewer decimals than Cardano, the conversion floor-divides and a small `local_amount` can truncate to a wire amount of `0`. The warp route rejects such transfers on-chain, so you cannot lock collateral without producing a corresponding remote credit. Size transfers above the remote decimal precision.

### Warp Route Identification

Warp routes are identified by their state NFT policy ID. The Hyperlane address is derived as `0x01000000 || nft_policy_id`. Generic script recipients use `0x02000000 || script_hash`. No separate registry registration is needed -- the relayer discovers warp routes directly via their NFT policy.

### E2E Testing Scenarios

The following scenarios test all warp route types bidirectionally:

| Scenario | Direction      | Type       | Action                                   |
| -------- | -------------- | ---------- | ---------------------------------------- |
| 1        | Cardano → Sepolia | Collateral | Lock WARPTEST, mint wCTEST on Sepolia       |
| 2        | Sepolia → Cardano | Synthetic  | Lock FTEST, mint wFTEST on Cardano       |
| 3        | Cardano → Sepolia | Native     | Lock ADA, mint wADA on Sepolia              |
| 4        | Sepolia → Cardano | Synthetic  | Lock ETH, mint wETH on Cardano         |
| 5        | Sepolia → Cardano | Native     | Burn wADA, release ADA on Cardano        |
| 6        | Sepolia → Cardano | Collateral | Burn wCTEST, release WARPTEST on Cardano |

> **Note**: For detailed step-by-step E2E testing instructions with Ethereum Sepolia, see [Appendix: Sepolia (Ethereum Testnet) Deployment Guide](#appendix-sepolia-ethereum-testnet-deployment-guide).

---

## Appendix: Sepolia (Ethereum Testnet) Deployment Guide

This appendix provides step-by-step instructions for deploying Hyperlane warp route infrastructure on Ethereum Sepolia testnet for E2E testing with Cardano.

### Prerequisites

#### 1. Install Foundry

```bash
curl -L https://foundry.paradigm.xyz | bash
foundryup
```

#### 2. Get Sepolia Test ETH

- Use the [Ethereum Sepolia Faucet](https://sepoliafaucet.com) to get test ETH
- You'll need at least 1 ETH for deployments

#### 3. Set Up Base Environment Variables

Create a `.env` file or export these variables. These are required for all subsequent steps:

```bash
# ============================================================
# BASE CONFIGURATION (Required for all steps)
# ============================================================

# Sepolia RPC endpoint
export EVM_RPC_URL="https://sepolia.drpc.org"

# Your Sepolia private key (with 0x prefix) - must have ETH for gas
export EVM_SIGNER_KEY="0x..."

# Sepolia Hyperlane infrastructure (already deployed on Sepolia testnet)
export EVM_MAILBOX="0xfFAEF09B3cd11D9b20d1a19bECca54EEC2884766"

# Domain IDs
export CARDANO_DOMAIN=2003  # Cardano Preview testnet
export EVM_DOMAIN=11155111    # Ethereum Sepolia testnet
```

### Deployment Flow Overview

The deployment follows this order, with each step producing outputs needed by subsequent steps:

```
Step 1: Deploy ISM ──────────────────► EVM_CARDANO_ISM
                                              │
Step 2: Deploy Warp Routes ──────────► EVM_SYNTHETIC_*, EVM_COLLATERAL_*, EVM_*
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

### Step 1: Deploy Cardano MultisigISM on Sepolia

The ISM (Interchain Security Module) validates messages coming from Cardano. It needs the Cardano validator's EVM address.

#### Required Environment Variables

| Variable            | Description                       | Example                                      |
| ------------------- | --------------------------------- | -------------------------------------------- |
| `EVM_SIGNER_KEY`   | Private key for Sepolia transactions | `0x...`                                      |
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
  --rpc-url $EVM_RPC_URL \
  --broadcast \
  --private-key $EVM_SIGNER_KEY

# ⚠️ IMPORTANT: Save the ISM address from the output
export EVM_CARDANO_ISM="0x..."  # Copy from "MultisigISM deployed:" line
```

---

### Step 2: Deploy Sepolia Warp Routes

This deploys all the test ERC20 tokens and warp routes needed for E2E testing.

#### Required Environment Variables

| Variable          | Description                       | Example |
| ----------------- | --------------------------------- | ------- |
| `EVM_SIGNER_KEY` | Private key for Sepolia transactions | `0x...` |

#### Optional Environment Variables for Token Customization

You can customize token names, symbols, and decimals via environment variables:

**Test ERC20 Tokens:**

| Variable          | Description              | Default           |
| ----------------- | ------------------------ | ----------------- |
| `FTEST_NAME`      | Name for FTEST token     | `Sepolia Test Token` |
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
| TestERC20            | Sepolia Test Token | FTEST  | 18       | Test token for Sepolia → Cardano     |
| TestERC20            | Wrapped ADA     | WADA   | 18       | Wrapped ADA for collateral route  |
| TestERC20            | Token A         | TOKA   | 18       | For collateral-collateral tests   |
| HypERC20 (Synthetic) | Wrapped CTEST   | wCTEST | 6        | Receives from Cardano collateral  |
| HypERC20 (Synthetic) | Wrapped ADA     | wADA   | 6        | Receives from Cardano native      |
| HypERC20Collateral   | -               | -      | -        | Locks FTEST for Cardano synthetic |
| HypERC20Collateral   | -               | -      | -        | Releases WADA for Cardano native  |
| HypNative            | -               | -      | -        | Locks native ETH                 |

#### 2.1 Deploy Warp Routes (Default Configuration)

```bash
cd solidity

forge script script/warp-e2e/DeploySepoliaWarp.s.sol:DeploySepoliaWarp \
  --rpc-url $EVM_RPC_URL \
  --broadcast \
  --private-key $EVM_SIGNER_KEY
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

forge script script/warp-e2e/DeploySepoliaWarp.s.sol:DeploySepoliaWarp \
  --rpc-url $EVM_RPC_URL \
  --broadcast \
  --private-key $EVM_SIGNER_KEY
```

#### 2.2 Save Output Addresses

The script outputs environment variables at the end. **Copy and export all of them**:

```bash
# ⚠️ IMPORTANT: Export ALL addresses from the deployment output

# Test ERC20 Tokens
export EVM_FTEST="0x..."           # Sepolia Test Token
export EVM_WADA="0x..."            # Wrapped ADA ERC20
export EVM_TOKENA="0x..."          # Token A

# Synthetic Warp Routes (mint tokens when receiving from Cardano)
export EVM_SYNTHETIC_WCTEST="0x..."   # Receives CTEST from Cardano, mints wCTEST
export EVM_SYNTHETIC_WADA="0x..."     # Receives ADA from Cardano, mints wADA

# Collateral Warp Routes (lock/release tokens)
export EVM_COLLATERAL_FTEST="0x..."   # Locks FTEST, Cardano receives synthetic wFTEST
export EVM_COLLATERAL_WADA="0x..."    # Releases WADA when Cardano sends ADA
export EVM_COLLATERAL_TOKENA="0x..."  # For collateral-collateral tests

# Native Warp Route
export EVM_NATIVE_ETH="0x..."        # Locks native ETH
```

---

### Step 3: Set Cardano ISM on Warp Routes

Configure the warp routes to use the Cardano ISM for validating inbound messages from Cardano.

#### Required Environment Variables

| Variable                | Description                       | Set In        |
| ----------------------- | --------------------------------- | ------------- |
| `EVM_SIGNER_KEY`       | Private key for Sepolia transactions | Prerequisites |
| `EVM_CARDANO_ISM`      | Cardano MultisigISM address       | Step 1        |
| `EVM_SYNTHETIC_WCTEST` | wCTEST synthetic route            | Step 2        |
| `EVM_SYNTHETIC_WADA`   | wADA synthetic route              | Step 2        |
| `EVM_COLLATERAL_FTEST` | FTEST collateral route            | Step 2        |
| `EVM_COLLATERAL_WADA`  | WADA collateral route             | Step 2        |

#### 3.1 Verify Variables Are Set

```bash
# Check all required variables are set
echo "ISM: $EVM_CARDANO_ISM"
echo "Synthetic wCTEST: $EVM_SYNTHETIC_WCTEST"
echo "Synthetic wADA: $EVM_SYNTHETIC_WADA"
echo "Collateral FTEST: $EVM_COLLATERAL_FTEST"
echo "Collateral WADA: $EVM_COLLATERAL_WADA"
```

#### 3.2 Set ISM on All Routes

```bash
cd solidity

forge script script/warp-e2e/DeployCardanoISM.s.sol:DeployCardanoISM \
  --sig "setISMOnWarpRoutes()" \
  --rpc-url $EVM_RPC_URL \
  --broadcast \
  --private-key $EVM_SIGNER_KEY
```

#### 3.3 Alternative: Set ISM on Single Route Using Cast

```bash
# Set ISM on a specific warp route manually
cast send $EVM_SYNTHETIC_WCTEST \
  "setInterchainSecurityModule(address)" \
  $EVM_CARDANO_ISM \
  --rpc-url $EVM_RPC_URL \
  --private-key $EVM_SIGNER_KEY
```

---

### Step 4: Mint Test Tokens

Mint test tokens to your wallet for testing transfers from Sepolia to Cardano.

#### Required Environment Variables

| Variable          | Description                       | Set In        |
| ----------------- | --------------------------------- | ------------- |
| `EVM_SIGNER_KEY` | Private key for Sepolia transactions | Prerequisites |
| `EVM_FTEST`      | FTEST token address               | Step 2        |
| `EVM_WADA`       | WADA token address                | Step 2        |
| `EVM_TOKENA`     | TokenA address                    | Step 2        |

#### 4.1 Mint Using Script

```bash
cd solidity

forge script script/warp-e2e/DeploySepoliaWarp.s.sol:DeploySepoliaWarp \
  --sig "mintTestTokens()" \
  --rpc-url $EVM_RPC_URL \
  --broadcast \
  --private-key $EVM_SIGNER_KEY
```

This mints 1,000,000 tokens (with 18 decimals) of each test token to your wallet.

#### 4.2 Alternative: Mint Using Cast

```bash
# Mint 1000 FTEST to your wallet
WALLET=$(cast wallet address --private-key $EVM_SIGNER_KEY)

cast send $EVM_FTEST \
  "mint(address,uint256)" \
  $WALLET \
  "1000000000000000000000" \
  --rpc-url $EVM_RPC_URL \
  --private-key $EVM_SIGNER_KEY
```

---

### Step 5: Pre-deposit Collateral (for Cardano → Sepolia)

For collateral routes that **release** tokens (like WADA when receiving ADA from Cardano), you must pre-deposit tokens into the collateral contract.

#### Required Environment Variables

| Variable                 | Description                       | Set In        |
| ------------------------ | --------------------------------- | ------------- |
| `EVM_SIGNER_KEY`        | Private key for Sepolia transactions | Prerequisites |
| `EVM_WADA`              | WADA token address                | Step 2        |
| `EVM_TOKENA`            | TokenA address                    | Step 2        |
| `EVM_COLLATERAL_WADA`   | WADA collateral route             | Step 2        |
| `EVM_COLLATERAL_TOKENA` | TokenA collateral route           | Step 2        |

#### 5.1 Pre-deposit Using Script

```bash
cd solidity

forge script script/warp-e2e/DeploySepoliaWarp.s.sol:DeploySepoliaWarp \
  --sig "preDepositCollateral()" \
  --rpc-url $EVM_RPC_URL \
  --broadcast \
  --private-key $EVM_SIGNER_KEY
```

This deposits 100,000 tokens to each collateral contract.

#### 5.2 Alternative: Pre-deposit Using Cast

```bash
# Transfer WADA directly to collateral contract
cast send $EVM_WADA \
  "transfer(address,uint256)" \
  $EVM_COLLATERAL_WADA \
  "100000000000000000000000" \
  --rpc-url $EVM_RPC_URL \
  --private-key $EVM_SIGNER_KEY
```

---

### Step 6: Enroll Cardano Routers on Sepolia

After deploying Cardano warp routes, enroll them as remote routers on the Sepolia warp routes.

#### Required Environment Variables

| Variable                   | Description                       | Format                     |
| -------------------------- | --------------------------------- | -------------------------- |
| `EVM_SIGNER_KEY`          | Private key for Sepolia transactions | `0x...`                    |
| `EVM_SYNTHETIC_WCTEST`    | Sepolia wCTEST synthetic             | `0x...` (20 bytes)         |
| `EVM_SYNTHETIC_WADA`      | Sepolia wADA synthetic               | `0x...` (20 bytes)         |
| `EVM_COLLATERAL_FTEST`    | Sepolia FTEST collateral             | `0x...` (20 bytes)         |
| `EVM_COLLATERAL_WADA`     | Sepolia WADA collateral              | `0x...` (20 bytes)         |
| `CARDANO_NATIVE_ADA`       | Cardano Native ADA route          | `0x01000000...` (32 bytes) |
| `CARDANO_COLLATERAL_CTEST` | Cardano Collateral CTEST route    | `0x01000000...` (32 bytes) |
| `CARDANO_SYNTHETIC_FTEST`  | Cardano Synthetic FTEST route     | `0x01000000...` (32 bytes) |

#### Optional Environment Variables

| Variable         | Description       | Default          |
| ---------------- | ----------------- | ---------------- |
| `CARDANO_DOMAIN` | Cardano domain ID | `2003` (Preview) |

#### 6.1 Get Cardano Warp Route Addresses

From your Cardano deployment, get the NFT policy IDs and convert to H256 format:

```bash
# Cardano warp routes use H256 format: 0x01000000 + 28-byte NFT policy ID
# The "01000000" prefix indicates a policy-based recipient (resolved by state NFT)

# Example: From Cardano CLI output or deployment artifacts
# If warp show --warp-policy returns NFT policy: 0ea635a9db202792c36ceec3a6c9d4bea53a15eb481eb545b6976ddb

# Native ADA warp route
export CARDANO_NATIVE_ADA="0x010000000ea635a9db202792c36ceec3a6c9d4bea53a15eb481eb545b6976ddb"

# Collateral CTEST warp route (for bridging Cardano native tokens)
export CARDANO_COLLATERAL_CTEST="0x01000000b72f2aeeddc9d0203429ecdb0fb1d65129592a9da62757a6bee7e472"

# Synthetic wFTEST warp route (receives FTEST from Sepolia)
export CARDANO_SYNTHETIC_FTEST="0x01000000503a80b8f25f64f5375f7b1cac6e862dd333ec3dace7dc9544e9040c"
```

> **Tip**: You can find NFT policy IDs in `cardano/deployments/preview/deployment_info.json` or by running `hyperlane-cardano warp show --warp-policy <NFT_POLICY>`.

#### 6.2 Run Enrollment Script

```bash
cd solidity

# Verify Cardano addresses are set
echo "Cardano Native ADA: $CARDANO_NATIVE_ADA"
echo "Cardano Collateral CTEST: $CARDANO_COLLATERAL_CTEST"
echo "Cardano Synthetic FTEST: $CARDANO_SYNTHETIC_FTEST"

forge script script/warp-e2e/EnrollCardanoRouters.s.sol:EnrollCardanoRouters \
  --rpc-url $EVM_RPC_URL \
  --broadcast \
  --private-key $EVM_SIGNER_KEY
```

#### 6.3 Alternative: Enroll Single Router

To enroll a single Cardano router on a specific Sepolia warp route:

```bash
# Set the specific route pair
export EVM_WARP_ROUTE="$EVM_SYNTHETIC_WADA"  # Sepolia route to configure
export CARDANO_ROUTER="$CARDANO_NATIVE_ADA"    # Cardano route to enroll

forge script script/warp-e2e/EnrollCardanoRouters.s.sol:EnrollCardanoRouters \
  --sig "enrollSingle()" \
  --rpc-url $EVM_RPC_URL \
  --broadcast \
  --private-key $EVM_SIGNER_KEY
```

#### 6.4 Alternative: Enroll Using Cast

```bash
# Enroll Cardano native ADA on Sepolia wADA synthetic
cast send $EVM_SYNTHETIC_WADA \
  "enrollRemoteRouter(uint32,bytes32)" \
  $CARDANO_DOMAIN \
  $CARDANO_NATIVE_ADA \
  --rpc-url $EVM_RPC_URL \
  --private-key $EVM_SIGNER_KEY
```

---

### Step 7: Verify Deployments

#### Check ISM Configuration

```bash
# Check ISM on a warp route (should return EVM_CARDANO_ISM address)
cast call $EVM_SYNTHETIC_WCTEST \
  "interchainSecurityModule()(address)" \
  --rpc-url $EVM_RPC_URL
```

#### Check Enrolled Routers

```bash
# Check if Cardano router is enrolled (should return non-zero bytes32)
cast call $EVM_SYNTHETIC_WCTEST \
  "routers(uint32)(bytes32)" \
  $CARDANO_DOMAIN \
  --rpc-url $EVM_RPC_URL

# Expected output: 0x01000000... (Cardano warp route address)
# If 0x0000...0000, enrollment failed or wasn't done
```

#### Check Token Balances

```bash
# Your wallet address
WALLET=$(cast wallet address --private-key $EVM_SIGNER_KEY)

# Check FTEST balance in your wallet
cast call $EVM_FTEST \
  "balanceOf(address)(uint256)" \
  $WALLET \
  --rpc-url $EVM_RPC_URL

# Check WADA balance in collateral contract (for Cardano → Sepolia releases)
cast call $EVM_WADA \
  "balanceOf(address)(uint256)" \
  $EVM_COLLATERAL_WADA \
  --rpc-url $EVM_RPC_URL
```

---

### Step 8: Test Transfer (Sepolia → Cardano)

> **Prerequisites**: Before testing transfers, ensure the Hyperlane validator and relayer agents are running and properly configured. See [Appendix: Agent Configuration Requirements](#appendix-agent-configuration-requirements) for setup instructions.

#### Required Environment Variables

| Variable                | Description                       |
| ----------------------- | --------------------------------- |
| `EVM_SIGNER_KEY`       | Private key for Sepolia transactions |
| `EVM_FTEST`            | FTEST token address               |
| `EVM_COLLATERAL_FTEST` | FTEST collateral warp route       |
| `CARDANO_DOMAIN`        | Cardano domain ID (2003)          |

#### 8.1 Approve Token Spending

```bash
# Approve FTEST collateral to spend your tokens
cast send $EVM_FTEST \
  "approve(address,uint256)" \
  $EVM_COLLATERAL_FTEST \
  "1000000000000000000000" \
  --rpc-url $EVM_RPC_URL \
  --private-key $EVM_SIGNER_KEY
```

#### 8.2 Prepare Cardano Recipient Address

```bash
# The recipient in transferRemote is the END USER wallet on Cardano,
# not the warp route (the route is resolved via enrolled routers).
#
# Format: 0x00000000 + 28-byte payment key hash
# The 4-byte prefix is stripped by the Cardano warp route — only the
# 28-byte credential matters. Use 0x00000000 for wallet recipients.

# Example: payment key hash from your Cardano wallet
CARDANO_RECIPIENT="0x000000001212a023380020f8c7b94b831e457b9ee65f009df9d1d588430dcc89"
```

#### 8.3 Initiate Transfer

```bash
# Transfer 5 FTEST (18 decimals) to Cardano
cast send $EVM_COLLATERAL_FTEST \
  "transferRemote(uint32,bytes32,uint256)" \
  $CARDANO_DOMAIN \
  $CARDANO_RECIPIENT \
  "5000000000000000000" \
  --rpc-url $EVM_RPC_URL \
  --private-key $EVM_SIGNER_KEY

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
# Sepolia E2E Deployment Environment Variables

# ============================================================
# PREREQUISITES (Set before starting)
# ============================================================
export EVM_RPC_URL="https://sepolia.drpc.org"
export EVM_SIGNER_KEY="0x..."  # Your Sepolia private key

# Sepolia Hyperlane Infrastructure (pre-deployed)
export EVM_MAILBOX="0xfFAEF09B3cd11D9b20d1a19bECca54EEC2884766"

# Domain IDs
export CARDANO_DOMAIN=2003
export EVM_DOMAIN=11155111

# ============================================================
# STEP 1: ISM Deployment Inputs
# ============================================================
export CARDANO_VALIDATOR="0x..."  # From: cast wallet address --private-key $CARDANO_VALIDATOR_KEY

# Optional
export CARDANO_ISM_THRESHOLD=1

# ============================================================
# STEP 1: ISM Deployment Outputs (set after deployment)
# ============================================================
export EVM_CARDANO_ISM="0x..."

# ============================================================
# STEP 2: Warp Route Deployment Outputs (set after deployment)
# ============================================================
# Test Tokens
export EVM_FTEST="0x..."
export EVM_WADA="0x..."
export EVM_TOKENA="0x..."

# Synthetic Routes
export EVM_SYNTHETIC_WCTEST="0x..."
export EVM_SYNTHETIC_WADA="0x..."

# Collateral Routes
export EVM_COLLATERAL_FTEST="0x..."
export EVM_COLLATERAL_WADA="0x..."
export EVM_COLLATERAL_TOKENA="0x..."

# Native Route
export EVM_NATIVE_ETH="0x..."

# ============================================================
# STEP 6: Cardano Router Enrollment Inputs
# (Get from Cardano deployment artifacts)
# ============================================================
export CARDANO_NATIVE_ADA="0x01000000..."
export CARDANO_COLLATERAL_CTEST="0x01000000..."
export CARDANO_SYNTHETIC_FTEST="0x01000000..."
```

---

### Warp Route Pairing Reference

| Scenario | Cardano Route    | Sepolia Route       | Direction      | Token Flow               |
| -------- | ---------------- | ---------------- | -------------- | ------------------------ |
| 1        | Collateral CTEST | Synthetic wCTEST | Cardano → Sepolia | Lock CTEST → Mint wCTEST |
| 2        | Synthetic wFTEST | Collateral FTEST | Sepolia → Cardano | Lock FTEST → Mint wFTEST |
| 3        | Native ADA       | Synthetic wADA   | Cardano → Sepolia | Lock ADA → Mint wADA     |
| 4        | Synthetic wETH  | Native ETH      | Sepolia → Cardano | Lock ETH → Mint wETH   |
| 5        | Native ADA       | Collateral WADA  | Cardano → Sepolia | Lock ADA → Release WADA  |

---

### Customizing Token Deployment

The `DeploySepoliaWarp.s.sol` script supports customization via environment variables.

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
forge script script/warp-e2e/DeploySepoliaWarp.s.sol:DeploySepoliaWarp \
  --rpc-url $EVM_RPC_URL \
  --broadcast \
  --private-key $EVM_SIGNER_KEY
```

See [Step 2](#step-2-deploy-sepolia-warp-routes) for the full list of customizable environment variables.

#### Option 2: Deploy Individual Contracts Manually

For complete control, deploy contracts individually using `forge create`:

```bash
# Deploy custom TestERC20
forge create script/warp-e2e/TestERC20.sol:TestERC20 \
  --constructor-args "My Token" "MTK" 18 \
  --rpc-url $EVM_RPC_URL \
  --private-key $EVM_SIGNER_KEY

# Deploy HypERC20 synthetic
forge create contracts/token/HypERC20.sol:HypERC20 \
  --constructor-args 6 1000000000000 $EVM_MAILBOX \
  --rpc-url $EVM_RPC_URL \
  --private-key $EVM_SIGNER_KEY

# Initialize the synthetic
WALLET=$(cast wallet address --private-key $EVM_SIGNER_KEY)
cast send $DEPLOYED_ADDRESS \
  "initialize(uint256,string,string,address,address,address)" \
  0 "Wrapped Token" "wTKN" "0x0000000000000000000000000000000000000000" $EVM_CARDANO_ISM $WALLET \
  --rpc-url $EVM_RPC_URL \
  --private-key $EVM_SIGNER_KEY
```

---

### Troubleshooting Sepolia Deployments

#### "Environment variable not set" Error

```bash
# Check which variables are missing
env | grep -E "^EVM_|^CARDANO_"

# Make sure to export (not just set) variables
export EVM_CARDANO_ISM="0x..."  # ✓ Correct
EVM_CARDANO_ISM="0x..."         # ✗ Won't be available to forge
```

#### "Execution reverted" on Transfer

1. **Check ISM is set correctly:**

   ```bash
   cast call $EVM_SYNTHETIC_WCTEST "interchainSecurityModule()(address)" --rpc-url $EVM_RPC_URL
   # Should return $EVM_CARDANO_ISM
   ```

2. **Verify router enrollment:**

   ```bash
   cast call $EVM_SYNTHETIC_WCTEST "routers(uint32)(bytes32)" $CARDANO_DOMAIN --rpc-url $EVM_RPC_URL
   # Should return non-zero (Cardano address)
   ```

3. **Ensure token approval for collateral routes:**
   ```bash
   WALLET=$(cast wallet address --private-key $EVM_SIGNER_KEY)
   cast call $EVM_FTEST "allowance(address,address)(uint256)" $WALLET $EVM_COLLATERAL_FTEST --rpc-url $EVM_RPC_URL
   ```

#### "Router not enrolled"

```bash
# Check current enrolled router
cast call $EVM_WARP_ROUTE "routers(uint32)(bytes32)" $CARDANO_DOMAIN --rpc-url $EVM_RPC_URL

# If returns 0x000...000, enroll the router
cast send $EVM_WARP_ROUTE \
  "enrollRemoteRouter(uint32,bytes32)" \
  $CARDANO_DOMAIN \
  $CARDANO_ROUTER \
  --rpc-url $EVM_RPC_URL \
  --private-key $EVM_SIGNER_KEY
```

#### "Insufficient balance" in Collateral

Pre-deposit more tokens to the collateral contract:

```bash
# First mint more tokens if needed
cast send $EVM_WADA "mint(address,uint256)" $WALLET "1000000000000000000000000" \
  --rpc-url $EVM_RPC_URL --private-key $EVM_SIGNER_KEY

# Then transfer to collateral
cast send $EVM_WADA "transfer(address,uint256)" $EVM_COLLATERAL_WADA "500000000000000000000000" \
  --rpc-url $EVM_RPC_URL --private-key $EVM_SIGNER_KEY
```

#### Message Not Delivered to Cardano

1. Check the [Hyperlane Explorer](https://explorer.hyperlane.xyz/) for message status
2. Verify Cardano relayer is running and configured for Sepolia (domain 11155111) as origin
3. Check relayer logs: `docker logs -f hyperlane-relayer 2>&1 | grep -E "(message|error)"`
4. Verify Cardano ISM has the correct Sepolia validators configured

---

## Appendix: Gas Payment (IGP) Configuration & Enforcement

This section covers pricing and enforcing interchain gas — configuring the IGP
oracles on both chains, requiring senders to pay for delivery, and paying for a
transfer. Full design rationale: **`cardano/docs/design/igp-gas-model.md`**.

### The model in one paragraph

The sender prepays, in the origin token, the cost the relayer spends delivering
on the destination. Two knobs: **`gasOverhead`** (a per-destination constant =
the recipient-*independent* base cost) and **`destinationGas` / `gasLimit`** (the
recipient-*specific* cost — warp routes set `destinationGas`, other senders pass
`gasLimit`). The relayer re-estimates the real cost at delivery and, under the
`onChainFeeQuoting` policy, refuses to deliver unless the payment covers it —
so a lowballed gas limit cannot slip through. Size overhead + destinationGas to
~1.5× the real cost for a positive relayer margin, and keep `gasFraction = 1/1`
(cover cost, never deliver at a loss).

### Cardano → Sepolia (Cardano IGP charges ADA for EVM delivery)

Destination is EVM, so gas is real. Set the oracle at `init igp` (Phase 3.5) or
recalibrate later:

```bash
# gas_price = 1 gwei, exchange_rate = 7171 ADA/ETH, gas_overhead = 211000
# (211000 ≈ 1.5 × the ~141k gas a Sepolia warp release costs)
BLOCKFROST_API_KEY=$BLOCKFROST_API_KEY \
$CLI --signing-key $CARDANO_SIGNING_KEY --network $NETWORK \
  igp set-oracle \
  --domain 11155111 \
  --gas-price 1000000000 \
  --exchange-rate 7171 \
  --gas-overhead 211000

# verify
$CLI --network $NETWORK igp show
```

### Sepolia → Cardano (Sepolia IGP charges ETH for Cardano delivery)

Cardano has no gas metering, so gas is denominated **1 lovelace = 1 gas unit**
(`gasPrice = 1`); a `gasLimit` reads directly as lovelace of Cardano cost. The
IGP + StorageGasOracle come from `DeploySepoliaIGP.s.sol` or the AggregationHook
(see the next appendix). Configure them with `cast` (authoritative — the forge
script's defaults are the older `gasPrice = 44` model):

```bash
export ETH_RPC_URL=$SEPOLIA_RPC_URL
ORACLE=<StorageGasOracle address>     # from the IGP/AggregationHook deploy
IGP=<InterchainGasPaymaster address>  # the IGP the warp aggregation hook pays

# 1) Oracle for Cardano (domain 2003): gasPrice=1, exchangeRate=1.395e18
#    (1.395e18 converts lovelace -> wei at ~7171 ADA/ETH; unchanged if the rate holds)
cast send $ORACLE "setRemoteGasData((uint32,uint128,uint128))" \
  "(2003,1395000000000000000,1)" --private-key $EVM_SIGNER_KEY

# 2) IGP overhead for domain 2003 = 1.5 × the ~1.375M-lovelace base delivery fee
cast send $IGP "setDestinationGasConfigs((uint32,(address,uint96))[])" \
  "[(2003,($ORACLE,2062550))]" --private-key $EVM_SIGNER_KEY

# 3) Per warp route: recipient-specific cost via destinationGas.
#    A route paired with a Cardano SYNTHETIC route mints a fresh token UTXO
#    (~1.2M lovelace minUTxO the relayer fronts) -> set 1.5 × that:
cast send $SEPOLIA_WARP_ROUTE "setDestinationGas(uint32,uint256)" 2003 1800000 \
  --private-key $EVM_SIGNER_KEY
#    Routes paired with a Cardano NATIVE or COLLATERAL route: leave destinationGas
#    at 0 — the released ADA / locked UTXO already funds the recipient minUTxO,
#    so the overhead alone covers them.

# verify the quote the sender will pay (wei)
cast call $SEPOLIA_WARP_ROUTE "quoteGasPayment(uint32)(uint256)" 2003
```

> **Which IGP?** The warp routes pay the IGP inside their aggregation hook, which
> may differ from a standalone `SEPOLIA_IGP`. The relayer must index the IGP that
> is actually paid. Find it with
> `cast call $ROUTE_HOOK "hooks(bytes)(address[])" 0x` (the non-MerkleTreeHook
> entry) and point the relayer's `interchainGasPaymaster` at it.

### Enforce payment in the relayer

Add a single catch-all policy to the relayer config (`gasPaymentEnforcement`).
It gates delivery on both directions, each estimating cost with its own
estimator (Blockfrost TX-evaluation for Cardano, `eth_estimateGas` for Sepolia):

```json
"gasPaymentEnforcement": [
  { "type": "onChainFeeQuoting", "gasFraction": "1/1" }
]
```

`1/1` requires the payment to cover the full estimate. Restart the relayer
(`docker compose up -d --force-recreate relayer`) after changing it.

### Paying for gas when you transfer

- **Cardano → Sepolia** (`warp transfer`): pass `--gas-limit` so the transfer
  bundles the IGP payment. With the Cardano overhead at 211000, `--gas-limit 0`
  already pays `0 + 211000` gas (≈1.5×). `mailbox dispatch` has **no** gas option
  — pay separately (below).
  ```bash
  $CLI --signing-key $CARDANO_SIGNING_KEY --network $NETWORK \
    warp transfer --warp-policy $WARP_POLICY --domain 11155111 \
    --recipient "0x000000000000000000000000$EVM_SIGNER_ADDRESS" \
    --amount 5000000 --gas-limit 0
  ```
- **Sepolia → Cardano** (`transferRemote`): send `--value` ≥ the route's
  `quoteGasPayment(2003)`; the route's aggregation hook forwards it to the IGP.
  Undershooting reverts with `StaticAggregationHook: insufficient value`, and the
  quote fluctuates, so pass a small margin.
- **Top up / pay for an already-dispatched message:**
  - Cardano-origin: `igp pay-for-gas --message-id <id> --destination 11155111 --gas-limit <n>`
  - Sepolia-origin: `cast send $IGP "payForGas(bytes32,uint32,uint256,address)" <id> 2003 <gasAmount> $EVM_SIGNER_ADDRESS --value <wei> --private-key $EVM_SIGNER_KEY`

### Test that enforcement works

1. **Underpay → rejected.** Dispatch with too little gas (e.g. a Cardano→Sepolia
   `mailbox dispatch` with no payment, or lower the overhead temporarily). The
   relayer logs the message as `Retry(GasPaymentRequirementNotMet)` and does
   **not** deliver.
2. **Pay enough → delivered.** A normal `warp transfer --gas-limit …` (or a
   `transferRemote` with sufficient `--value`) is delivered. Relayer logs show
   `paid gas_amount ≥ estimate → ReadyToSubmit`.
3. **Top up → delivered.** Take the rejected message from (1), top it up with
   `pay-for-gas` / `payForGas`, and confirm the relayer now delivers it.

---

## Appendix: EVM-Side Hook Configuration (AggregationHook)

When dispatching messages from an EVM chain (e.g. Sepolia) to Cardano,
the message **must** be inserted into the MerkleTreeHook. Without this,
validators cannot sign checkpoints covering the message and cross-chain delivery
will fail.

### Why This Matters

Hyperlane validators watch the MerkleTreeHook for new message insertions. They
sign checkpoints (merkle root + index) that the relayer uses as proof when
delivering messages on the destination chain. If a message is dispatched through
the Mailbox but never inserted into the MerkleTreeHook, validators will never
see it.

### When You Need an AggregationHook

| Scenario | Hook Needed? | Why |
| --- | --- | --- |
| Warp route `transferRemote()` | Automatic | Warp routes have their own hook configured at deploy time |
| `Mailbox.dispatch()` (3-arg) | Default hook only | Uses the Mailbox's default hook (usually MerkleTreeHook) — **cannot accept ETH for gas** |
| `Mailbox.dispatch()` (5-arg) with custom hook | **Yes — must include MerkleTreeHook** | If your custom hook is only an IGP, messages won't be in the merkle tree |

The 3-argument `dispatch(uint32,bytes32,bytes)` uses the Mailbox's default hook
(typically MerkleTreeHook), which works but **does not accept ETH value** for gas
payment. To pay for interchain gas, you need the 5-argument form with an
AggregationHook that combines MerkleTreeHook + IGP.

### Deploying an AggregationHook

Use the provided Forge script at `solidity/script/warp-e2e/DeployAggregationHook.s.sol`:

```bash
cd solidity

# Set environment variables
export EVM_SIGNER_KEY="0x..."                                    # Your deployer key
export EVM_MERKLE_TREE_HOOK="0x4917a9746A7B6E0A57159cCb7F5a6744247f2d0d"  # Sepolia example
export EVM_IGP="0xb9655C900Ef6104a776E533E93dC1D32BEe8cd93"              # Your IGP address

# Deploy
forge script script/warp-e2e/DeployAggregationHook.s.sol:DeployAggregationHook \
  --rpc-url $EVM_RPC_URL \
  --broadcast

# Save the output address
export EVM_AGGREGATION_HOOK="0x..."  # Copy from "AggregationHook deployed:" line
```

The AggregationHook is reusable — you only need to redeploy if the IGP or
MerkleTreeHook address changes.

### Dispatching with the AggregationHook

When sending messages directly through the Mailbox (not via warp routes), use
the 5-argument `dispatch()` with the AggregationHook:

```bash
# Quote gas payment from the IGP
GAS_QUOTE=$(cast call $EVM_IGP \
  "quoteGasPayment(uint32)(uint256)" 2003 \
  --rpc-url $EVM_RPC_URL)

# Dispatch with AggregationHook (MerkleTreeHook + IGP)
cast send $EVM_MAILBOX \
  "dispatch(uint32,bytes32,bytes,bytes,address)" \
  2003 \
  $CARDANO_RECIPIENT \
  $MESSAGE_BODY \
  "0x" \
  $EVM_AGGREGATION_HOOK \
  --value $GAS_QUOTE \
  --rpc-url $EVM_RPC_URL \
  --private-key $EVM_SIGNER_KEY
```

The `--value` pays the IGP for interchain gas, and the AggregationHook ensures
the message is also inserted into the MerkleTreeHook for validator signing.

### Common Mistake: "No Value Expected" Error

If you try to send ETH value with the 3-argument `dispatch()`:

```
MerkleTreeHook: no value expected
```

This happens because the default Mailbox hook is MerkleTreeHook alone, which
does not accept ETH. Switch to the 5-argument form with the AggregationHook as
shown above.

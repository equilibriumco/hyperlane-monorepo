# Warp Route Guide: Cross-Chain Token Bridging

This guide explains how to deploy and use warp routes to bridge tokens between Cardano and other Hyperlane-connected chains. Examples use Fuji (Avalanche testnet) as the remote chain.

## Overview

Warp routes enable cross-chain token transfers through Hyperlane's messaging protocol. Cardano supports three warp route types:

| Type           | Use Case                                     | Mechanism                                        |
| -------------- | -------------------------------------------- | ------------------------------------------------ |
| **Collateral** | Bridge Cardano native tokens to other chains | Lock tokens in vault on send, release on receive |
| **Synthetic**  | Receive tokens from other chains on Cardano  | Mint synthetic tokens on receive, burn on send   |
| **Native**     | Bridge ADA to other chains                   | Lock ADA in vault on send, release on receive    |

## Prerequisites

### 1. Cardano CLI Setup

```bash
cd cardano/cli
cargo build --release
```

### 2. Environment Configuration

```bash
# Blockfrost API key (Preview testnet)
export BLOCKFROST_API_KEY=your_api_key_here

# Signing key path
export CARDANO_SIGNING_KEY=./testnet-keys/payment.skey
```

### 3. Funded Wallet

Ensure your wallet has sufficient ADA:

- Minimum 50 ADA recommended for deployments
- Each warp route deployment requires ~4 UTXOs with 10+ ADA each

### 4. Mailbox Deployed

The Hyperlane mailbox must be deployed first. Verify with:

```bash
cat deployments/preview/deployment_info.json | jq '.mailbox.stateNftPolicy'
```

## Chain Information

### Cardano Preview Testnet

- **Domain ID:** 1000 (example, check actual deployment)
- **Network:** Preview

### Fuji (Avalanche Testnet)

- **Domain ID:** 43113
- **Mailbox:** `0x5b6CFf85442B851A8e6eaBd2A4E4507B5135B3B0`
- **RPC:** `https://api.avax-test.network/ext/bc/C/rpc`

---

## Collateral Warp Route

Use collateral routes to bridge **existing Cardano native tokens** to other chains.

### When to Use

- You have a Cardano native token (CNT) you want to make available on other chains
- The token's "home chain" is Cardano
- Other chains will receive synthetic/wrapped versions

### Deploy Collateral Route

```bash
# Deploy with your token's policy ID and asset name
hyperlane-cardano warp deploy \
  --token-type collateral \
  --token-policy <YOUR_TOKEN_POLICY_ID> \
  --token-asset <YOUR_TOKEN_ASSET_NAME> \
  --decimals 6 \
  --signing-key ./testnet-keys/payment.skey \
  --contracts-dir ./contracts
```

**Example with test token:**

```bash
hyperlane-cardano warp deploy \
  --token-type collateral \
  --token-policy 908d51752e4c76fe1404a92b1276b1c1093dae0c7f302c5442f0177e \
  --token-asset WARPTEST \
  --decimals 6 \
  --signing-key ./testnet-keys/payment.skey \
  --contracts-dir ./contracts
```

### Deployment Output

```
Warp Route Deployment Complete!

Vault:
  Script Hash: a3a296f04fc7387fef4c1740fa755f1b7ca42c57e42bf8c02532dae5
  NFT Policy: <vault_nft_policy>
  Address: addr_test1wz3699hsflrnsll0fst5p7n4tudhefpv2ljzh7xqy5ed4egpdvw7c

Warp Route:
  Script Hash: 3d076cd4c8b5e8f66ae70f38aeae1fcfe5764183a1803725191e0b3c
  NFT Policy: <warp_nft_policy>
  Address: addr_test1wq7swmx5ez673an2uu8n3t4wrl872ajpswscqde9ry0qk0qxrz9y6
```

### Enroll Fuji Router

After deploying a corresponding route on Fuji, enroll it:

```bash
hyperlane-cardano warp enroll-router \
  --warp-policy <WARP_NFT_POLICY> \
  --domain 43113 \
  --router 0x0000000000000000000000005b6cff85442b851a8e6eabd2a4e4507b5135b3b0 \
  --signing-key ./testnet-keys/payment.skey \
  --contracts-dir ./contracts
```

### Transfer Flow

**Cardano → Fuji (Outbound):**

1. User calls `warp transfer` with tokens
2. Tokens are locked in the vault
3. Message dispatched via mailbox
4. Fuji receives message and mints wrapped tokens

**Fuji → Cardano (Inbound):**

1. User burns wrapped tokens on Fuji
2. Message sent to Cardano mailbox
3. Cardano warp route releases tokens from vault
4. User receives original tokens

---

## Synthetic Warp Route

Use synthetic routes to **receive tokens from other chains** on Cardano.

### When to Use

- You want to bring tokens from another chain to Cardano
- The token's "home chain" is NOT Cardano (e.g., ETH, AVAX)
- Cardano will mint synthetic/wrapped versions

### Deploy Synthetic Route

```bash
hyperlane-cardano warp deploy \
  --token-type synthetic \
  --decimals 18 \
  --signing-key ./testnet-keys/payment.skey \
  --contracts-dir ./contracts
```

**Note:** Use the decimals of the original token on its home chain.

### Deployment Output

```
Synthetic Warp Route Deployment Complete!

Synthetic Token:
  Policy ID: 795cc3628197b0ed3b38a1c993fe4b1ec848bc6d5d2abab71781ac22
  Note: Tokens are minted when receiving transfers from other chains

Warp Route:
  Script Hash: 3d076cd4c8b5e8f66ae70f38aeae1fcfe5764183a1803725191e0b3c
  NFT Policy: <warp_nft_policy>
  Address: addr_test1wq7swmx5ez673an2uu8n3t4wrl872ajpswscqde9ry0qk0qxrz9y6
```

### Enroll Fuji Router

```bash
hyperlane-cardano warp enroll-router \
  --warp-policy <WARP_NFT_POLICY> \
  --domain 43113 \
  --router 0x0000000000000000000000005b6cff85442b851a8e6eabd2a4e4507b5135b3b0 \
  --signing-key ./testnet-keys/payment.skey \
  --contracts-dir ./contracts
```

### Transfer Flow

**Fuji → Cardano (Inbound):**

1. User locks/burns tokens on Fuji
2. Message sent to Cardano mailbox
3. Cardano warp route mints synthetic tokens
4. User receives synthetic tokens with the minting policy

**Cardano → Fuji (Outbound):**

1. User calls `warp transfer` with synthetic tokens
2. Synthetic tokens are burned
3. Message dispatched via mailbox
4. Fuji releases original tokens

---

## Native (ADA) Warp Route

Use native routes to bridge **ADA itself** to other chains.

### When to Use

- You want to bridge ADA to another chain
- Users on other chains will receive wrapped ADA

### Deploy Native Route

```bash
hyperlane-cardano warp deploy \
  --token-type native \
  --decimals 6 \
  --signing-key ./testnet-keys/payment.skey \
  --contracts-dir ./contracts
```

**Note:** ADA always has 6 decimals (1 ADA = 1,000,000 lovelace).

### Deployment Output

```
Native (ADA) Warp Route Deployment Complete!

Vault:
  Script Hash: a3a296f04fc7387fef4c1740fa755f1b7ca42c57e42bf8c02532dae5
  NFT Policy: <vault_nft_policy>
  Address: addr_test1wz3699hsflrnsll0fst5p7n4tudhefpv2ljzh7xqy5ed4egpdvw7c

Warp Route:
  Script Hash: 3d076cd4c8b5e8f66ae70f38aeae1fcfe5764183a1803725191e0b3c
  NFT Policy: <warp_nft_policy>
  Address: addr_test1wq7swmx5ez673an2uu8n3t4wrl872ajpswscqde9ry0qk0qxrz9y6
```

### Enroll Fuji Router

```bash
hyperlane-cardano warp enroll-router \
  --warp-policy <WARP_NFT_POLICY> \
  --domain 43113 \
  --router 0x0000000000000000000000005b6cff85442b851a8e6eabd2a4e4507b5135b3b0 \
  --signing-key ./testnet-keys/payment.skey \
  --contracts-dir ./contracts
```

### Transfer Flow

**Cardano → Fuji (Outbound):**

1. User calls `warp transfer` with ADA amount
2. ADA is locked in the vault
3. Message dispatched via mailbox
4. Fuji mints wrapped ADA (wADA)

**Fuji → Cardano (Inbound):**

1. User burns wADA on Fuji
2. Message sent to Cardano mailbox
3. Cardano warp route releases ADA from vault
4. User receives ADA

---

## Common Operations

### View Warp Route Configuration

```bash
hyperlane-cardano warp show --warp-policy <WARP_NFT_POLICY>
```

### List Enrolled Routers

```bash
hyperlane-cardano warp routers --warp-policy <WARP_NFT_POLICY>
```

**Example output:**

```
Enrolled Remote Routers

Remote Routers:
--------------------------------------------------------------------------------
  Domain 43113: 0x0000000000000000000000005b6cff85442b851a8e6eabd2a4e4507b5135b3b0
```

### Transfer Tokens (Outbound)

```bash
hyperlane-cardano warp transfer \
  --warp-policy <WARP_NFT_POLICY> \
  --domain 43113 \
  --recipient 0x<FUJI_RECIPIENT_ADDRESS> \
  --amount 1000000 \
  --signing-key ./testnet-keys/payment.skey \
  --contracts-dir ./contracts
```

**Note:** Amount is in the smallest unit (lovelace for ADA, token's smallest unit for others).

---

## Complete Example: Bridge Test Token to Fuji

### Step 1: Deploy Collateral Route on Cardano

```bash
# Deploy collateral warp route for WARPTEST token
hyperlane-cardano warp deploy \
  --token-type collateral \
  --token-policy 908d51752e4c76fe1404a92b1276b1c1093dae0c7f302c5442f0177e \
  --token-asset WARPTEST \
  --decimals 6 \
  --signing-key ./testnet-keys/payment.skey \
  --contracts-dir ./contracts

# Note the warp NFT policy from output
export CARDANO_WARP_POLICY=<warp_nft_policy_from_output>
```

### Step 2: Deploy Synthetic Route on Fuji

On Fuji, deploy a synthetic HypERC20 that will mint wrapped WARPTEST:

```bash
# Using Hyperlane CLI on Fuji
hyperlane warp deploy --config warp-config.yaml
```

### Step 3: Enroll Routes (Both Sides)

**On Cardano - Enroll Fuji router:**

```bash
hyperlane-cardano warp enroll-router \
  --warp-policy $CARDANO_WARP_POLICY \
  --domain 43113 \
  --router 0x<FUJI_WARP_ROUTE_ADDRESS_PADDED_TO_32_BYTES> \
  --signing-key ./testnet-keys/payment.skey \
  --contracts-dir ./contracts
```

**On Fuji - Enroll Cardano router:**

```bash
# Using Hyperlane CLI or direct contract call
# Enroll Cardano domain with Cardano warp route address
```

### Step 4: Transfer Tokens

**Send WARPTEST from Cardano to Fuji:**

```bash
hyperlane-cardano warp transfer \
  --warp-policy $CARDANO_WARP_POLICY \
  --domain 43113 \
  --recipient 0x000000000000000000000000<YOUR_FUJI_ADDRESS> \
  --amount 1000000 \
  --signing-key ./testnet-keys/payment.skey \
  --contracts-dir ./contracts
```

### Step 5: Verify

- Check Cardano: Tokens should be locked in vault
- Check Fuji: Wrapped tokens should be minted to recipient
- Use Hyperlane Explorer to track the message

---

## Troubleshooting

### "Warp route UTXO not found"

The warp route NFT policy ID is incorrect or the route hasn't been deployed yet.

```bash
# Verify the policy exists
hyperlane-cardano warp show --warp-policy <POLICY_ID>
```

### "Mailbox not deployed"

Deploy the Hyperlane mailbox first:

```bash
hyperlane-cardano init --signing-key ./testnet-keys/payment.skey
```

### "Need at least 4 UTXOs"

Your wallet doesn't have enough separate UTXOs. Split your ADA:

```bash
# Send ADA to yourself in multiple transactions to create UTXOs
```

### "No remote routers enrolled"

Enroll the remote chain's warp route before transferring:

```bash
hyperlane-cardano warp enroll-router --domain <DOMAIN> --router <ADDRESS> ...
```

---

## Architecture Reference

### Warp Route Datum Structure

```
WarpRouteDatum {
  config: WarpRouteConfig {
    token_type: Collateral | Synthetic | Native,
    decimals: Int,
    remote_routes: List<(Domain, RouterAddress)>
  },
  owner: VerificationKeyHash,
  total_bridged: Int
}
```

### Token Type Constructors

| Type       | Constructor | Fields                                     |
| ---------- | ----------- | ------------------------------------------ |
| Collateral | 0           | `policy_id`, `asset_name`, `vault_locator` |
| Synthetic  | 1           | `minting_policy`                           |
| Native     | 2           | `vault_locator`                            |

### Related Documentation

- [DESIGN.md](./DESIGN.md) - Architecture overview
- [DEPLOYMENT_GUIDE.md](./DEPLOYMENT_GUIDE.md) - Full deployment instructions
- [INTEGRATION_STATUS.md](./INTEGRATION_STATUS.md) - Current integration status

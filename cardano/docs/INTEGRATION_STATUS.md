# Hyperlane-Cardano Integration Status

This document describes the current state of the Hyperlane-Cardano integration, what has been implemented, what is missing, and the recommended next steps.

## Executive Summary

The Hyperlane-Cardano integration is **complete for bidirectional messaging and token bridging**. The on-chain contracts (Aiken) and off-chain infrastructure (Rust) are mature and have been deployed and tested end-to-end on Preview testnet with Avalanche Fuji.

| Component                                  | Status         | Notes                                        |
| ------------------------------------------ | -------------- | -------------------------------------------- |
| Incoming Messages (Fuji -> Cardano)        | Tested         | End-to-end working                           |
| Outgoing Messages (Cardano -> Fuji)        | Tested         | Validator + relayer delivering to Fuji       |
| Multisig ISM                               | Complete       | ECDSA secp256k1 signatures verified          |
| Validator Agent                            | Tested         | Signing checkpoints, storing in S3           |
| Warp Routes (Native, Collateral, Synth)    | Tested         | All 6 directions verified end-to-end         |
| Interchain Gas Paymaster                   | Untested       | Contract implemented, indexer stub           |
| Per-recipient ISM                          | Implemented    | Relayer reads ISM from WarpRouteDatum        |
| NFT Policy Addressing                      | Complete       | O(1) lookups, no registry contract needed    |

---

## 1. Incoming Message Flow (Other Chains -> Cardano)

**Status: Tested and Working**

This flow is mature and has been tested end-to-end with messages from Fuji (Avalanche testnet) to Cardano Preview.

### What's Implemented

#### On-Chain (Aiken)

- **Mailbox** (`contracts/validators/mailbox.ak`): Full `Process` action implementation

  - Message destination validation
  - Message ID verification (keccak256)
  - Replay protection via processed message markers
  - ISM verification enforcement
  - Recipient invocation validation
  - Continuation UTXO management

- **Multisig ISM** (`contracts/validators/multisig_ism.ak`): Complete signature verification

  - ECDSA secp256k1 signature verification (CIP-49)
  - Ethereum address derivation from public keys
  - Per-origin domain validator sets and thresholds
  - Checkpoint signing format matching Hyperlane spec

- **Processed Message NFT** (`contracts/validators/processed_message_nft.ak`): Replay protection

  - Mints unique NFT per processed message
  - Enables O(1) delivery status lookups

- **Deferred Recipient** (`contracts/validators/deferred_recipient.ak`): Two-phase processing
  - Phase 1: Relayer stores message with NFT proof
  - Phase 2: Separate processing burns NFT and handles message

#### Off-Chain (Rust)

- **CardanoMailbox** (`rust/main/chains/hyperlane-cardano/src/mailbox.rs`)

  - `process()` implementation for message delivery
  - `delivered()` for checking message status
  - Complete transaction building with UTXOs

- **CardanoMailboxIndexer** (`rust/main/chains/hyperlane-cardano/src/mailbox_indexer.rs`)

  - Full `Indexer<HyperlaneMessage>` implementation
  - Parses Dispatch redeemers from Blockfrost
  - `SequenceAwareIndexer` for ordered processing

- **CardanoMultisigIsm** (`rust/main/chains/hyperlane-cardano/src/multisig_ism.rs`)

  - `MultisigIsm` trait implementation
  - Fetches validator sets and thresholds per domain

- **Recipient Resolver** (`rust/main/chains/hyperlane-cardano/src/recipient_resolver.rs`)

  - O(1) NFT-based recipient lookups (no registry iteration)
  - State UTXO discovery via NFT policy query
  - Reference script UTXO resolution

- **Transaction Builder** (`rust/main/chains/hyperlane-cardano/src/tx_builder.rs`)
  - Complex UTXO selection
  - Reference script support
  - Fee calculation
  - Witness set construction

### Deployment

- Deployed to Cardano Preview testnet (domain 2003)
- Connected to Fuji (domain 43113) for bidirectional testing
- Relayer configuration in `cardano/e2e-docker/config/relayer-cardano-fuji.json`

---

## 2. Outgoing Message Flow (Cardano -> Other Chains)

**Status: Tested and Working**

### What's Implemented

#### On-Chain (Aiken)

- **Mailbox Dispatch** (`contracts/validators/mailbox.ak`): Complete implementation

  - Message construction with version, nonce, origin, sender
  - Merkle tree update with message hash (keccak256)
  - Nonce increment and continuation validation

- **Merkle Tree** (`contracts/lib/merkle.ak`): Full implementation
  - Incremental merkle tree for message commitments
  - `insert()` and `root()` operations
  - Maximum depth of 32 levels

#### Off-Chain (Rust)

- **Mailbox Indexer** (`rust/main/chains/hyperlane-cardano/src/mailbox_indexer.rs`)

  - `parse_dispatch_redeemer()` extracts message data
  - Indexes dispatched messages from Cardano
  - Extracts nonce from transaction outputs

- **Validator Agent**: Fully integrated
  - Signs checkpoints for Cardano-originated messages
  - Stores signed checkpoints in AWS S3
  - Relayer fetches checkpoints and delivers to destination

### Tested Flows

- Cardano -> Fuji: Messages dispatched from Cardano mailbox are indexed, signed by the validator, and delivered to Fuji by the relayer.

---

## 3. Validator Agent for Cardano

**Status: Integrated and Working**

### What's Implemented

- `CardanoValidatorAnnounce` implementation (`rust/main/chains/hyperlane-cardano/src/validator_announce.rs`)
- Validator announcement on-chain contract (`contracts/validators/validator_announce.ak`)
- Validator agent signs checkpoints for Cardano-originated messages
- Checkpoints stored in AWS S3
- CLI `validator announce` command for on-chain announcement

### Known Limitations

- **Reorg Reporter**: Not yet implemented for Cardano (`rust/main/agents/validator/src/reorg_reporter.rs:205`). Low priority for testnet but needed for production.

---

## 4. Interchain Gas Paymaster (IGP)

**Status: Contract Implemented, Off-Chain Partial**

### What's Implemented

#### On-Chain (Aiken)

- **IGP Contract** (`contracts/validators/igp.ak`): Complete
  - `PayForGas`: Users pay for message delivery
  - `Claim`: Beneficiary claims accumulated fees
  - `SetGasOracle`: Owner configures gas oracles per destination
  - Gas calculation formula implemented

#### Off-Chain (Rust)

- **InterchainGas struct** (`rust/main/chains/hyperlane-cardano/src/interchain_gas.rs`)
  - Implements `InterchainGasPaymaster` trait
  - `fetch_logs_in_range()` with graceful degradation
  - Parsing for gas payments

### What's Missing

1. **RPC Endpoint Integration**

   - `get_gas_payments_by_block_range()` endpoint needed in Cardano RPC server
   - Currently returns empty results with debug logging

2. **CLI Commands**

   - `igp pay-for-gas` command
   - `igp claim` command
   - `igp set-oracle` command

3. **Testing**
   - No end-to-end testing of gas payment flow

---

## 5. Warp Routes (Token Bridge)

**Status: Tested End-to-End**

All three warp route types have been deployed and tested bidirectionally between Cardano Preview and Avalanche Fuji.

### What's Implemented

#### On-Chain (Aiken)

- **Warp Route** (`contracts/validators/warp_route.ak`): Full implementation

  - `TransferRemote`: Send tokens to another chain
  - `ReceiveTransfer`: Receive tokens from another chain
  - `EnrollRemoteRoute`: Register remote warp routes

- **Token Types Supported**:

  - **Native**: Lock ADA in vault, mint wADA on remote
  - **Collateral**: Lock Cardano native tokens in vault, mint wrapped tokens on remote
  - **Synthetic**: Mint/burn synthetic tokens on Cardano for tokens originating elsewhere

- **Vault** (`contracts/validators/vault.ak`): Collateral management

- **Synthetic Token** (`contracts/validators/synthetic_token.ak`): Minting policy

#### Off-Chain (Rust)

- Transaction builder handles all warp route operations (lock, release, mint, burn)
- Redemption pattern: relayer creates redemption UTXO, recipient claims with own ADA
- Decimal conversion between Cardano (0/6 decimals) and EVM (6/18 decimals)

### Test Results

| Direction                           | Status | Notes                              |
| ----------------------------------- | ------ | ---------------------------------- |
| Native ADA Cardano -> Fuji wADA    | Pass   | ADA locked, wADA minted on Fuji   |
| Fuji wADA -> Cardano Native ADA    | Pass   | wADA burned, ADA released + claim |
| Collateral TEST Cardano -> Fuji    | Pass   | TEST locked, wCTEST minted        |
| Fuji wCTEST -> Cardano Collateral  | Pass   | wCTEST burned, TEST released      |
| Fuji FTEST -> Cardano Synthetic    | Pass   | FTEST locked, synthetic minted    |
| Cardano Synthetic -> Fuji FTEST    | Pass   | Synthetic burned, FTEST released  |

---

## 6. Per-Recipient Custom ISM

**Status: Implemented (relayer-side)**

### What's Implemented

- The `ism: Option<ScriptHash>` field in `WarpRouteDatum` allows each warp route to specify a custom ISM
- The relayer's `recipient_ism()` reads the ISM from the datum and uses it when building process transactions
- Falls back to the default ISM when no custom ISM is set

### Limitations

- **On-chain**: The mailbox's `get_recipient_ism()` still returns the default ISM. Custom ISM selection happens off-chain in the relayer, not enforced on-chain by the mailbox contract itself

---

## 7. NFT Policy Addressing

**Status: Complete**

The previous registry-based architecture has been replaced with direct NFT policy-based addressing.

### How It Works

- Recipients are identified by their state NFT policy ID
- Hyperlane address format: `0x01000000{nft_policy_id}` for NFT-based addresses
- Core contracts use: `0x02000000{script_hash}` for script-based addresses
- The relayer's `RecipientResolver` performs O(1) lookups via Blockfrost NFT queries
- No registry contract or registration transactions needed

### Benefits

- O(1) recipient discovery (query NFT by policy)
- No registry contract to deploy or maintain
- No registration transactions needed
- Simpler deployment flow

---

## 8. Current Deployment Status

### Testnet (Preview)

Contract addresses and policy IDs change with each deployment. For current values, check:

```bash
cat cardano/deployments/preview/deployment_info.json
```

### Connected Chains

- Cardano Preview (domain 2003)
- Fuji Avalanche testnet (domain 43113)

---

## 9. Recommended Next Steps

### High Priority (Blocking for Production)

1. **Security Audit**
   - Audit Aiken contracts
   - Review signature verification logic
   - Validate merkle tree implementation

2. **IGP Integration**
   - Add RPC endpoint for gas payments
   - Test gas payment flow
   - Integrate with relayer economics

### Medium Priority (Production Hardening)

3. **Reorg Reporter**
   - Implement Cardano-specific reorg detection
   - Currently marked as not implemented for Cardano

4. **Monitoring & Observability**
   - Metrics for Cardano message processing
   - Alerting for failed deliveries

5. **On-chain Custom ISM Enforcement**
   - Move per-recipient ISM selection from relayer to mailbox contract

### Low Priority (Optimization)

6. **UTXO Contention Mitigation**
   - Convert Mailbox/ISM to minting policies for parallel processing
   - See `FUTURE_OPTIMIZATIONS.md`

---

## 10. Architecture Overview

```
+----------------------------------------------------------------------+
|                         CARDANO CHAIN                                 |
+----------------------------------------------------------------------+
|                                                                       |
|  +------------+   +------------+   +------------+   +------------+   |
|  |  Mailbox   |   |Multisig ISM|   |    IGP     |   |  Warp      |   |
|  |  Complete  |   |  Complete  |   |  Partial   |   |  Route     |   |
|  +------------+   +------------+   +------------+   |  Tested    |   |
|                                                      +------------+   |
|  Recipients resolved via NFT policy queries (O(1) lookups)           |
|                                                                       |
+----------------------------------------------------------------------+
                               |
                               | Relayer (Rust)
                               | Bidirectional: Working
                               v
+----------------------------------------------------------------------+
|                         OTHER CHAINS                                  |
|                (Fuji, Ethereum, Solana, Cosmos, etc.)                |
+----------------------------------------------------------------------+
```

---

## 11. File References

### On-Chain Contracts (Validators)

| File                                       | Lines | Description                                  |
| ------------------------------------------ | ----- | -------------------------------------------- |
| `validators/mailbox.ak`                    | 366   | Core mailbox - dispatch and process messages |
| `validators/multisig_ism.ak`               | 264   | Multisig signature verification              |
| `validators/multisig_ism_test.ak`          | 520   | ISM unit tests                               |
| `validators/warp_route.ak`                 | 519   | Token bridge implementation                  |
| `validators/igp.ak`                        | 275   | Interchain gas paymaster                     |
| `validators/deferred_recipient.ak`         | 336   | Two-phase message processing                 |
| `validators/example_deferred_recipient.ak` | 468   | Example deferred recipient                   |
| `validators/example_generic_recipient.ak`  | 203   | Example basic recipient                      |
| `validators/vault.ak`                      | 286   | Token vault for collateral                   |
| `validators/validator_announce.ak`         | 181   | Validator announcements                      |
| `validators/stored_message_nft.ak`         | 109   | NFT for stored messages                      |
| `validators/processed_message_nft.ak`      | 58    | NFT for processed messages                   |
| `validators/state_nft.ak`                  | 39    | One-shot state NFT policy                    |
| `validators/synthetic_token.ak`            | 32    | Synthetic token minting                      |

### On-Chain Libraries

| File                 | Lines | Description            |
| -------------------- | ----- | ---------------------- |
| `lib/types.ak`       | 496   | Core type definitions  |
| `lib/types_test.ak`  | 221   | Type encoding tests    |
| `lib/merkle.ak`      | 164   | Merkle tree operations |
| `lib/merkle_test.ak` | 100   | Merkle tree tests      |
| `lib/utils.ak`       | 141   | Utility functions      |
| `lib/utils_test.ak`  | 186   | Utility tests          |

**Total: ~5,850 lines of Aiken code**

### Off-Chain (Rust)

| File                         | Size  | Description                  |
| ---------------------------- | ----- | ---------------------------- |
| `src/tx_builder.rs`          | 159KB | Transaction construction     |
| `src/recipient_resolver.rs`  | ~20KB | NFT-based recipient resolver |
| `src/multisig_ism.rs`        | 20KB  | ISM verification             |
| `src/validator_announce.rs`  | 19KB  | Validator announcements      |
| `src/types.rs`               | 19KB  | Type definitions             |
| `src/mailbox.rs`             | 18KB  | Mailbox implementation       |
| `src/mailbox_indexer.rs`     | 16KB  | Message indexing             |
| `src/blockfrost_provider.rs` | 24KB  | Blockfrost RPC client        |
| `src/merkle_tree_hook.rs`    | 5.7KB | Merkle tree hook (stub)      |
| `src/interchain_gas.rs`      | ~5KB  | IGP indexer                  |
| `src/provider.rs`            | 4.5KB | HyperlaneProvider impl       |

### CLI

- `cardano/cli/src/commands/` - CLI command implementations
  - `deploy.rs` - Contract deployment
  - `init.rs` - Contract initialization
  - `mailbox.rs` - Mailbox operations
  - `ism.rs` - ISM validator management
  - `warp.rs` - Warp route commands
  - `query.rs` - State queries
  - `deferred.rs` - Deferred message processing

### Configuration

- `cardano/e2e-docker/config/relayer-cardano-fuji.json` - Relayer configuration for testnet

---

## 12. Known Issues & TODOs

| Location                | Issue                                                                                              |
| ----------------------- | -------------------------------------------------------------------------------------------------- |
| `mailbox.ak:296`        | On-chain `get_recipient_ism()` still returns default ISM (custom ISM handled off-chain by relayer) |
| `warp_route.ak:484`     | `get_minted_amount()` returns placeholder value                                                    |
| `reorg_reporter.rs:205` | Cardano reorg reporting not implemented                                                            |
| `merkle_tree_hook.rs`   | Mostly stubs, needs full implementation for validators                                             |

---

_Last Updated: February 2026_

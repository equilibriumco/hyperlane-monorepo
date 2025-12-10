# Hyperlane-Cardano Integration Status

This document describes the current state of the Hyperlane-Cardano integration, what has been implemented, what is missing, and the recommended next steps.

## Executive Summary

The Hyperlane-Cardano integration is **substantially complete for incoming messages** (other chains → Cardano). The on-chain contracts (Aiken) and off-chain infrastructure (Rust) are mature and have been deployed to Preview testnet. However, **outgoing message flow** (Cardano → other chains) and some auxiliary features require additional work.

| Component | Status | Notes |
|-----------|--------|-------|
| Incoming Messages (Fuji → Cardano) | ✅ Tested | End-to-end working |
| Outgoing Messages (Cardano → other chains) | ⚠️ Partial | Contracts ready, agent integration needed |
| Multisig ISM | ✅ Complete | ECDSA secp256k1 signatures verified |
| Validator Agent | ⚠️ Partial | Not integrated with Cardano |
| Warp Routes | ⚠️ Untested | Contracts implemented, not deployed/tested |
| Interchain Gas Paymaster | ⚠️ Untested | Contract implemented, indexer stub |
| Per-recipient ISM | ⏳ TODO | Uses default ISM only |

---

## 1. Incoming Message Flow (Other Chains → Cardano)

**Status: ✅ Tested and Working**

This is the most mature part of the integration and has been tested end-to-end with messages from Fuji (Avalanche testnet) to Cardano Preview.

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

- **Recipient Registry** (`contracts/validators/registry.ak`): Full recipient management
  - Script registration with state NFT locators
  - Custom ISM overrides
  - Additional input requirements
  - Multiple recipient types (Generic, TokenReceiver, Deferred)

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

- **Registry Integration** (`rust/main/chains/hyperlane-cardano/src/registry.rs`)
  - Recipient lookup by script hash
  - State NFT discovery
  - Additional input resolution

- **Transaction Builder** (`rust/main/chains/hyperlane-cardano/src/tx_builder.rs`)
  - Complex UTXO selection
  - Reference script support
  - Fee calculation
  - Witness set construction

### Deployment
- Deployed to Cardano Preview testnet (domain 2003)
- Connected to Fuji (domain 43113) for bidirectional testing
- Relayer configuration in `cardano/config/relayer-config.json`

---

## 2. Outgoing Message Flow (Cardano → Other Chains)

**Status: ⚠️ Contracts Implemented, Agent Integration Needed**

### What's Implemented

#### On-Chain (Aiken)
- **Mailbox Dispatch** (`contracts/validators/mailbox.ak:72-114`): Complete implementation
  ```aiken
  Dispatch { destination, recipient, body } ->
    validate_dispatch(mailbox_datum, destination, recipient, body, tx, own_ref)
  ```
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

- **Dispatch Redeemer Support** (`rust/main/chains/hyperlane-cardano/src/types.rs:120`)
  ```rust
  pub enum MailboxRedeemer {
      Dispatch { destination: u32, recipient: [u8; 32], body: Vec<u8> },
      // ...
  }
  ```

### What's Missing

1. **Validator Agent for Cardano**
   - The Hyperlane validator agent signs checkpoints for dispatched messages
   - Current code explicitly notes: "Cardano validator reorg reporting not yet implemented" (`rust/main/agents/validator/src/reorg_reporter.rs:205`)
   - No Cardano-specific checkpoint signing flow

2. **CLI Commands for Dispatch**
   - Need `dispatch` command in CLI to send test messages
   - Currently only `process` flow is exercised

3. **End-to-End Testing**
   - No verified test of Cardano → Fuji (or other chain) message delivery
   - Merkle tree indexing on the Cardano side needs validation

### Architecture Gap

```
Current Flow (Working):
Fuji Mailbox → Dispatch → Hyperlane Validators → Sign Checkpoint →
Relayer → Cardano Mailbox.process() ✅

Missing Flow:
Cardano Mailbox → Dispatch → ??? Validator Agent → Sign Checkpoint →
Relayer → Fuji Mailbox.process() ❌
```

The Hyperlane validator agent needs to:
1. Index dispatched messages from Cardano mailbox
2. Build merkle tree from dispatched message hashes
3. Sign checkpoints (merkle root + index + message ID)
4. Store signed checkpoints for relayer retrieval

---

## 3. Validator Agent for Cardano

**Status: ⚠️ Not Integrated**

### What Exists
- `CardanoValidatorAnnounce` implementation (`rust/main/chains/hyperlane-cardano/src/validator_announce.rs`)
- Validator announcement on-chain contract (`contracts/validators/validator_announce.ak`)

### What's Missing
1. **Validator agent Cardano support**
   - The main validator agent (`rust/main/agents/validator/`) doesn't have Cardano-specific handlers
   - Checkpoint signing for Cardano-originated messages not implemented

2. **Merkle Tree Hook Indexer**
   - `CardanoMerkleTreeHook` exists but is mostly stubs
   - Needs to track merkle tree state from dispatched messages

3. **Reorg Reporter**
   - Explicitly marked as not implemented for Cardano
   - Low priority for testnet but needed for production

---

## 4. Interchain Gas Paymaster (IGP)

**Status: ⚠️ Contract Implemented, Off-Chain Partial**

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

**Status: ⚠️ Contracts Implemented, Not Tested**

### What's Implemented

#### On-Chain (Aiken)
- **Warp Route** (`contracts/validators/warp_route.ak`): Full implementation
  - `TransferRemote`: Send tokens to another chain
  - `ReceiveTransfer`: Receive tokens from another chain
  - `EnrollRemoteRoute`: Register remote warp routes

- **Token Types Supported**:
  - **Collateral**: Lock native Cardano tokens in vault
  - **Synthetic**: Mint/burn synthetic tokens
  - **Native**: Lock ADA in vault

- **Vault** (`contracts/validators/vault.ak`): Collateral management

- **Synthetic Token** (`contracts/validators/synthetic_token.ak`): Minting policy

### What's Missing
1. **Deployment and Configuration**
   - No deployed warp route instances
   - No enrolled remote routes

2. **CLI Commands**
   - Commands exist in `cardano/cli/src/commands/warp.rs` but untested

3. **End-to-End Testing**
   - Token transfer Cardano → Other chain
   - Token receive Other chain → Cardano

4. **Minor Code TODO**
   - `get_minted_amount()` in `warp_route.ak:484-488` returns placeholder

---

## 6. Per-Recipient Custom ISM

**Status: ⏳ TODO**

### What Exists
- Registry supports `custom_ism` field per recipient
- ISM hash stored in recipient registration

### What's Missing
- **On-chain**: `get_recipient_ism()` in `mailbox.ak:290-298` always returns default ISM
  ```aiken
  fn get_recipient_ism(
    _recipient: HyperlaneAddress,
    default_ism: ScriptHash,
    _tx: Transaction,
  ) -> ScriptHash {
    // For now, always use default ISM
    // TODO: Check recipient's custom ISM from registry
    default_ism
  }
  ```
- **Off-chain**: Need to pass custom ISM to transaction builder

---

## 7. Current Deployment Status

### Testnet (Preview)

Contract addresses and policy IDs change with each deployment. For current values, check:

```bash
cat cardano/deployments/preview/deployment_info.json
```

### Connected Chains
- Cardano Preview (domain 2003)
- Fuji Avalanche testnet (domain 43113)

---

## 8. Recommended Next Steps

### High Priority (Blocking for Production)

1. **Validator Agent Integration**
   - Implement Cardano chain support in validator agent
   - Enable checkpoint signing for Cardano-originated messages
   - Test full outgoing message flow

2. **Outgoing Message E2E Test**
   - Add CLI command for dispatching test messages
   - Verify message appears on destination chain
   - Test full Cardano → Fuji → back flow

3. **Security Audit**
   - Audit Aiken contracts
   - Review signature verification logic
   - Validate merkle tree implementation

### Medium Priority (Production Hardening)

4. **IGP Integration**
   - Add RPC endpoint for gas payments
   - Test gas payment flow
   - Integrate with relayer economics

5. **Warp Route Deployment**
   - Deploy test warp route on Preview
   - Enroll remote routes
   - Test token transfers both directions

6. **Per-Recipient ISM**
   - Implement registry lookup in mailbox
   - Support custom ISM in off-chain code

### Low Priority (Optimization)

7. **Performance Optimization**
   - NFT-based recipient lookups (O(1) vs O(n))
   - Reference script optimization

8. **Reorg Reporter**
   - Implement Cardano-specific reorg detection

9. **Monitoring & Observability**
   - Metrics for Cardano message processing
   - Alerting for failed deliveries

---

## 9. Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│                         CARDANO CHAIN                                │
├─────────────────────────────────────────────────────────────────────┤
│                                                                      │
│  ┌────────────┐   ┌────────────┐   ┌────────────┐   ┌────────────┐ │
│  │  Mailbox   │   │Multisig ISM│   │    IGP     │   │  Warp      │ │
│  │  ✅ Ready  │   │  ✅ Ready  │   │ ⚠️ Partial │   │  Route     │ │
│  └─────┬──────┘   └─────┬──────┘   └────────────┘   │ ⚠️ Untested│ │
│        │                │                            └────────────┘ │
│        │   Reference    │                                           │
│        │   Inputs       │                                           │
│        ▼                ▼                                           │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │                   Recipient Registry ✅                       │  │
│  │  ┌───────────────────────────────────────────────────────┐   │  │
│  │  │ ScriptHash → { stateNft, additionalInputs, ismOverride }│   │  │
│  │  └───────────────────────────────────────────────────────┘   │  │
│  └──────────────────────────────────────────────────────────────┘  │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
                               │
                               │ Relayer (Rust)
                               │ ✅ Incoming Working
                               │ ⚠️ Outgoing Needs Validator
                               ▼
┌─────────────────────────────────────────────────────────────────────┐
│                         OTHER CHAINS                                 │
│                (Fuji, Ethereum, Solana, Cosmos, etc.)               │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 10. File References

### On-Chain Contracts (Validators)
| File | Lines | Description |
|------|-------|-------------|
| `validators/mailbox.ak` | 366 | Core mailbox - dispatch and process messages |
| `validators/multisig_ism.ak` | 264 | Multisig signature verification |
| `validators/multisig_ism_test.ak` | 520 | ISM unit tests |
| `validators/registry.ak` | 365 | Recipient registry management |
| `validators/registry_test.ak` | 523 | Registry unit tests |
| `validators/warp_route.ak` | 519 | Token bridge implementation |
| `validators/igp.ak` | 275 | Interchain gas paymaster |
| `validators/deferred_recipient.ak` | 336 | Two-phase message processing |
| `validators/example_deferred_recipient.ak` | 468 | Example deferred recipient |
| `validators/example_generic_recipient.ak` | 203 | Example basic recipient |
| `validators/vault.ak` | 286 | Token vault for collateral |
| `validators/validator_announce.ak` | 181 | Validator announcements |
| `validators/stored_message_nft.ak` | 109 | NFT for stored messages |
| `validators/processed_message_nft.ak` | 58 | NFT for processed messages |
| `validators/state_nft.ak` | 39 | One-shot state NFT policy |
| `validators/synthetic_token.ak` | 32 | Synthetic token minting |

### On-Chain Libraries
| File | Lines | Description |
|------|-------|-------------|
| `lib/types.ak` | 496 | Core type definitions |
| `lib/types_test.ak` | 221 | Type encoding tests |
| `lib/merkle.ak` | 164 | Merkle tree operations |
| `lib/merkle_test.ak` | 100 | Merkle tree tests |
| `lib/utils.ak` | 141 | Utility functions |
| `lib/utils_test.ak` | 186 | Utility tests |

**Total: ~5,850 lines of Aiken code**

### Off-Chain (Rust)
| File | Size | Description |
|------|------|-------------|
| `src/tx_builder.rs` | 159KB | Transaction construction |
| `src/registry.rs` | 33KB | Registry client |
| `src/multisig_ism.rs` | 20KB | ISM verification |
| `src/validator_announce.rs` | 19KB | Validator announcements |
| `src/types.rs` | 19KB | Type definitions |
| `src/mailbox.rs` | 18KB | Mailbox implementation |
| `src/mailbox_indexer.rs` | 16KB | Message indexing |
| `src/blockfrost_provider.rs` | 24KB | Blockfrost RPC client |
| `src/merkle_tree_hook.rs` | 5.7KB | Merkle tree hook (stub) |
| `src/interchain_gas.rs` | ~5KB | IGP indexer |
| `src/provider.rs` | 4.5KB | HyperlaneProvider impl |

### CLI
- `cardano/cli/src/commands/` - CLI command implementations
  - `deploy.rs` - Contract deployment
  - `init.rs` - Contract initialization
  - `mailbox.rs` - Mailbox operations
  - `ism.rs` - ISM validator management
  - `registry.rs` - Registry operations
  - `warp.rs` - Warp route commands
  - `query.rs` - State queries
  - `deferred.rs` - Deferred message processing

### Scripts
- `cardano/scripts/send-message-to-recipient.sh` - Send test message to recipient

### Configuration
- `cardano/config/relayer-config.json` - Relayer configuration for testnet

---

## 11. Known Issues & TODOs

| Location | Issue |
|----------|-------|
| `mailbox.ak:296` | Per-recipient ISM not implemented (uses default) |
| `warp_route.ak:484` | `get_minted_amount()` returns placeholder value |
| `reorg_reporter.rs:205` | Cardano reorg reporting not implemented |
| `merkle_tree_hook.rs` | Mostly stubs, needs full implementation for validators |
| `blockfrost_provider.rs:51` | TODO: Migrate to NFT-based tracking for O(1) lookups |

---

*Last Updated: December 2024*
*Based on commit: dad07419f (fix(cardano): recipient not validating message)*

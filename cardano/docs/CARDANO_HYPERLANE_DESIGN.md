# Hyperlane Cardano Integration: Design Document

This document provides a comprehensive overview of the Hyperlane cross-chain messaging protocol integration with Cardano, including architecture, message flow, security guarantees, implementation status, and deviations from the EVM implementation.

## Table of Contents

1. [Overview](#overview)
2. [Architecture](#architecture)
3. [Message Flow Diagrams](#message-flow-diagrams)
4. [Security Model](#security-model)
5. [Contract Implementations](#contract-implementations)
6. [Implementation Status](#implementation-status)
7. [Deviations from EVM](#deviations-from-evm)
8. [Potential Security Vulnerabilities](#potential-security-vulnerabilities)
9. [Scalability Limitations and Improvements](#scalability-limitations-and-improvements)
10. [Future Work](#future-work)

---

## Overview

Hyperlane is an interchain messaging protocol that enables arbitrary message passing between blockchains. The Cardano integration enables bidirectional messaging between Cardano and other Hyperlane-supported chains (Ethereum, Solana, Cosmos, etc.).

### Key Design Constraints

1. **eUTXO Model**: Cardano's extended UTXO model requires fundamentally different patterns than account-based chains
2. **Relayer-Driven**: The Hyperlane relayer constructs and submits all Cardano transactions
3. **NFT-Based UTXO Discovery**: State UTXOs are identified by unique NFT markers
4. **Reference Scripts**: Uses Cardano's reference script feature to minimize transaction costs
5. **Aiken Smart Contracts**: All on-chain validators are written in Aiken (Plutus V3)

### Domain Identifiers

| Network | Domain ID |
|---------|-----------|
| Cardano Mainnet | 2001 (TBD) |
| Cardano Preview | 2003 |
| Cardano Preprod | 2002 |

---

## Architecture

### High-Level System Architecture

```
+==================================================================================+
|                              HYPERLANE ECOSYSTEM                                  |
+==================================================================================+
|                                                                                  |
|  +------------------+    +-----------------+    +------------------+             |
|  |  ORIGIN CHAIN    |    |    RELAYER      |    | DESTINATION CHAIN|             |
|  |   (e.g., EVM)    |    |    NETWORK      |    |   (Cardano)      |             |
|  +------------------+    +-----------------+    +------------------+             |
|  |                  |    |                 |    |                  |             |
|  | [Application]    |    | [Indexer]       |    | [Mailbox]        |             |
|  |      |           |    |     |           |    |      |           |             |
|  |      v           |    |     |           |    |      v           |             |
|  | [Mailbox]        |    |     v           |    | [ISM]            |             |
|  |      |           |    | [Validator      |    |      |           |             |
|  |      v           |    |  Aggregator]    |    |      v           |             |
|  | [MerkleTreeHook] |    |     |           |    | [Recipient]      |             |
|  |      |           |    |     v           |    |                  |             |
|  |      v           |    | [Tx Builder]    |    |                  |             |
|  | [Validators]     |    |     |           |    |                  |             |
|  |                  |    |     v           |    |                  |             |
|  +------------------+    | [Submitter]     |    +------------------+             |
|                          |                 |                                      |
|                          +-----------------+                                      |
|                                                                                  |
+==================================================================================+
```

### Cardano On-Chain Architecture

```
+===========================================================================+
|                          CARDANO CHAIN                                     |
+===========================================================================+
|                                                                           |
|  +------------------+  +------------------+  +------------------+         |
|  |     MAILBOX      |  |   MULTISIG ISM   |  |     REGISTRY     |         |
|  |    Validator     |  |    Validator     |  |    Validator     |         |
|  +--------+---------+  +--------+---------+  +--------+---------+         |
|           |                     |                     |                   |
|           | [State NFT]         | [State NFT]         | [State NFT]       |
|           v                     v                     v                   |
|  +------------------+  +------------------+  +------------------+         |
|  | Mailbox UTXO     |  | ISM UTXO         |  | Registry UTXO    |         |
|  | - local_domain   |  | - validators[]   |  | - registrations[]|         |
|  | - default_ism    |  | - thresholds[]   |  | - owner          |         |
|  | - owner          |  | - owner          |  +------------------+         |
|  | - outbound_nonce |  +------------------+           |                   |
|  | - merkle_root    |                                 |                   |
|  | - merkle_count   |                      +----------+----------+        |
|  +------------------+                      |          |          |        |
|                                            v          v          v        |
|  +------------------+             +------------+ +------------+ +------+  |
|  | Processed Msg    |             | Recipient  | | Recipient  | | Warp |  |
|  | Markers (NFTs)   |             | A (Generic)| | B (Token)  | | Route|  |
|  +------------------+             +------------+ +------------+ +------+  |
|                                                                           |
+===========================================================================+
```

### Component Roles

| Component | Purpose |
|-----------|---------|
| **Mailbox** | Central hub for dispatching and processing messages |
| **Multisig ISM** | Verifies validator signatures on checkpoints |
| **Registry** | Stores recipient metadata for relayer discovery |
| **Processed Message Markers** | NFTs preventing message replay |
| **Recipients** | Application contracts receiving messages |
| **Warp Route** | Token bridge contract (lock/mint pattern) |

---

## Message Flow Diagrams

### Inbound Message Flow (Other Chain -> Cardano)

```
+=============================================================================+
|                    INBOUND MESSAGE FLOW (EVM -> CARDANO)                     |
+=============================================================================+
|                                                                             |
|  ORIGIN CHAIN (EVM)                                                         |
|  +-------------------+                                                      |
|  | 1. App calls      |                                                      |
|  |    dispatch()     |                                                      |
|  +--------+----------+                                                      |
|           |                                                                 |
|           v                                                                 |
|  +-------------------+                                                      |
|  | 2. Mailbox emits  |                                                      |
|  |    Dispatch event |                                                      |
|  |    + merkle insert|                                                      |
|  +--------+----------+                                                      |
|           |                                                                 |
|           v                                                                 |
|  +-------------------+                                                      |
|  | 3. Validators sign|                                                      |
|  |    checkpoint     |                                                      |
|  +--------+----------+                                                      |
|           |                                                                 |
|           |                                                                 |
|  RELAYER NETWORK     |                                                      |
|  +--------v----------+                                                      |
|  | 4. Indexer        |                                                      |
|  |    detects msg    |                                                      |
|  +--------+----------+                                                      |
|           |                                                                 |
|           v                                                                 |
|  +-------------------+                                                      |
|  | 5. Fetch validator|                                                      |
|  |    signatures     |                                                      |
|  +--------+----------+                                                      |
|           |                                                                 |
|           v                                                                 |
|  +-------------------+                                                      |
|  | 6. Query registry |                                                      |
|  |    for recipient  |                                                      |
|  +--------+----------+                                                      |
|           |                                                                 |
|           v                                                                 |
|  +-------------------+                                                      |
|  | 7. Discover UTXOs |                                                      |
|  |    (mailbox, ISM, |                                                      |
|  |    recipient, etc)|                                                      |
|  +--------+----------+                                                      |
|           |                                                                 |
|           v                                                                 |
|  +-------------------+                                                      |
|  | 8. Build Cardano  |                                                      |
|  |    transaction    |                                                      |
|  +--------+----------+                                                      |
|           |                                                                 |
|           |                                                                 |
|  DESTINATION CHAIN (CARDANO)                                                |
|  +--------v----------+                                                      |
|  | 9. Submit tx with |                                                      |
|  |    multiple script|                                                      |
|  |    inputs:        |                                                      |
|  |    - Mailbox      |                                                      |
|  |    - ISM          |                                                      |
|  |    - Recipient    |                                                      |
|  +-------------------+                                                      |
|                                                                             |
+=============================================================================+
```

### Cardano Process Transaction Structure

```
+===========================================================================+
|                   CARDANO PROCESS TRANSACTION                              |
+===========================================================================+
|                                                                           |
|  INPUTS (Each triggers its validator):                                    |
|  +---------------------------------------------------------------------+  |
|  | [0] Mailbox UTXO                                                    |  |
|  |     - Redeemer: Process { message, metadata, message_id }           |  |
|  |     - Validates: message_id correct, destination matches,           |  |
|  |                  ISM present, recipient called, marker created      |  |
|  +---------------------------------------------------------------------+  |
|  | [1] ISM UTXO                                                        |  |
|  |     - Redeemer: Verify { checkpoint, validator_signatures[] }       |  |
|  |     - Validates: threshold signatures from trusted validators       |  |
|  +---------------------------------------------------------------------+  |
|  | [2] Recipient UTXO                                                  |  |
|  |     - Redeemer: HandleMessage { origin, sender, body }              |  |
|  |     - Validates: mailbox NFT present in inputs (caller auth)        |  |
|  +---------------------------------------------------------------------+  |
|  | [3+] Fee Payment UTXOs (from relayer wallet)                        |  |
|  +---------------------------------------------------------------------+  |
|                                                                           |
|  REFERENCE INPUTS (read-only, no redeemer):                               |
|  +---------------------------------------------------------------------+  |
|  | - Mailbox Reference Script UTXO                                     |  |
|  | - ISM Reference Script UTXO                                         |  |
|  | - Recipient Reference Script UTXO (if separate)                     |  |
|  +---------------------------------------------------------------------+  |
|                                                                           |
|  OUTPUTS:                                                                 |
|  +---------------------------------------------------------------------+  |
|  | [0] Mailbox Continuation (datum unchanged)                          |  |
|  | [1] ISM Continuation (datum unchanged)                              |  |
|  | [2] Recipient Continuation (datum updated with message)             |  |
|  | [3] Processed Message Marker NFT (prevents replay)                  |  |
|  | [4] Change back to relayer                                          |  |
|  +---------------------------------------------------------------------+  |
|                                                                           |
|  All validators execute in parallel. If ANY fails -> entire TX rejected.  |
|                                                                           |
+===========================================================================+
```

### Outbound Message Flow (Cardano -> Other Chain)

```
+=============================================================================+
|                   OUTBOUND MESSAGE FLOW (CARDANO -> EVM)                     |
+=============================================================================+
|                                                                             |
|  CARDANO CHAIN                                                              |
|  +-------------------+                                                      |
|  | 1. User builds tx |                                                      |
|  |    spending:      |                                                      |
|  |    - App UTXO     |                                                      |
|  |    - Mailbox UTXO |                                                      |
|  +--------+----------+                                                      |
|           |                                                                 |
|           v                                                                 |
|  +-------------------+                                                      |
|  | 2. Mailbox        |                                                      |
|  |    Dispatch:      |                                                      |
|  |    - Increment    |                                                      |
|  |      nonce        |                                                      |
|  |    - Update       |                                                      |
|  |      merkle tree  |                                                      |
|  +--------+----------+                                                      |
|           |                                                                 |
|           v                                                                 |
|  +-------------------+                                                      |
|  | 3. TX confirmed   |                                                      |
|  |    on Cardano     |                                                      |
|  +--------+----------+                                                      |
|           |                                                                 |
|           |                                                                 |
|  RELAYER/VALIDATOR NETWORK                                                  |
|  +--------v----------+                                                      |
|  | 4. Cardano        |                                                      |
|  |    validators     |                                                      |
|  |    index dispatch |                                                      |
|  +--------+----------+                                                      |
|           |                                                                 |
|           v                                                                 |
|  +-------------------+                                                      |
|  | 5. Sign checkpoint|                                                      |
|  |    (merkle_root,  |                                                      |
|  |     index, msg_id)|                                                      |
|  +--------+----------+                                                      |
|           |                                                                 |
|           v                                                                 |
|  +-------------------+                                                      |
|  | 6. Relayer builds |                                                      |
|  |    destination tx |                                                      |
|  +--------+----------+                                                      |
|           |                                                                 |
|           |                                                                 |
|  DESTINATION CHAIN (EVM)                                                    |
|  +--------v----------+                                                      |
|  | 7. Mailbox.process|                                                      |
|  |    (message,      |                                                      |
|  |     metadata)     |                                                      |
|  +--------+----------+                                                      |
|           |                                                                 |
|           v                                                                 |
|  +-------------------+                                                      |
|  | 8. ISM verifies   |                                                      |
|  |    Cardano        |                                                      |
|  |    validator sigs |                                                      |
|  +--------+----------+                                                      |
|           |                                                                 |
|           v                                                                 |
|  +-------------------+                                                      |
|  | 9. Recipient      |                                                      |
|  |    handle()       |                                                      |
|  +-------------------+                                                      |
|                                                                             |
+=============================================================================+
```

---

## Security Model

### Cryptographic Security Chain

```
+=============================================================================+
|                          MESSAGE INTEGRITY CHAIN                             |
+=============================================================================+
|                                                                             |
|  [1] Application dispatches message on Origin Chain                         |
|       |                                                                     |
|       v                                                                     |
|  [2] Message ID = keccak256(version || nonce || origin || sender ||         |
|                             destination || recipient || body)               |
|       |                                                                     |
|       v                                                                     |
|  [3] Message ID inserted into Merkle Tree                                   |
|       |                                                                     |
|       v                                                                     |
|  [4] Validators sign Checkpoint:                                            |
|      - domain_hash = keccak256(origin || merkle_hook || "HYPERLANE")        |
|      - digest = keccak256(domain_hash || root || index || message_id)       |
|      - signature = ECDSA_secp256k1(EIP-191(digest))                         |
|       |                                                                     |
|       v                                                                     |
|  [5] Relayer collects threshold signatures + merkle proof                   |
|       |                                                                     |
|       v                                                                     |
|  [6] Destination ISM verifies:                                              |
|      - Signature validity (secp256k1)                                       |
|      - Signer address derivation (keccak256(pubkey)[12:32])                 |
|      - Signer in trusted validator set                                      |
|      - Threshold met                                                        |
|       |                                                                     |
|       v                                                                     |
|  [7] Mailbox verifies message_id & delivers to recipient                    |
|                                                                             |
+=============================================================================+
```

### Security Properties

| Property | Mechanism | Guarantee |
|----------|-----------|-----------|
| **Authenticity** | keccak256 hash | Message content is exactly what was dispatched |
| **Ordering** | Unique nonce per origin | Prevents out-of-order delivery |
| **No Replay** | Processed message NFT markers | Prevents re-delivery |
| **No Tampering** | Message ID verification | Any modification detected |
| **Threshold Security** | m-of-n multisig | Requires collusion of m validators |
| **Domain Separation** | Domain hash includes origin chain | Cross-chain signature replay prevented |

### Cardano-Specific Security: NFT-Based Authentication

Unlike EVM where `msg.sender` provides caller authentication, Cardano uses **NFT markers**:

```
+=============================================================================+
|                    NFT-BASED AUTHENTICATION MODEL                            |
+=============================================================================+
|                                                                             |
|  WHY NFTs?                                                                  |
|  - Anyone can create a UTXO at ANY script address with ANY datum           |
|  - Script hash check alone is insufficient (attacker can create fake UTXO) |
|  - NFTs are unique (only one can exist with given policy_id)               |
|                                                                             |
|  AUTHENTICATION FLOW:                                                       |
|  +---------------------+                                                    |
|  | Recipient contract  |                                                    |
|  | parameterized with  |                                                    |
|  | mailbox_policy_id   |                                                    |
|  +---------+-----------+                                                    |
|            |                                                                |
|            v                                                                |
|  +---------------------+                                                    |
|  | On HandleMessage:   |                                                    |
|  | Check if any input  |                                                    |
|  | contains NFT with   |                                                    |
|  | mailbox_policy_id   |                                                    |
|  +---------+-----------+                                                    |
|            |                                                                |
|            v                                                                |
|  +---------------------+                                                    |
|  | If NFT present:     |                                                    |
|  | REAL mailbox called |                                                    |
|  | this recipient      |                                                    |
|  +---------------------+                                                    |
|                                                                             |
|  ATTACK PREVENTION:                                                         |
|  - Attacker cannot mint mailbox NFT (one-shot policy)                       |
|  - Fake mailbox UTXO without NFT rejected                                   |
|  - Only tx with real mailbox UTXO can spend recipient                       |
|                                                                             |
+=============================================================================+
```

---

## Contract Implementations

### Mailbox Validator (`mailbox.ak`)

**Purpose**: Central hub for dispatching and processing messages

**Actions**:
| Redeemer | Description |
|----------|-------------|
| `Dispatch` | Send message to another chain (outbound) |
| `Process` | Receive message from another chain (inbound) |
| `SetDefaultIsm` | Admin: update default ISM |
| `TransferOwnership` | Admin: transfer ownership |

**Key Validations (Process)**:
1. Message destination matches local domain
2. Message ID correctly computed (keccak256)
3. Message not already processed (no existing marker)
4. ISM UTXO is spent (triggers verification)
5. Recipient UTXO is spent (triggers handling)
6. Processed message marker is created

### Multisig ISM Validator (`multisig_ism.ak`)

**Purpose**: Verifies that enough trusted validators signed the checkpoint

**Signature Verification Flow**:
```
1. Relayer recovers public keys from signatures off-chain (using recovery ID)
2. Passes both compressed (33 bytes) and uncompressed (64 bytes) pubkeys
3. ISM verifies:
   a. Compressed and uncompressed keys match (same x-coordinate)
   b. Signature valid using verify_ecdsa_secp256k1_signature (CIP-49)
   c. Ethereum address = keccak256(uncompressed_pubkey)[12:32]
   d. Address is in trusted validators list for origin domain
4. Count valid signatures >= threshold
```

### Registry Validator (`registry.ak`)

**Purpose**: Stores recipient metadata enabling generic relayer logic

**Registration Entry**:
```aiken
type RecipientRegistration {
  script_hash: ScriptHash,
  state_locator: UtxoLocator,           // NFT to find state UTXO
  reference_script_locator: Option<UtxoLocator>,  // NFT to find script UTXO
  additional_inputs: List<AdditionalInput>,
  recipient_type: RecipientType,        // GenericHandler | TokenReceiver | ContractCaller
  custom_ism: Option<ScriptHash>,
}
```

### Generic Recipient (`generic_recipient.ak`)

**Purpose**: Example minimal recipient that stores received messages

**Security Pattern**:
```aiken
// Parameterized by mailbox NFT policy ID
validator generic_recipient(mailbox_policy_id: PolicyId) {
  spend(...) {
    when redeemer is {
      HandleMessage { origin, sender, body } -> {
        // CRITICAL: Check for mailbox NFT, not just script hash
        expect mailbox_is_caller(tx, mailbox_policy_id)
        // ... process message
      }
    }
  }
}
```

### Warp Route (`warp_route.ak`)

**Purpose**: Token bridge with lock/mint pattern

**Token Types**:
| Type | Outbound (Send) | Inbound (Receive) |
|------|----------------|-------------------|
| `Collateral` | Lock tokens in vault | Release from vault |
| `Synthetic` | Burn synthetic tokens | Mint synthetic tokens |
| `Native` | Lock ADA in vault | Release ADA from vault |

---

## Implementation Status

### Completed

| Component | Status | Notes |
|-----------|--------|-------|
| Mailbox validator | COMPLETE | Dispatch + Process working |
| Multisig ISM | COMPLETE | secp256k1 ECDSA verification |
| Registry validator | COMPLETE | With reference_script_locator support |
| Generic Recipient | COMPLETE | Example implementation |
| Merkle tree library | COMPLETE | keccak256-based incremental tree |
| State NFT minting | COMPLETE | One-shot policies |
| Processed message markers | COMPLETE | NFT-based replay prevention |
| Rust tx_builder | COMPLETE | Full transaction construction |
| Rust mailbox | COMPLETE | Process + count + delivered |
| Rust registry client | COMPLETE | Registration lookup |
| Rust ISM encoding | COMPLETE | Checkpoint + signatures |
| Reference script support | COMPLETE | Two-UTXO pattern |
| CLI deployment tools | COMPLETE | init, deploy, register commands |

### In Progress

| Component | Status | Notes |
|-----------|--------|-------|
| End-to-end testing | IN PROGRESS | Fuji <-> Cardano Preview |
| Warp Route validator | PARTIAL | Core logic done, needs vault integration |
| IGP (Gas Paymaster) | PARTIAL | Types defined, validator logic pending |

### Missing / Future Work

| Component | Priority | Notes |
|-----------|----------|-------|
| Validator Announce | HIGH | Required for outbound messages |
| Cardano validator (signer) | HIGH | Off-chain component for outbound |
| Merkle proof verification | MEDIUM | For light client ISM variants |
| Custom recipient ISM | LOW | NFT-based ISM selection |
| Vault validator | MEDIUM | For collateral token warp routes |
| Production audit | HIGH | Security review before mainnet |

---

## Deviations from EVM

### 1. Transaction Model

| Aspect | EVM | Cardano |
|--------|-----|---------|
| Model | Account-based | UTXO-based |
| State | Contract storage | Datum in UTXO |
| Caller ID | `msg.sender` | NFT-based authentication |
| Script execution | Sequential calls | Parallel validation |
| State mutation | In-place update | Consume + recreate UTXO |

### 2. Contract Invocation

**EVM:**
```solidity
mailbox.process(metadata, message);
// Internally calls: recipient.handle(origin, sender, body)
```

**Cardano:**
```
Transaction must include:
- Mailbox UTXO as input (with Process redeemer)
- ISM UTXO as input (with Verify redeemer)
- Recipient UTXO as input (with HandleMessage redeemer)
All validators execute and ALL must pass
```

### 3. ISM Selection

| Aspect | EVM | Cardano |
|--------|-----|---------|
| Recipient-specific ISM | `recipient.interchainSecurityModule()` | Not safely supported* |
| Default | Mailbox's default ISM | Mailbox's default ISM |

*On Cardano, reading ISM from recipient datum is vulnerable because anyone can create a fake UTXO with malicious ISM in datum.

### 4. Message Delivery Check

**EVM:**
```solidity
mapping(bytes32 => bool) delivered;
// O(1) lookup
```

**Cardano:**
- **With NFT policy**: Query by asset (O(1))
- **Without NFT policy**: Scan UTXOs at script address (O(n))

### 5. Script Deployment

| Aspect | EVM | Cardano |
|--------|-----|---------|
| Deployment | One transaction | State UTXO + Reference Script UTXO |
| Script storage | In contract account | Reference script field or inline |
| Parameterization | Constructor args | Applied at compile time (Aiken) |

### 6. Fee Model

| Aspect | EVM | Cardano |
|--------|-----|---------|
| Gas | Variable per operation | Fixed per tx size + script execution |
| Estimation | Gas estimation | Ex-units estimation (mem + steps) |
| Payment | Native (ETH) | Native (ADA) |
| IGP | Abstract gas payments | Lovelace-based quotes |

---

## Potential Security Vulnerabilities

### 1. UTXO Contention (Severity: Medium)

**Description**: If multiple relayers try to process messages to the same recipient simultaneously, all but one will fail due to UTXO consumption.

**Mitigation**:
- Retry logic with exponential backoff
- Relayer coordination (different relayers for different recipients)
- Multi-UTXO recipient patterns (advanced)

### 2. Reference Script UTXO Destruction (Severity: Medium)

**Description**: If the reference script UTXO is spent/destroyed, transactions referencing it will fail.

**Mitigation**:
- Use "always fails" script address to make UTXO unspendable
- OR use multisig protection for deployer address
- Monitor reference script UTXO health

### 3. Registry Data Manipulation (Severity: Low)

**Description**: Malicious registration could point to wrong state locators.

**Mitigation**:
- Registration requires spending the recipient script (proof of ownership)
- Admin-only registration bypasses this but requires owner signature

### 4. ISM Datum Spoofing (Severity: N/A - Mitigated)

**Description**: Attacker creates fake ISM UTXO with malicious validator set.

**Mitigation**:
- ISM UTXO identified by state NFT (not just script address)
- Mailbox checks for specific ISM script hash

### 5. Processed Message Marker Spoofing (Severity: N/A - Mitigated)

**Description**: Attacker pre-creates processed message markers to block legitimate messages.

**Mitigation**:
- Markers created only when mailbox UTXO is consumed
- One-shot NFT policy tied to mailbox script

### 6. Merkle Tree State Drift (Severity: Low)

**Description**: On-chain merkle tree may drift from expected state.

**Mitigation**:
- Validators track and sign checkpoints
- Message ID includes nonce for ordering
- Tree root verified in checkpoint

### 7. Signature Recovery Manipulation (Severity: Low)

**Description**: Relayer provides wrong public key for signature.

**Mitigation**:
- On-chain signature verification with provided pubkey
- Address derivation from verified pubkey
- Address must match trusted validator set

---

## Scalability Limitations and Improvements

### The State Thread Token Bottleneck

The current design uses the **State Thread Token (STT)** pattern from Aiken for all stateful contracts. While this provides strong security guarantees, it creates a fundamental scalability limitation: **only one transaction can consume a given UTXO per block**.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                    STATE THREAD TOKEN BOTTLENECK                             │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  SCENARIO: 3 messages arrive simultaneously for Cardano                     │
│                                                                             │
│  Relayer 1: Build TX with Mailbox UTXO (tx_hash#0)                          │
│  Relayer 2: Build TX with Mailbox UTXO (tx_hash#0)  ← Same UTXO!            │
│  Relayer 3: Build TX with Mailbox UTXO (tx_hash#0)  ← Same UTXO!            │
│                                                                             │
│  Submit to mempool:                                                         │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │  TX 1 ──────▶ ✅ Confirmed (consumes tx_hash#0)                     │   │
│  │  TX 2 ──────▶ ❌ REJECTED (tx_hash#0 already spent!)                │   │
│  │  TX 3 ──────▶ ❌ REJECTED (tx_hash#0 already spent!)                │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  Result: Only 1 message processed per block for this mailbox                │
│                                                                             │
│  Timeline:                                                                  │
│  ┌──────┐    ┌──────┐    ┌──────┐    ┌──────┐                              │
│  │Slot 1│───▶│Slot 2│───▶│Slot 3│───▶│Slot 4│ ...                          │
│  │Msg 1 │    │Msg 2 │    │Msg 3 │    │      │                              │
│  └──────┘    └──────┘    └──────┘    └──────┘                              │
│    20s         20s         20s                                              │
│                                                                             │
│  3 messages = 60 seconds minimum (vs EVM: all in 1 block)                   │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Contention Points Analysis

| Component | Contention? | Why |
|-----------|-------------|-----|
| **Mailbox UTXO** | ✅ YES | Single state NFT, all messages go through it |
| **ISM UTXO** | ✅ YES | Single state NFT, spent for every verification |
| **Recipient UTXO** | ✅ YES | Each recipient has one state UTXO |
| **Registry UTXO** | ⚠️ Rarely | Only contention during registration, not message processing |
| **Processed Msg NFTs** | ❌ NO | New UTXO created each time, no contention |

**Worst case**: If two messages go to the **same recipient**, you have contention on:
1. Mailbox (1 UTXO)
2. ISM (1 UTXO)
3. Recipient (1 UTXO)

All three must be in the same TX → only 1 message per ~20 seconds to that recipient.

### Current Throughput

| Scenario | Throughput |
|----------|------------|
| Single recipient (worst case) | ~3 messages/minute |
| Single recipient (hourly) | ~180 messages/hour |
| Different recipients (parallel) | ~3 messages/minute per recipient |

### Potential Improvements

#### 1. Batching Multiple Messages Per Transaction

Process multiple messages in a single TX to amortize the UTXO contention:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         BATCHED PROCESSING                                   │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  Single Transaction:                                                        │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │  INPUTS:                                                            │   │
│  │    - Mailbox UTXO (redeemer: ProcessBatch [msg1, msg2, msg3])       │   │
│  │    - ISM UTXO (redeemer: VerifyBatch [checkpoint1, 2, 3])           │   │
│  │    - Recipient A UTXO (redeemer: HandleBatch [msg1, msg2])          │   │
│  │    - Recipient B UTXO (redeemer: HandleBatch [msg3])                │   │
│  │                                                                     │   │
│  │  OUTPUTS:                                                           │   │
│  │    - Mailbox continuation                                           │   │
│  │    - ISM continuation                                               │   │
│  │    - Recipient A continuation                                       │   │
│  │    - Recipient B continuation                                       │   │
│  │    - 3x Processed message markers                                   │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  Throughput: 3 messages in 1 slot (~20s) instead of 3 slots (60s)           │
│  Limitation: TX size limits, execution unit limits                          │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Changes Required**:
- New `ProcessBatch` redeemer in mailbox
- Batch verification in ISM
- Batch handling in recipients
- TX builder batching logic

**Estimated Throughput**: 5-10 messages per TX → ~15-30 messages/minute

#### 2. Sharded Mailboxes (Multiple State Threads)

Deploy multiple mailbox instances to enable parallel processing:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         SHARDED MAILBOXES                                    │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  Mailbox Shard 0        Mailbox Shard 1        Mailbox Shard 2             │
│  (NFT policy A)         (NFT policy B)         (NFT policy C)              │
│       │                      │                      │                       │
│       ▼                      ▼                      ▼                       │
│  ┌─────────┐            ┌─────────┐            ┌─────────┐                 │
│  │ State 0 │            │ State 1 │            │ State 2 │                 │
│  └─────────┘            └─────────┘            └─────────┘                 │
│       │                      │                      │                       │
│   Msg 0, 3, 6...         Msg 1, 4, 7...         Msg 2, 5, 8...             │
│                                                                             │
│  Parallel processing: 3 messages per slot (one per shard)                   │
│                                                                             │
│  Routing: recipient_hash % num_shards = shard_id                            │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Trade-offs**:
- ✅ Linear scalability with shard count
- ❌ More complex deployment and configuration
- ❌ Cross-shard ordering guarantees lost
- ❌ Relayer must know which shard to use

#### 3. Stateless Mailbox for Inbound Processing

The mailbox datum **doesn't change** during `Process` (only during `Dispatch`). We can leverage this:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                       STATELESS INBOUND PROCESSING                           │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  Current Design:                                                            │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │  Mailbox UTXO (SPENT) ──▶ Mailbox Continuation                      │   │
│  │  - Validates message                                                 │   │
│  │  - Checks ISM present                                                │   │
│  │  - Checks recipient called                                           │   │
│  │  - Datum unchanged (no state update needed!)                         │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  Observation: For Process, mailbox datum DOESN'T CHANGE!                    │
│  We only need mailbox state for Dispatch (outbound nonce, merkle tree)      │
│                                                                             │
│  Alternative: Mailbox as Reference Input                                    │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │  Mailbox UTXO (REFERENCE INPUT) ── read datum, don't spend          │   │
│  │  ISM UTXO (SPENT) ── signature verification                         │   │
│  │  Recipient UTXO (SPENT) ── handle message                           │   │
│  │  "Mailbox Authorizer" minting policy ── proves mailbox read         │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  Result: Mailbox UTXO never spent for inbound → no contention!              │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

**How It Would Work**:
1. Mailbox becomes a **reference input** for Process (not spent)
2. A new "Mailbox Authorizer" minting policy verifies:
   - Mailbox reference input is present (by NFT)
   - ISM is spent (triggers verification)
   - Recipient is spent (triggers handling)
   - Processed message marker is minted
3. Recipients check for "Mailbox Authorizer" token being minted instead of mailbox NFT in inputs

**Trade-offs**:
- ✅ Eliminates mailbox contention entirely for inbound
- ✅ Multiple messages can process in parallel
- ❌ More complex authentication (minting policy instead of NFT check)
- ❌ Recipients need different authentication pattern

#### 4. ISM as Staking Validator (Zero-ADA Withdrawals)

The ISM doesn't update state during verification—it only reads validators and thresholds. Using the **Forwarding Validation** pattern with staking validators:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                      ISM AS STAKING VALIDATOR                                │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  Current: ISM is SPENT                                                      │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │  ISM UTXO (input) ─── Verify redeemer ───▶ ISM Continuation          │   │
│  │  - Validates signatures                                              │   │
│  │  - Datum unchanged                                                   │   │
│  │  - Creates contention!                                               │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  Alternative: ISM as Reference + Staking Validator                          │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │  ISM UTXO (REFERENCE INPUT) ── read validators/thresholds           │   │
│  │  ISM Staking Validator (WITHDRAW 0 ADA) ── verify signatures         │   │
│  │  - No UTXO contention!                                               │   │
│  │  - Parallel verification possible                                    │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  This uses the "Forwarding Validation" pattern with zero-ADA withdrawals.   │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Improvement Summary

| Contention Point | Current | Possible Improvement | Complexity | Impact |
|------------------|---------|---------------------|------------|--------|
| **Mailbox (inbound)** | SPENT | Reference + mint auth | High | Eliminates contention |
| **Mailbox (outbound)** | SPENT | Must stay (state changes) | N/A | N/A |
| **ISM** | SPENT | Staking validator | Medium | Eliminates contention |
| **Recipient** | SPENT | Sharding per recipient | Medium | Linear scaling |
| **Processed markers** | ✅ None | Already optimal | N/A | N/A |

### Recommended Roadmap

1. **Short-term**: Implement message batching (easiest, moderate improvement)
2. **Medium-term**: Convert ISM to staking validator pattern (removes one contention point)
3. **Long-term**: Stateless mailbox for inbound + recipient sharding (maximum throughput)

With all optimizations, throughput would be limited only by:
- Cardano block size and execution unit limits
- Recipient-level contention (addressable via sharding)
- Network latency for signature collection

---

## Future Work

### Short Term

1. **Complete End-to-End Testing**: Verify full message flow between Fuji and Cardano Preview
2. **Validator Announce**: Enable validators to announce their storage locations
3. **Cardano Validator Implementation**: Off-chain signer component for outbound messages

### Medium Term

1. **Production Warp Routes**: Complete vault integration for token bridges
2. **IGP Implementation**: Full gas payment system
3. **Performance Optimization**: Reduce execution units, optimize CBOR encoding
4. **Multi-Recipient Batching**: Process multiple messages in single transaction

### Long Term

1. **Security Audit**: Professional review before mainnet
2. **Mainnet Deployment**: Production contracts on Cardano mainnet
3. **Light Client ISM**: ZK-based or light client verification
4. **Custom Recipient ISM**: NFT-based per-recipient ISM selection
5. **Rollup Support**: Integration with Cardano L2 solutions

---

## References

- [Hyperlane Documentation](https://docs.hyperlane.xyz/)
- [Hyperlane Monorepo](https://github.com/hyperlane-xyz/hyperlane-monorepo)
- [Aiken Documentation](https://aiken-lang.org/)
- [Cardano eUTXO Model](https://docs.cardano.org/learn/eutxo-explainer)
- [CIP-49: ECDSA secp256k1](https://cips.cardano.org/cip/CIP-0049)
- [Plutus V3 Reference](https://plutus.readthedocs.io/)

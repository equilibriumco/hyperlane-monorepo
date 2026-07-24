# Hyperlane Cardano Guide

This document is the consolidated reference for the Hyperlane-Cardano integration. It covers architecture, message flows, NFT patterns, recipient development, warp routes, IGP, validator operations, E2E testing, and integration status.

## Table of Contents

1. [Architecture Overview](#1-architecture-overview)
2. [Message Flows](#2-message-flows)
3. [NFT Patterns](#3-nft-patterns)
4. [Reference Scripts](#4-reference-scripts)
5. [Recipients](#5-recipients)
6. [Warp Routes](#6-warp-routes)
7. [Interchain Gas Paymaster (IGP)](#7-interchain-gas-paymaster-igp)
8. [Validator Operations](#8-validator-operations)
9. [Sepolia E2E Testing](#9-sepolia-e2e-testing)
10. [Known Limitations](#10-known-limitations)
11. [Integration Status](#11-integration-status)
12. [Future Optimizations](#12-future-optimizations)

---

## 1. Architecture Overview

Hyperlane is an interchain messaging protocol that enables applications to send arbitrary messages between blockchains. The Cardano integration adapts the protocol to Cardano's eUTXO model.

### Design Principles

1. **Relayer-driven**: The Hyperlane relayer constructs and submits all Cardano transactions
2. **eUTXO-compatible**: All state is managed through UTXOs with inline datums
3. **NFT-based identity**: State UTXOs are identified by unique NFTs rather than addresses
4. **Reference scripts**: Validators are stored as reference scripts to minimize transaction size

### Contract Dependency Graph

The arrows represent "checks that X is spent" relationships, not invocations:

```mermaid
flowchart TB
    subgraph Core["Core Contracts"]
        MB["Mailbox<br/>dispatch() / process()"]
        ISM["Multisig ISM<br/>verify()"]
    end

    subgraph Tokens["Token Contracts"]
        WR["Warp Route<br/>transferRemote() / receiveTransfer()"]
        SYNTH["Synthetic Token<br/>mint() / burn()"]
    end

    subgraph Recipients["Recipient Contracts"]
        GEN["Generic Recipient<br/>handleMessage()"]
    end

    subgraph NFTs["NFT Policies"]
        STATE["State NFT"]
        VERIFIED["Verified Message NFT"]
    end

    MB -->|"verifies"| ISM
    MB -->|"delivers NFT to"| GEN
    MB -->|"co-spends"| WR

    WR -->|"mints/burns"| SYNTH

    MB -->|"mints"| VERIFIED

    STATE -.->|"identifies"| MB
    STATE -.->|"identifies"| ISM
    STATE -.->|"identifies"| WR
```

The relayer resolves warp routes (`0x01`) via O(1) NFT queries using the state NFT policy. Generic recipients (`0x02`) are resolved by script hash address. No registry contract is needed.

### Cross-Contract Coordination in eUTXO

Unlike account-based chains (EVM, Solana), Cardano does not support cross-contract calls. All contracts in a transaction validate **independently and simultaneously** against the same transaction context.

Cross-contract coordination uses **mutual spending validation**: each contract checks that the other contracts it depends on are being spent in the same transaction.

```mermaid
flowchart TB
    subgraph Transaction["Single Atomic Transaction"]
        subgraph Inputs["Inputs (all spent together)"]
            MB["Mailbox UTXO"]
            ISM["ISM UTXO"]
            REC["Recipient UTXO"]
        end

        subgraph Validators["Validators (run independently)"]
            MBV["Mailbox Validator<br/>checks ISM is spent<br/>checks Recipient is spent<br/>updates SMT for replay protection"]
            ISMV["ISM Validator<br/>checks Signatures valid<br/>checks Threshold met"]
            RECV["Recipient Validator<br/>checks Mailbox is spent<br/>checks Message matches redeemer"]
        end

        subgraph Result["All Must Pass"]
            OK["Transaction Valid"]
        end
    end

    MB --> MBV
    ISM --> ISMV
    REC --> RECV

    MBV --> OK
    ISMV --> OK
    RECV --> OK
```

**How each contract ensures correctness:**

- **Mailbox** ensures the **trusted ISM** verifies **this specific message**: it resolves the ISM script hash for the recipient (the `default_ism`, or a per-recipient override taken from the recipient's authenticated config), checks that an input with that script hash **carrying the ISM's own state NFT** is being spent, then inspects that input's redeemer to confirm the checkpoint's `message_id` matches the expected one. This prevents untrusted-ISM attacks, datum-swapped ISM impostors, and signature replay.

- **Recipient** ensures the message came from the mailbox by checking that the mailbox UTXO (identified by its state NFT) is being spent in the same transaction.

In Cardano, all contracts validate the same transaction simultaneously. There is no control flow between contracts. Coordination is via "I see you're being spent, so I know your rules passed."

### Domain and Address Encoding

Cardano uses 28-byte identifiers (script hashes and policy IDs) padded to 32 bytes for Hyperlane compatibility. Two prefix types distinguish the identifier kind:

| Prefix | Meaning | Used By |
|--------|---------|---------|
| `0x01000000` | NFT minting policy ID | Warp routes |
| `0x02000000` | Script hash credential | Generic recipients, mailbox, ISM |

```
State NFT Policy ID:        0xabcdef1234567890... (28 bytes)
Hyperlane Address (NFT):    0x01000000abcdef1234567890... (32 bytes)

Generic Recipient Hash:     0x7fb8e3ae915c4c37... (28 bytes)
Hyperlane Address (script): 0x020000007fb8e3ae915c4c37... (32 bytes)
```

The mailbox's `verified_message_nft` minting/delivery is **conditional on the prefix**: only `0x02` recipients get verified message NFTs. Both paths are bound to the mailbox `Process` redeemer itself, not merely to the mailbox state NFT appearing as a spent input. For `0x02` recipients the mailbox mints the verified message NFT (asset name = `message_id`) and delivers it to the recipient's script address. For `0x01` warp routes the binding is bidirectional in the same transaction: the mailbox `Process` requires a matching warp `ReceiveTransfer{message, message_id}`, and the warp route requires the mailbox to be spent with the matching `Process{message, message_id}`. Because `Dispatch` is permissionless, a mere "mailbox is co-spending" check would be forgeable — the redeemer match is what proves the message was ISM-verified and SMT replay-checked.

### Domain IDs

| Chain            | Domain ID |
|------------------|-----------|
| Cardano Preview  | 2003      |
| Cardano Preprod  | 2002      |
| Cardano Mainnet  | 2001      |
| Sepolia (Ethereum) | 11155111     |
| Ethereum Mainnet | 1         |
| Ethereum Sepolia | 11155111  |

---

## 2. Message Flows

### Incoming Messages (Other Chains -> Cardano)

```mermaid
sequenceDiagram
    participant User as User/App
    participant SrcMailbox as Source Mailbox<br/>(e.g., Sepolia)
    participant Validators as Hyperlane Validators
    participant Storage as Checkpoint Storage<br/>(S3/GCS)
    participant Relayer as Hyperlane Relayer
    participant CardanoMailbox as Cardano Mailbox
    participant ISM as Multisig ISM
    participant Recipient as Recipient Contract

    User->>SrcMailbox: dispatch(destDomain, recipient, body)
    SrcMailbox->>SrcMailbox: Emit Dispatch event<br/>Update merkle tree

    Note over Validators: Validators monitor source chain
    Validators->>Validators: Sign checkpoint<br/>(merkleRoot, index, messageId)
    Validators->>Storage: Store signed checkpoint

    Note over Relayer: Relayer monitors source chain
    Relayer->>SrcMailbox: Index dispatched messages
    Relayer->>Storage: Fetch signed checkpoints
    Relayer->>Relayer: Build metadata<br/>(signatures + checkpoint)

    Note over Relayer: Relayer builds Cardano transaction
    Relayer->>CardanoMailbox: process(message, metadata)

    activate CardanoMailbox
    CardanoMailbox->>CardanoMailbox: Verify destination domain
    CardanoMailbox->>CardanoMailbox: Verify message ID (keccak256)
    CardanoMailbox->>CardanoMailbox: Check not already processed
    CardanoMailbox->>ISM: Verify signatures (spent in same tx)
    ISM->>ISM: Verify threshold signatures<br/>against validator set
    CardanoMailbox->>Recipient: Bind recipient (warp - ReceiveTransfer same tx,<br/>generic - mint verified-message UTXO for later)
    Recipient->>Recipient: Handle message (warp now, generic in separate tx)
    CardanoMailbox->>CardanoMailbox: Update SMT (replay protection)
    deactivate CardanoMailbox

    Note over Recipient: Message delivered!
```

The process transaction structure depends on the recipient type:

#### WarpRoute Recipients (Single TX)

The recipient UTXO is spent in the same transaction. Tokens go directly to the recipient wallet.

```mermaid
flowchart TB
    subgraph Inputs["Transaction Inputs"]
        MI[/"Mailbox UTXO<br/>with State NFT"/]
        II[/"ISM UTXO<br/>with State NFT"/]
        RI[/"Recipient UTXO<br/>with State NFT"/]
        CI[/"Collateral UTXO<br/>(for fees)"/]
    end

    subgraph RefInputs["Reference Inputs"]
        MBScript[/"Mailbox Reference Script"/]
        ISMScript[/"ISM Reference Script"/]
        RecScript[/"Recipient Reference Script"/]
    end

    subgraph Redeemers["Redeemers"]
        MR["Mailbox: Process{<br/>message, metadata, message_id}"]
        IR["ISM: Verify{<br/>checkpoint, signatures}"]
        RR["Warp Route: ReceiveTransfer{<br/>message, message_id}"]
    end

    subgraph Outputs["Transaction Outputs"]
        MO[/"Mailbox UTXO<br/>(updated SMT in datum)"/]
        IO[/"ISM UTXO<br/>(unchanged datum)"/]
        RO[/"Warp Route UTXO<br/>(updated total_bridged)"/]
    end

    MI --> MR
    II --> IR
    RI --> RR

    MR --> MO
    IR --> IO
    RR --> RO

    MBScript -.-> MI
    ISMScript -.-> II
    RecScript -.-> RI
```

#### Generic Recipients (Two-Phase Verified Message)

The mailbox creates a verified message UTXO at the recipient's script address. The recipient processes it in a separate transaction.

**TX 1: Mailbox Process**

```mermaid
flowchart TB
    subgraph Inputs["Transaction Inputs"]
        MI[/"Mailbox UTXO<br/>with State NFT"/]
        II[/"ISM UTXO<br/>with State NFT"/]
        CI[/"Collateral UTXO<br/>(for fees)"/]
    end

    subgraph RefInputs["Reference Inputs"]
        MBScript[/"Mailbox Reference Script"/]
        ISMScript[/"ISM Reference Script"/]
    end

    subgraph Redeemers["Redeemers"]
        MR["Mailbox: Process{<br/>message, metadata, message_id}"]
        IR["ISM: Verify{<br/>checkpoint, signatures}"]
    end

    subgraph Outputs["Transaction Outputs"]
        MO[/"Mailbox UTXO<br/>(updated SMT in datum)"/]
        IO[/"ISM UTXO<br/>(unchanged datum)"/]
        VMO[/"Verified Message UTXO<br/>at recipient script address<br/>(VerifiedMessageDatum + NFT)"/]
    end

    subgraph Minting["Minting"]
        VMNFT["Verified Message NFT"]
    end

    MI --> MR
    II --> IR

    MR --> MO
    IR --> IO

    VMNFT --> VMO

    MBScript -.-> MI
    ISMScript -.-> II
```

**TX 2: Recipient Handling**

```mermaid
flowchart TB
    subgraph Inputs["Transaction Inputs"]
        VMI[/"Verified Message UTXO<br/>at recipient script address<br/>(VerifiedMessageDatum + NFT)"/]
        RSI[/"Recipient State UTXO<br/>with State NFT"/]
    end

    subgraph RefInputs["Reference Inputs"]
        RecScript[/"Recipient Reference Script"/]
    end

    subgraph Redeemers["Redeemers"]
        RR["Recipient: HandleMessage"]
    end

    subgraph Outputs["Transaction Outputs"]
        RO[/"Recipient State UTXO<br/>(updated state)"/]
    end

    subgraph Burning["Burning"]
        VMNFT["Verified Message NFT<br/>(burned)"]
    end

    VMI --> RR
    RSI --> RR

    RR --> RO
    VMI --> VMNFT

    RecScript -.-> VMI
    RecScript -.-> RSI
```

### Outgoing Messages (Cardano -> Other Chains)

```mermaid
sequenceDiagram
    participant User as User/App
    participant CardanoMailbox as Cardano Mailbox
    participant CardanoIndexer as Cardano Indexer<br/>(in Relayer)
    participant Validators as Hyperlane Validators<br/>(for Cardano)
    participant Storage as Checkpoint Storage<br/>(S3/GCS)
    participant Relayer as Hyperlane Relayer
    participant DestMailbox as Destination Mailbox<br/>(e.g., Sepolia)
    participant DestISM as Destination ISM
    participant Recipient as Recipient Contract

    User->>CardanoMailbox: dispatch(destDomain, recipient, body)

    activate CardanoMailbox
    CardanoMailbox->>CardanoMailbox: Build message<br/>(version, nonce, origin, sender,<br/>destination, recipient, body)
    CardanoMailbox->>CardanoMailbox: Compute message hash (keccak256)
    CardanoMailbox->>CardanoMailbox: Insert into merkle tree
    CardanoMailbox->>CardanoMailbox: Increment nonce
    deactivate CardanoMailbox

    Note over CardanoIndexer: Indexer monitors Cardano chain
    CardanoIndexer->>CardanoMailbox: Index Dispatch redeemers
    CardanoIndexer->>CardanoIndexer: Extract message data

    Note over Validators: Validators monitor Cardano
    Validators->>CardanoMailbox: Read merkle root & count
    Validators->>Validators: Sign checkpoint<br/>(merkleRoot, index, messageId)
    Validators->>Storage: Store signed checkpoint

    Relayer->>CardanoIndexer: Get dispatched messages
    Relayer->>Storage: Fetch signed checkpoints
    Relayer->>Relayer: Build metadata

    Relayer->>DestMailbox: process(message, metadata)
    DestMailbox->>DestISM: verify(message, metadata)
    DestISM->>DestISM: Verify Cardano validator signatures
    DestMailbox->>Recipient: handle(origin, sender, body)

    Note over Recipient: Message delivered!
```

### Dispatch TX Structure

```mermaid
flowchart TB
    subgraph Inputs["Transaction Inputs"]
        MI[/"Mailbox UTXO<br/>with State NFT<br/>datum: {nonce: N, merkle_tree: {branches, count}}"/]
        SI[/"Sender UTXO<br/>(pays for tx)"/]
    end

    subgraph RefInputs["Reference Inputs"]
        MBScript[/"Mailbox Reference Script"/]
    end

    subgraph Redeemer["Redeemer"]
        DR["Mailbox: Dispatch{<br/>destination, recipient, body}"]
    end

    subgraph Validation["On-Chain Validation"]
        V1["1. Build message struct"]
        V2["2. Compute message_hash = keccak256(message)"]
        V3["3. Insert hash into merkle tree (update branches)"]
        V4["4. Verify continuation datum:<br/>- nonce = N + 1<br/>- merkle_tree.branches updated<br/>- merkle_tree.count incremented"]
    end

    subgraph Outputs["Transaction Outputs"]
        MO[/"Mailbox UTXO<br/>datum: {nonce: N+1, merkle_tree: {newBranches, count+1}}"/]
        CO[/"Change UTXO"/]
    end

    MI --> DR
    SI --> DR
    DR --> V1
    V1 --> V2
    V2 --> V3
    V3 --> V4
    V4 --> MO
    DR --> CO

    MBScript -.-> MI
```

The mailbox stores the full incremental merkle tree state (32 branches x 32 bytes = 1024 bytes) in the datum. This enables proper on-chain merkle validation at the cost of ~4.4 ADA in minUTxO. The fixed-size branch array (32 branches for 2^32 capacity) means storage remains constant regardless of message count.

### Signature Verification Flow

```mermaid
flowchart LR
    subgraph Metadata["Metadata (from Relayer)"]
        CP["Checkpoint:<br/>origin, merkleRoot,<br/>merkleIndex, messageId"]
        SIGS["Signatures:<br/>[(pubkey, signature), ...]"]
    end

    subgraph ISMVerify["ISM Verification"]
        DH["1. Compute domain_hash<br/>keccak256(origin || merkleTreeHook || 'HYPERLANE')"]
        DIGEST["2. Compute digest<br/>keccak256(domainHash || root || index || messageId)"]
        ETH["3. Compute signing hash<br/>EIP-191 prefix + digest"]
        VERIFY["4. For each signature:<br/>- ECDSA verify (secp256k1)<br/>- Derive ETH address<br/>- Check against validator set"]
        THRESHOLD["5. Count valid >= threshold"]
    end

    CP --> DH
    DH --> DIGEST
    DIGEST --> ETH
    ETH --> VERIFY
    SIGS --> VERIFY
    VERIFY --> THRESHOLD
```

On-chain guarantees beyond the diagram: the threshold must be non-zero — a domain configured with threshold `0` is rejected by `verify_checkpoint`, so an empty signature set can never satisfy it — and duplicate signatures from the same validator address are counted once, so `N` copies of a single signature cannot meet an `N`-of-`M` threshold.

---

## 3. NFT Patterns

Cardano's eUTXO model requires different patterns than account-based chains. NFTs are used extensively to solve several challenges.

### State NFTs (One-Shot, State Thread)

**Problem**: UTXOs at a script address are not uniquely identifiable by address alone.

**Solution**: Each contract's state UTXO contains a unique "state NFT" combining two well-known Cardano patterns:

- [One-Shot Minting Policy](https://aiken-lang.org/fundamentals/common-design-patterns#one-shot-minting-policies): The NFT can only be minted once (parameterized by a UTXO that must be consumed)
- [State Thread Token](https://aiken-lang.org/fundamentals/common-design-patterns#state-thread-token): The NFT identifies the "current" state UTXO as it moves through transactions

```mermaid
flowchart TB
    subgraph StateNFT["State NFT Pattern"]
        direction TB
        MINT["One-shot minting policy<br/>(tied to specific UTXO)"]
        NFT["State NFT<br/>policy: 0xabc...<br/>name: 'Mailbox State'"]
        UTXO["State UTXO<br/>contains NFT + datum"]
    end

    subgraph Lookup["UTXO Lookup"]
        QUERY["Query by asset:<br/>policy_id + asset_name"]
        FOUND["Find unique UTXO<br/>containing the NFT"]
    end

    MINT --> NFT
    NFT --> UTXO
    UTXO --> QUERY
    QUERY --> FOUND
```

**Contracts using State NFTs:**

| Contract | NFT Purpose |
|----------|-------------|
| Mailbox | Identifies the single mailbox state UTXO |
| ISM | Identifies the ISM configuration UTXO |
| Warp Route | Identifies each warp route's state UTXO |
| Recipients | Each registered recipient has a state NFT |

### Replay Protection (Sparse Merkle Tree)

**Problem**: Prevent the same message from being processed twice.

**Solution**: The mailbox datum contains a Sparse Merkle Tree (SMT) that tracks processed message IDs. During `process()`, the mailbox inserts the message ID into the SMT and verifies it was not already present.

**On-chain**: The mailbox validator checks the SMT non-membership proof for the message ID before processing and verifies the continuation datum contains the updated SMT with the new member inserted.

**Off-chain (relayer)**: The relayer initializes the SMT by scanning all mailbox address transactions via Blockfrost and parsing Process redeemers to extract delivered message IDs. The `delivered()` check is an in-memory SMT lookup with no network calls.

**Key properties:**

- All replay protection state is in the mailbox datum (no separate UTXOs or NFTs)
- O(1) in-memory delivery check in the relayer
- No on-chain queries needed for delivery status

### Verified Message NFTs (Two-Phase Delivery)

**Problem**: Generic recipients (scripts) cannot be invoked by the mailbox directly. We need a way to prove that a message was validated by the mailbox so the recipient can process it later.

**Solution**: During mailbox `process()`, the mailbox mints a "verified message NFT" and creates a UTXO at the recipient's script address containing the NFT and a `VerifiedMessageDatum`. The recipient processes this UTXO in a separate transaction, burning the NFT.

```mermaid
flowchart TB
    subgraph Phase1["Phase 1: Mailbox Process (relayer TX)"]
        MB["Mailbox validates message<br/>+ ISM signature verification"]
        MINT["Mint Verified Message NFT<br/>(requires mailbox Process,<br/>asset name = message_id)"]
        STORE["Create UTXO at recipient's<br/>script address with NFT +<br/>VerifiedMessageDatum"]
    end

    subgraph Phase2["Phase 2: Recipient Handling (separate TX)"]
        READ["Spend message UTXO at<br/>recipient's script address"]
        VERIFY["Verify Verified Message NFT present<br/>(proves mailbox created it)"]
        PROCESS["Update recipient state"]
        BURN["Burn Verified Message NFT"]
    end

    MB --> MINT
    MINT --> STORE
    STORE --> READ
    READ --> VERIFY
    VERIFY --> PROCESS
    PROCESS --> BURN
```

**Security Properties:**

- The NFT can only be minted in a transaction where the mailbox is spent with a `Process` redeemer, its asset name must equal that `Process`'s `message_id`, and minting is restricted to canonical `0x02` recipients
- NFT presence therefore proves the mailbox ran `Process` (ISM-verified and SMT replay-checked) for that exact `message_id`, not merely that the mailbox was touched
- Burning prevents double-processing

### Synthetic Tokens

Warp routes mint/burn synthetic tokens representing assets from other chains. The `synthetic_token.ak` policy is parameterized by both the warp validator hash and route state NFT policy, then requires `ReceiveTransfer` for minting or `TransferRemote` for burning.

### NFT Summary Table

| NFT Type              | Purpose                      | Minting Policy          | When Minted                                  | When Burned                    |
|-----------------------|------------------------------|-------------------------|----------------------------------------------|--------------------------------|
| State NFT             | Identify state UTXOs         | One-shot (tied to UTXO) | Contract deployment                          | Never                          |
| Verified Message NFT  | Message authentication       | Mailbox-controlled      | Message processing (generic recipients only) | Message handling by recipient  |
| Synthetic Token       | Bridged token representation | Warp route-controlled   | Token receive                                | Token send                     |

### Datum Versioning

Every state datum carries a `version: Int` as **field 0**:

```aiken
pub type MailboxDatum {
  version: Int,          // field 0 on every state datum
  local_domain: Domain,
  default_ism: ScriptHash,
  ...
}
```

`MailboxDatum`, `IgpDatum`, `WarpRouteDatum` and `MultisigIsmDatum` all follow this
shape. The field exists so a validator can read the version out of a datum whose
remaining layout it does not know — which is what lets a migration check that
state moved *forward* rather than sideways.

**Rules:**

- Ordinary spends **preserve** the version. Every validator's continuation check
  asserts `old.version == new.version`, so a redeemer cannot quietly renumber
  state.
- Only `Migrate*` bumps it, computing `old + 1`. That is why an ordinary spend
  carrying a different version is rejected: the next migration would otherwise
  compute its successor from a moved base.

**Implications when writing tooling:**

Datum fields are positional in CBOR. Anything that builds or reads a datum by
index must account for `version` occupying index 0 — `local_domain` is field 1,
not field 0. A reader anchored to the old offsets does not fail loudly; it
returns the neighbouring field's value. The Rust agent parses `local_domain`
from index 1 for exactly this reason, and the mailbox datum tests assert every
field at once so a future insertion cannot shift one silently.

---

## 4. Reference Scripts

### Problem

When spending a script UTXO, the transaction needs access to the validator code. Including the full script in every transaction is expensive. Reference scripts allow pointing to a UTXO that contains the script, minimizing transaction size.

### Two-UTXO Pattern

Separate the state UTXO from the reference script UTXO:

```
Reference Script UTXO (never spent)
  Address: deployer address
  Value: ~20-30 ADA + NFT(policy, "ref")
  Datum: None
  Reference Script: <validator code>     <-- Script lives here

Recipient State UTXO (spent on handle)
  Address: script address
  Value: ~2 ADA + NFT(policy, "state")
  Datum: { contract state... }
  Reference Script: None
```

Advantages: state UTXO is smaller (less locked ADA), reference script UTXO is immutable and stable. This is the standard pattern used by all Hyperlane Cardano contracts.

### Configuration-Based Discovery

Reference scripts are resolved without any on-chain registry:

- **Core contracts** (mailbox, ISM): Reference script UTXOs configured in the relayer's `ConnectionConf` (e.g., `mailbox_reference_script_utxo`, `ism_reference_script_utxo`)
- **Warp routes**: A shared reference script UTXO configured via `warp_route_reference_script_utxo`. All warp route instances of the same validator share the same reference script.

### Deployment

When deploying a warp route, the deployment transaction produces two outputs:

- **Output #0**: State UTXO at the script address (state NFT + datum)
- **Output #1**: Reference script UTXO (reference script NFT with asset name `726566` = "ref" in hex + validator code)

The reference script NFT uses the same minting policy as the state NFT but with asset name `726566`. This UTXO is never spent.

### Relayer UTXO Discovery Flow

```
Message arrives: { recipient: 0x01...{nft_policy} or 0x02...{script_hash} }
  |
  v
1. Resolve recipient from address prefix
   0x01: Warp route -- extract NFT policy, query state UTXO by NFT
   0x02: Generic recipient -- extract script hash, query UTXOs at script address
  |
  v
2. Discover state UTXO by NFT query
   Query: UTXO containing NFT(nft_policy)
   Read datum for config, ISM, etc.
  |
  v
3. Load reference script UTXOs from config
   mailbox_reference_script_utxo, ism_reference_script_utxo, etc.
  |
  v
4. Build transaction
   Reference Inputs (read-only): reference script UTXOs
   Script Inputs (spent): mailbox UTXO, ISM UTXO, recipient state UTXO
   Outputs: continuations (mailbox with updated SMT, ISM unchanged)
  |
  v
5. Sign and submit (retry on contention)
```

| Component                   | Discovery Method                          | Purpose                                  |
|-----------------------------|-------------------------------------------|------------------------------------------|
| Recipient state UTXO        | NFT query from recipient address          | Find recipient's current state and datum |
| Mailbox reference script    | `mailbox_reference_script_utxo` config    | Mailbox validator code                   |
| ISM reference script        | `ism_reference_script_utxo` config        | ISM validator code                       |
| Warp route reference script | `warp_route_reference_script_utxo` config | Warp route validator code                |

---

## 5. Recipients

### Contract Pattern

On Cardano, Hyperlane recipients are Plutus V3 scripts that receive cross-chain messages from the Mailbox. The relayer handles message discovery, ISM verification, transaction building, and submission.

#### Datum Structure

The `HyperlaneRecipientDatum` wrapper is available for application metadata and nonce tracking. ISM selection comes from the separately authenticated canonical config UTXO:

```aiken
type HyperlaneRecipientDatum<inner> {
  ism: Option<ScriptHash>,
  last_processed_nonce: Option<Int>,
  inner: inner,
}
```

Simpler recipients (like `greeting.ak`) can define their own datum type directly.

#### Redeemer Structure

```aiken
type HyperlaneRecipientRedeemer<contract_redeemer> {
  HandleMessage { message: Message, message_id: ByteArray }
  ContractAction { action: contract_redeemer }
}
```

The `message_id` is what the ISM validated. Recipients **must** verify `keccak256(encode_message(message)) == message_id` to ensure message content authenticity.

### Greeting Example

The reference implementation is `greeting.ak`. Generic recipients are parameterized by `verified_message_nft_policy` and `owner`, and handle two types of UTXOs at their script address: **state UTXOs** (with the contract's datum) and **message UTXOs** (with a `verified_message_nft`, no typed datum).

```aiken
use types.{Message, PolicyId, encode_message}

type GreetingDatum {
  last_greeting: ByteArray,
  greeting_count: Int,
}

type GreetingRedeemer {
  Init
  HandleMessage { message: Message, message_id: ByteArray }
  Reclaim
}

validator greeting(verified_message_nft_policy: PolicyId, owner: VerificationKeyHash) {
  spend(
    datum: Option<Data>,
    redeemer: GreetingRedeemer,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    expect Some(own_input) = find_input(tx, own_ref)

    when redeemer is {
      Init -> list.has(tx.extra_signatories, owner) && is_ada_only(own_input)
      Reclaim -> list.has(tx.extra_signatories, owner) && is_ada_only(own_input)
      HandleMessage { message, message_id } -> {
        expect list.has(tx.extra_signatories, owner)
        // Message UTXO: require the matching state spend as well as the burn
        // State UTXO (holds greeting state NFT): full message processing
        let is_message_utxo = quantity_of(own_input, verified_message_nft_policy, message_id) == 1

        if is_message_utxo {
          verified_nft_burned(tx, verified_message_nft_policy, message_id)
            && matching_state_input_spent(tx, message, message_id)
        } else {
          expect Some(raw_datum) = datum
          expect old_datum: GreetingDatum = raw_datum
          let greeting = bytearray.concat("Hello, ", message.body)
          // ... verify continuation datum, NFT burn, message UTXO spent
          new_datum.last_greeting == greeting
            && new_datum.greeting_count == old_datum.greeting_count + 1
        }
      }
    }
  }

  else(_) { fail }
}
```

Key points:
- `HandleMessage` carries the full `Message` and `message_id`
- Verifies `keccak256(encode_message(message)) == message_id`
- The `verified_message_nft` burn proves the mailbox created the message
- Both message and state inputs require the same `HandleMessage`; the message UTXO cannot be grief-burned alone
- The reference recipient owner signs delivery, preventing fake-state substitution; permissionless custom recipients must authenticate their own state NFT instead
- `Init` and `Reclaim` redeemers require owner signature and ADA-only input
- The `None` datum branch handles the message UTXO (no contract-specific datum)

### State UTXO Pattern

Recipients must have a **state UTXO** at the script address containing an NFT marker for unique identification and contract state in an inline datum. The relayer uses this NFT to find the state UTXO.

### Addressing (`0x01` vs `0x02`)

**Warp routes** use NFT-policy addressing (`0x01` prefix):
- Hyperlane address = `0x01000000{state_nft_policy_id}` (32 bytes)
- Relayer discovers via O(1) NFT query
- Spent in the same TX as the mailbox (TokenReceiver)

**Generic recipients** use script-hash addressing (`0x02` prefix):
- Hyperlane address = `0x02000000{script_hash}` (32 bytes)
- Relayer discovers by querying UTXOs at the script address
- Uses two-phase verified message delivery

No registration transaction is needed on either side. Remote chains enroll the appropriate address format.

### Two-Phase Message Delivery (Verified Message Pattern)

This is the **default pattern** for generic (non-WarpRoute) recipients:

- **Phase 1 (Process TX)**: The mailbox mints a `verified_message_nft` and creates a UTXO at the recipient's script address containing the NFT and a `VerifiedMessageDatum`.
- **Phase 2 (Receive TX)**: Anyone spends the message UTXO together with the recipient's state UTXO, burning the NFT and updating the recipient state.

This pattern exists because the relayer cannot know how to build arbitrary recipient outputs.

#### VerifiedMessageDatum

```aiken
type VerifiedMessageDatum {
  origin: Domain,
  sender: HyperlaneAddress,
  body: ByteArray,
  message_id: ByteArray,
  nonce: Int,
}
```

#### The verified_message_nft Minting Policy

Parameterized by the mailbox policy ID. Minting requires the canonical mailbox input to use `Process`, and the minted asset name must equal that redeemer's `message_id`. Burning is allowed for 32-byte asset names with negative quantities.

### Canonical Config NFT (Per-Recipient ISM Override)

The `canonical_config_nft.ak` policy authenticates per-recipient ISM selection. It is a fixed (non-parameterized) policy whose ID is a protocol constant. The asset name of the minted token is the recipient's script hash (28 bytes), allowing the relayer to derive the config token for any `0x02 + script_hash` recipient without per-recipient pre-configuration.

Minting is only allowed when a spent input at the recipient's script address carries a constructor-0 redeemer (the `Init` tag).

> **Hard security requirement:** every `0x02` recipient using this mechanism must owner-gate constructor 0. The fixed policy cannot inspect an arbitrary recipient's semantics. A permissionless constructor-0 path lets an attacker mint another config token and select attacker-controlled ISM state. Do not enable per-recipient ISM overrides for external recipients until this invariant has been audited, or a trusted ISM registry replaces the convention.

An override commits to both the ISM script hash and its one-shot state NFT policy:

```aiken
type IsmConfig {
  script_hash: ScriptHash,
  state_nft_policy: PolicyId,
}
```

The selected ISM input must carry `ISM State` under that exact policy. This authenticates independently deployed per-recipient ISM state as well as the code.

### Deployment

#### Using the CLI (Recommended)

```bash
BLOCKFROST_API_KEY=your_api_key ./cli/target/release/hyperlane-cardano \
  --signing-key path/to/payment.skey \
  --network preview \
  init recipient \
  --custom-contracts ./contracts \
  --custom-module greeting \
  --custom-validator greeting
```

The CLI handles parameterization, NFT minting, and initial state creation. Output includes recipient script hash, state NFT policy ID, recipient script address, and TX hash. The `verified_message_nft_policy` is auto-derived from the mailbox deployment.

To select an independently deployed ISM, pass both authenticated identifiers:

```bash
  --custom-ism <script_hash> \
  --custom-ism-policy <state_nft_policy>
```

See the [DEPLOYMENT_GUIDE.md](./DEPLOYMENT_GUIDE.md) for full step-by-step instructions.

### Operating a Recipient

#### List Pending Messages

```bash
hyperlane-cardano message list \
  --recipient-address addr_test1wz...
```

#### Receive a Message

```bash
# Dry run first
hyperlane-cardano message receive \
  --message-utxo "txhash#0" \
  --recipient-policy def456... \
  --dry-run

# Submit
hyperlane-cardano message receive \
  --message-utxo "txhash#0" \
  --recipient-policy def456...
```

Parameters: `--message-utxo`, `--recipient-policy`, `--recipient-state-asset`, `--message-nft-policy` (auto-derived from mailbox deployment), `--recipient-ref-script`, `--nft-ref-script`, `--dry-run`.

#### Automated Processing

```bash
#!/bin/bash
MESSAGES=$(hyperlane-cardano message list \
  --recipient-address $RECIPIENT_ADDRESS --format json)

echo "$MESSAGES" | jq -r '.[].utxo' | while read UTXO; do
  hyperlane-cardano message receive \
    --message-utxo "$UTXO" \
    --recipient-policy $STATE_POLICY \
    --recipient-ref-script "$RECIPIENT_REF" \
    --nft-ref-script "$NFT_REF"
  sleep 30  # Avoid contention
done
```

### Security Considerations

**Generic recipients**: Verify the `verified_message_nft` is being burned (proves ISM validation):

```aiken
expect keccak_256(encode_message(message)) == message_id
expect verified_nft_burned(tx, verified_message_nft_policy, message_id)
expect message_utxo_spent(tx, own_addr, verified_message_nft_policy, message_id, own_ref)
```

**WarpRoute recipients**: Verify the mailbox script is co-spending:

```aiken
expect has_script_input(tx, mailbox_hash)
```

Both patterns ensure the message was validated by the ISM. Replay protection is handled by the mailbox's Sparse Merkle Tree.

**Validate origin and sender** for access control:

```aiken
expect origin == 1  // Only accept from Ethereum
expect sender == expected_sender  // Only accept from trusted contract
```

---

## 6. Warp Routes

Warp routes enable cross-chain token transfers through Hyperlane's messaging protocol.

### Types

| Type           | Use Case                                     | Mechanism                                              |
|----------------|----------------------------------------------|--------------------------------------------------------|
| **Native**     | Bridge ADA to other chains                   | Lock ADA in state UTXO on send, release on receive     |
| **Collateral** | Bridge Cardano native tokens to other chains | Lock tokens in state UTXO on send, release on receive  |
| **Synthetic**  | Receive tokens from other chains on Cardano  | Mint synthetic tokens on receive, burn on send          |

Both Native ADA and Collateral tokens are locked directly in the warp route state UTXO (no separate vault contract).

### Deploy

```bash
# Native (ADA)
hyperlane-cardano warp deploy --token-type native --decimals 6 \
  --signing-key /path/to/payment.skey --contracts-dir ./contracts

# Collateral
hyperlane-cardano warp deploy --token-type collateral \
  --token-policy <POLICY_ID> --token-asset <ASSET_NAME> --decimals 6 \
  --signing-key /path/to/payment.skey --contracts-dir ./contracts

# Synthetic
hyperlane-cardano warp deploy --token-type synthetic --decimals 6 \
  --remote-decimals 18 \
  --signing-key /path/to/payment.skey --contracts-dir ./contracts
```

### Enroll Remote Router

```bash
hyperlane-cardano warp enroll-router \
  --warp-policy <WARP_NFT_POLICY> \
  --domain 11155111 \
  --router 0x<REMOTE_ROUTER_PADDED_TO_32_BYTES> \
  --signing-key /path/to/payment.skey --contracts-dir ./contracts
```

### Transfer Tokens (Outbound)

```bash
hyperlane-cardano warp transfer \
  --warp-policy <WARP_NFT_POLICY> \
  --domain 11155111 \
  --recipient 0x<REMOTE_RECIPIENT> \
  --amount 1000000 \
  --signing-key /path/to/payment.skey --contracts-dir ./contracts
```

Amount is in the smallest unit (lovelace for ADA).

### Common Operations

```bash
# View configuration
hyperlane-cardano warp show --warp-policy <WARP_NFT_POLICY>

# List enrolled routers
hyperlane-cardano warp routers --warp-policy <WARP_NFT_POLICY>
```

### Architecture

#### UTXO Structure

Each warp route creates two UTXOs at deployment:

```
State UTXO (at warp route address) - ALL types
  Location: addr_test1wz...
  Value: 2,000,000+ lovelace + locked tokens*
  NFT: {nft_policy}."" (empty asset name)
  Datum: WarpRouteState { ... }
  * Native: holds locked ADA
  * Collateral: holds locked tokens
  * Synthetic: only MIN_UTXO lovelace

Reference Script UTXO (at deployer address) - ALL types
  Location: addr_test1qz... (deployer)
  Value: ~15,000,000 lovelace
  NFT: {nft_policy}.726566 ("ref")
  Script: warp_route validator

Minting Ref UTXO (for Synthetic routes only)
  Location: addr_test1qz... (deployer)
  NFT: {nft_policy}.6d696e745f726566 ("mint_ref")
  Script: synthetic_minting_policy
```

#### Datum Structure

```
WarpRouteDatum {
  config: WarpRouteConfig {
    token_type: Collateral | Synthetic | Native,
    decimals: Int,
    remote_routes: List<(Domain, RouterAddress)>
  },
  owner: VerificationKeyHash,
  total_bridged: Int,
  ism: Option<IsmConfig>
}
```

| Type       | Constructor | Fields                                     |
|------------|-------------|-------------------------------------------|
| Collateral | 0           | `policy_id`, `asset_name`                  |
| Synthetic  | 1           | `minting_policy`                           |
| Native     | 2           | (none)                                     |

#### Transfer Flows

**Native ADA:**

```
Outbound (Cardano -> Remote):
  User sends ADA -> locked in warp route state UTXO -> mailbox dispatch -> remote mints wADA

Inbound (Remote -> Cardano):
  Remote burns wADA -> message to Cardano -> warp route releases ADA -> user receives ADA
```

**Collateral:**

```
Outbound (Cardano -> Remote):
  User locks tokens in state UTXO -> mailbox dispatch -> remote mints wrapped tokens

Inbound (Remote -> Cardano):
  Remote burns wrapped tokens -> message to Cardano -> warp route releases tokens -> user receives tokens
```

**Synthetic:**

```
Inbound (Remote -> Cardano):
  Remote locks/burns tokens -> message to Cardano -> warp route mints synthetic tokens -> user receives tokens

Outbound (Cardano -> Remote):
  User burns synthetic tokens -> mailbox dispatch -> remote releases original tokens
```

#### Decimal Conversion

| Asset  | Cardano Decimals | EVM Decimals | Conversion Factor |
|--------|-----------------|--------------|-------------------|
| ADA    | 6               | 18           | 10^12             |
| HOSKY  | 0               | 18           | 10^18             |
| Custom | Varies          | 18           | 10^(18-local)     |

Wire amount: `wire_amount = local_amount * 10^(remote_decimals - local_decimals)`

When the remote chain has fewer decimals than Cardano, this conversion floor-divides. An outbound transfer whose wire amount would truncate to `0` (dust below the remote precision) is rejected on-chain, so collateral is never locked without a corresponding remote credit.

---

## 7. Interchain Gas Paymaster (IGP)

### Cardano Gas Cost Model

Cardano transaction costs:

```
cardano_tx_fee = min_fee_a * tx_size_bytes + min_fee_b + script_execution_cost + ref_script_cost
```

Where `min_fee_a` = 44 lovelace/byte, `min_fee_b` = 155,381 lovelace, `ref_script_cost` = 15 lovelace/byte (Conway era).

Costs depend on the **recipient type**:

#### Warp Route Recipients (`0x01`)

No `verified_message` UTXO created.

| Component | Cost |
|-----------|------|
| Script execution fee | ~95,000-133,000 lovelace |
| Base TX skeleton (~8KB) | ~330,000-346,000 lovelace |
| Reference script fee | ~150,000 lovelace |
| Message body in TX | ~44 lovelace/byte |

**Total = ~600,000 fixed + 44 * body_size variable**

#### Script Recipients (`0x02`)

The `verified_message` UTXO stores the full body as inline datum (`coins_per_utxo_byte` = 4,310 lovelace/byte):

| Component | Cost |
|-----------|------|
| Verified-message UTXO | ~1,700,000 + 4,310 * body_size |
| Script execution fee | ~95,000-133,000 lovelace |
| Base TX skeleton | ~330,000-346,000 lovelace |
| Reference script fee | ~150,000 lovelace |
| Message body in TX | ~44 lovelace/byte |

**Total = ~2,300,000 fixed + 4,354 * body_size variable**

> Full derivation, both directions and the reasoning behind the split live in
> [`design/igp-gas-model.md`](design/igp-gas-model.md). This section is the summary.

### Gas is denominated 1:1 in lovelace

Cardano has no gas market — fees are deterministic. Rather than invent a gas
price, the **Sepolia→Cardano** oracle sets `gasPrice = 1`, so one gas unit *is*
one lovelace and a `gasLimit` reads directly as "lovelace of estimated Cardano
delivery cost". The exchange rate carries the whole conversion.

> Superseded: earlier revisions modelled `gasPrice = 44` (Cardano's
> `min_fee_a`) and divided costs by 44. Any oracle or `gasLimit` still using
> that scheme is misconfigured.

### Oracle Configuration

#### EVM IGP (for Cardano destination)

```bash
# StorageGasOracle — gasPrice 1, exchange rate in wei-per-lovelace x 1e10
cast send $STORAGE_GAS_ORACLE \
  "setRemoteGasDataConfigs((uint32,uint128,uint128)[])" \
  "[(2003, 1395000000000000000, 1)]"

# IGP — recipient-independent overhead, in lovelace
cast send $IGP \
  "setDestinationGasConfigs((uint32,(address,uint96))[])" \
  "[(2003, ($STORAGE_GAS_ORACLE, 2062550))]"
```

#### Cardano IGP (for EVM destination)

```bash
hyperlane-cardano igp set-oracle \
  --domain 11155111 \
  --gas-price 1000000000 \
  --exchange-rate 7171 \
  --gas-overhead 155100
```

**The Cardano IGP's scale factor is `1e12`, not the `1e10` used by the Solidity
IGP.** An exchange rate copied from EVM tooling is wrong by a factor of 100. The
upside is that the rate stays human-readable: 7171 ADA per ETH is written `7171`.

### The overhead is charged on-chain

`PayForGas` carries **application gas only** — what the recipient costs to run.
The validator adds the destination's `gas_overhead` itself:

```
payment = (gas_amount + gas_overhead) x gas_price x token_exchange_rate / 1e12
```

Omitting the overhead is therefore not a way to pay less, and unlike the Solidity
IGP there is no mode that skips it — a standalone `igp pay-for-gas` top-up is
charged the overhead exactly as a bundled payment is.

### Payment is exact, and unpriced destinations are refused

Two rules that differ from the EVM IGP and surprise people:

- **The payment must equal the quote exactly.** Underpaying fails, and so does
  *over*paying. The IGP has one way out — `Claim`, to the beneficiary — so an
  overpayment is unrecoverable for the payer. Refusing the transaction costs a
  retry; accepting it would cost the difference, silently.
- **A destination with no configured oracle is rejected**, not defaulted. An
  earlier fallback priced such domains at zero, so a domain enrolled without its
  oracle looked paid for and was never delivered — failing nowhere near the
  missing config.

There is no refund path, no fee token, and no refund address: payment is always
lovelace, and the exact amount.

### Recipient-specific cost: warp `destination_gas`

The recipient-independent base belongs in the oracle's `gas_overhead`. Anything
recipient-specific belongs with the recipient:

| Sender | Where the recipient cost goes |
|---|---|
| Cardano warp route | `destination_gas` in the route datum, set by the owner via `warp set-destination-gas` |
| Any other Cardano sender | `--gas-limit` on the dispatching command |
| EVM warp route | `destinationGas` on the route (`GasRouter`) |
| Any other EVM sender | `gasLimit` in `StandardHookMetadata` |

The CLI resolves a warp transfer's gas as `--gas-limit` if given, else the route's
`destination_gas` for that domain, else it omits the IGP input entirely. **The
lookup is client-side**: the warp validator stores `destination_gas` and guards
updates to it, but never checks that a transfer actually paid it.

### Dispatching to Cardano from EVM

A `gasLimit` is lovelace of Cardano delivery cost.

| Recipient Type | What the sender must cover |
|----------------|----------------------------|
| Warp route (`0x01`) | Covered by the overhead for native/collateral. A **synthetic mint** additionally needs ~1.2 ADA of recipient minUTxO the relayer fronts and never recovers — the paired route must carry `destinationGas` for it, or the mint is rejected as underpaid. |
| Script (`0x02`) | The verified-message UTXO, which stores the full body in its inline datum: roughly `1_720_800 + 4_400 x body_bytes` lovelace. |

#### Example: Script Recipient

```solidity
// ~1.5x the verified-message minUTxO for this body size
uint256 gasLimit = (1_720_800 + 4_400 * body.length) * 3 / 2;
bytes memory metadata = StandardHookMetadata.overrideGasLimit(gasLimit);
uint256 fee = mailbox.quoteDispatch(cardanoDomain, recipient, body, metadata);
mailbox.dispatch{value: fee}(cardanoDomain, recipient, body, metadata);
```

Sending with **empty metadata** inherits Hyperlane's 50,000 default, which reads
as 50,000 lovelace — far below any real delivery. The message dispatches and then
parks as `GasPaymentRequirementNotMet`.

#### EVM Warp Routes (Automatic)

Warp routes extend `GasRouter`, which stores per-destination `destinationGas` and includes it as metadata automatically:

```solidity
// Owner configures once:
warpRoute.setDestinationGas(2003, 200);

// Users just call:
uint256 fee = warpRoute.quoteGasPayment(2003);
warpRoute.transferRemote{value: fee}(2003, recipient, amount);
```

### Relayer Gas Payment Enforcement

```json
{
  "gasPaymentEnforcement": [
    {
      "type": "onChainFeeQuoting",
      "gasFraction": "1/1"
    }
  ]
}
```

The relayer indexes `PayForGas` transactions from the IGP contract. Messages without sufficient payment are skipped.

### Cost Summary Table

All figures in lovelace, since `gasPrice = 1`.

| | Warp Routes (`0x01`) | Script Recipients (`0x02`) |
|---|---|---|
| `verified_message` UTXO | Not created | Stores full body as inline datum |
| Ledger fee | Covered by the oracle's `gas_overhead` | Covered by the oracle's `gas_overhead` |
| Unrecoverable minUTxO | ~0 for native/collateral; ~1.2 ADA per synthetic mint | `1_720_800 + 4_400 x body_bytes` |
| Who covers the recipient cost | Route `destination_gas` (synthetic only) | Sender's `gasLimit` |

The relayer's estimate is **what it spends and cannot recover** — ledger fee plus
that minUTxO — not the fee alone. Charging only the fee made the relayer deliver
synthetic mints at a loss.

### Recalibration

The oracle is a set of owner-set constants, not a feed. Drift in the sender's
favour is the dangerous direction: messages look paid for and the relayer quietly
declines to deliver them. Update when:

- Market exchange rates change >10%
- Cardano protocol parameters change (`min_fee_b`, `coins_per_utxo_byte`, and
  since Conway `min_fee_ref_script_cost_per_byte`)
- EVM destination gas prices change significantly
- Measured delivery cost drifts from ~1.5x the overhead you sized for

---

## 8. Validator Operations

A Hyperlane validator monitors the Cardano mailbox for dispatched messages, signs checkpoints proving message inclusion in the merkle tree, and stores these checkpoints for relayers to fetch.

### Prerequisites

1. **Blockfrost API Key** from [blockfrost.io](https://blockfrost.io)
2. **Validator Signing Key** - 32-byte hex-encoded ECDSA private key
3. **Funded Cardano Address** - minimum 3 ADA for on-chain announcement

### Quick Start

```bash
# 1. Generate config
cd cardano
./cli/target/release/hyperlane-cardano config update-validator \
  --validator-key 0x<your-64-char-hex-key> \
  --checkpoint-path ./signatures \
  --db-path /tmp/hyperlane-validator-db

# 2. Set environment
export BLOCKFROST_API_KEY=preview<your-api-key>

# 3. Create checkpoint directory
mkdir -p ./signatures

# 4. Run the validator
cd rust/main
cargo build --release -p validator
export CONFIG_FILES=/path/to/cardano/config/validator-config.json
./target/release/validator
```

### Configuration

#### Command Line Options

| Option | Description | Default |
|--------|-------------|---------|
| `--validator-key` | Validator signing key (hex) | Required |
| `--checkpoint-path` | Checkpoint storage directory | `./signatures` |
| `--db-path` | Validator database path | `/tmp/hyperlane-validator-db` |
| `--metrics-port` | Prometheus metrics port | `9091` |
| `--index-from` | Block to start indexing from | Auto-detected |

#### Environment Variables

| Variable | Description | Required |
|----------|-------------|----------|
| `BLOCKFROST_API_KEY` | Blockfrost API key | Yes |
| `VALIDATOR_HEX_KEY` | Validator signing key (alternative) | No |
| `CONFIG_FILES` | Path(s) to config file(s) | Yes (at runtime) |

#### Config File Structure

```json
{
  "originChainName": "cardanopreview",
  "db": "/tmp/hyperlane-validator-db",
  "interval": 5,
  "validator": {
    "type": "hexKey",
    "key": "0x..."
  },
  "checkpointSyncer": {
    "type": "localStorage",
    "path": "./signatures"
  },
  "chains": {
    "cardanopreview": {
      "name": "cardanopreview",
      "domainId": 2003,
      "protocol": "cardano",
      "connection": { ... }
    }
  }
}
```

### Validator Lifecycle

1. **Startup**: Load configuration, connect to Blockfrost
2. **Announcement Check**: Query ValidatorAnnounce contract
3. **Self-Announce**: If not announced, submit announcement transaction
4. **Wait for Messages**: Poll merkle tree hook until messages exist
5. **Sync Messages**: Index dispatched messages from the mailbox
6. **Sign Checkpoints**: For each new message, sign and store checkpoint
7. **Serve Checkpoints**: Checkpoints available for relayers

### Checkpoint Storage

**Local** (testing):

```json
"checkpointSyncer": {
  "type": "localStorage",
  "path": "./signatures"
}
```

**S3** (production):

```json
"checkpointSyncer": {
  "type": "s3",
  "bucket": "your-bucket-name",
  "region": "us-east-1"
}
```

### Validator Announcement

The validator announces its storage location on-chain so relayers can discover checkpoints. Requires a `signer` configured for the origin chain with sufficient ADA (minimum 3).

S3 URL format: `s3://<bucket>/<region>/<folder>`

```bash
./cli/target/release/hyperlane-cardano \
  --signing-key $CARDANO_SIGNING_KEY --network $NETWORK \
  validator announce --storage-location "s3://your-bucket/your-region/your-folder"
```

### Reorgs and settlement

Cardano's Ouroboros Praos settles **probabilistically**. There is no finality
gadget and no `finalized` block tag: a recently minted block can be rolled back,
and the chance of that decays exponentially with depth. The protocol bounds the
worst case at `k = 2160` blocks (~12h), which is the depth beyond which a node
refuses to switch chains at all — not something that happens in normal operation.

This matters for a validator more than for most agents. A signed checkpoint
cannot be withdrawn, so signing a root that later disappears leaves a valid
signature attesting to a dispatch that never happened — on the destination side,
a mint with nothing backing it.

#### `reorgPeriod` — reading behind the tip

The validator reads mailbox state **`reorgPeriod` blocks behind the tip** rather
than at it. Because Cardano exposes only the current UTXO set, past state is
reconstructed: the agent walks the mailbox state NFT back to the last
transaction at or before the target height and reads the datum it produced. This
is the equivalent of pinning an `eth_call` to a block.

```json
"blocks": { "reorgPeriod": 2, "estimateBlockTime": 20 }
```

- The **effective depth is `max(reorgPeriod, confirmationBlockDelay)`.** Reading
  shallower than the indexing delay does not read fresher state — it fails to
  find state that exists, because the provider's index has not caught up.
- **A block-tag reorg period is rejected.** `"finalized"` and friends are
  meaningful on post-merge Ethereum; on Praos there is nothing to point them at,
  so the agent errors rather than silently substituting a block count.
- Each block is ~20s, so the setting is a direct latency cost on every message.

Choose the number from measured rollback depth rather than from a guess, and
leave headroom over the worst you have observed. Tightening later is easy;
loosening after an incident is not.

#### Rollback detection

Two independent mechanisms:

- **Root mismatch** (chain-agnostic, inherited from the validator): the agent
  rebuilds the tree from indexed dispatches and compares it with the checkpoint
  read from chain. A disagreement is reported and the validator crash-loops
  deliberately. This only fires at checkpoint time, and only for rollbacks that
  move the merkle root.
- **Direct detection** (Cardano-specific): the indexer remembers the newest
  block it actually read data from and re-checks, on the following scan, that
  the chain still has that block at that height. A different hash, or a height
  that has fallen off the end, is reported as a rollback. This catches rollbacks
  the root check cannot see — a reverted gas payment or delivery leaves the root
  untouched.

Blockfrost offers no chain-sync subscription, so there is no rollback signal to
subscribe to; asking is the only option. Anchoring only on blocks that carried
transactions keeps that to zero extra requests while the chain is idle. A
provider that cannot answer is deliberately **not** reported as a rollback —
this log is meant to stop an operator, and one that fires on API hiccups would
teach them to scroll past it.

### Monitoring

Prometheus metrics on configured port (default 9091):

- `hyperlane_latest_checkpoint` - Latest checkpoint index
- `hyperlane_backfill_complete` - Historical backfill status
- `hyperlane_reached_initial_consistency` - Initial sync status

### Troubleshooting

**Rate limiting**: Blockfrost free tier allows 10 requests/second. Increase `interval` or reduce `maxSignConcurrency`.

**Reorg handling**: see [Reorgs and settlement](#reorgs-and-settlement) below. If a
reorg is reported the validator panics with a detailed error. Do NOT force
restart — investigate first.

**"Mailbox not deployed"**: Deploy mailbox first via CLI.

**"Invalid validator key format"**: Key must be 32 bytes (64 hex chars) with `0x` prefix.

### Network-Specific Settings

| Network | Domain ID | Blockfrost URL |
|---------|-----------|----------------|
| Preview | 2003 | https://cardano-preview.blockfrost.io/api/v0 |
| Preprod | 2002 | https://cardano-preprod.blockfrost.io/api/v0 |
| Mainnet | 2001 | https://cardano-mainnet.blockfrost.io/api/v0 |

---

## 9. Sepolia E2E Testing

This section provides step-by-step instructions for deploying Hyperlane warp route infrastructure on Ethereum Sepolia testnet for E2E testing with Cardano.

### Prerequisites

1. **Foundry** installed: `curl -L https://foundry.paradigm.xyz | bash && foundryup`
2. **Test ETH** from [Ethereum Sepolia Faucet](https://sepoliafaucet.com) (at least 1 ETH)
3. **Base environment variables**:

```bash
export EVM_RPC_URL="https://sepolia.drpc.org"
export EVM_SIGNER_KEY="0x..."
export EVM_MAILBOX="0xfFAEF09B3cd11D9b20d1a19bECca54EEC2884766"
export CARDANO_DOMAIN=2003
export EVM_DOMAIN=11155111
```

### Deployment Flow

```
Step 1: Deploy ISM ----------------> EVM_ISM
Step 2: Deploy Warp Routes --------> EVM_SYNTHETIC_*, EVM_COLLATERAL_*, etc.
Step 3: Set ISM on Routes
Step 4: Mint Test Tokens
Step 5: Pre-deposit Collateral
Step 6: Enroll Cardano Routers <---- CARDANO_NATIVE_ADA, CARDANO_COLLATERAL_*, etc.
```

### Step 1: Deploy Cardano MultisigISM on Sepolia

The ISM validates messages from Cardano. Get the Cardano validator's EVM address:

```bash
# Derive from validator ECDSA key
export CARDANO_VALIDATOR=$(cast wallet address --private-key $CARDANO_VALIDATOR_KEY)

# Deploy
cd solidity
forge script script/warp-e2e/DeployCardanoISM.s.sol:DeployCardanoISM \
  --rpc-url $EVM_RPC_URL --broadcast --private-key $EVM_SIGNER_KEY

export EVM_ISM="0x..."  # from output
```

### Step 2: Deploy Sepolia Warp Routes

```bash
cd solidity
forge script script/warp-e2e/DeploySepoliaWarp.s.sol:DeploySepoliaWarp \
  --rpc-url $EVM_RPC_URL --broadcast --private-key $EVM_SIGNER_KEY
```

Deploys: TestERC20s (FTEST, WADA, TOKA), HypERC20 synthetics (wCTEST, wADA), HypERC20Collateral (FTEST, WADA), HypNative (ETH).

Save all output addresses as environment variables.

### Step 3: Set ISM on Warp Routes

```bash
cd solidity
forge script script/warp-e2e/DeployCardanoISM.s.sol:DeployCardanoISM \
  --sig "setISMOnWarpRoutes()" \
  --rpc-url $EVM_RPC_URL --broadcast --private-key $EVM_SIGNER_KEY
```

### Step 4: Mint Test Tokens

```bash
cd solidity
forge script script/warp-e2e/DeploySepoliaWarp.s.sol:DeploySepoliaWarp \
  --sig "mintTestTokens()" \
  --rpc-url $EVM_RPC_URL --broadcast --private-key $EVM_SIGNER_KEY
```

### Step 5: Pre-deposit Collateral

For collateral routes that release tokens (e.g., WADA when receiving ADA from Cardano):

```bash
cd solidity
forge script script/warp-e2e/DeploySepoliaWarp.s.sol:DeploySepoliaWarp \
  --sig "preDepositCollateral()" \
  --rpc-url $EVM_RPC_URL --broadcast --private-key $EVM_SIGNER_KEY
```

### Step 6: Enroll Cardano Routers on Sepolia

Get Cardano warp route addresses from deployment (format: `0x01000000{nft_policy_id}`):

```bash
export CARDANO_NATIVE_ADA="0x01000000..."
export CARDANO_COLLATERAL_CTEST="0x01000000..."
export CARDANO_SYNTHETIC_FTEST="0x01000000..."

cd solidity
forge script script/warp-e2e/EnrollCardanoRouters.s.sol:EnrollCardanoRouters \
  --rpc-url $EVM_RPC_URL --broadcast --private-key $EVM_SIGNER_KEY
```

### Test Scenarios

| Scenario | Cardano Route    | Sepolia Route       | Direction      | Token Flow               |
|----------|------------------|------------------|----------------|--------------------------|
| 1        | Collateral CTEST | Synthetic wCTEST | Cardano -> Sepolia | Lock CTEST -> Mint wCTEST |
| 2        | Synthetic wFTEST | Collateral FTEST | Sepolia -> Cardano | Lock FTEST -> Mint wFTEST |
| 3        | Native ADA       | Synthetic wADA   | Cardano -> Sepolia | Lock ADA -> Mint wADA     |
| 4        | Synthetic wETH  | Native ETH      | Sepolia -> Cardano | Lock ETH -> Mint wETH   |
| 5        | Native ADA       | Collateral WADA  | Cardano -> Sepolia | Lock ADA -> Release WADA  |

### Test Transfer (Sepolia -> Cardano)

```bash
# Approve
cast send $EVM_FTEST "approve(address,uint256)" $EVM_COLLATERAL_FTEST "1000000000000000000000" \
  --rpc-url $EVM_RPC_URL --private-key $EVM_SIGNER_KEY

# Transfer
cast send $EVM_COLLATERAL_FTEST "transferRemote(uint32,bytes32,uint256)" \
  $CARDANO_DOMAIN $CARDANO_RECIPIENT "5000000000000000000" \
  --rpc-url $EVM_RPC_URL --private-key $EVM_SIGNER_KEY
```

### Verification

```bash
# Check ISM on warp route
cast call $EVM_SYNTHETIC_WCTEST "interchainSecurityModule()(address)" --rpc-url $EVM_RPC_URL

# Check enrolled router
cast call $EVM_SYNTHETIC_WCTEST "routers(uint32)(bytes32)" $CARDANO_DOMAIN --rpc-url $EVM_RPC_URL

# Check token balance
cast call $EVM_FTEST "balanceOf(address)(uint256)" $WALLET --rpc-url $EVM_RPC_URL
```

### Troubleshooting

- **"Environment variable not set"**: Use `export` (not just assignment)
- **"Execution reverted"**: Check ISM is set, router is enrolled, tokens are approved
- **"Insufficient balance"**: Pre-deposit more tokens to collateral contracts
- **Message not delivered**: Verify relayer is running, check Hyperlane Explorer

---

## 10. Known Limitations

### Contract Upgradeability

Cardano contracts are **not upgradeable** after deployment. Any code change to a validator results in a different script hash (different address). The state UTXO locked at the old address cannot be spent by the new validator.

```
Validator Code -> Script Hash -> Address
     | (any change)
New Validator Code -> Different Script Hash -> Different Address
-> Cannot spend old UTXO with new validator
```

**What is preserved across deployments:**

| Component                       | Stability     | Notes                                |
|---------------------------------|---------------|--------------------------------------|
| `mailbox_policy_id` (state NFT) | Fixed at init | Determined by seed UTXO              |
| `mailbox_script_hash`           | **Changes**   | Hash of validator code               |
| Mailbox address                 | **Changes**   | Derived from script hash             |
| Merkle tree state               | **Lost**      | Locked at old address                |
| SMT (replay protection)         | **Lost**      | Locked at old address                |
| Pending messages                | **Orphaned**  | Cannot be relayed                    |

**Implications:**

1. Thorough testing before mainnet is critical
2. New deployment = new identity (all connected chains must reconfigure)
3. Message continuity breaks on redeployment

**Potential future solutions:** migration redeemer, proxy pattern, governance mechanism.

### UTXO Contention (Sequential Message Processing)

#### Incoming Messages

Processing a message requires spending multiple UTXOs (mailbox, ISM, recipient) in a single transaction. Each UTXO can only be spent once per block:

```
Block N:   Message 1 spends Mailbox v1 -> creates Mailbox v2
Block N+1: Message 2 spends Mailbox v2 -> creates Mailbox v3
```

Maximum throughput: ~1 message per block (~3 messages/minute). Messages to different recipients are still sequential due to mailbox contention.

#### Outgoing Messages

Dispatch has similar contention: each dispatch increments the nonce and updates the merkle tree.

#### Mitigation

- Current: retry with backoff, sequential processing with queue management
- Future: convert Mailbox and ISM to minting policies (see [Section 12](#12-future-optimizations))

The limitation becomes relevant when sustained volume exceeds ~100/hour or low-latency delivery is required. For most initial deployments, it is acceptable.

---

## 11. Integration Status

### Component Status

| Component                               | Status         | Notes                                        |
|-----------------------------------------|----------------|----------------------------------------------|
| Incoming Messages (Other -> Cardano)    | Tested         | End-to-end working                           |
| Outgoing Messages (Cardano -> Other)    | Tested         | Validator + relayer delivering               |
| Multisig ISM                            | Complete       | ECDSA secp256k1 verified                     |
| Validator Agent                         | Tested         | Signing checkpoints, storing in S3           |
| Warp Routes (Native, Collateral, Synth) | Tested         | All 6 directions verified                    |
| Interchain Gas Paymaster                | Implemented    | Contract, indexer, and relayer integration   |
| Per-recipient ISM                       | Implemented    | Relayer reads ISM from WarpRouteDatum        |
| NFT Policy Addressing                   | Complete       | O(1) lookups, no registry needed             |

### What's Implemented

**On-Chain (Aiken):**
- Mailbox: full dispatch and process, merkle tree, continuation UTXOs
- Multisig ISM: ECDSA secp256k1 verification (CIP-49), per-origin validator sets
- Verified Message NFTs: message authentication for generic recipients
- Sparse Merkle Tree (SMT): on-chain replay protection in mailbox datum
- Warp Routes: all three types (native, collateral, synthetic)
- IGP: gas oracle, pay-for-gas, claim
- Validator Announce: on-chain announcements

**Off-Chain (Rust):**
- Mailbox: `process()`, `delivered()`, full indexer
- Multisig ISM: `MultisigIsm` trait implementation
- Recipient Resolver: O(1) NFT-based lookups
- Transaction Builder: UTXO selection, reference scripts, fee calculation
- Validator Agent: checkpoint signing, S3 storage

### What's Missing

1. **On-chain Custom ISM Enforcement**: Currently relayer-side only
2. **Scraper support**: no Cardano implementation, so no explorer/analytics indexing
3. **Metrics**: the chain crate exposes almost none; rollback detection reports via logs only

### Recommended Next Steps

**High Priority (Production):**
1. Security audit (Aiken contracts, signature verification, merkle tree)
2. Provider abstraction — Blockfrost is currently the sole source of truth for
   an agent that signs checkpoints, with no fallback and no cross-check
3. Choose `reorgPeriod` from measured rollback depth (see
   [Reorgs and settlement](#reorgs-and-settlement))

**Medium Priority (Hardening):**
4. Monitoring and observability
5. On-chain per-recipient ISM enforcement
6. Key management for relayer/validator signing keys

**Low Priority (Optimization):**
7. UTXO contention mitigation via minting policies

---

## 12. Future Optimizations

### Parallel Message Processing (Minting Policy Architecture)

**Status:** Design complete, not implemented.

The current architecture creates UTXO contention limiting throughput to ~1 message per block. A future optimization would convert the Mailbox and ISM from spend validators (requiring UTXO spending) to minting policies (requiring only token minting). This eliminates contention on mailbox and ISM UTXOs, as minting policies run without spending any UTXO:

| Scenario | Current | Optimized |
|----------|---------|-----------|
| 10 messages to 10 recipients | ~3.3 min | ~20 sec |
| 10 messages to 1 recipient | ~3.3 min | ~3.3 min |

Contention would remain only on recipient UTXOs (unavoidable -- state must update).

> **Note**: True parallel inbound processing (N messages per block to the same recipient) remains limited by recipient UTXO contention, since the recipient state must update sequentially.

### Dispatch Batching

Batch multiple outgoing dispatches into a single transaction to reduce mailbox UTXO contention. Less critical than incoming optimization since dispatch is user-initiated.

### Reference Script Caching

In-memory cache with TTL for reference scripts, invalidated on UTXO consumption. Avoids repeated Blockfrost fetches.

### Parallel Blockfrost Queries

Use `tokio::try_join!` for independent queries that currently run sequentially.

### Future Features

These are part of the Hyperlane specification but not required for initial Cardano integration:

| Feature | Priority | Description |
|---------|----------|-------------|
| Routing ISM | Low | Different ISMs per origin domain |
| Aggregation ISM | Low | Multiple ISMs must verify (AND logic) |
| Interchain Accounts (ICA) | Medium | Cross-chain account control |
| Interchain Query System (IQS) | Low | Remote state queries |
| Warp Route Rate Limiting | Medium | Transfer limits and circuit breakers for mainnet |

---

_Last Updated: July 2026_

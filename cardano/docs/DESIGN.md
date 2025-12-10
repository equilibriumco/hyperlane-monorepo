# Hyperlane-Cardano Design Document

This document describes the architectural design of the Hyperlane-Cardano integration, including message flows, contract interactions, and key design patterns specific to Cardano's eUTXO model.

## Table of Contents

1. [Overview](#overview)
2. [Incoming Message Flow (Other Chains → Cardano)](#incoming-message-flow-other-chains--cardano)
3. [Outgoing Message Flow (Cardano → Other Chains)](#outgoing-message-flow-cardano--other-chains)
4. [NFT Usage Patterns](#nft-usage-patterns)
5. [Contract Architecture](#contract-architecture)
6. [Transaction Structure](#transaction-structure)

---

## Overview

Hyperlane is an interchain messaging protocol that enables applications to send arbitrary messages between blockchains. The Cardano integration adapts the Hyperlane protocol to work with Cardano's eUTXO model, which differs significantly from the account-based models of EVM chains.

### Key Design Principles

1. **Relayer-driven**: The Hyperlane relayer constructs and submits all Cardano transactions
2. **eUTXO-compatible**: All state is managed through UTXOs with inline datums
3. **NFT-based identity**: State UTXOs are identified by unique NFTs rather than addresses
4. **Reference scripts**: Validators are stored as reference scripts to minimize transaction size

---

## Incoming Message Flow (Other Chains → Cardano)

This flow describes how a message dispatched from another chain (e.g., Fuji/Avalanche) is delivered to a recipient on Cardano.

### High-Level Flow

```mermaid
sequenceDiagram
    participant User as User/App
    participant SrcMailbox as Source Mailbox<br/>(e.g., Fuji)
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
    CardanoMailbox->>Recipient: Invoke recipient (spent in same tx)
    Recipient->>Recipient: Handle message
    CardanoMailbox->>CardanoMailbox: Mint processed message NFT
    deactivate CardanoMailbox

    Note over Recipient: Message delivered!
```

### Detailed Transaction Flow

```mermaid
flowchart TB
    subgraph Inputs["Transaction Inputs"]
        MI[/"Mailbox UTXO<br/>with State NFT"/]
        II[/"ISM UTXO<br/>with State NFT"/]
        RI[/"Recipient UTXO<br/>with State NFT"/]
        CI[/"Collateral UTXO<br/>(for fees)"/]
    end

    subgraph RefInputs["Reference Inputs"]
        RegRef[/"Registry UTXO<br/>(lookup recipient config)"/]
        MBScript[/"Mailbox Reference Script"/]
        ISMScript[/"ISM Reference Script"/]
        RecScript[/"Recipient Reference Script"/]
    end

    subgraph Redeemers["Redeemers"]
        MR["Mailbox: Process{<br/>message, metadata, message_id}"]
        IR["ISM: Verify{<br/>checkpoint, signatures}"]
        RR["Recipient: HandleMessage{<br/>origin, sender, body}"]
    end

    subgraph Outputs["Transaction Outputs"]
        MO[/"Mailbox UTXO<br/>(unchanged datum)"/]
        IO[/"ISM UTXO<br/>(unchanged datum)"/]
        RO[/"Recipient UTXO<br/>(updated state)"/]
        PMO[/"Processed Message UTXO<br/>(new NFT minted)"/]
    end

    subgraph Minting["Minting"]
        PMNFT["Processed Message NFT<br/>(message_id as asset name)"]
    end

    MI --> MR
    II --> IR
    RI --> RR

    MR --> MO
    IR --> IO
    RR --> RO

    PMNFT --> PMO

    RegRef -.-> MR
    MBScript -.-> MI
    ISMScript -.-> II
    RecScript -.-> RI
```

### Signature Verification Detail

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
        THRESHOLD["5. Count valid ≥ threshold"]
    end

    CP --> DH
    DH --> DIGEST
    DIGEST --> ETH
    ETH --> VERIFY
    SIGS --> VERIFY
    VERIFY --> THRESHOLD
```

---

## Outgoing Message Flow (Cardano → Other Chains)

This flow describes how a message dispatched from Cardano reaches a recipient on another chain. **Note: This flow is implemented in contracts but not yet fully integrated with the validator agent.**

### High-Level Flow

```mermaid
sequenceDiagram
    participant User as User/App
    participant CardanoMailbox as Cardano Mailbox
    participant CardanoIndexer as Cardano Indexer<br/>(in Relayer)
    participant Validators as Hyperlane Validators<br/>(for Cardano)
    participant Storage as Checkpoint Storage<br/>(S3/GCS)
    participant Relayer as Hyperlane Relayer
    participant DestMailbox as Destination Mailbox<br/>(e.g., Fuji)
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

### Dispatch Transaction Structure

```mermaid
flowchart TB
    subgraph Inputs["Transaction Inputs"]
        MI[/"Mailbox UTXO<br/>with State NFT<br/>datum: {nonce: N, merkleRoot, ...}"/]
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
        V3["3. Insert hash into merkle tree"]
        V4["4. Verify continuation datum:<br/>- nonce = N + 1<br/>- merkle_root updated<br/>- merkle_count incremented"]
    end

    subgraph Outputs["Transaction Outputs"]
        MO[/"Mailbox UTXO<br/>datum: {nonce: N+1, newMerkleRoot, ...}"/]
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

### Merkle Tree Updates

```mermaid
flowchart LR
    subgraph Before["Before Dispatch"]
        T1["Merkle Tree<br/>count: 5<br/>root: 0xabc..."]
    end

    subgraph Message["New Message"]
        MSG["Message {<br/>version: 3<br/>nonce: 5<br/>origin: 2003<br/>sender: 0x...<br/>destination: 43113<br/>recipient: 0x...<br/>body: ...}"]
        HASH["message_hash =<br/>keccak256(encode(message))"]
    end

    subgraph After["After Dispatch"]
        T2["Merkle Tree<br/>count: 6<br/>root: 0xdef..."]
    end

    T1 --> MSG
    MSG --> HASH
    HASH --> T2
```

---

## NFT Usage Patterns

Cardano's eUTXO model requires different patterns than account-based chains. We use NFTs extensively to solve several challenges:

### 1. State UTXO Identification

**Problem**: UTXOs at a script address are not uniquely identifiable by address alone.

**Solution**: Each contract's state UTXO contains a unique "state NFT" that identifies it. This combines two well-known Cardano patterns:

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
| Registry | Identifies the recipient registry UTXO |
| Warp Route | Identifies each warp route's state UTXO |
| Vault | Identifies token vault UTXOs |
| Recipients | Each registered recipient has a state NFT |

### 2. Replay Protection (Processed Message NFTs)

**Problem**: Prevent the same message from being processed twice.

**Solution**: Mint a unique NFT for each processed message, using the message ID as the asset name.

```mermaid
flowchart TB
    subgraph Process["Message Processing"]
        MSG["Incoming Message<br/>message_id: 0x123..."]
        CHECK["Check: Does NFT exist<br/>with name = message_id?"]
        MINT["Mint Processed Message NFT<br/>name: 0x123..."]
        STORE["Store in UTXO at<br/>any address"]
    end

    subgraph Replay["Replay Attempt"]
        MSG2["Same Message<br/>message_id: 0x123..."]
        CHECK2["Check: Does NFT exist<br/>in reference inputs?"]
        FOUND["NFT EXISTS!"]
        REJECT["Reject: Already processed"]
    end

    MSG --> CHECK
    CHECK -->|"Not found"| MINT
    MINT --> STORE

    MSG2 --> CHECK2
    CHECK2 --> FOUND
    FOUND --> REJECT

    STORE -.->|"NFT now exists"| FOUND
```

**Benefits:**
- O(1) lookup via Blockfrost asset query
- Immutable proof of processing
- No state bloat in mailbox datum
- **Upgrade-safe**: Policy ID remains stable across mailbox upgrades (see below)

**Why parameterized by `mailbox_policy_id` (not `mailbox_script_hash`)?**

The `processed_message_nft` minting policy is parameterized by `mailbox_policy_id` (the one-shot NFT policy that identifies the mailbox state UTXO) rather than `mailbox_script_hash`. This is critical for upgrade safety:

| Parameter | Stability | Effect on Replay Protection |
|-----------|-----------|----------------------------|
| `mailbox_script_hash` | Changes with every code update | ❌ Old NFTs under different policy, replay possible |
| `mailbox_policy_id` | Fixed at initialization | ✅ Same policy forever, replay protection maintained |

The `mailbox_policy_id` is determined once during mailbox initialization and never changes. This ensures that all processed message NFTs, regardless of when they were minted, are under the same policy and can be found during replay checks.

### 3. Message Authentication (Stored Message NFTs)

**Problem**: In deferred processing, prove that a stored message came from the mailbox.

**Solution**: The mailbox mints a "stored message NFT" that only it can create. The recipient verifies this NFT exists when processing.

```mermaid
flowchart TB
    subgraph Phase1["Phase 1: Relayer Stores Message"]
        MB["Mailbox validates message"]
        MINT["Mint Stored Message NFT<br/>(only mailbox can mint)"]
        STORE["Create Stored Message UTXO<br/>with NFT + message datum"]
    end

    subgraph Phase2["Phase 2: User/Anyone Processes"]
        READ["Read Stored Message UTXO"]
        VERIFY["Verify Stored Message NFT present<br/>(proves mailbox created it)"]
        PROCESS["Process message in recipient"]
        BURN["Burn Stored Message NFT"]
    end

    MB --> MINT
    MINT --> STORE
    STORE --> READ
    READ --> VERIFY
    VERIFY --> PROCESS
    PROCESS --> BURN
```

**Security Properties:**
- Only mailbox can mint the NFT (parameterized minting policy)
- NFT presence proves message authenticity
- Burning prevents double-processing

### 4. Recipient Registration Ownership

**Problem**: Prove ownership of a recipient script when registering/modifying.

**Solution**: The script must be spent in the same transaction as the registry update.

```mermaid
flowchart TB
    subgraph Registration["Recipient Registration"]
        REG["Registry UTXO"]
        REC["Recipient UTXO<br/>(with state NFT)"]
        SPEND["Both spent in same tx"]
        PROOF["Spending recipient = proof of ownership"]
    end

    subgraph Update["Registration Update"]
        OWNER["Owner signature required<br/>(stored in registration)"]
        OR["OR"]
        SCRIPT["Spend recipient script"]
    end

    REG --> SPEND
    REC --> SPEND
    SPEND --> PROOF

    OWNER --> OR
    SCRIPT --> OR
```

### NFT Summary Table

| NFT Type | Purpose | Minting Policy | When Minted | When Burned |
|----------|---------|----------------|-------------|-------------|
| State NFT | Identify state UTXOs | One-shot (tied to UTXO) | Contract deployment | Never |
| Processed Message NFT | Replay protection | Mailbox-controlled | Message processing | Never |
| Stored Message NFT | Message authentication | Mailbox-controlled | Deferred store | Deferred process |
| Synthetic Token | Bridged token representation | Warp route-controlled | Token receive | Token send |

---

## Contract Architecture

### Cross-Contract Coordination in eUTXO

Unlike account-based chains (EVM, Solana), Cardano does not support cross-contract calls. A contract cannot "invoke" another contract directly. Instead, all contracts in a transaction validate **independently and simultaneously** against the same transaction context.

We achieve cross-contract coordination through a pattern we call **"mutual spending validation"**: each contract checks that the other contracts it depends on are being spent in the same transaction.

```mermaid
flowchart TB
    subgraph Transaction["Single Atomic Transaction"]
        subgraph Inputs["Inputs (all spent together)"]
            MB["Mailbox UTXO"]
            ISM["ISM UTXO"]
            REC["Recipient UTXO"]
        end

        subgraph Validators["Validators (run independently)"]
            MBV["Mailbox Validator<br/>✓ ISM is spent<br/>✓ Recipient is spent<br/>✓ Processed NFT minted"]
            ISMV["ISM Validator<br/>✓ Signatures valid<br/>✓ Threshold met"]
            RECV["Recipient Validator<br/>✓ Mailbox is spent<br/>✓ Message matches redeemer"]
        end

        subgraph Result["All Must Pass"]
            OK["Transaction Valid ✓"]
        end
    end

    MB --> MBV
    ISM --> ISMV
    REC --> RECV

    MBV --> OK
    ISMV --> OK
    RECV --> OK
```

#### How Each Contract Ensures Correctness

**Mailbox ensures the TRUSTED ISM verifies THIS specific message:**

The mailbox performs two critical checks:
1. **Trust check**: Only an ISM with the exact script hash stored in `datum.default_ism` is accepted
2. **Message binding**: The ISM's redeemer must contain a checkpoint for THIS specific `message_id`

```aiken
// In mailbox.ak - verify_ism_for_message()
// 1. Get trusted ISM hash from mailbox datum (or recipient's custom ISM)
let ism_hash = get_recipient_ism(message.recipient, datum.default_ism, tx)

// 2. Find an input with that EXACT script hash (trust check)
// 3. Look up its redeemer from tx.redeemers
// 4. Parse it as MultisigIsmRedeemer
// 5. Verify checkpoint.message_id == expected_message_id (message binding)

list.any(tx.inputs, fn(input) {
    when input.output.address.payment_credential is {
        Script(hash) ->
            // CRITICAL: Only accept ISM with the exact trusted script hash
            if hash == ism_hash {
                verify_ism_redeemer_message_id(input.output_reference, message_id, tx)
            } else {
                False
            }
        _ -> False
    }
})
```

This prevents two attack vectors:
- **Untrusted ISM**: An attacker cannot use an arbitrary ISM contract
- **Signature replay**: An attacker cannot reuse valid signatures from a different message

**Recipient ensures message came from mailbox:**

The recipient checks that the mailbox UTXO (identified by its state NFT) is being spent in the same transaction. Since the mailbox validates the message and verifies the ISM redeemer, the recipient trusts the message is authentic.

```aiken
// In recipient - mailbox_is_caller()
let mailbox_spent = list.any(tx.inputs, fn(input) {
    // Check if input contains the mailbox state NFT
    let policy_tokens = tokens(input.output.value, mailbox_policy_id)
    !dict.is_empty(policy_tokens)
})
```

**Why this works:**
1. All validators in a transaction run against the **same transaction context**
2. If ANY validator fails, the **entire transaction fails**
3. By checking that required contracts are spent, we ensure their validators run
4. The mailbox **inspects the ISM's redeemer** to verify it's for the correct message
5. Each validator enforces its own invariants (ISM checks signatures, mailbox checks message binding, recipient checks mailbox presence)

This is fundamentally different from EVM where:
- Contract A calls Contract B, passing control flow
- Contract B can modify state and return values to A
- Execution is sequential

In Cardano:
- All contracts validate the same transaction simultaneously
- No control flow between contracts
- Coordination via "I see you're being spent, so I know your rules passed"

### Contract Dependency Graph

The arrows below represent "checks that X is spent" relationships, not invocations:

```mermaid
flowchart TB
    subgraph Core["Core Contracts"]
        MB["Mailbox<br/>dispatch() / process()"]
        ISM["Multisig ISM<br/>verify()"]
        REG["Registry<br/>register() / lookup()"]
    end

    subgraph Tokens["Token Contracts"]
        WR["Warp Route<br/>transferRemote() / receiveTransfer()"]
        VAULT["Vault<br/>lock() / release()"]
        SYNTH["Synthetic Token<br/>mint() / burn()"]
    end

    subgraph Recipients["Recipient Contracts"]
        GEN["Generic Recipient<br/>handleMessage()"]
        DEF["Deferred Recipient<br/>storeMessage() / processMessage()"]
    end

    subgraph NFTs["NFT Policies"]
        STATE["State NFT"]
        PROC["Processed Message NFT"]
        STORED["Stored Message NFT"]
    end

    MB -->|"verifies"| ISM
    MB -->|"looks up"| REG
    MB -->|"invokes"| GEN
    MB -->|"invokes"| DEF
    MB -->|"invokes"| WR

    WR -->|"uses"| VAULT
    WR -->|"mints/burns"| SYNTH

    MB -->|"mints"| PROC
    MB -->|"mints"| STORED
    DEF -->|"burns"| STORED

    STATE -.->|"identifies"| MB
    STATE -.->|"identifies"| ISM
    STATE -.->|"identifies"| REG
    STATE -.->|"identifies"| WR
```

### Recipient Types

```mermaid
flowchart TB
    subgraph Types["Recipient Types"]
        GEN["Generic<br/>Simple state update"]
        TOK["TokenReceiver<br/>Releases/mints tokens"]
        DEF["Deferred<br/>Two-phase processing"]
    end

    subgraph Generic["Generic Flow"]
        G1["Relayer builds full tx"]
        G2["State in → State out"]
        G3["No extra outputs"]
    end

    subgraph Token["TokenReceiver Flow"]
        T1["Relayer decodes body"]
        T2["Releases tokens from vault<br/>OR mints synthetic"]
        T3["Sends to recipient address"]
    end

    subgraph Deferred["Deferred Flow"]
        D1["Phase 1: Store message"]
        D2["Mint message NFT"]
        D3["Phase 2: Process later"]
        D4["Burn message NFT"]
    end

    GEN --> G1 --> G2 --> G3
    TOK --> T1 --> T2 --> T3
    DEF --> D1 --> D2 --> D3 --> D4
```

---

## Transaction Structure

### Reference Script Usage

To minimize transaction size, validator scripts are stored as reference scripts:

```mermaid
flowchart TB
    subgraph Deployment["Deployment (One-time)"]
        SCRIPT["Validator Script<br/>(~10-50 KB)"]
        REFUTXO["Reference Script UTXO<br/>at always-fails address"]
    end

    subgraph Usage["Every Transaction"]
        TX["Transaction"]
        REFINPUT["Reference Input<br/>(points to REFUTXO)"]
        WITNESS["Witness Set<br/>(NO script bytes)"]
    end

    SCRIPT -->|"stored in"| REFUTXO
    REFUTXO -->|"referenced by"| REFINPUT
    REFINPUT -->|"included in"| TX
    TX -->|"minimal"| WITNESS
```

## Appendix: Domain and Address Encoding

### Hyperlane Address Format

Cardano script hashes (28 bytes) are padded to 32 bytes for Hyperlane compatibility:

```
Cardano Script Hash: 0x1234567890abcdef... (28 bytes)
Hyperlane Address:   0x020000001234567890abcdef... (32 bytes)
                       ^^^^^^^^
                       Prefix: 0x02000000 = Script credential
```

### Domain IDs

| Chain | Domain ID |
|-------|-----------|
| Cardano Preview | 2003 |
| Fuji (Avalanche) | 43113 |
| Ethereum Mainnet | 1 |
| Ethereum Sepolia | 11155111 |

---

*Last Updated: December 2024*

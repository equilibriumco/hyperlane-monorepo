# Hyperlane Cross-Chain Security Model

This document explains how Hyperlane guarantees that a message dispatched on an origin chain (e.g., Fuji/Avalanche) is delivered to the recipient contract on the destination chain (e.g., Cardano) with cryptographic integrity.

## Overview

The security model relies on a chain of cryptographic commitments:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           MESSAGE INTEGRITY CHAIN                           │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  [1] Application dispatches message on Origin Chain                         │
│       ↓                                                                     │
│  [2] Message ID = keccak256(message_bytes)                                  │
│       ↓                                                                     │
│  [3] Message ID inserted into Merkle Tree                                   │
│       ↓                                                                     │
│  [4] Validators sign Checkpoint (merkle_root at index N)                    │
│       ↓                                                                     │
│  [5] Relayer collects signatures + builds metadata                          │
│       ↓                                                                     │
│  [6] Destination ISM verifies signatures on-chain                           │
│       ↓                                                                     │
│  [7] Mailbox verifies message_id matches & delivers to recipient            │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

## Component Details

### 1. Message Structure

Every Hyperlane message has a fixed binary format:

```
┌──────────┬───────┬────────┬────────┬─────────────┬───────────┬──────────┐
│ VERSION  │ NONCE │ ORIGIN │ SENDER │ DESTINATION │ RECIPIENT │   BODY   │
│ (1 byte) │(4 b)  │ (4 b)  │ (32 b) │   (4 b)     │  (32 b)   │(variable)│
└──────────┴───────┴────────┴────────┴─────────────┴───────────┴──────────┘
                    Total header: 77 bytes
```

**Fields:**
- **version**: Protocol version (currently 3)
- **nonce**: Unique counter per origin mailbox (prevents replay)
- **origin**: Source chain domain ID (e.g., 43113 for Fuji)
- **sender**: 32-byte padded address of the caller on origin chain
- **destination**: Target chain domain ID (e.g., 2002 for Cardano Preview)
- **recipient**: 32-byte address of recipient contract on destination
- **body**: Application-specific payload

### 2. Message ID Computation

The **Message ID** is the cryptographic fingerprint of the message:

```
message_id = keccak256(version || nonce || origin || sender || destination || recipient || body)
```

This 32-byte hash is:
- **Deterministic**: Same message always produces same ID
- **Collision-resistant**: Practically impossible to find two messages with same ID
- **Tamper-evident**: Any byte change produces completely different ID

### 3. Merkle Tree Accumulator

The origin chain's **MerkleTreeHook** maintains a sparse Merkle tree of all dispatched message IDs:

```
                    Root (checkpoint)
                   /                \
                  /                  \
           Hash(0,1)              Hash(2,3)
           /      \               /      \
        msg_0    msg_1        msg_2    msg_3
```

- Each message ID is inserted as a leaf
- Tree depth: 32 (supports 2^32 messages)
- After each insertion, the root is updated
- The root at any index commits to all messages up to that index

### 4. Validator Checkpoint Signing

Validators are trusted entities that monitor the origin chain and sign **Checkpoints**:

```
Checkpoint = {
    origin:            uint32   // Source domain ID
    merkleTreeHook:    bytes32  // Address of merkle hook contract
    root:              bytes32  // Merkle root at this checkpoint
    index:             uint32   // Message count at this checkpoint
    messageId:         bytes32  // Optional: specific message ID
}
```

#### Signing Process

**Step 1: Domain Hash** (prevents cross-chain replay)
```
domain_hash = keccak256(origin || merkle_tree_hook || "HYPERLANE")
```

**Step 2: Checkpoint Digest**
```
checkpoint_digest = keccak256(domain_hash || root || index || message_id)
```

**Step 3: EIP-191 Signed Message Hash**
```
final_hash = keccak256("\x19Ethereum Signed Message:\n32" || checkpoint_digest)
```

**Step 4: ECDSA Signature**
```
signature = ECDSA_sign(validator_private_key, final_hash)
```

The signature is 65 bytes: `r (32 bytes) || s (32 bytes) || v (1 byte)`

### 5. Relayer Metadata Construction

The relayer (untrusted) constructs metadata containing:

```
┌───────────────────┬────────────────┬────────────┬──────────────┬─────────────┬────────────┐
│ MERKLE_TREE_HOOK  │ LEAF_INDEX     │ MESSAGE_ID │ MERKLE_PROOF │ CKPT_INDEX  │ SIGNATURES │
│    (32 bytes)     │   (4 bytes)    │ (32 bytes) │  (32×depth)  │  (4 bytes)  │ (variable) │
└───────────────────┴────────────────┴────────────┴──────────────┴─────────────┴────────────┘
```

**Components:**
- **merkle_tree_hook**: Origin chain's merkle hook address
- **leaf_index**: Position of this message in the merkle tree
- **message_id**: The keccak256 hash of the message
- **merkle_proof**: Path from leaf to root (32 bytes × tree_depth)
- **checkpoint_index**: The checkpoint index used
- **signatures**: Concatenated validator signatures

### 6. Destination Chain Verification

#### 6.1 Mailbox Verification (Cardano)

The destination Mailbox validates:

1. **Message ID Match**
   ```
   computed_id = keccak256(message_bytes)
   assert(computed_id == provided_message_id)
   ```

2. **Destination Domain**
   ```
   assert(message.destination == local_domain)
   ```

3. **Not Already Processed**
   ```
   assert(!processed_messages.contains(message_id))
   ```

4. **ISM Verification Triggered**
   ```
   // ISM script must be present in transaction inputs
   assert(ism_input_present(tx))
   ```

#### 6.2 Multisig ISM Verification (Cardano)

The ISM verifies validator signatures on-chain:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                        ISM SIGNATURE VERIFICATION                           │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  1. Recompute checkpoint hash (must match what validators signed)           │
│     ┌─────────────────────────────────────────────────────────────────┐     │
│     │ domain_hash = keccak256(origin || merkle_hook || "HYPERLANE")   │     │
│     │ digest = keccak256(domain_hash || root || index || message_id)  │     │
│     │ hash = keccak256("\x19Ethereum Signed Message:\n32" || digest)  │     │
│     └─────────────────────────────────────────────────────────────────┘     │
│                                                                             │
│  2. For each signature:                                                     │
│     a. Verify ECDSA signature against the hash                              │
│     b. Derive Ethereum address from public key                              │
│        address = keccak256(uncompressed_pubkey)[12:32]                      │
│     c. Check address is in trusted validator set                            │
│                                                                             │
│  3. Count valid signatures ≥ threshold                                      │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

#### 6.3 Merkle Proof Verification

The ISM also verifies the message is included in the signed checkpoint:

```
// Verify: message_id at leaf_index produces checkpoint_root
computed_root = merkle_verify(message_id, leaf_index, proof)
assert(computed_root == checkpoint.root)
```

### 7. Processed Message Marker

After successful processing, an NFT or UTXO marker is created:

```
processed_message_marker = {
    script: processed_messages_policy_id,
    datum: message_id
}
```

This prevents replay attacks - any future attempt to process the same message will find the marker and fail.

## Security Properties

### Message Integrity
| Property | Guarantee |
|----------|-----------|
| Authenticity | Message content is exactly what was dispatched |
| Ordering | Nonce prevents out-of-order delivery |
| No Replay | Processed marker prevents re-delivery |
| No Tampering | keccak256 hash detects any modification |

### Validator Security
| Property | Guarantee |
|----------|-----------|
| Threshold | m-of-n validators must agree |
| Domain Separation | Signatures can't be replayed across chains |
| Key Binding | ECDSA ties signature to specific private key |
| Address Derivation | Deterministic from public key |

### Relayer Trust Model
| Aspect | Status |
|--------|--------|
| Can forge messages | ❌ No - needs valid signatures |
| Can modify messages | ❌ No - ID verification fails |
| Can delay messages | ⚠️ Yes - but can't prevent eventual delivery |
| Can censor messages | ⚠️ Temporarily - other relayers can deliver |

## Recipient Contract Protection

A critical security question: **Can someone directly call a recipient contract with a fake message?**

**Answer: NO** - The architecture prevents this through multiple layers:

### Layer 1: `onlyMailbox` Modifier (EVM)

On EVM chains, recipient contracts inherit from `MailboxClient` which provides the `onlyMailbox` modifier:

```solidity
// From MailboxClient.sol
modifier onlyMailbox() {
    require(
        msg.sender == address(mailbox),
        "MailboxClient: sender not mailbox"
    );
    _;
}
```

The `handle()` function uses this modifier:

```solidity
// From Router.sol
function handle(
    uint32 _origin,
    bytes32 _sender,
    bytes calldata _message
) external payable virtual override onlyMailbox {  // <-- PROTECTED
    bytes32 _router = _mustHaveRemoteRouter(_origin);
    require(_router == _sender, "Enrolled router does not match sender");
    _handle(_origin, _sender, _message);
}
```

### Layer 2: Remote Router Verification

Even if called by the Mailbox, the Router verifies the sender is a trusted remote router:

```solidity
bytes32 _router = _mustHaveRemoteRouter(_origin);
require(_router == _sender, "Enrolled router does not match sender");
```

This means:
1. The sender must be an enrolled router for the origin domain
2. Prevents impersonation even if an attacker could somehow bypass ISM

### Layer 3: Mailbox Process Flow

The Mailbox only calls recipients after full verification:

```solidity
// From Mailbox.sol - process()
function process(bytes calldata _metadata, bytes calldata _message) external payable {
    // 1. Check message is for this domain
    require(_message.destination() == localDomain, "Mailbox: unexpected destination");

    // 2. Check not already delivered
    bytes32 _id = _message.id();
    require(delivered(_id) == false, "Mailbox: already delivered");

    // 3. Get recipient's ISM and verify
    IInterchainSecurityModule ism = recipientIsm(recipient);
    require(ism.verify(_metadata, _message), "Mailbox: ISM verification failed");

    // 4. Only then call the recipient
    IMessageRecipient(recipient).handle{value: msg.value}(
        _message.origin(),
        _message.sender(),
        _message.body()
    );
}
```

### Cardano Recipient Protection (UTXO Model)

Cardano doesn't have cross-contract calls like EVM. Instead, **all scripts in a transaction execute independently and must ALL pass**. The security comes from **mutual verification** - each script checks that the others are present and valid.

#### How It Works: Transaction Composition

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                 CARDANO PROCESS TRANSACTION STRUCTURE                       │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  INPUTS (each triggers its validator):                                      │
│  ┌─────────────────────────────────────────────────────────────────┐        │
│  │  [1] Mailbox UTXO                                               │        │
│  │      → Validates: message_id, destination, ISM present,         │        │
│  │                   recipient called, marker created              │        │
│  │                                                                 │        │
│  │  [2] ISM UTXO                                                   │        │
│  │      → Validates: checkpoint signatures from trusted validators │        │
│  │                                                                 │        │
│  │  [3] Recipient UTXO (e.g., Warp Route)                          │        │
│  │      → Validates: mailbox is caller, sender is enrolled router  │        │
│  └─────────────────────────────────────────────────────────────────┘        │
│                                                                             │
│  OUTPUTS:                                                                   │
│  ┌─────────────────────────────────────────────────────────────────┐        │
│  │  [1] Mailbox continuation (datum unchanged)                     │        │
│  │  [2] ISM continuation                                           │        │
│  │  [3] Recipient continuation (tokens released/minted)            │        │
│  │  [4] Processed message marker (new UTXO with message_id)        │        │
│  │  [5] User receives tokens                                       │        │
│  └─────────────────────────────────────────────────────────────────┘        │
│                                                                             │
│  ALL validators execute. If ANY fails → entire TX rejected.                 │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

#### Mailbox Verifies Recipient Is Called

The Mailbox validator checks that the recipient script is being spent:

```aiken
// From mailbox.ak - validate_process()

// 5. Verify recipient is called (recipient UTXO must be spent)
expect recipient_called(message.recipient, tx)

fn recipient_called(recipient: HyperlaneAddress, tx: Transaction) -> Bool {
  // Extract script hash from recipient address
  expect Some(recipient_hash) = hyperlane_address_to_script_hash(recipient)
  // Check that an input from recipient script is spent
  has_script_input(tx, recipient_hash)
}
```

#### Recipient Verifies Mailbox Is Caller

The recipient (e.g., Warp Route) checks that the Mailbox UTXO is in the same transaction:

```aiken
// From warp_route.ak - validate_receive_transfer()

// 2. Verify mailbox is calling us (mailbox UTXO spent in same tx)
expect mailbox_is_caller(mailbox_policy_id, tx)

fn mailbox_is_caller(mailbox_policy_id: PolicyId, tx: Transaction) -> Bool {
  list.any(tx.inputs, fn(input) {
    // Check if any input contains the mailbox NFT
    quantity_of(input.output.value, mailbox_policy_id, "") > 0
  })
}
```

#### Remote Router Verification (Same as EVM)

The recipient also verifies the sender is an enrolled router:

```aiken
// From warp_route.ak - validate_receive_transfer()

// 1. Verify sender is an enrolled remote route
expect Some(expected_sender) = assoc_find(datum.config.remote_routes, origin)
expect sender == expected_sender
```

#### Why Direct Calls Fail

If an attacker tries to spend the Warp Route UTXO directly:

```
ATTACK TRANSACTION:
┌───────────────────────────────────────────────────────────────┐
│  INPUT: Warp Route UTXO with ReceiveTransfer redeemer         │
│         → Validator runs...                                   │
│         → Checks: mailbox_is_caller(mailbox_policy_id, tx)    │
│         → NO mailbox NFT in inputs!                           │
│         → ❌ FAILS                                             │
└───────────────────────────────────────────────────────────────┘
```

If attacker includes Mailbox but without valid ISM signatures:

```
ATTACK TRANSACTION:
┌───────────────────────────────────────────────────────────────┐
│  INPUT: Mailbox UTXO with Process redeemer                    │
│         → Validator runs...                                   │
│         → Checks: verify_ism_present(ism_hash, tx, metadata)  │
│         → ISM UTXO must be spent too                          │
│                                                               │
│  INPUT: ISM UTXO with Verify redeemer                         │
│         → Validator runs...                                   │
│         → Checks: validator signatures                        │
│         → NO valid signatures!                                │
│         → ❌ FAILS                                             │
└───────────────────────────────────────────────────────────────┘
```

#### The Binding Mechanism: NFT Identification

Scripts identify each other using **NFT markers** (unique tokens):

| Contract | Identified By |
|----------|---------------|
| Mailbox | `mailbox_policy_id` NFT |
| ISM | Script hash in datum |
| Warp Route | Parameterized with `mailbox_policy_id` |

The Warp Route is **compiled with** the mailbox policy ID, so it can only accept calls from that specific Mailbox. This is similar to EVM's immutable constructor parameters.

### ISM Selection (Cardano Limitation)

On EVM, each recipient can specify its own ISM via `interchainSecurityModule()`. On Cardano, **all recipients currently use the mailbox's default ISM**.

#### Why Cardano Can't Support Recipient-Specified ISM (Yet)

On EVM:
```solidity
// Safe: contract storage can only be modified by the contract itself
function recipientIsm(address _recipient) public view returns (IInterchainSecurityModule) {
    return _recipient.interchainSecurityModule();  // Reads from contract storage
}
```

On Cardano, this is **vulnerable**:
```
ATTACK:
1. Attacker creates fake UTXO at recipient address with datum: { ism: attacker_ism }
2. Mailbox reads ISM from fake UTXO → uses attacker's ISM
3. Attacker's ISM "verifies" the forged message
4. Real recipient UTXO is never touched, but message is "processed"
```

The root cause: **anyone can create a UTXO at any script address with any datum**.

#### Current Behavior

All recipients use the mailbox's default ISM. For recipients needing custom security:
- Deploy a stricter ISM
- Have the mailbox owner set it as the default

#### Future: NFT-Based Recipient ISM

To support recipient-specified ISMs securely, we would need:
1. Each recipient has a unique NFT marker in its state UTXO
2. Mailbox checks for the recipient's NFT before reading its datum
3. This ensures we read from the REAL recipient UTXO, not a fake one

### Attack Scenarios and Defenses

| Attack | Defense |
|--------|---------|
| Call `handle()` directly | `onlyMailbox` modifier rejects |
| Forge message to Mailbox | ISM signature verification fails |
| Spoof sender address | Remote router enrollment check fails |
| Bypass ISM verification | Mailbox requires `ism.verify()` to pass |
| Create fake Mailbox | Recipient is deployed with hardcoded mailbox address |

## Attack Resistance

### 1. Message Forgery
**Attack**: Create fake message claiming to be from origin chain.
**Defense**: Forged message won't have valid validator signatures.

### 2. Message Tampering
**Attack**: Modify message body during transit.
**Defense**: keccak256(modified_message) ≠ message_id → verification fails.

### 3. Signature Replay
**Attack**: Reuse signatures from one chain on another.
**Defense**: Domain hash includes origin + merkle_hook + "HYPERLANE".

### 4. Double Processing
**Attack**: Process the same message twice.
**Defense**: Processed message marker created atomically.

### 5. Validator Impersonation
**Attack**: Claim to be a trusted validator.
**Defense**: ECDSA signature verification + address derivation.

### 6. Merkle Proof Manipulation
**Attack**: Create fake proof for non-existent message.
**Defense**: Proof must produce the signed checkpoint root.

## Key File References

### Origin Chain (Solidity)
- `solidity/contracts/Mailbox.sol` - Message dispatch
- `solidity/contracts/libs/Message.sol` - Message encoding
- `solidity/contracts/libs/CheckpointLib.sol` - Checkpoint format
- `solidity/contracts/hooks/MerkleTreeHook.sol` - Merkle accumulator

### Relayer (Rust)
- `rust/main/hyperlane-core/src/types/message.rs` - Message types
- `rust/main/agents/relayer/src/prover.rs` - Merkle proofs
- `rust/main/agents/relayer/src/msg/metadata/` - Metadata construction

### Destination Chain (Cardano/Aiken)
- `cardano/contracts/validators/mailbox.ak` - Mailbox validator
- `cardano/contracts/validators/multisig_ism.ak` - ISM validator
- `cardano/contracts/lib/types.ak` - Data types

## Verification Checklist

When a message arrives at Cardano, verification requires:

- [ ] Message ID = keccak256(message_bytes)
- [ ] Message destination = Cardano domain ID
- [ ] Message not previously processed
- [ ] Checkpoint hash computed correctly
- [ ] Threshold signatures from trusted validators
- [ ] Each signature valid (ECDSA secp256k1)
- [ ] Each signer address derived from public key
- [ ] Each signer in trusted validator set for origin
- [ ] Merkle proof valid (message_id → checkpoint_root)
- [ ] Processed message marker created

If ALL checks pass, the message is authentic and delivered to the recipient.

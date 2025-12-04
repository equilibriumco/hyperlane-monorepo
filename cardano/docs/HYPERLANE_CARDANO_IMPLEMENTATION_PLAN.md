# Hyperlane Cardano Implementation Plan

## Overview

This document describes the design and implementation plan for integrating Hyperlane cross-chain messaging protocol with Cardano. The goal is to enable arbitrary message passing between Cardano and other Hyperlane-supported chains (EVM, Solana, Cosmos, etc.) with the relayer handling all transaction construction—no per-recipient off-chain components required.

### Key Design Constraints

1. **Relayer-driven**: The Hyperlane relayer must be able to construct and submit complete Cardano transactions without recipient-specific off-chain code
2. **eUTXO-compatible**: Design must work within Cardano's eUTXO model where UTXOs are ephemeral and must be discovered
3. **Standardized interface**: Recipients follow a standard pattern enabling generic relayer logic
4. **Aiken contracts**: All on-chain validators written in Aiken

### Terminology

- **Mailbox**: Core Hyperlane contract that dispatches and processes messages
- **ISM (Interchain Security Module)**: Verifies message authenticity (signatures, proofs)
- **IGP (Interchain Gas Paymaster)**: Handles cross-chain gas payments
- **Warp Route**: Token bridging contracts (lock/mint pattern)
- **Recipient**: Any contract that can receive Hyperlane messages

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────────┐
│                           CARDANO CHAIN                                  │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐                  │
│  │   Mailbox    │  │     ISM      │  │     IGP      │                  │
│  │  Validator   │  │  Validator   │  │  Validator   │                  │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘                  │
│         │                 │                 │                           │
│         │    Reference    │    Reference    │                           │
│         │      Inputs     │      Inputs     │                           │
│         ▼                 ▼                 ▼                           │
│  ┌─────────────────────────────────────────────────────────────────┐   │
│  │                    Recipient Registry                            │   │
│  │  ┌─────────────────────────────────────────────────────────┐    │   │
│  │  │ ScriptHash → { stateNft, additionalInputs, recipientType }│    │   │
│  │  └─────────────────────────────────────────────────────────┘    │   │
│  └─────────────────────────────────────────────────────────────────┘   │
│                              │                                          │
│         ┌────────────────────┼────────────────────┐                    │
│         ▼                    ▼                    ▼                    │
│  ┌──────────────┐     ┌──────────────┐     ┌──────────────┐           │
│  │  Recipient A │     │  Recipient B │     │  Warp Route  │           │
│  │  (Generic)   │     │  (Token Rx)  │     │   (Bridge)   │           │
│  └──────────────┘     └──────────────┘     └──────────────┘           │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    │ Relayer
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                          OTHER CHAINS                                    │
│                  (Ethereum, Solana, Cosmos, etc.)                       │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## Data Structures

### Core Types (Shared across contracts)

```aiken
// Domain identifier (chain ID in Hyperlane terms)
// Cardano mainnet: TBD (e.g., 2001)
// Cardano testnet: TBD (e.g., 2002)
type Domain = Int

// 32-byte addresses (Hyperlane standard)
type HyperlaneAddress = ByteArray // 32 bytes, left-padded for Cardano script hashes

// Message structure (matches Hyperlane spec)
type Message {
  version: Int,           // Protocol version (currently 3)
  nonce: Int,             // Unique per-mailbox nonce
  origin: Domain,         // Source chain domain
  sender: HyperlaneAddress,
  destination: Domain,    // Destination chain domain (Cardano)
  recipient: HyperlaneAddress, // Recipient script hash (padded)
  body: ByteArray,        // Arbitrary message content
}

// Compute message ID (keccak256 hash of encoded message)
fn message_id(msg: Message) -> ByteArray {
  // keccak256(abi.encode(message))
  // Note: Aiken doesn't have native keccak256, needs to be passed as redeemer
  // or use blake2b with domain separation
}
```

### Mailbox Types

```aiken
// Mailbox state stored in datum
type MailboxDatum {
  // Configuration
  local_domain: Domain,
  default_ism: ScriptHash,
  owner: VerificationKeyHash,
  
  // Outbound state
  outbound_nonce: Int,      // Next nonce for dispatched messages
  merkle_root: ByteArray,   // Current merkle tree root (32 bytes)
  merkle_count: Int,        // Number of leaves in tree
  
  // Inbound state
  // Processed messages tracked via separate UTXOs (see ProcessedMessage)
}

// Mailbox redeemer actions
type MailboxRedeemer {
  // Outbound: dispatch a message to another chain
  Dispatch {
    destination: Domain,
    recipient: HyperlaneAddress,
    body: ByteArray,
  }
  
  // Inbound: process a message from another chain
  Process {
    message: Message,
    metadata: ByteArray,      // ISM-specific proof data
    message_id: ByteArray,    // Pre-computed message ID (verified on-chain)
  }
  
  // Admin
  SetDefaultIsm { new_ism: ScriptHash }
  TransferOwnership { new_owner: VerificationKeyHash }
}

// Processed message marker (separate UTXOs to avoid state bloat)
type ProcessedMessageDatum {
  message_id: ByteArray,  // 32 bytes
}
```

### ISM Types (Multisig ISM)

```aiken
// Multisig ISM configuration
type MultisigIsmDatum {
  // Validators per origin domain
  validators: List<(Domain, List<VerificationKeyHash>)>,
  // Threshold per origin domain  
  thresholds: List<(Domain, Int)>,
  owner: VerificationKeyHash,
}

// Multisig ISM redeemer
type MultisigIsmRedeemer {
  // Verification (called as reference input, no state change)
  Verify {
    message: Message,
    signatures: List<(Int, ByteArray)>, // (validator_index, signature)
  }
  
  // Admin
  SetValidators { domain: Domain, validators: List<VerificationKeyHash> }
  SetThreshold { domain: Domain, threshold: Int }
}
```

### Recipient Registry Types

```aiken
// How to locate a UTXO
type UtxoLocator {
  policy_id: PolicyId,
  asset_name: AssetName,
}

// Additional input required by recipient
type AdditionalInput {
  name: ByteArray,           // Human-readable identifier
  locator: UtxoLocator,      // NFT marking the UTXO
  must_be_spent: Bool,       // True = input, False = reference input
}

// Recipient type determines how relayer constructs outputs
type RecipientType {
  // State in, state out, no other effects
  Generic

  // Mints/releases tokens to address specified in message body
  TokenReceiver {
    vault_locator: Option<UtxoLocator>,   // For collateral-backed
    minting_policy: Option<ScriptHash>,    // For synthetic
  }

  // For complex recipients where relayer cannot build outputs
  // Messages are stored on-chain for later processing
  Deferred {
    message_policy: ScriptHash,           // NFT policy proving message legitimacy
  }
}

// Registry entry for a recipient
type RecipientRegistration {
  script_hash: ScriptHash,
  state_locator: UtxoLocator,
  additional_inputs: List<AdditionalInput>,
  recipient_type: RecipientType,
  custom_ism: Option<ScriptHash>,  // Override default ISM
}

// Registry datum
type RegistryDatum {
  registrations: List<RecipientRegistration>,
  owner: VerificationKeyHash,
}

// Registry redeemer
type RegistryRedeemer {
  Register { registration: RecipientRegistration }
  Unregister { script_hash: ScriptHash }
  UpdateRegistration { registration: RecipientRegistration }
}
```

### Standard Recipient Types

```aiken
// Standard wrapper for recipient datums
type HyperlaneRecipientDatum<inner> {
  // Hyperlane-specific fields
  ism: Option<ScriptHash>,          // Custom ISM override
  last_processed_nonce: Option<Int>, // For ordering (optional)
  
  // Contract-specific state
  inner: inner,
}

// Standard redeemer for recipients
type HyperlaneRecipientRedeemer<contract_redeemer> {
  // Handle incoming Hyperlane message
  HandleMessage {
    origin: Domain,
    sender: HyperlaneAddress,
    body: ByteArray,
  }
  
  // Contract-specific actions
  ContractAction { action: contract_redeemer }
}
```

### Warp Route Types (Token Bridge)

```aiken
// Warp route configuration
type WarpRouteConfig {
  // Token info
  token_type: WarpTokenType,
  decimals: Int,
  
  // Remote routes (other chains)
  remote_routes: List<(Domain, HyperlaneAddress)>,
}

type WarpTokenType {
  // Lock tokens in vault, release on receive
  Collateral { 
    policy_id: PolicyId, 
    asset_name: AssetName,
    vault_locator: UtxoLocator,
  }
  
  // Mint synthetic tokens on receive, burn on send
  Synthetic { 
    minting_policy: ScriptHash,
  }
  
  // Native ADA
  Native {
    vault_locator: UtxoLocator,
  }
}

// Warp route datum
type WarpRouteDatum {
  config: WarpRouteConfig,
  owner: VerificationKeyHash,
  total_bridged: Int,  // For accounting
}

// Warp route redeemer
type WarpRouteRedeemer {
  // Send tokens to another chain
  TransferRemote {
    destination: Domain,
    recipient: HyperlaneAddress,  // Final recipient on destination
    amount: Int,
  }
  
  // Receive tokens from another chain (via Hyperlane message)
  HandleMessage {
    origin: Domain,
    sender: HyperlaneAddress,
    body: ByteArray,  // Encoded: (recipient_address, amount)
  }
  
  // Admin
  EnrollRemoteRoute { domain: Domain, route: HyperlaneAddress }
}

// Message body for warp transfers
type WarpTransferBody {
  recipient: Address,  // Cardano address receiving tokens
  amount: Int,
}
```

### IGP Types (Interchain Gas Paymaster)

```aiken
// Gas oracle data per destination
type GasOracleConfig {
  gas_price: Int,          // In remote chain units
  token_exchange_rate: Int, // Lovelace per remote gas unit (scaled)
}

// IGP datum
type IgpDatum {
  owner: VerificationKeyHash,
  beneficiary: Address,    // Receives collected fees
  gas_oracles: List<(Domain, GasOracleConfig)>,
  default_gas_limit: Int,
}

// IGP redeemer
type IgpRedeemer {
  // Pay for message gas
  PayForGas {
    message_id: ByteArray,
    destination: Domain,
    gas_amount: Int,
  }
  
  // Claim collected fees
  Claim { amount: Int }
  
  // Admin
  SetGasOracle { domain: Domain, config: GasOracleConfig }
}
```

---

## Contract Implementations

### 1. Mailbox Validator (`mailbox.ak`)

```aiken
use aiken/list
use aiken/transaction.{ScriptContext, Spend, InlineDatum}
use aiken/transaction/credential.{ScriptCredential}

// Validator
validator {
  fn mailbox(datum: MailboxDatum, redeemer: MailboxRedeemer, ctx: ScriptContext) -> Bool {
    let ScriptContext { transaction, purpose } = ctx
    
    expect Spend(own_ref) = purpose
    
    when redeemer is {
      Dispatch { destination, recipient, body } -> {
        // Validate dispatch
        validate_dispatch(datum, destination, recipient, body, transaction, own_ref)
      }
      
      Process { message, metadata, message_id } -> {
        // Validate inbound message processing
        validate_process(datum, message, metadata, message_id, transaction, own_ref)
      }
      
      SetDefaultIsm { new_ism } -> {
        // Only owner can update
        validate_admin(datum.owner, transaction) &&
        validate_continuation(datum, MailboxDatum { ..datum, default_ism: new_ism }, transaction, own_ref)
      }
      
      TransferOwnership { new_owner } -> {
        validate_admin(datum.owner, transaction) &&
        validate_continuation(datum, MailboxDatum { ..datum, owner: new_owner }, transaction, own_ref)
      }
    }
  }
}

fn validate_dispatch(
  datum: MailboxDatum,
  destination: Domain,
  recipient: HyperlaneAddress,
  body: ByteArray,
  tx: Transaction,
  own_ref: OutputReference
) -> Bool {
  // 1. Continuation UTXO exists with incremented nonce
  let new_datum = MailboxDatum { 
    ..datum, 
    outbound_nonce: datum.outbound_nonce + 1,
    // TODO: Update merkle tree
  }
  
  // 2. Message event emitted (via datum in separate output or metadata)
  // 3. IGP payment made (check IGP UTXO spent or reference input)
  
  validate_continuation(datum, new_datum, tx, own_ref)
}

fn validate_process(
  datum: MailboxDatum,
  message: Message,
  metadata: ByteArray,
  message_id: ByteArray,
  tx: Transaction,
  own_ref: OutputReference
) -> Bool {
  // 1. Verify message is for this domain
  expect message.destination == datum.local_domain
  
  // 2. Verify message_id is correct
  expect verify_message_id(message, message_id)
  
  // 3. Verify message not already processed (no UTXO with this message_id)
  expect not(is_message_processed(message_id, tx))
  
  // 4. Verify ISM approves (check ISM reference input and verify signature)
  let ism = get_recipient_ism(message.recipient, datum.default_ism, tx)
  expect verify_ism(ism, message, metadata, tx)
  
  // 5. Recipient is called (recipient UTXO spent with HandleMessage redeemer)
  expect recipient_called(message.recipient, message, tx)
  
  // 6. Create processed message marker UTXO
  expect processed_marker_created(message_id, tx)
  
  // 7. Continuation UTXO
  validate_continuation(datum, datum, tx, own_ref)
}

fn verify_ism(
  ism: ScriptHash,
  message: Message,
  metadata: ByteArray,
  tx: Transaction
) -> Bool {
  // ISM must be present as reference input
  // The ISM's datum contains validator set
  // Metadata contains signatures that must be verified
  // This is done via the ISM validator logic
  todo
}

fn recipient_called(
  recipient: HyperlaneAddress,
  message: Message,
  tx: Transaction
) -> Bool {
  // Find recipient script hash from HyperlaneAddress
  let recipient_hash = address_to_script_hash(recipient)
  
  // Check that an input from recipient script is spent
  // with HandleMessage redeemer
  list.any(tx.inputs, fn(input) {
    when input.output.address.payment_credential is {
      ScriptCredential(hash) -> hash == recipient_hash
      _ -> False
    }
  })
}

fn validate_continuation(
  old_datum: MailboxDatum,
  new_datum: MailboxDatum,
  tx: Transaction,
  own_ref: OutputReference
) -> Bool {
  // Find own input
  expect Some(own_input) = transaction.find_input(tx, own_ref)
  let own_address = own_input.output.address
  let own_value = own_input.output.value
  
  // Find continuation output
  expect Some(continuation) = list.find(tx.outputs, fn(output) {
    output.address == own_address
  })
  
  // Verify datum updated correctly
  expect InlineDatum(cont_datum) = continuation.datum
  expect cont_datum == new_datum
  
  // Verify value preserved (NFT marker stays)
  value_preserved(own_value, continuation.value)
}

fn validate_admin(owner: VerificationKeyHash, tx: Transaction) -> Bool {
  list.any(tx.extra_signatories, fn(sig) { sig == owner })
}
```

### 2. Multisig ISM Validator (`multisig_ism.ak`)

```aiken
validator {
  fn multisig_ism(datum: MultisigIsmDatum, redeemer: MultisigIsmRedeemer, ctx: ScriptContext) -> Bool {
    when redeemer is {
      Verify { message, signatures } -> {
        // Get validators and threshold for origin domain
        expect Some(validators) = list.find(datum.validators, fn(v) { v.1st == message.origin })
        expect Some(threshold) = list.find(datum.thresholds, fn(t) { t.1st == message.origin })
        
        // Verify we have enough valid signatures
        let valid_sig_count = count_valid_signatures(message, signatures, validators.2nd)
        valid_sig_count >= threshold.2nd
      }
      
      SetValidators { domain, validators } -> {
        validate_admin(datum.owner, ctx.transaction)
        // ... update logic
      }
      
      SetThreshold { domain, threshold } -> {
        validate_admin(datum.owner, ctx.transaction)
        // ... update logic
      }
    }
  }
}

fn count_valid_signatures(
  message: Message,
  signatures: List<(Int, ByteArray)>,
  validators: List<VerificationKeyHash>
) -> Int {
  // Compute signing hash (checkpoint)
  let domain_hash = compute_domain_hash(message.origin, mailbox_address)
  let signing_hash = compute_signing_hash(domain_hash, merkle_root, checkpoint_index)
  
  list.foldl(signatures, 0, fn(acc, sig) {
    let (idx, signature) = sig
    expect Some(validator) = list.at(validators, idx)
    
    if verify_signature(validator, signing_hash, signature) {
      acc + 1
    } else {
      acc
    }
  })
}

// Placeholder - actual implementation needs ed25519 or secp256k1 verification
fn verify_signature(
  validator: VerificationKeyHash,
  message_hash: ByteArray,
  signature: ByteArray
) -> Bool {
  // Cardano native scripts use ed25519
  // For EVM compatibility might need secp256k1 via plutus builtin
  todo
}
```

### 3. Recipient Registry (`registry.ak`)

```aiken
validator {
  fn registry(datum: RegistryDatum, redeemer: RegistryRedeemer, ctx: ScriptContext) -> Bool {
    when redeemer is {
      Register { registration } -> {
        // Anyone can register their own script
        // Verify caller owns the script being registered
        validate_script_ownership(registration.script_hash, ctx.transaction) &&
        validate_registration(registration) &&
        validate_continuation_with_new_registration(datum, registration, ctx)
      }
      
      Unregister { script_hash } -> {
        // Only script owner can unregister
        validate_script_ownership(script_hash, ctx.transaction) &&
        validate_continuation_without_registration(datum, script_hash, ctx)
      }
      
      UpdateRegistration { registration } -> {
        validate_script_ownership(registration.script_hash, ctx.transaction) &&
        validate_continuation_with_updated_registration(datum, registration, ctx)
      }
    }
  }
}

fn validate_registration(reg: RecipientRegistration) -> Bool {
  // Basic validation
  // - state_locator policy_id is derived correctly from script_hash
  // - additional_inputs are valid
  True
}

fn validate_script_ownership(script_hash: ScriptHash, tx: Transaction) -> Bool {
  // Verify that the script is actually being spent in this transaction
  // This proves the caller has authority over it
  list.any(tx.inputs, fn(input) {
    when input.output.address.payment_credential is {
      ScriptCredential(hash) -> hash == script_hash
      _ -> False
    }
  })
}
```

### 4. Example Recipient: Generic Handler (`generic_recipient.ak`)

```aiken
// Example of a minimal Hyperlane recipient
// Stores received messages in datum

type GenericRecipientInner {
  messages_received: Int,
  last_message: Option<ByteArray>,
}

validator {
  fn generic_recipient(
    datum: HyperlaneRecipientDatum<GenericRecipientInner>,
    redeemer: HyperlaneRecipientRedeemer<Void>,
    ctx: ScriptContext
  ) -> Bool {
    when redeemer is {
      HandleMessage { origin, sender, body } -> {
        // Verify called by Mailbox (mailbox must be spending its UTXO too)
        expect mailbox_is_caller(ctx.transaction)
        
        // Update state
        let new_inner = GenericRecipientInner {
          messages_received: datum.inner.messages_received + 1,
          last_message: Some(body),
        }
        
        let new_datum = HyperlaneRecipientDatum {
          ..datum,
          inner: new_inner,
        }
        
        validate_continuation(datum, new_datum, ctx)
      }
      
      ContractAction { action } -> {
        // No custom actions for this simple recipient
        False
      }
    }
  }
}

fn mailbox_is_caller(tx: Transaction) -> Bool {
  // Verify mailbox UTXO is being spent in same transaction
  // This ensures the message was validated by mailbox
  let mailbox_hash = get_mailbox_hash() // Known constant
  
  list.any(tx.inputs, fn(input) {
    when input.output.address.payment_credential is {
      ScriptCredential(hash) -> hash == mailbox_hash
      _ -> False
    }
  })
}
```

### 5. Warp Route Validator (`warp_route.ak`)

```aiken
validator {
  fn warp_route(
    datum: WarpRouteDatum,
    redeemer: WarpRouteRedeemer,
    ctx: ScriptContext
  ) -> Bool {
    when redeemer is {
      TransferRemote { destination, recipient, amount } -> {
        // User sending tokens to another chain
        validate_transfer_remote(datum, destination, recipient, amount, ctx)
      }
      
      HandleMessage { origin, sender, body } -> {
        // Receiving tokens from another chain via Hyperlane
        expect mailbox_is_caller(ctx.transaction)
        
        // Decode body
        expect Some(transfer) = decode_warp_transfer_body(body)
        
        // Verify sender is enrolled remote route
        expect is_enrolled_route(datum.config.remote_routes, origin, sender)
        
        // Process based on token type
        when datum.config.token_type is {
          Collateral { policy_id, asset_name, vault_locator } -> {
            // Release tokens from vault to recipient
            validate_collateral_release(
              vault_locator,
              policy_id,
              asset_name,
              transfer.recipient,
              transfer.amount,
              ctx
            )
          }
          
          Synthetic { minting_policy } -> {
            // Mint synthetic tokens to recipient
            validate_synthetic_mint(
              minting_policy,
              transfer.recipient,
              transfer.amount,
              ctx
            )
          }
          
          Native { vault_locator } -> {
            // Release ADA from vault
            validate_native_release(
              vault_locator,
              transfer.recipient,
              transfer.amount,
              ctx
            )
          }
        }
      }
      
      EnrollRemoteRoute { domain, route } -> {
        validate_admin(datum.owner, ctx.transaction)
        // ... update logic
      }
    }
  }
}

fn validate_transfer_remote(
  datum: WarpRouteDatum,
  destination: Domain,
  recipient: HyperlaneAddress,
  amount: Int,
  ctx: ScriptContext
) -> Bool {
  // Verify destination route is enrolled
  expect is_enrolled_route(datum.config.remote_routes, destination, recipient)
  
  // Based on token type, lock or burn
  when datum.config.token_type is {
    Collateral { policy_id, asset_name, vault_locator } -> {
      // Tokens must be sent to vault
      validate_collateral_lock(vault_locator, policy_id, asset_name, amount, ctx)
    }
    
    Synthetic { minting_policy } -> {
      // Tokens must be burned
      validate_synthetic_burn(minting_policy, amount, ctx)
    }
    
    Native { vault_locator } -> {
      // ADA must be sent to vault
      validate_native_lock(vault_locator, amount, ctx)
    }
  } &&
  
  // Mailbox dispatch must be called
  mailbox_dispatch_called(destination, recipient, encode_warp_transfer(recipient, amount), ctx)
}

fn validate_collateral_release(
  vault_locator: UtxoLocator,
  policy_id: PolicyId,
  asset_name: AssetName,
  recipient: Address,
  amount: Int,
  ctx: ScriptContext
) -> Bool {
  // Find vault input (by NFT locator)
  expect Some(vault_input) = find_input_by_locator(vault_locator, ctx.transaction)
  
  // Find vault output (continuation)
  expect Some(vault_output) = find_output_by_locator(vault_locator, ctx.transaction)
  
  // Verify vault released correct amount
  let input_tokens = value.quantity_of(vault_input.output.value, policy_id, asset_name)
  let output_tokens = value.quantity_of(vault_output.value, policy_id, asset_name)
  expect input_tokens - output_tokens == amount
  
  // Verify recipient received tokens
  let recipient_output = find_output_to_address(recipient, ctx.transaction)
  expect Some(ro) = recipient_output
  let recipient_tokens = value.quantity_of(ro.value, policy_id, asset_name)
  recipient_tokens >= amount
}
```

### 6. NFT Minting Policies

```aiken
// State NFT minting policy
// One-shot minting tied to specific UTXO consumption

validator(utxo_ref: OutputReference) {
  fn state_nft_policy(_redeemer: Void, ctx: ScriptContext) -> Bool {
    let ScriptContext { transaction, purpose } = ctx
    
    expect Mint(own_policy) = purpose
    
    // Can only mint if specific UTXO is consumed (one-shot)
    let utxo_consumed = list.any(transaction.inputs, fn(input) {
      input.output_reference == utxo_ref
    })
    
    // Can only mint exactly 1 token
    let minted = value.from_minted_value(transaction.mint)
    let own_minted = value.tokens(minted, own_policy)
    let total_minted = dict.foldl(own_minted, 0, fn(_k, v, acc) { acc + v })
    
    utxo_consumed && total_minted == 1
  }
}

// Synthetic token minting policy (for warp routes)
validator(warp_route_hash: ScriptHash) {
  fn synthetic_token_policy(_redeemer: Void, ctx: ScriptContext) -> Bool {
    // Can only mint/burn if warp route validator is satisfied
    let warp_route_involved = list.any(ctx.transaction.inputs, fn(input) {
      when input.output.address.payment_credential is {
        ScriptCredential(hash) -> hash == warp_route_hash
        _ -> False
      }
    })
    
    warp_route_involved
  }
}
```

---

## Relayer Implementation

### Relayer Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     HYPERLANE RELAYER                           │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  ┌─────────────────┐  ┌─────────────────┐  ┌────────────────┐  │
│  │  Message        │  │   Cardano       │  │   Transaction  │  │
│  │  Indexer        │──│   Adapter       │──│   Submitter    │  │
│  │  (multi-chain)  │  │                 │  │                │  │
│  └─────────────────┘  └─────────────────┘  └────────────────┘  │
│           │                   │                    │            │
│           │                   ▼                    │            │
│           │          ┌─────────────────┐           │            │
│           │          │  UTXO Discovery │           │            │
│           │          │  Service        │           │            │
│           │          └─────────────────┘           │            │
│           │                   │                    │            │
│           ▼                   ▼                    ▼            │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │                    Chain Providers                       │   │
│  │  (Ethereum RPC, Cardano Node, Solana RPC, etc.)         │   │
│  └─────────────────────────────────────────────────────────┘   │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

### Cardano Adapter (Rust pseudo-code)

```rust
// relayer/src/cardano/adapter.rs

use cardano_serialization_lib as csl;

pub struct CardanoAdapter {
    node_client: CardanoNodeClient,
    registry_utxo: UtxoRef,
    mailbox_utxo: UtxoRef,
    signing_key: PrivateKey,
}

impl CardanoAdapter {
    /// Process an inbound Hyperlane message
    pub async fn process_message(
        &self,
        message: HyperlaneMessage,
        metadata: Vec<u8>,
    ) -> Result<TxHash> {
        // 1. Lookup recipient in registry
        let registration = self.lookup_recipient(&message.recipient).await?;
        
        // 2. Discover all required UTXOs
        let utxos = self.discover_utxos(&registration).await?;
        
        // 3. Build transaction based on recipient type
        let tx = self.build_process_tx(
            message,
            metadata,
            registration,
            utxos,
        )?;
        
        // 4. Sign and submit
        let signed_tx = self.sign_tx(tx)?;
        let tx_hash = self.submit_tx(signed_tx).await?;
        
        Ok(tx_hash)
    }
    
    async fn lookup_recipient(
        &self,
        recipient: &HyperlaneAddress,
    ) -> Result<RecipientRegistration> {
        // Query registry UTXO
        let registry_utxo = self.node_client
            .get_utxo(&self.registry_utxo)
            .await?;
        
        // Decode datum
        let registry_datum: RegistryDatum = decode_datum(&registry_utxo.datum)?;
        
        // Find registration
        let script_hash = recipient.to_script_hash()?;
        registry_datum.registrations
            .iter()
            .find(|r| r.script_hash == script_hash)
            .cloned()
            .ok_or(Error::RecipientNotRegistered)
    }
    
    async fn discover_utxos(
        &self,
        registration: &RecipientRegistration,
    ) -> Result<DiscoveredUtxos> {
        // Find mailbox state UTXO
        let mailbox = self.find_utxo_by_nft(&MAILBOX_STATE_NFT).await?;
        
        // Find ISM state UTXO
        let ism_hash = registration.custom_ism
            .unwrap_or(self.default_ism);
        let ism = self.find_utxo_by_script(&ism_hash).await?;
        
        // Find recipient state UTXO
        let recipient_state = self.find_utxo_by_nft(
            &registration.state_locator
        ).await?;
        
        // Find additional inputs
        let mut additional = Vec::new();
        for input in &registration.additional_inputs {
            let utxo = self.find_utxo_by_nft(&input.locator).await?;
            additional.push((input.clone(), utxo));
        }
        
        Ok(DiscoveredUtxos {
            mailbox,
            ism,
            recipient_state,
            additional,
        })
    }
    
    fn build_process_tx(
        &self,
        message: HyperlaneMessage,
        metadata: Vec<u8>,
        registration: RecipientRegistration,
        utxos: DiscoveredUtxos,
    ) -> Result<Transaction> {
        let mut tx_builder = TransactionBuilder::new();
        
        // Add mailbox input with Process redeemer
        tx_builder.add_script_input(
            utxos.mailbox,
            MailboxRedeemer::Process {
                message: message.clone(),
                metadata: metadata.clone(),
                message_id: message.id(),
            },
        );
        
        // Add ISM as reference input
        tx_builder.add_reference_input(utxos.ism);
        
        // Add recipient input with HandleMessage redeemer
        tx_builder.add_script_input(
            utxos.recipient_state.clone(),
            HyperlaneRecipientRedeemer::HandleMessage {
                origin: message.origin,
                sender: message.sender,
                body: message.body.clone(),
            },
        );
        
        // Add additional inputs
        for (input_spec, utxo) in utxos.additional {
            if input_spec.must_be_spent {
                tx_builder.add_script_input(utxo, /* appropriate redeemer */);
            } else {
                tx_builder.add_reference_input(utxo);
            }
        }
        
        // Build outputs based on recipient type
        self.build_outputs(
            &mut tx_builder,
            &message,
            &registration,
            &utxos,
        )?;
        
        // Add collateral
        tx_builder.add_collateral(self.collateral_utxo);
        
        // Build and balance
        tx_builder.build()
    }
    
    fn build_outputs(
        &self,
        tx_builder: &mut TransactionBuilder,
        message: &HyperlaneMessage,
        registration: &RecipientRegistration,
        utxos: &DiscoveredUtxos,
    ) -> Result<()> {
        match &registration.recipient_type {
            RecipientType::Generic => {
                // Just continuation of recipient state
                let new_datum = self.compute_new_recipient_datum(
                    &utxos.recipient_state,
                    message,
                )?;
                
                tx_builder.add_output(
                    utxos.recipient_state.address.clone(),
                    utxos.recipient_state.value.clone(),
                    new_datum,
                );
            }
            
            RecipientType::TokenReceiver { vault_locator, minting_policy } => {
                // Decode transfer from message body
                let transfer: WarpTransferBody = decode(&message.body)?;
                
                if let Some(vault) = vault_locator {
                    // Release from vault
                    let vault_utxo = self.find_utxo_by_nft(vault).await?;
                    
                    // Vault continuation (minus tokens)
                    tx_builder.add_output(
                        vault_utxo.address,
                        vault_utxo.value - transfer.amount,
                        vault_utxo.datum,
                    );
                    
                    // Recipient receives tokens
                    tx_builder.add_output(
                        transfer.recipient,
                        Value::from_token(token_policy, token_name, transfer.amount),
                        None,
                    );
                } else if let Some(policy) = minting_policy {
                    // Mint synthetic
                    tx_builder.add_mint(policy, transfer.amount);
                    tx_builder.add_output(
                        transfer.recipient,
                        Value::from_token(policy, asset_name, transfer.amount),
                        None,
                    );
                }
                
                // Recipient state continuation
                tx_builder.add_output(
                    utxos.recipient_state.address.clone(),
                    utxos.recipient_state.value.clone(),
                    self.compute_new_recipient_datum(&utxos.recipient_state, message)?,
                );
            }
            
            RecipientType::Deferred { message_policy } => {
                // Store message on-chain with NFT for later processing
                // Mint message NFT, create stored message UTXO
                todo!()
            }
        }
        
        // Mailbox continuation
        tx_builder.add_output(
            utxos.mailbox.address.clone(),
            utxos.mailbox.value.clone(),
            utxos.mailbox.datum.clone(), // Mailbox datum doesn't change on process
        );
        
        // Processed message marker
        tx_builder.add_output(
            processed_messages_address(),
            min_ada_value(),
            ProcessedMessageDatum { message_id: message.id() },
        );
        
        Ok(())
    }
    
    async fn find_utxo_by_nft(&self, locator: &UtxoLocator) -> Result<Utxo> {
        self.node_client
            .query_utxos_by_asset(locator.policy_id, locator.asset_name)
            .await?
            .into_iter()
            .next()
            .ok_or(Error::UtxoNotFound)
    }
}

// Retry logic for contention
impl CardanoAdapter {
    pub async fn process_message_with_retry(
        &self,
        message: HyperlaneMessage,
        metadata: Vec<u8>,
        max_retries: u32,
    ) -> Result<TxHash> {
        let mut attempts = 0;
        
        loop {
            match self.process_message(message.clone(), metadata.clone()).await {
                Ok(hash) => return Ok(hash),
                Err(Error::UtxoConsumed) | Err(Error::TxSubmitFailed) => {
                    attempts += 1;
                    if attempts >= max_retries {
                        return Err(Error::MaxRetriesExceeded);
                    }
                    
                    // Wait a bit and retry with fresh UTXO discovery
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Err(e) => return Err(e),
            }
        }
    }
}
```

---

## Implementation Phases

### Phase 1: Core Infrastructure

**Goal**: Deploy basic Mailbox and ISM that can receive messages

**Tasks**:
1. [ ] Set up Aiken project structure
2. [ ] Implement core types module (`types.ak`)
3. [ ] Implement Mailbox validator (process only, no dispatch yet)
4. [ ] Implement Multisig ISM validator
5. [ ] Implement state NFT minting policy
6. [ ] Write unit tests for validators
7. [ ] Deploy to preview testnet
8. [ ] Implement basic Cardano adapter in relayer (Rust)
9. [ ] End-to-end test: receive message from Sepolia testnet

**Deliverables**:
- Working mailbox that can receive messages
- Relayer can process EVM → Cardano messages
- Basic ISM with configurable validators

### Phase 2: Recipient Registry & Generic Recipients

**Goal**: Enable arbitrary contracts to receive Hyperlane messages

**Tasks**:
1. [ ] Implement Recipient Registry validator
2. [ ] Implement generic recipient example
3. [ ] Update relayer to query registry
4. [ ] Implement UTXO discovery in relayer
5. [ ] Write registration CLI tool
6. [ ] Test with multiple registered recipients
7. [ ] Documentation for recipient developers

**Deliverables**:
- Any Aiken contract can register to receive Hyperlane messages
- Relayer handles all transaction construction
- Developer guide for creating Hyperlane-compatible recipients

### Phase 3: Outbound Messages (Dispatch)

**Goal**: Send messages from Cardano to other chains

**Tasks**:
1. [ ] Complete Mailbox dispatch functionality
2. [ ] Implement IGP validator
3. [ ] Implement merkle tree for message commitments
4. [ ] Set up Cardano validators (off-chain signers)
5. [ ] Implement Cardano indexer in relayer
6. [ ] End-to-end test: Cardano → Sepolia message

**Deliverables**:
- Cardano contracts can dispatch messages
- Messages are relayed to destination chains
- Gas payment system operational

### Phase 4: Warp Routes (Token Bridge)

**Goal**: Bridge tokens between Cardano and other chains

**Tasks**:
1. [ ] Implement Warp Route validator
2. [ ] Implement synthetic token minting policy
3. [ ] Implement vault logic for collateral tokens
4. [ ] Update relayer for TokenReceiver type
5. [ ] Deploy test warp routes (ADA ↔ Sepolia)
6. [ ] UI integration (optional)

**Deliverables**:
- Working token bridge
- ADA bridgeable to/from testnets
- Custom token warp route deployment guide

### Phase 5: Production Hardening

**Goal**: Production-ready deployment

**Tasks**:
1. [ ] Security audit of Aiken contracts
2. [ ] Formal verification where possible
3. [ ] Mainnet deployment scripts
4. [ ] Monitoring and alerting
5. [ ] Rate limiting and gas optimization
6. [ ] Documentation and runbooks

**Deliverables**:
- Audited contracts
- Mainnet deployment
- Operational documentation

---

## Testing Strategy

### Unit Tests (Aiken)

```aiken
// tests/mailbox_tests.ak

test mailbox_process_valid_message() {
  let datum = mock_mailbox_datum()
  let message = mock_message()
  let metadata = mock_multisig_metadata()
  
  let ctx = mock_script_context()
    .with_input(mock_mailbox_utxo(datum))
    .with_reference_input(mock_ism_utxo())
    .with_input(mock_recipient_utxo())
    .with_output(mock_mailbox_continuation())
    .with_output(mock_processed_marker(message.id()))
  
  mailbox(datum, Process { message, metadata, message_id: message.id() }, ctx)
}

test mailbox_process_replay_rejected() fail {
  // Same message ID already processed
  let datum = mock_mailbox_datum()
  let message = mock_message()
  
  let ctx = mock_script_context()
    .with_input(mock_mailbox_utxo(datum))
    .with_reference_input(mock_ism_utxo())
    .with_reference_input(mock_processed_marker(message.id())) // Already exists!
  
  mailbox(datum, Process { message, ... }, ctx)
}

test mailbox_process_invalid_ism_rejected() fail {
  // ISM verification fails
  let datum = mock_mailbox_datum()
  let message = mock_message()
  let bad_metadata = vec![] // No signatures
  
  mailbox(datum, Process { message, metadata: bad_metadata, ... }, mock_ctx())
}
```

### Integration Tests (Rust/TypeScript)

```typescript
// tests/integration/process_message.test.ts

describe('Process Message', () => {
  it('should process valid message from EVM', async () => {
    // 1. Dispatch message on Sepolia
    const tx = await sepoliaMailbox.dispatch(
      CARDANO_DOMAIN,
      cardanoRecipient,
      messageBody
    );
    await tx.wait();
    
    // 2. Wait for relayer to process
    await waitForMessage(tx.hash);
    
    // 3. Verify recipient received message
    const recipientState = await queryRecipientState(cardanoRecipient);
    expect(recipientState.messagesReceived).toBe(1);
    expect(recipientState.lastMessage).toEqual(messageBody);
  });
  
  it('should handle UTXO contention gracefully', async () => {
    // Send multiple messages rapidly
    const messages = await Promise.all([
      sepoliaMailbox.dispatch(CARDANO_DOMAIN, recipient, body1),
      sepoliaMailbox.dispatch(CARDANO_DOMAIN, recipient, body2),
      sepoliaMailbox.dispatch(CARDANO_DOMAIN, recipient, body3),
    ]);
    
    // All should eventually be processed
    await Promise.all(messages.map(m => waitForMessage(m.hash)));
    
    const state = await queryRecipientState(recipient);
    expect(state.messagesReceived).toBe(3);
  });
});
```

### Property-Based Tests

```aiken
// tests/property_tests.ak

// Any valid message should be processable exactly once
test_property message_processed_once(message: Message) {
  // Process succeeds first time
  let result1 = try_process(message)
  expect result1 == Ok(())
  
  // Process fails second time (replay)
  let result2 = try_process(message)
  expect result2 == Err(AlreadyProcessed)
}

// Nonces must be sequential
test_property dispatch_nonces_sequential(messages: List<Message>) {
  let nonces = list.map(messages, fn(m) { dispatch_and_get_nonce(m) })
  let expected = list.range(0, list.length(messages))
  nonces == expected
}
```

---

## Directory Structure

```
hyperlane-cardano/
├── contracts/                    # Aiken smart contracts
│   ├── aiken.toml
│   ├── lib/
│   │   ├── types.ak              # Shared type definitions
│   │   ├── utils.ak              # Utility functions
│   │   └── merkle.ak             # Merkle tree implementation
│   ├── validators/
│   │   ├── mailbox.ak            # Core mailbox validator
│   │   ├── multisig_ism.ak       # Multisig ISM
│   │   ├── registry.ak           # Recipient registry
│   │   ├── igp.ak                # Interchain gas paymaster
│   │   └── warp_route.ak         # Token bridge
│   ├── minting_policies/
│   │   ├── state_nft.ak          # One-shot state NFTs
│   │   └── synthetic_token.ak    # Warp route synthetics
│   └── tests/
│       ├── mailbox_tests.ak
│       ├── ism_tests.ak
│       └── warp_route_tests.ak
│
├── relayer/                      # Rust relayer components
│   ├── Cargo.toml
│   └── src/
│       ├── cardano/
│       │   ├── mod.rs
│       │   ├── adapter.rs        # Main Cardano adapter
│       │   ├── utxo_discovery.rs # UTXO query logic
│       │   ├── tx_builder.rs     # Transaction construction
│       │   └── types.rs          # Cardano-specific types
│       └── lib.rs
│
├── sdk/                          # TypeScript SDK
│   ├── package.json
│   └── src/
│       ├── cardano/
│       │   ├── mailbox.ts
│       │   ├── warpRoute.ts
│       │   └── types.ts
│       └── index.ts
│
├── scripts/                      # Deployment and CLI tools
│   ├── deploy.ts                 # Contract deployment
│   ├── register-recipient.ts    # Registry CLI
│   └── setup-warp-route.ts      # Warp route setup
│
├── tests/                        # Integration tests
│   ├── e2e/
│   │   ├── evm-to-cardano.test.ts
│   │   └── cardano-to-evm.test.ts
│   └── fixtures/
│
└── docs/
    ├── architecture.md
    ├── recipient-guide.md        # How to build recipients
    └── deployment.md
```

---

## Configuration

### Contract Parameters (to be set at deployment)

```json
{
  "mailbox": {
    "localDomain": 2001,
    "defaultIsm": "<multisig_ism_script_hash>",
    "owner": "<deployer_pub_key_hash>"
  },
  "multisigIsm": {
    "validators": {
      "1": ["<eth_validator_1>", "<eth_validator_2>", "<eth_validator_3>"],
      "1399811149": ["<sol_validator_1>", "<sol_validator_2>"]
    },
    "thresholds": {
      "1": 2,
      "1399811149": 2
    }
  },
  "igp": {
    "gasOracles": {
      "1": {
        "gasPrice": 30000000000,
        "tokenExchangeRate": 1500000
      }
    },
    "defaultGasLimit": 300000
  }
}
```

### Relayer Configuration

```toml
# relayer.toml

[cardano]
node_socket = "/path/to/node.socket"
network_magic = 2  # Preview testnet
signing_key_file = "/path/to/relayer.skey"

[cardano.contracts]
mailbox_script_hash = "abc123..."
mailbox_state_nft = { policy_id = "def456...", asset_name = "state" }
registry_script_hash = "789xyz..."
registry_state_nft = { policy_id = "...", asset_name = "registry" }
default_ism_script_hash = "..."

[cardano.retry]
max_attempts = 5
base_delay_ms = 1000
max_delay_ms = 30000

[domains]
cardano_preview = 2002
ethereum_sepolia = 11155111
```

---

## Open Questions / Decisions Needed

1. **Signature Scheme**: Cardano uses Ed25519 natively, but EVM validators use secp256k1. Options:
   - Use Ed25519 for Cardano validators (requires separate validator set)
   - Implement secp256k1 verification in Aiken (expensive, may need Plutus builtins)
   - Hybrid approach with adapter signatures

2. **Merkle Tree**: For outbound messages, need incremental merkle tree. Options:
   - Store full tree on-chain (expensive)
   - Store only root, validators reconstruct from events
   - Off-chain tree with on-chain commitments

3. **Message ID Computation**: Hyperlane uses keccak256, Cardano has blake2b. Options:
   - Pass pre-computed message ID in redeemer, verify in ISM
   - Implement keccak256 in Aiken (if possible)
   - Use domain-separated blake2b for Cardano-originated messages

4. **Processed Message Tracking**: Options:
   - Separate UTXOs per message (scalable but many UTXOs)
   - Bloom filter in mailbox datum (false positives possible)
   - Reference script with set membership

5. **Gas/Fee Payment**: How to handle Cardano tx fees for relayer?
   - IGP collects fees, relayer claims periodically
   - Direct payment to relayer address
   - Fee abstraction layer

---

## References

- [Hyperlane Documentation](https://docs.hyperlane.xyz/)
- [Hyperlane Monorepo](https://github.com/hyperlane-xyz/hyperlane-monorepo)
- [Hyperlane Sealevel (Solana)](https://github.com/hyperlane-xyz/hyperlane-monorepo/tree/main/rust/sealevel)
- [Aiken Documentation](https://aiken-lang.org/)
- [Cardano eUTXO Model](https://docs.cardano.org/learn/eutxo-explainer)
- [CIP-57: Plutus Smart Contract Blueprints](https://cips.cardano.org/cip/CIP-0057)

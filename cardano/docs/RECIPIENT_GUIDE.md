# Hyperlane Cardano Recipient Developer Guide

This guide explains how to build a Hyperlane-compatible recipient contract on Cardano and register it with the Hyperlane relayer network.

## Overview

On Cardano, Hyperlane recipients are Plutus V3 scripts that can receive cross-chain messages from the Hyperlane Mailbox. The relayer network handles:

1. **Message Discovery**: Indexing messages dispatched from other chains
2. **ISM Verification**: Gathering validator signatures to prove message authenticity
3. **Transaction Building**: Constructing the Cardano transaction to deliver the message
4. **Message Delivery**: Submitting the transaction to process the message

## Recipient Contract Pattern

### Required Datum Structure

Your recipient contract should use the `HyperlaneRecipientDatum` wrapper:

```aiken
/// Standard wrapper for recipient datums
type HyperlaneRecipientDatum<inner> {
  /// Custom ISM override (optional)
  ism: Option<ScriptHash>,
  /// For ordering (optional)
  last_processed_nonce: Option<Int>,
  /// Your contract-specific state
  inner: inner,
}
```

### Required Redeemer Structure

```aiken
/// Standard redeemer for recipients
type HyperlaneRecipientRedeemer<contract_redeemer> {
  /// Handle incoming Hyperlane message
  HandleMessage { origin: Domain, sender: HyperlaneAddress, body: ByteArray }
  /// Contract-specific actions
  ContractAction { action: contract_redeemer }
}
```

### Example Recipient Contract

```aiken
use types.{
  Domain, HyperlaneAddress, HyperlaneRecipientDatum, HyperlaneRecipientRedeemer,
  ScriptHash,
}

/// Your contract's inner state
type MyContractState {
  counter: Int,
  last_sender: Option<HyperlaneAddress>,
}

/// Validator parameterized by mailbox script hash
validator my_recipient(mailbox_hash: ScriptHash) {
  spend(
    datum: Option<HyperlaneRecipientDatum<MyContractState>>,
    redeemer: HyperlaneRecipientRedeemer<Void>,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    expect Some(d) = datum

    when redeemer is {
      HyperlaneRecipientRedeemer.HandleMessage { origin, sender, body } -> {
        // IMPORTANT: Verify the mailbox is spending its UTXO too
        // This proves the message was validated by the ISM
        expect has_script_input(tx, mailbox_hash)

        // Process your message
        let new_state = MyContractState {
          counter: d.inner.counter + 1,
          last_sender: Some(sender),
        }

        // Validate continuation output
        validate_continuation(d, new_state, tx, own_ref)
      }

      HyperlaneRecipientRedeemer.ContractAction { .. } -> {
        // Handle your contract's other actions
        False
      }
    }
  }

  else(_) { fail }
}
```

## State UTXO Pattern

Your recipient must have a **state UTXO** that:
1. Is at your script address
2. Contains an NFT marker for unique identification
3. Stores your contract state in an inline datum

The NFT marker pattern:
- Mint a unique NFT (policy ID + asset name)
- The NFT stays in the state UTXO
- The relayer uses this NFT to find your state UTXO

## Registration

To receive messages, you must register your recipient in the Hyperlane Registry.

### Registration Data

```rust
struct RecipientRegistration {
    // Your script hash (28 bytes)
    script_hash: ScriptHash,

    // How to find your state UTXO
    state_locator: UtxoLocator {
        policy_id: String,   // NFT policy ID
        asset_name: String,  // NFT asset name
    },

    // Additional UTXOs needed for your contract
    additional_inputs: Vec<AdditionalInput>,

    // What type of recipient
    recipient_type: RecipientType,

    // Optional custom ISM
    custom_ism: Option<ScriptHash>,
}
```

### Using the Registration CLI

```bash
# Set your Blockfrost API key
export BLOCKFROST_API_KEY=your_api_key

# Register a generic handler
cardano_register \
    --script-hash "your_script_hash_hex" \
    --state-policy "your_nft_policy_id" \
    --state-asset "your_nft_asset_name" \
    --recipient-type generic \
    --network preprod \
    --dry-run

# Register a token receiver
cardano_register \
    --script-hash "your_script_hash_hex" \
    --state-policy "your_nft_policy_id" \
    --state-asset "your_nft_asset_name" \
    --recipient-type token-receiver \
    --vault-policy "vault_policy_id" \
    --vault-asset "vault_asset_name" \
    --network preprod

# Register a deferred recipient (for deferred processing)
cardano_register \
    --script-hash "your_script_hash_hex" \
    --state-policy "your_nft_policy_id" \
    --state-asset "your_nft_asset_name" \
    --recipient-type deferred-recipient \
    --message-policy "message_nft_policy_id" \
    --network preprod

# With additional inputs
cardano_register \
    --script-hash "your_script_hash_hex" \
    --state-policy "your_nft_policy_id" \
    --state-asset "your_nft_asset_name" \
    --additional-input "oracle:oracle_policy:price_feed:false" \
    --recipient-type generic \
    --network preprod
```

### Recipient Types

1. **Generic**: Basic state-in, state-out pattern. The relayer builds a transaction that spends your state UTXO and creates a continuation output.

2. **TokenReceiver**: For warp routes and token bridges. Can include:
   - `vault_locator`: UTXO holding locked tokens
   - `minting_policy`: For synthetic token minting

3. **Deferred**: For complex recipients where the relayer cannot know how to build outputs. Messages are stored on-chain for later processing:
   - `message_policy`: The minting policy for message NFTs (proves message legitimacy)

## Transaction Flow

When a message is delivered to your recipient:

```
┌─────────────────────────────────────────────────────────────┐
│                     Process Transaction                       │
├─────────────────────────────────────────────────────────────┤
│ INPUTS:                                                       │
│   - Mailbox UTXO (with Process redeemer)                     │
│   - Your Recipient UTXO (with HandleMessage redeemer)        │
│   - Fee payment UTXOs                                         │
│                                                               │
│ REFERENCE INPUTS:                                             │
│   - ISM UTXO (for verification)                               │
│                                                               │
│ OUTPUTS:                                                      │
│   - Mailbox continuation (unchanged datum)                   │
│   - Your Recipient continuation (updated datum)              │
│   - Processed Message Marker (prevents replay)               │
│   - Change output                                             │
└─────────────────────────────────────────────────────────────┘
```

## Deferred Pattern (Deferred Message Processing)

The Deferred pattern is designed for complex recipients where the Hyperlane relayer cannot know how to build the transaction outputs. Instead of processing messages immediately, they are stored on-chain for later processing by a separate process (which could be an automated service, a dApp, manual intervention, or any other mechanism operated by the recipient team).

### When to Use Deferred

Use Deferred when your recipient needs to:
- Create complex or variable output UTXOs that the relayer can't predict
- Interact with external protocols in ways that require custom transaction building
- Implement business logic that requires off-chain computation
- Handle messages asynchronously with human intervention

### Two-Phase Message Flow

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                    PHASE 1: Relayer Stores Message                          │
├─────────────────────────────────────────────────────────────────────────────┤
│ INPUTS:                                                                     │
│   - Mailbox UTXO (with Process redeemer)                                   │
│   - Deferred State UTXO (with HandleMessage redeemer)                           │
│   - ISM UTXO (spent for verification)                                      │
│   - Fee payment UTXOs                                                       │
│                                                                             │
│ MINTS:                                                                      │
│   - Message NFT (asset name = message_id, proves legitimacy)               │
│                                                                             │
│ OUTPUTS:                                                                    │
│   - Mailbox continuation (unchanged)                                        │
│   - Deferred continuation (messages_stored += 1)                                │
│   - ISM continuation (unchanged)                                            │
│   - Message UTXO (StoredMessageDatum + Message NFT)  <-- NEW               │
│   - Processed Message Marker                                                │
│   - Change output                                                           │
└─────────────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────────┐
│                    PHASE 2: Process Stored Message                          │
├─────────────────────────────────────────────────────────────────────────────┤
│ INPUTS:                                                                     │
│   - Message UTXO (with ProcessStoredMessage redeemer)                      │
│   - Deferred State UTXO (with ContractAction redeemer)                          │
│   - Any additional inputs needed for processing                             │
│                                                                             │
│ BURNS:                                                                      │
│   - Message NFT (proves message was consumed)                               │
│                                                                             │
│ OUTPUTS:                                                                    │
│   - Deferred continuation (messages_processed += 1)                             │
│   - Custom outputs (your business logic)                                   │
│   - Change output                                                           │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Data Structures

#### StoredMessageDatum (stored in Message UTXO)

```aiken
type StoredMessageDatum {
  origin: Domain,           // Source chain
  sender: HyperlaneAddress, // Sender on source chain
  body: ByteArray,          // Message payload
  message_id: ByteArray,    // 32-byte message ID (matches NFT asset name)
  nonce: Int,               // Message nonce for ordering
}
```

#### DeferredInner (in recipient state UTXO)

```aiken
type DeferredInner {
  messages_stored: Int,     // Count of messages received
  messages_processed: Int,  // Count of messages processed
}
```

#### MessageNftRedeemer

```aiken
type MessageNftRedeemer {
  MintMessage   // Used by relayer when storing message
  BurnMessage   // Used when processing message
}
```

### Implementing a Deferred Recipient

#### 1. Deploy the Message NFT Minting Policy

```aiken
use types.{MessageNftRedeemer, PolicyId}

/// Message NFT minting policy
/// Parameterized by the mailbox policy ID for security
validator stored_message_nft(mailbox_policy_id: PolicyId) {
  mint(redeemer: MessageNftRedeemer, own_policy: ByteArray, tx: Transaction) {
    when redeemer is {
      MintMessage -> {
        // Only allow minting when mailbox NFT is in inputs
        // This proves the message went through proper Hyperlane validation
        expect Some(_) = find_mailbox_input(tx, mailbox_policy_id)

        // Asset name must be 32 bytes (message_id)
        expect [(asset_name, 1)] = get_minted_assets(tx, own_policy)
        expect bytearray.length(asset_name) == 32

        True
      }
      BurnMessage -> {
        // Always allow burning (cleanup)
        expect [(_, -1)] = get_minted_assets(tx, own_policy)
        True
      }
    }
  }
}
```

#### 2. Deploy the Deferred Recipient

```aiken
use types.{
  HyperlaneRecipientDatum, HyperlaneRecipientRedeemer, PolicyId,
  StoredMessageDatum, DeferredAction, DeferredInner,
}

validator deferred_recipient(mailbox_policy_id: PolicyId, message_nft_policy: PolicyId) {
  spend(
    datum: Option<HyperlaneRecipientDatum<DeferredInner>>,
    redeemer: HyperlaneRecipientRedeemer<DeferredAction>,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    expect Some(d) = datum

    when redeemer is {
      HandleMessage { origin, sender, body } -> {
        // Phase 1: Relayer stores message
        // Verify mailbox is spending (proves ISM validation)
        expect has_mailbox_input(tx, mailbox_policy_id)

        // Verify message NFT is being minted
        expect message_nft_minted(tx, message_nft_policy)

        // Verify message UTXO is created with correct datum
        expect message_utxo_created(tx, origin, sender, body)

        // Update state
        let new_inner = DeferredInner {
          messages_stored: d.inner.messages_stored + 1,
          messages_processed: d.inner.messages_processed,
        }

        validate_continuation(d, new_inner, tx, own_ref)
      }

      ContractAction { action: ProcessStoredMessage { message_id } } -> {
        // Phase 2: Process stored message
        // Verify message NFT is being burned
        expect message_nft_burned(tx, message_nft_policy, message_id)

        // Update state
        let new_inner = DeferredInner {
          messages_stored: d.inner.messages_stored,
          messages_processed: d.inner.messages_processed + 1,
        }

        // Processor can create any outputs it needs
        validate_continuation(d, new_inner, tx, own_ref)
      }
    }
  }
}
```

#### 3. Register the Deferred Recipient

```bash
# Register your Deferred recipient
hyperlane-cardano registry register \
    --script-hash "your_deferred_recipient_hash" \
    --state-policy "your_state_nft_policy" \
    --state-asset "your_state_nft_name" \
    --recipient-type deferred-recipient \
    --message-policy "your_message_nft_policy" \
    --signing-key your_key.skey \
    --network preprod
```

### Processing Stored Messages

To process stored messages, you need to:

1. **Monitor for new Message UTXOs**: Query the blockchain for UTXOs at your Deferred recipient address that contain your message NFT policy.

2. **Parse StoredMessageDatum**: Decode the datum to get message details (origin, sender, body, message_id, nonce).

3. **Build custom outputs**: Based on your business logic, create the appropriate transaction outputs.

4. **Submit ProcessStoredMessage transaction**:
   - Spend the Message UTXO
   - Burn the Message NFT
   - Create your custom outputs
   - Update the Deferred recipient state

### Using the CLI for Deferred Message Processing

The Hyperlane Cardano CLI provides commands specifically for working with deferred messages. These commands are designed for the `example_deferred_recipient` pattern but demonstrate the general approach.

#### List Pending Messages

Query for all pending (unprocessed) messages at a deferred recipient:

```bash
# List pending messages in table format
hyperlane-cardano deferred list \
  --recipient-address addr_test1wz... \
  --message-nft-policy abc123...

# List pending messages in JSON format (for scripting)
hyperlane-cardano deferred list \
  --recipient-address addr_test1wz... \
  --message-nft-policy abc123... \
  --format json
```

Example output:
```
Listing pending deferred messages...
  Recipient: addr_test1wz...
  NFT Policy: abc123...

Found 2 pending message(s):

UTXO                                                                   Message ID                                                         Lovelace
------------------------------------------------------------------------------------------------------------------------------------------------------
a1b2c3d4e5f6...#0    0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef      2000000
f6e5d4c3b2a1...#1    fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210      2000000
```

#### Show Message Details

View the full details of a specific message UTXO:

```bash
hyperlane-cardano deferred show \
  --message-utxo "a1b2c3d4e5f6...#0"
```

Example output:
```
Fetching message details...

Message UTXO Details:
  TX Hash: a1b2c3d4e5f6...
  Output Index: 0
  Address: addr_test1wz...
  Lovelace: 2000000

  Assets:
    - abc123....0123456789abcdef...: 1

  StoredMessageDatum:
    Origin: 1
    Sender: 000000000000000000000000deadbeef...
    Message ID: 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
    Nonce: 42
    Body (32 bytes): 48656c6c6f20576f726c6421...
```

#### Process a Message (Example)

Process a deferred message using the example_deferred_recipient pattern:

```bash
# Dry run first to see what would happen
hyperlane-cardano deferred process \
  --message-utxo "a1b2c3d4e5f6...#0" \
  --recipient-state-policy def456... \
  --message-nft-policy abc123... \
  --recipient-ref-script "ref_tx_hash#0" \
  --nft-ref-script "nft_ref_tx_hash#0" \
  --dry-run

# Actually submit the transaction
hyperlane-cardano deferred process \
  --message-utxo "a1b2c3d4e5f6...#0" \
  --recipient-state-policy def456... \
  --message-nft-policy abc123... \
  --recipient-ref-script "ref_tx_hash#0" \
  --nft-ref-script "nft_ref_tx_hash#0"
```

**Parameters:**
- `--message-utxo`: The UTXO containing the stored message (format: `txhash#index`)
- `--recipient-state-policy`: Policy ID of the recipient's state NFT (used to find the state UTXO)
- `--recipient-state-asset`: Asset name of the state NFT (default: empty for unit token)
- `--message-nft-policy`: Policy ID of the message NFTs (used for burning)
- `--recipient-ref-script`: Optional reference script UTXO for the recipient validator
- `--nft-ref-script`: Optional reference script UTXO for the message NFT policy
- `--dry-run`: Build but don't submit the transaction

**Note:** The `process` command is specifically for the `example_deferred_recipient` contract pattern. For custom deferred recipients with different business logic, you'll need to implement your own processor that builds appropriate outputs.

### Custom Processor Implementation

For production deferred recipients, you'll typically write a custom processor. Here's a general approach:

```rust
// Pseudocode for a custom deferred message processor

async fn process_pending_messages(
    client: &BlockfrostClient,
    recipient_address: &str,
    message_nft_policy: &str,
) -> Result<()> {
    // 1. Find all pending message UTXOs
    let utxos = client.get_utxos(recipient_address).await?;
    let message_utxos: Vec<_> = utxos
        .iter()
        .filter(|u| u.has_asset(message_nft_policy))
        .collect();

    for message_utxo in message_utxos {
        // 2. Parse the StoredMessageDatum
        let datum = parse_stored_message_datum(&message_utxo.inline_datum)?;

        // 3. Execute your business logic based on message content
        let custom_outputs = process_message_body(&datum.body)?;

        // 4. Build transaction:
        //    - Spend message UTXO (with ProcessStoredMessage redeemer)
        //    - Spend state UTXO (with ContractAction redeemer)
        //    - Burn message NFT
        //    - Create custom outputs
        //    - Create state continuation (messages_processed += 1)
        let tx = build_process_tx(message_utxo, custom_outputs)?;

        // 5. Sign and submit
        let signed_tx = sign_tx(tx)?;
        client.submit_tx(&signed_tx).await?;
    }

    Ok(())
}
```

Example query (using Blockfrost API directly):
```bash
# Find message UTXOs with your NFT policy
curl "https://cardano-preprod.blockfrost.io/api/v0/addresses/${DEFERRED_RECIPIENT_ADDRESS}/utxos/${MESSAGE_NFT_POLICY}" \
  -H "project_id: ${BLOCKFROST_API_KEY}"
```

### Security Considerations for Deferred

1. **Message NFT proves legitimacy**: The message NFT can only be minted when the mailbox is spending its UTXO (which requires ISM verification). This proves the message is authentic.

2. **NFT burn proves consumption**: The NFT must be burned when processing, ensuring each message is processed exactly once.

3. **State tracking**: The `messages_stored` and `messages_processed` counters help track the backlog of unprocessed messages.

4. **Processor authorization**: Consider adding authorization checks in `ProcessStoredMessage` to control who can process messages.

## Security Considerations

### Always Verify Mailbox Caller

Your `HandleMessage` handler MUST verify that the mailbox script is also spending its UTXO in the same transaction:

```aiken
fn handle_message(..., mailbox_hash: ScriptHash) -> Bool {
  // This is CRITICAL for security
  expect has_script_input(tx, mailbox_hash)

  // Only now process the message...
}
```

This ensures:
1. The message was validated by the ISM
2. A processed message marker will be created (preventing replay)
3. The full Hyperlane security guarantees apply

### Validate Origin and Sender

You may want to restrict which origins and senders can call your contract:

```aiken
// Only accept messages from Ethereum (domain 1)
expect origin == 1

// Only accept from your trusted sender contract
let expected_sender = #"00000000your_trusted_sender_address_here"
expect sender == expected_sender
```

### Custom ISM

For critical applications, consider using a custom ISM:

```aiken
type MyDatum = HyperlaneRecipientDatum<MyState> {
  ism: Some(my_custom_ism_hash),
  ...
}
```

## Testing Your Recipient

### Local Testing

1. Deploy your recipient on preprod testnet
2. Send a test message from another chain (or use the testnet faucet)
3. Monitor the relayer logs to see your message being processed

### Integration Testing

```bash
# Query the registry to verify your registration
# (Use Blockfrost or cardano-cli)

# Send a test message through Hyperlane
# (From an EVM testnet or other connected chain)

# Verify message delivery by checking your contract state
```

## Common Issues

### "Recipient not registered"
- Ensure your registration is in the registry
- Verify the script hash matches exactly

### "State UTXO not found"
- Check that your state NFT exists
- Verify the policy ID and asset name in registration

### "Mailbox validation failed"
- Ensure you're checking `has_script_input(tx, mailbox_hash)`
- The mailbox hash must be the correct one for the network

### "ISM verification failed"
- Check that your custom ISM (if any) is correctly configured
- Ensure the relayer has access to validator signatures

## Example Contracts

See the `contracts/validators/` directory for example implementations:

### Example Recipients
- `example_generic_recipient.ak` - Basic Generic recipient example (immediate processing)
- `example_deferred_recipient.ak` - Deferred recipient example (deferred processing)

### Core Contracts
- `mailbox.ak` - The mailbox contract (for understanding the message flow)
- `multisig_ism.ak` - The default multisig ISM (signature verification)

### Deferred Components
- `deferred_recipient.ak` - Production Deferred recipient validator
- `stored_message_nft.ak` - Message NFT minting policy for Deferred

## Support

- [Hyperlane Discord](https://discord.gg/hyperlane)
- [Hyperlane Documentation](https://docs.hyperlane.xyz)
- [GitHub Issues](https://github.com/hyperlane-xyz/hyperlane-monorepo/issues)

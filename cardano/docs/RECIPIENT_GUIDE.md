# Hyperlane Cardano Recipient Developer Guide

This guide explains how to build a Hyperlane-compatible recipient contract on Cardano.

## Overview

On Cardano, Hyperlane recipients are Plutus V3 scripts that can receive cross-chain messages from the Hyperlane Mailbox. The relayer network handles:

1. **Message Discovery**: Indexing messages dispatched from other chains
2. **ISM Verification**: Gathering validator signatures to prove message authenticity
3. **Transaction Building**: Constructing the Cardano transaction to deliver the message
4. **Message Delivery**: Submitting the transaction to process the message

## Recipient Contract Pattern

### Recommended Datum Structure

The `HyperlaneRecipientDatum` wrapper is available for recipients that need custom ISM support or nonce tracking:

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

Simpler recipients (like `greeting.ak`) can define their own datum type directly without this wrapper.

### Required Redeemer Structure

```aiken
/// Standard redeemer for recipients
/// SECURITY: Recipients MUST verify keccak256(encode_message(message)) == message_id
type HyperlaneRecipientRedeemer<contract_redeemer> {
  /// Handle incoming Hyperlane message
  HandleMessage { message: Message, message_id: ByteArray }
  /// Contract-specific actions
  ContractAction { action: contract_redeemer }
}
```

The `message_id` is what the ISM validated. Recipients must verify `keccak256(encode_message(message)) == message_id` to ensure the message content is authentic.

### Example Recipient Contract (Greeting Pattern)

The reference implementation is the `greeting.ak` contract. Generic recipients are parameterized by `verified_message_nft_policy` and handle two types of UTXOs at their script address: **state UTXOs** (with the contract's datum) and **message UTXOs** (with a `verified_message_nft`, no typed datum).

```aiken
use types.{Message, PolicyId, encode_message}

/// Your contract's state
type GreetingDatum {
  last_greeting: ByteArray,
  greeting_count: Int,
}

/// Redeemer includes the full message and its ID
type GreetingRedeemer {
  HandleMessage {
    message: Message,
    message_id: ByteArray,
  }
}

/// Validator parameterized by the verified_message_nft policy
validator greeting(verified_message_nft_policy: PolicyId) {
  spend(
    datum: Option<GreetingDatum>,
    redeemer: GreetingRedeemer,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    expect Some(own_input) = find_input(tx, own_ref)
    let HandleMessage { message, message_id } = redeemer

    when datum is {
      Some(old_datum) -> {
        // State UTXO: update the greeting
        let greeting = bytearray.concat("Hello, ", message.body)
        let own_addr = own_input.output.address

        // Find continuation output
        expect Some(continuation) =
          list.find(tx.outputs, fn(output) { output.address == own_addr })
        expect InlineDatum(raw_datum) = continuation.datum
        expect new_datum: GreetingDatum = raw_datum

        // Verify message authenticity
        expect keccak_256(encode_message(message)) == message_id

        // Verify the verified_message_nft is being burned
        expect verified_nft_burned(tx, verified_message_nft_policy, message_id)

        // Verify a message UTXO with the NFT is being spent
        expect message_utxo_spent(tx, own_addr, verified_message_nft_policy, message_id, own_ref)

        new_datum.last_greeting == greeting
          && new_datum.greeting_count == old_datum.greeting_count + 1
      }
      None ->
        // Message UTXO (no typed datum): allow spending if the NFT is burned
        verified_nft_burned(tx, verified_message_nft_policy, message_id)
    }
  }

  else(_) { fail }
}
```

Key points:
- The `HandleMessage` redeemer carries the full `Message` and `message_id`
- The contract verifies `keccak256(encode_message(message)) == message_id`
- The `verified_message_nft` burn proves the mailbox created the message (ISM-validated)
- The `None` datum branch handles the message UTXO (which has no contract-specific datum)

> **Note:** WarpRoute recipients use a different pattern -- they are spent as inputs in the same Process TX alongside the mailbox, verifying the mailbox is co-spending via `has_script_input(tx, mailbox_hash)`. This is possible because the relayer knows exactly how to build WarpRoute outputs.

## State UTXO Pattern

Your recipient must have a **state UTXO** that:

1. Is at your script address
2. Contains an NFT marker for unique identification
3. Stores your contract state in an inline datum

The NFT marker pattern:

- Mint a unique NFT (policy ID + asset name)
- The NFT stays in the state UTXO
- The relayer uses this NFT to find your state UTXO

## Recipient Addressing

On Cardano, there is no separate registry contract. A recipient's Hyperlane address is derived directly from its state NFT policy ID.

### How It Works

The addressing scheme depends on the recipient type:

**Warp routes** use NFT-policy addressing (`0x01` prefix):
1. Hyperlane address = `0x01000000{state_nft_policy_id}` (32 bytes)
2. The relayer discovers warp routes via O(1) NFT query
3. Warp routes are TokenReceivers — spent in the same TX as the mailbox

**Generic recipients** (e.g., greeting) use script-hash addressing (`0x02` prefix):
1. Hyperlane address = `0x02000000{script_hash}` (32 bytes)
2. The relayer discovers recipients by querying UTXOs at the script address
3. Generic recipients use two-phase verified message delivery

Remote chains enroll the appropriate address format. No registration transaction is needed.

## Transaction Flow

Generic recipients (like `greeting.ak`) use a two-phase message delivery. WarpRoute recipients are handled differently -- see the note at the end.

### Phase 1: Process TX (relayer delivers message)

The mailbox Process TX creates a `verified_message_nft` UTXO at the recipient's script address. The recipient is **not** spent as an input in this TX.

```
┌─────────────────────────────────────────────────────────────────┐
│                  Phase 1: Process Transaction                    │
├─────────────────────────────────────────────────────────────────┤
│ INPUTS:                                                          │
│   - Mailbox UTXO (with Process redeemer)                        │
│   - ISM UTXO (spent for verification)                           │
│   - Fee payment UTXOs                                            │
│                                                                  │
│ MINTS:                                                           │
│   - verified_message_nft (asset name = message_id)              │
│                                                                  │
│ OUTPUTS:                                                         │
│   - Mailbox continuation (unchanged datum)                      │
│   - ISM continuation (unchanged)                                │
│   - Message UTXO at recipient address (VerifiedMessageDatum     │
│     + verified_message_nft)                              <-- NEW │
│   - Processed Message Marker (prevents replay)                  │
│   - Change output                                                │
└─────────────────────────────────────────────────────────────────┘
```

### Phase 2: Receive TX (anyone handles the message)

A separate transaction spends the message UTXO and the recipient's state UTXO, burns the NFT, and updates the recipient state.

```
┌─────────────────────────────────────────────────────────────────┐
│                  Phase 2: Receive Transaction                    │
├─────────────────────────────────────────────────────────────────┤
│ INPUTS:                                                          │
│   - Message UTXO at recipient address (verified_message_nft)    │
│   - Recipient State UTXO (with HandleMessage redeemer)          │
│   - Fee payment UTXOs                                            │
│                                                                  │
│ BURNS:                                                           │
│   - verified_message_nft (proves message was consumed)          │
│                                                                  │
│ OUTPUTS:                                                         │
│   - Recipient State continuation (updated datum)                │
│   - Change output                                                │
└─────────────────────────────────────────────────────────────────┘
```

> **WarpRoute recipients** are handled in a single TX -- the relayer includes the warp route script as a spent input in the same Process TX alongside the mailbox. The relayer knows how to build WarpRoute outputs, so no second phase is needed.

## Two-Phase Message Delivery (Verified Message Pattern)

This is the **default pattern** for generic (non-WarpRoute) recipients on Cardano. The Hyperlane relayer delivers messages to recipients using a two-phase approach:

- **Phase 1 (Process TX)**: The mailbox mints a `verified_message_nft` and creates a UTXO at the recipient's script address containing the NFT and a `VerifiedMessageDatum`.
- **Phase 2 (Receive TX)**: Anyone spends the message UTXO together with the recipient's state UTXO, burning the NFT and updating the recipient state.

This pattern exists because the relayer cannot know how to build arbitrary recipient outputs. The recipient team (or an automated service, dApp, or manual process) handles the second phase.

### When This Applies

All generic recipients use this pattern, including recipients that need to:

- Create complex or variable output UTXOs that the relayer cannot predict
- Interact with external protocols requiring custom transaction building
- Implement business logic that requires off-chain computation
- Handle messages asynchronously with human intervention

### Data Structures

#### VerifiedMessageDatum (in message UTXO)

Created by the mailbox during Process and placed at the recipient's script address:

```aiken
type VerifiedMessageDatum {
  origin: Domain,           // Source chain
  sender: HyperlaneAddress, // Sender on source chain
  body: ByteArray,          // Message payload
  message_id: ByteArray,    // 32-byte message ID (matches NFT asset name)
  nonce: Int,               // Message nonce for ordering
}
```

#### MessageNftRedeemer

```aiken
type MessageNftRedeemer {
  MintMessage   // Used by mailbox when creating the message UTXO
  BurnMessage   // Used when receiving/consuming the message
}
```

### The verified_message_nft Minting Policy

The `verified_message_nft` policy is parameterized by the mailbox policy ID. It only allows minting when the mailbox NFT is present in the transaction inputs (proving the message went through ISM verification):

```aiken
use types.{MessageNftRedeemer, MintMessage, BurnMessage, PolicyId}

validator verified_message_nft(mailbox_policy_id: PolicyId) {
  mint(redeemer: MessageNftRedeemer, own_policy: ByteArray, tx: Transaction) {
    when redeemer is {
      MintMessage -> {
        // Only allow minting when mailbox NFT is in inputs
        // This proves the message went through proper Hyperlane validation
        let mailbox_involved = list.any(tx.inputs, fn(input) {
          !dict.is_empty(assets.tokens(input.output.value, mailbox_policy_id))
        })

        // Exactly one NFT minted with 32-byte asset name (message_id)
        let own_mints = assets.tokens(tx.mint, own_policy)
        let mint_pairs = dict.to_pairs(own_mints)
        let valid_mint = list.any(mint_pairs, fn(pair) {
          let Pair(asset_name, quantity) = pair
          bytearray.length(asset_name) == 32 && quantity == 1
        })

        mailbox_involved && list.length(mint_pairs) == 1 && valid_mint
      }
      BurnMessage -> {
        // Allow burning NFTs (32-byte asset names, negative quantities)
        let own_mints = assets.tokens(tx.mint, own_policy)
        list.all(dict.to_pairs(own_mints), fn(pair) {
          let Pair(asset_name, quantity) = pair
          bytearray.length(asset_name) == 32 && quantity < 0
        })
      }
    }
  }
}
```

### Receiving Verified Messages

To receive (process) verified messages, you need to:

1. **Monitor for new message UTXOs**: Query the blockchain for UTXOs at your recipient script address that contain a `verified_message_nft`.

2. **Parse VerifiedMessageDatum**: Decode the datum to get message details (origin, sender, body, message_id, nonce).

3. **Build the receive transaction**: Spend the message UTXO and your state UTXO, burn the verified_message_nft, and create your updated state output.

### Using the CLI for Message Receiving

The Hyperlane Cardano CLI provides commands for listing and receiving verified messages.

#### List Pending Messages

Query for all pending (unprocessed) messages at a recipient:

```bash
# List pending messages in table format
hyperlane-cardano message list \
  --recipient-address addr_test1wz...

# List pending messages in JSON format (for scripting)
hyperlane-cardano message list \
  --recipient-address addr_test1wz... \
  --format json
```

The `--message-nft-policy` flag is auto-derived from `deployment_info.json` if omitted.

#### Show Message Details

View the full details of a specific message UTXO:

```bash
hyperlane-cardano message show \
  --message-utxo "a1b2c3d4e5f6...#0"
```

Example output:

```
Message UTXO Details:
  TX Hash: a1b2c3d4e5f6...
  Output Index: 0
  Address: addr_test1wz...
  Lovelace: 2000000

  Assets:
    - abc123....0123456789abcdef...: 1

  VerifiedMessageDatum:
    Origin: 1
    Sender: 000000000000000000000000deadbeef...
    Message ID: 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
    Nonce: 42
    Body (32 bytes): 48656c6c6f20576f726c6421...
```

#### Receive a Message

Receive a verified message (spend message UTXO, burn NFT, update recipient state):

```bash
# Dry run first to see what would happen
hyperlane-cardano message receive \
  --message-utxo "a1b2c3d4e5f6...#0" \
  --recipient-policy def456... \
  --dry-run

# Actually submit the transaction
hyperlane-cardano message receive \
  --message-utxo "a1b2c3d4e5f6...#0" \
  --recipient-policy def456...
```

**Parameters:**

- `--message-utxo`: The UTXO containing the verified message (format: `txhash#index`)
- `--recipient-policy`: Policy ID of the recipient's state NFT (used to find the state UTXO)
- `--recipient-state-asset`: Asset name of the state NFT (default: empty for unit token)
- `--message-nft-policy`: Policy ID of the verified message NFTs (auto-derived if omitted)
- `--recipient-ref-script`: Reference script UTXO for the recipient validator (auto-discovered if omitted)
- `--nft-ref-script`: Reference script UTXO for the verified_message_nft policy
- `--recipient-redeemer`: CBOR hex of recipient spend redeemer (for script-based recipients)
- `--recipient-new-datum`: CBOR hex of updated state datum
- `--dry-run`: Build but don't submit the transaction

### Custom Processor Implementation

For recipients with complex business logic, you can write a custom processor:

```rust
// Pseudocode for a custom message processor

async fn process_pending_messages(
    client: &BlockfrostClient,
    recipient_address: &str,
    verified_nft_policy: &str,
) -> Result<()> {
    // 1. Find all pending message UTXOs
    let utxos = client.get_utxos(recipient_address).await?;
    let message_utxos: Vec<_> = utxos
        .iter()
        .filter(|u| u.has_asset(verified_nft_policy))
        .collect();

    for message_utxo in message_utxos {
        // 2. Parse the VerifiedMessageDatum
        let datum = parse_verified_message_datum(&message_utxo.inline_datum)?;

        // 3. Execute your business logic based on message content
        let new_state = compute_new_state(&datum)?;

        // 4. Build transaction:
        //    - Spend message UTXO (burns verified_message_nft)
        //    - Spend state UTXO (with HandleMessage redeemer)
        //    - Create state continuation (updated datum)
        let tx = build_receive_tx(message_utxo, new_state)?;

        // 5. Sign and submit
        let signed_tx = sign_tx(tx)?;
        client.submit_tx(&signed_tx).await?;
    }

    Ok(())
}
```

Example query (using Blockfrost API directly):

```bash
# Find message UTXOs with your verified_message_nft policy
curl "https://cardano-preprod.blockfrost.io/api/v0/addresses/${RECIPIENT_ADDRESS}/utxos/${VERIFIED_NFT_POLICY}" \
  -H "project_id: ${BLOCKFROST_API_KEY}"
```

### Security Considerations for Verified Messages

1. **NFT proves legitimacy**: The `verified_message_nft` can only be minted when the mailbox NFT is present in the transaction inputs (which requires ISM verification). This proves the message is authentic.

2. **NFT burn proves consumption**: The NFT must be burned when receiving, ensuring each message is processed exactly once.

3. **Message ID integrity**: Recipients verify `keccak256(encode_message(message)) == message_id` to ensure the message content matches the NFT's asset name.

## Security Considerations

### Always Verify Message Authenticity

How you verify authenticity depends on your recipient type:

**Generic recipients** (two-phase pattern): Verify the `verified_message_nft` is being burned. This proves the mailbox created the message (and thus the ISM validated it):

```aiken
// Verify message authenticity via NFT burn
expect keccak_256(encode_message(message)) == message_id
expect verified_nft_burned(tx, verified_message_nft_policy, message_id)
expect message_utxo_spent(tx, own_addr, verified_message_nft_policy, message_id, own_ref)
```

**WarpRoute recipients** (single-TX pattern): Verify the mailbox script is co-spending its UTXO in the same transaction:

```aiken
// Verify mailbox is spending (WarpRoute pattern only)
expect has_script_input(tx, mailbox_hash)
```

Both patterns ensure:

1. The message was validated by the ISM
2. A processed message marker was created (preventing replay)
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
# Send a test message through Hyperlane
# (From an EVM testnet or other connected chain)

# Verify message delivery by checking your contract state
```

## Common Issues

### "State UTXO not found"

- Check that your state NFT exists on-chain
- Verify the NFT policy ID matches the Hyperlane address enrolled on the remote chain
- Ensure the state UTXO contains the NFT with the expected policy ID

### "Mailbox validation failed"

- For WarpRoute recipients: ensure you're checking `has_script_input(tx, mailbox_hash)` and the mailbox hash is correct for the network
- For generic recipients: ensure the `verified_message_nft_policy` parameter matches the deployed policy

### "ISM verification failed"

- Check that your custom ISM (if any) is correctly configured
- Ensure the relayer has access to validator signatures

## Example Contracts

See the `contracts/validators/` directory for implementations:

### Recipient Contracts

- `greeting.ak` - Reference generic recipient (two-phase verified message pattern)
- `warp_route.ak` - WarpRoute recipient (single-TX mailbox co-spending pattern)

### Core Contracts

- `mailbox.ak` - The mailbox contract (message dispatch and process flow)
- `multisig_ism.ak` - The default multisig ISM (signature verification)
- `verified_message_nft.ak` - NFT minting policy for verified message delivery

## Support

- [Hyperlane Discord](https://discord.gg/hyperlane)
- [Hyperlane Documentation](https://docs.hyperlane.xyz)
- [GitHub Issues](https://github.com/hyperlane-xyz/hyperlane-monorepo/issues)

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

1. **GenericHandler**: Basic state-in, state-out pattern. The relayer builds a transaction that spends your state UTXO and creates a continuation output.

2. **TokenReceiver**: For warp routes and token bridges. Can include:
   - `vault_locator`: UTXO holding locked tokens
   - `minting_policy`: For synthetic token minting

3. **ContractCaller**: For contracts that call other contracts:
   - `target_locator`: The downstream contract to call

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

- `generic_recipient.ak` - Basic recipient example
- `validators/mailbox.ak` - The mailbox contract (for understanding the flow)
- `validators/multisig_ism.ak` - The default multisig ISM

## Support

- [Hyperlane Discord](https://discord.gg/hyperlane)
- [Hyperlane Documentation](https://docs.hyperlane.xyz)
- [GitHub Issues](https://github.com/hyperlane-xyz/hyperlane-monorepo/issues)

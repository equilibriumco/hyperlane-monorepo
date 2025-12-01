# Hyperlane Recipient Developer Guide

This guide explains how to build Cardano smart contracts that can receive cross-chain messages via Hyperlane.

## Overview

Any Aiken smart contract can receive Hyperlane messages by:
1. Following the standard recipient interface
2. Registering in the Recipient Registry
3. Being discovered by the relayer via the registry

The relayer handles all transaction construction - no off-chain code required per recipient.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                  Hyperlane Message Flow                      │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  Origin Chain                    Cardano                    │
│  ┌──────────┐                   ┌──────────────┐            │
│  │ Dispatch │ ──── Relayer ───→ │   Mailbox    │            │
│  └──────────┘                   └──────┬───────┘            │
│                                        │                     │
│                                        ▼                     │
│                                 ┌──────────────┐            │
│                                 │   Registry   │            │
│                                 │  (lookup)    │            │
│                                 └──────┬───────┘            │
│                                        │                     │
│                                        ▼                     │
│                                 ┌──────────────┐            │
│                                 │  Your        │            │
│                                 │  Recipient   │            │
│                                 └──────────────┘            │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

## Recipient Types

### 1. GenericHandler

Simple message receiver that stores/processes message data.

**Use cases:**
- Cross-chain governance
- Oracle updates
- State synchronization

### 2. TokenReceiver

Receives tokens from other chains via warp routes.

**Variants:**
- **Collateral**: Release locked tokens from a vault
- **Synthetic**: Mint new tokens via a minting policy

### 3. ContractCaller

Forwards messages to another contract (advanced).

## Building a Recipient

### Step 1: Define Your Datum

Wrap your contract state with the Hyperlane recipient wrapper:

```aiken
use types.{HyperlaneRecipientDatum, ScriptHash}

// Your contract-specific state
type MyContractState {
  messages_received: Int,
  last_sender: ByteArray,
  data: ByteArray,
}

// Full datum type
type MyRecipientDatum = HyperlaneRecipientDatum<MyContractState>
```

The wrapper adds:
- `ism: Option<ScriptHash>` - Custom ISM override (use default if None)
- `last_processed_nonce: Option<Int>` - For message ordering (optional)
- `inner: T` - Your contract state

### Step 2: Define Your Redeemer

Use the standard redeemer wrapper:

```aiken
use types.{HyperlaneRecipientRedeemer, Domain, HyperlaneAddress}

// Your contract-specific actions
type MyContractAction {
  UpdateConfig { new_config: ByteArray }
  Withdraw { amount: Int }
}

// Full redeemer type
type MyRecipientRedeemer = HyperlaneRecipientRedeemer<MyContractAction>
```

The wrapper provides:
- `HandleMessage { origin, sender, body }` - Receive Hyperlane message
- `ContractAction { action: T }` - Your custom actions

### Step 3: Implement the Validator

```aiken
use aiken/transaction.{ScriptContext, Transaction, InlineDatum}
use aiken/list
use cardano/address.{Script}

validator my_recipient(mailbox_policy_id: PolicyId) {
  spend(
    datum: Option<MyRecipientDatum>,
    redeemer: MyRecipientRedeemer,
    own_ref: OutputReference,
    tx: Transaction,
  ) {
    expect Some(my_datum) = datum

    when redeemer is {
      HyperlaneRecipientRedeemer.HandleMessage { origin, sender, body } -> {
        // 1. REQUIRED: Verify mailbox is calling us
        expect mailbox_is_caller(mailbox_policy_id, tx)

        // 2. Process the message
        let new_state = process_message(my_datum.inner, origin, sender, body)

        // 3. Build new datum
        let new_datum = HyperlaneRecipientDatum {
          ..my_datum,
          inner: new_state,
        }

        // 4. Validate continuation UTXO
        validate_continuation(my_datum, new_datum, tx, own_ref)
      }

      HyperlaneRecipientRedeemer.ContractAction { action } -> {
        // Handle your contract-specific actions
        when action is {
          MyContractAction.UpdateConfig { new_config } -> {
            // Your logic here
            True
          }
          MyContractAction.Withdraw { amount } -> {
            // Your logic here
            True
          }
        }
      }
    }
  }

  else(_) {
    fail
  }
}

// CRITICAL: Verify the mailbox is calling this recipient
fn mailbox_is_caller(mailbox_policy_id: PolicyId, tx: Transaction) -> Bool {
  list.any(
    tx.inputs,
    fn(input) {
      // Check if any input contains the mailbox state NFT
      quantity_of(input.output.value, mailbox_policy_id, "") > 0
    },
  )
}

// Process incoming message
fn process_message(
  state: MyContractState,
  origin: Domain,
  sender: HyperlaneAddress,
  body: ByteArray,
) -> MyContractState {
  MyContractState {
    messages_received: state.messages_received + 1,
    last_sender: sender,
    data: body,
  }
}

// Validate state continuation
fn validate_continuation(
  old_datum: MyRecipientDatum,
  new_datum: MyRecipientDatum,
  tx: Transaction,
  own_ref: OutputReference,
) -> Bool {
  // Find own input
  expect Some(own_input) = find_input(tx, own_ref)
  let own_address = own_input.output.address
  let own_value = own_input.output.value

  // Find continuation output
  expect Some(continuation) =
    list.find(tx.outputs, fn(output) { output.address == own_address })

  // Verify datum updated correctly
  expect InlineDatum(cont_datum_data) = continuation.datum
  expect cont_datum: MyRecipientDatum = cont_datum_data
  expect cont_datum == new_datum

  // Verify value preserved (state NFT stays)
  lovelace_of(continuation.value) >= lovelace_of(own_value) &&
  // Ensure state NFT is preserved
  quantity_of(continuation.value, state_nft_policy, state_nft_name) == 1
}
```

### Step 4: Create State NFT Minting Policy

Each recipient needs a unique state NFT to identify its UTXO:

```aiken
// One-shot minting policy
validator state_nft(utxo_ref: OutputReference) {
  mint(_redeemer: Void, _policy_id: PolicyId, tx: Transaction) {
    // Can only mint if specific UTXO is consumed (one-time)
    let utxo_consumed = list.any(
      tx.inputs,
      fn(input) { input.output_reference == utxo_ref }
    )

    // Can only mint exactly 1 token
    let mint_value = value.from_minted_value(tx.mint)
    let total = // count tokens minted under this policy

    utxo_consumed && total == 1
  }
}
```

## Registering Your Recipient

### Using the CLI

```bash
# Set environment variables
export BLOCKFROST_API_KEY="your_key"
export CARDANO_SKEY_PATH="/path/to/signing.skey"
export CARDANO_NETWORK="preprod"

# Register a generic handler
./register-recipient.sh register \
    --script-hash "your_script_hash_28_bytes_hex" \
    --state-policy "state_nft_policy_id" \
    --state-asset "7374617465" \  # "state" in hex
    --recipient-type generic

# Register a token receiver with vault
./register-recipient.sh register \
    --script-hash "warp_route_hash" \
    --state-policy "state_nft_policy" \
    --state-asset "7374617465" \
    --recipient-type token-receiver \
    --vault-policy "vault_nft_policy" \
    --vault-asset "7661756c74"  # "vault" in hex

# Register with custom ISM
./register-recipient.sh register \
    --script-hash "your_script_hash" \
    --state-policy "state_nft_policy" \
    --state-asset "7374617465" \
    --recipient-type generic \
    --custom-ism "custom_ism_script_hash"
```

### Registration Datum Structure

```json
{
  "script_hash": "28-byte script hash",
  "state_locator": {
    "policy_id": "NFT policy ID",
    "asset_name": "NFT asset name"
  },
  "additional_inputs": [],
  "recipient_type": "GenericHandler | TokenReceiver | ContractCaller",
  "custom_ism": null
}
```

## Message Body Encoding

### For Generic Messages

The body is passed as-is to your `HandleMessage` handler. Define your own encoding.

**Example: JSON-like encoding**
```
body = "{'action':'update','value':42}"
```

### For Warp Route Transfers

Standard encoding for token transfers:
```
body = recipient_bytes || amount_u64_big_endian
```

Decoding in Aiken:
```aiken
fn decode_transfer(body: ByteArray) -> Option<(ByteArray, Int)> {
  let len = bytearray.length(body)
  if len < 8 {
    None
  } else {
    let amount_bytes = bytearray.slice(body, len - 8, len - 1)
    let amount = bytes_to_int(amount_bytes)
    let recipient = bytearray.slice(body, 0, len - 9)
    Some((recipient, amount))
  }
}
```

## Security Considerations

### 1. Always Verify Mailbox Caller

**CRITICAL**: Your validator MUST verify the mailbox is calling it:

```aiken
// DO THIS
expect mailbox_is_caller(mailbox_policy_id, tx)

// The mailbox has already:
// - Verified the ISM (signatures)
// - Checked message not replayed
// - Validated message format
```

### 2. Validate Sender

For sensitive operations, verify the sender is authorized:

```aiken
fn validate_sender(
  origin: Domain,
  sender: HyperlaneAddress,
  allowed_senders: List<(Domain, HyperlaneAddress)>,
) -> Bool {
  list.any(allowed_senders, fn(allowed) {
    allowed.1st == origin && allowed.2nd == sender
  })
}
```

### 3. Use Custom ISM When Needed

For high-value operations, configure a custom ISM with higher security:

```aiken
type MyRecipientDatum = HyperlaneRecipientDatum<MyState>

// Set custom ISM in datum
let datum = HyperlaneRecipientDatum {
  ism: Some(high_security_ism_hash),
  last_processed_nonce: None,
  inner: my_state,
}
```

### 4. Handle State Transitions Carefully

```aiken
// Good: Atomic state update
let new_state = MyState {
  ..old_state,
  balance: old_state.balance + amount,
}

// Bad: Non-deterministic state
let new_state = MyState {
  ..old_state,
  timestamp: get_current_time(),  // Don't do this
}
```

## Testing Your Recipient

### Unit Tests

```aiken
test my_recipient_handles_message() {
  let datum = mock_recipient_datum()
  let redeemer = HyperlaneRecipientRedeemer.HandleMessage {
    origin: 1,
    sender: mock_sender(),
    body: "test message",
  }

  let ctx = mock_context()
    .with_input(mock_mailbox_utxo())  // Mailbox is caller
    .with_input(mock_recipient_utxo(datum))
    .with_output(mock_recipient_continuation())

  my_recipient(mailbox_policy, datum, redeemer, ctx)
}

test my_recipient_rejects_non_mailbox_caller() fail {
  let datum = mock_recipient_datum()
  let redeemer = HyperlaneRecipientRedeemer.HandleMessage {
    origin: 1,
    sender: mock_sender(),
    body: "test message",
  }

  // NO mailbox input - should fail
  let ctx = mock_context()
    .with_input(mock_recipient_utxo(datum))
    .with_output(mock_recipient_continuation())

  my_recipient(mailbox_policy, datum, redeemer, ctx)
}
```

### Integration Testing

See [TESTING_GUIDE.md](./TESTING_GUIDE.md) for end-to-end testing with other chains.

## Example Recipients

### Simple Counter

```aiken
type CounterState {
  count: Int,
}

validator counter(mailbox_policy: PolicyId) {
  spend(datum, redeemer, own_ref, tx) {
    expect Some(d) = datum
    when redeemer is {
      HandleMessage { origin, sender, body } -> {
        expect mailbox_is_caller(mailbox_policy, tx)
        let new_datum = HyperlaneRecipientDatum {
          ..d,
          inner: CounterState { count: d.inner.count + 1 },
        }
        validate_continuation(d, new_datum, tx, own_ref)
      }
      _ -> False
    }
  }
}
```

### Cross-Chain Governance

```aiken
type GovernanceState {
  proposals: List<Proposal>,
  executed: List<ByteArray>,
}

type Proposal {
  id: ByteArray,
  action: ByteArray,
  votes: Int,
}

validator governance(mailbox_policy: PolicyId, allowed_origins: List<Domain>) {
  spend(datum, redeemer, own_ref, tx) {
    when redeemer is {
      HandleMessage { origin, sender, body } -> {
        expect mailbox_is_caller(mailbox_policy, tx)
        expect list.any(allowed_origins, fn(d) { d == origin })

        // Decode and execute governance action
        let action = decode_governance_action(body)
        execute_action(action, datum, tx, own_ref)
      }
      _ -> False
    }
  }
}
```

## Troubleshooting

### "Recipient not found in registry"

- Ensure your recipient is registered with the correct script hash
- Verify the state NFT policy and asset name are correct
- Check the registration transaction was confirmed

### "Mailbox not caller"

- The mailbox UTXO must be spent in the same transaction
- Verify the mailbox policy ID is correct in your validator

### "UTXO not found"

- The state NFT must exist at your script address
- Verify the NFT was minted and sent to the correct address
- Check for UTXO contention (someone else consumed it)

### "ISM verification failed"

- Ensure validators have signed the checkpoint
- Check the threshold is met for the origin domain
- Verify signatures are correctly formatted

## Resources

- [Hyperlane Documentation](https://docs.hyperlane.xyz/)
- [Aiken Documentation](https://aiken-lang.org/)
- [Cardano eUTXO Model](https://docs.cardano.org/learn/eutxo-explainer)
- [Example Contracts](../../../hyperlane-cardano/contracts/validators/)

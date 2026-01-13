[← Epic 4: Advanced Features](./EPIC.md) | [Epics Overview](../README.md)

# Task 4.4: NFT-Based Contract Identity

**Status:** ⬜ Not Started
**Complexity:** High
**Depends On:** None

## Objective

Implement a stable identity system using NFTs that allows contracts to be upgraded independently without cascading redeployments.

## Problem Statement

Currently, contracts are parameterized by each other's policy IDs:

```
processed_msg_nft(mailbox_policy_id)    → tied to mailbox identity
recipient(mailbox_policy_id)             → tied to mailbox identity
```

When the mailbox code changes:
1. Mailbox policy ID changes (new code = new hash)
2. `processed_msg_nft` must be redeployed with new parameter
3. All recipients must be redeployed with new parameter
4. Registry entries must be updated
5. Old processed message NFTs are at wrong policy (replay protection breaks)

## Solution: Identity NFT Pattern

Create a one-shot "identity NFT" that represents the mailbox's identity. The NFT never changes - only the contract holding it does.

```
┌─────────────────────────────────────────────────────────────────┐
│                    IDENTITY NFT PATTERN                          │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│   1. Deploy mailbox_identity_nft policy (one-shot, at genesis)  │
│      - Policy ID is stable forever                              │
│      - Mints exactly one NFT: "Mailbox Identity"                │
│                                                                  │
│   2. Mailbox v1 holds the identity NFT                          │
│      - Mailbox address can change                               │
│      - Identity (NFT policy) stays constant                     │
│                                                                  │
│   3. Other contracts check for identity NFT, not policy ID      │
│      - processed_msg_nft: "mailbox_identity_nft in inputs?"     │
│      - recipient: "mailbox_identity_nft in inputs?"             │
│                                                                  │
│   4. Upgrade: migrate identity NFT to new mailbox               │
│      - Old mailbox releases NFT                                 │
│      - New mailbox receives NFT                                 │
│      - All other contracts continue working                     │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

## Implementation

### 1. New Contract: `mailbox_identity_nft.ak`

A one-shot minting policy for the mailbox identity NFT:

```aiken
/// One-shot identity NFT for mailbox
/// Minted once at deployment, held by mailbox contract
/// Used by other contracts to identify the "real" mailbox
validator mailbox_identity_nft(seed_utxo: OutputReference) {
  mint(redeemer: Data, own_policy: ByteArray, tx: Transaction) {
    // One-shot: can only mint if seed UTXO is consumed
    let seed_consumed = list.any(tx.inputs, fn(i) {
      i.output_reference == seed_utxo
    })

    // Must mint exactly 1 token with empty name
    let own_mints = assets.tokens(tx.mint, own_policy)
    let valid_mint = dict.to_pairs(own_mints) == [Pair("", 1)]

    seed_consumed && valid_mint
  }

  else(_) {
    fail
  }
}
```

### 2. Modify `processed_message_nft.ak`

Change from checking policy ID to checking identity NFT:

```aiken
// BEFORE: parameterized by mailbox_policy_id
validator processed_message_nft(mailbox_policy_id: PolicyId) {
  mint(_redeemer: Data, own_policy: ByteArray, tx: Transaction) {
    // Check mailbox NFT (state NFT) is in inputs
    let mailbox_involved = list.any(tx.inputs, fn(input) {
      let policy_tokens = assets.tokens(input.output.value, mailbox_policy_id)
      !dict.is_empty(policy_tokens)
    })
    // ...
  }
}

// AFTER: parameterized by mailbox_identity_nft_policy
validator processed_message_nft(mailbox_identity_policy: PolicyId) {
  mint(_redeemer: Data, own_policy: ByteArray, tx: Transaction) {
    // Check mailbox IDENTITY NFT is in inputs (or reference inputs)
    let mailbox_involved = list.any(tx.inputs, fn(input) {
      let identity_tokens = assets.tokens(input.output.value, mailbox_identity_policy)
      !dict.is_empty(identity_tokens)
    }) || list.any(tx.reference_inputs, fn(ref) {
      let identity_tokens = assets.tokens(ref.output.value, mailbox_identity_policy)
      !dict.is_empty(identity_tokens)
    })
    // ...
  }
}
```

### 3. Modify `example_generic_recipient.ak`

Same pattern - check for identity NFT:

```aiken
// BEFORE: parameterized by mailbox_policy_id
validator example_generic_recipient(mailbox_policy_id: PolicyId) {
  // ...
  fn mailbox_is_caller(inputs: List<Input>) -> Bool {
    list.any(inputs, fn(input) {
      let tokens = assets.tokens(input.output.value, mailbox_policy_id)
      !dict.is_empty(tokens)
    })
  }
}

// AFTER: parameterized by mailbox_identity_policy
validator example_generic_recipient(mailbox_identity_policy: PolicyId) {
  // ...
  fn mailbox_is_caller(inputs: List<Input>) -> Bool {
    list.any(inputs, fn(input) {
      let tokens = assets.tokens(input.output.value, mailbox_identity_policy)
      !dict.is_empty(tokens)
    })
  }
}
```

### 4. Mailbox Upgrade Mechanism

Add a `Migrate` redeemer to the mailbox to enable upgrades:

```aiken
type MailboxRedeemer {
  Dispatch { destination: Domain, recipient: HyperlaneAddress, body: ByteArray }
  Process { message: Message, metadata: ByteArray, message_id: ByteArray }
  SetDefaultIsm { new_ism: ScriptHash }
  TransferOwnership { new_owner: VerificationKeyHash }
  Migrate { new_mailbox_address: Address }  // NEW
}

// In mailbox validator
Migrate { new_mailbox_address } -> {
  // Only owner can migrate
  expect is_signed_by(tx, datum.owner)

  // Identity NFT must go to new address
  expect identity_nft_sent_to(tx, mailbox_identity_policy, new_mailbox_address)

  // State NFT must go to new address (carries the datum)
  expect state_nft_sent_to(tx, own_policy, new_mailbox_address)

  True
}
```

## Deployment Sequence

### Initial Deployment (New Chain)

1. Mint `mailbox_identity_nft` with seed UTXO
2. Deploy mailbox holding the identity NFT
3. Deploy `processed_message_nft(mailbox_identity_policy)`
4. Deploy recipients with `mailbox_identity_policy` parameter

### Upgrade Existing Deployment

For existing deployments, a migration is needed:

1. Deploy new contracts with identity NFT pattern
2. Deploy `mailbox_identity_nft`
3. Initialize new mailbox with identity NFT
4. Migrate state from old mailbox to new (manual process)
5. Deploy new recipients with identity policy parameter
6. Update registry entries
7. Deprecate old contracts

## Files to Create

| File | Description |
|------|-------------|
| `cardano/contracts/validators/mailbox_identity_nft.ak` | One-shot identity NFT policy |

## Files to Modify

| File | Changes |
|------|---------|
| `cardano/contracts/validators/processed_message_nft.ak` | Check identity NFT instead of state NFT |
| `cardano/contracts/validators/example_generic_recipient.ak` | Check identity NFT |
| `cardano/contracts/validators/mailbox.ak` | Add Migrate redeemer |
| `cardano/cli/src/commands/init.rs` | Deploy identity NFT |
| `rust/main/chains/hyperlane-cardano/src/tx_builder.rs` | Include identity NFT in transactions |

## Testing

| Test Case | Expected Result |
|-----------|-----------------|
| Deploy with identity NFT | Mailbox holds identity NFT |
| Process message | Identity NFT found in inputs |
| Migrate mailbox | Identity NFT moves to new contract |
| Process after migration | Works with new mailbox |
| Old mailbox after migration | Cannot process (no identity NFT) |

## Definition of Done

- [ ] `mailbox_identity_nft` contract implemented
- [ ] `processed_message_nft` uses identity NFT
- [ ] Recipients use identity NFT
- [ ] Mailbox has Migrate redeemer
- [ ] CLI deploys identity NFT at initialization
- [ ] Transaction builder includes identity NFT
- [ ] Migration tested end-to-end
- [ ] Documentation updated

## Acceptance Criteria

1. New deployments use identity NFT pattern
2. Mailbox can be upgraded without redeploying recipients
3. Processed message NFTs remain valid across upgrades
4. Existing security guarantees maintained

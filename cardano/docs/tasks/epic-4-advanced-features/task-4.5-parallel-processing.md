[← Epic 4: Advanced Features](./EPIC.md) | [Epics Overview](../README.md)

# Task 4.5: Parallel Inbound Processing

**Status:** ⬜ Not Started
**Complexity:** High
**Depends On:** Task 4.4 (NFT-Based Contract Identity)

## Objective

Enable parallel processing of inbound messages by removing UTXO contention on the mailbox and ISM contracts. Different recipients should be able to receive messages in the same block.

## Problem Statement

Currently, the `process` operation consumes the mailbox UTXO:

```aiken
// In mailbox.ak validate_process()
validate_continuation(datum, datum, tx, own_ref)  // datum unchanged but UTXO consumed
```

This creates a bottleneck:
- Only one transaction can consume a specific UTXO per block
- All `process` operations must be sequential
- **Throughput: ~3 messages/minute** (1 per block × 3 blocks/minute, with ~20s block time)

The ISM is also spent during processing, creating another potential bottleneck.

## Solution: Reference Inputs + Minting Policy Validation

Move all validation logic to the `processed_message_nft` minting policy. The mailbox and ISM become read-only reference inputs.

```
┌─────────────────────────────────────────────────────────────────┐
│              CURRENT: SEQUENTIAL (UTXO CONTENTION)               │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│   TX inputs (spent):                                             │
│     - mailbox_utxo (CONTENTION)                                 │
│     - ism_utxo (CONTENTION)                                     │
│     - recipient_utxo                                            │
│                                                                  │
│   Only 1 process TX per block                                   │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│              PROPOSED: PARALLEL (REFERENCE INPUTS)               │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│   TX reference_inputs (read-only, no contention):               │
│     - mailbox_utxo    → local_domain, default_ism               │
│     - ism_utxo        → validator_set, threshold                │
│     - registry_utxo   → recipient ISM overrides                 │
│                                                                  │
│   TX inputs (spent):                                             │
│     - recipient_utxo  → only this has contention                │
│                                                                  │
│   N process TXs per block (N = unique recipients)               │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

## Implementation

### 1. Rewrite `processed_message_nft.ak`

The minting policy becomes the "process validator":

```aiken
/// Processed Message NFT - Parallel Processing Version
///
/// This policy validates inbound message processing and mints a proof-of-delivery NFT.
/// All validation logic is here - mailbox and ISM are reference inputs only.
///
/// Parameters:
/// - mailbox_identity_policy: Identity NFT policy (stable across upgrades)
/// - ism_identity_policy: ISM identity NFT policy (stable across upgrades)
/// - registry_nft_policy: Registry state NFT policy
validator processed_message_nft(
  mailbox_identity_policy: PolicyId,
  ism_identity_policy: PolicyId,
  registry_nft_policy: PolicyId,
) {
  mint(redeemer: ProcessRedeemer, own_policy: ByteArray, tx: Transaction) {
    let ProcessRedeemer { message, metadata, message_id } = redeemer

    // 1. Find mailbox in REFERENCE inputs (not spent)
    expect Some(mailbox_ref) = find_ref_by_nft(tx.reference_inputs, mailbox_identity_policy)
    expect mailbox_datum: MailboxDatum = parse_inline_datum(mailbox_ref)

    // 2. Verify message destination
    expect message.destination == mailbox_datum.local_domain

    // 3. Verify message_id is correctly computed
    expect keccak_256(encode_message(message)) == message_id

    // 4. Get ISM for recipient (custom or default)
    let ism_policy = get_recipient_ism(
      message.recipient,
      tx.reference_inputs,
      registry_nft_policy,
      mailbox_datum.default_ism,
    )

    // 5. Find ISM in REFERENCE inputs
    expect Some(ism_ref) = find_ref_by_nft(tx.reference_inputs, ism_policy)
    expect ism_datum: MultisigIsmDatum = parse_inline_datum(ism_ref)

    // 6. Verify ISM signatures (logic moved from ISM validator)
    expect verify_multisig(
      message_id,
      metadata,
      ism_datum.validators,
      ism_datum.threshold,
    )

    // 7. Verify recipient script is spent
    expect recipient_called(message.recipient, tx.inputs)

    // 8. Verify not already processed (check reference inputs for existing NFT)
    expect !is_already_processed(message_id, tx.reference_inputs, own_policy)

    // 9. Valid mint: exactly one NFT with message_id as name
    let own_mints = assets.tokens(tx.mint, own_policy)
    dict.to_pairs(own_mints) == [Pair(message_id, 1)]
  }

  else(_) {
    fail
  }
}

/// Redeemer for process operation
type ProcessRedeemer {
  message: Message,
  metadata: ByteArray,  // Contains validator signatures
  message_id: ByteArray,
}

/// Find a reference input containing a specific NFT
fn find_ref_by_nft(refs: List<Input>, nft_policy: PolicyId) -> Option<Output> {
  list.find_map(refs, fn(ref) {
    let tokens = assets.tokens(ref.output.value, nft_policy)
    if !dict.is_empty(tokens) {
      Some(ref.output)
    } else {
      None
    }
  })
}

/// Get ISM for recipient - check registry for custom ISM, fall back to default
fn get_recipient_ism(
  recipient: HyperlaneAddress,
  refs: List<Input>,
  registry_policy: PolicyId,
  default_ism: PolicyId,
) -> PolicyId {
  // Find registry in reference inputs
  when find_ref_by_nft(refs, registry_policy) is {
    Some(registry_output) -> {
      expect registry_datum: RegistryDatum = parse_inline_datum(registry_output)
      // Look up recipient's custom ISM
      when find_recipient_ism(registry_datum, recipient) is {
        Some(custom_ism) -> custom_ism
        None -> default_ism
      }
    }
    None -> default_ism
  }
}

/// Verify multisig threshold is met
fn verify_multisig(
  message_id: ByteArray,
  metadata: ByteArray,
  validators: List<ByteArray>,
  threshold: Int,
) -> Bool {
  // Parse signatures from metadata
  let signatures = parse_signatures(metadata)

  // Count valid signatures
  let valid_count = list.foldl(signatures, 0, fn(sig, count) {
    let signer = recover_signer(message_id, sig)
    if list.has(validators, signer) {
      count + 1
    } else {
      count
    }
  })

  valid_count >= threshold
}
```

### 2. Simplify `mailbox.ak`

Remove `Process` redeemer - mailbox is only for `Dispatch` and admin operations:

```aiken
validator mailbox(processed_messages_nft_policy: PolicyId) {
  spend(datum: Option<MailboxDatum>, redeemer: MailboxRedeemer, own_ref: OutputReference, tx: Transaction) {
    expect Some(mailbox_datum) = datum

    when redeemer is {
      // Dispatch still requires spending mailbox (updates merkle tree)
      Dispatch { destination, recipient, body } ->
        validate_dispatch(mailbox_datum, destination, recipient, body, tx, own_ref)

      // Admin operations
      SetDefaultIsm { new_ism } ->
        validate_set_default_ism(mailbox_datum, new_ism, tx, own_ref)

      TransferOwnership { new_owner } ->
        validate_transfer_ownership(mailbox_datum, new_owner, tx, own_ref)

      Migrate { new_mailbox_address } ->
        validate_migrate(mailbox_datum, new_mailbox_address, tx, own_ref)

      // REMOVED: Process - now handled by minting policy
    }
  }
}
```

### 3. Simplify `multisig_ism.ak`

ISM becomes read-only for verification, only spent for admin:

```aiken
validator multisig_ism {
  spend(datum: Option<MultisigIsmDatum>, redeemer: MultisigIsmRedeemer, own_ref: OutputReference, tx: Transaction) {
    expect Some(ism_datum) = datum

    when redeemer is {
      // Admin operations only - verification moved to minting policy
      SetValidators { new_validators } ->
        validate_set_validators(ism_datum, new_validators, tx, own_ref)

      SetThreshold { new_threshold } ->
        validate_set_threshold(ism_datum, new_threshold, tx, own_ref)

      // REMOVED: Verify - now handled by minting policy
    }
  }
}
```

### 4. Update Transaction Builder

```rust
// In tx_builder.rs

pub async fn build_process_tx(&self, message: &HyperlaneMessage, metadata: &[u8]) -> Result<Transaction> {
    // Add reference inputs (NOT spent)
    let mailbox_utxo = self.find_mailbox_utxo().await?;
    let ism_utxo = self.find_ism_utxo().await?;
    let registry_utxo = self.find_registry_utxo().await?;

    // Add spent inputs (only recipient)
    let recipient_utxo = self.find_recipient_utxo(&message.recipient).await?;

    // Build transaction
    let tx = StagingTransaction::new()
        .reference_input(mailbox_utxo)      // READ ONLY
        .reference_input(ism_utxo)          // READ ONLY
        .reference_input(registry_utxo)     // READ ONLY
        .input(recipient_utxo)              // SPENT
        .mint_asset(                        // Mint processed message NFT
            self.processed_msg_policy,
            message_id.as_bytes(),
            1,
        )
        .add_mint_redeemer(
            self.processed_msg_policy,
            ProcessRedeemer { message, metadata, message_id },
        )
        // ... outputs
        .build()?;

    Ok(tx)
}
```

## Throughput Analysis

| Scenario | Bottleneck | Messages/Block | Messages/Minute |
|----------|------------|----------------|-----------------|
| Current (mailbox spent) | Mailbox UTXO | 1 | ~3 |
| Proposed (reference inputs) | Recipient UTXOs | N (unique recipients) | ~3N |

With 10 different recipients receiving messages: **~30 messages/minute** (10x improvement)

## Files to Modify

| File | Changes |
|------|---------|
| `cardano/contracts/validators/processed_message_nft.ak` | Full rewrite with validation logic |
| `cardano/contracts/validators/mailbox.ak` | Remove Process redeemer |
| `cardano/contracts/validators/multisig_ism.ak` | Remove Verify redeemer |
| `rust/main/chains/hyperlane-cardano/src/tx_builder.rs` | Use reference inputs |
| `rust/main/chains/hyperlane-cardano/src/mailbox.rs` | Update process flow |

## Testing

| Test Case | Expected Result |
|-----------|-----------------|
| Single message process | Works with reference inputs |
| Two messages, different recipients, same block | Both processed |
| Two messages, same recipient, same block | Second waits for next block |
| Custom ISM per recipient | Correct ISM used |
| Invalid signature | Minting rejected |
| Replay attempt | Minting rejected (NFT exists) |

## Security Considerations

1. **Same validation, different location:** All checks move to minting policy
2. **Reference input authenticity:** Identity NFTs ensure correct contracts
3. **Replay protection:** Unchanged - processed_message_nft existence check
4. **Signature verification:** Same algorithm, runs in minting policy

## Definition of Done

- [ ] Minting policy contains full validation logic
- [ ] Mailbox is reference input for process
- [ ] ISM is reference input for process
- [ ] Multiple messages processed in same block (different recipients)
- [ ] Custom ISM lookup works via registry
- [ ] All existing tests pass
- [ ] E2E parallel processing demonstrated

## Acceptance Criteria

1. Messages to different recipients processed in parallel
2. No regression in security guarantees
3. Throughput scales with unique recipient count
4. Existing CLI and relayer work with new design

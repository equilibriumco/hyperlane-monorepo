# MerkleRoot ISM — mailbox↔ISM binding design

**Status:** draft for security sanity-check (step 1 of the MerkleRoot migration)
**Branch:** `feat/cardano-merkleroot-ism`
**Scope:** the on-chain binding between the mailbox and a multisig ISM, and the module-type dispatch. Out of scope: the relayer leaf-indexer, metadata encoders, EVM side. Settle this first — if the binding isn't sound, nothing downstream matters.

---

## 1. Why this is the load-bearing change

The review hardened inbound delivery so a message is processed only if a threshold of validators attested to *that exact message*. MessageId ISM makes that direct: the signed checkpoint's `message_id` **is** the delivered message. MerkleRoot breaks the directness — the signed checkpoint is at index `j`, the delivered message at index `i ≤ j`, so `checkpoint.message_id` (leaf at `j`) is generally **not** the delivered message. The binding must move from "the checkpoint names this message" to "this message is provably inside the root the checkpoint names."

## 2. Current (MessageId) binding — what is asserted, where

```
mailbox.validate_process(message, message_id, ...):
  expect keccak256(encode_message(message)) == message_id      # (a) id ↔ message
  smt.verify_non_membership_and_insert(root, key(message_id))   # (b) replay guard
  ism_config = get_recipient_ism(message.recipient, ...)        # (c) which ISM
  verify_ism_for_message(ism_config, message_id, tx, metadata)

verify_ism_for_message(ism_config, message_id, tx, ...):
  find an input at ism_config.script_hash holding ism_config.state_nft_policy / "ISM State"
  verify_ism_redeemer_message_id(that input's ref, message_id, tx)

verify_ism_redeemer_message_id(ism_ref, expected_message_id, tx):
  expect Verify { checkpoint, .. } = redeemer of ism_ref
  checkpoint.message_id == expected_message_id                  # (d) THE delivery binding

ism.verify_checkpoint(...):   # runs when the ISM UTXO is spent
  threshold of unique valid sigs over EIP191(keccak(domain_hash || root || index || message_id))  # (e)
```

Division of labour: the **mailbox** owns "this message" (d); the **ISM script** owns the crypto (e). The mailbox never re-verifies signatures — it trusts the ISM *ran* (a failing (e) fails the ISM spend, reverting the whole tx).

## 3. Architecture decision: two scripts + on-chain module_type

- **Two separate ISM validator scripts** — `MessageIdMultisigIsm` (unchanged, already reviewed) and a new `MerkleRootMultisigIsm` — each with **its own `Verify` redeemer type**. This mirrors Hyperlane's EVM model (`StaticMessageId…` vs `StaticMerkleRoot…`) and keeps the reviewed MessageId path frozen. (Rejected: one dual-mode script, or one shared redeemer with a MessageId-must-pin-delivered-id invariant — too easy to get subtly wrong.)
- **`module_type` lives in the ISM state datum.** Cardano has no view calls, so "query `moduleType()` on-chain" = "read the ISM's state UTXO datum." This is the discriminant for **both** consumers:
  - **Relayer:** resolves the recipient's ISM (its `IsmConfig { script_hash, state_nft_policy }`, or the mailbox `default_ism`), reads that ISM's state UTXO, gets `module_type`, and builds the matching metadata/redeemer. Fully dynamic — default can be MerkleRoot, a recipient can override to MessageId or another MerkleRoot, and the relayer discovers each at delivery time. No static config.
  - **Mailbox:** reads `module_type` from the same (state-NFT-authenticated) datum and branches to decode the correct redeemer type.
- **Why the datum discriminant is safe:** the ISM state datum is authenticated by the state NFT (the review's fix). A wrong `module_type` can't forge — if the datum claims MessageId but the code is the MerkleRoot script, the real script runs on spend and rejects a mismatched redeemer → the tx fails (liveness, not forgery), and the ISM owner only hurts themselves. Trust boundary unchanged: "the ISM state NFT authenticates the datum."

## 4. Proposed MerkleRoot binding

### 4.1 New redeemer type (MerkleRoot ISM only; MessageId untouched)

```
MerkleRootVerify {
  checkpoint: Checkpoint,                    // signed (root_j, index_j, message_id_j, origin, hook)
  validator_signatures: List<ValidatorSignature>,
  delivered_message_id: ByteArray,           // keccak256(delivered message) — the leaf value
  delivered_index: Int,                      // i — position of the delivered leaf
  merkle_proof: List<ByteArray>,             // 32 sibling hashes, leaf→root
}
```

### 4.2 MerkleRoot ISM verification

```
MerkleRootVerify { checkpoint, sigs, delivered_message_id, delivered_index, merkle_proof } -> {
  expect verify_checkpoint(datum, checkpoint, sigs)                         # (e) threshold signed root_j
  expect merkle.verify_proof(checkpoint.merkle_root,
                             delivered_message_id, delivered_index, merkle_proof)  # (f) leaf ∈ signed root
  expect delivered_index <= checkpoint.merkle_index                        # (g) leaf at/under the frontier
  validate_ism_continuation(...)
}
```

`merkle.verify_proof` already exists in `lib/merkle.ak` and is the standard convention (see §6 — this was validated, and a latent inconsistency was fixed as part of this spike).

### 4.3 Mailbox

`verify_ism_for_message` reads `module_type` from the resolved ISM datum, then:

```
when module_type is {
  MessageId  -> expect Verify { checkpoint, .. };            checkpoint.message_id  == expected_message_id   # (d) as today
  MerkleRoot -> expect MerkleRootVerify { delivered_message_id, checkpoint, .. }
                expect delivered_message_id == expected_message_id                                            # (d')
                expect checkpoint.origin    == message.origin                                                 # (h)
}
```

Everything else in `validate_process` is unchanged: (a) id↔message, (b) SMT replay guard on `message_id`, (c) ISM resolution, and the ISM-state-NFT authenticity check.

## 5. Security argument (MerkleRoot path)

Every redeemer field is attacker-controlled:

| Field | Constraint that stops abuse |
| --- | --- |
| `validator_signatures` | (e) verifies ECDSA over the checkpoint digest → no forgery without validator keys. |
| `checkpoint.merkle_root` | Bound by (e): the digest includes `merkle_root`, so a threshold actually signed *this* root. |
| `delivered_message_id` | (f) proves it's a real leaf under the signed root; (d') pins it to `expected_message_id`; (a) pins that to the message. |
| `merkle_proof` / `delivered_index` | (f) only accepts a branch that hashes the leaf up to the signed root — a merkle proof can't fabricate membership. (g) forbids a leaf beyond the signed frontier. |
| `checkpoint.origin` | (h) forces the validator set to be the delivered message's own origin. |
| replay / ISM authenticity | (b) SMT and the state-NFT check unchanged. |

Cross-origin: even without (h), `delivered_message_id = keccak(message)` encodes the origin, and a Y-origin message can only be a real leaf in Y's tree (signed by Y's validators), so (f) already blocks it. (h) makes it explicit and cheap — keep it.

## 6. Spike result — `verify_proof`/`root` consistency (was O5)

`verify_proof` already existed in `lib/merkle.ak`; the real risk was whether it agrees with the tree's own `root()`. It did **not**. Round-trip tests (added in `merkle_test.ak`) showed:

- 1-leaf tree round-trips; **≥2-leaf trees failed** — a standard inclusion proof did not verify against `root()`.
- Root cause: `compute_root` had a non-standard `if level == 0 { branch }` special case that diverged from the eth2/Hyperlane frontier algorithm for even leaf counts.
- An independent test (`verify_proof_matches_standard_root`, hand-computed standard root) confirms **`verify_proof` is canonical**; `root()` was the wrong one.
- `root()` is **dead on-chain today** (the mailbox only calls `merkle.insert`; no validator computes a root), which is why MessageId was unaffected — but it was a live trap for MerkleRoot.

**Fixed** `compute_root` to the standard algorithm; all round-trip tests pass, full contract suite green (69/69).

### ⚠️ Prerequisite before MerkleRoot ships (Rust parity)

The **relayer/validator** computes the root that gets signed for Cardano-origin messages (Rust, `merkle_tree_hook.rs`). This spike only fixed the **Aiken** `root()`. Before the **Cardano → Sepolia** MerkleRoot leg can work, confirm the Rust root computation produces the **same standard root** (an EVM MerkleRoot ISM verifies Cardano-signed roots with the standard proof, so a non-standard Cardano root would silently fail). Note the **Sepolia → Cardano** leg does *not* depend on this — it verifies against the standard Sepolia root using the now-validated `verify_proof`.

## 7. Costs / impact

- **On-chain:** +~32 keccak per inbound `Process` (the proof fold), on top of the 128-hash SMT proof and ECDSA checks. Higher ExUnits/fee; watch the per-tx budget.
- **New ISM script** + `module_type` datum field → deploy the MerkleRoot ISM; recipients opt in via `IsmConfig`. MessageId ISM and its hash are unchanged.
- The mailbox gains a `when module_type is` branch in the security-critical path — re-review against §5.

## 8. Resolved / open

- **O1 (redeemer):** RESOLVED — two scripts, two redeemer types (Option B).
- **O2 (module_type):** RESOLVED — dynamic, read from the ISM state datum on-chain (relayer + mailbox).
- **O3 (proof length):** fixed 32 with `expect length == 32` in the MerkleRoot path (matches tree depth). *Recommend yes.*
- **O4 (g):** keep `delivered_index <= checkpoint.merkle_index` as a cheap bound.
- **O5 (proof convention):** RESOLVED by the spike — `verify_proof` is canonical, `root()` fixed to match. **Rust-parity check (§6) is the remaining prerequisite.**

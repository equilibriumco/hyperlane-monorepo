# MerkleRoot ISM — mailbox↔ISM binding design

**Status:** draft for security sanity-check (step 1 of the MerkleRoot migration)
**Branch:** `feat/cardano-merkleroot-ism`
**Scope of this doc:** *only* the on-chain binding between the mailbox and the multisig ISM. It does **not** cover the relayer leaf-indexer, metadata plumbing, or the EVM side. If this binding isn't sound, nothing downstream matters, so we settle it first.

---

## 1. Why this is the load-bearing change

The security review just hardened the inbound path so that a message is delivered only if a threshold of validators attested to *that exact message*. For MessageId ISM the attestation is direct: validators sign a checkpoint whose `message_id` **is** the delivered message. MerkleRoot breaks that directness — the signed checkpoint is at some index `j`, and the delivered message is at index `i ≤ j`, so `checkpoint.message_id` (the leaf at `j`) is generally **not** the delivered message. The binding must move from "the checkpoint names this message" to "this message is provably inside the root the checkpoint names."

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

ism.verify_checkpoint(datum, checkpoint, signatures):           # runs when ISM is spent
  validators = datum.validators[checkpoint.origin]
  threshold  = datum.thresholds[checkpoint.origin]  (> 0)
  digest = EIP191(keccak(domain_hash(origin, hook) || root || index || message_id))
  count unique valid sigs >= threshold                          # (e) crypto
```

End-to-end guarantee today: (e) proves a threshold signed a checkpoint whose digest binds `message_id`; (d) proves that `message_id` is the one being delivered; (a) ties it to the actual message; (b) stops replay. The **mailbox** owns the "this message" binding (d); the **ISM script** owns the crypto (e). The mailbox never re-verifies signatures — it trusts that the ISM *ran* (if (e) failed, the ISM spend fails and the whole tx reverts).

## 3. Proposed MerkleRoot binding

Keep the exact same division of labour (mailbox binds "this message", ISM owns crypto), but split the ISM's job into **two** proofs: signatures over the *root*, and merkle inclusion of the *delivered leaf* under that root.

### 3.1 Redeemer change (`types.ak`)

```
Verify {
  checkpoint: Checkpoint,                    // signed (root_j, index_j, message_id_j, origin, hook)
  validator_signatures: List<ValidatorSignature>,
  // NEW:
  delivered_message_id: ByteArray,           // keccak256(delivered message)  — leaf value
  delivered_index: Int,                       // i — position of the delivered leaf
  merkle_proof: List<ByteArray>,              // 32 sibling hashes, leaf→root
}
```

`checkpoint.message_id` becomes vestigial for the delivery decision (it's still signed, so it stays in the struct and in the digest — do **not** remove it, signatures depend on it). The delivered message is `delivered_message_id`, bound by the proof, not by `checkpoint.message_id`.

### 3.2 ISM verification (`multisig_ism.ak`)

```
Verify { checkpoint, validator_signatures, delivered_message_id, delivered_index, merkle_proof } -> {
  expect verify_checkpoint(datum, checkpoint, validator_signatures)         # (e) unchanged: threshold signed root_j
  expect merkle.verify_proof(
           leaf:  delivered_message_id,
           index: delivered_index,
           branch: merkle_proof,
           root:  checkpoint.merkle_root,
         )                                                                   # (f) NEW: delivered leaf ∈ signed root
  expect delivered_index <= checkpoint.merkle_index                         # (g) leaf is at/under the signed frontier
  validate_ism_continuation(...)  # unchanged
}
```

New helper in `lib/merkle.ak` (building blocks — `hash_pair`, depth 32 — already exist; the proof-verify fold does not):

```
pub fn verify_proof(leaf, index, branch, root) -> Bool {
  // fold leaf up 32 levels; at level L use bit L of index to pick sibling side
  // (must match the outbound insert()/root() hashing convention exactly)
  computed_root(leaf, index, branch) == root
}
```

### 3.3 Mailbox assertions (`mailbox.ak`)

`verify_ism_redeemer_message_id` becomes `verify_ism_redeemer_delivers`:

```
expect Verify { delivered_message_id, checkpoint, .. } = redeemer of ism_ref
expect delivered_message_id == expected_message_id       # (d') THE delivery binding (was checkpoint.message_id)
expect checkpoint.origin == message.origin               # (h) NEW: validator set is the message's own origin
```

Everything else in `validate_process` is **unchanged**: (a) id↔message, (b) SMT replay guard on `message_id` (= `delivered_message_id`), (c) ISM resolution, and the ISM-state-NFT authenticity check from the review.

## 4. Security argument

The redeemer is fully attacker-controlled, so we check every field can't be abused:

| Field | Constraint that stops abuse |
| --- | --- |
| `validator_signatures` | (e) verifies ECDSA over the checkpoint digest → can't forge without validator keys. |
| `checkpoint.merkle_root` | Bound by (e): the digest includes `merkle_root`, so a threshold actually signed *this* root. |
| `delivered_message_id` | (f) proves it's a real leaf under the signed root; (d') pins it to `keccak(message)`; (a) pins that to the message. |
| `merkle_proof` / `delivered_index` | (f) only accepts a branch that hashes `delivered_message_id` up to the signed root — a merkle proof can't fabricate membership for a non-member. (g) forbids claiming a leaf beyond the signed frontier. |
| `checkpoint.origin` | (h) forces the validator set to be the delivered message's own origin, so a message can't be "verified" by a different/weaker domain's validators. |
| Replay | (b) unchanged — the SMT still consumes `message_id` exactly once. |
| ISM authenticity | ISM-state-NFT check unchanged — a datum-swapped impostor ISM still can't stand in. |

Cross-origin note: even without (h), `delivered_message_id = keccak(message)` encodes the origin domain, and a message with origin Y can only be a genuine leaf in Y's tree (signed by Y's validators), so (f) already blocks cross-origin verification. (h) makes it explicit and cheap — defense in depth, keep it.

MessageId is the **degenerate case** of this design: `delivered_index == checkpoint.merkle_index`, `delivered_message_id == checkpoint.message_id`, empty proof (leaf verifies against itself). That gives us a clean backward-compat story (see open question O1).

## 5. Costs / impact

- **On-chain:** +~32 keccak per inbound `Process` (the proof fold), on top of the 128-hash SMT proof and the ECDSA checks. Higher ExUnits/fee per inbound message; watch the per-tx execution budget.
- **Contract hashes change** → ISM redeploy and the downstream cascade (recipients/config that reference the ISM policy).
- **The interface we just hardened moves** (assertion (d) → (d') + (h)); this doc *is* the re-review of that move. Any implementation must be adversarially re-reviewed against §4.

## 6. Explicitly out of scope here

Relayer NFT-following Cardano leaf indexer + `IncrementalMerkle` + proof generation; the Cardano-ISM metadata builder emitting `(delivered_index, merkle_proof)`; deploying `StaticMerkleRootMultisigIsm` on Sepolia. Those come *after* this binding is agreed.

## 7. Open questions (decide before coding)

- **O1 — one redeemer or two?** Unify (single `Verify` where an empty proof + `index == frontier` means MessageId) vs. add a separate `VerifyMerkleRoot` variant and keep `Verify` as-is. Unify = less code, one path to review; separate = clearer intent, no risk of regressing the MessageId path. *Recommendation:* unify, because a single verification path is easier to prove correct and MessageId falls out as `proof=[]`.
- **O2 — `module_type()`**: report `MerkleRootMultisig` unconditionally, or keep MessageId selectable per deployment? Affects the relayer's generic metadata builder.
- **O3 — proof length**: fixed 32 (Hyperlane tree depth) with `expect length == 32`, matching the SMT's fixed-length discipline? (Recommend yes.)
- **O4 — enforce (g) `delivered_index <= checkpoint.merkle_index`?** It's redundant given (f) can only succeed for real leaves, but it's a cheap sanity bound. (Recommend keep.)
- **O5 — bit/endianness convention**: `verify_proof` MUST use the identical left/right rule as the outbound `insert()`/`root()` in `lib/merkle.ak`, or roots won't match. Needs a shared helper + a round-trip test (`insert N leaves → prove leaf i → verify_proof == root`).

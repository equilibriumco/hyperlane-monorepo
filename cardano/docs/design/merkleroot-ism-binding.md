# MerkleRoot ISM — mailbox↔ISM binding design

**Status:** implemented and **validated end-to-end on-chain, both directions** (Cardano preview ↔ Sepolia, Protocol v11 / Van Rossem). See §10.
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

---

## 9. Implementation progress

**Done (committed, on branch `feat/cardano-merkleroot-ism`):**
- On-chain: incremental `root()` fixed to match `verify_proof` (round-trip tests); `merkleroot_ism.ak` (shared logic in `lib/multisig.ak`); mailbox dispatches on `module_type` from the ISM datum (MessageId binds `checkpoint.message_id`; MerkleRoot binds `delivered_message_id` + `checkpoint.origin == message.origin`). Aiken suite 71/71.
- Rust audit: the relayer's signed Cardano root already uses hyperlane-core `IncrementalMerkle` (standard) — no change needed.
- Datum plumbing: relayer parses `module_type` (CBOR + JSON) and `module_type()` queries the ISM state UTXO on-chain; CLI datum builders emit the field. Relayer + CLI compile.

**Also done since:**
- **B (relayer inbound) — done.** `parse_merkleroot_metadata` parses the Hyperlane MerkleRoot layout, recomputes the signed root via `branch_root`, and emits the `MerkleRootVerify` redeemer; dispatch is by metadata length. `branch_root` is unit-tested against the same vector as the on-chain `verify_proof`. Shared sig-recovery/encoding helpers added. `module_type()` is dynamic. Blueprint rebuilt (`plutus.json` now has `merkleroot_ism`).

**Done (validated live — see §10):**
- **A. CLI MerkleRoot deploy path** — `deploy extract --ism-module-type merkleroot` fills the ISM slot with `merkleroot_ism`, `init` writes `module_type = MerkleRoot`, `reference-scripts-all` picks the ISM script by module_type, and `set-validators`/`set-threshold` preserve `module_type` (via `ism_module_type(ctx)`).
- **B. Relayer MerkleRoot metadata → Plutus redeemer (Sepolia→Cardano).** `parse_merkleroot_metadata` decodes the Hyperlane layout (merkleTreeHook | messageIndex | signedMessageId | 32×32 proof | signedIndex | signatures), recomputes the signed root via `branch_root`, recovers sigs against the EIP-191 digest, and emits `MerkleRootVerify`. Dispatch is by metadata length (`>= MERKLEROOT_ISM_METADATA_MIN_LEN`).
- **C. Cardano→Sepolia.** The Cardano `MerkleTreeHook` indexer already emits `MerkleTreeInsertion`, so the generic prover builds proofs over a **fresh** Cardano tree (index_from = mailbox deploy block). `StaticMerkleRootMultisigIsm` deployed on Sepolia (`DeployCardanoMerkleRootISM.s.sol`), recipient repointed at it.
- **D. Deploy** — built, deployed to preview, agents reconfigured.

## 10. End-to-end validation (Cardano preview ↔ Sepolia, 2026-07-15)

Both directions verified on-chain under **Protocol v11 (Van Rossem)**.

**Inbound — Sepolia → Cardano (exercises the new Cardano Plutus MerkleRoot ISM):**
- Message `0x278a71aa…` (nonce 871322, body "Alice") dispatched on the official Sepolia mailbox to the greeting recipient (default ISM = MerkleRoot).
- Relayer read `module_type = MerkleRoot` from the ISM state datum, built a real inclusion proof (leaf 869960 under the signed root at index 869962), fetched a 1-of-3 quorum checkpoint from the Sepolia validators' S3, and submitted a `Process` tx with `MerkleRootVerify`.
- On-chain: `verify_checkpoint` + `merkle.verify_proof` passed; the `verified_message_nft` (asset name = the message id) was minted and delivered to the recipient. **The Cardano MerkleRoot ISM verified a real Sepolia proof + validator signature on-chain.**

**Outbound — Cardano → Sepolia (exercises the relayer's MerkleRoot metadata builder + Sepolia's audited ISM):**
- Message `0x758c0c97…` (nonce 0) dispatched on the fresh Cardano mailbox to a Sepolia `TestRecipient` whose ISM is `StaticMerkleRootMultisigIsm` (trusts the Cardano validator `0x0A923108…`, 1-of-1).
- Cardano validator signed checkpoint 0 (root `0x5c1e69e8…`); relayer built the MerkleRoot proof over the fresh Cardano tree and called `process()` on the Sepolia mailbox.
- On-chain: `delivered(0x758c0c97…) == true`. **The Sepolia MerkleRoot ISM verified a real Cardano proof + Cardano validator signature on-chain.**

## 11. Operational constraint — MerkleRoot needs the origin tree from leaf 0

A MerkleRoot proof is a merkle **inclusion proof**, so the relayer must reconstruct the origin's incremental tree by replaying **every** `MerkleTreeInsertion` from leaf 0 (`highest_known_leaf_index()` returns `None` until leaf 0 is present — the prover has no snapshot to import). Consequences:

- **Fresh / dedicated origin mailbox → trivial.** The Cardano mailbox here starts at leaf 0 (index_from = deploy block), so outbound proofs build in seconds.
- **Busy shared origin mailbox → a full backfill.** The official Sepolia mailbox had ~869 963 leaves across ~6.76M blocks (deploy block 4 517 413). Inbound MerkleRoot requires backfilling the whole tree once (per fresh `relayer_db`).
  - The relayer's message *and* merkle-tree cursors backfill from `index.from`; set it to the origin **MerkleTreeHook deploy block**.
  - The RPC's `eth_getLogs` block-range cap dominates wall-clock: **dRPC free tier caps at 10 000 blocks; Tenderly allows ~500 000** — put a large-range RPC first and set `index.chunk` near the cap (e.g. 99 999). The full backfill then completes in minutes rather than hours.
  - Cut the noise: the message cursor also re-indexes the whole origin history; a relayer `whitelist` (e.g. `{"origindomain": [<origin>]}` or a specific `messageid`) stops it from hammering the destination RPC for undeliverable historical messages while the tree finishes.
- **MessageId ISM has none of this** (it fetches one signed checkpoint per message, no tree) — which is why production/prior runs used MessageId against the shared mailbox. Prefer MerkleRoot when the origin mailbox is dedicated, or accept a one-time backfill for a shared one.

## 12. Deployment hygiene — `verified_message_nft` is mailbox-parameterized

`verified_message_nft(mailbox_policy_id)` bakes in the mailbox state-NFT policy, and the mailbox bakes in the resulting `verified_message_nft_policy` — a mutual parameterization. **Whenever the mailbox is redeployed, the relayer/agent config's `verifiedMessageNftScriptCbor` + `verifiedMessageNftPolicyId` must be regenerated** from the new deployment; a stale value makes every inbound delivery fail with a bare Plutus `error` (the policy can't find "its" mailbox in the tx inputs). Take both from `deployments/preview/verified_message_nft_applied.plutus` (the applied CBOR); the policy id is `blake2b_224(0x03 ‖ <applied cbor bytes>)` for PlutusV3. This bit the first inbound e2e run.

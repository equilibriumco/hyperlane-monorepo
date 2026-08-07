# Road to Production

What stands between the working preview integration and a Cardano deployment
that can carry real value on mainnet, and the order in which to clear it.

Architecture, message flows and operational procedure live in
[CARDANO_GUIDE.md](CARDANO_GUIDE.md) and
[DEPLOYMENT_GUIDE.md](DEPLOYMENT_GUIDE.md). This document is only about
readiness: what works, what does not, what should change, and what has to happen
first.

## Table of contents

1. [Readiness scorecard](#1-readiness-scorecard)
2. [What is implemented](#2-what-is-implemented)
3. [Known limitations](#3-known-limitations)
4. [Production blockers](#4-production-blockers)
5. [The contention redesign](#5-the-contention-redesign)
6. [Other optimizations](#6-other-optimizations)
7. [Unimplemented Hyperlane features](#7-unimplemented-hyperlane-features)
8. [Sequence to mainnet](#8-sequence-to-mainnet)
9. [Open decisions](#9-open-decisions)

---

## 1. Readiness scorecard

The integration moves messages and value in both directions today, and has done
so repeatedly on preview. What is left to establish is how it behaves under
load, under attack, and in the hands of someone who did not build it.

| Dimension | State | What is missing |
|-----------|-------|-----------------|
| Message delivery | Proven | Both directions, all three warp flavours, repeatedly on preview |
| Inbound authentication | Hardened | Process ↔ ReceiveTransfer binding is symmetric, pinned by test |
| Indexing and explorer | Working | Cardano lands in the same schema as every other Hyperlane chain |
| Throughput | Ceiling | ~1 message per block; acceptable under 100/hour, not a mainnet answer |
| Upgradeability | Owner-gated migration | Same-shape upgrades preserve state and identity; topology changes still need a redeploy |
| Observability | Thin | Chain crate exposes almost no metrics; reorgs report via logs only |
| Security audit | Not started | No external review of validators, signature path or merkle code |
| Provider trust | Single source | Blockfrost alone feeds an agent that signs checkpoints |
| Key management | Files on disk | No HSM, no rotation procedure, no separation of duties |
| TypeScript SDK | Absent | Cardano is reachable only through the Rust agents and its own CLI |
| CI coverage | None | Unit tests exist; nothing runs the bridge end to end |

---

## 2. What is implemented

### Component status

| Component | Status | Notes |
|-----------|--------|-------|
| Incoming messages (other → Cardano) | Tested | End-to-end working |
| Outgoing messages (Cardano → other) | Tested | Validator + relayer delivering |
| Multisig ISM | Complete | ECDSA secp256k1 verified |
| Validator agent | Tested | Signing checkpoints; local or S3/GCS storage |
| Warp routes (native, collateral, synthetic) | Tested | All three flavours; the shipped demo runs native ↔ synthetic |
| Interchain Gas Paymaster | Implemented | Contract, indexer, and relayer integration |
| Per-recipient ISM | Implemented | Resolved and enforced on-chain; see [section 4](#4-production-blockers) for the `0x02` downgrade path |
| NFT policy addressing | Complete | O(1) lookups, no registry needed |
| Scraper | Implemented | Indexes dispatches, deliveries and gas payments into Postgres |
| Explorer | Implemented | Renders Cardano addresses, transactions and warp routes |

### On-chain (Aiken)

- Mailbox: full dispatch and process, merkle tree, continuation UTXOs
- Multisig ISM: ECDSA secp256k1 verification (CIP-49), per-origin validator sets
- Verified message NFTs: message authentication for generic recipients
- Sparse merkle tree (SMT): on-chain replay protection in the mailbox datum
- Warp routes: all three types (native, collateral, synthetic)
- IGP: gas oracle, pay-for-gas, claim
- Validator announce: on-chain announcements

### Off-chain (Rust)

- Mailbox: `process()`, `delivered()`, full indexer
- Multisig ISM: `MultisigIsm` trait implementation
- Recipient resolver: O(1) NFT-based lookups
- Transaction builder: UTXO selection, reference scripts, fee calculation
- Validator agent: checkpoint signing, local / S3 / GCS storage
- Scraper: Cardano indexer plus the migration that inserts the Cardano domains,
  so messages, deliveries and gas payments land in the same schema every other
  Hyperlane chain uses
- Explorer: Cardano address and transaction rendering, warp route resolution and
  a server-side route balance read (on the `cardano` branch of the
  [hyperlane-explorer fork](https://github.com/equilibriumco/hyperlane-explorer/tree/cardano))

---

## 3. Known limitations

### Upgrades go through owner-signed migration, not a proxy

Changing a validator's code changes its script hash and therefore its address,
and the state UTXO at the old address cannot be spent by the new script. What
bridges that gap is a `Migrate` redeemer, implemented on all five stateful
contracts (mailbox, both ISM flavours, IGP and warp route), and exercised by
`hyperlane-cardano <contract> migrate`.

Migration is a spend at the *old* address, so the old validator is the one
enforcing it. `validate_migrate` requires:

| Requirement | Effect |
|-------------|--------|
| Owner signature | Migration is a governance action, not a permissionless one |
| A continuation output at the new address | State lands somewhere specific, chosen by the owner |
| `value_preserved` on that output | The state NFT and all lovelace move with it; stripping the NFT fails |
| New datum's field 0 == `version + 1` | Exactly one step forward, so a migration cannot repeat, skip or roll back |

Every datum carries `version` at field 0 precisely so this check works: Plutus
Data is self-describing, so the old validator can read field 0 out of a datum
shape that did not exist when it was compiled. Everything past field 0 is
owner-attested, because the old code cannot verify a shape it has never seen. That
is the deliberate trade: migration lets the owner rewrite state, which is why it
is owner-gated and value-preserving.

Because the state NFT moves with the UTXO, the identity is stable across a
migration, and that is what makes it useful. Downstream contracts are
parameterized by the state NFT *policy* rather than by script hash.
`verified_message_nft(mailbox_policy_id)` locates the mailbox by finding that
policy among the transaction's inputs, so a migrated mailbox is still the
mailbox as far as every dependent policy is concerned. Agents resolve contracts
the same way, by policy rather than address.

What still has to be done by hand after a migration: the new script's reference
script must be deployed, and the agents' script hash and reference-script UTXO
must be updated. Only the policy is stable; the address is not.

Fourteen tests in `migrate_test.ak` pin the mechanism, including the case where
the NFT is extracted while lovelace is preserved.

#### What migration cannot do

- **Change the state NFT policy.** It is fixed at init by the seed UTXO, and it
  is the contract's permanent identity.
- **Change the state topology.** The continuation is a single output carrying a
  single preserved value, so one state UTXO migrates to one state UTXO. Splitting
  the mailbox into an outbox and an inbox ([section 5](#5-the-contention-redesign))
  needs two independently NFT-anchored states where one exists today, which no
  sequence of migrations expresses. That change is a redeploy.
- **Remove the trust.** An owner who can migrate can rewrite state. This is
  upgradeability under governance, not trustless upgradeability.

#### The fresh-redeploy path, and what it costs

Initializing a new deployment rather than migrating means a new seed UTXO and
therefore a new state NFT policy. That cascades: every downstream policy
parameterized by it changes too, so warp routes and recipients must be
redeployed alongside, and:

| Component | On migration | On fresh redeploy |
|-----------|--------------|-------------------|
| State NFT policy | Preserved | **Changes** |
| Script hash and address | **Changes** | **Changes** |
| Merkle tree state | Carried in the datum (owner-attested) | **Lost** |
| SMT (replay protection) | Carried in the datum (owner-attested) | **Lost** |
| Pending messages | Deliverable, since resolution follows the policy | **Orphaned** |
| Downstream parameterized policies | Unaffected | Must be redeployed |
| Connected chains | Unaffected | Must be reconfigured |

So a same-shape upgrade migrates under owner signature with state and identity
intact, while a change to the state topology, or anything needing a new
identity, requires a redeploy and pays the costs in the right-hand column.

### UTXO contention limits throughput to one message per block

Processing a message spends the mailbox, ISM and recipient UTXOs in a single
transaction. A UTXO can be spent once, so two deliveries cannot share a block:

```
Block N:   Message 1 spends Mailbox v1 -> creates Mailbox v2
Block N+1: Message 2 spends Mailbox v2 -> creates Mailbox v3
```

That serialises every message through one UTXO regardless of recipient, so a
transfer to Alice blocks a transfer to Bob for no reason either of them would
recognise. Measured throughput with transaction chaining is ~3.3 s per message
(100 messages in 4 m 58 s, zero errors), which is respectable and still a
ceiling of roughly three per minute.

Dispatch has the same shape: each outbound message increments the nonce and
updates the merkle tree in the same UTXO.

Current mitigation is retry with backoff and sequential queue management. The
structural fix is [section 5](#5-the-contention-redesign). The limitation starts
to matter above ~100 messages/hour, or wherever low delivery latency is
required.

---

## 4. Production blockers

The audit, key management, validator topology and the preprod campaign are
organisational as much as technical, which makes them easy to postpone and
costly to have postponed.

### Security audit

Not started. The scope that matters: the Aiken validators, the ECDSA secp256k1
verification path (CIP-49), the merkle and SMT implementations, and the inbound
authentication binding, which has already had one critical forge-and-drain bug
found and fixed in it. That history is the argument for an audit, not against
one.

### Blockfrost is a single point of trust

This is the one to fix first, and it is not primarily an availability concern.
Blockfrost is the sole source of chain state for an agent that *signs
checkpoints*. A wrong answer from it, such as a stale UTXO set or a rolled-back
block reported as settled, is something the validator will sign, and there is no
second source to disagree.

Two directions, not mutually exclusive: a provider abstraction with a real
second implementation so answers can be cross-checked, and a self-hosted node
read path so the validator stops trusting a third party for the data it attests
to. The observed 25 to 40 s lag between a block being available and its
address-transaction index catching up is a symptom of the same dependency.

### Key management

The validator signing key and the relayer's Cardano signer are plain files on
disk. No HSM, no rotation procedure, and no separation between the key that
signs checkpoints and the key that pays for transactions.

### Validator set topology

The preview set was chosen for convenience rather than as a real topology:
2-of-3 for inbound, 1-of-1 for outbound. Production needs independent operators,
and the announced checkpoint storage location needs to be genuinely public
rather than a volume shared with one relayer.

### Observability

Indexing and the explorer are done. What remains is metrics: the chain crate
exposes almost none, so Cardano is close to invisible on the standard agent
dashboards, including the queue-length and reprepare-reason panels an operator
reaches for during an incident. Reorg detection reports through logs only, so
there is nothing to alert on.

### No TypeScript SDK

Cardano is wired into the Rust agents and the Cardano CLI, not into
`@hyperlane-xyz/sdk`. `hyperlane warp deploy` and the rest of the TypeScript
tooling do not target it, so anything a third party would build has to be
written twice. Chain metadata and the warp route also sit on a
[registry fork](https://github.com/equilibriumco/hyperlane-registry/tree/cardano)
rather than the canonical registry, which is why the explorer needs a build-time
override to see Cardano at all. Wallet support via CIP-30 is untouched: there is
no path today for a browser user to sign a Cardano warp transfer.

### No end-to-end CI

The contracts have 362 Aiken tests, the agent crate 128, the CLI 76. Nothing
runs the bridge end to end, so every regression in the delivery path is found by
a person noticing.

Before mainnet the integration needs a sustained campaign on preprod: real
duration, real volume, deliberate reorgs, deliberate provider failures, and a
redeploy exercised as a rehearsed procedure rather than discovered under
pressure. Two operational traps have turned up so far: an `INDEX_FROM` set later
than the mailbox block, leaving the relayer at merkle leaf 0 forever, and a
timeout that reports failure on a transaction that actually confirmed. A campaign
surfaces that class of problem; a unit test does not.

### A generic recipient's ISM override can be downgraded

Per-recipient ISM selection *is* enforced on-chain. `validate_process` resolves
the recipient's ISM and then requires that exact ISM to have taken part in the
transaction: an input at `ism_config.script_hash`, carrying an "ISM State" NFT
under `ism_config.state_nft_policy`, with a redeemer matching the message. A
relayer cannot substitute a different ISM, and a script hash alone does not
satisfy the check, because the NFT is what authenticates the datum-held
validator set.

Where the override is read from differs by recipient kind, and that is where the
gap is:

| Recipient | Override lives in | Can the relayer skip it? |
|-----------|-------------------|--------------------------|
| `0x01` warp route | The route's own state UTXO datum, a **spent input** | No, the route must be spent for the transfer to occur |
| `0x02` generic script | An authenticated config UTXO passed as a **reference input** | **Yes**, omit it and resolution falls back to the mailbox default ISM |

For warp routes the override cannot be bypassed, because the same transaction
has to spend the route to move the tokens. For a generic script recipient the
config UTXO is a reference input, which nothing forces the relayer to supply;
omitting it silently yields the default ISM rather than failing. A recipient that
sets a stricter ISM than the default therefore cannot rely on it being applied.

Closing this means making the config UTXO mandatory for `0x02` recipients that
have ever declared an override. That requires the mailbox to distinguish "no
override" from "override withheld", so it needs a positive marker rather than
the absence of a reference input.

---

## 5. The contention redesign

Approved direction for removing the throughput ceiling in
[section 3](#3-known-limitations). This cannot ride the migration path: splitting
the mailbox turns one NFT-anchored state into two, which a value-preserving
single continuation cannot express. It is a redeploy, and therefore lands as one
bundle rather than a series of patches.

### Phase 1: one redeploy bundle

Three changes, shipped together because each one alone still costs a full
redeploy and a reconfiguration of every connected chain.

#### A. Split the mailbox into outbox and inbox

`outbox.ak` keeps the nonce and the merkle tree. `inbox.ak` keeps the SMT replay
root and `default_ism`, so delivery-side configuration moves with the delivery
side, including `SetDefaultIsm`.

**Why two scripts rather than one script with two state UTXOs:** upgrade blast
radius. An inbound-only patch must not move the outbox address, reset
`INDEX_FROM`, or churn the checkpoint history validators have already signed.
Two addresses make that separation structural instead of a rule someone has to
remember. The combined reference-script fee (~7.4 KB) and audit clarity point
the same way.

Continuation lookup must become state-NFT-anchored rather than address-anchored
in every validator regardless of this split.

**Naming:** outbox and inbox restore the Hyperlane v1 contract names; "mailbox"
stays the umbrella term in agent configuration, so operators read the same word
they read on every other chain.

#### B. Make ISM verification an observer

The ISM's `Verify` path becomes a withdraw-zero observer reading
NFT-authenticated state from a reference input. The spend handler stays, but
only for administrative operations: setting validators and setting thresholds.

Verification then stops consuming the ISM UTXO, so the ISM is no longer a
contention point and concurrent deliveries can observe the same validator set in
the same block.

#### C. Free-standing IGP payment UTXOs

A gas payment becomes its own output carrying `{message_id, dest, gas, payer}`,
alongside a separate configuration UTXO, swept later by a batched `Claim`. Quote
enforcement moves relayer-side, which is what the EVM implementation does.

**Why enforcement moves:** the current validator requires the paid amount to
equal the quote exactly, so overpayment is rejected as firmly as underpayment,
because the IGP's only exit is `Claim` to the beneficiary and an overpayment
would quietly become operator revenue. That is correct, and it makes every payer
recompute against a datum the owner can change underneath them. Relayer-side
enforcement puts the decision where the estimate already lives.

#### What does not move

Replay protection stays protocol-owned. Moving the SMT nullifier into recipient
state (the warp datum, or a per-recipient inbox) is technically sound and would
eliminate the freeze-griefing class by construction. It was rejected on
principle: the mailbox is responsible for not delivering the same message twice,
and that responsibility cannot be delegated to the thing being protected. If
inbound parallelism is ever genuinely needed, the answer is a **sharded inbox**,
meaning K protocol-owned inbox UTXOs routed by `message_id` prefix, and never a
recipient-local guard.

One obligation carries across: `inbox.ak` must reproduce the symmetric Process
binding verbatim, along with its pinning test
`standalone_process_cannot_consume_warp_message`. The binding is correct today;
the risk in this move is regression, not an open hole.

### Phase 2: dispatch queue

Outbound contention is answered separately, by a dispatch queue with a
permissionless fold: senders drop tickets, anyone folds a batch into the outbox
via `DispatchBatch`. The consequence worth naming early is that `message_id` is
only known at fold time rather than at submission. This is a ticket flow rather
than a dispatch call, and every client-side assumption about receiving an id back
has to change.

**This is explicitly not on the mainnet critical path.** Inbound parallelism from
Phase 1 unblocks realistic volume; outbound dispatch is sender-paced and rarely
the first bottleneck.

### Expected effect

| Scenario | Current | After Phase 1 |
|----------|---------|---------------|
| 10 messages to 10 recipients | ~3.3 min | ~20 sec |
| 10 messages to 1 recipient | ~3.3 min | ~3.3 min |

Contention remains on recipient UTXOs, which is unavoidable, since recipient
state has to update.

---

## 6. Other optimizations

Smaller, independent of the redesign, none of them blocking.

| Optimization | Notes |
|--------------|-------|
| Dispatch batching | Batch outgoing dispatches into one transaction. Less critical than inbound, since dispatch is user-initiated |
| Reference script caching | In-memory cache with TTL, invalidated on UTXO consumption. Avoids repeated Blockfrost fetches |
| Parallel Blockfrost queries | `tokio::try_join!` for independent queries that currently run sequentially |

---

## 7. Unimplemented Hyperlane features

Part of the Hyperlane specification, not required for the initial integration.

| Feature | Priority | Description |
|---------|----------|-------------|
| Routing ISM | Low | Different ISMs per origin domain |
| Aggregation ISM | Low | Multiple ISMs must verify (AND logic) |
| Interchain Accounts (ICA) | Medium | Cross-chain account control |
| Interchain Query System (IQS) | Low | Remote state queries |
| Warp route rate limiting | Medium | Transfer limits and circuit breakers for mainnet |

---

## 8. Sequence to mainnet

Ordered by dependency, not by preference. The first three streams are genuinely
parallel: different people, no shared code.

| Work | Timing | Why there |
|------|--------|-----------|
| Phase 1 redesign | Parallel, start now | Gates the audit |
| End-to-end CI | Parallel, start now | Independent of the redesign, and what stops the redesign silently breaking delivery |
| Metrics and alerting | Parallel, start now | Indexing and explorer are done; this is the remaining half of observability |
| Security audit | After Phase 1 lands | Auditing the current layout pays twice, since Phase 1 rewrites the mailbox into two scripts and changes how the ISM is invoked |
| Provider abstraction | Before mainnet, independent of Phase 1 | Can run alongside the audit |
| Preprod campaign | Gates mainnet | An audit finds code defects; a campaign finds operational ones |
| Phase 2 dispatch queue | Off the mainnet critical path | Inbound parallelism carries the volume; outbound can wait for evidence it needs to change |

---

## 9. Open decisions

### 1. Bundle Phase 1, or land it as separate redeploys?

Bundling costs one reconfiguration of every connected chain. Separating gives
each change its own audit surface and its own rollback, at the price of
repeating that reconfiguration.

### 2. Is the dispatch queue additive, or the only path?

Keeping direct dispatch alongside the queue preserves the
`message_id`-at-submission contract for existing senders. Queue-only is simpler
on-chain and forces every client onto the ticket flow.

### 3. Where does gas quote enforcement live?

Relayer-side matches EVM and removes the exact-payment trap. A receipt policy
keeps enforcement on-chain and verifiable by anyone, at the cost of the same
recompute-against-a-moving-datum problem.

---

_Last updated: September 2026._

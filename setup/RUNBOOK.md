# Standing up a Cardano <-> Midnight bridge

What has to exist on both chains before `docker compose up` produces a working
bridge, in dependency order, with the failure modes that cost the most time.

`README.md` covers running the stack. This covers getting the two chains into a
state where running it is worthwhile.

The single rule behind most of the pain below: **every step whose effect is
"nothing happens" fails silently.** An unset threshold, an index-from set too
late, a missing oracle and an unannounced validator all present identically —
agents healthy, messages indexed, nothing delivered.

---

## 0. Decide who owns what

A bridge needs a mailbox on each side, and the two sides are not symmetric in
who can change what.

- **Cardano** contracts are ours: the ISM, IGP and warp route are all
  reconfigurable with the CLI at any time.
- **Midnight**'s `night` contract is monolithic — mailbox, hook, ISM and warp
  route in one — and its admin circuits are owner-gated. If someone else
  deployed it, they hold the owner identity and you must either get that state
  or ask them to make the calls.

Reusing someone else's `night` is usually the right call: a deploy needs faucet
funding, DUST registration, a chunked deploy and roughly half an hour of
waiting. But it constrains you (see §3).

**Owner identity** is `secretNonce` + `instanceSalt` in
`$MIDNIGHT_STATE_DIR/owner-state.json`. Losing that file does not produce an
error — `getOrCreateOwnerState` silently mints a _fresh_ identity, and the
contract becomes permanently unadministrable. Back it up before anything else.

---

## 1. Cardano contracts

```sh
cd cardano/contracts && aiken build
cd ../cli && cargo build --release        # rebuild: the CLI and contracts drift together
cd .. && ./cli/target/release/hyperlane-cardano --network preview deploy extract --output deployments/preview
```

Then `init all --domain 2003`, `init igp`, `init validator-announce`, and the
reference scripts.

**Deploy reference scripts one at a time**, with ~90s between:

```sh
for s in mailbox message_id_multisig_ism igp validator_announce; do
    ./cli/target/release/hyperlane-cardano --network preview deploy reference-script --script $s
    sleep 90
done
```

`deploy reference-scripts-all` chains four transactions against Blockfrost's
25–40s stale UTxO view and its internal 120s wait is not enough on a wallet with
hundreds of UTxOs. It got through one script and died on the next.

### Quirks

- **`init all --validators X --thresholds Y` silently skipped the thresholds**
  until `ec4d2d3ba`. On an older CLI, always follow with
  `ism set-threshold`. `set-validators --threshold` never applies one — the
  SetValidators redeemer cannot change thresholds, so it is a separate
  transaction by construction.
- **"Timeout waiting for transaction" does not mean failure.** Several
  transactions confirmed on-chain while the CLI reported a timeout — and because
  it errored, it never recorded the result in `deployment_info.json`. Always
  check `https://cardano-preview.blockfrost.io/api/v0/txs/<hash>` before
  retrying; retrying a confirmed deploy mints a second state NFT.
- **Synthetic `warp deploy` needs two UTxOs of >= 28 ADA.** A long-lived
  deployer wallet holds hundreds of dust UTxOs pinned by old state NFTs, so its
  ADA sits in one output. The CLI prints the exact `utxo split` remedy.
- Synthetic routes need `warp deploy-minting-ref --warp-policy <state-nft>` as a
  separate step, or the relayer cannot mint inbound.
- `deploy extract` clears every `initialized` flag but leaves stale
  `warp_routes` / `recipients` entries pointing at superseded scripts. Prune by
  hand.
- `reference_scripts.json` is a cumulative log, not current state.
  `deployment_info.json`'s per-contract `referenceScriptUtxo` is authoritative.
- `validator show` derives a different validator-announce address than the one
  deployed, so it reports "No announcements found" even when announcements
  exist. Check the address from `deployment_info.json` on a chain explorer
  instead.

### Record the index-from

`CARDANO_INDEX_FROM` must be **at or before** the block of the earliest core
init transaction. This is the one value not in any file — read it off the init
transaction. Too late, and the relayer never sees merkle leaf 0 and delivers
nothing, forever, with only an easily-missed startup warning.

---

## 2. Midnight contracts

Either reuse an existing `night` (get `addresses.json`, `owner-state.json` and
the seeds) or deploy your own. To deploy: fund both wallets from the faucet,
run `register-dust.ts` per wallet — `deploy-contracts.ts` does **not** do it —
and wait ~25 min for DUST to accrue, because retries burn it.

`MIDNIGHT_INDEX_FROM` follows the same rule as Cardano's, and a reused contract
makes it sharper: its merkle tree already holds leaves, so the value must
predate its **first dispatch**, not the moment you started using it. Old
messages aimed at a superseded remote will be indexed and fail to deliver;
blacklist those rather than skipping the history that contains them.

Contract addresses are raw hex in `addresses.json`; the agent config needs `0x`
prepended.

---

## 3. Wiring, both directions

Four things, and each one's absence is invisible:

| #   | Where    | What                                                                                      |
| --- | -------- | ----------------------------------------------------------------------------------------- |
| 1   | Cardano  | `ism set-validators --domain 1234 …` then `ism set-threshold --domain 1234 --threshold 2` |
| 2   | Cardano  | `warp enroll-router --domain 1234 --router <night>`                                       |
| 3   | Midnight | `enrollRemoteRouter(2003, <cardano warp route>)` — owner-gated                            |
| 4   | both     | IGP oracles per direction, if enforcing gas (§5)                                          |

The Cardano ISM is **origin-scoped**: a validator set registered for one origin
domain does nothing for another. Midnight's ISM is not — one set covers every
origin, which is what makes §4 delicate.

Enrollment is per-domain on both sides, so adding domain 2003 to a contract
already serving another domain is additive.

---

## 4. Validator keys — the asymmetry that matters

Two directions, two different sets, and they are chosen under different
constraints:

| Direction           | Signed by                  | Constraint                                     |
| ------------------- | -------------------------- | ---------------------------------------------- |
| Cardano -> Midnight | Cardano-origin validators  | must be in `night`'s enrolled set              |
| Midnight -> Cardano | Midnight-origin validators | must be in the Cardano ISM's domain-1234 entry |

`night`'s set _is_ replaceable — `setValidatorsAndThreshold` is owner-gated, not
sealed, and takes 64-byte uncompressed pubkeys (`cast wallet public-key`), not
addresses. **But do not replace it on a shared deployment.** That same set
verifies every other origin the contract serves, so repointing it at your keys
breaks the other operator's bridge.

On a shared `night`, run your Cardano-origin validators with **its** validator
keys, and keep your own keys for the Midnight-origin side where the Cardano ISM
is yours to configure. That needs no change to their contract.

Checkpoint digests are origin-domain separated, so one key may sign for two
origins safely.

### Announcements

Validators announce where their checkpoints live, and the relayer reads that.

- **Cardano-origin** validators self-announce on startup. Stagger them: they
  share a wallet and collide on its collateral UTxOs.
- **Midnight-origin** validators nominally self-announce too, but
  `ANNOUNCE_TIMEOUT` in `hyperlane-midnight` is hardcoded to 120s while an
  announce is a ZK proof on top of a wallet sync. The agent gives up — **after
  submitting**, so the announcement often lands anyway and the agent reports a
  failure that was a success. Restarting it makes it find its own announcement.
  A subsequent out-of-band announce then fails with `VA: duplicate
announcement`, which is confirmation, not an error.
- Pre-announcing out of band avoids the confusion entirely:
  `tests/e2e/scripts/announce-cardano-bridge-validators.ts` in the Midnight
  repo. Announce **exactly** the location the validator would announce itself —
  for S3 that is `s3://<bucket>/<region>/<folder>`, region included as a path
  component.
- An announcement pointing at a `localStorage` path on someone else's host is
  useless to your relayer, so validator keys inherited from another operator
  still need announcing at _your_ storage location.

---

## 5. Gas

`onChainFeeQuoting` requires an IGP payment covering `gasFraction` of the
destination's estimated cost, and the relayer requires a payment to exist. With
no oracle the quote is zero, the sender pays nothing, and every message is
indexed and then silently never delivered.

Bring the bridge up with `GAS_ENFORCEMENT='[{"type": "none"}]'`, prove both
directions, then set the oracles and switch over — that separates a gas problem
from a wiring problem. Per direction you need an oracle on the origin IGP for
the destination domain, a non-zero `igp quote`, and a sender that actually pays
(Cardano's `warp transfer` only builds the IGP payment when `--gas-limit` is
given).

`gasPaymentEnforcement` entries carry a matching list, so enforcement can be
relaxed for one direction while the other enforces.

---

## 6. First transfer

Over a synthetic route, the first transfer must **mint before it can burn** —
into Cardano first. The Midnight side of this pairing is a collateral route: it
releases NIGHT it already holds and outbound locks replenish it, so read
`vaultBalance()` rather than trusting a recorded figure.

---

## 7. Container quirks

Hit while containerising the agents; all fixed in this directory, listed so the
symptoms are searchable.

- **`Database failed to open`** — the Midnight toolkit creates its LevelDB as
  `./midnight-level-db` relative to the **working directory**. `/app` is
  root-owned and shared, so agents run from `/data`, which is writable and
  per-agent. The underlying `cause` is swallowed, so the message says nothing
  about paths.
- **`Failed to open config directory`** — the agent also opens `./config`
  relative to its working directory; the entrypoint creates it.
- **`Cannot announce validator without a signer`** — the Midnight chain block
  needs `"signer": {}`. An _empty_ block is what parses as `SignerConf::Node`;
  any `"type"` value is rejected outright. The wallet seed reaches the toolkit
  through the environment, not the config.
- **Scraper `Expected key db to be defined`** — the scraper's `db` is the
  Postgres URL, not the rocksdb path the relayer and validators use. It needs
  `HYP_DB` set to the connection string as well as `DATABASE_URL`.
- **Config template changes need `docker compose build`.** The template is
  copied into the image, so editing it on disk changes nothing until a rebuild;
  compose reports the container as "Running" and you watch stale behaviour.
- **Checkpoints on S3: writes are authenticated, reads are anonymous.** A bucket
  without public read fails in the worst way — validators sign happily while the
  relayer fetches no metadata at all.

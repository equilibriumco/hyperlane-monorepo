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
- **Pass `--token-name` on a synthetic deploy.** The name is carried in the
  route datum, and omitting it is valid — you get a nameless asset, which is
  only visible later when a wallet or explorer shows the minted token with a
  blank name. `--token-name sNIGHT` takes text (`hex:` prefix for raw bytes).
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

### 5.1 What "gas" means on each side — get this wrong and nothing adds up

The two chains do not agree on what a unit of gas is, and the oracle values only
make sense per destination.

| Destination       | Estimate the relayer demands                                                             | Unit          |
| ----------------- | ---------------------------------------------------------------------------------------- | ------------- |
| Cardano (`2003`)  | **dynamic** — a real Blockfrost tx evaluation                                            | lovelace, 1:1 |
| Midnight (`1234`) | **fixed 1,000,000** — `handle` is dry-run for validity only, then a constant is returned | abstract      |

Cardano's `gasLimit` **is lovelace** (`hyperlane-cardano::mailbox`: "gas is
denominated 1:1 in lovelace"). So an oracle pricing Cardano as a destination
must use `gasPrice = 1`. Setting it to anything else multiplies every quote by
that factor and, worse, makes the gas amount you register incomparable with the
~1.4 M lovelace the relayer expects.

Measured 2026-08-05: a Cardano delivery estimates **1,420,435 lovelace** against
an actual cost of 1,414,400 — 0.4% apart.

### 5.2 Cardano IGP — pricing Midnight as a destination

```sh
cd cardano
export BLOCKFROST_API_KEY=... CARDANO_SIGNING_KEY=$PWD/testnet-keys/payment.skey

# owner-gated. gas-price 2 uNIGHT/gas, exchange-rate 0.098 ADA per NIGHT x 1e12
./cli/target/release/hyperlane-cardano --network preview \
  igp set-oracle --domain 1234 --gas-price 2 --exchange-rate 98000000000

./cli/target/release/hyperlane-cardano --network preview \
  igp quote --destination 1234 --gas-limit 600000     # -> 0.1176 ADA
```

`payment = (gas_limit + overhead) * gas_price * exchange_rate / 1e12`, in
lovelace. Midnight's fixed 1,000,000 estimate means `gasFraction 1/2` wants
**500,000** gas registered, so `--gas-limit 600000` clears it with margin.

The IGP validator checks `paid == required_lovelace` **exactly** — not `>=`.
Overpaying is impossible by construction (there is no refund path, only `claim`
to the beneficiary), so a quote that drifts from the oracle fails the script.

### 5.3 Midnight IGP — pricing Cardano as a destination

```sh
cd tests/e2e     # in the hyperlane-midnight clone
set -a; . ~/.midnight-stagenet-test/secrets.env; set +a
export MIDNIGHT_NETWORK=stagenet MIDNIGHT_NETWORK_ID=stagenet \
       MIDNIGHT_STATE_DIR=~/.midnight-stagenet-test \
       MIDNIGHT_PROOF_SERVER_URL=http://127.0.0.1:6300

# gasPrice MUST be 1 — Cardano gasLimit is already lovelace (5.1)
GAS_PRICE=1 EXCHANGE_RATE=102040816326 npx tsx scripts/set-cardano-gas-data.ts
```

`quote = gasLimit * gasPrice * exchangeRate / 1e10`, in uNIGHT.
`102040816326` is `1 / 0.098 * 1e10` — lovelace to uNIGHT at 1 NIGHT = 0.098 ADA
(Kraken, 2026-08-05). Both tokens have 6 decimals, so it is the price ratio
directly. Against the 1,420,435 estimate, `gasFraction 1/2` wants **710,218**
gas registered; 750,000 costs 7.65 NIGHT.

### 5.4 Enforcement values in `.env`

```sh
# refuse anything underpaid, both directions
GAS_ENFORCEMENT='[{"type": "onChainFeeQuoting", "gasFraction": "1/2"}]'
# or, to prove wiring first
GAS_ENFORCEMENT='[{"type": "none"}]'
```

Single-quote it: the JSON contains `{}` and `source .env` otherwise mangles it.

Enforcement is working when the relayer logs, per undelivered message:

```
Message does not meet the gas payment requirement preflight check   # no payment at all
Message does not meet the gas payment requirement after gas estimation  # paid, but short
```

The wording distinguishes the two — the second means the payment was seen and
attributed, which is the useful signal that indexing works.

---

## 6. Sending transfers

Over a synthetic route, the first transfer must **mint before it can burn** —
into Cardano first. The Midnight side of this pairing is a collateral route: it
releases NIGHT it already holds and outbound locks replenish it, so read
`vaultBalance()` rather than trusting a recorded figure.

Two throwaway wallets in `setup/recipients/` stand in for real users: **Alice**
on Midnight and **Bob** on Cardano. Both directions below run between them
rather than deployer-to-deployer, because the deployer hides what a user pays —
it already holds every token, so its transfers never cover the cost of a fresh
output or an empty fee balance.

Each `.recipient` file holds the address in Hyperlane's 32-byte form, ready to
paste. Both wallets must be funded first (§6.3); an unfunded one fails at
dispatch, before anything reaches a chain.

### 6.1 Midnight -> Cardano: Alice to Bob

Recipient is the Cardano payment credential in Hyperlane's 32-byte form: kind
byte (`0x00` key, `0x02` script), three zero bytes, then the 28-byte credential.
Bob's is a key credential, so `0x00`.

```sh
cd tests/e2e     # hyperlane-midnight clone
set -a; . ~/.midnight-stagenet-test/secrets.env; set +a
export MIDNIGHT_NETWORK=stagenet MIDNIGHT_NETWORK_ID=stagenet \
       MIDNIGHT_STATE_DIR=~/.midnight-stagenet-test \
       MIDNIGHT_PROOF_SERVER_URL=http://127.0.0.1:6300

R=<monorepo>/setup/recipients
SEED=$(cat $R/alice-midnight.seed) \
CARDANO_RECIPIENT=$(cat $R/bob-cardano.recipient) \
AMOUNT=500000 \
GAS_LIMIT=1050000 GAS_PRICE=1 EXCHANGE_RATE=102040816326 \
  npx tsx scripts/transfer-to-cardano.ts
```

`SEED` picks the sending wallet and covers the gas payment too; omit it to send
as the deployer.

`GAS_LIMIT=1050000` rather than 750000 because Bob is a real recipient: see
§6.4 on reading the live estimate, and raise it further the first time you mint
to an address that holds no sNIGHT yet.

Bob's sNIGHT afterwards — §6.5.

`GAS_LIMIT=0` skips the payment — use it to watch enforcement refuse, then pay
the dispatched id separately (the same `payForGas` the script calls):

```sh
MESSAGE_ID=0x<id> GAS_LIMIT=750000 npx tsx scripts/pay-one.ts
```

`night` has no post-dispatch hook, so the payment **cannot** ride along with the
dispatch — it references a messageId that only exists once the dispatch lands.
Two proofs, and the `transferRemote` one is the heavy one (>12 GB).

### 6.2 Cardano -> Midnight: Bob to Alice

Recipient is the raw 32-byte Midnight address, hex — Alice's unshielded address
is already exactly that. Bob signs, so he spends the sNIGHT §6.1 minted him.

```sh
cd cardano
# A function, not CLI="..." — zsh does not word-split unquoted variables, so a
# string holding a command plus flags is read as one long filename.
cli() { ./cli/target/release/hyperlane-cardano --network preview "$@"; }

export BLOCKFROST_API_KEY=...
export CARDANO_SIGNING_KEY=../setup/recipients/bob-cardano.skey

cli warp transfer --domain 1234 \
  --recipient $(cat ../setup/recipients/alice-midnight.recipient) \
  --amount 200000 \
  --warp-policy ed08f892a125915b483cd7547a2f9dfbf0531b21ec7389110bedfc2f
```

The route's `destination_gas` prices the IGP payment that rides along, and it is
**owner-gated** — so it is set once by the deployer, not by Bob:

```sh
CARDANO_SIGNING_KEY=$PWD/testnet-keys/payment.skey \
  cli warp set-destination-gas --domain 1234 --gas 600000 \
    --warp-policy ed08f892a125915b483cd7547a2f9dfbf0531b21ec7389110bedfc2f
```

**Any** `destination_gas` on the route — including `0` — forces the atomic IGP
path. To dispatch underpaid on purpose, set a small value (`--gas 1000` pays 196
lovelace) and top up afterwards:

```sh
cli igp pay-for-gas --message-id 0x<id> --destination 1234 --gas-limit 600000
```

Payments **accumulate per message**: the relayer sums `gas_amount` across them,
so a short dispatch plus a top-up delivers once the total clears the fraction.

### 6.3 Funding Alice and Bob

Both are empty on creation, and a sender pays the fees as well as providing the
tokens — so each needs funding before it can dispatch. One-time, per wallet.

**Bob, on Cardano.** ADA for fees and the IGP payment: ~10 ADA, from the
deployer with `utxo send`, or the Cardano testnet faucet
(<https://docs.cardano.org/cardano-testnets/tools/faucet>). The sNIGHT he burns
comes from a §6.1 transfer.

The min-UTxO ADA that arrives locked alongside his sNIGHT does **not** count —
it cannot be spent without moving the tokens, so a wallet showing ~1.2 ADA next
to its tokens still has nothing to pay with.

**Alice, on Midnight.** Two things, and the second is the one that bites:

1. **NIGHT to lock** — the stagenet faucet
   (<https://faucet.stagenet.shielded.tools>) wants her bech32m address from
   `alice-midnight.addr`, not the hex; captcha and a daily limit, so claim
   before you need it.
2. **Registered DUST for fees** — `register-dust.ts` for that wallet, then ~25
   min for DUST to accrue. The faucet pays tNIGHT only; DUST comes from
   registering that NIGHT. A wallet with a full NIGHT balance and no DUST cannot
   pay for anything, which reads as a broken script rather than a funding gap.

`SEED` works the same way in `pay-one.ts` and `balance.ts`, so a top-up or a
balance check can be aimed at whichever wallet sent.

This is the more interesting round trip — deployer -> Bob (M->C), then Bob ->
Alice (C->M) — but the funding and DUST wait make it a deliberate exercise
rather than the default path.

### 6.4 Checking a message

```sh
curl -s -X POST http://localhost:8080/v1/graphql -H 'Content-Type: application/json' \
  -d '{"query":"{ message_view(where: {msg_id: {_eq: \"\\\\x<id-no-0x>\"}}) { is_delivered total_gas_amount num_payments destination_tx_hash } }"}'
```

`total_gas_amount` is what enforcement compares — not `total_payment`. Trust it
over the relayer logs: the relayer re-checks on a widening backoff, so it can
already have delivered while the last log line still says the requirement is
unmet.

When it says the requirement is not met, read the estimate it is comparing
against — the cost is per-message, not fixed:

```sh
docker logs cm-relayer --since 10m 2>&1 | grep "Dynamic cost estimate"
```

Half of that figure is what the payment must cover. It moves: minting to an
address that holds no sNIGHT yet costs ~1.92M lovelace against ~1.42M for one
that does, because a fresh output carries its own min-UTxO and token bundle. A
first transfer to a new recipient therefore needs meaningfully more gas than a
repeat one.

### 6.5 Checking Alice's and Bob's balances

**Alice, on Midnight** — needs the SDK, so it runs from the hyperlane-midnight
clone. Prints her balance and both encodings of her address:

```sh
cd tests/e2e
set -a; . ~/.midnight-stagenet-test/secrets.env; set +a
MIDNIGHT_NETWORK=stagenet MIDNIGHT_NETWORK_ID=stagenet \
SEED=$(cat <monorepo>/setup/recipients/alice-midnight.seed) \
  npx tsx scripts/balance.ts
```

Omit `SEED` for the deployer. The first sync on a public network takes a minute
or so.

**Bob, on Cardano** — the CLI reads it through Blockfrost:

```sh
cd cardano
cli query utxos $(cat ../setup/recipients/bob-cardano.addr)
```

The address is positional, not `--address`. And **unset `CARDANO_SIGNING_KEY`
first** if it points anywhere unreadable — the CLI loads it even for a read-only
query, and fails on the key rather than on anything to do with the address.

Reading the output:

```
2d2ca1489bec5a2f#2 - 1200000 lovelace (1.2 ADA)
  + 500000 (0.5 sNIGHT) policy 82b02f24862d5e48...
a6aa4c9ee00c011f#0 - 10000000000 lovelace (10000 ADA)   <- the spendable ADA

Total: 10001200000 lovelace (10001.2 ADA)
```

Every line reads the same way: the raw on-chain amount, then what it means in
brackets. Scaling only applies to routes in this deployment — decimals belong
to the minting route, not to the asset on-chain — so another deployment's token
prints raw, and its name stays hex if it is not printable text.

The split matters: the 1.2 ADA on the first line is min-UTxO pinned to the token
output and cannot pay a fee (§6.3). The total tells you nothing about what is
actually spendable.

`cardano-cli query utxo` is **not** an option here — it needs a local node
socket, and this stack has none. It is still the right tool for the offline
work (`address key-gen`, `key-hash`, `build`).

A never-used address returns no UTXOs — via raw Blockfrost that surfaces as a
404, `"The requested component has not been found."`, which is the expected
answer before the first transfer rather than a failure.

Or on the explorer:
`https://preview.cardanoscan.io/address/<contents of bob-cardano.addr>`

---

## 6b. Handing the stack to someone else

Most of `.env` is derivable, so only a few values actually have to be passed
between people.

**Regenerate rather than copy.** The Cardano half comes straight from the
committed deployment:

```sh
cd cardano && ./cli/target/release/hyperlane-cardano --network preview config generate-env
```

That fills every address, policy id, script hash and reference UTxO from
`deployments/preview/deployment_info.json`. Note it writes a **placeholder** for
`CARDANO_SIGNER_KEY` — non-empty, so a naive "use it if set" check accepts it and
the agent then dies with `Expected a valid private key in hex`.

The Midnight half comes from whoever owns that deployment, in their
`addresses.json` and `secrets.env`.

**Must be shared out of band** (none are in git):

| Value                                         | Notes                                                                                                                                                                                                     |
| --------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `BLOCKFROST_API_KEY`                          | any preview project id; it is metered, so prefer one per person                                                                                                                                           |
| `CARDANO_SIGNER_KEY`                          | a funded preview wallet. Delivering messages only needs ADA — but the **owner-gated** operations (`ism set-*`, `warp enroll-router`, `igp set-oracle`) only work with the key that deployed the contracts |
| `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` | write access to the checkpoint bucket. A different bucket is fine; it must allow **anonymous read** (§8)                                                                                                  |
| Midnight seeds + validator keys               | from the Midnight deployment owner's `secrets.env`                                                                                                                                                        |

**Not in any file, and not derivable** — record these per deployment:

| Value                 | Current   | Meaning                                                 |
| --------------------- | --------- | ------------------------------------------------------- |
| `CARDANO_INDEX_FROM`  | `4542207` | block of the ISM init tx; the mailbox landed at 4542208 |
| `MIDNIGHT_INDEX_FROM` | `200000`  | before the reused `night`'s deploy (~206,170)           |

Committed validator keys in `keys/` are throwaway and shared deliberately, but
only the `midnight-*` pair is used against a _shared_ `night`: the Cardano-origin
validators must sign with keys that contract already enrols (§4).

---

## 7. Recurring operations

Not one-time setup — these come back.

**After wiping the database** (`docker compose down -v`, or any fresh Postgres
volume): re-run `./hasura/track-tables.sh`. Hasura tracks nothing by default, so
the explorer loads normally and shows an empty message list with no error. The
script is idempotent, so running it when unsure costs nothing.

**After redeploying either mailbox:**

1. `CARDANO_INDEX_FROM` / `MIDNIGHT_INDEX_FROM` to the new deployment's first block
2. `docker compose down -v` — validator databases describe the old merkle tree
3. bump `CHECKPOINT_S3_PREFIX` — checkpoints under the old prefix describe that
   same old tree, and a validator resuming onto them signs the wrong history
4. re-run the §3 wiring: ISM validators + threshold, both router enrollments
5. re-run `./hasura/track-tables.sh` if the database went with it

**Between chained Cardano CLI operations:** wait ~90s. Blockfrost's UTxO view
lags 25–40s, and back-to-back transactions select inputs the previous one
already spent.

**Before a heavy Midnight leg:** restart the proof server if it has been up a
long time. Handle proofs have been measured around 12 GiB, and a long-lived
prover is where the memory has gone.

**Keep the Midnight paying wallet consolidated to one coin.** A contract call
built from two unshielded inputs exceeds stagenet's time-to-dismiss budget and
is rejected at submission with:

```
1010: Invalid Transaction: Custom error: 231
```

That is `FeeCalculationError::OutsideTimeToDismiss`. Substrate flattens the
error struct, so nothing in the message mentions inputs, size or time — it looks
random, and it is not. The ZK proof verify already consumes most of the 15 ms
`min_time_to_dismiss` floor; each extra input's signature check tips it over.

The trap is self-inflicted: a **successful** payment splits its change into two
coins, so the next payment fails at a size that worked minutes earlier. Merge
them with a self-send (a plain transfer carries no circuit proof, so two inputs
fit fine):

```sh
AMOUNT=<balance minus headroom> npx tsx scripts/consolidate.ts
```

Observed 2026-08-05: 21 consecutive failures across 0.7–21 NIGHT, then success
on the first attempt after consolidating. Raising
`time_to_dismiss_per_byte` / `min_time_to_dismiss` is the real fix, but on a
public chain that is a governance change, not a config one.

**When Blockfrost returns `402 Project Over Limit`:** the daily cap is spent, not
the burst limit. Every agent and CLI call fails until it resets — swap in another
project id or wait. (`429` is different: that is the 10 req/s burst limit under
five agents, and they retry through it.)

**When the explorer serves `MODULE_UNPARSABLE` or `Cannot find module for
page`:** its `.next` dev cache is inconsistent — restarting the container while
it is mid-compile is enough to cause it, and static assets start 404ing too.
Stop the container first (it holds the directory open), delete `.next`, start it
again, and give the first request ~15s to compile.

**Do not restart the explorer for registry changes.** It re-fetches the registry
on its own cache expiry, so a restart buys nothing and is the main way the build
cache above gets corrupted.

**Stagenet resets quarterly.** Contracts vanish and everything in §2 and §3 is
redone. `owner-state.json` is what you cannot regenerate.

---

## 8. Container quirks

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

# Cardano preview <-> Midnight stagenet bridge

Self-contained stack for running the bridge: four validators, one relayer, the
proof server, the scraper + Hasura pair that feeds the explorer, and optionally
the explorer itself.

Everything is driven by `.env` and one config template. There is no config
assembly step — `entrypoint.sh` renders `config/agent-config.json.tmpl` with
`envsubst` and hands the result to the agent.

## Prerequisites

- Docker with BuildKit (the Rust build uses cache mounts)
- A `midnight-hyperlane` checkout with `npm install` already run. The Midnight
  submitter is a bash wrapper around `npx tsx`, so the repo is bind mounted into
  the agents rather than built into the image.
- Midnight contracts deployed to stagenet, with state in
  `$MIDNIGHT_STATE_DIR_HOST` (default `~/.midnight-stagenet-test`)
- Cardano contracts deployed to preview
- A Blockfrost project id for preview

## Run

```sh
cp .env.example .env
$EDITOR .env
docker compose up -d --build
docker compose logs -f relayer
```

The first build compiles the agents from source and takes a while; later builds
reuse the cargo cache mounts.

Metrics: validators on `:9080`–`:9083`, relayer on `:9089`, scraper on `:9092`.
Hasura on `:8080`.

## Running the explorer too

The explorer is opt-in, because it lives in its own repository:

```sh
docker compose --profile explorer up -d      # everything above, plus the UI on :3000
```

Set `EXPLORER_REPO` to a `hyperlane-explorer` checkout on a branch carrying both
protocols, with `pnpm install` already run — like the Midnight repo, it is
mounted rather than built, since it ships no Dockerfile.

Two environment details that are easy to get backwards:

- `NEXT_PUBLIC_API_URL` is inlined into the client bundle, so the **browser**
  fetches it. It must be the host-visible `http://localhost:8080/v1/graphql`;
  `http://hasura:8080` resolves only inside the compose network.
- `BLOCKFROST_API_KEY` is read only by the `/api/cardano-warp-route-balance`
  route, so it stays server-side and never reaches the browser.

Chain metadata and warp routes come from `NEXT_PUBLIC_REGISTRY_BRANCH`, which
must contain both chains — otherwise they degrade to `ProtocolType.Unknown` and
lose address formatting, the timeline and the warp sections. Note that a
cardanopreview<->midnight warp route has to exist there for the warp sections to
resolve for bridge transfers.

## Why the pieces are shaped this way

**One template, not one per agent.** The chain blocks are identical for every
agent; only each agent's own knobs differ, and those are `HYP_*` environment
variables in `docker-compose.yml` (`HYP_ORIGINCHAINNAME`, `HYP_DB`,
`HYP_METRICSPORT`, ...). Nothing merges or rewrites JSON.

**`--features midnight` in the Dockerfile.** The Midnight arms in
`hyperlane-base` are feature-gated. Without the flag the binary cannot build a
midnight chain entry and dies at config load.

**Node in the runtime image.** `hyperlane-midnight` submits by spawning the
executable named in `toolkitPath`, which is `npx tsx` over the Midnight repo's
TypeScript. The image carries the interpreter; the repo is mounted.

**Only the proof server is local.** Stagenet's node and indexer are remote.
Proving is client-side in Midnight's architecture and stagenet exposes no public
prover, so the prover runs here. Note that the submitter defaults its proof
server to `127.0.0.1:6300`, which inside a container is the container itself —
`docker-compose.yml` overrides it to `http://proof-server:6300`.

## After redeploying on Cardano

Only `.env` changes — no file in this directory does. But four things around it
need doing:

1. **`CARDANO_INDEX_FROM` must be at or before the mailbox deployment block.**
   Set it later and the relayer never sees the tree's first leaves: it sits at
   merkle index 0 and delivers nothing, with no error.
2. **Wipe the agent volumes.** Validator databases and checkpoints from the old
   mailbox describe a different merkle tree:
   `docker compose down -v`
3. **Register the Midnight-origin validator set for domain 1234.** The Cardano
   ISM is origin-scoped, so a set registered for one origin does not apply to
   another: `ism set-validators --domain 1234 ...`
4. **Enroll the route both ways** — the Cardano route enrolls Midnight's mailbox
   at domain 1234, and Midnight's side needs `enrollRemoteRouter(2003, route)`.

## Validator keys

`keys/` holds four throwaway secp256k1 keypairs — two per origin — **committed on
purpose**, because this bridge is a test deployment and having the same keys
across machines is worth more here than secrecy. They must never hold value or be
reused anywhere else. Regenerate with `cast wallet new` if that ever stops being
true.

Each `validator-<name>.key` has a matching `.addr`. The addresses are what the
_contracts_ need, and they are needed at different times:

| Address used for                                                   | When                                                                                                   |
| ------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------ |
| Cardano ISM entry for domain 1234 (the two `midnight-*` addresses) | after Midnight is deployed, via `ism set-validators --domain 1234`                                     |
| Midnight's own validator set (the two `cardano-*` addresses)       | **at Midnight deploy time** — `VALIDATOR_i_PUBKEY` is baked in, so changing it later means redeploying |

`keys/load-into-env.sh` copies the key values into `../.env` in place.

## Checkpoints on S3

All four validators write checkpoints to S3, one folder per validator:
`s3://$CHECKPOINT_S3_BUCKET/$CHECKPOINT_S3_REGION/$CHECKPOINT_S3_PREFIX/<name>`,
where `<name>` is `cardano-1`, `cardano-2`, `midnight-1` or `midnight-2`. Note
the region is a **path component** of a Hyperlane storage location, not just
client config — `s3://bucket/region/folder` is how the agents parse it.

**Writes are authenticated, reads are anonymous.** Validators write using
`AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`, but the relayer fetches checkpoints
with an anonymous client. A bucket without public read therefore produces the
most confusing failure mode available: every validator looks healthy and signs,
while the relayer never fetches metadata and nothing is ever delivered.

Validators announce their own storage location on first start, derived from this
config, so there is nothing to type. When pre-announcing the Midnight-origin
validators out of band (worth doing — their own announce path proves in ZK and is
slow), announce exactly the same URL the validator would have.

**Bump `CHECKPOINT_S3_PREFIX` after redeploying a mailbox.** Checkpoints under the
old prefix describe a different merkle tree; a validator that resumes onto them
signs against the wrong history. This is the S3 counterpart to wiping the
validator databases.

## Gas enforcement

`GAS_ENFORCEMENT` defaults to `onChainFeeQuoting` at `1/1`: a message is
delivered only if its IGP payment covers the destination's full estimated gas.
The relayer requires the payment to exist, so anything unpaid is indexed and
then silently never delivered.

Three things per direction have to be true:

|                      | Cardano -> Midnight (dest 1234)                                                                   | Midnight -> Cardano (dest 2003)                 |
| -------------------- | ------------------------------------------------------------------------------------------------- | ----------------------------------------------- |
| Oracle on origin IGP | `igp set-oracle --domain 1234 --gas-price ... --token-exchange-rate ... --gas-overhead ...`       | IGP's owner-gated `setRemoteGasData` for 2003   |
| Verify the quote     | `igp quote --domain 1234 --gas-limit N`                                                           | `quoteDispatch(2003, N)` / `isRegistered(2003)` |
| Sender pays          | `warp transfer --gas-limit N` — the atomic IGP payment is only built when `--gas-limit` is passed | paid by the dispatching circuit                 |

Without an oracle the quote is zero, the sender pays nothing, and every message
fails the check. So set the oracles before turning this on — or run
`GAS_ENFORCEMENT='[{"type": "none"}]'` during bring-up, which skips the payment
requirement, and switch over once `igp quote` returns a sane number.

`1/1` demands the full estimate. Upstream defaults to `1/2`, which tolerates a
destination estimate that drifts upward after dispatch; if messages start
stalling as underpaid despite a paid IGP, that drift is the first thing to check.

## Gotchas

- **The first transfer over a synthetic route must mint, not burn** — into
  Cardano before out of it.
- **Proofs are memory-hungry.** A handle proof has been measured around 12 GiB.
  If the proof server has been up a long time, restart it before the return leg.
- **Blockfrost is metered and lags 25–40 s.** Serial CLI operations can race its
  view of UTxOs; agents retry through the burst limit on their own.

# Hyperlane on Cardano

A Hyperlane deployment for Cardano: Plutus contracts for the mailbox, ISM, IGP
and warp routes, a CLI to deploy and drive them, and the Rust agent code that
lets the standard Hyperlane relayer, validator and scraper speak to Cardano.

There is a live deployment on **preview** with a working ADA ↔ wADA warp route to
**Ethereum Sepolia**. The contracts, agents, CLI and demo all live here; the
explorer and the chain registry are two public companion repositories, listed
below and pulled automatically when the demo builds.

## Layout

| Path | What it is |
| --- | --- |
| `contracts/` | Aiken (Plutus V3) validators: mailbox, ISMs, IGP, warp route, NFT policies |
| `cli/` | `hyperlane-cardano` — deploy, configure, transfer, query |
| `deployments/preview/` | The live preview deployment: addresses, applied scripts, reference-script UTXOs |
| `demo/` | Everything needed to run the bridge end to end (see below) |
| `docs/` | [CARDANO_GUIDE.md](docs/CARDANO_GUIDE.md) — architecture and concepts<br>[DEPLOYMENT_GUIDE.md](docs/DEPLOYMENT_GUIDE.md) — deploying from scratch |

The agent-side Rust lives outside this directory, in
`rust/main/chains/hyperlane-cardano/`.

Two companion repositories carry the Cardano support that does not belong in a
monorepo — both public, both on a `cardano` branch, both pulled automatically
when the demo builds:

- [equilibriumco/hyperlane-registry](https://github.com/equilibriumco/hyperlane-registry/tree/cardano)
  — the `cardanopreview` chain entry and the ADA ↔ wADA route
- [equilibriumco/hyperlane-explorer](https://github.com/equilibriumco/hyperlane-explorer/tree/cardano)
  — Cardano address and transaction rendering, warp route resolution

See [Tier 2](#tier-2--run-the-bridge-and-move-tokens--a-sepolia-rpc-1-h) for how
they are wired in and what to change if you fork them.

## Getting started

Pick the depth you need. Each tier builds on the one above it.

### Tier 0 — build and test offline (no accounts, ~5 min)

Proves the contracts and the code build and their unit tests pass. Needs
[Aiken](https://aiken-lang.org/installation-instructions) and a Rust toolchain.

```bash
cd cardano/contracts && aiken check          # Plutus validator tests
cd ../cli && cargo test --locked             # CLI tests
cd ../../rust/main && cargo test -p hyperlane-cardano
```

### Tier 1 — read the live deployment (Blockfrost key, ~10 min)

Confirms the shipped deployment is real and intact, without spending anything.
Get a free key from [blockfrost.io](https://blockfrost.io) — the project must be
on network **Preview**.

```bash
cargo install --path cardano/cli --locked      # puts hyperlane-cardano on PATH

cd cardano/demo/docker && cp .env.example .env
# put your key in BLOCKFROST_API_KEY, then:
cd ../.. && demo/scripts/preflight.sh
```

`--locked` is not optional: without it `cargo install` re-resolves dependencies
and picks up a `pallas-txbuilder` the code does not compile against.

`preflight.sh` checks the key, the demo wallet's balance and every
reference-script UTXO. If it fails, the shipped deployment has been spent out
from under you — see [Tier 3](#tier-3--deploy-your-own-faucet-funds-hours).

Then read on-chain state. Sourcing `.env` supplies `BLOCKFROST_API_KEY`,
`CARDANO_NETWORK` and `CARDANO_SIGNING_KEY`, and the deployment/contract
directories default to `./deployments` and `./contracts` — so from `cardano/`
these commands need no flags:

```bash
set -a; . demo/docker/.env; set +a

hyperlane-cardano ism show     # validator sets and thresholds per origin domain
hyperlane-cardano igp show     # gas oracles
hyperlane-cardano igp quote --destination 11155111 --gas-limit 300000
```

### Tier 2 — run the bridge and move tokens (+ a Sepolia RPC, ~1 h)

Brings up the validator, relayer, scraper and a local Hyperlane Explorer, then
transfers ADA to Sepolia and back. This is the demo.

```bash
cd cardano/demo/docker && docker compose up -d --build
```

Then follow [demo/runbook.md](demo/runbook.md).

**This step reaches outside the repository.** Cardano support spans three repos,
because the explorer and the chain registry are separate projects upstream and
neither carries Cardano yet. `docker compose up --build` pulls from both:

| Repository | Branch | When | What breaks without it |
| --- | --- | --- | --- |
| [equilibriumco/hyperlane-explorer](https://github.com/equilibriumco/hyperlane-explorer) | `cardano` | cloned during `docker compose build explorer` | The explorer image will not build |
| [equilibriumco/hyperlane-registry](https://github.com/equilibriumco/hyperlane-registry) | `cardano` | fetched by your **browser**, per page load | Explorer runs but shows no Cardano chain and no warp route |

All three repositories are public, so nothing needs authenticating. But the build
does need outbound network access to `github.com` and
`raw.githubusercontent.com`; on a restricted network the explorer is what fails
first. Everything else — agents, CLI, contracts, transfers — is self-contained
in this repository and unaffected.

The registry pins are build-time values (`NEXT_PUBLIC_*`, which Next.js inlines),
so pointing the demo at your own fork means rebuilding, not restarting:

```bash
EXPLORER_REF=my-branch \
EXPLORER_REGISTRY_URL=https://github.com/me/hyperlane-registry \
EXPLORER_REGISTRY_BRANCH=my-branch \
  docker compose up -d --build explorer
```

If you fork the registry, note that consumers read the **generated** aggregates
(`deployments/warp_routes/warpRouteConfigs.yaml`, `chains/metadata.yaml`), not
the per-chain sources — so edit a source file, then run `pnpm tsx
scripts/build.ts` and commit the regenerated output, or nothing changes.

### Tier 3 — deploy your own (faucet funds, hours)

[DEPLOYMENT_GUIDE.md](docs/DEPLOYMENT_GUIDE.md) walks through building the
contracts, deploying the core, wiring the Sepolia side and standing up agents
against a deployment you control. Do this if `preflight.sh` fails, or if you want
a deployment nobody else can disturb.

## About the shipped keys

`demo/keys/` contains real signing keys, committed deliberately so the demo runs
without a setup ritual. They are **throwaway preview/Sepolia keys holding
demo-level funds**. Anyone with this repository can spend them and reconfigure
the Sepolia contracts they own. Never reuse them, and never fund them beyond
what a demo needs.

## Caveats worth knowing up front

- **The preview deployment is shared, mutable state.** Reference-script UTXOs can
  be spent and the wallet can be drained. `preflight.sh` tells you which.
- **One Blockfrost key serves four consumers** (validator, relayer, scraper,
  explorer), so the free tier's per-second limit gets hit in bursts. It recovers
  on retry; see [demo/docker/README.md](demo/docker/README.md).
- **TypeScript SDK support is not part of this work.** Cardano is wired into the
  Rust agents and this CLI; `hyperlane warp deploy` and friends do not target it.

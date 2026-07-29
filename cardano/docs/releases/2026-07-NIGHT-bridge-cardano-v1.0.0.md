# NIGHT Bridge v1.0.0 — Cardano Side Deployment

|                | **Details**               |
| -------------- | ------------------------- |
| **Created By** | Guilherme Felipe da Silva |
| **Deployment** | Guilherme Felipe da Silva |

| **Network**         | **Deployment Status**   | **Date**   |
| ------------------- | ----------------------- | ---------- |
| **Cardano preview** | **Deployed & verified** | 2026-07-28 |

Companion document:
[Agents & verification checklist](./2026-07-NIGHT-bridge-agents-v1.0.0.md)

This document covers the Cardano side only, from scratch, and is
counterparty-agnostic: the remote chain (Midnight stagenet in this release,
domain `1234`) supplies its own inputs — its origin-validator addresses
before [Deployment Steps](#deployment-steps) step 3, and its router address
before [Initialization Steps](#initialization-steps) step 3. The full
reference for every command (dual-ISM setups, recipients, Sepolia, gas
calibration) remains [`DEPLOYMENT_GUIDE.md`](../DEPLOYMENT_GUIDE.md);
everything the bridge needs is inlined below.

- [Background](#background)
- [Deployed Contracts](#deployed-contracts)
- [Deployment](#deployment)
  - [Prerequisites](#prerequisites)
  - [Deployment Steps](#deployment-steps)
- [Initialization Steps](#initialization-steps)
- [Troubleshooting](#troubleshooting)

## Background

The Cardano side of the NIGHT bridge lives on **preview** (Hyperlane domain
`2003`): the Hyperlane Cardano core — mailbox, origin-scoped MessageId
multisig ISM, ValidatorAnnounce (plus its mandatory reference script), IGP —
and one **synthetic hNIGHT warp route** that mints hNIGHT (a Cardano native
asset, 6 decimals) on verified Midnight-origin transfers and burns it on the
return leg. The ISM registers the bridge validator set for origin domain
`1234`; because it is origin-scoped, other origins (e.g. Sepolia
`11155111`) can coexist on the same ISM untouched.

> [!NOTE]
> **Naming.** hNIGHT ("Hyperlane NIGHT") is this release's Hyperlane warp
> *synthetic* — minted on verified inbound transfers, burned on the return.
> It is deliberately **not** named cNIGHT: the grant reserves **cNIGHT** for
> the lock-and-release Cardano representation (strict 1:1, no mint/burn
> semantics), and cNIGHT is also the ecosystem's name for canonical NIGHT
> on Cardano. When the vault-model route lands, that representation takes
> the cNIGHT name; hNIGHT is the interim synthetic. The asset name is
> **route state**: `--token-name` at deploy time writes it into the route
> datum, the route only validates mints of that exact name, and the
> relayer reads it from the datum (nothing is hardcoded in agent code).
> Deployed with `684e49474854` ("hNIGHT"), the asset's unit is
> `<minting_policy>684e49474854`. Zero units exist until the first
> verified inbound transfer mints them.

## Deployed Contracts

The 2026-07-28 verified deployment (yours will differ on a fresh deploy —
all values land in `cardano/deployments/preview/deployment_info.json`):

| **Contract**                       | **Address**                                                          |
| ---------------------------------- | -------------------------------------------------------------------- |
| Mailbox (H256 form)                | `0x00000000afc68363b43ccd8534c975c86479845fb6ba92d615c9c91fd4039c69`  |
| MessageId multisig ISM (H256 form) | `0x00000000fc16a986e4a17c42e33228947cbaec6c6372e9c856fb796cd7d832cb`  |
| ValidatorAnnounce (H256 form)      | `0x00000000092a77de62c6fe005e51dbe8b2c23aaec35a21d02be6e0f51e4e74ec`  |
| IGP (H256 form)                    | `0x00000000061a6290caf75341ac9f4cd33aeb8be59b814666515c7c124bea26fb`  |
| hNIGHT route — NFT policy          | `975824ed7acedc08a8b767ce507b21a3167a032a38a3d04121417c69`            |
| hNIGHT route — Hyperlane address   | `0x01000000975824ed7acedc08a8b767ce507b21a3167a032a38a3d04121417c69`  |
| hNIGHT minting policy (asset)      | `294f891b1b403e124c2f5c03cd7f2006368c1dff4b1e686a83c23544`            |
| Route script address               | `addr_test1wqsaw4e3rrhzeh4x6llemxu8n46j584yrlh0zlh20jfd5lqln6vk0`     |

Address forms: core contracts are `0x00000000` + 28-byte script hash;
warp-route recipients are `0x01000000` + the 28-byte **NFT policy** (not the
script hash). The route's Hyperlane address is what the Midnight side
enrolls as its remote router.

> [!IMPORTANT]
> A mailbox redeploy changes the state-NFT policy and therefore **every**
> downstream policy — all warp routes must then be redeployed and both
> sides re-enrolled. Treat the core as immutable once live.

## Deployment

### Prerequisites

1. Required tools:

   ```sh
   # Aiken compiler (contracts)
   curl -sSfL https://install.aiken-lang.org | bash
   aiken --version          # v1.0.0+

   # Rust toolchain (CLI)
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   rustup default stable

   # Install the Hyperlane Cardano CLI (binary `hyperlane-cardano` in ~/.cargo/bin)
   cd hyperlane-monorepo/cardano/cli && cargo install --locked --path .
   ```

2. Credentials and environment:

   The CLI reads all of these directly from the environment (clap `env`
   bindings; a matching flag overrides its variable) — no per-command flags
   needed:

   ```sh
   export BLOCKFROST_API_KEY=<KEY>            # free tier from https://blockfrost.io (preview project)
   export CARDANO_SIGNING_KEY=/path/to/payment.skey   # ed25519 payment key
   export CARDANO_NETWORK=preview             # note: CARDANO_NETWORK, not NETWORK
   export CARDANO_DEPLOYMENTS_DIR=~/workspace/eiger/hyperlane-monorepo/cardano/deployments
   export CARDANO_CONTRACTS_DIR=~/workspace/eiger/hyperlane-monorepo/cardano/contracts
   ```

   > [!NOTE]
   > The Blockfrost free tier enforces a 10 req/s burst limit and a daily
   > quota — enough for this deployment and demo traffic, but expect
   > WARN-level 429s once the agents run.

3. A funded wallet controlled by the signing key: ~100 tADA recommended for
   the core (reference scripts are ~9–37 ADA each), plus ≥2 clean UTxOs of
   ≥28 ADA for the warp deploy (`utxo consolidate` / `utxo split` if the
   wallet is polluted). Fund via the
   [Cardano testnet faucet](https://docs.cardano.org/cardano-testnets/tools/faucet)
   (preview network).

4. Generate the **Cardano-origin validator set** — the validators that
   validate Cardano, i.e. sign checkpoints of the mailbox's merkle tree.
   Three secp256k1 keypairs, threshold 2 (validators 1–2 run as agents and
   announce here; 3 is key-only). Requires
   [Foundry](https://book.getfoundry.sh/getting-started/installation)'s
   `cast`; run three times:

   ```sh
   cast wallet new
   # -> Address:     0x<20-byte CARDANO_VALIDATOR_n_ADDR>
   # -> Private key: 0x<32-byte CARDANO_VALIDATOR_n_PRIVKEY>

   # 64-byte uncompressed pubkey body (X||Y), for remote ISMs that enroll
   # public keys rather than addresses:
   cast wallet public-key --raw-private-key 0x<CARDANO_VALIDATOR_n_PRIVKEY>
   ```

   The **private keys** drive the cardano-origin validator agents and the
   VA announcements ([Initialization Steps](#initialization-steps)). The
   **addresses/public keys** are an *output of this deployment*: hand them
   to the counterparty chain, whose ISM must trust them (2-of-3) for origin
   domain `2003`. Store the triples securely (e.g. alongside the agents'
   env file).

5. Collect the **remote-origin validator addresses** — an *input from the
   counterparty chain's deployment*: the 20-byte addresses of the
   validators that validate the remote chain. The Cardano ISM registers
   them for the remote domain in Deployment step 3 (flags take them
   **without** the `0x` prefix).

6. CLI conventions used throughout: with the environment from
   Prerequisite 2 exported, `hyperlane-cardano` needs no flags and runs
   from any directory. (Without `CARDANO_DEPLOYMENTS_DIR` /
   `CARDANO_CONTRACTS_DIR` the defaults are `./deployments` and
   `./contracts` relative to the current directory — i.e. run from
   `hyperlane-monorepo/cardano/`.)

   > [!IMPORTANT]
   > **Serialize all Cardano transactions.** One wallet signs everything and
   > Blockfrost's UTxO view lags a submitted tx by 25–40 s. The CLI waits
   > for confirmation by default (`--no-wait` disables it) — keep the
   > default for scripted deployments, and still pause between separate CLI
   > invocations, or the next call fails with
   > `CannotCreateEvaluationContext`.

### Deployment Steps

1. Build the contracts:

   ```sh
   cd cardano/contracts && aiken build
   cat plutus.json | jq '.validators[].title'   # mailbox, ISMs, state_nft, warp_route, …
   ```

2. Extract the validators (writes `*.plutus`/`*.hash`/`*.addr` +
   `deployment_info.json`; `--ism-module-type messageid` selects the
   default ISM flavour):

   ```sh
   cd ..   # cardano/
   hyperlane-cardano deploy extract --ism-module-type messageid
   # output defaults to $CARDANO_DEPLOYMENTS_DIR/$CARDANO_NETWORK (override with --output)
   hyperlane-cardano deploy info    # inspect
   ```

   > [!IMPORTANT]
   > `deploy extract` **overwrites** `deployment_info.json` — fresh
   > deployments only. Never re-run it against a live deployment.

3. Initialize the core (mailbox + default ISM), registering the
   **remote-origin validator addresses** (Prerequisite 5) for the remote
   domain in the same command. Addresses are 20-byte hex, no `0x`:

   ```sh
   hyperlane-cardano init all \
     --domain 2003 \
     --origin-domains 1234 \
     --validators "1234:<REMOTE_VALIDATOR_1_ADDR>,<REMOTE_VALIDATOR_2_ADDR>,<REMOTE_VALIDATOR_3_ADDR>" \
     --thresholds "1234:2"
   ```

   - `--domain 2003` — the domain Cardano preview identifies as on remote
     chains (`2002` for preprod).
   - Skipping `--validators`/`--thresholds` leaves the ISM at threshold 0,
     which rejects every checkpoint until configured.

   > [!TIP]
   > Remote validator set not generated yet? Validator keypairs are
   > chain-independent (`cast wallet new` — see Prerequisite 4), so the
   > remote side's set can be generated *now* and handed to that deployment
   > later. Alternatively run the minimal form —
   > `hyperlane-cardano init all --domain 2003` — and add the origin later
   > via [Initialization step 1](#initialization-steps)
   > (`ism set-validators` + `ism set-threshold` create new origin entries
   > on the live ISM). The ISM rejects all inbound messages until an
   > origin's set and threshold are configured.
   - The bridge announces its validators separately
     ([Initialization step 2](#initialization-steps)), so
     `--storage-location`/`--validator-key` are not needed here.

4. Initialize the IGP with the gas oracle for the remote domain — the
   relayer enforces payment (`onChainFeeQuoting`, fraction 1/1), so this is
   load-bearing. Oracle format is `domain:gas_price:exchange_rate:gas_overhead`
   with `quote_lovelace = (gas + overhead) × gas_price × exchange_rate / 1e12`:

   ```sh
   hyperlane-cardano init igp --oracle "1234:1:1000000000000:500000"
   ```

   Calibration for Midnight (domain `1234`): the relayer's delivery
   estimate on Midnight is a fixed `1_000_000` gas (fees there are
   DUST-denominated, so the estimate is a placeholder constant), and the
   enforcement floor requires the paid gas to cover it. With
   `gas_price 1` and `exchange_rate 1e12`, 1 midnight-gas prices as
   1 lovelace, and the `500_000` overhead is the operator margin the
   contract adds on top — a sender paying the required `1_000_000` app gas
   is charged exactly **1.5 tADA** per delivery. The on-chain IGP requires
   the paid delta to equal the quote exactly, and rejects destinations
   with no oracle. (Full model: `cardano/docs/design/igp-gas-model.md`.)

5. Parametrize `validator_announce` (derives its applied script from the
   live mailbox — submits no transaction, but Phase 6 needs its output):

   ```sh
   hyperlane-cardano init validator-announce
   ```

6. Deploy the reference scripts for the mailbox, IGP, ValidatorAnnounce and
   the default ISM:

   ```sh
   hyperlane-cardano deploy reference-scripts-all
   ```

   > [!IMPORTANT]
   > The `validator_announce` reference script is **mandatory, not an
   > optimization** — validators cannot announce without it, and the
   > failure shows up later as an announce retry loop, not at deploy time.

7. Verify:

   ```sh
   hyperlane-cardano mailbox show
   hyperlane-cardano ism show
   jq '.mailbox.referenceScriptUtxo, .ism.referenceScriptUtxo, .validator_announce.referenceScriptUtxo' \
     $CARDANO_DEPLOYMENTS_DIR/$CARDANO_NETWORK/deployment_info.json
   ```

8. Generate the agent environment file from the deployment state:

   ```sh
   hyperlane-cardano config generate-env   # -> e2e-docker/.env.generated (never touches .env)

   # Merge into .env: updates matching CARDANO_* keys in place, appends new
   # ones, and leaves everything else (secrets, other chains, comments) alone.
   # Placeholder values (starting with '<') are skipped, so real secrets survive.
   awk -F= 'NR==FNR { if ($0 ~ /^CARDANO_[A-Z_]*=/ && $2 !~ /^</) new[$1]=$0; next }
        /^CARDANO_[A-Z_]*=/ && ($1 in new) { print new[$1]; delete new[$1]; next }
        { print }
        END { for (k in new) print new[k] }' \
     e2e-docker/.env.generated e2e-docker/.env > e2e-docker/.env.new
   diff e2e-docker/.env e2e-docker/.env.new   # review, then:
   mv e2e-docker/.env.new e2e-docker/.env
   ```

   On a first-ever setup (no `.env` yet), start from `.env.generated`
   directly and fill in the secret placeholders (`BLOCKFROST_API_KEY`,
   `CARDANO_SIGNER_KEY`).

   > [!IMPORTANT]
   > `CARDANO_INDEX_FROM` must be ≤ the first mailbox dispatch block ever
   > (the generator derives it from the deployment's init block — keep it).
   > The merkle tree loader starts at leaf 0 — a later start point makes
   > quorum unreachable.

9. Deploy the synthetic hNIGHT warp route (once; it survives Midnight
   redeploys). NIGHT is 6-dec locally, 18-dec on the wire:

   ```sh
   hyperlane-cardano warp deploy --token-type synthetic \
     --token-name hNIGHT \
     --decimals 6 --remote-decimals 18
   ```

   `--token-name` is the on-chain asset name as text (the CLI stores it as
   UTF-8 hex, `684e49474854`; use `hex:<bytes>` for raw bytes); it becomes
   route state in the datum and every mint/burn is validated against it.
   Omit it for a nameless asset (legacy behavior).

   Record from the output: the **NFT policy** (route identity, used in every
   later `--warp-policy` flag and in the route's Hyperlane address) and the
   **minting policy** (the hNIGHT asset id).

   If step 7 of the deploy (minting-policy reference script) fails on a
   stale input — a known race — recover with:

   ```sh
   hyperlane-cardano warp deploy-minting-ref --warp-policy <NFT_POLICY>
   ```

## Initialization Steps

1. *(only when re-pointing an existing core — a fresh `init all` from
   Deployment step 3 already did this)* Register or update the
   remote-origin validator set for the remote domain:

   ```sh
   hyperlane-cardano ism set-validators --domain 1234 \
     --validators <REMOTE_VALIDATOR_1_ADDR>,<REMOTE_VALIDATOR_2_ADDR>,<REMOTE_VALIDATOR_3_ADDR>
   # wait for indexing, then:
   hyperlane-cardano ism set-threshold --domain 1234 --threshold 2
   hyperlane-cardano ism show --domain 1234    # verify: 3 validators, threshold 2
   ```

2. *(optional)* Pre-announce the two cardano-origin validators on the
   ValidatorAnnounce. The validator agents self-announce on first start
   (the pre-2026-07-28 digest bug is fixed — the VA contract now signs
   over the canonical mailbox H256), so this step only front-loads the
   announcements; skip it and the agents handle it. If pre-announcing,
   the storage locations must match the checkpoint directories the agent
   runner creates (see the
   [agents document](./2026-07-NIGHT-bridge-agents-v1.0.0.md#agent-operation)):

   ```sh
   D=$PWD/e2e-docker/local-data/cardano-stagenet
   hyperlane-cardano validator announce \
     --storage-location "file://$D/validator-0/checkpoints" \
     --validator-key <VALIDATOR_1_PRIVKEY_NO_0x>
   # wait for indexing, then:
   hyperlane-cardano validator announce \
     --storage-location "file://$D/validator-1/checkpoints" \
     --validator-key <VALIDATOR_2_PRIVKEY_NO_0x>
   ```

3. *(after each remote-side (re)deploy)* Enroll the remote router on the
   hNIGHT route for the remote domain (owner-gated, idempotent overwrite).
   The router is the counterparty warp contract's 32-byte Hyperlane
   address, an input from that chain's deployment:

   ```sh
   hyperlane-cardano warp enroll-router --domain 1234 \
     --router 0x<REMOTE_ROUTER_ADDRESS> \
     --warp-policy <NFT_POLICY>
   ```

   The counterparty must symmetrically enroll this route's Hyperlane
   address (`0x01000000` + NFT policy) for domain `2003`.

4. Proceed to the
   [agents document](./2026-07-NIGHT-bridge-agents-v1.0.0.md) to run the
   stack and execute the verification checklist.

## Troubleshooting

| Symptom | Cause / fix |
| --- | --- |
| `CannotCreateEvaluationContext … missing from UTxO set` on any CLI op | Blockfrost UTxO lag (25–40 s). Wait, retry. |
| Synthetic `warp deploy` fails at step 7 | Same lag; run `warp deploy-minting-ref --warp-policy <NFT_POLICY>`. The route itself deployed. |
| `NoCollateralInputs` / collateral collisions | Parallel CLI invocations from one wallet — run strictly sequentially. |
| Warp deploy rejects wallet UTxOs | Needs ≥2 clean UTxOs of ≥28 ADA — `utxo consolidate` then `utxo split`. |
| Validator stuck in an announce retry loop | Missing `validator_announce` reference script — Deployment steps 5–6. |
| Relayer "Unable to reach quorum" from leaf 0 | `CARDANO_INDEX_FROM` set after the first dispatch — lower it and clear the relayer DB. |
| Validator agent announce rejected by the VA script (CekError) | Known agent bug — announce via the CLI (Initialization step 2). |
| ISM rejects every checkpoint | Threshold 0 for the origin — `ism show --domain 1234`, then `set-validators`/`set-threshold`. |

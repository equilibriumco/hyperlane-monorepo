# NIGHT Bridge v1.0.0 — Midnight Side Deployment

|                | **Details**               |
| -------------- | ------------------------- |
| **Created By** | Guilherme Felipe da Silva |
| **Deployment** | Guilherme Felipe da Silva |

| **Network**           | **Deployment Status**    | **Date**   |
| --------------------- | ------------------------ | ---------- |
| Midnight devnet       | Retired (validation run) | 2026-07-27 |
| **Midnight stagenet** | **Deployed & verified**  | 2026-07-28 |

Companion document:
[Agents & verification checklist](./2026-07-NIGHT-bridge-agents-v1.0.0.md)

This document covers the Midnight side only and is counterparty-agnostic:
the remote chain (Cardano preview in this release) supplies its own inputs —
its origin-validator public keys before [Deployment Steps](#deployment-steps),
and its warp-route address before [Initialization Steps](#initialization-steps).

- [Background](#background)
- [Deployed Contracts](#deployed-contracts)
- [Deployment](#deployment)
  - [Prerequisites](#prerequisites)
  - [Bridge Identity](#bridge-identity)
  - [Deployment Steps](#deployment-steps)
- [Initialization Steps](#initialization-steps)
- [Troubleshooting](#troubleshooting)

## Background

The Midnight side of the NIGHT bridge is three Compact contracts on
**stagenet** (Hyperlane domain `1234`): `night` — a monolithic WarpRoute +
mailbox + merkle tree that locks NIGHT on outbound transfers and releases it
on verified inbound burns — plus `igp` and `validator-announce`. The
contract's ISM is a 2-of-3 secp256k1 multisig enrolled at deploy time from
env; the validator set is shared with the Cardano side (see
[Bridge Identity](#bridge-identity)).

Stagenet endpoints: node `wss://rpc.stagenet.shielded.tools`, indexer
`https://indexer.stagenet.shielded.tools/api/v3/graphql`, faucet
`https://faucet.stagenet.shielded.tools`. No tx explorer; use
[polkadot.js apps](https://polkadot.js.org/apps/?rpc=wss%3A%2F%2Frpc.stagenet.shielded.tools#/explorer).
All ZK proving is local (stagenet exposes no public proof server).

## Deployed Contracts

| **Contract**                         | **Address**                                                        |
| ------------------------------------ | ------------------------------------------------------------------ |
| `night` — WarpRoute + mailbox + tree | `32782800d500697d19ad42ae2a8b83936381f23c5c53cd7a8f2c7cc64e5c0759` |
| `igp`                                | `7248eeb16fd5e8d31c92fd1e7484cefb35bd9a3adfc078c61ea4ea7086c3181b` |
| `validator-announce`                 | `301d291646ec563b89bca7adc3430c898f2bcb32864e2b20423a65bd662fad08` |

> [!IMPORTANT]
> Addresses change on every redeploy, and **stagenet resets to genesis
> quarterly**. After either event: redo [Deployment Steps](#deployment-steps)
> and [Initialization Steps](#initialization-steps), re-enroll the new
> `night` address as the remote router on the counterparty chain's warp
> route, and restart the agents with the new config.

## Deployment

### Prerequisites

1. Tooling: Node 22 (`nvm use` picks it from `.nvmrc`), Docker, ≥16 GB RAM,
   and [Foundry](https://book.getfoundry.sh/getting-started/installation)
   (`cast` is used for validator key generation). Checkout + deps:

   ```sh
   git clone <midnight-hyperlane-remote> ~/workspace/eiger/midnight-hyperlane
   cd ~/workspace/eiger/midnight-hyperlane
   nvm use && npm install
   ```

2. Install the pinned Compact toolchain. The 0.33.x RC compiler ships from
   `LFDT-Minokawa/compact`, **not** the channel `compact update` queries:

   ```sh
   cd midnight-hyperlane && source scripts/compact-versions.env
   curl --proto '=https' --tlsv1.2 -LsSf \
     "https://github.com/midnightntwrk/compact/releases/download/compact-v${COMPACT_TOOLCHAIN_VERSION}/compact-installer.sh" | sh
   platform="$(uname -m)-unknown-linux-musl"
   dest="$HOME/.compact/versions/${COMPACT_COMPILER_VERSION}/${platform}"
   mkdir -p "$dest"
   curl --proto '=https' --tlsv1.2 -LsSf \
     "https://github.com/LFDT-Minokawa/compact/releases/download/compactc-v${COMPACT_COMPILER_VERSION}/compactc_v${COMPACT_COMPILER_VERSION}_${platform}.zip" -o /tmp/compactc.zip
   unzip -oq /tmp/compactc.zip -d "$dest" && chmod +x "$dest"/*
   ```

3. Compile and verify the artifact actually emits events (`managed/` is
   gitignored; a stale tree deploys silently and the relayer then indexes
   nothing):

   ```sh
   npm run compile
   grep -c '72, 89, 80' contracts/src/managed/night/contract/index.js   # expect >= 2
   ```

4. Start the local proof server (used by the deploy, wiring, dispatches,
   and the relayer's delivery path):

   ```sh
   docker compose -f docker/compose.yaml up -d proof-server
   ```

### Bridge Identity

1. Two 32-byte wallet seeds (deployer/owner and relayer):

   ```sh
   openssl rand -hex 32   # MIDNIGHT_DEPLOYER_SEED
   openssl rand -hex 32   # MIDNIGHT_RELAYER_SEED
   ```

2. Generate the **Midnight-origin validator set** — the validators that
   validate Midnight, i.e. sign checkpoints of `night`'s merkle tree. Three
   secp256k1 keypairs, threshold 2 (validators 1–2 run as agents, 3 is
   key-only). Requires [Foundry](https://book.getfoundry.sh/getting-started/installation)'s
   `cast`; run three times:

   ```sh
   cast wallet new
   # -> Address:     0x<20-byte MIDNIGHT_VALIDATOR_n_ADDR>
   # -> Private key: 0x<32-byte MIDNIGHT_VALIDATOR_n_PRIVKEY>
   ```

   The **private keys** are used by the midnight-origin validator agents
   and the VA announcements ([Initialization Steps](#initialization-steps)).
   The **addresses** are an *output of this deployment*: hand them to the
   counterparty chain, whose ISM must trust them (2-of-3) for origin
   domain `1234`.

3. Collect the **remote-origin validator public keys** — an *input from
   the counterparty chain's deployment*: the 64-byte uncompressed secp256k1
   public key bodies (X‖Y) of the validators that validate the remote
   chain. The `night` ISM enrolls them at deploy time to verify inbound
   messages. (Given a private key: `cast wallet public-key --raw-private-key 0x<KEY>`.)

4. Persist in `~/.midnight-stagenet-test/stagenet.env` (chmod 600):

   ```sh
   export MIDNIGHT_NETWORK=stagenet
   export MIDNIGHT_DEPLOYER_SEED=<SEED_1>
   export MIDNIGHT_RELAYER_SEED=<SEED_2>
   # Remote-origin validator pubkeys (INPUT — trusted by the night ISM):
   export VALIDATOR_1_PUBKEY=0x<64B_REMOTE_PUBKEY_1>
   export VALIDATOR_2_PUBKEY=0x<64B_REMOTE_PUBKEY_2>
   export VALIDATOR_3_PUBKEY=0x<64B_REMOTE_PUBKEY_3>
   # Midnight-origin validator keypairs (OUTPUT — agents sign with these;
   # hand the addresses to the counterparty ISM). Keep as comments:
   #   v1 <ADDR_1> <PRIVKEY_1>
   #   v2 <ADDR_2> <PRIVKEY_2>
   #   v3 <ADDR_3> <PRIVKEY_3>
   ```

5. Fund both wallets from the faucet (captcha-gated, daily limit; 5000
   tNIGHT each is ample). Derive the addresses to paste:

   ```sh
   cd tests/e2e
   MIDNIGHT_NETWORK=stagenet SEED=<SEED> npx tsx scripts/derive-address.ts
   # -> bech32m: mn_addr_stagenet1...   (paste at https://faucet.stagenet.shielded.tools)
   ```

6. Register **both** wallets for DUST (fees are paid in DUST, which accrues
   over time from held NIGHT; the deploy does not do this):

   ```sh
   MIDNIGHT_NETWORK=stagenet SEED=<SEED> npx tsx scripts/register-dust.ts   # per wallet
   ```

   > [!IMPORTANT]
   > After registering, **wait ~25 minutes** before deploying so enough DUST
   > accrues for the whole chunked deploy. Early retries burn the accrued
   > DUST on partial deploys — waiting once beats retrying often.

### Deployment Steps

1. Deploy the three contracts (chunked `night` — deploy tx + 3 verifier-key
   maintenance txs — then `igp` and `validator-announce`, ~5 min total). The
   ISM validator set is read from `VALIDATOR_{1,2,3}_PUBKEY`:

   ```sh
   source ~/.midnight-stagenet-test/stagenet.env
   npm run devnet:deploy
   # -> ~/.midnight-stagenet-test/addresses.json
   ```

   > [!NOTE]
   > `~/.midnight-stagenet-test/owner-state.json` holds the maintenance
   > authority for the chunk-deployed contract — losing it means never being
   > able to update verifier keys. Never delete it.

2. Record the chain height at deploy time — the agents' `MIDNIGHT_INDEX_FROM`:

   ```sh
   curl -s https://indexer.stagenet.shielded.tools/api/v3/graphql \
     -H 'content-type: application/json' \
     -d '{"query":"query { block { height } }"}'
   ```

3. Render the agent chain config (stagenet endpoints):

   ```sh
   npm run devnet:render
   # -> ~/.midnight-stagenet-test/agent-config.json
   ```

## Initialization Steps

> [!IMPORTANT]
> Requires the counterparty chain's warp route to be deployed first — its
> Hyperlane address is this step's input (for a Cardano warp route:
> `0x01000000` + the route's 28-byte NFT policy).

1. Wire the counterparty: enroll the remote route on `night` for its
   domain, seed the IGP gas oracle for that domain, and pre-announce the
   two midnight-origin validators (each announce is a ~2 min ZK proof; the
   validator agents then skip their own). Uses the **midnight-origin
   validator private keys** from [Bridge Identity](#bridge-identity).

   The oracle defaults price 1 gas as 1 micro-NIGHT (`gasPrice 1`,
   `exchangeRate 1e10` = "1.0") — outbound senders then pay
   `gasLimit` micro-NIGHT per delivery (e.g. `1_800_000` gas → 1.8 NIGHT,
   sized ~1.5× the measured Cardano delivery fee). Override via
   `CARDANO_ORACLE_GAS_PRICE` / `CARDANO_ORACLE_EXCHANGE_RATE`.

   ```sh
   cd tests/e2e
   source ~/.midnight-stagenet-test/stagenet.env
   CARDANO_ROUTE_H256=0x<REMOTE_ROUTE_HYPERLANE_ADDRESS> \
   BRIDGE_VALIDATOR_KEY_1=0x<MIDNIGHT_VALIDATOR_1_PRIVKEY> \
   BRIDGE_VALIDATOR_KEY_2=0x<MIDNIGHT_VALIDATOR_2_PRIVKEY> \
   BRIDGE_DATA_DIR_NAME=cardano-stagenet \
   PROOF_TIMEOUT_MS=14400000 npx tsx scripts/wire-cardano-bridge.ts
   ```

   The announce storage locations are `file://` paths under the agents'
   `DATA_DIR` — they must match the runner's layout (see the
   [agents document](./2026-07-NIGHT-bridge-agents-v1.0.0.md#agent-operation)).

2. Proceed to the
   [agents document](./2026-07-NIGHT-bridge-agents-v1.0.0.md) to run the
   stack and execute the verification checklist.

## Troubleshooting

| Symptom | Cause / fix |
| --- | --- |
| Deploy dies with `InsufficientFunds: could not balance dust` | DUST accrual too low. Wait 15–25 min, re-run (the partial contract is inert). Never retry in a tight loop. |
| Dispatch succeeds but the relayer sees 0 events | Deployed contract predates events — run the artifact check (Prerequisite 3) and redeploy. |
| Proof server 400 on `/prove`; log shows SRS download "error sending request" | Container egress hiccup — `docker restart midnight-proof-server`. |
| `compact update <ver>` says "Couldn't find version" | The RC ships from `LFDT-Minokawa/compact` — manual install per Prerequisite 2. |

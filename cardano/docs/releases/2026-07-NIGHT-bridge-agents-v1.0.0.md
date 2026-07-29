# NIGHT Bridge v1.0.0 — Agents & Verification Checklist

|                | **Details**               |
| -------------- | ------------------------- |
| **Created By** | Guilherme Felipe da Silva |
| **Deployment** | Guilherme Felipe da Silva |

| **Network pair**                        | **Status**                | **Date**   |
| --------------------------------------- | ------------------------- | ---------- |
| **Midnight stagenet ↔ Cardano preview** | **Round trip verified**   | 2026-07-28 |

Companion documents (complete both before this one):

- [Midnight side deployment](./2026-07-NIGHT-bridge-midnight-v1.0.0.md)
- [Cardano side deployment](./2026-07-NIGHT-bridge-cardano-v1.0.0.md)

- [Background](#background)
- [Agent Operation](#agent-operation)
- [Checklist](#checklist)
  - [Midnight to Cardano](#midnight-to-cardano)
  - [Cardano to Midnight](#cardano-to-midnight)
- [Troubleshooting](#troubleshooting)
- [Known Gaps](#known-gaps)

## Background

One relayer relays both directions (`relayChains=cardanopreview,midnight`),
plus four validator processes — two per origin. The cardano-origin pair
signs with keys 1–2 of the Cardano-origin validator set (generated in the
[Cardano document](./2026-07-NIGHT-bridge-cardano-v1.0.0.md); trusted by
the remote ISM), and the midnight-origin pair with keys 1–2 of the
Midnight-origin set (generated in the
[Midnight document](./2026-07-NIGHT-bridge-midnight-v1.0.0.md); trusted by
the Cardano ISM). Both sets are 2-of-3. The two sets MAY be the same
keypairs — checkpoint digests are origin-domain-separated, so one set can
safely sign both origins (the 2026-07-28 deployment did exactly that).
Checkpoints use local-file storage; the storage locations were announced on
each origin's ValidatorAnnounce during initialization. Gas payment
enforcement is `onChainFeeQuoting` with `gasFraction 1/1`: a message is
delivered only once its origin-chain IGP payment covers the relayer's live
delivery estimate — unpaid messages park as
`Retry(GasPaymentRequirementNotMet)`. The margin lives in the oracles
(quotes are sized ~1.5× measured cost; see each deployment document).

Measured timings (2026-07-28, 31 GB host): outbound ~2 min end-to-end after
a ~1.5 min dispatch proof; return ~3 min once the relayer has quorum
metadata (~50 s toolkit prep + ~2 min `handle` proof, up to 16 GiB peak on
a busy prover).

## Agent Operation

Requires: built agents
(`cargo build --release --features midnight --bin relayer --bin validator`
in `rust/main`), the rendered Midnight chain config, the local proof-server
container running, and `cardano/e2e-docker/.env` populated.

One command starts the full stack — 4 validators (metrics `:9080–:9083`)
and the relayer (`:9089`); logs in `local-data/cardano-stagenet/*.log`:

```sh
cd hyperlane-monorepo/cardano/e2e-docker
MIDNIGHT_NETWORK=stagenet \
MIDNIGHT_RENDERED=$HOME/.midnight-stagenet-test/agent-config.json \
DATA_DIR=$PWD/local-data/cardano-stagenet \
BRIDGE_CARDANO_VALIDATOR_KEY_1=0x<CARDANO_VALIDATOR_1_PRIVKEY> \
BRIDGE_CARDANO_VALIDATOR_KEY_2=0x<CARDANO_VALIDATOR_2_PRIVKEY> \
BRIDGE_MIDNIGHT_VALIDATOR_KEY_1=0x<MIDNIGHT_VALIDATOR_1_PRIVKEY> \
BRIDGE_MIDNIGHT_VALIDATOR_KEY_2=0x<MIDNIGHT_VALIDATOR_2_PRIVKEY> \
MIDNIGHT_RELAYER_SEED=<RELAYER_SEED> \
MIDNIGHT_INDEX_FROM=<MIDNIGHT_DEPLOY_HEIGHT> \
MIDNIGHT_SUBMIT_TIMEOUT_SECS=600 \
./run-cardano-midnight.sh
```

The runner merges the rendered Midnight chain block into the Cardano relayer
config template, spawns the validators (the two cardano-origin ones
staggered 90 s apart), then the relayer.

| Variable                       | Purpose                                                                                                                                             |
| ------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------|
| `MIDNIGHT_NETWORK`             | **Load-bearing.** The relayer's `submit-handle` subprocess builds its wallet from this env; without it the submit path silently targets devnet endpoints and hangs (the dry-run path works either way). |
| `MIDNIGHT_RENDERED`            | The Midnight chain block rendered during deployment.                                                                                                |
| `DATA_DIR`                     | Agent state root; the `validator-{0..3}/checkpoints` dirs under it must match the announced storage locations.                                      |
| `BRIDGE_CARDANO_VALIDATOR_KEY_{1,2}` / `BRIDGE_MIDNIGHT_VALIDATOR_KEY_{1,2}` | Per-origin validator signing keys. Both fall back to the shared `BRIDGE_VALIDATOR_KEY_{1,2}` when one set signs both origins. |
| `MIDNIGHT_SUBMIT_TIMEOUT_SECS` | Default 120 is too short — the measured submit path is ~50 s prep + ~2 min proof. 600 is comfortable.                                               |
| `MIDNIGHT_INDEX_FROM`          | Midnight deploy height — avoids scanning the full stagenet history.                                                                                 |
| `RELAYER_BLACKLIST`            | Optional `[{"messageid": "0x…"}]` for old mailbox messages aimed at dead night instances (e.g. after a Midnight redeploy).                          |
| `GAS_ENFORCEMENT`              | Optional JSON override of the gas policy. Default: `[{"type": "onChainFeeQuoting", "gasFraction": "1/1"}]`.                                          |

Health checks:

```sh
D=local-data/cardano-stagenet
grep -c ERROR $D/relayer.log        # expect 0 (Blockfrost 429s are WARN-level burst noise)
ls $D/validator-0/checkpoints       # cardano-origin checkpoints appear within ~1 min
ls $D/validator-2/checkpoints       # announcement.json (midnight-origin, pre-announced)
```

Stop with Ctrl-C (or `pkill -x relayer; pkill -x validator`).

> [!TIP]
> Stop the agents when idle — they consume Blockfrost quota continuously.
> A machine reboot mid-flight costs nothing: all bridge state is on-chain or
> on disk, and pending messages resume on the next start.

## Checklist

### Midnight to Cardano

1. Dispatch a transfer (locks NIGHT in `night`, emits `HYP_DISPATCH`; live
   ZK proof ~1.5 min). The recipient is `0x00000000` + the 28-byte payment
   key hash of the receiving Cardano wallet:

   ```sh
   cd midnight-hyperlane/tests/e2e
   source ~/.midnight-stagenet-test/stagenet.env
   # edit RECIPIENT_H256 / AMOUNT in scripts/dispatch-to-cardano.ts
   PROOF_TIMEOUT_MS=14400000 npx tsx scripts/dispatch-to-cardano.ts
   # -> "dispatched messageId 0x…"
   # -> "payForGas(1800000 gas, 1800000 micro-NIGHT)" then "gas paid" —
   #    the script pays the Midnight IGP after dispatching; the relayer
   #    withholds delivery until this payment is indexed.
   ```

2. Confirm the event is served by the indexer (this is what the relayer
   indexes; zero events here means a pre-events contract artifact):

   ```sh
   curl -s https://indexer.stagenet.shielded.tools/api/v3/graphql \
     -H 'content-type: application/json' \
     -d '{"query":"query ($f: ContractEventFilter!, $l: Int!, $o: Int!) { contractEvents(filter: $f, limit: $l, offset: $o) { id ... on MiscContractEvent { name } } }","variables":{"f":{"contractAddress":"<NIGHT_ADDRESS>","types":["MISC"],"fromBlock":<DEPLOY_HEIGHT>,"toBlock":<TIP>},"l":200,"o":0}}'
   ```

   A `HYP_DISPATCH` event appears with name
   `4859505f44495350415443480000…` (byte-encoded, zero-padded to 32 bytes).

3. Watch the pipeline: both midnight-origin validators write
   `<index>_with_id.json` under `validator-{2,3}/checkpoints` within ~1 min;
   the relayer then logs `ReceiveTransfer with direct delivery` and submits
   the mint transaction on Cardano.

4. Verify the mint — the hNIGHT asset's unit is the **minting policy**
   followed by the asset name the route was deployed with
   (`<minting_policy>684e49474854` for "hNIGHT"); quantities are 6-dec
   units (1.5 NIGHT → `1500000`):

   ```sh
   curl -s -H "project_id: $BLOCKFROST_API_KEY" \
     "https://cardano-preview.blockfrost.io/api/v0/addresses/<RECIPIENT_BECH32>" \
     | jq '.amount[] | select(.unit | startswith("<MINTING_POLICY>"))'
   ```

### Cardano to Midnight

1. Restart the proof server first — the `handle` proof can peak ~16 GiB on
   a fragmented server:

   ```sh
   docker restart midnight-proof-server
   ```

2. Derive the 32-byte Midnight recipient (the receiving wallet's unshielded
   address):

   ```sh
   cd midnight-hyperlane/tests/e2e
   MIDNIGHT_NETWORK=stagenet SEED=<SEED> npx tsx scripts/derive-address.ts
   # -> bytes32: 0x…
   ```

3. Burn hNIGHT toward it (amount in 6-dec units; the sending wallet must
   hold the hNIGHT). Run from `hyperlane-monorepo/cardano/` with the CLI
   conventions from the
   [Cardano document](./2026-07-NIGHT-bridge-cardano-v1.0.0.md#prerequisites):

   ```sh
   hyperlane-cardano warp transfer --domain 1234 \
     --recipient 0x<RECIPIENT_BYTES32> \
     --amount 1000000 \
     --gas-limit 1000000 \
     --warp-policy <NFT_POLICY>
   # -> note the "Nonce: <N>" in the output
   ```

   `--gas-limit 1000000` covers the relayer's fixed Midnight delivery
   estimate; with the domain-1234 oracle the bundled IGP payment is
   exactly 1.5 tADA (1,000,000 gas + 500,000 overhead at 1 lovelace/gas).

4. Watch the pipeline: both cardano-origin validators sign checkpoint `<N>`
   (`validator-{0,1}/checkpoints/<N>_with_id.json`) within ~1 min; the
   relayer builds the 2-of-3 metadata, runs the submit toolkit (~50 s prep +
   ~2 min proof), and logs `Confirm(SubmittedBySelf)`.

5. Verify the release — the recipient's unshielded NIGHT balance increases
   by exactly the burned amount:

   ```sh
   MIDNIGHT_NETWORK=stagenet SEED=<SEED> npx tsx scripts/seed-balance.ts
   ```

6. Reconcile the books: hNIGHT total supply on preview must equal the NIGHT
   still escrowed in `night`.

   > Verified 2026-07-28: 1.5 locked − 1 released = 0.5 escrowed =
   > 0.5 hNIGHT circulating; deployer wallet 5,000 → 4,999.5 exactly.

## Troubleshooting

| Symptom | Cause / fix |
| --- | --- |
| Message stuck `Retry(ErrorSubmitting)` + `SubmitterTimeout { 120 }` | Raise `MIDNIGHT_SUBMIT_TIMEOUT_SECS` ([Agent Operation](#agent-operation)). |
| Submit toolkit runs forever with the proof server idle | `MIDNIGHT_NETWORK` missing from the agent env → the wallet targets devnet defaults. The classic. |
| Relayer stuck `AwaitingValidatorSignatures` despite checkpoint files on disk | VA-location lookups failing (usually a Blockfrost 429 storm). Restart the relayer in calm conditions. |
| Relayer retries a message whose recipient no longer exists | Message targets a dead night instance from before a redeploy — add it to `RELAYER_BLACKLIST`. |
| Validator agent: `validator_announce_reference_script_utxo not configured` | The chain `connection` block needs `validatorAnnounceReferenceScriptUtxo`; the runner injects it from `CARDANO_VA_REF_UTXO`. |
| Validator agent's Cardano self-announce rejected (VA script CekError) | The VA instance predates the 2026-07-28 digest fix (it signs over the policy-id form, agents sign the canonical H256). Re-run `init validator-announce` + `deploy reference-script --script validator_announce` with current contracts, or announce via the CLI as a workaround. |
| Proof server 400 on `/prove`; SRS download "error sending request" | Container egress hiccup — `docker restart midnight-proof-server`. |
| Relayer re-prepares an already-delivered Cardano tx (`BadInputsUTxO` loop) | Known issue — restart the relayer. |
| Blockfrost 429s in agent logs | Burst limit (10 req/s) under 5 concurrent agents — retried automatically; only investigate if a message stalls on it. |
| Message parked `Retry(GasPaymentRequirementNotMet)` | Origin IGP payment missing or below the live estimate. Top up: `payForGas(messageId, …)` on the origin IGP (Midnight: `tests/e2e/shared/igp.ts` helper; Cardano: `hyperlane-cardano igp pay-for-gas`). |

## Known Gaps

- `submit-handle` re-syncs a fresh wallet and re-verifies the night contract
  on every delivery (~50 s) — persistent wallet/contract state would shave
  it; not a blocker.
- ~~Cardano validator agent self-announce rejected by the VA script~~ —
  fixed 2026-07-28: `validator_announce.ak` now digests the canonical
  mailbox H256 (script hash, left-padded) that stock agents sign, pinned
  by matching aiken and CLI tests. Applies to VA instances deployed from
  current contracts; older instances need a VA re-parametrize + reference
  script redeploy.
- Gas oracles are calibrated for testnets, not markets: the Midnight
  delivery estimate is a placeholder constant (fees are DUST-denominated),
  and neither oracle tracks a real ADA/NIGHT exchange rate. Production
  needs market-fed oracle values and periodic recalibration.
- The Cardano representation is a mint/burn **synthetic (hNIGHT)**, while
  the grant specifies a lock-and-release cNIGHT ("avoiding mint-and-burn
  semantics"). The 1:1 backing invariant is identical, but the
  grant-conforming design is a collateral-style route over a one-time
  capped cNIGHT supply held in the route vault — a future release.
- Stagenet is a no-SLA network with quarterly genesis resets — see the reset
  procedure in the
  [Midnight document](./2026-07-NIGHT-bridge-midnight-v1.0.0.md#deployed-contracts).

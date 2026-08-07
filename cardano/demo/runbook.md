# Runbook: running the demo

Moves ADA from Cardano preview to Sepolia as wADA, and back. Assumes the stack
is up; see [cardano/README.md](../README.md) for the tiers above and below this
one, and [DEPLOYMENT_GUIDE.md](../docs/DEPLOYMENT_GUIDE.md) to deploy your own.

Every snippet assumes:

```bash
cargo install --path cardano/cli --locked      # once; puts hyperlane-cardano on PATH

cd cardano                       # everything below is relative to cardano/
set -a; . demo/docker/.env; set +a
export ETH_RPC_URL=$SEPOLIA_RPC_URL
```

`--locked` matters: without it `cargo install` resolves dependencies afresh and
picks up a `pallas-txbuilder` the code does not compile against.

Sourcing `.env` is what lets the commands below carry no flags — the CLI reads
`BLOCKFROST_API_KEY`, `CARDANO_NETWORK` and `CARDANO_SIGNING_KEY` from the
environment, and defaults `--deployments-dir`/`--contracts-dir` to `./deployments`
and `./contracts`, which is why everything runs from `cardano/`.

## 0. Check the deployment is intact

```bash
demo/scripts/preflight.sh
```

The preview deployment is shared testnet state. If a reference-script UTXO has
been spent or the wallet drained, every later command fails in its own confusing
way — this reports it once, up front.

## 1. Start the stack

```bash
cd demo/docker && docker compose up -d --build && cd ../..
docker compose -f demo/docker/docker-compose.yml ps    # all should be healthy
```

Six services: `validator-cardano`, `relayer`, `scraper`, `postgres`, `hasura`,
`explorer`, plus a one-shot `hasura-init` that exposes the scraper tables to the
explorer and then exits.

| Service | Where |
| --- | --- |
| Explorer | <http://localhost:3000> |
| GraphQL (Hasura) | <http://localhost:8080/v1/graphql> |
| Metrics | validator `:9090`, relayer `:9091`, scraper `:9092` |

**Stop them when you are not testing** — idle agents still poll Blockfrost and
burn free-tier credits:

```bash
docker compose -f demo/docker/docker-compose.yml stop
```

> After editing `.env`, `restart` is not enough: a container keeps the
> environment it started with. Use `up -d --force-recreate`.

## 2. Cardano → Sepolia

Locks ADA in the Cardano warp route and mints wADA on Sepolia.

```bash
# Any EVM address, left-padded to 32 bytes. This one is the demo Sepolia signer.
EVM_RECIPIENT=$(cast wallet address --private-key "$SEPOLIA_SIGNER_KEY")
RECIPIENT=0x000000000000000000000000${EVM_RECIPIENT#0x}

hyperlane-cardano warp transfer \
  --domain 11155111 \
  --recipient "$RECIPIENT" \
  --amount 5000000 \
  --warp-policy "$CARDANO_NATIVE_WARP_NFT_POLICY" \
  --gas-limit 300000
```

`--amount` is lovelace, so `5000000` is 5 ADA. Both sides of the route use 6
decimals, so 5 ADA arrives as exactly 5.000000 wADA — no rescaling.

The command prints a **Message ID**. Watch it at
`http://localhost:3000/message/<id>`, then check the balance:

```bash
cast call "$SEPOLIA_SYNTHETIC_WADA" 'balanceOf(address)(uint256)' <recipient> --rpc-url "$SEPOLIA_RPC_URL"
```

Delivery takes a couple of minutes: the validator must sign a checkpoint at the
message's index before the relayer can build metadata.

## 3. Sepolia → Cardano

Burns wADA and releases the locked ADA.

```bash
# 32-byte Cardano recipient: 0x00000000 + the 28-byte payment key hash
CARDANO_RECIPIENT=0x000000001212a023380020f8c7b94b831e457b9ee65f009df9d1d588430dcc89

QUOTE=$(cast call "$SEPOLIA_SYNTHETIC_WADA" 'quoteGasPayment(uint32)(uint256)' 2003 \
  --rpc-url "$SEPOLIA_RPC_URL" | awk '{print $1}')

cast send "$SEPOLIA_SYNTHETIC_WADA" 'transferRemote(uint32,bytes32,uint256)(bytes32)' \
  2003 "$CARDANO_RECIPIENT" 2000000 \
  --value "$QUOTE" --private-key "$SEPOLIA_SIGNER_KEY" --rpc-url "$SEPOLIA_RPC_URL"
```

The value must cover the IGP quote or the relayer will not deliver. Confirm:

```bash
hyperlane-cardano query message --message-id <id>
```

## 4. Check balances

```bash
# Cardano: the demo wallet, and the ADA locked in the route
hyperlane-cardano query utxos "$(cat demo/keys/cardano/payment.addr)"

curl -s "http://localhost:3000/api/cardano-warp-route-balance?chainName=cardanopreview&addressOrDenom=$CARDANO_NATIVE_WARP_ROUTE&standard=CardanoHypNative"

# Sepolia: circulating wADA
cast call "$SEPOLIA_SYNTHETIC_WADA" 'totalSupply()(uint256)' --rpc-url "$SEPOLIA_RPC_URL"
```

The two are related but **not equal**:

```text
route UTXO lovelace  −  route min-UTxO baseline  =  wADA totalSupply
      8000000        −         5000000           =      3000000
```

A Cardano UTXO cannot hold zero ADA, so the route's state output carries a
baseline of its own — 5 ADA for this deployment, set when it was deployed and
never released. Only the excess is bridged collateral. Read the baseline from
the route's first output if you need it for another deployment:

```bash
hyperlane-cardano query utxo "$CARDANO_WARP_ROUTE_REF_UTXO"
```

With that subtraction, locked collateral and wADA supply must match exactly —
the quickest end-to-end sanity check there is.

## 5. Gas

```bash
hyperlane-cardano igp quote --destination 11155111 --gas-limit 300000
```

The Cardano IGP charges in lovelace for gas that will be spent on Sepolia. See
[CARDANO_GUIDE.md](../docs/CARDANO_GUIDE.md#7-interchain-gas-paymaster-igp) for
the formula and for why the scale factor is 1e12 rather than Hyperlane's usual
1e10.

Sepolia's IGP prices the other direction and needs no configuration from us —
it is the official deployment, already configured for domain 2003.

## Troubleshooting

Log noise that looks alarming and is not (Blockfrost 429s especially) is covered
in [docker/README.md](docker/README.md#known-noise-in-the-logs).

**A message is dispatched but never delivered.** Check gas first — an underpaid
message sits in the relayer's queue indefinitely:

```bash
docker compose -f demo/docker/docker-compose.yml logs relayer | grep -i 'gas payment requirement'
```

**The explorer finds nothing.** The scraper tables must be tracked in Hasura.
`hasura-init` does this automatically; if it failed, run it by hand:

```bash
demo/docker/hasura/track-tables.sh
```

**The explorer shows chains but no warp route panel.** It resolves the route
through the registry configured at build time. Confirm the route is there:

```bash
curl -s https://raw.githubusercontent.com/equilibriumco/hyperlane-registry/cardano/deployments/warp_routes/warpRouteConfigs.yaml | grep -A6 'wADA/cardanopreview'
```

If you changed a registry entry, remember it is the *generated* aggregates that
consumers read — see [docker/README.md](docker/README.md#pointing-the-explorer-at-a-different-registry).

**After redeploying anything**, refresh `demo/docker/.env` from
`deployments/preview/deployment_info.json` and recreate the containers. The
mailbox and the verified-message NFT policy are mutually parameterized, so a
stale `CARDANO_VERIFIED_MSG_NFT_*` pair makes every inbound delivery fail with a
bare Plutus error.

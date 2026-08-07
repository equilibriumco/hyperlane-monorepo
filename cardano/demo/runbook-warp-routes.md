# Runbook: adding a warp route

Deploys a new token bridge on top of an existing core deployment. The demo ships
one route — ADA ↔ wADA to Sepolia — and this is how it was built; follow it to
add another, or to rebuild it after a redeploy.

Assumes the [demo runbook](runbook.md)'s setup block: `hyperlane-cardano` on
PATH, `.env` sourced, working from `cardano/`.

## Choosing the pair

A route has one flavour per side:

| Cardano side | What it does | Sepolia counterpart |
| --- | --- | --- |
| `native` | Locks ADA | synthetic (`HypERC20`) |
| `collateral` | Locks an existing Cardano token | synthetic |
| `synthetic` | Mints a wrapper for a remote asset | collateral (`HypERC20Collateral`) or native |

**Decimals must agree.** The Cardano route's `--remote-decimals` and the Sepolia
token's `decimals` describe the same wire amount from opposite ends. If they
disagree, transfers arrive silently scaled by a power of ten. The demo uses 6 on
both sides, matching ADA, so nothing is rescaled.

## Step 0 — Free up UTXOs

A warp deploy needs **at least two clean ADA-only UTXOs of ≥ 28 ADA**. Deploys
leave the wallet littered with reference-script and NFT UTXOs, so check first:

```bash
hyperlane-cardano utxo list
```

If you are short, split a large one:

```bash
hyperlane-cardano utxo split --utxo <tx_hash>#<index> --count 6 --amount 60000000
```

To reclaim ADA locked in stale token UTXOs, consolidate then re-split:

```bash
hyperlane-cardano utxo consolidate --max 50
```

Blockfrost paginates at 100 UTXOs, so a cluttered wallet also slows every query.

## Step 1 — Deploy the Cardano side

```bash
hyperlane-cardano warp deploy --token-type native --remote-decimals 6
```

For a collateral route add `--token-policy` and `--token-name`; for a synthetic
one add `--decimals` and the `--token-name` minted tokens will carry.

The command prints an **NFT Policy** — that is the route's identity. Its
Hyperlane address is `0x01000000` + that policy, and it goes in `.env` as
`CARDANO_NATIVE_WARP_ROUTE` / `CARDANO_NATIVE_WARP_NFT_POLICY`.

Note the route state UTXO's lovelace too. A Cardano UTXO cannot hold zero ADA,
so this baseline sits under the route forever and must be subtracted when
reconciling locked collateral against remote supply.

## Step 2 — Deploy the Sepolia side

[`solidity/DeployWAdaRoute.s.sol`](solidity/DeployWAdaRoute.s.sol) deploys the
synthetic, points it at the Cardano ISM, wires the aggregation hook and enrolls
the Cardano router — in one transaction batch. Copy it and change the name,
symbol, decimals and scale for a different token.

```bash
cd demo/solidity
forge script DeployWAdaRoute.s.sol:DeployWAdaRoute \
  --rpc-url "$SEPOLIA_RPC_URL" --broadcast
```

It reads `SEPOLIA_SIGNER_KEY`, `SEPOLIA_MAILBOX`, `SEPOLIA_ISM`,
`SEPOLIA_AGGREGATION_HOOK` and `CARDANO_NATIVE_WARP_ROUTE` from the environment,
and prints `SEPOLIA_SYNTHETIC_WADA=0x…` for `.env`.

> The aggregation hook is not optional. Without it the route posts to the merkle
> tree but never pays the IGP, so the relayer sees an unpaid message and the
> transfer stalls with no error on either chain.

## Step 3 — Enroll the reverse direction

Step 2 taught Sepolia about Cardano. Cardano must be taught about Sepolia:

```bash
hyperlane-cardano warp enroll-router \
  --domain 11155111 \
  --router 0x000000000000000000000000<sepolia-route-without-0x> \
  --warp-policy <cardano-route-nft-policy>
```

`--router` must be a full 32 bytes: an EVM address left-padded with 12 zero
bytes. The CLI rejects anything shorter rather than padding it for you.

## Step 4 — Gas

The Cardano IGP needs an oracle for the destination domain, or payment is
refused outright (there is no fallback price):

```bash
hyperlane-cardano igp set-oracle \
  --domain 11155111 --gas-price 3000000000 --exchange-rate 7500
```

Then set the **recipient-specific** cost on the Sepolia route, if any:

```bash
# Paired with a Cardano SYNTHETIC route, delivery mints a fresh token UTXO
# (~1.2M lovelace min-UTXO), so budget ~1.5x that:
cast send <sepolia-route> "setDestinationGas(uint32,uint256)" 2003 1800000 \
  --private-key "$SEPOLIA_SIGNER_KEY" --rpc-url "$SEPOLIA_RPC_URL"

# Paired with a NATIVE or COLLATERAL Cardano route: leave it at 0. The released
# ADA already funds the recipient's min-UTxO.
```

See [CARDANO_GUIDE.md §7](../docs/CARDANO_GUIDE.md#7-interchain-gas-paymaster-igp)
for the model, and note the Cardano IGP's scale factor is `1e12` rather than the
`1e10` used by the Solidity IGP — a rate copied from EVM tooling is wrong by 100×.

## Step 5 — Refresh the agents and test

A new route changes `deployment_info.json`, so update `demo/docker/.env` and
recreate the containers — `restart` keeps the old environment:

```bash
cd demo/docker && docker compose up -d --force-recreate relayer validator-cardano scraper
```

Then run a transfer both ways per the [demo runbook](runbook.md), and reconcile:

```text
route UTXO lovelace − route min-UTxO baseline = remote totalSupply
```

## Checklist

- [ ] Decimals agree on both sides
- [ ] Cardano route deployed; NFT policy recorded
- [ ] Sepolia route deployed, ISM and aggregation hook set
- [ ] Routers enrolled in **both** directions
- [ ] IGP oracle set for the destination domain
- [ ] `.env` refreshed and containers recreated
- [ ] A transfer completes each way and the balances reconcile

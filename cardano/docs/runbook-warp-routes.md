# Runbook: Deploying Tokens and Warp Routes

How to add a token and a warp route pair to an **already-deployed** stack, both
sides. First-time deployment of the whole system is
[DEPLOYMENT_GUIDE.md](DEPLOYMENT_GUIDE.md); day-to-day agent operation and
testing is [runbook.md](runbook.md).

A warp route is always a **pair**: one route on Cardano, one on Sepolia, each
enrolled as the other's remote router. Neither half works alone.

Every snippet assumes:

```bash
cd cardano/e2e-docker
set -a; . ./.env; set +a
export ETH_RPC_URL=$SEPOLIA_RPC_URL

# run from the repo root
CLI="./cardano/cli/target/release/hyperlane-cardano --network preview \
  --deployments-dir cardano/deployments --contracts-dir cardano/contracts \
  --signing-key cardano/testnet-keys/payment.skey"
```

---

## Choosing the pair

| You want to bridge | Cardano side | Sepolia side |
| ------------------ | ------------ | ------------ |
| native ADA | **native** | synthetic (mints wADA) |
| an existing Cardano token | **collateral** (locks it) | synthetic (mints a wrapper) |
| an existing Sepolia ERC20 | **synthetic** (mints a wrapper) | collateral (locks it) |

The two sides are always opposite: whichever chain holds the real asset runs the
**collateral/native** route, the other runs the **synthetic**.

---

## Step 0 — Free up UTXOs (Cardano)

A warp deploy needs **at least two clean ADA-only UTXOs of ≥ 28 ADA**. Deploys
leave the wallet littered with reference-script NFTs, so check first:

```bash
$CLI utxo list
```

If you are short, split a large one:

```bash
$CLI utxo split --utxo <tx_hash>#<index> --count 6 --amount 60000000
```

To reclaim ADA locked in stale token/NFT UTXOs, consolidate then re-split:

```bash
$CLI utxo consolidate --max 50
```

Blockfrost paginates at 100 UTXOs, so a very cluttered wallet also slows every
query.

---

## Step 1 — Deploy a test token (Cardano, collateral routes only)

Skip this if you are bridging ADA (native) or an ERC20 from Sepolia (synthetic),
or if the token already exists.

```bash
$CLI token deploy --name CTEST --amount 1000000000
```

Creates a one-shot minting policy tied to a UTXO — only the deployer can mint,
and only once. Note the **policy ID** it prints; you need it next.

Find an existing token's policy and asset name:

```bash
$CLI utxo list        # shows policy.asset for every token held
```

---

## Step 2 — Deploy the Cardano route

Each route bundles its own reference script at output `#1`; no separate
ref-script command is needed.

```bash
# native (ADA)
$CLI warp deploy --token-type native --decimals 6 --remote-decimals 18

# collateral (locks an existing Cardano token)
#   --token-asset MUST be hex-encoded, not ASCII:  printf 'CTEST' | xxd -p
$CLI warp deploy --token-type collateral \
  --token-policy 4485fc4c31c80e7449dab7464bdcd19247ce29d74e1fe28de1044650 \
  --token-asset 4354455354 \
  --decimals 6 --remote-decimals 18

# synthetic (mints a wrapper for a remote asset)
$CLI warp deploy --token-type synthetic --decimals 6 --remote-decimals 18
```

Record the **NFT policy** from the output — that is the route's identity
everywhere else (`--warp-policy`, and the Sepolia router address).

> **`--remote-decimals 18`, not 6.** The Sepolia routes are 6-decimal but expect
> an 18-decimal wire amount. Setting `6` here makes inbound transfers mint 0.

> **`--token-asset` is hex.** An ASCII name fails with
> `Invalid hex: Odd number of digits` (odd length) or is silently misread
> (`CAFE` is valid hex).

### Synthetic only: the minting reference script

The relayer needs the minting policy on-chain to mint inbound. `warp deploy
--token-type synthetic` attempts this automatically, but the second transaction
can lose a UTXO race. Confirm `mintingRefScriptUtxo` exists in
`deployment_info.json`; if not, run it manually:

```bash
$CLI warp deploy-minting-ref --warp-policy <synthetic-nft-policy>
```

The relayer finds this UTXO on-chain by `reference_script_hash` — it needs no
config for it.

---

## Step 3 — Deploy the Sepolia route

`DeploySepoliaWarp.s.sol` deploys the standard test set (ERC20s plus synthetic
and collateral routes). Every function reads `EVM_MAILBOX` and `EVM_ISM` in the
constructor, so both must be exported for **any** invocation.

```bash
cd solidity
source sepolia.env
export EVM_MAILBOX=0xfFAEF09B3cd11D9b20d1a19bECca54EEC2884766
export EVM_ISM=<your Cardano ISM>          # from DeployCardanoISM

forge script script/warp-e2e/DeploySepoliaWarp.s.sol:DeploySepoliaWarp \
  --rpc-url $EVM_RPC_URL --broadcast --private-key $EVM_SIGNER_KEY 2>&1 | tee /tmp/step.out

# capture every address it printed, so you never transcribe one by hand
grep -oE 'EVM_[A-Z_]+=0x[0-9a-fA-F]{40}' /tmp/step.out | sed 's/^/export /' >> sepolia.env
source sepolia.env
```

Available functions (`--sig "name()"`):

| function | purpose |
| -------- | ------- |
| `run()` | deploy tokens + routes (default) |
| `mintTestTokens()` | mint 1M of each test ERC20 to the deployer |
| `preDepositCollateral()` | fund collateral routes so they can *release* |
| `enrollRouters()` | enroll the Cardano routes as remote routers |
| `setRouteHooks()` | point routes at the aggregation hook — **required** |

For a single hand-rolled route instead, deploy `HypERC20` (synthetic) or
`HypERC20Collateral` and `initialize(...)` it — see
[DEPLOYMENT_GUIDE.md § Step 2](DEPLOYMENT_GUIDE.md#step-2-deploy-sepolia-warp-routes).

---

## Step 4 — Point the Sepolia routes at the aggregation hook

**Do not skip.** A fresh route has `hook() == address(0)`, so `Mailbox.dispatch`
falls back to the mailbox's own default hook — which on the shared Sepolia
mailbox pays an IGP your relayer does not index. The transfer succeeds on Sepolia
and is then **never delivered**, with no error pointing at the cause.

```bash
export EVM_AGGREGATION_HOOK=0xeF7B2092DF766152b4f314d0ED9bC5980F1Bd2B2
forge script script/warp-e2e/DeploySepoliaWarp.s.sol:DeploySepoliaWarp \
  --sig "setRouteHooks()" --rpc-url $EVM_RPC_URL --broadcast --private-key $EVM_SIGNER_KEY

# verify — address(0) means still broken
for r in $EVM_SYNTHETIC_WADA $EVM_SYNTHETIC_WCTEST $EVM_COLLATERAL_FTEST; do
  echo "$r hook=$(cast call $r 'hook()(address)')"
done
```

The hook must wrap the **same IGP the relayer indexes** (`SEPOLIA_IGP`):

```bash
cast call $EVM_AGGREGATION_HOOK 'hooks(bytes)(address[])' 0x
# -> [<MerkleTreeHook>, <IGP>]   and that IGP == $SEPOLIA_IGP
```

---

## Step 5 — Enroll routers on both sides

Each side must know the other. Addresses are 32 bytes.

**Cardano → learns the Sepolia route** (run sequentially; parallel CLI calls on
one wallet collide on collateral selection and fail with `BadInputsUTxO`):

```bash
$CLI warp enroll-router --warp-policy <cardano-nft-policy> --domain 11155111 \
  --router 0x000000000000000000000000<sepolia-route-20-byte-address>

$CLI warp routers --warp-policy <cardano-nft-policy>     # verify
```

**Sepolia → learns the Cardano route.** A Cardano route's Hyperlane address is
`0x01000000` + its 28-byte NFT policy:

```bash
export CARDANO_NATIVE_ADA=0x01000000$(...)          # NFT policy of the native route
export CARDANO_COLLATERAL_CTEST=0x01000000...
export CARDANO_SYNTHETIC_FTEST=0x01000000...

forge script script/warp-e2e/DeploySepoliaWarp.s.sol:DeploySepoliaWarp \
  --sig "enrollRouters()" --rpc-url $EVM_RPC_URL --broadcast --private-key $EVM_SIGNER_KEY

# verify each
cast call <sepolia-route> 'routers(uint32)(bytes32)' 2003
```

Scenarios whose Cardano counterpart you did not deploy are skipped rather than
reverting, so a partial deployment enrolls only what exists.

> `0x01` prefixes a warp/NFT-policy recipient, `0x02` a script hash, and
> `0x00000000` + a 28-byte key hash a plain wallet.

---

## Step 6 — Fund the routes

A route can only **release** what it holds.

```bash
# Sepolia collateral routes
forge script script/warp-e2e/DeploySepoliaWarp.s.sol:DeploySepoliaWarp \
  --sig "preDepositCollateral()" --rpc-url $EVM_RPC_URL --broadcast --private-key $EVM_SIGNER_KEY
```

On Cardano, send tokens straight to the route address for a collateral route.
A native route needs ADA before it can release ADA.

> **Synthetic routes drain ADA.** Each inbound mint moves ~1.2 ADA of the route's
> own reserve into the recipient's min-UTXO (5.0 → 3.8 → 2.6 …). Once it hits the
> continuation minimum the relayer starts fronting that cost, so top the state
> UTXO up periodically.

---

## Step 7 — Gas configuration

Set the **recipient-specific** cost on the paired route. The
recipient-independent base is already covered by the IGP's `gasOverhead`.

```bash
# a Sepolia route paired with a Cardano SYNTHETIC route mints a fresh token UTXO
# (~1.2M lovelace min-UTXO) -> 1.5x that:
cast send <sepolia-route> "setDestinationGas(uint32,uint256)" 2003 1800000 \
  --private-key $EVM_SIGNER_KEY

# routes paired with a Cardano NATIVE or COLLATERAL route: leave at 0.
# The released ADA / locked UTXO already funds the recipient min-UTXO.
```

Full model, oracle values and enforcement policy:
[DEPLOYMENT_GUIDE.md § Gas Payment appendix](DEPLOYMENT_GUIDE.md#appendix-gas-payment-igp-configuration--enforcement).

---

## Step 8 — Refresh the agents and test

New routes change `deployment_info.json`, so refresh the agents' `.env`
(the upsert block in
[DEPLOYMENT_GUIDE.md § Phase 8.1](DEPLOYMENT_GUIDE.md#81-refresh-env-from-the-deployment)),
then recreate and test:

```bash
cd cardano/e2e-docker
docker compose up -d --force-recreate relayer validator-cardano
```

Then run the transfers in [runbook.md](runbook.md#test-2--warp-transfer-cardano--sepolia).

**A synthetic pair must be exercised remote-side first** — the Cardano synthetic
token cannot exist until an inbound transfer mints it, so send Sepolia → Cardano
before trying to send any back.

---

## Checklist

A route pair is only complete when all of these hold:

- [ ] Cardano route deployed, NFT policy recorded
- [ ] synthetic only: `mintingRefScriptUtxo` present in `deployment_info.json`
- [ ] Sepolia route deployed, address captured into `sepolia.env`
- [ ] Sepolia route `interchainSecurityModule()` == your Cardano ISM
- [ ] Sepolia route `hook()` == aggregation hook wrapping the indexed IGP
- [ ] Cardano route knows the Sepolia router (`warp routers`)
- [ ] Sepolia route knows the Cardano router (`routers(2003)`)
- [ ] whichever side releases is funded
- [ ] `destinationGas` set on routes paired with a Cardano synthetic
- [ ] agents recreated after the `.env` refresh

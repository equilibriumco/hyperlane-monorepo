# Runbook: Running the Agents and Testing Transfers

Operational cheat-sheet for an **already-deployed** stack. First-time deployment
is [DEPLOYMENT_GUIDE.md](DEPLOYMENT_GUIDE.md); adding tokens or warp routes to a
live deployment is [runbook-warp-routes.md](runbook-warp-routes.md).

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

Addresses below are the **preview** deployment. Re-read them from
`cardano/deployments/preview/deployment_info.json` and `solidity/sepolia.env`
after any redeploy — see [Refreshing after a redeploy](#refreshing-after-a-redeploy).

---

## Start and stop the agents

```bash
cd cardano/e2e-docker
docker compose start relayer validator-cardano
docker compose ps                     # both must read "healthy"
docker compose logs -f relayer
```

**Stop them whenever you are not actively testing** — an idle relayer still polls
Blockfrost and burns credits:

```bash
docker compose stop relayer validator-cardano
```

> **After editing `.env`, `start`/`restart` is not enough** — a container keeps the
> environment it started with. Use `docker compose up -d --force-recreate relayer
> validator-cardano`.

> **Only the first checkpoint needs a nudge.** After a *fresh mailbox*, the
> validator ingests leaf 0 but reports `checkpoint_queue_len: 0` and publishes
> nothing until restarted once: `docker compose restart validator-cardano`.
> Index ≥ 1 signs on its own; no restart needed.

---

## Test 1 — Greeting (Sepolia → Cardano)

Exercises the generic inbound path: dispatch → relayer mints a
`verified_message_nft` at the greeting address → you consume it.

```bash
RECIPIENT=0x02000000$(jq -r '.recipients[0].script_hash' cardano/deployments/preview/deployment_info.json)
IGP=0x5aC1BCA88fd8416C6cC2D29A832EEDA75dfF6424
SIGNER=0x1f26bfC6f52CbFad5c3fA8dABb71007b28bf4749

# 1) dispatch. Body is arbitrary bytes; the greeting prepends "Hello, ".
#    "Alice" = 0x416c696365.  --value 1 covers the mailbox protocol fee.
MSG=$(cast send $SEPOLIA_MAILBOX "dispatch(uint32,bytes32,bytes)(bytes32)" \
  2003 $RECIPIENT 0x416c696365 --value 1 --private-key $EVM_SIGNER_KEY --json \
  | jq -r '.logs[]|select(.topics[0]=="0x788dbc1b7152732178210e7f4d9d010ef016f9eafbe66786bd7169f56e0c353a")|.topics[1]')
echo "message: $MSG"

# 2) pay interchain gas separately (the 3-arg dispatch cannot carry it).
#    gas = overhead 2062550 + 1.5 x (1720800 + 4400 x body_bytes)
GAS=4680000
QUOTE=$(cast call $IGP "quoteGasPayment(uint32,uint256)(uint256)" 2003 $GAS | awk '{print $1}')
cast send $IGP "payForGas(bytes32,uint32,uint256,address)" $MSG 2003 $GAS $SIGNER \
  --value $(python3 -c "print(int($QUOTE*1.1))") --private-key $EVM_SIGNER_KEY

# 3) wait ~2 min, then consume. MUST be signed by the greeting OWNER key.
$CLI greeting list
$CLI greeting receive
$CLI greeting show     # last_greeting = "Hello, Alice", count incremented
```

Until the payment is indexed the relayer logs `Retry(GasPaymentNotFound)` and
computes **no** estimate. That is the enforcement working, not a failure.

### Sizing the gas for a larger body

Cost grows linearly at roughly **4,400 lovelace per body byte**. Measured on this
deployment:

| body | relayer estimate | paid | margin |
| ---- | ---------------- | ---- | ------ |
| 5 B | 2,960,504 | 4,680,000 | 1.58x |
| 100 B | 3,368,976 | 5,303,750 | 1.57x |
| 300 B | 4,288,268 | 6,623,750 | 1.55x |

`1720800 + 4400 x bytes` over-predicts the real cost by 1–3%, which is the safe
direction. Bodies over 64 bytes are CBOR-chunked on-chain — the CLI handles that,
but a CLI older than commit `e03fdc451` fails with
`Invalid CBOR additional info: 31`.

---

## Test 2 — Warp transfer (Cardano → Sepolia)

Gas is bundled into the transfer, so there is nothing to pay separately.

```bash
$CLI warp transfer --warp-policy <POLICY> --domain 11155111 \
  --recipient 0x000000000000000000000000<your-20-byte-eth-address> \
  --amount 10000000 --gas-limit 0
```

`--gas-limit 0` still pays the Cardano IGP overhead (211,000 gas ~ 1.5x a typical
Sepolia delivery). Route policies and where the tokens land:

| Cardano route | `--warp-policy` | Sepolia counterpart |
| ------------- | --------------- | ------------------- |
| native (ADA) | `33fcbe33558a436c900dfa1e6f28990462bed65cd41c18a8039b2b29` | wADA `0x7B1604c4` |
| collateral (CTEST) | `4ecf8a4191c594da420986ee563c3ae557df7be868c925ea594cd496` | wCTEST `0xCdBA5903` |
| synthetic | `e20cefa9bc93e56542198599d30fa2ab4033d3c0b1030623dbd2945f` | FTEST collateral `0x9e113F5E` |

Verify on Sepolia:

```bash
cast call <sepolia-route> 'balanceOf(address)(uint256)' $SIGNER
```

**Expect 2–3 minutes.** The relayer's Cardano cursor waits out a reorg buffer
before it indexes the block. It is not stuck.

---

## Test 3 — Warp transfer (Sepolia → Cardano)

```bash
# recipient = 0x00000000 + your 28-byte Cardano payment key hash
CARDANO_RECIP=0x000000001212a023380020f8c7b94b831e457b9ee65f009df9d1d588430dcc89
ROUTE=0x7B1604c4696d781Fd06Be96E0F0dA369058b3Cb5      # wADA synthetic

# Collateral routes (e.g. FTEST) need an allowance first. Let it mine before
# the transfer, or transferRemote's gas estimation reverts with
# "ERC20: insufficient allowance".
# cast send $EVM_FTEST 'approve(address,uint256)' $ROUTE <amount> --private-key $EVM_SIGNER_KEY

QUOTE=$(cast call $ROUTE 'quoteGasPayment(uint32)(uint256)' 2003 | awk '{print $1}')
cast send $ROUTE "transferRemote(uint32,bytes32,uint256)(bytes32)" \
  2003 $CARDANO_RECIP 5000000 \
  --value $(python3 -c "print(int($QUOTE*1.2))") --private-key $EVM_SIGNER_KEY
```

The quote fluctuates, so send a small margin — undershooting reverts with
`StaticAggregationHook: insufficient value`.

Amounts are in the route's own decimals. **These routes are 6-decimal**, so
5 wADA is `5000000`. (Examples elsewhere using `5e18` come from an 18-decimal
deployment.)

Verify the release on Cardano:

```bash
$CLI utxo list    # or query the recipient address for the new UTXO
```

---

## Checking gas and margins

```bash
cd cardano/e2e-docker
L() { docker compose logs relayer 2>&1 | sed -E 's/\x1b\[[0-9;]*m//g'; }

# what the relayer estimated a delivery would cost
L | grep "Estimated cost"
# -> Estimated cost: fee=1345925, payer_extra=0, verified_msg=0, total=1345925

# what a given message actually paid, and the estimate it was checked against
L | grep "<message-id-prefix>" | grep -oE "gas_amount: [0-9]+|gas_limit: [0-9]+"
```

`margin = gas_amount / gas_limit`. Target is ~1.5x. Reference values:

| leg | overhead paid | typical estimate | margin |
| --- | ------------- | ---------------- | ------ |
| C→S (any route) | 211,000 gas | 110k–141k gas | 1.5–1.9x |
| S→C native / collateral | 2,062,550 lovelace | ~1.35M | ~1.5x |
| S→C synthetic | 3,862,550 (overhead + destGas) | varies | ≥1.5x |

`payer_extra` is what the **relayer** fronts and never recovers. It is legitimately
`0` while a synthetic route still holds spare ADA — the route funds the recipient
min-UTXO out of its own reserve. Once that reserve drains to the continuation
minimum the relayer starts fronting and `payer_extra` becomes non-zero; the
paired route's `destinationGas` is what reimburses it.

---

## Refreshing after a redeploy

A redeploy changes the mailbox and every downstream state-NFT policy, so the
agents' `.env` is stale until refreshed. Run the upsert block in
**[DEPLOYMENT_GUIDE.md § Phase 8.1](DEPLOYMENT_GUIDE.md#81-refresh-env-from-the-deployment)**,
then:

```bash
docker compose up -d --force-recreate relayer validator-cardano
docker compose restart validator-cardano    # once, for checkpoint_0
```

Started against a stale `.env`, the relayer indexes a mailbox that no longer
exists and delivers nothing, with no error naming the cause.

---

## Troubleshooting

| Symptom | Cause / fix |
| ------- | ----------- |
| `Retry(GasPaymentNotFound)` right after dispatch | Payment not indexed yet. Normal for a minute; persistent means you never paid, or paid the wrong IGP. |
| C→S message never delivered, relayer never submits | Recipient's ISM does not trust the Cardano validator. Warp routes are fine; the shared `TestRecipient` uses the official default ISM and can never verify. |
| S→C delivered but nothing on Cardano | Route `hook()` is `address(0)`, so gas went to the mailbox default hook and *our* IGP saw nothing. Fix with `setRouteHooks()`. |
| `Invalid CBOR additional info: 31` | CLI older than `e03fdc451`; rebuild. Bodies > 64 bytes are chunked on-chain. |
| `ERC20: insufficient allowance` | `approve` had not mined when `transferRemote` estimated gas. Re-run the transfer. |
| `StaticAggregationHook: insufficient value` | `--value` below the live quote. Re-quote and add ~20%. |
| Message stuck in prepare, no retries | `docker compose restart relayer` re-queues pending messages from the DB. |
| `402 Project Over Limit` | Blockfrost credits exhausted. Rotate `BLOCKFROST_API_KEY`, then `up -d --force-recreate` (a running container keeps the old key). |
| Relayer floods with old undeliverable messages | Backlog from the shared Sepolia mailbox. Raise `SEPOLIA_INDEX_FROM` toward the tip, clear `relayer-data`, recreate. |

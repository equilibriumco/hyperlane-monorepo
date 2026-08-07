# The demo stack

Docker Compose for running the Cardano preview <-> Ethereum Sepolia bridge
locally: the Hyperlane agents, an indexer, and a Hyperlane Explorer pointed at
it. See [../runbook.md](../runbook.md) for driving it.

## Services

| Service | Port | Purpose |
| --- | --- | --- |
| `validator-cardano` | 9090 | Signs checkpoints for Cardano-dispatched messages |
| `relayer` | 9091 | Relays both directions between Cardano and Sepolia |
| `scraper` | 9092 | Indexes both chains into Postgres |
| `postgres` | 5432 | Scraper database |
| `hasura` | 8080 | GraphQL over the scraper DB (admin secret `hyperlane`) |
| `explorer` | 3000 | Hyperlane Explorer, reading the local Hasura |
| `hasura-init` | — | One-shot: exposes the scraper tables to Hasura, then exits |

## Quick start

```bash
cp .env.example .env      # then fill in BLOCKFROST_API_KEY
docker compose up -d --build
docker compose logs -f relayer
```

Everything else in `.env.example` is pre-filled against the live preview
deployment, including the demo signing keys. A free
[Blockfrost](https://blockfrost.io) key on network **Preview** is the only thing
you must supply.

## Checkpoint storage: no AWS needed

A Hyperlane validator normally publishes signed checkpoints to S3 or GCS so any
relayer can fetch them. This stack instead writes them to a docker volume shared
with the relayer, and the validator announces that path on-chain:

```text
file:///data/checkpoints
```

The relayer honours it because its config sets `allowLocalCheckpointSyncers`.
The trade-off is that **only a relayer with that volume mounted can verify
Cardano-origin messages** — fine for a self-contained demo, wrong for
production, where you want the announced location to be publicly readable. To
switch, set `checkpointSyncer` in `config/validator-cardano-preview.json` back
to `s3` and re-announce with `hyperlane-cardano validator announce`.

## Configuration

### Environment Variables

See `.env.example` for all required variables. Key configurations:

| Variable | Description |
|----------|-------------|
| `BLOCKFROST_API_KEY` | **The only value you must supply.** Free, network Preview |
| `CARDANO_VALIDATOR_KEY` | Validator signing key (ECDSA secp256k1) |
| `CARDANO_SIGNER_KEY` | Cardano transaction signer (for Sepolia->Cardano relay) |
| `BLOCKFROST_API_KEY` | Blockfrost API key for Cardano |
| `SEPOLIA_SIGNER_KEY` | Key for signing Sepolia transactions |
| `CARDANO_INDEX_FROM` | Starting block for Cardano indexing |
| `SEPOLIA_INDEX_FROM` | Starting block for Sepolia indexing |

### Contract Addresses

Cardano uses 28-byte identifiers padded to 32 bytes for Hyperlane. Two prefix types:

- `0x01000000` -- NFT minting policy ID (warp routes, recipients)
- `0x02000000` -- Script hash credential (mailbox, ISM)

```
State NFT Policy:  789ca889... (28 bytes)
Hyperlane Address: 0x01000000789ca889... (32 bytes)
```

## Monitoring

Prometheus metrics: validator <http://localhost:9090/metrics>, relayer
<http://localhost:9091/metrics>, scraper <http://localhost:9092/metrics>.

## Dispatching Messages

Once the services are running, you can dispatch messages from your terminal:

### Cardano -> Sepolia
Use the Cardano CLI dispatch command:
```bash
cardano-cli dispatch --destination 11155111 --recipient <sepolia-recipient> --body "Hello Sepolia"
```

### Sepolia -> Cardano
Use Foundry cast or ethers to call the Sepolia mailbox:
```bash
cast send $SEPOLIA_MAILBOX "dispatch(uint32,bytes32,bytes)" 2003 <cardano-recipient> <message-body> --rpc-url $SEPOLIA_RPC_URL --private-key $SEPOLIA_SIGNER_KEY
```

## Known noise in the logs

Two things look alarming and are not. Both are self-healing; neither stops a
transfer.

### `429 Too Many Requests` from Blockfrost

Four processes share one Blockfrost key — validator, relayer, scraper and the
explorer's Cardano balance panel — so the free tier's **per-second** rate limit
gets hit in bursts. This is not the daily quota running out. It cascades
briefly: UTXO lookups fail, a metadata build fails, the cursor stalls, the
message reprepares, and it recovers on the next attempt.

Only worry if a message stops progressing for several minutes rather than
retrying. To reduce it, raise `estimateBlockTime` in the chain config so the
agents poll less, or give the explorer its own Blockfrost key.

### Occasional `FeeTooSmallUTxO` on a first submission

```text
FeeTooSmallUTxO Mismatch (RelGTEQ) {supplied: Coin 1386642, expected: Coin 1432002}
```

Cardano fees depend on the serialised size of the transaction, but the fee is
*part* of that transaction — writing it in changes the bytes and therefore the
required fee. The builder now iterates to a fixed point, so this should not
appear; if it does, the transaction hit a size that oscillates rather than
converging, and the builder falls back to reading the node's expected fee out of
this error and resubmitting. The transfer still completes.

## Pointing the explorer at a different registry

The canonical Hyperlane registry has no `cardanopreview` entry, so the explorer
is built against our fork:

```yaml
NEXT_PUBLIC_REGISTRY_URL: https://github.com/equilibriumco/hyperlane-registry
NEXT_PUBLIC_REGISTRY_BRANCH: cardano
```

Override with `EXPLORER_REGISTRY_URL` / `EXPLORER_REGISTRY_BRANCH`. Both are
`NEXT_PUBLIC_*`, which Next.js inlines at **build** time, so changing them needs
`docker compose build explorer`, not just a restart.

**Editing a registry entry requires regenerating it.** Consumers read the
generated aggregates — `deployments/warp_routes/warpRouteConfigs.yaml` and
`chains/metadata.yaml` — not the per-chain and per-route sources. Editing a
source file alone changes nothing:

```bash
pnpm install && pnpm tsx scripts/build.ts   # in the registry checkout
git add chains/ deployments/ && git commit
```

The explorer fetches over the network, so the change must also be **pushed**
before it will see it.

## Troubleshooting

### View service status
```bash
docker compose ps
```

### Check logs
```bash
docker compose logs validator-cardano
docker compose logs relayer
```

### Restart services
```bash
docker compose restart
```

### Clean up and rebuild
```bash
docker compose down -v
docker compose build --no-cache
docker compose up -d
```


## Scraper + GraphQL API

The `scraper`, `postgres` and `hasura` services index Cardano into a relational
database and expose it over GraphQL, which is what [hyperlane-explorer] reads.

```bash
# Just the indexing stack, no agents
docker compose up -d postgres scraper hasura hasura-init
```

`hasura-init` runs `hasura/track-tables.sh` once the scraper is healthy and then
exits. Without it Hasura serves an empty schema and the explorer finds no
messages at all — which looks like a broken integration rather than unconfigured
metadata. Run the script by hand if you ever need to reapply it.

Both sides of the bridge are indexed. A message is only shown as delivered once
the scraper sees the delivery, which happens on the *destination* chain, so
scraping Cardano alone would leave every outbound message pending forever.

**Set `CARDANO_INDEX_FROM` and `SEPOLIA_INDEX_FROM` just behind the current tips
before starting.** The defaults replay a long history, and on Cardano every
block costs Blockfrost credits.

```bash
curl -s -H "project_id: $BLOCKFROST_API_KEY" \
  https://cardano-preview.blockfrost.io/api/v0/blocks/latest | jq .height
cast block-number --rpc-url "$SEPOLIA_RPC_URL"
```

The `explorer` service is already pointed at this API. To run the explorer from
a checkout instead:

```bash
NEXT_PUBLIC_API_URL=http://localhost:8080/v1/graphql \
NEXT_PUBLIC_REGISTRY_URL=https://github.com/equilibriumco/hyperlane-registry \
NEXT_PUBLIC_REGISTRY_BRANCH=cardano pnpm dev
```

The scraper applies its own migrations on startup, which include the Cardano
domain rows (`cardano` 2001, `cardanopreprod` 2002, `cardanopreview` 2003) that
the `block`/`message` foreign keys require.

| Service  | Port | Purpose                                        |
| -------- | ---- | ---------------------------------------------- |
| postgres | 5432 | Scraper database                               |
| hasura   | 8080 | GraphQL API + console (admin secret `hyperlane`) |
| scraper  | 9092 | Prometheus metrics                             |

[hyperlane-explorer]: https://github.com/equilibriumco/hyperlane-explorer/tree/cardano

## Related Documentation

- [Demo runbook](../runbook.md) — moving tokens across
- [Deployment guide](../../docs/DEPLOYMENT_GUIDE.md) — deploying your own
- [Cardano CLI](../../cli/README.md)
- [Hyperlane docs](https://docs.hyperlane.xyz/)
- [AWS validator signatures](https://docs.hyperlane.xyz/docs/operate/validators/validator-signatures-aws) — for production checkpoint storage

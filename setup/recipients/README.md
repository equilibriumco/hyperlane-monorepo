# Test recipients

Two throwaway wallets to send to, so transfers land somewhere other than the
deployer that already holds everything. Committed deliberately — they exist only
for this test bridge, and neither is ever funded beyond what a transfer puts
there. **Never point them at anything holding value.**

| Who   | Chain    | Receives            |
| ----- | -------- | ------------------- |
| Alice | Midnight | NIGHT, from C -> M  |
| Bob   | Cardano  | sNIGHT, from M -> C |

## Ready to paste

`*.recipient` holds the address already in Hyperlane's 32-byte form, which is
what both transfer commands want:

```sh
# Midnight -> Cardano, to Bob
CARDANO_RECIPIENT=$(cat setup/recipients/bob-cardano.recipient) \
AMOUNT=500000 GAS_LIMIT=750000 GAS_PRICE=1 \
  npx tsx scripts/transfer-to-cardano.ts

# Cardano -> Midnight, to Alice
$CLI warp transfer --domain 1234 \
  --recipient $(cat setup/recipients/alice-midnight.recipient) \
  --amount 200000 --warp-policy <route-nft-policy>
```

## What each file is

| File                         | What                                                       |
| ---------------------------- | ---------------------------------------------------------- |
| `bob-cardano.skey` / `.vkey` | Cardano payment keypair (`cardano-cli address key-gen`)    |
| `bob-cardano.addr`           | preview enterprise address, for explorer lookups           |
| `bob-cardano.recipient`      | `0x00` + three zero bytes + the 28-byte payment key hash   |
| `alice-midnight.seed`        | 32-byte hex seed; the wallet is derived from it at runtime |
| `alice-midnight.recipient`   | her 32-byte unshielded address, for `--recipient`          |
| `alice-midnight.addr`        | the same address in bech32m, for the faucet and wallets    |

**Two encodings, one key.** Hyperlane wants the raw 32 bytes; the faucet and
every Midnight wallet want bech32m (`mn_addr_stagenet1…`) and reject the hex as
invalid. The hex is the payload inside the bech32m string — network-tagged, so
the stagenet form is not interchangeable with devnet's.

Bob's kind byte is `0x00` because his is a key credential — a script recipient
would be `0x02`, and messages to one need an explicit `gasLimit` in the
StandardHookMetadata.

## Checking a balance

```sh
# Bob's sNIGHT (policy is the route's minting policy, not its state NFT)
curl -H "project_id: $BLOCKFROST_API_KEY" \
  "https://cardano-preview.blockfrost.io/api/v0/addresses/$(cat bob-cardano.addr)"
```

Alice's NIGHT needs the SDK — `openRecipient(<seed>)` then `balance()`, the same
path `scripts/consolidate.ts` uses.

## Regenerating

```sh
cardano-cli address key-gen --verification-key-file bob-cardano.vkey \
                            --signing-key-file bob-cardano.skey
cardano-cli address build --payment-verification-key-file bob-cardano.vkey \
                          --testnet-magic 2 --out-file bob-cardano.addr
echo "0x00000000$(cardano-cli address key-hash \
  --payment-verification-key-file bob-cardano.vkey)" > bob-cardano.recipient

openssl rand -hex 32 > alice-midnight.seed
# both forms, from tests/e2e in the hyperlane-midnight clone:
#   const a = await (await openRecipient(seed)).unshieldedAddress();
#   a.hexString                                             -> .recipient
#   MidnightBech32m.encode('stagenet', a).toString()        -> .addr
```

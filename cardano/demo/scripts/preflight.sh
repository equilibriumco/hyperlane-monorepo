#!/usr/bin/env bash
# Check that the shipped `preview` deployment is still usable before you spend
# time on a transfer.
#
#   cardano/demo/scripts/preflight.sh
#
# The deployment is shared, mutable testnet state: the wallet can be drained and
# the reference-script UTXOs can be spent by anyone holding the committed keys.
# When that happens every command fails in a different and unhelpful way, so
# check the preconditions once, here.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEMO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CARDANO_DIR="$(cd "$DEMO_DIR/.." && pwd)"
ENV_FILE="$DEMO_DIR/docker/.env"
DEPLOYMENT="$CARDANO_DIR/deployments/preview/deployment_info.json"
WALLET_ADDR_FILE="$DEMO_DIR/keys/cardano/payment.addr"

# Below this the demo cannot complete a round trip: reference-script UTXOs alone
# lock ~80 ADA, and each transfer needs a pure-ADA fee UTXO plus collateral.
MIN_ADA=100

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
ok()   { echo -e "  ${GREEN}ok${NC}    $1"; }
warn() { echo -e "  ${YELLOW}warn${NC}  $1"; WARNINGS=$((WARNINGS + 1)); }
fail() { echo -e "  ${RED}FAIL${NC}  $1"; FAILURES=$((FAILURES + 1)); }
FAILURES=0
WARNINGS=0

echo "Preflight: Cardano preview demo"
echo

echo "Tooling"
for tool in curl jq; do
    if command -v "$tool" >/dev/null 2>&1; then
        ok "$tool"
    else
        fail "$tool is not installed"
    fi
done
[ "$FAILURES" -gt 0 ] && exit 1

echo
echo "Configuration"
if [ -f "$ENV_FILE" ]; then
    ok ".env found"
    set -a
    # shellcheck disable=SC1090
    source "$ENV_FILE"
    set +a
else
    fail ".env not found at $ENV_FILE -- cp docker/.env.example docker/.env"
    exit 1
fi

if [ -n "${BLOCKFROST_API_KEY:-}" ]; then
    ok "BLOCKFROST_API_KEY is set"
else
    fail "BLOCKFROST_API_KEY is empty -- get a free Preview key at https://blockfrost.io"
    exit 1
fi

API="https://cardano-preview.blockfrost.io/api/v0"
bf() { curl -sS -H "project_id: $BLOCKFROST_API_KEY" "$API/$1"; }

echo
echo "Blockfrost"
TIP=$(bf blocks/latest)
if echo "$TIP" | jq -e '.height' >/dev/null 2>&1; then
    ok "reachable, preview tip at block $(echo "$TIP" | jq -r '.height')"
else
    # 403 means a bad key or a key issued for the wrong network; 402 means the
    # daily request quota is spent and the demo will stall mid-transfer.
    case "$(echo "$TIP" | jq -r '.status_code // "?"')" in
        403) fail "key rejected -- is this project on network \"Preview\"?" ;;
        402) fail "daily quota exhausted -- wait for the reset or use another key" ;;
        *)   fail "unexpected response: $(echo "$TIP" | jq -c '.' 2>/dev/null || echo "$TIP")" ;;
    esac
    exit 1
fi

echo
echo "Demo wallet"
if [ -f "$WALLET_ADDR_FILE" ]; then
    WALLET=$(tr -d '[:space:]' < "$WALLET_ADDR_FILE")
    BAL=$(bf "addresses/$WALLET" | jq -r '.amount[]? | select(.unit=="lovelace") | .quantity' 2>/dev/null)
    if [ -n "$BAL" ]; then
        ADA=$((BAL / 1000000))
        if [ "$ADA" -ge "$MIN_ADA" ]; then
            ok "$ADA ADA at ${WALLET:0:20}..."
        else
            fail "$ADA ADA at ${WALLET:0:20}... -- need at least $MIN_ADA; top up at https://docs.cardano.org/cardano-testnets/tools/faucet"
        fi
    else
        fail "address has no funds or was never used: $WALLET"
    fi
else
    fail "wallet address file missing: $WALLET_ADDR_FILE"
fi

echo
echo "Reference scripts"
# A spent reference-script UTXO is the failure that looks like a code bug: every
# transaction referencing it fails to build, with no hint that the cause is
# off-chain. Reference scripts are held at the deployer wallet, not at the
# script address, so the wallet's UTXO set is what proves they are still there.
if [ -n "${WALLET:-}" ]; then
    WALLET_UTXOS=$(mktemp)
    trap 'rm -f "$WALLET_UTXOS"' EXIT
    echo '[]' > "$WALLET_UTXOS"
    page=1
    while [ "$page" -le 20 ]; do
        chunk=$(bf "addresses/$WALLET/utxos?count=100&page=$page")
        jq -e 'type == "array"' <<<"$chunk" >/dev/null 2>&1 || break
        jq -s '.[0] + .[1]' "$WALLET_UTXOS" <(echo "$chunk") > "$WALLET_UTXOS.tmp" \
            && mv "$WALLET_UTXOS.tmp" "$WALLET_UTXOS"
        [ "$(jq 'length' <<<"$chunk")" -lt 100 ] && break
        page=$((page + 1))
    done

    while read -r name txhash idx; do
        [ -z "$name" ] && continue
        if jq -e --arg h "$txhash" --argjson i "$idx" \
              'any(.[]?; .tx_hash == $h and .output_index == $i)' "$WALLET_UTXOS" >/dev/null 2>&1; then
            ok "$name"
        else
            fail "$name reference script is gone ($txhash#$idx) -- redeploy, see docs/DEPLOYMENT_GUIDE.md"
        fi
    done < <(jq -r '
        [
          ( to_entries[]
            | select(.value | type == "object" and has("referenceScriptUtxo"))
            | {name: .key, ref: .value.referenceScriptUtxo} ),
          ( .warp_routes[]?
            | {name: "warp_route:\(.warpType)", ref: .referenceScriptUtxo} ),
          ( .warp_routes[]?
            | select(.mintingRefScriptUtxo)
            | {name: "warp_route_minting:\(.warpType)", ref: .mintingRefScriptUtxo} )
        ][]
        | "\(.name) \(.ref.txHash) \(.ref.outputIndex)"
    ' "$DEPLOYMENT")
else
    fail "skipped -- the wallet address could not be read"
fi

echo
echo "Agents (optional -- only if docker compose is up)"
for svc in validator:9090 relayer:9091 scraper:9092; do
    if curl -sf --max-time 2 "http://localhost:${svc#*:}/metrics" >/dev/null 2>&1; then
        ok "${svc%%:*} responding on ${svc#*:}"
    else
        warn "${svc%%:*} not responding on ${svc#*:} -- start it with: cd $DEMO_DIR/docker && docker compose up -d"
    fi
done

echo
if [ "$FAILURES" -gt 0 ]; then
    echo -e "${RED}$FAILURES check(s) failed.${NC} The shipped deployment is not usable as-is;"
    echo "deploy your own by following cardano/docs/DEPLOYMENT_GUIDE.md."
    exit 1
fi
echo -e "${GREEN}Ready.${NC}${WARNINGS:+ ($WARNINGS warning(s).)}"

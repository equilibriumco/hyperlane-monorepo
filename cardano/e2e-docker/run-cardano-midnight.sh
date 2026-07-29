#!/usr/bin/env bash
# Run the combined Cardano <-> Midnight local bridge stack: one relayer
# relaying cardanopreview <-> midnight plus four validators (two per origin,
# both origins reuse the midnight devnet fixture keys because the night
# contract's ISM enrols exactly that set at deploy time and the Cardano ISM is
# origin-scoped, so domain 1234 gets the same set via `ism set-validators`).
#
# Prerequisites:
#   - midnight devnet up in $MIDNIGHT_REPO (npm run devnet:up) — rendered
#     config at docker/.rendered/agent-config.json
#   - agents built: cargo build --release --features midnight --bin relayer --bin validator
#   - cardano/e2e-docker/.env populated (Blockfrost key, contract addresses)
#
# Usage: ./run-cardano-midnight.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
RUST_DIR="$REPO_ROOT/rust/main"
MIDNIGHT_REPO="${MIDNIGHT_REPO:-$HOME/workspace/eiger/midnight-hyperlane}"
# For stagenet: MIDNIGHT_RENDERED=$HOME/.midnight-stagenet-test/agent-config.json
#               DATA_DIR=$SCRIPT_DIR/local-data/cardano-stagenet
MIDNIGHT_RENDERED="${MIDNIGHT_RENDERED:-$MIDNIGHT_REPO/docker/.rendered/agent-config.json}"
DATA_DIR="${DATA_DIR:-$SCRIPT_DIR/local-data/cardano-midnight}"

RELAYER_BIN="$RUST_DIR/target/release/relayer"
VALIDATOR_BIN="$RUST_DIR/target/release/validator"
for bin in "$RELAYER_BIN" "$VALIDATOR_BIN"; do
    [ -x "$bin" ] || { echo "missing $bin — build with: cargo build --release --features midnight --bin relayer --bin validator"; exit 1; }
done
[ -f "$MIDNIGHT_RENDERED" ] || { echo "missing $MIDNIGHT_RENDERED — run devnet:up in $MIDNIGHT_REPO"; exit 1; }

ENV_FILE="$SCRIPT_DIR/.env"
[ -f "$ENV_FILE" ] || { echo "Missing $ENV_FILE"; exit 1; }
set -a
# shellcheck disable=SC1090
source "$ENV_FILE"
set +a

# Bridge validator keys, per origin (threshold 2 of 3 on each side). The
# cardano-origin set must be trusted by the remote chain's ISM; the
# midnight-origin set by the Cardano ISM's remote-domain entry. Per-origin
# envs fall back to the shared BRIDGE_VALIDATOR_KEY_{1,2} (one set on both
# origins — safe: checkpoint digests are origin-domain-separated), then to
# the devnet fixtures.
CARDANO_KEY_0="${BRIDGE_CARDANO_VALIDATOR_KEY_1:-${BRIDGE_VALIDATOR_KEY_1:-0x1111111111111111111111111111111111111111111111111111111111111111}}"
CARDANO_KEY_1="${BRIDGE_CARDANO_VALIDATOR_KEY_2:-${BRIDGE_VALIDATOR_KEY_2:-0x2222222222222222222222222222222222222222222222222222222222222222}}"
MIDNIGHT_KEY_0="${BRIDGE_MIDNIGHT_VALIDATOR_KEY_1:-${BRIDGE_VALIDATOR_KEY_1:-0x1111111111111111111111111111111111111111111111111111111111111111}}"
MIDNIGHT_KEY_1="${BRIDGE_MIDNIGHT_VALIDATOR_KEY_2:-${BRIDGE_VALIDATOR_KEY_2:-0x2222222222222222222222222222222222222222222222222222222222222222}}"
MIDNIGHT_RELAYER_SEED="${MIDNIGHT_RELAYER_SEED:-0000000000000000000000000000000000000000000000000000000000000003}"

mkdir -p "$DATA_DIR"

# Combined config: cardanopreview block from the envsubst'd template + the
# midnight block from the devnet's rendered config. Gas enforcement is `none`
# for now — no IGP oracle exists for domain 1234 on either side yet.
COMBINED_CONFIG="$DATA_DIR/agent-config.json"
envsubst < "$SCRIPT_DIR/config/relayer-cardano-sepolia.json" \
    | jq --slurpfile mid "$MIDNIGHT_RENDERED" '
        del(.chains.sepolia)
        | .chains.cardanopreview.connection.validatorAnnounceReferenceScriptUtxo = env.CARDANO_VA_REF_UTXO
        | .chains.midnight = $mid[0].chains.midnight
        | .chains.midnight.index.from = ((env.MIDNIGHT_INDEX_FROM // .chains.midnight.index.from) | tonumber)
        | .relayChains = "cardanopreview,midnight"
        | .originChainNames = "cardanopreview,midnight"
        | .destinationChainNames = "cardanopreview,midnight"
        | .gasPaymentEnforcement = ((env.GAS_ENFORCEMENT // "[{\"type\": \"onChainFeeQuoting\", \"gasFraction\": \"1/1\"}]") | fromjson)
        | .allowLocalCheckpointSyncers = true
        | (if env.RELAYER_BLACKLIST then .blacklist = env.RELAYER_BLACKLIST else del(.blacklist) end)
    ' > "$COMBINED_CONFIG"
echo "Combined config: $COMBINED_CONFIG"

export CONFIG_FILES="$COMBINED_CONFIG"

pids=()
cleanup() {
    echo "==> stopping agents"
    kill "${pids[@]}" 2>/dev/null || true
    wait 2>/dev/null || true
}
trap cleanup EXIT INT TERM

start_validator() { # index origin key
    local i=$1 origin=$2 key=$3
    local dir="$DATA_DIR/validator-$i"
    mkdir -p "$dir/checkpoints" "$dir/db" "$dir/config"
    (
        cd "$dir"
        HYP_ORIGINCHAINNAME="$origin" \
        HYP_VALIDATOR_KEY="$key" \
        HYP_VALIDATOR_TYPE=hexKey \
        HYP_CHECKPOINTSYNCER_TYPE=localStorage \
        HYP_CHECKPOINTSYNCER_PATH="$dir/checkpoints" \
        HYP_DB="$dir/db" \
        HYP_METRICSPORT=$((9080 + i)) \
        HYP_INTERVAL=5 \
        "$VALIDATOR_BIN" 2>&1 | sed "s/^/[validator-$i:$origin] /"
    ) | tee "$DATA_DIR/validator-$i.log" &
    pids+=($!)
}

# Cardano-origin validators announce on the Cardano VA with the shared wallet;
# stagger them so the announce txs don't collide on collateral UTxOs.
start_validator 0 cardanopreview "$CARDANO_KEY_0"
echo "==> waiting 90s before second cardano validator (VA announce wallet contention)"
sleep 90
start_validator 1 cardanopreview "$CARDANO_KEY_1"

# Midnight-origin validators: pre-announce these on the midnight VA before
# first run (heavy ZK proofs) — see the roundtrip env controller for the flow.
start_validator 2 midnight "$MIDNIGHT_KEY_0"
start_validator 3 midnight "$MIDNIGHT_KEY_1"

mkdir -p "$DATA_DIR/relayer/db" "$DATA_DIR/relayer/config"
(
    cd "$DATA_DIR/relayer"
    HYP_RELAYCHAINS="cardanopreview,midnight" \
    HYP_DB="$DATA_DIR/relayer/db" \
    HYP_ALLOWLOCALCHECKPOINTSYNCERS=true \
    HYP_METRICSPORT=9089 \
    MIDNIGHT_RELAYER_SEED="$MIDNIGHT_RELAYER_SEED" \
    MIDNIGHT_SUBMIT_TIMEOUT_SECS="${MIDNIGHT_SUBMIT_TIMEOUT_SECS:-1800}" \
    MIDNIGHT_NETWORK="${MIDNIGHT_NETWORK:-devnet}" \
    "$RELAYER_BIN" 2>&1 | sed 's/^/[relayer] /'
) | tee "$DATA_DIR/relayer.log" &
pids+=($!)

echo "==> agents up: validators :9080-:9083, relayer :9089 — Ctrl-C to stop"
wait

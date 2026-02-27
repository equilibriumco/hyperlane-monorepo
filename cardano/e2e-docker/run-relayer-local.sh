#!/usr/bin/env bash
# Run the Cardano relayer directly on the host, skipping Docker image rebuild.
# Incremental cargo builds are much faster (~30s) than full Docker builds (~5min).
#
# Usage:
#   ./run-relayer-local.sh            # just run (uses last binary)
#   ./run-relayer-local.sh --build    # rebuild binary first
#   ./run-relayer-local.sh --release  # rebuild in release mode (slower, faster binary)
#   DATA_DIR=/tmp/relayer_db ./run-relayer-local.sh  # custom data dir

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
RUST_DIR="$REPO_ROOT/rust/main"
CONFIG_TEMPLATE="${RELAYER_CONFIG:-relayer-cardano-sepolia.json}"
DATA_DIR="${DATA_DIR:-$SCRIPT_DIR/local-data/relayer}"

BUILD=false
RELEASE=false
for arg in "$@"; do
    case "$arg" in
        --build)   BUILD=true ;;
        --release) BUILD=true; RELEASE=true ;;
    esac
done

# Build the relayer binary if requested
if $BUILD; then
    echo "Building relayer binary..."
    if $RELEASE; then
        cargo build --release -p relayer --manifest-path "$RUST_DIR/Cargo.toml"
        RELAYER_BIN="$RUST_DIR/target/release/relayer"
    else
        cargo build -p relayer --manifest-path "$RUST_DIR/Cargo.toml"
        RELAYER_BIN="$RUST_DIR/target/debug/relayer"
    fi
else
    # Auto-detect: prefer release binary if it exists and is newer
    RELEASE_BIN="$RUST_DIR/target/release/relayer"
    DEBUG_BIN="$RUST_DIR/target/debug/relayer"
    if [ -f "$RELEASE_BIN" ] && [ -f "$DEBUG_BIN" ]; then
        if [ "$RELEASE_BIN" -nt "$DEBUG_BIN" ]; then
            RELAYER_BIN="$RELEASE_BIN"
        else
            RELAYER_BIN="$DEBUG_BIN"
        fi
    elif [ -f "$RELEASE_BIN" ]; then
        RELAYER_BIN="$RELEASE_BIN"
    elif [ -f "$DEBUG_BIN" ]; then
        RELAYER_BIN="$DEBUG_BIN"
    else
        echo "No relayer binary found. Run with --build first."
        exit 1
    fi
fi
echo "Using relayer binary: $RELAYER_BIN"

# Load environment variables from .env
ENV_FILE="$SCRIPT_DIR/.env"
if [ ! -f "$ENV_FILE" ]; then
    echo "Missing $ENV_FILE"
    exit 1
fi
set -a
# shellcheck disable=SC1090
source "$ENV_FILE"
set +a

# Process config template (envsubst replaces ${VAR} placeholders)
PROCESSED_CONFIG="/tmp/relayer-local-config.json"
envsubst < "$SCRIPT_DIR/config/$CONFIG_TEMPLATE" \
    | jq --arg db "$DATA_DIR" '. + {db: $db}' \
    > "$PROCESSED_CONFIG"
echo "Config written to $PROCESSED_CONFIG (db=$DATA_DIR)"

# Create data directory and an empty config/ subdirectory.
# The relayer loader always reads ./config/*.json on startup; an empty dir
# prevents it from loading unprocessed template files.
mkdir -p "$DATA_DIR" "$DATA_DIR/config"
echo "Data directory: $DATA_DIR"

export CONFIG_FILES="$PROCESSED_CONFIG"

echo "Starting relayer..."
# Run from DATA_DIR so it finds the empty ./config/ (not the templates dir).
cd "$DATA_DIR"
exec "$RELAYER_BIN"

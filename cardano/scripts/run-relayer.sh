#!/bin/bash
# Run Hyperlane relayer for Cardano <-> Fuji testing

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MONOREPO_DIR="/home/guilherme/workspace/eiger/hyperlane-monorepo/rust/main"
KEYS_DIR="$SCRIPT_DIR/../testnet-keys"
CONFIG_FILE="$SCRIPT_DIR/../config/relayer-config.json"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info() { echo -e "${BLUE}[INFO]${NC} $1"; }
log_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }

# Check prerequisites
if [ ! -f "$KEYS_DIR/payment.skey" ]; then
    log_error "Cardano signing key not found at $KEYS_DIR/payment.skey"
    exit 1
fi

if [ ! -f "$CONFIG_FILE" ]; then
    log_error "Config file not found at $CONFIG_FILE"
    exit 1
fi

# Signer configuration via environment variables
# For Cardano, point to the signing key
export HYP_CHAINS_CARDANOPREVIEW_SIGNER_TYPE="cardanoKey"
export HYP_CHAINS_CARDANOPREVIEW_SIGNER_KEYPATH="$KEYS_DIR/payment.skey"

# For Fuji (EVM), use the funded wallet
export HYP_CHAINS_FUJI_SIGNER_TYPE="hexKey"
export HYP_CHAINS_FUJI_SIGNER_KEY="0x8ab7a00c32d023e31b7763e7d44d4868d82ecd618fd77a322c61783a57b02505"

# Log level
export RUST_LOG="info,hyperlane_cardano=debug,hyperlane_base=debug"

log_info "=== Starting Hyperlane Relayer ==="
log_info "Config: $CONFIG_FILE"
log_info "Origin chains: fuji, cardanopreview"
log_info "Destination chains: fuji, cardanopreview"
log_info ""

# Create DB directory
mkdir -p /tmp/hyperlane-relayer-db

# Run relayer from /tmp to avoid loading other config files from config/ directory
cd /tmp
log_info "Starting relayer..."
log_info ""

# Run with only our config file
"$MONOREPO_DIR/target/debug/relayer" --configPath "$CONFIG_FILE"

#!/bin/bash
# Send a test message from Cardano to Fuji via Hyperlane
#
# Usage:
#   cd cardano/e2e-docker
#   ./send-cardano-to-fuji.sh [message]

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CLI_DIR="$SCRIPT_DIR/../cli"

# Load environment variables from .env
if [ -f "$SCRIPT_DIR/.env" ]; then
    set -a
    source "$SCRIPT_DIR/.env"
    set +a
else
    echo "Error: .env file not found at $SCRIPT_DIR/.env"
    echo "Copy .env.example to .env and fill in your values"
    exit 1
fi

# Fuji configuration
FUJI_DOMAIN="43113"
# The CLI automatically pads shorter addresses (20-byte ETH) to 32-byte Hyperlane format
TEST_RECIPIENT="${FUJI_TEST_RECIPIENT:-0x5738088244a020f9B875D8d22D425F3082c66C1C}"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info() { echo -e "${BLUE}[INFO]${NC} $1"; }
log_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }

# Check for required env vars
if [ -z "$BLOCKFROST_API_KEY" ]; then
    log_error "BLOCKFROST_API_KEY not set in .env"
    exit 1
fi

# Check CLI exists
CLI="$CLI_DIR/target/release/hyperlane-cardano"
if [ ! -f "$CLI" ]; then
    log_error "CLI not found at $CLI"
    echo "Build with: cd ../cli && cargo build --release"
    exit 1
fi

# Generate test message with timestamp
TIMESTAMP=$(date +%s)
MESSAGE="${1:-Hello from Cardano at $TIMESTAMP}"

log_info "=== Cardano -> Fuji E2E Test ==="
log_info ""
log_info "Destination: Fuji (Domain: $FUJI_DOMAIN)"
log_info "Recipient: $TEST_RECIPIENT"
log_info "Message: \"$MESSAGE\""
log_info ""

# Dispatch message
log_info "Dispatching message via Cardano Mailbox..."

DISPATCH_OUTPUT=$($CLI mailbox dispatch \
    --destination "$FUJI_DOMAIN" \
    --recipient "$TEST_RECIPIENT" \
    --body "$MESSAGE" \
    --api-key "$BLOCKFROST_API_KEY" \
    --signing-key "$SCRIPT_DIR/../testnet-keys/payment.skey" \
    --deployments-dir "$SCRIPT_DIR/../deployments" \
    --contracts-dir "$SCRIPT_DIR/../contracts" \
    --network preview 2>&1) || {
    log_error "Error dispatching message:"
    echo "$DISPATCH_OUTPUT"
    exit 1
}

echo "$DISPATCH_OUTPUT"
echo ""

log_success "=== Dispatch Complete ==="
log_info ""
log_info "Next steps:"
log_info "1. Validator signs checkpoint for this message"
log_info "2. Relayer picks up message and submits to Fuji"
log_info "3. Verify on Fuji with:"
log_info ""
log_info "   cast call $TEST_RECIPIENT 'getLastMessage()(uint32,bytes32,bytes)' \\"
log_info "       --rpc-url $FUJI_RPC_URL"
log_info ""
log_info "Or check Snowtrace: https://testnet.snowtrace.io/address/$TEST_RECIPIENT"

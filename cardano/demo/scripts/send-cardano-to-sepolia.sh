#!/bin/bash
# Send a test message from Cardano to Sepolia via Hyperlane
#
# Usage:
#   cardano/demo/scripts/send-cardano-to-sepolia.sh [message]

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEMO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CARDANO_DIR="$(cd "$DEMO_DIR/.." && pwd)"
CLI_DIR="$CARDANO_DIR/cli"
ENV_FILE="$DEMO_DIR/docker/.env"

# Load environment variables from .env
if [ -f "$ENV_FILE" ]; then
    set -a
    source "$ENV_FILE"
    set +a
else
    echo "Error: .env file not found at $ENV_FILE"
    echo "Copy docker/.env.example to docker/.env and fill in your values"
    exit 1
fi

# Sepolia configuration
SEPOLIA_DOMAIN="11155111"
# The CLI automatically pads shorter addresses (20-byte ETH) to 32-byte Hyperlane format
TEST_RECIPIENT="${SEPOLIA_TEST_RECIPIENT:-0x5738088244a020f9B875D8d22D425F3082c66C1C}"

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
# A relative CARDANO_SIGNING_KEY is relative to cardano/, not to wherever this
# script happens to be invoked from.
case "${CARDANO_SIGNING_KEY:-}" in
    /*|"") ;;
    *) CARDANO_SIGNING_KEY="$CARDANO_DIR/$CARDANO_SIGNING_KEY" ;;
esac

if [ -z "$CARDANO_SIGNING_KEY" ] || [ ! -f "$CARDANO_SIGNING_KEY" ]; then
    log_error "CARDANO_SIGNING_KEY must point to an existing payment signing key"
    exit 1
fi

# Check CLI exists
CLI="$CLI_DIR/target/release/hyperlane-cardano"
if [ ! -f "$CLI" ]; then
    log_error "CLI not found at $CLI"
    echo "Build with: cd $CLI_DIR && cargo build --release"
    exit 1
fi

# Generate test message with timestamp
TIMESTAMP=$(date +%s)
MESSAGE="${1:-Hello from Cardano at $TIMESTAMP}"

log_info "=== Cardano -> Sepolia E2E Test ==="
log_info ""
log_info "Destination: Sepolia (Domain: $SEPOLIA_DOMAIN)"
log_info "Recipient: $TEST_RECIPIENT"
log_info "Message: \"$MESSAGE\""
log_info ""

# Dispatch message
log_info "Dispatching message via Cardano Mailbox..."

DISPATCH_OUTPUT=$($CLI mailbox dispatch \
    --destination "$SEPOLIA_DOMAIN" \
    --recipient "$TEST_RECIPIENT" \
    --body "$MESSAGE" \
    --api-key "$BLOCKFROST_API_KEY" \
    --signing-key "$CARDANO_SIGNING_KEY" \
    --deployments-dir "$CARDANO_DIR/deployments" \
    --contracts-dir "$CARDANO_DIR/contracts" \
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
log_info "2. Relayer picks up message and submits to Sepolia"
log_info "3. Verify on Sepolia with:"
log_info ""
log_info "   cast call $TEST_RECIPIENT 'getLastMessage()(uint32,bytes32,bytes)' \\"
log_info "       --rpc-url $SEPOLIA_RPC_URL"
log_info ""
log_info "Or check Etherscan: https://sepolia.etherscan.io/address/$TEST_RECIPIENT"

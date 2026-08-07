#!/bin/bash
# Send a test message from Sepolia to Cardano via Hyperlane
#
# Usage:
#   cardano/demo/scripts/send-sepolia-to-cardano.sh [message]
#
# Prerequisites:
#   - Foundry installed (cast command)
#   - ETH balance on the signer address

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEMO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
ENV_FILE="$DEMO_DIR/docker/.env"

# Load environment variables from .env
if [ -f "$ENV_FILE" ]; then
    set -a
    source "$ENV_FILE"
    set +a
    # Unset RUST_LOG to prevent trace output from corrupting cast output
    unset RUST_LOG
else
    echo "Error: .env file not found at $ENV_FILE"
    echo "Copy docker/.env.example to docker/.env and fill in your values"
    exit 1
fi

# Cardano configuration
CARDANO_DOMAIN="2003"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info() { echo -e "${BLUE}[INFO]${NC} $1"; }
log_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }

# Check for required env vars
if [ -z "$SEPOLIA_RPC_URL" ]; then
    log_error "SEPOLIA_RPC_URL not set in .env"
    exit 1
fi

if [ -z "$SEPOLIA_SIGNER_KEY" ]; then
    log_error "SEPOLIA_SIGNER_KEY not set in .env"
    exit 1
fi

if [ -z "$SEPOLIA_MAILBOX" ]; then
    log_error "SEPOLIA_MAILBOX not set in .env"
    exit 1
fi

if [ -z "$CARDANO_RECIPIENT" ]; then
    log_error "CARDANO_RECIPIENT not set in .env"
    exit 1
fi

# Check cast is available
if ! command -v cast &>/dev/null; then
    log_error "cast command not found. Please install Foundry: https://book.getfoundry.sh/getting-started/installation"
    exit 1
fi

# Generate test message with timestamp
TIMESTAMP=$(date +%s)
MESSAGE="${1:-Hello from Sepolia at $TIMESTAMP}"

# Convert message to hex
MESSAGE_HEX=$(echo -n "$MESSAGE" | xxd -p | tr -d '\n')

log_info "=== Sepolia -> Cardano E2E Test ==="
log_info ""
log_info "Destination: Cardano Preview (Domain: $CARDANO_DOMAIN)"
log_info "Recipient: $CARDANO_RECIPIENT"
log_info "Message: \"$MESSAGE\""
log_info ""

# Get sender address for display
SENDER_ADDRESS=$(cast wallet address --private-key "$SEPOLIA_SIGNER_KEY" 2>/dev/null || echo "unknown")
log_info "Sender: $SENDER_ADDRESS"

# Check balance
BALANCE=$(cast balance "$SENDER_ADDRESS" --rpc-url "$SEPOLIA_RPC_URL" 2>/dev/null || echo "0")
log_info "Balance: $BALANCE wei"
log_info ""

# Query required protocol fee
log_info "Querying required protocol fee..."
QUOTE_FEE=$(cast call "$SEPOLIA_MAILBOX" \
    "quoteDispatch(uint32,bytes32,bytes)(uint256)" \
    "$CARDANO_DOMAIN" \
    "$CARDANO_RECIPIENT" \
    "0x$MESSAGE_HEX" \
    --rpc-url "$SEPOLIA_RPC_URL" 2>&1) || {
    log_error "Error querying fee:"
    echo "$QUOTE_FEE"
    exit 1
}

log_info "Required fee: $QUOTE_FEE wei"
log_info ""

# Dispatch message via Sepolia Mailbox
log_info "Dispatching message via Sepolia Mailbox..."

# The dispatch function signature: dispatch(uint32 destinationDomain, bytes32 recipientAddress, bytes messageBody)
TX_HASH=$(cast send "$SEPOLIA_MAILBOX" \
    "dispatch(uint32,bytes32,bytes)(bytes32)" \
    "$CARDANO_DOMAIN" \
    "$CARDANO_RECIPIENT" \
    "0x$MESSAGE_HEX" \
    --rpc-url "$SEPOLIA_RPC_URL" \
    --private-key "$SEPOLIA_SIGNER_KEY" \
    --value "$QUOTE_FEE" \
    --json 2>&1) || {
    log_error "Error dispatching message:"
    echo "$TX_HASH"
    exit 1
}

# Parse transaction hash from JSON output
TX_HASH_PARSED=$(echo "$TX_HASH" | jq -r '.transactionHash' 2>/dev/null || echo "$TX_HASH")

log_success "Transaction submitted!"
log_info "TX Hash: $TX_HASH_PARSED"
echo ""

log_success "=== Dispatch Complete ==="
log_info ""
log_info "Next steps:"
log_info "1. Sepolia validator signs checkpoint for this message"
log_info "2. Relayer picks up message and submits to Cardano"
log_info "3. Monitor relayer logs:"
log_info ""
log_info "   docker compose logs -f relayer"
log_info ""
log_info "Or check Etherscan: https://sepolia.etherscan.io/tx/$TX_HASH_PARSED"

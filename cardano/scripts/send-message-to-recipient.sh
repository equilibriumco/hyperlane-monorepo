#!/bin/bash
# Send a test message from Fuji to a Cardano recipient contract via Hyperlane
#
# Usage:
#   ./send-message-to-recipient.sh <recipient_script_hash> [message]
#
# Examples:
#   # Send to a specific recipient with default message
#   ./send-message-to-recipient.sh 931e71c75bd0ac35ff9024b3c2a578e006bf3abca509c11734f7f9bc
#
#   # Send with custom message
#   ./send-message-to-recipient.sh 931e71c75bd0ac35ff9024b3c2a578e006bf3abca509c11734f7f9bc "Hello from Fuji!"
#
# Prerequisites:
#   1. Deploy a recipient contract using:
#      cd cardano && ./cli/target/release/hyperlane-cardano init recipient --dry-run
#
#   2. Register the recipient in the registry (if not already done)
#
#   3. Ensure the relayer is running with the recipient registered

set -e

# Configuration
FUJI_RPC="https://api.avax-test.network/ext/bc/C/rpc"
FUJI_MAILBOX="0x5b6CFf85442B851A8e6eaBd2A4E4507B5135B3B0"
FUJI_PRIVATE_KEY="${FUJI_PRIVATE_KEY:-0x8ab7a00c32d023e31b7763e7d44d4868d82ecd618fd77a322c61783a57b02505}"

# Cardano Preview domain
CARDANO_DOMAIN=2003

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

log_info() { echo -e "${BLUE}[INFO]${NC} $1"; }
log_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }

usage() {
    echo "Usage: $0 <recipient_script_hash> [message]"
    echo ""
    echo "Arguments:"
    echo "  recipient_script_hash  28-byte Cardano script hash (56 hex chars)"
    echo "  message                Optional message body (default: 'Hello from Hyperlane!')"
    echo ""
    echo "Environment Variables:"
    echo "  FUJI_PRIVATE_KEY       Private key for signing (default: test key)"
    echo "  FUJI_RPC               RPC endpoint (default: Avalanche Fuji)"
    echo ""
    echo "Examples:"
    echo "  $0 931e71c75bd0ac35ff9024b3c2a578e006bf3abca509c11734f7f9bc"
    echo "  $0 931e71c75bd0ac35ff9024b3c2a578e006bf3abca509c11734f7f9bc 'Test message #1'"
    exit 1
}

# Parse arguments
if [ $# -lt 1 ]; then
    usage
fi

RECIPIENT_SCRIPT_HASH="$1"
MESSAGE="${2:-Hello from Hyperlane!}"

# Validate script hash (should be 56 hex characters = 28 bytes)
if ! [[ "$RECIPIENT_SCRIPT_HASH" =~ ^[0-9a-fA-F]{56}$ ]]; then
    log_error "Invalid recipient script hash. Expected 56 hex characters (28 bytes)."
    log_error "Got: $RECIPIENT_SCRIPT_HASH (${#RECIPIENT_SCRIPT_HASH} chars)"
    exit 1
fi

# Convert script hash to Hyperlane address format
# Format: 0x02 (Cardano protocol) + 000000 (padding) + script_hash (28 bytes)
CARDANO_RECIPIENT="0x02000000${RECIPIENT_SCRIPT_HASH}"

# Message body (hex encoded)
MESSAGE_BODY=$(echo -n "$MESSAGE" | xxd -p | tr -d '\n')

log_info "=== Sending Message: Fuji -> Cardano Recipient ==="
log_info ""
log_info "Configuration:"
log_info "  Fuji Mailbox:     $FUJI_MAILBOX"
log_info "  Cardano Domain:   $CARDANO_DOMAIN"
log_info ""
log_info "Recipient:"
log_info "  Script Hash:      $RECIPIENT_SCRIPT_HASH"
log_info "  Hyperlane Addr:   $CARDANO_RECIPIENT"
log_info ""
log_info "Message:"
log_info "  Text:             $MESSAGE"
log_info "  Hex:              0x$MESSAGE_BODY"
log_info ""

# Check if cast is available
if ! command -v cast &>/dev/null; then
    log_error "cast (foundry) not found. Please install foundry:"
    log_error "  curl -L https://foundry.paradigm.xyz | bash"
    exit 1
fi

# Get sender address
SENDER=$(cast wallet address "$FUJI_PRIVATE_KEY")
log_info "Sender Address: $SENDER"

# Check balance
BALANCE=$(cast balance "$SENDER" --rpc-url "$FUJI_RPC")
BALANCE_ETH=$(cast from-wei "$BALANCE" 2>/dev/null || echo "$BALANCE wei")
log_info "Sender Balance: $BALANCE_ETH"

if [ "$BALANCE" = "0" ]; then
    log_error "Sender has no balance. Please fund the address with AVAX on Fuji."
    log_error "Faucet: https://faucet.avax.network/"
    exit 1
fi

# Get the quote for the dispatch
log_info ""
log_info "Getting dispatch quote..."
QUOTE=$(cast call "$FUJI_MAILBOX" \
    "quoteDispatch(uint32,bytes32,bytes)(uint256)" \
    "$CARDANO_DOMAIN" \
    "$CARDANO_RECIPIENT" \
    "0x$MESSAGE_BODY" \
    --rpc-url "$FUJI_RPC" 2>/dev/null || echo "0")

QUOTE_ETH=$(cast from-wei "$QUOTE" 2>/dev/null || echo "$QUOTE wei")
log_info "Quote: $QUOTE_ETH"

# Send the dispatch transaction
log_info ""
log_info "Sending dispatch transaction..."

TX_OUTPUT=$(cast send "$FUJI_MAILBOX" \
    "dispatch(uint32,bytes32,bytes)" \
    "$CARDANO_DOMAIN" \
    "$CARDANO_RECIPIENT" \
    "0x$MESSAGE_BODY" \
    --rpc-url "$FUJI_RPC" \
    --private-key "$FUJI_PRIVATE_KEY" \
    --value "$QUOTE" \
    --json 2>&1)

TX_HASH=$(echo "$TX_OUTPUT" | jq -r '.transactionHash // empty' 2>/dev/null)

if [ -z "$TX_HASH" ]; then
    log_warn "Failed with quote value, retrying without value..."
    TX_OUTPUT=$(cast send "$FUJI_MAILBOX" \
        "dispatch(uint32,bytes32,bytes)" \
        "$CARDANO_DOMAIN" \
        "$CARDANO_RECIPIENT" \
        "0x$MESSAGE_BODY" \
        --rpc-url "$FUJI_RPC" \
        --private-key "$FUJI_PRIVATE_KEY" \
        --json 2>&1)
    TX_HASH=$(echo "$TX_OUTPUT" | jq -r '.transactionHash // empty' 2>/dev/null)
fi

if [ -z "$TX_HASH" ]; then
    log_error "Failed to send transaction"
    log_error "Output: $TX_OUTPUT"
    exit 1
fi

log_success "Transaction sent!"
log_info ""
echo -e "${CYAN}========================================${NC}"
echo -e "${CYAN}           Transaction Details          ${NC}"
echo -e "${CYAN}========================================${NC}"
log_info "TX Hash:    $TX_HASH"
log_info "Explorer:   https://testnet.snowtrace.io/tx/$TX_HASH"
echo -e "${CYAN}========================================${NC}"
log_info ""

# Try to get message ID from logs
log_info "Fetching transaction receipt..."
sleep 2

RECEIPT=$(cast receipt "$TX_HASH" --rpc-url "$FUJI_RPC" --json 2>/dev/null || echo "{}")
LOGS=$(echo "$RECEIPT" | jq -r '.logs // []')

# The Dispatch event is: Dispatch(address indexed sender, uint32 indexed destination, bytes32 indexed recipient, bytes message)
# Topic 0 is the event signature
MESSAGE_ID=$(echo "$LOGS" | jq -r '.[0].topics[0] // empty' 2>/dev/null)

if [ -n "$MESSAGE_ID" ]; then
    log_info ""
    log_info "Dispatch Event Found:"
    log_info "  First Topic: $MESSAGE_ID"
fi

log_info ""
echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}              Next Steps                ${NC}"
echo -e "${GREEN}========================================${NC}"
log_info ""
log_info "1. The relayer should pick up this message and deliver it to Cardano"
log_info ""
log_info "2. Monitor the relayer logs:"
log_info "   tail -f /tmp/relayer.log"
log_info ""
log_info "3. Check the recipient's state on Cardano after delivery:"
log_info "   - messages_received should increment"
log_info "   - last_message should contain: '$MESSAGE'"
log_info ""
log_info "4. If using the generic_recipient, verify the UTXO datum was updated"
log_info ""

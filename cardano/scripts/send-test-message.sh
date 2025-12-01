#!/bin/bash
# Send a test message from Fuji to Cardano via Hyperlane

set -e

# Configuration
FUJI_RPC="https://api.avax-test.network/ext/bc/C/rpc"
FUJI_MAILBOX="0x5b6CFf85442B851A8e6eaBd2A4E4507B5135B3B0"
FUJI_PRIVATE_KEY="0x8ab7a00c32d023e31b7763e7d44d4868d82ecd618fd77a322c61783a57b02505"

# Cardano Preview domain
CARDANO_DOMAIN=2003

# Recipient on Cardano (our mailbox policy ID padded to 32 bytes)
# Using the mailbox as recipient for testing
CARDANO_RECIPIENT="0x00000000fd308acbff820d0db35e2a50fd9bca23049ed4ceed21f795a09eb467"

# Message body (hex encoded "Hello Cardano from Fuji!")
MESSAGE_BODY=$(echo -n "Hello Cardano from Fuji!" | xxd -p | tr -d '\n')

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info() { echo -e "${BLUE}[INFO]${NC} $1"; }
log_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }

log_info "=== Sending Test Message: Fuji -> Cardano ==="
log_info "Mailbox: $FUJI_MAILBOX"
log_info "Destination Domain: $CARDANO_DOMAIN"
log_info "Recipient: $CARDANO_RECIPIENT"
log_info "Message: Hello Cardano from Fuji!"
log_info ""

# Check if cast is available
if ! command -v cast &> /dev/null; then
    log_error "cast (foundry) not found. Please install foundry: curl -L https://foundry.paradigm.xyz | bash"
    exit 1
fi

# Get sender address
SENDER=$(cast wallet address "$FUJI_PRIVATE_KEY")
log_info "Sender: $SENDER"

# Check balance
BALANCE=$(cast balance "$SENDER" --rpc-url "$FUJI_RPC")
log_info "Balance: $BALANCE wei"

# Dispatch function signature: dispatch(uint32 _destinationDomain, bytes32 _recipientAddress, bytes calldata _messageBody)
# Returns: bytes32 messageId

log_info ""
log_info "Calling dispatch on Fuji Mailbox..."

# First, let's get the quote for the dispatch
QUOTE=$(cast call "$FUJI_MAILBOX" \
    "quoteDispatch(uint32,bytes32,bytes)(uint256)" \
    "$CARDANO_DOMAIN" \
    "$CARDANO_RECIPIENT" \
    "0x$MESSAGE_BODY" \
    --rpc-url "$FUJI_RPC" 2>/dev/null || echo "0")

log_info "Quote: $QUOTE wei"

# Send the dispatch transaction
log_info "Sending dispatch transaction..."

TX_HASH=$(cast send "$FUJI_MAILBOX" \
    "dispatch(uint32,bytes32,bytes)" \
    "$CARDANO_DOMAIN" \
    "$CARDANO_RECIPIENT" \
    "0x$MESSAGE_BODY" \
    --rpc-url "$FUJI_RPC" \
    --private-key "$FUJI_PRIVATE_KEY" \
    --value "$QUOTE" \
    --json 2>&1 | jq -r '.transactionHash // empty')

if [ -z "$TX_HASH" ]; then
    log_error "Failed to send transaction"
    # Try without value
    log_info "Retrying without value..."
    TX_HASH=$(cast send "$FUJI_MAILBOX" \
        "dispatch(uint32,bytes32,bytes)" \
        "$CARDANO_DOMAIN" \
        "$CARDANO_RECIPIENT" \
        "0x$MESSAGE_BODY" \
        --rpc-url "$FUJI_RPC" \
        --private-key "$FUJI_PRIVATE_KEY" \
        --json 2>&1 | jq -r '.transactionHash // empty')
fi

if [ -z "$TX_HASH" ]; then
    log_error "Failed to send transaction"
    exit 1
fi

log_success "Transaction sent!"
log_info "TX Hash: $TX_HASH"
log_info ""
log_info "View on explorer: https://testnet.snowtrace.io/tx/$TX_HASH"
log_info ""
log_info "The relayer should pick up this message and deliver it to Cardano."
log_info "Monitor the relayer logs: tail -f /tmp/relayer.log"

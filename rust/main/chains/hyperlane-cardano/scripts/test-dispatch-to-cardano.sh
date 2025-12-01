#!/bin/bash
# Test script for dispatching a message from Sepolia to Cardano
# Uses Foundry's cast tool

set -e

# Configuration
SEPOLIA_RPC_URL="${SEPOLIA_RPC_URL:-}"
SEPOLIA_MAILBOX="${SEPOLIA_MAILBOX:-}"
EVM_PRIVATE_KEY="${EVM_PRIVATE_KEY:-}"

# Destination
DEST_DOMAIN=2002  # Cardano Preprod
RECIPIENT="${CARDANO_RECIPIENT:-0x0000000000000000000000000000000000000000000000000000000000000001}"
MESSAGE="Hello from Sepolia!"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

print_header() {
    echo -e "\n${GREEN}=== $1 ===${NC}\n"
}

print_error() {
    echo -e "${RED}ERROR: $1${NC}"
}

# Check prerequisites
check_prerequisites() {
    print_header "Checking Prerequisites"

    if [ -z "$SEPOLIA_RPC_URL" ]; then
        print_error "SEPOLIA_RPC_URL not set"
        exit 1
    fi

    if [ -z "$SEPOLIA_MAILBOX" ]; then
        print_error "SEPOLIA_MAILBOX not set"
        exit 1
    fi

    if [ -z "$EVM_PRIVATE_KEY" ]; then
        print_error "EVM_PRIVATE_KEY not set"
        exit 1
    fi

    command -v cast >/dev/null 2>&1 || { print_error "cast (Foundry) not found. Install: curl -L https://foundry.paradigm.xyz | bash"; exit 1; }

    echo "All prerequisites met!"
}

# Get sender address
get_sender_address() {
    cast wallet address --private-key "$EVM_PRIVATE_KEY"
}

# Get sender balance
get_sender_balance() {
    cast balance $(get_sender_address) --rpc-url "$SEPOLIA_RPC_URL"
}

# Get current nonce
get_mailbox_nonce() {
    cast call "$SEPOLIA_MAILBOX" "nonce()(uint32)" --rpc-url "$SEPOLIA_RPC_URL"
}

# Dispatch message
dispatch_message() {
    local body_hex=$(echo -n "$MESSAGE" | xxd -p | tr -d '\n')
    body_hex="0x${body_hex}"

    print_header "Dispatching Message"
    echo "Destination: $DEST_DOMAIN (Cardano Preprod)"
    echo "Recipient: $RECIPIENT"
    echo "Message: $MESSAGE"
    echo "Body (hex): $body_hex"
    echo ""

    echo "Sending transaction..."
    local tx_hash=$(cast send "$SEPOLIA_MAILBOX" \
        "dispatch(uint32,bytes32,bytes)(bytes32)" \
        "$DEST_DOMAIN" \
        "$RECIPIENT" \
        "$body_hex" \
        --rpc-url "$SEPOLIA_RPC_URL" \
        --private-key "$EVM_PRIVATE_KEY" \
        --json | jq -r '.transactionHash')

    echo ""
    echo "Transaction hash: $tx_hash"
    echo "Etherscan: https://sepolia.etherscan.io/tx/$tx_hash"
    echo ""

    # Wait for confirmation
    echo "Waiting for confirmation..."
    cast receipt "$tx_hash" --rpc-url "$SEPOLIA_RPC_URL" --json | jq '{blockNumber, status, gasUsed}'

    # Get the message ID from logs
    print_header "Message Details"
    local logs=$(cast receipt "$tx_hash" --rpc-url "$SEPOLIA_RPC_URL" --json | jq '.logs')
    echo "Transaction logs:"
    echo "$logs" | jq '.[0].topics'

    # The message ID is typically in the first topic after the event signature
    local message_id=$(echo "$logs" | jq -r '.[0].topics[1] // "unknown"')
    echo ""
    echo "Message ID: $message_id"
    echo ""
    echo "Monitor delivery on Cardano by checking for Process transactions"
    echo "at the mailbox address with this message ID."
}

# Check if message was delivered (on Sepolia side, for return messages)
check_delivery() {
    local message_id=$1
    if [ -z "$message_id" ]; then
        echo "Usage: $0 check <message_id>"
        exit 1
    fi

    local delivered=$(cast call "$SEPOLIA_MAILBOX" \
        "delivered(bytes32)(bool)" \
        "$message_id" \
        --rpc-url "$SEPOLIA_RPC_URL")

    echo "Message $message_id delivered: $delivered"
}

# Main
main() {
    local cmd=${1:-dispatch}

    case $cmd in
        dispatch)
            print_header "Sepolia → Cardano Dispatch Test"

            check_prerequisites

            echo "Sender: $(get_sender_address)"
            echo "Balance: $(get_sender_balance) wei"
            echo "Mailbox nonce: $(get_mailbox_nonce)"
            echo ""

            read -p "Proceed with dispatch? (y/n) " -n 1 -r
            echo
            if [[ $REPLY =~ ^[Yy]$ ]]; then
                dispatch_message
            else
                echo "Cancelled."
            fi
            ;;
        check)
            check_delivery "$2"
            ;;
        *)
            echo "Usage: $0 [dispatch|check <message_id>]"
            exit 1
            ;;
    esac
}

main "$@"

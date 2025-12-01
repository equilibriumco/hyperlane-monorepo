#!/bin/bash
# Test script for dispatching a message from Cardano to Sepolia
# This script builds and submits a dispatch transaction

set -e

# Configuration
NETWORK="${CARDANO_NETWORK:-preprod}"
BLOCKFROST_API_KEY="${BLOCKFROST_API_KEY_PREPROD:-}"
MAILBOX_POLICY_ID="${CARDANO_MAILBOX_POLICY_ID:-}"
SENDER_SKEY="${CARDANO_SKEY_PATH:-./payment.skey}"
SENDER_ADDR="${CARDANO_ADDRESS:-}"

# Destination
DEST_DOMAIN=11155111  # Sepolia
RECIPIENT="0x0000000000000000000000000000000000000000000000000000000000000001"
MESSAGE_BODY="Hello from Cardano!"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

print_header() {
    echo -e "\n${GREEN}=== $1 ===${NC}\n"
}

print_warning() {
    echo -e "${YELLOW}WARNING: $1${NC}"
}

print_error() {
    echo -e "${RED}ERROR: $1${NC}"
}

# Check prerequisites
check_prerequisites() {
    print_header "Checking Prerequisites"

    if [ -z "$BLOCKFROST_API_KEY" ]; then
        print_error "BLOCKFROST_API_KEY_PREPROD not set"
        exit 1
    fi

    if [ -z "$MAILBOX_POLICY_ID" ]; then
        print_error "CARDANO_MAILBOX_POLICY_ID not set"
        exit 1
    fi

    if [ -z "$SENDER_ADDR" ]; then
        print_error "CARDANO_ADDRESS not set"
        exit 1
    fi

    if [ ! -f "$SENDER_SKEY" ]; then
        print_error "Signing key not found at $SENDER_SKEY"
        exit 1
    fi

    command -v cardano-cli >/dev/null 2>&1 || { print_error "cardano-cli not found"; exit 1; }
    command -v jq >/dev/null 2>&1 || { print_error "jq not found"; exit 1; }

    echo "All prerequisites met!"
}

# Get network magic
get_network_magic() {
    case $NETWORK in
        mainnet) echo "" ;;
        preprod) echo "--testnet-magic 1" ;;
        preview) echo "--testnet-magic 2" ;;
        *) print_error "Unknown network: $NETWORK"; exit 1 ;;
    esac
}

# Query Blockfrost
blockfrost_query() {
    local endpoint=$1
    curl -s -H "project_id: $BLOCKFROST_API_KEY" \
        "https://cardano-${NETWORK}.blockfrost.io/api/v0${endpoint}"
}

# Get current mailbox state
get_mailbox_state() {
    print_header "Querying Mailbox State"

    local mailbox_addr=$(blockfrost_query "/scripts/${MAILBOX_POLICY_ID}/cbor" | jq -r '.address // empty')

    if [ -z "$mailbox_addr" ]; then
        print_warning "Could not get mailbox address from script hash"
        print_warning "Using placeholder - you'll need to provide the actual mailbox UTXO"
        return 1
    fi

    echo "Mailbox address: $mailbox_addr"

    local utxos=$(blockfrost_query "/addresses/${mailbox_addr}/utxos")
    echo "Current UTXOs at mailbox:"
    echo "$utxos" | jq '.[0]'
}

# Build dispatch redeemer
build_dispatch_redeemer() {
    local body_hex=$(echo -n "$MESSAGE_BODY" | xxd -p | tr -d '\n')

    cat <<EOF
{
  "constructor": 0,
  "fields": [
    {"int": $DEST_DOMAIN},
    {"bytes": "${RECIPIENT:2}"},
    {"bytes": "$body_hex"}
  ]
}
EOF
}

# Main execution
main() {
    print_header "Cardano → Sepolia Dispatch Test"

    echo "Configuration:"
    echo "  Network: $NETWORK"
    echo "  Destination Domain: $DEST_DOMAIN (Sepolia)"
    echo "  Recipient: $RECIPIENT"
    echo "  Message: $MESSAGE_BODY"
    echo ""

    check_prerequisites

    # Get mailbox state
    get_mailbox_state || true

    print_header "Dispatch Redeemer"
    local redeemer=$(build_dispatch_redeemer)
    echo "$redeemer" | jq .

    print_header "Next Steps"
    echo "To complete the dispatch, you need to:"
    echo ""
    echo "1. Find the mailbox UTXO (state NFT marker)"
    echo "2. Build a transaction that:"
    echo "   - Spends the mailbox UTXO with the Dispatch redeemer above"
    echo "   - Creates a new mailbox UTXO with:"
    echo "     - Incremented nonce"
    echo "     - Updated merkle tree (insert message id)"
    echo "   - Includes collateral"
    echo ""
    echo "3. Sign and submit the transaction"
    echo ""
    echo "Example cardano-cli command structure:"
    echo ""
    echo "  cardano-cli transaction build \\"
    echo "    --tx-in <mailbox_utxo> \\"
    echo "    --tx-in-script-file mailbox.plutus \\"
    echo "    --tx-in-inline-datum-present \\"
    echo "    --tx-in-redeemer-file dispatch-redeemer.json \\"
    echo "    --tx-out <mailbox_addr>+<value> \\"
    echo "    --tx-out-inline-datum-file new-mailbox-datum.json \\"
    echo "    --tx-in-collateral <collateral_utxo> \\"
    echo "    --change-address $SENDER_ADDR \\"
    echo "    $(get_network_magic) \\"
    echo "    --out-file tx.raw"
    echo ""

    # Save redeemer to file
    echo "$redeemer" > dispatch-redeemer.json
    echo "Redeemer saved to: dispatch-redeemer.json"
}

main "$@"

#!/bin/bash
# Register a recipient contract in the Hyperlane Recipient Registry
#
# This script helps register contracts that want to receive Hyperlane messages.

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

print_header() {
    echo -e "\n${GREEN}=== $1 ===${NC}\n"
}

print_info() {
    echo -e "${BLUE}INFO: $1${NC}"
}

print_warning() {
    echo -e "${YELLOW}WARNING: $1${NC}"
}

print_error() {
    echo -e "${RED}ERROR: $1${NC}"
}

# Default configuration
NETWORK="${CARDANO_NETWORK:-preprod}"
CONTRACTS_DIR="${CONTRACTS_DIR:-../../../hyperlane-cardano/contracts}"

usage() {
    cat << EOF
Usage: $0 <command> [options]

Commands:
    register      Register a new recipient in the registry
    unregister    Remove a recipient from the registry
    update        Update an existing registration
    list          List all registered recipients
    info          Get information about a specific recipient

Options:
    --network <network>           Cardano network (mainnet, preprod, preview) [default: preprod]
    --script-hash <hash>          Recipient script hash (28 bytes hex)
    --state-policy <policy>       State NFT policy ID
    --state-asset <name>          State NFT asset name (hex)
    --recipient-type <type>       Type: generic, token-receiver, contract-caller
    --vault-policy <policy>       Vault NFT policy (for token-receiver)
    --vault-asset <name>          Vault NFT asset name (for token-receiver)
    --minting-policy <hash>       Minting policy (for synthetic token-receiver)
    --custom-ism <hash>           Custom ISM script hash (optional)
    --additional-input <spec>     Additional input: "name:policy:asset:spend" (can repeat)
    --dry-run                     Print transaction without submitting

Examples:
    # Register a generic handler
    $0 register --script-hash abc123... --state-policy def456... --state-asset 7374617465 --recipient-type generic

    # Register a token receiver (collateral)
    $0 register --script-hash abc123... --state-policy def456... --state-asset 7374617465 \\
        --recipient-type token-receiver --vault-policy 789... --vault-asset 7661756c74

    # Register a token receiver (synthetic)
    $0 register --script-hash abc123... --state-policy def456... --state-asset 7374617465 \\
        --recipient-type token-receiver --minting-policy xyz...

    # List all registered recipients
    $0 list

    # Get info about a specific recipient
    $0 info --script-hash abc123...

EOF
    exit 1
}

# Check prerequisites
check_prerequisites() {
    print_header "Checking Prerequisites"

    if [ -z "$BLOCKFROST_API_KEY" ]; then
        print_error "BLOCKFROST_API_KEY not set"
        exit 1
    fi

    if [ -z "$CARDANO_SKEY_PATH" ]; then
        print_error "CARDANO_SKEY_PATH not set"
        exit 1
    fi

    if [ ! -f "$CARDANO_SKEY_PATH" ]; then
        print_error "Signing key not found at $CARDANO_SKEY_PATH"
        exit 1
    fi

    command -v cardano-cli >/dev/null 2>&1 || { print_error "cardano-cli not found"; exit 1; }
    command -v jq >/dev/null 2>&1 || { print_error "jq not found"; exit 1; }

    print_info "All prerequisites met"
}

# Get network magic for cardano-cli
get_network_magic() {
    case $NETWORK in
        mainnet) echo "" ;;
        preprod) echo "--testnet-magic 1" ;;
        preview) echo "--testnet-magic 2" ;;
        *) print_error "Unknown network: $NETWORK"; exit 1 ;;
    esac
}

# Query Blockfrost API
blockfrost_query() {
    local endpoint=$1
    curl -s -H "project_id: $BLOCKFROST_API_KEY" \
        "https://cardano-${NETWORK}.blockfrost.io/api/v0${endpoint}"
}

# Build recipient type datum
build_recipient_type() {
    local type=$1

    case $type in
        generic)
            echo '{"constructor": 0, "fields": []}'
            ;;
        token-receiver)
            local vault_field="[]"
            local minting_field="[]"

            if [ -n "$VAULT_POLICY" ] && [ -n "$VAULT_ASSET" ]; then
                vault_field=$(cat << EOF
[{
  "constructor": 0,
  "fields": [
    {"bytes": "$VAULT_POLICY"},
    {"bytes": "$VAULT_ASSET"}
  ]
}]
EOF
)
            fi

            if [ -n "$MINTING_POLICY" ]; then
                minting_field="[{\"bytes\": \"$MINTING_POLICY\"}]"
            fi

            cat << EOF
{
  "constructor": 1,
  "fields": [
    {"list": $vault_field},
    {"list": $minting_field}
  ]
}
EOF
            ;;
        contract-caller)
            if [ -z "$TARGET_POLICY" ] || [ -z "$TARGET_ASSET" ]; then
                print_error "contract-caller requires --target-policy and --target-asset"
                exit 1
            fi
            cat << EOF
{
  "constructor": 2,
  "fields": [
    {
      "constructor": 0,
      "fields": [
        {"bytes": "$TARGET_POLICY"},
        {"bytes": "$TARGET_ASSET"}
      ]
    }
  ]
}
EOF
            ;;
        *)
            print_error "Unknown recipient type: $type"
            exit 1
            ;;
    esac
}

# Build additional inputs list
build_additional_inputs() {
    if [ ${#ADDITIONAL_INPUTS[@]} -eq 0 ]; then
        echo "[]"
        return
    fi

    local inputs=""
    for input in "${ADDITIONAL_INPUTS[@]}"; do
        IFS=':' read -r name policy asset spend <<< "$input"
        local must_spend="false"
        if [ "$spend" = "true" ] || [ "$spend" = "1" ] || [ "$spend" = "spend" ]; then
            must_spend="true"
        fi

        if [ -n "$inputs" ]; then
            inputs="$inputs,"
        fi
        inputs="$inputs{\"constructor\": 0, \"fields\": [{\"bytes\": \"$(echo -n "$name" | xxd -p | tr -d '\n')\"}, {\"constructor\": 0, \"fields\": [{\"bytes\": \"$policy\"}, {\"bytes\": \"$asset\"}]}, {\"constructor\": $( [ "$must_spend" = "true" ] && echo "1" || echo "0" ), \"fields\": []}]}"
    done

    echo "[$inputs]"
}

# Build registration datum
build_registration() {
    local custom_ism_field="[]"
    if [ -n "$CUSTOM_ISM" ]; then
        custom_ism_field="[{\"bytes\": \"$CUSTOM_ISM\"}]"
    fi

    local recipient_type=$(build_recipient_type "$RECIPIENT_TYPE")
    local additional_inputs=$(build_additional_inputs)

    cat << EOF
{
  "constructor": 0,
  "fields": [
    {"bytes": "$SCRIPT_HASH"},
    {
      "constructor": 0,
      "fields": [
        {"bytes": "$STATE_POLICY"},
        {"bytes": "$STATE_ASSET"}
      ]
    },
    {"list": $additional_inputs},
    $recipient_type,
    {"list": $custom_ism_field}
  ]
}
EOF
}

# Register recipient
register_recipient() {
    print_header "Registering Recipient"

    if [ -z "$SCRIPT_HASH" ] || [ -z "$STATE_POLICY" ] || [ -z "$STATE_ASSET" ] || [ -z "$RECIPIENT_TYPE" ]; then
        print_error "Missing required options"
        print_info "Required: --script-hash, --state-policy, --state-asset, --recipient-type"
        exit 1
    fi

    print_info "Script Hash: $SCRIPT_HASH"
    print_info "State NFT: $STATE_POLICY.$STATE_ASSET"
    print_info "Recipient Type: $RECIPIENT_TYPE"
    print_info "Network: $NETWORK"

    # Build registration datum
    local registration=$(build_registration)

    print_header "Registration Datum"
    echo "$registration" | jq .

    # Build redeemer
    local redeemer=$(cat << EOF
{
  "constructor": 0,
  "fields": [$registration]
}
EOF
)

    print_header "Register Redeemer"
    echo "$redeemer" | jq .

    cat << EOF

To register the recipient:

1. Find the registry UTXO (contains the registry state NFT)
2. Build transaction with Register redeemer
3. Include the recipient script as an input (proves ownership)
4. Sign and submit

Example cardano-cli command structure:

cardano-cli transaction build \\
    --tx-in <registry_utxo> \\
    --tx-in-script-file registry.plutus \\
    --tx-in-inline-datum-present \\
    --tx-in-redeemer-file register-redeemer.json \\
    --tx-in <recipient_script_utxo> \\
    --tx-in-script-file <recipient>.plutus \\
    --tx-in-inline-datum-present \\
    --tx-in-redeemer-value '{"constructor": 0, "fields": []}' \\
    --tx-out <registry_addr>+<value> \\
    --tx-out-inline-datum-file updated-registry-datum.json \\
    --tx-out <recipient_addr>+<value> \\
    --tx-out-inline-datum-file <recipient-datum>.json \\
    $(get_network_magic) \\
    --out-file tx.raw

EOF

    if [ "$DRY_RUN" != "true" ]; then
        echo "$registration" > recipient-registration.json
        echo "$redeemer" > register-redeemer.json
        print_info "Registration saved to: recipient-registration.json"
        print_info "Redeemer saved to: register-redeemer.json"
    fi
}

# Unregister recipient
unregister_recipient() {
    print_header "Unregistering Recipient"

    if [ -z "$SCRIPT_HASH" ]; then
        print_error "Missing required option: --script-hash"
        exit 1
    fi

    print_info "Script Hash: $SCRIPT_HASH"

    # Build redeemer
    local redeemer=$(cat << EOF
{
  "constructor": 1,
  "fields": [{"bytes": "$SCRIPT_HASH"}]
}
EOF
)

    print_header "Unregister Redeemer"
    echo "$redeemer" | jq .

    cat << EOF

To unregister the recipient:

1. Find the registry UTXO
2. Build transaction with Unregister redeemer
3. Include the recipient script as an input (proves ownership)
4. Sign and submit

The recipient script hash will be removed from the registry.

EOF

    if [ "$DRY_RUN" != "true" ]; then
        echo "$redeemer" > unregister-redeemer.json
        print_info "Redeemer saved to: unregister-redeemer.json"
    fi
}

# List registered recipients
list_recipients() {
    print_header "Listing Registered Recipients"

    if [ -z "$REGISTRY_POLICY" ]; then
        print_warning "REGISTRY_POLICY not set, using placeholder"
        print_info "Set REGISTRY_POLICY environment variable to the registry state NFT policy ID"

        cat << EOF

To list recipients, query the registry UTXO:

1. Find the UTXO containing the registry state NFT
2. Decode the datum to get the list of registrations

Using Blockfrost:
  curl -H "project_id: \$BLOCKFROST_API_KEY" \\
    "https://cardano-${NETWORK}.blockfrost.io/api/v0/assets/<registry_policy><registry_asset>/utxos"

EOF
        return
    fi

    print_info "Querying registry..."

    local utxos=$(blockfrost_query "/assets/${REGISTRY_POLICY}${REGISTRY_ASSET:-}/utxos")
    echo "$utxos" | jq '.'
}

# Get recipient info
get_recipient_info() {
    print_header "Recipient Information"

    if [ -z "$SCRIPT_HASH" ]; then
        print_error "Missing required option: --script-hash"
        exit 1
    fi

    print_info "Script Hash: $SCRIPT_HASH"
    print_info "Network: $NETWORK"

    # Get script address
    local script_addr=$(blockfrost_query "/scripts/${SCRIPT_HASH}" | jq -r '.address // empty')

    if [ -z "$script_addr" ]; then
        print_warning "Could not get script address from Blockfrost"
        print_info "The script may not be deployed yet"
    else
        print_info "Script Address: $script_addr"

        # Get UTXOs
        local utxos=$(blockfrost_query "/addresses/${script_addr}/utxos")
        print_header "UTXOs at Script Address"
        echo "$utxos" | jq '.[0:5]'  # Show first 5
    fi
}

# Parse command line arguments
ADDITIONAL_INPUTS=()

parse_args() {
    while [[ $# -gt 0 ]]; do
        case $1 in
            --network)
                NETWORK="$2"
                shift 2
                ;;
            --script-hash)
                SCRIPT_HASH="$2"
                shift 2
                ;;
            --state-policy)
                STATE_POLICY="$2"
                shift 2
                ;;
            --state-asset)
                STATE_ASSET="$2"
                shift 2
                ;;
            --recipient-type)
                RECIPIENT_TYPE="$2"
                shift 2
                ;;
            --vault-policy)
                VAULT_POLICY="$2"
                shift 2
                ;;
            --vault-asset)
                VAULT_ASSET="$2"
                shift 2
                ;;
            --minting-policy)
                MINTING_POLICY="$2"
                shift 2
                ;;
            --target-policy)
                TARGET_POLICY="$2"
                shift 2
                ;;
            --target-asset)
                TARGET_ASSET="$2"
                shift 2
                ;;
            --custom-ism)
                CUSTOM_ISM="$2"
                shift 2
                ;;
            --additional-input)
                ADDITIONAL_INPUTS+=("$2")
                shift 2
                ;;
            --registry-policy)
                REGISTRY_POLICY="$2"
                shift 2
                ;;
            --registry-asset)
                REGISTRY_ASSET="$2"
                shift 2
                ;;
            --dry-run)
                DRY_RUN="true"
                shift
                ;;
            *)
                COMMAND="$1"
                shift
                ;;
        esac
    done
}

# Main
main() {
    parse_args "$@"

    if [ -z "$COMMAND" ]; then
        usage
    fi

    case $COMMAND in
        register)
            check_prerequisites
            register_recipient
            ;;
        unregister)
            check_prerequisites
            unregister_recipient
            ;;
        update)
            check_prerequisites
            print_info "Update uses the same flow as register"
            print_info "The registry will update the existing entry for the script hash"
            register_recipient
            ;;
        list)
            list_recipients
            ;;
        info)
            get_recipient_info
            ;;
        *)
            print_error "Unknown command: $COMMAND"
            usage
            ;;
    esac
}

main "$@"

#!/bin/bash
# Deploy a Warp Route for token bridging between Cardano and other chains
#
# This script helps deploy and configure warp routes on Cardano

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
    collateral    Deploy a collateral-backed warp route (lock/release native tokens)
    synthetic     Deploy a synthetic warp route (mint/burn synthetic tokens)
    native        Deploy a native ADA warp route
    enroll        Enroll a remote route on an existing warp route
    info          Display information about a warp route

Options:
    --network <network>       Cardano network (mainnet, preprod, preview) [default: preprod]
    --token-policy <policy>   Token policy ID (for collateral routes)
    --token-name <name>       Token asset name (for collateral routes)
    --decimals <n>            Token decimals [default: 6]
    --owner <pkh>             Owner verification key hash
    --mailbox <policy>        Mailbox policy ID
    --dry-run                 Print transaction without submitting

Examples:
    # Deploy a collateral-backed warp route for a native token
    $0 collateral --token-policy abc123... --token-name "MyToken" --decimals 6

    # Deploy a synthetic warp route
    $0 synthetic --decimals 18

    # Deploy native ADA warp route
    $0 native

    # Enroll a remote route (e.g., Sepolia)
    $0 enroll --warp-route <policy_id> --domain 11155111 --remote-route 0x...

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

# Get domain ID for network
get_domain_id() {
    case $NETWORK in
        mainnet) echo "2001" ;;
        preprod) echo "2002" ;;
        preview) echo "2003" ;;
        *) print_error "Unknown network: $NETWORK"; exit 1 ;;
    esac
}

# Query Blockfrost API
blockfrost_query() {
    local endpoint=$1
    curl -s -H "project_id: $BLOCKFROST_API_KEY" \
        "https://cardano-${NETWORK}.blockfrost.io/api/v0${endpoint}"
}

# Build warp route datum
build_warp_datum() {
    local token_type=$1
    local decimals=$2
    local owner=$3

    cat << EOF
{
  "constructor": 0,
  "fields": [
    $(build_warp_config "$token_type" "$decimals"),
    {"bytes": "$owner"},
    {"int": 0}
  ]
}
EOF
}

# Build warp route config based on token type
build_warp_config() {
    local token_type=$1
    local decimals=$2

    case $token_type in
        collateral)
            cat << EOF
{
  "constructor": 0,
  "fields": [
    {
      "constructor": 0,
      "fields": [
        {"bytes": "$TOKEN_POLICY"},
        {"bytes": "$TOKEN_NAME_HEX"},
        {
          "constructor": 0,
          "fields": [
            {"bytes": "$VAULT_POLICY"},
            {"bytes": "$VAULT_ASSET"}
          ]
        }
      ]
    },
    {"int": $decimals},
    {"list": []}
  ]
}
EOF
            ;;
        synthetic)
            cat << EOF
{
  "constructor": 0,
  "fields": [
    {
      "constructor": 1,
      "fields": [
        {"bytes": "$SYNTHETIC_POLICY"}
      ]
    },
    {"int": $decimals},
    {"list": []}
  ]
}
EOF
            ;;
        native)
            cat << EOF
{
  "constructor": 0,
  "fields": [
    {
      "constructor": 2,
      "fields": [
        {
          "constructor": 0,
          "fields": [
            {"bytes": "$VAULT_POLICY"},
            {"bytes": "$VAULT_ASSET"}
          ]
        }
      ]
    },
    {"int": $decimals},
    {"list": []}
  ]
}
EOF
            ;;
    esac
}

# Deploy collateral warp route
deploy_collateral() {
    print_header "Deploying Collateral Warp Route"

    if [ -z "$TOKEN_POLICY" ] || [ -z "$TOKEN_NAME" ]; then
        print_error "Token policy and name are required for collateral routes"
        print_info "Use: --token-policy <policy> --token-name <name>"
        exit 1
    fi

    TOKEN_NAME_HEX=$(echo -n "$TOKEN_NAME" | xxd -p | tr -d '\n')

    print_info "Token: $TOKEN_POLICY.$TOKEN_NAME"
    print_info "Decimals: $DECIMALS"
    print_info "Network: $NETWORK (domain $(get_domain_id))"

    # Build warp route datum
    local datum=$(build_warp_datum "collateral" "$DECIMALS" "$OWNER_PKH")

    print_header "Warp Route Datum"
    echo "$datum" | jq .

    print_header "Deployment Steps"
    cat << EOF
To deploy the collateral warp route:

1. First, deploy the vault (holds locked tokens):
   - Create state NFT for vault
   - Deploy vault UTXO with VaultDatum

2. Then, deploy the warp route:
   - Create state NFT for warp route
   - Deploy warp route UTXO with WarpRouteDatum
   - Register warp route in recipient registry

3. Enroll remote routes:
   - For each destination chain, call EnrollRemoteRoute
   - Provide the remote warp route address

Example cardano-cli command structure:

cardano-cli transaction build \\
    --tx-in <funding_utxo> \\
    --tx-out <warp_route_addr>+2000000 \\
    --tx-out-inline-datum-file warp-route-datum.json \\
    --mint "1 <state_nft_policy>.<state_nft_name>" \\
    --mint-script-file state-nft.plutus \\
    --mint-redeemer-value '{"constructor":0,"fields":[]}' \\
    --change-address <your_address> \\
    $(get_network_magic) \\
    --out-file tx.raw
EOF

    if [ "$DRY_RUN" != "true" ]; then
        echo "$datum" > warp-route-datum.json
        print_info "Datum saved to: warp-route-datum.json"
    fi
}

# Deploy synthetic warp route
deploy_synthetic() {
    print_header "Deploying Synthetic Warp Route"

    print_info "Decimals: $DECIMALS"
    print_info "Network: $NETWORK (domain $(get_domain_id))"

    print_header "Synthetic Token Deployment Steps"
    cat << EOF
To deploy a synthetic warp route:

1. Deploy the warp route validator first (need hash for minting policy)

2. Deploy the synthetic token minting policy:
   - Parameterize with warp route script hash
   - Only warp route can mint/burn

3. Deploy the warp route UTXO:
   - Include synthetic minting policy in datum
   - Register in recipient registry

4. Enroll remote routes

Note: Synthetic tokens are minted on receive and burned on send.
EOF

    local datum=$(build_warp_datum "synthetic" "$DECIMALS" "$OWNER_PKH")

    print_header "Warp Route Datum Template"
    echo "$datum" | jq .

    if [ "$DRY_RUN" != "true" ]; then
        echo "$datum" > warp-route-datum.json
        print_info "Datum saved to: warp-route-datum.json"
    fi
}

# Deploy native ADA warp route
deploy_native() {
    print_header "Deploying Native ADA Warp Route"

    print_info "Network: $NETWORK (domain $(get_domain_id))"

    print_header "Native ADA Deployment Steps"
    cat << EOF
To deploy a native ADA warp route:

1. Deploy the vault (holds locked ADA):
   - Create state NFT for vault
   - Deploy vault UTXO

2. Deploy the warp route:
   - Reference vault in datum
   - Register in recipient registry

3. Enroll remote routes

Users will lock ADA to send, and receive ADA on inbound transfers.
EOF

    local datum=$(build_warp_datum "native" "6" "$OWNER_PKH")

    print_header "Warp Route Datum Template"
    echo "$datum" | jq .

    if [ "$DRY_RUN" != "true" ]; then
        echo "$datum" > warp-route-datum.json
        print_info "Datum saved to: warp-route-datum.json"
    fi
}

# Enroll a remote route
enroll_remote_route() {
    print_header "Enrolling Remote Route"

    if [ -z "$WARP_ROUTE_POLICY" ] || [ -z "$REMOTE_DOMAIN" ] || [ -z "$REMOTE_ROUTE" ]; then
        print_error "Missing required options"
        print_info "Use: --warp-route <policy> --domain <domain_id> --remote-route <address>"
        exit 1
    fi

    print_info "Warp Route: $WARP_ROUTE_POLICY"
    print_info "Remote Domain: $REMOTE_DOMAIN"
    print_info "Remote Route: $REMOTE_ROUTE"

    # Build EnrollRemoteRoute redeemer
    local redeemer=$(cat << EOF
{
  "constructor": 2,
  "fields": [
    {"int": $REMOTE_DOMAIN},
    {"bytes": "${REMOTE_ROUTE:2}"}
  ]
}
EOF
)

    print_header "Enroll Remote Route Redeemer"
    echo "$redeemer" | jq .

    cat << EOF

To enroll the remote route:

1. Find the warp route UTXO
2. Build transaction with EnrollRemoteRoute redeemer
3. Sign with owner key
4. Submit

Example:

cardano-cli transaction build \\
    --tx-in <warp_route_utxo> \\
    --tx-in-script-file warp_route.plutus \\
    --tx-in-inline-datum-present \\
    --tx-in-redeemer-file enroll-redeemer.json \\
    --tx-out <warp_route_addr>+<value> \\
    --tx-out-inline-datum-file updated-datum.json \\
    --required-signer-hash $OWNER_PKH \\
    $(get_network_magic) \\
    --out-file tx.raw
EOF

    if [ "$DRY_RUN" != "true" ]; then
        echo "$redeemer" > enroll-redeemer.json
        print_info "Redeemer saved to: enroll-redeemer.json"
    fi
}

# Display warp route info
show_info() {
    print_header "Warp Route Information"

    if [ -z "$WARP_ROUTE_POLICY" ]; then
        print_error "Warp route policy ID required"
        print_info "Use: --warp-route <policy>"
        exit 1
    fi

    print_info "Querying warp route..."

    # Get script address
    local warp_addr=$(blockfrost_query "/scripts/${WARP_ROUTE_POLICY}" | jq -r '.address // empty')

    if [ -z "$warp_addr" ]; then
        print_warning "Could not get warp route address from script"
        print_info "The warp route may not be deployed yet"
        exit 0
    fi

    print_info "Warp Route Address: $warp_addr"

    # Get UTXOs
    local utxos=$(blockfrost_query "/addresses/${warp_addr}/utxos")
    echo "UTXOs at warp route:"
    echo "$utxos" | jq '.[0]'
}

# Parse command line arguments
parse_args() {
    while [[ $# -gt 0 ]]; do
        case $1 in
            --network)
                NETWORK="$2"
                shift 2
                ;;
            --token-policy)
                TOKEN_POLICY="$2"
                shift 2
                ;;
            --token-name)
                TOKEN_NAME="$2"
                shift 2
                ;;
            --decimals)
                DECIMALS="$2"
                shift 2
                ;;
            --owner)
                OWNER_PKH="$2"
                shift 2
                ;;
            --mailbox)
                MAILBOX_POLICY="$2"
                shift 2
                ;;
            --warp-route)
                WARP_ROUTE_POLICY="$2"
                shift 2
                ;;
            --domain)
                REMOTE_DOMAIN="$2"
                shift 2
                ;;
            --remote-route)
                REMOTE_ROUTE="$2"
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
            --synthetic-policy)
                SYNTHETIC_POLICY="$2"
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

    # Defaults
    DECIMALS="${DECIMALS:-6}"
    OWNER_PKH="${OWNER_PKH:-0000000000000000000000000000000000000000000000000000000000}"
    VAULT_POLICY="${VAULT_POLICY:-0000000000000000000000000000000000000000000000000000000000}"
    VAULT_ASSET="${VAULT_ASSET:-}"
}

# Main
main() {
    parse_args "$@"

    if [ -z "$COMMAND" ]; then
        usage
    fi

    check_prerequisites

    case $COMMAND in
        collateral)
            deploy_collateral
            ;;
        synthetic)
            deploy_synthetic
            ;;
        native)
            deploy_native
            ;;
        enroll)
            enroll_remote_route
            ;;
        info)
            show_info
            ;;
        *)
            print_error "Unknown command: $COMMAND"
            usage
            ;;
    esac
}

main "$@"

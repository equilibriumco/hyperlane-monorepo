#!/bin/bash
# Deploy Hyperlane core contracts to Cardano Preview testnet

set -e

# Configuration
NETWORK_MAGIC=2  # Preview testnet
BLOCKFROST_API_KEY="${BLOCKFROST_API_KEY:-previewjtDS4mHuhJroFIX0BfOpVmMdnyTrWMfh}"
BLOCKFROST_URL="https://cardano-preview.blockfrost.io/api/v0"

# Paths
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KEYS_DIR="$SCRIPT_DIR/../testnet-keys"
CONTRACTS_DIR="$SCRIPT_DIR/../contracts"
DEPLOY_DIR="$SCRIPT_DIR/../deployments/preview"

# Create deployment directory
mkdir -p "$DEPLOY_DIR"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info() { echo -e "${BLUE}[INFO]${NC} $1"; }
log_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }

# Check prerequisites
check_prerequisites() {
    log_info "Checking prerequisites..."

    if ! command -v cardano-cli &> /dev/null; then
        log_error "cardano-cli not found"
        exit 1
    fi

    if ! command -v jq &> /dev/null; then
        log_error "jq not found"
        exit 1
    fi

    if [ ! -f "$KEYS_DIR/payment.skey" ]; then
        log_error "Signing key not found at $KEYS_DIR/payment.skey"
        exit 1
    fi

    if [ ! -f "$CONTRACTS_DIR/plutus.json" ]; then
        log_error "plutus.json not found. Run 'aiken build' first."
        exit 1
    fi

    log_success "Prerequisites OK"
}

# Get wallet address
get_wallet_address() {
    cat "$KEYS_DIR/payment.addr"
}

# Query UTxOs at address
query_utxos() {
    local addr=$1
    curl -s -H "project_id: $BLOCKFROST_API_KEY" \
        "$BLOCKFROST_URL/addresses/$addr/utxos" | jq -r '.[] | "\(.tx_hash)#\(.tx_index) \(.amount[0].quantity)"' 2>/dev/null || echo ""
}

# Get protocol parameters
get_protocol_params() {
    log_info "Fetching protocol parameters..."
    curl -s -H "project_id: $BLOCKFROST_API_KEY" \
        "$BLOCKFROST_URL/epochs/latest/parameters" > "$DEPLOY_DIR/protocol-params.json"

    # Convert to cardano-cli format (simplified)
    cat > "$DEPLOY_DIR/protocol.json" << 'PARAMS'
{
    "txFeePerByte": 44,
    "txFeeFixed": 155381,
    "maxTxSize": 16384,
    "protocolVersion": {"major": 8, "minor": 0},
    "minUTxOValue": 1000000,
    "costModels": {},
    "executionUnitPrices": {"priceMemory": 0.0577, "priceSteps": 0.0000721},
    "maxTxExecutionUnits": {"memory": 14000000, "steps": 10000000000},
    "maxBlockExecutionUnits": {"memory": 62000000, "steps": 40000000000},
    "maxValueSize": 5000,
    "collateralPercentage": 150,
    "maxCollateralInputs": 3,
    "coinsPerUTxOByte": 4310,
    "utxoCostPerWord": 4310
}
PARAMS
    log_success "Protocol parameters saved"
}

# Extract validator from plutus.json
extract_validator() {
    local validator_name=$1
    local output_file=$2

    log_info "Extracting $validator_name..."

    # Find the spend validator
    local compiled_code=$(cat "$CONTRACTS_DIR/plutus.json" | jq -r ".validators[] | select(.title == \"${validator_name}.spend\") | .compiledCode")

    if [ -z "$compiled_code" ] || [ "$compiled_code" == "null" ]; then
        log_error "Validator $validator_name not found in plutus.json"
        return 1
    fi

    # Create the script file in cardano-cli format
    cat > "$output_file" << EOF
{
    "type": "PlutusScriptV3",
    "description": "$validator_name",
    "cborHex": "$compiled_code"
}
EOF

    log_success "Extracted $validator_name to $output_file"
}

# Get script hash
get_script_hash() {
    local script_file=$1
    cardano-cli hash script --script-file "$script_file"
}

# Get script address
get_script_address() {
    local script_file=$1
    cardano-cli latest address build \
        --payment-script-file "$script_file" \
        --testnet-magic $NETWORK_MAGIC
}

# Deploy MultisigISM (no parameters needed for basic version)
deploy_multisig_ism() {
    log_info "=== Deploying MultisigISM ==="

    extract_validator "multisig_ism.multisig_ism" "$DEPLOY_DIR/multisig_ism.plutus"

    local script_hash=$(get_script_hash "$DEPLOY_DIR/multisig_ism.plutus")
    local script_addr=$(get_script_address "$DEPLOY_DIR/multisig_ism.plutus")

    echo "$script_hash" > "$DEPLOY_DIR/multisig_ism.hash"
    echo "$script_addr" > "$DEPLOY_DIR/multisig_ism.addr"

    log_success "MultisigISM script hash: $script_hash"
    log_success "MultisigISM address: $script_addr"

    echo "$script_hash"
}

# Deploy Mailbox
deploy_mailbox() {
    log_info "=== Deploying Mailbox ==="

    extract_validator "mailbox.mailbox" "$DEPLOY_DIR/mailbox.plutus"

    local script_hash=$(get_script_hash "$DEPLOY_DIR/mailbox.plutus")
    local script_addr=$(get_script_address "$DEPLOY_DIR/mailbox.plutus")

    echo "$script_hash" > "$DEPLOY_DIR/mailbox.hash"
    echo "$script_addr" > "$DEPLOY_DIR/mailbox.addr"

    log_success "Mailbox script hash: $script_hash"
    log_success "Mailbox address: $script_addr"

    echo "$script_hash"
}

# Deploy Registry
deploy_registry() {
    log_info "=== Deploying Registry ==="

    extract_validator "registry.registry" "$DEPLOY_DIR/registry.plutus"

    local script_hash=$(get_script_hash "$DEPLOY_DIR/registry.plutus")
    local script_addr=$(get_script_address "$DEPLOY_DIR/registry.plutus")

    echo "$script_hash" > "$DEPLOY_DIR/registry.hash"
    echo "$script_addr" > "$DEPLOY_DIR/registry.addr"

    log_success "Registry script hash: $script_hash"
    log_success "Registry address: $script_addr"

    echo "$script_hash"
}

# Create initial datum for Mailbox
create_mailbox_datum() {
    local ism_hash=$1
    local owner_pkh=$2

    # MailboxDatum structure:
    # - local_domain: Int (2003 for Preview)
    # - default_ism: ScriptHash
    # - owner: VerificationKeyHash
    # - outbound_nonce: Int
    # - merkle_root: ByteArray (32 bytes of zeros)
    # - merkle_count: Int

    cat > "$DEPLOY_DIR/mailbox_datum.json" << EOF
{
    "constructor": 0,
    "fields": [
        {"int": 2003},
        {"bytes": "$ism_hash"},
        {"bytes": "$owner_pkh"},
        {"int": 0},
        {"bytes": "0000000000000000000000000000000000000000000000000000000000000000"},
        {"int": 0}
    ]
}
EOF
    log_success "Created mailbox datum"
}

# Create initial datum for MultisigISM
create_ism_datum() {
    local owner_pkh=$1

    # MultisigIsmDatum structure:
    # - validators: List<(Domain, List<VerificationKeyHash>)>
    # - thresholds: List<(Domain, Int)>
    # - owner: VerificationKeyHash

    # For testing, we'll set up a simple config:
    # - Fuji (43113) with 1 validator, threshold 1

    cat > "$DEPLOY_DIR/ism_datum.json" << EOF
{
    "constructor": 0,
    "fields": [
        {"list": [
            {"constructor": 0, "fields": [
                {"int": 43113},
                {"list": [{"bytes": "$owner_pkh"}]}
            ]}
        ]},
        {"list": [
            {"constructor": 0, "fields": [
                {"int": 43113},
                {"int": 1}
            ]}
        ]},
        {"bytes": "$owner_pkh"}
    ]
}
EOF
    log_success "Created ISM datum"
}

# Get owner public key hash
get_owner_pkh() {
    cardano-cli address key-hash --payment-verification-key-file "$KEYS_DIR/payment.vkey"
}

# Main deployment
main() {
    log_info "Starting Hyperlane deployment to Preview testnet..."

    check_prerequisites
    get_protocol_params

    local wallet_addr=$(get_wallet_address)
    log_info "Wallet address: $wallet_addr"

    local owner_pkh=$(get_owner_pkh)
    log_info "Owner PKH: $owner_pkh"

    # Check wallet balance
    log_info "Checking wallet UTxOs..."
    local utxos=$(query_utxos "$wallet_addr")
    if [ -z "$utxos" ]; then
        log_error "No UTxOs found at wallet address"
        exit 1
    fi
    echo "$utxos"

    # Extract and get script hashes
    local ism_hash=$(deploy_multisig_ism)
    local mailbox_hash=$(deploy_mailbox)
    local registry_hash=$(deploy_registry)

    # Create datums
    create_ism_datum "$owner_pkh"
    create_mailbox_datum "$ism_hash" "$owner_pkh"

    log_info ""
    log_info "=== Deployment Summary ==="
    log_info "Network: Preview (magic=$NETWORK_MAGIC)"
    log_info "Owner PKH: $owner_pkh"
    log_info ""
    log_info "Script Hashes:"
    log_info "  MultisigISM: $ism_hash"
    log_info "  Mailbox:     $mailbox_hash"
    log_info "  Registry:    $registry_hash"
    log_info ""
    log_info "Script Addresses:"
    log_info "  MultisigISM: $(cat $DEPLOY_DIR/multisig_ism.addr)"
    log_info "  Mailbox:     $(cat $DEPLOY_DIR/mailbox.addr)"
    log_info "  Registry:    $(cat $DEPLOY_DIR/registry.addr)"
    log_info ""
    log_success "Scripts extracted. Now need to initialize with UTxOs."
    log_info ""
    log_info "Next steps:"
    log_info "1. Send initial UTxO to ISM address with ism_datum.json"
    log_info "2. Send initial UTxO to Mailbox address with mailbox_datum.json"
    log_info ""
}

main "$@"

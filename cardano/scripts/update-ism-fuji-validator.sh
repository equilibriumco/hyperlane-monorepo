#!/bin/bash
# Update ISM with Hyperlane's first Fuji validator (for testing)
# This configures the ISM with 1 validator and threshold of 1

set -e

# Configuration
NETWORK_MAGIC=2  # Preview testnet
BLOCKFROST_API_KEY="${BLOCKFROST_API_KEY:-previewjtDS4mHuhJroFIX0BfOpVmMdnyTrWMfh}"
BLOCKFROST_URL="https://cardano-preview.blockfrost.io/api/v0"

# Paths
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KEYS_DIR="$SCRIPT_DIR/../testnet-keys"
DEPLOY_DIR="$SCRIPT_DIR/../deployments/preview"

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

# Hyperlane's first Fuji validator (from defaultMultisigConfigs)
FUJI_DOMAIN=43113
FUJI_VALIDATOR="d8154f73d04cc7f7f0c332793692e6e6f6b2402e"
FUJI_THRESHOLD=1

log_info "=== Updating ISM with Fuji Validator ==="
log_info "Domain: $FUJI_DOMAIN (Fuji)"
log_info "Validator: 0x$FUJI_VALIDATOR"
log_info "Threshold: $FUJI_THRESHOLD"
log_info ""

# Get wallet and ISM addresses
WALLET_ADDR=$(cat "$KEYS_DIR/payment.addr")
ISM_ADDR=$(cat "$DEPLOY_DIR/multisig_ism.addr")

log_info "Wallet: $WALLET_ADDR"
log_info "ISM Address: $ISM_ADDR"

# Query UTxOs
query_utxos_json() {
    local addr=$1
    curl -s -H "project_id: $BLOCKFROST_API_KEY" \
        "$BLOCKFROST_URL/addresses/$addr/utxos"
}

# Submit transaction via Blockfrost
submit_tx() {
    local tx_file=$1
    local cbor_hex=$(cat "$tx_file" | jq -r '.cborHex')
    local tx_binary="$DEPLOY_DIR/tx.cbor"
    echo "$cbor_hex" | xxd -r -p > "$tx_binary"

    curl -s -X POST \
        -H "project_id: $BLOCKFROST_API_KEY" \
        -H "Content-Type: application/cbor" \
        --data-binary @"$tx_binary" \
        "$BLOCKFROST_URL/tx/submit"
}

# Step 1: Create updated ISM datum
log_info "Creating updated ISM datum..."

# Read current datum to preserve owner
CURRENT_DATUM=$(cat "$DEPLOY_DIR/ism_datum.json")
OWNER_BYTES=$(echo "$CURRENT_DATUM" | jq -r '.fields[2].bytes')

log_info "Preserving owner: $OWNER_BYTES"

# Create new datum with Hyperlane's validator
cat > "$DEPLOY_DIR/ism_datum_updated.json" << EOF
{
    "constructor": 0,
    "fields": [
        {"list": [
            {"constructor": 0, "fields": [
                {"int": $FUJI_DOMAIN},
                {"list": [
                    {"bytes": "$FUJI_VALIDATOR"}
                ]}
            ]}
        ]},
        {"list": [
            {"constructor": 0, "fields": [
                {"int": $FUJI_DOMAIN},
                {"int": $FUJI_THRESHOLD}
            ]}
        ]},
        {"bytes": "$OWNER_BYTES"}
    ]
}
EOF

log_success "Updated datum created: $DEPLOY_DIR/ism_datum_updated.json"
log_info "Contents:"
cat "$DEPLOY_DIR/ism_datum_updated.json"

# Step 2: Create SetValidators redeemer
log_info ""
log_info "Creating SetValidators redeemer..."

cat > "$DEPLOY_DIR/ism_set_validators_redeemer.json" << EOF
{
    "constructor": 1,
    "fields": [
        {"int": $FUJI_DOMAIN},
        {"list": [
            {"bytes": "$FUJI_VALIDATOR"}
        ]}
    ]
}
EOF

log_success "Redeemer created: $DEPLOY_DIR/ism_set_validators_redeemer.json"

# Step 3: Find ISM UTxO
log_info "Querying ISM UTxOs..."
ISM_UTXOS=$(query_utxos_json "$ISM_ADDR")
ISM_UTXO_COUNT=$(echo "$ISM_UTXOS" | jq 'length')

if [ "$ISM_UTXO_COUNT" -lt 1 ]; then
    log_error "No ISM UTxO found. Please initialize the ISM first."
    exit 1
fi

ISM_TX_HASH=$(echo "$ISM_UTXOS" | jq -r '.[0].tx_hash')
ISM_TX_INDEX=$(echo "$ISM_UTXOS" | jq -r '.[0].tx_index')
ISM_VALUE=$(echo "$ISM_UTXOS" | jq -r '.[0].amount[0].quantity')

log_info "ISM UTxO: ${ISM_TX_HASH}#${ISM_TX_INDEX}"
log_info "ISM Value: $ISM_VALUE lovelace"

# Step 4: Get wallet UTxO for fees and collateral
log_info "Querying wallet UTxOs..."
WALLET_UTXOS=$(query_utxos_json "$WALLET_ADDR")
WALLET_UTXO_COUNT=$(echo "$WALLET_UTXOS" | jq 'length')

if [ "$WALLET_UTXO_COUNT" -lt 2 ]; then
    log_error "Need at least 2 wallet UTxOs (one for fees, one for collateral)"
    log_error "Run: cardano/scripts/split-utxo.sh"
    exit 1
fi

WALLET_TX_HASH=$(echo "$WALLET_UTXOS" | jq -r '.[0].tx_hash')
WALLET_TX_INDEX=$(echo "$WALLET_UTXOS" | jq -r '.[0].tx_index')
WALLET_VALUE=$(echo "$WALLET_UTXOS" | jq -r '.[0].amount[0].quantity')

COLLATERAL_TX_HASH=$(echo "$WALLET_UTXOS" | jq -r '.[1].tx_hash')
COLLATERAL_TX_INDEX=$(echo "$WALLET_UTXOS" | jq -r '.[1].tx_index')

log_info "Wallet UTxO: ${WALLET_TX_HASH}#${WALLET_TX_INDEX} ($WALLET_VALUE lovelace)"
log_info "Collateral: ${COLLATERAL_TX_HASH}#${COLLATERAL_TX_INDEX}"

# Step 5: Build transaction
log_info ""
log_info "Building transaction to update validators..."

# Get current slot and protocol params
CURRENT_SLOT=$(curl -s -H "project_id: $BLOCKFROST_API_KEY" "$BLOCKFROST_URL/blocks/latest" | jq -r '.slot')
VALIDITY_END=$((CURRENT_SLOT + 7200))

log_info "Current slot: $CURRENT_SLOT, validity end: $VALIDITY_END"

# Get the ISM script file
ISM_SCRIPT="$DEPLOY_DIR/multisig_ism.plutus"
if [ ! -f "$ISM_SCRIPT" ]; then
    log_error "ISM script not found: $ISM_SCRIPT"
    log_error "Please deploy the ISM first"
    exit 1
fi

FEE=2000000  # Estimated fee for script execution
CHANGE=$((WALLET_VALUE - FEE))

if [ $CHANGE -lt 1000000 ]; then
    log_error "Not enough funds for transaction fee"
    exit 1
fi

# Build transaction without protocol params to avoid hash mismatch
cardano-cli conway transaction build-raw \
    --tx-in "${WALLET_TX_HASH}#${WALLET_TX_INDEX}" \
    --tx-in "${ISM_TX_HASH}#${ISM_TX_INDEX}" \
    --tx-in-script-file "$ISM_SCRIPT" \
    --tx-in-inline-datum-present \
    --tx-in-redeemer-file "$DEPLOY_DIR/ism_set_validators_redeemer.json" \
    --tx-in-execution-units "(1000000000,5000000)" \
    --required-signer "$KEYS_DIR/payment.skey" \
    --tx-in-collateral "${COLLATERAL_TX_HASH}#${COLLATERAL_TX_INDEX}" \
    --tx-out "$ISM_ADDR+$ISM_VALUE" \
    --tx-out-inline-datum-file "$DEPLOY_DIR/ism_datum_updated.json" \
    --tx-out "$WALLET_ADDR+$CHANGE" \
    --fee $FEE \
    --invalid-hereafter $VALIDITY_END \
    --out-file "$DEPLOY_DIR/update_ism_tx.raw"

if [ $? -ne 0 ]; then
    log_error "Transaction build failed"
    exit 1
fi

log_success "Transaction built: $DEPLOY_DIR/update_ism_tx.raw"

# Step 6: Sign transaction
log_info "Signing transaction..."

cardano-cli conway transaction sign \
    --testnet-magic $NETWORK_MAGIC \
    --tx-body-file "$DEPLOY_DIR/update_ism_tx.raw" \
    --signing-key-file "$KEYS_DIR/payment.skey" \
    --out-file "$DEPLOY_DIR/update_ism_tx.signed"

log_success "Transaction signed: $DEPLOY_DIR/update_ism_tx.signed"

# Step 7: Get transaction ID
TX_ID=$(cardano-cli conway transaction txid --tx-file "$DEPLOY_DIR/update_ism_tx.signed")
log_info "Transaction ID: $TX_ID"

# Step 8: Submit transaction
log_info "Submitting transaction..."

SUBMIT_RESULT=$(submit_tx "$DEPLOY_DIR/update_ism_tx.signed")

if [[ "$SUBMIT_RESULT" == *"error"* ]] || [[ "$SUBMIT_RESULT" == *"Error"* ]]; then
    log_error "Transaction submission failed"
    echo "$SUBMIT_RESULT" | jq '.'
    exit 1
fi

log_success "Transaction submitted!"
log_info ""
log_info "=== ISM Update Complete ==="
log_info "Transaction ID: $TX_ID"
log_info ""
log_info "View on explorer:"
log_info "  https://preview.cardanoscan.io/transaction/$TX_ID"
log_info ""
log_info "Updated ISM UTxO: ${TX_ID}#0"
log_info ""
log_success "ISM now configured with Hyperlane's official Fuji validator!"
log_info ""
log_info "Next steps:"
log_info "1. Wait for transaction to confirm (~20 seconds)"
log_info "2. Restart your relayer to pick up the new configuration"
log_info "3. Try sending a test message from Fuji to Cardano"
log_info ""
log_info "The relayer should now be able to:"
log_info "  - Query ISM and get validator 0x$FUJI_VALIDATOR"
log_info "  - Fetch checkpoint signatures from Hyperlane's infrastructure"
log_info "  - Deliver messages from Fuji to Cardano"

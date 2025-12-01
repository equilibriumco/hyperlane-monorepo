#!/bin/bash
# Initialize Hyperlane contracts with initial UTxOs and state NFTs
# This script uses build-raw to work without a local node

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

# Get wallet address
WALLET_ADDR=$(cat "$KEYS_DIR/payment.addr")

# Script addresses
ISM_ADDR=$(cat "$DEPLOY_DIR/multisig_ism.addr")
MAILBOX_ADDR=$(cat "$DEPLOY_DIR/mailbox.addr")

# Query UTxOs
query_utxos_json() {
    local addr=$1
    curl -s -H "project_id: $BLOCKFROST_API_KEY" \
        "$BLOCKFROST_URL/addresses/$addr/utxos"
}

# Submit transaction via Blockfrost
submit_tx() {
    local tx_file=$1

    # Extract CBOR hex from the signed tx envelope and convert to binary
    local cbor_hex=$(cat "$tx_file" | jq -r '.cborHex')
    local tx_binary="$DEPLOY_DIR/tx.cbor"
    echo "$cbor_hex" | xxd -r -p > "$tx_binary"

    curl -s -X POST \
        -H "project_id: $BLOCKFROST_API_KEY" \
        -H "Content-Type: application/cbor" \
        --data-binary @"$tx_binary" \
        "$BLOCKFROST_URL/tx/submit"
}

# Encode OutputReference as CBOR hex
# OutputReference = Constr 0 [ByteArray tx_hash, Int output_index]
encode_output_reference() {
    local tx_hash=$1
    local output_index=$2

    # CBOR encoding:
    # d8799f = constructor 0 (tag 121) + indefinite array start
    # 5820 = 32-byte bytestring prefix
    # tx_hash = 32 bytes (64 hex chars)
    # 00-17 = small int (0-23)
    # 18 XX = int 24-255
    # ff = break (end indefinite array)

    local cbor="d8799f5820${tx_hash}"

    if [ "$output_index" -le 23 ]; then
        # Small integer (0-23) encodes directly
        cbor="${cbor}$(printf '%02x' $output_index)"
    else
        # Larger integers need prefix 18
        cbor="${cbor}18$(printf '%02x' $output_index)"
    fi

    cbor="${cbor}ff"
    echo "$cbor"
}

# Apply parameter to state_nft and get policy script
create_state_nft_policy() {
    local tx_hash=$1
    local output_index=$2
    local output_file=$3

    local cbor=$(encode_output_reference "$tx_hash" "$output_index")
    # Log to stderr so it doesn't interfere with return value
    echo -e "${BLUE}[INFO]${NC} OutputReference CBOR: $cbor" >&2

    # Apply parameter to state_nft minting policy
    cd "$CONTRACTS_DIR"
    aiken blueprint apply "$cbor" \
        --module state_nft \
        --validator state_nft \
        --out "$output_file" >&2

    # Extract the compiled code
    local compiled_code=$(cat "$output_file" | jq -r '.validators[] | select(.title == "state_nft.state_nft.mint") | .compiledCode')

    # Create plutus script file (same format as other working scripts)
    cat > "${output_file%.json}.plutus" << EOF
{
    "type": "PlutusScriptV3",
    "description": "state_nft for ${tx_hash}#${output_index}",
    "cborHex": "$compiled_code"
}
EOF

    # Get the policy ID using cardano-cli (must match the script file)
    local policy_id=$(cardano-cli hash script --script-file "${output_file%.json}.plutus")

    # Return only the policy ID (no colors, no extra output)
    echo "$policy_id"
}

log_info "=== Initializing Hyperlane Contracts with State NFTs ==="
log_info "Wallet: $WALLET_ADDR"
log_info "ISM Address: $ISM_ADDR"
log_info "Mailbox Address: $MAILBOX_ADDR"

# Get UTxOs for inputs
UTXOS_JSON=$(query_utxos_json "$WALLET_ADDR")
UTXO_COUNT=$(echo "$UTXOS_JSON" | jq 'length')

log_info "Found $UTXO_COUNT UTxOs"

if [ "$UTXO_COUNT" -lt 1 ]; then
    log_error "Not enough UTxOs. Need at least 1."
    exit 1
fi

# Get first UTxO details
TX_HASH=$(echo "$UTXOS_JSON" | jq -r '.[0].tx_hash')
TX_INDEX=$(echo "$UTXOS_JSON" | jq -r '.[0].tx_index')
INPUT_VALUE=$(echo "$UTXOS_JSON" | jq -r '.[0].amount[0].quantity')

log_info "Using UTxO: ${TX_HASH}#${TX_INDEX}"
log_info "Input value: $INPUT_VALUE lovelace"

# Create state NFT policy for mailbox (using the input UTxO)
log_info "Creating mailbox state NFT policy..."
MAILBOX_NFT_POLICY=$(create_state_nft_policy "$TX_HASH" "$TX_INDEX" "$DEPLOY_DIR/mailbox_state_nft.json")
log_success "Mailbox NFT Policy ID: $MAILBOX_NFT_POLICY"

# Minimum ADA for script outputs with NFT and datum
MIN_ADA=5000000

# Fee estimate for minting tx with plutus script
# Higher because of script execution costs
FEE=1500000

# Calculate change
CHANGE=$((INPUT_VALUE - MIN_ADA - FEE))
log_info "Change: $CHANGE lovelace"

if [ $CHANGE -lt 1000000 ]; then
    log_error "Not enough funds for transaction"
    exit 1
fi

# Build transaction with NFT minting using build-raw
log_info "Building initialization transaction with state NFT..."

# Asset format: policy_id.asset_name (empty asset name = just policy_id)
MAILBOX_ASSET="${MAILBOX_NFT_POLICY}"

# Get current slot for validity
CURRENT_SLOT=$(curl -s -H "project_id: $BLOCKFROST_API_KEY" "$BLOCKFROST_URL/blocks/latest" | jq -r '.slot')
VALIDITY_END=$((CURRENT_SLOT + 7200))  # Valid for ~2 hours

log_info "Current slot: $CURRENT_SLOT, validity end: $VALIDITY_END"

# Create empty redeemer file for mint
cat > "$DEPLOY_DIR/mint_redeemer.json" << 'EOF'
{"constructor":0,"fields":[]}
EOF

# We need a second UTXO for collateral, or use the same one
# Check if we have a second UTXO
UTXO2_HASH=$(echo "$UTXOS_JSON" | jq -r '.[1].tx_hash // empty')
if [ -z "$UTXO2_HASH" ]; then
    log_error "Need a second UTXO for collateral. Please split your UTXO first:"
    log_error "  cardano-cli conway transaction build --tx-in ${TX_HASH}#${TX_INDEX} --tx-out $WALLET_ADDR+50000000 --change-address $WALLET_ADDR --out-file split.raw"
    exit 1
fi
UTXO2_INDEX=$(echo "$UTXOS_JSON" | jq -r '.[1].tx_index')
log_info "Using collateral: ${UTXO2_HASH}#${UTXO2_INDEX}"

# Build the transaction with collateral
cardano-cli conway transaction build-raw \
    --tx-in "${TX_HASH}#${TX_INDEX}" \
    --tx-in-collateral "${UTXO2_HASH}#${UTXO2_INDEX}" \
    --tx-out "$MAILBOX_ADDR+$MIN_ADA+1 ${MAILBOX_ASSET}" \
    --tx-out-inline-datum-file "$DEPLOY_DIR/mailbox_datum.json" \
    --tx-out "$WALLET_ADDR+$CHANGE" \
    --mint "1 ${MAILBOX_ASSET}" \
    --mint-script-file "$DEPLOY_DIR/mailbox_state_nft.plutus" \
    --mint-redeemer-file "$DEPLOY_DIR/mint_redeemer.json" \
    --mint-execution-units "(500000000,2000000)" \
    --fee $FEE \
    --invalid-hereafter $VALIDITY_END \
    --out-file "$DEPLOY_DIR/init_tx.raw"

log_success "Transaction built: $DEPLOY_DIR/init_tx.raw"

# Sign transaction
log_info "Signing transaction..."

cardano-cli conway transaction sign \
    --testnet-magic $NETWORK_MAGIC \
    --tx-body-file "$DEPLOY_DIR/init_tx.raw" \
    --signing-key-file "$KEYS_DIR/payment.skey" \
    --out-file "$DEPLOY_DIR/init_tx.signed"

log_success "Transaction signed: $DEPLOY_DIR/init_tx.signed"

# Get transaction ID
TX_ID=$(cardano-cli conway transaction txid --tx-file "$DEPLOY_DIR/init_tx.signed")
log_info "Transaction ID: $TX_ID"

# Submit transaction
log_info "Submitting transaction..."

SUBMIT_RESULT=$(submit_tx "$DEPLOY_DIR/init_tx.signed")
echo "Submit result: $SUBMIT_RESULT"

if [[ "$SUBMIT_RESULT" == *"error"* ]] || [[ "$SUBMIT_RESULT" == *"Error"* ]]; then
    log_error "Transaction submission failed"
    echo "$SUBMIT_RESULT"
    exit 1
fi

log_success "Transaction submitted!"
log_info ""
log_info "=== Deployment Complete ==="
log_info "Transaction ID: $TX_ID"
log_info ""
log_info "View on explorer:"
log_info "  https://preview.cardanoscan.io/transaction/$TX_ID"
log_info ""
log_info "Mailbox UTxO: ${TX_ID}#0"
log_info "Mailbox State NFT Policy: $MAILBOX_NFT_POLICY"

# Save deployment info
cat > "$DEPLOY_DIR/deployment_info.json" << EOF
{
    "network": "preview",
    "tx_id": "$TX_ID",
    "mailbox": {
        "script_hash": "$(cat $DEPLOY_DIR/mailbox.hash)",
        "address": "$MAILBOX_ADDR",
        "utxo": "${TX_ID}#0",
        "state_nft_policy": "$MAILBOX_NFT_POLICY"
    }
}
EOF

log_success "Deployment info saved to $DEPLOY_DIR/deployment_info.json"
log_info ""
log_warn "IMPORTANT: Update your relayer config with the state NFT policy ID!"
log_info "In relayer-config.json, set:"
log_info "  \"mailboxPolicyId\": \"$MAILBOX_NFT_POLICY\""

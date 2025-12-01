#!/bin/bash
# Split UTXO to create collateral input

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.."

WALLET_ADDR=$(cat testnet-keys/payment.addr)
BLOCKFROST_API_KEY="previewjtDS4mHuhJroFIX0BfOpVmMdnyTrWMfh"
BLOCKFROST_URL="https://cardano-preview.blockfrost.io/api/v0"

# Get UTxO info
UTXOS=$(curl -s -H "project_id: $BLOCKFROST_API_KEY" "$BLOCKFROST_URL/addresses/$WALLET_ADDR/utxos")
TX_HASH=$(echo "$UTXOS" | jq -r '.[0].tx_hash')
TX_INDEX=$(echo "$UTXOS" | jq -r '.[0].tx_index')
INPUT_VALUE=$(echo "$UTXOS" | jq -r '.[0].amount[0].quantity')

echo "Input UTXO: ${TX_HASH}#${TX_INDEX}"
echo "Input value: $INPUT_VALUE lovelace"

COLLATERAL=50000000
FEE=200000
CHANGE=$((INPUT_VALUE - COLLATERAL - FEE))

echo "Splitting into:"
echo "  Collateral: $COLLATERAL lovelace"
echo "  Change: $CHANGE lovelace"
echo "  Fee: $FEE lovelace"

# Get current slot for validity
SLOT=$(curl -s -H "project_id: $BLOCKFROST_API_KEY" "$BLOCKFROST_URL/blocks/latest" | jq -r '.slot')
VALIDITY=$((SLOT + 7200))

echo "Validity until slot: $VALIDITY"

# Build transaction
cardano-cli conway transaction build-raw \
    --tx-in "${TX_HASH}#${TX_INDEX}" \
    --tx-out "$WALLET_ADDR+$COLLATERAL" \
    --tx-out "$WALLET_ADDR+$CHANGE" \
    --fee $FEE \
    --invalid-hereafter $VALIDITY \
    --out-file deployments/preview/split_tx.raw

echo "Transaction built"

# Sign transaction
cardano-cli conway transaction sign \
    --testnet-magic 2 \
    --tx-body-file deployments/preview/split_tx.raw \
    --signing-key-file testnet-keys/payment.skey \
    --out-file deployments/preview/split_tx.signed

echo "Transaction signed"

# Get TX ID
TX_ID=$(cardano-cli conway transaction txid --tx-file deployments/preview/split_tx.signed)
echo "Transaction ID: $TX_ID"

# Submit via Blockfrost
CBOR_HEX=$(cat deployments/preview/split_tx.signed | jq -r '.cborHex')
echo "$CBOR_HEX" | xxd -r -p > deployments/preview/split_tx.cbor

RESULT=$(curl -s -X POST "$BLOCKFROST_URL/tx/submit" \
    -H "project_id: $BLOCKFROST_API_KEY" \
    -H "Content-Type: application/cbor" \
    --data-binary @deployments/preview/split_tx.cbor)

echo "Submit result: $RESULT"

if [[ "$RESULT" == *"error"* ]]; then
    echo "Transaction submission failed!"
    exit 1
fi

echo ""
echo "SUCCESS! New UTXOs will be:"
echo "  ${TX_ID}#0 - Collateral ($COLLATERAL lovelace)"
echo "  ${TX_ID}#1 - Change ($CHANGE lovelace)"
echo ""
echo "Wait ~20 seconds for confirmation, then run init-contracts.sh"

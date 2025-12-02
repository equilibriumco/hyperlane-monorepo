#!/bin/bash
# Fetch validator public keys from S3 checkpoint storage
#
# Hyperlane validators store signed checkpoints in S3 buckets.
# We can recover their public keys from the signatures.
#
# Prerequisites:
# - aws CLI configured (even for public S3 buckets)
# - Node.js with ethers library

set -e

echo "=========================================="
echo "Hyperlane Validator Public Key Recovery"
echo "=========================================="
echo ""

# Fuji validators from Hyperlane docs (2/3 threshold)
VALIDATORS=(
    "0xd8154f73d04cc7f7f0c332793692e6e6f6b2402e"
    "0x895ae30bc83ff1493b9cf7781b0b813d23659857"
    "0x43e915573d9f1383cbf482049e4a012290759e7f"
)

echo "Fuji Validators:"
for v in "${VALIDATORS[@]}"; do
    echo "  - $v"
done
echo ""

# Check if we have the validator storage locations
echo "To recover public keys, we need to:"
echo "1. Find the S3 bucket URLs where validators store their checkpoints"
echo "2. Fetch a signed checkpoint from each validator"
echo "3. Recover the public key from the signature"
echo ""

# The storage locations are announced via ValidatorAnnounce contract
# Let's query them using cast (foundry)

FUJI_RPC="https://api.avax-test.network/ext/bc/C/rpc"
VALIDATOR_ANNOUNCE="0x4f7179A691F8a684f56cF7Fed65171877d30739a"

echo "Querying ValidatorAnnounce contract..."
echo "Contract: $VALIDATOR_ANNOUNCE"
echo "RPC: $FUJI_RPC"
echo ""

# Check if cast is available
if command -v cast &> /dev/null; then
    echo "Using cast to query storage locations..."

    # Get announced validators
    ANNOUNCED=$(cast call $VALIDATOR_ANNOUNCE "getAnnouncedValidators()(address[])" --rpc-url $FUJI_RPC 2>/dev/null || echo "[]")
    echo "Announced validators: $ANNOUNCED"
    echo ""

    # Get storage locations for our validators
    for VALIDATOR in "${VALIDATORS[@]}"; do
        echo "Validator: $VALIDATOR"
        LOCATIONS=$(cast call $VALIDATOR_ANNOUNCE "getAnnouncedStorageLocations(address[])(string[][])" "[$VALIDATOR]" --rpc-url $FUJI_RPC 2>/dev/null || echo "")
        echo "  Storage locations: $LOCATIONS"
        echo ""
    done
else
    echo "cast (foundry) not installed. Install it with:"
    echo "  curl -L https://foundry.paradigm.xyz | bash"
    echo "  foundryup"
    echo ""
fi

echo ""
echo "=========================================="
echo "Alternative: Manual Public Key Entry"
echo "=========================================="
echo ""
echo "If you have the validator public keys (from the operator), you can"
echo "register them directly with the ISM:"
echo ""
echo "Format: 33-byte compressed secp256k1 public key (hex)"
echo "Example: 0x02abc123...def (33 bytes = 66 hex chars)"
echo ""
echo "To update the ISM validators, use:"
echo ""
echo "  ./cli/target/release/hyperlane-cardano ism set-validators \\"
echo "    --domain 43113 \\"
echo "    --validators 02abc...,03def...,02ghi..."
echo ""
echo "Note: Public keys must be in the SAME ORDER as they appear in"
echo "the EVM ISM configuration to match validator indices."

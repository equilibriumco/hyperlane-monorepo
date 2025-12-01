#!/bin/bash
cd /home/guilherme/workspace/eiger/hyperlane-cardano/contracts

# Extract mailbox script
CODE=$(cat plutus.json | jq -r '.validators[] | select(.title == "mailbox.mailbox.spend") | .compiledCode')
echo "Code length: ${#CODE}"
echo "Code prefix: ${CODE:0:20}"

# Create plutus file
cat > /tmp/test.plutus << EOF
{
    "type": "PlutusScriptV3",
    "cborHex": "$CODE"
}
EOF

echo "Created /tmp/test.plutus"
cat /tmp/test.plutus | head -c 200
echo ""

# Try to hash
echo "Hashing..."
cardano-cli hash script --script-file /tmp/test.plutus

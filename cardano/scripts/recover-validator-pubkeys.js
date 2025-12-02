// Script to recover secp256k1 public keys from Hyperlane validator checkpoints
//
// Usage: node recover-validator-pubkeys.js
//
// This script:
// 1. Fetches signed checkpoints from validator S3 buckets
// 2. Recomputes the signing hash using Hyperlane's checkpoint format
// 3. Recovers the secp256k1 public key from the signature
// 4. Outputs the compressed public keys for ISM configuration
//
// Requires: npm install ethers@5

const { ethers } = require("ethers");
const https = require("https");

// Fuji validators (2/3 threshold)
const FUJI_VALIDATORS = [
  {
    address: "0xd8154f73d04cc7f7f0c332793692e6e6f6b2402e",
    bucket: "hyperlane-testnet4-fuji-validator-0",
    region: "us-east-1"
  },
  {
    address: "0x895ae30bc83ff1493b9cf7781b0b813d23659857",
    bucket: "hyperlane-testnet4-fuji-validator-1",
    region: "us-east-1"
  },
  {
    address: "0x43e915573d9f1383cbf482049e4a012290759e7f",
    bucket: "hyperlane-testnet4-fuji-validator-2",
    region: "us-east-1"
  }
];

// Fuji domain ID
const FUJI_DOMAIN = 43113;

// Fetch JSON from URL
function fetchJson(url) {
  return new Promise((resolve, reject) => {
    https.get(url, (res) => {
      let data = "";
      res.on("data", chunk => data += chunk);
      res.on("end", () => {
        try {
          resolve(JSON.parse(data));
        } catch (e) {
          reject(new Error(`Failed to parse JSON from ${url}: ${e.message}`));
        }
      });
      res.on("error", reject);
    }).on("error", reject);
  });
}

// Compute Hyperlane domain hash
// domain_hash(address, domain) = keccak256(domain_be_bytes || address || "HYPERLANE")
function computeDomainHash(merkleTreeHookAddress, domain) {
  // Domain as 4-byte big-endian
  const domainBytes = ethers.utils.zeroPad(ethers.utils.hexlify(domain), 4);

  // Address as 32 bytes (H256)
  const addressBytes = ethers.utils.arrayify(merkleTreeHookAddress);

  // "HYPERLANE" as bytes
  const hyperlaneBytes = ethers.utils.toUtf8Bytes("HYPERLANE");

  // Concatenate and hash
  const data = ethers.utils.concat([domainBytes, addressBytes, hyperlaneBytes]);
  return ethers.utils.keccak256(data);
}

// Compute checkpoint signing hash
// signing_hash = keccak256(domain_hash || root || index_be_bytes || message_id)
function computeSigningHash(checkpoint, messageId) {
  const domainHash = computeDomainHash(
    checkpoint.merkle_tree_hook_address,
    checkpoint.mailbox_domain
  );

  // Index as 4-byte big-endian
  const indexBytes = ethers.utils.zeroPad(ethers.utils.hexlify(checkpoint.index), 4);

  // Concatenate: domain_hash || root || index || message_id
  const data = ethers.utils.concat([
    ethers.utils.arrayify(domainHash),
    ethers.utils.arrayify(checkpoint.root),
    indexBytes,
    ethers.utils.arrayify(messageId)
  ]);

  return ethers.utils.keccak256(data);
}

// Compute EIP-191 signed message hash
// eth_signed_message_hash = keccak256("\x19Ethereum Signed Message:\n32" || hash)
function computeEthSignedMessageHash(hash) {
  return ethers.utils.hashMessage(ethers.utils.arrayify(hash));
}

// Compress a secp256k1 public key from 65 bytes to 33 bytes
function compressPublicKey(uncompressedHex) {
  // Remove 0x prefix if present
  let hex = uncompressedHex.startsWith("0x") ? uncompressedHex.slice(2) : uncompressedHex;

  // Remove 04 prefix (uncompressed format indicator)
  if (hex.startsWith("04")) {
    hex = hex.slice(2);
  }

  // X and Y coordinates (32 bytes each)
  const x = hex.slice(0, 64);
  const y = hex.slice(64, 128);

  // Compressed format: prefix + x
  // prefix is 0x02 if y is even, 0x03 if y is odd
  const yLastByte = parseInt(y.slice(-2), 16);
  const prefix = (yLastByte % 2 === 0) ? "02" : "03";

  return "0x" + prefix + x;
}

async function recoverValidatorPublicKey(validator) {
  const s3Url = `https://${validator.bucket}.s3.${validator.region}.amazonaws.com`;

  console.log(`\nValidator: ${validator.address}`);
  console.log(`S3 Bucket: ${validator.bucket}`);
  console.log("=".repeat(70));

  try {
    // First, get the latest checkpoint index
    const latestUrl = `${s3Url}/checkpoint_latest_index.json`;
    const latestIndex = await fetchJson(latestUrl);
    console.log(`Latest checkpoint index: ${latestIndex}`);

    // Fetch the signed checkpoint
    const checkpointUrl = `${s3Url}/checkpoint_${latestIndex}_with_id.json`;
    console.log(`Fetching: ${checkpointUrl}`);
    const signedCheckpoint = await fetchJson(checkpointUrl);

    const checkpoint = signedCheckpoint.value.checkpoint;
    const messageId = signedCheckpoint.value.message_id;
    const signature = signedCheckpoint.signature;

    console.log(`\nCheckpoint data:`);
    console.log(`  Merkle tree hook: ${checkpoint.merkle_tree_hook_address}`);
    console.log(`  Domain: ${checkpoint.mailbox_domain}`);
    console.log(`  Root: ${checkpoint.root}`);
    console.log(`  Index: ${checkpoint.index}`);
    console.log(`  Message ID: ${messageId}`);

    // Compute the signing hash
    const signingHash = computeSigningHash(checkpoint, messageId);
    console.log(`\nComputed signing hash: ${signingHash}`);

    // Compute EIP-191 message hash
    const ethSignedHash = computeEthSignedMessageHash(signingHash);
    console.log(`EIP-191 message hash: ${ethSignedHash}`);

    // Construct the signature for recovery
    const sig = {
      r: signature.r,
      s: signature.s,
      v: signature.v
    };

    // Convert to flat signature format
    const flatSig = ethers.utils.joinSignature(sig);
    console.log(`\nSignature: ${flatSig}`);

    // Recover the address to verify
    const recoveredAddress = ethers.utils.recoverAddress(ethSignedHash, flatSig);
    console.log(`\nRecovered address: ${recoveredAddress}`);

    if (recoveredAddress.toLowerCase() === validator.address.toLowerCase()) {
      console.log(`Address MATCHES expected validator!`);

      // Now recover the full public key
      const recoveredPubKey = ethers.utils.recoverPublicKey(ethSignedHash, flatSig);
      console.log(`\nRecovered public key (uncompressed, 65 bytes):`);
      console.log(`  ${recoveredPubKey}`);

      // Compress the public key for Cardano ISM
      const compressedPubKey = compressPublicKey(recoveredPubKey);
      console.log(`\nCompressed public key (33 bytes) for ISM:`);
      console.log(`  ${compressedPubKey}`);

      return {
        address: validator.address,
        publicKey: compressedPubKey
      };
    } else {
      console.log(`Address MISMATCH!`);
      console.log(`  Expected: ${validator.address}`);
      console.log(`  Got: ${recoveredAddress}`);
      return null;
    }

  } catch (error) {
    console.log(`Error: ${error.message}`);
    return null;
  }
}

async function main() {
  console.log("=========================================");
  console.log("Hyperlane Validator Public Key Recovery");
  console.log("=========================================");
  console.log(`\nRecovering public keys for Fuji (domain ${FUJI_DOMAIN}) validators...`);
  console.log(`Threshold: 2 of 3\n`);

  const results = [];

  for (const validator of FUJI_VALIDATORS) {
    const result = await recoverValidatorPublicKey(validator);
    if (result) {
      results.push(result);
    }
  }

  console.log("\n\n=========================================");
  console.log("SUMMARY: Public Keys for ISM Configuration");
  console.log("=========================================\n");

  if (results.length === 0) {
    console.log("No public keys recovered. Check errors above.");
    return;
  }

  console.log("Domain:", FUJI_DOMAIN);
  console.log("Threshold: 2");
  console.log("\nValidator public keys (in order):\n");

  const pubKeys = [];
  for (const result of results) {
    console.log(`  ${result.address}`);
    console.log(`  -> ${result.publicKey}\n`);
    pubKeys.push(result.publicKey.slice(2)); // Remove 0x prefix
  }

  console.log("\n=========================================");
  console.log("CLI Command to Update ISM");
  console.log("=========================================\n");

  const pubKeysArg = pubKeys.join(",");
  console.log(`./cli/target/release/hyperlane-cardano ism set-validators \\`);
  console.log(`  --domain ${FUJI_DOMAIN} \\`);
  console.log(`  --threshold 2 \\`);
  console.log(`  --validators ${pubKeysArg}`);

  console.log("\n\nNote: The public keys MUST be in the same order as validators");
  console.log("appear in the EVM ISM configuration to match validator indices.");
}

main().catch(console.error);

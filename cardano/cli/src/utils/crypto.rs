//! Cryptographic utilities for Cardano

use anyhow::{anyhow, Context, Result};
use blake2::digest::{consts::U28, Digest};
use blake2::Blake2b;
use ed25519_dalek::{SigningKey, VerifyingKey};
use pallas_addresses::{Address, Network, ShelleyAddress, ShelleyDelegationPart, ShelleyPaymentPart};
use pallas_crypto::key::ed25519::{PublicKey as PallasPublicKey, SecretKey as PallasSecretKey};
use std::path::Path;

/// Ed25519 keypair for signing transactions
#[derive(Clone)]
pub struct Keypair {
    signing_key: SigningKey,
    pallas_public: PallasPublicKey,
    payment_cred_hash: [u8; 28],
}

impl Keypair {
    /// Create keypair from 32-byte secret key
    pub fn from_secret_key(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 32 {
            return Err(anyhow!("Secret key must be 32 bytes, got {}", bytes.len()));
        }
        let mut key_bytes = [0u8; 32];
        key_bytes.copy_from_slice(bytes);
        let signing_key = SigningKey::from_bytes(&key_bytes);

        // Create pallas secret key and derive public key
        let pallas_secret = PallasSecretKey::from(key_bytes);
        let pallas_public = pallas_secret.public_key();

        // Compute payment credential hash (Blake2b-224 of public key)
        let public_key_bytes = signing_key.verifying_key().to_bytes();
        let payment_cred_hash = blake2b_224(&public_key_bytes);

        Ok(Self { signing_key, pallas_public, payment_cred_hash })
    }

    /// Load keypair from a file (supports Cardano CLI JSON format or raw hex)
    pub fn from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read key file: {:?}", path))?;

        // Try JSON format first (Cardano CLI format)
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(cbor_hex) = json.get("cborHex").and_then(|v| v.as_str()) {
                // CBOR format: 5820 (32-byte bytestring prefix) + key
                let hex_key = cbor_hex.strip_prefix("5820").unwrap_or(cbor_hex);
                let bytes = hex::decode(hex_key)
                    .with_context(|| "Failed to decode cborHex")?;
                return Self::from_secret_key(&bytes);
            }
        }

        // Try raw hex
        let trimmed = content.trim();
        let hex_str = trimmed.strip_prefix("0x").unwrap_or(trimmed);
        if hex_str.len() == 64 {
            let bytes = hex::decode(hex_str)
                .with_context(|| "Failed to decode hex key")?;
            return Self::from_secret_key(&bytes);
        }

        // Try raw binary
        let bytes = std::fs::read(path)
            .with_context(|| format!("Failed to read binary key: {:?}", path))?;
        if bytes.len() >= 32 {
            Self::from_secret_key(&bytes[..32])
        } else {
            Err(anyhow!("Invalid key file format"))
        }
    }

    /// Get the public key
    pub fn public_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// Get the public key bytes
    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.public_key().to_bytes()
    }

    /// Get the verification key hash (28 bytes blake2b-224)
    pub fn verification_key_hash(&self) -> [u8; 28] {
        blake2b_224(&self.public_key_bytes())
    }

    /// Get the verification key hash as hex string
    pub fn verification_key_hash_hex(&self) -> String {
        hex::encode(self.verification_key_hash())
    }

    /// Get the Shelley address for this keypair
    pub fn address(&self, network: Network) -> Address {
        let vkh = self.verification_key_hash();
        let payment = ShelleyPaymentPart::key_hash(pallas_crypto::hash::Hash::new(vkh));
        Address::Shelley(ShelleyAddress::new(network, payment, ShelleyDelegationPart::Null))
    }

    /// Get the bech32 address string
    pub fn address_bech32(&self, network: Network) -> String {
        self.address(network).to_bech32().unwrap_or_default()
    }

    /// Sign a message (returns 64-byte signature)
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        use ed25519_dalek::Signer;
        self.signing_key.sign(message).to_bytes()
    }

    /// Get the raw signing key bytes (for transaction signing)
    pub fn signing_key_bytes(&self) -> &[u8; 32] {
        self.signing_key.as_bytes()
    }

    /// Get the pallas public key (for transaction signing)
    pub fn pallas_public_key(&self) -> &PallasPublicKey {
        &self.pallas_public
    }

    /// Get the payment credential hash (28 bytes, for required signers)
    pub fn payment_credential_hash(&self) -> &[u8; 28] {
        &self.payment_cred_hash
    }

    /// Get the public key hash as Vec<u8> (alias for convenience)
    pub fn pub_key_hash(&self) -> Vec<u8> {
        self.payment_cred_hash.to_vec()
    }
}

/// Compute Blake2b-224 hash (28 bytes)
pub fn blake2b_224(data: &[u8]) -> [u8; 28] {
    type Blake2b224 = Blake2b<U28>;
    let mut hasher = Blake2b224::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut hash = [0u8; 28];
    hash.copy_from_slice(&result);
    hash
}

/// Compute Blake2b-256 hash (32 bytes)
pub fn blake2b_256(data: &[u8]) -> [u8; 32] {
    use blake2::digest::consts::U32;
    type Blake2b256 = Blake2b<U32>;
    let mut hasher = Blake2b256::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&result);
    hash
}

/// Compute Keccak256 hash (for Hyperlane message IDs)
pub fn keccak256(data: &[u8]) -> [u8; 32] {
    use sha3::Keccak256;
    let mut hasher = Keccak256::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&result);
    hash
}

/// Compute script hash from compiled CBOR code
pub fn script_hash(cbor_code: &[u8]) -> [u8; 28] {
    // PlutusV3 scripts use a 0x03 prefix before hashing
    let mut prefixed = vec![0x03];
    prefixed.extend_from_slice(cbor_code);
    blake2b_224(&prefixed)
}

/// Compute script hash from hex-encoded CBOR
pub fn script_hash_from_hex(cbor_hex: &str) -> Result<[u8; 28]> {
    let cbor = hex::decode(cbor_hex)
        .with_context(|| "Failed to decode CBOR hex")?;
    Ok(script_hash(&cbor))
}

/// Derive script address from script hash
pub fn script_address(hash: &[u8; 28], network: Network) -> Address {
    let payment = ShelleyPaymentPart::script_hash(pallas_crypto::hash::Hash::new(*hash));
    Address::Shelley(ShelleyAddress::new(network, payment, ShelleyDelegationPart::Null))
}

/// Derive script address bech32 from script hash
pub fn script_address_bech32(hash: &[u8; 28], network: Network) -> String {
    script_address(hash, network).to_bech32().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blake2b_224() {
        let data = b"hello";
        let hash = blake2b_224(data);
        assert_eq!(hash.len(), 28);
    }

    #[test]
    fn test_blake2b_256() {
        let data = b"hello";
        let hash = blake2b_256(data);
        assert_eq!(hash.len(), 32);
    }

    #[test]
    fn test_keccak256() {
        let data = b"hello";
        let hash = keccak256(data);
        assert_eq!(hash.len(), 32);
    }
}

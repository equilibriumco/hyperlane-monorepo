use blake2::{Blake2b, Digest};
use ed25519_dalek::{SecretKey, SigningKey, VerifyingKey};
use hyperlane_core::H256;
use pallas_addresses::{Address as CardanoAddress, Network, ShelleyAddress};
use pallas_crypto::hash::Hash;
use pallas_crypto::key::ed25519::{PublicKey as PallasPublicKey, SecretKey as PallasSecretKey};

/// Cardano Ed25519 Keypair for Hyperlane
///
/// Cardano addresses are 28 bytes (Blake2b_224 hash of the public key).
/// For Hyperlane compatibility, they're padded to 32 bytes with a 4-byte prefix:
/// - 0x00000000: Payment credential (must sign tx)
/// - 0x01000000: Minting policy hash (must mint in tx)
/// - 0x02000000: Validator hash (must run in tx)
#[derive(Debug, Clone)]
pub struct Keypair {
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
    _pallas_secret: PallasSecretKey,
    pallas_public: PallasPublicKey,
    payment_credential_hash: [u8; 28], // Blake2b_224 of public key
}

impl Keypair {
    /// Create a keypair from a secret key (32 bytes)
    pub fn from_secret_key(secret_bytes: &[u8]) -> Result<Self, String> {
        if secret_bytes.len() != 32 {
            return Err(format!(
                "Secret key must be 32 bytes, got {}",
                secret_bytes.len()
            ));
        }

        // Create ed25519-dalek signing key
        let secret_key =
            SecretKey::try_from(secret_bytes).map_err(|e| format!("Invalid secret key: {}", e))?;
        let signing_key = SigningKey::from_bytes(&secret_key);
        let verifying_key = signing_key.verifying_key();

        // Create Pallas keys
        let mut secret_array = [0u8; 32];
        secret_array.copy_from_slice(secret_bytes);
        let pallas_secret = PallasSecretKey::from(secret_array);
        let pallas_public = pallas_secret.public_key();

        // Compute payment credential hash (Blake2b_224 of public key)
        let public_key_bytes = verifying_key.as_bytes();
        let payment_credential_hash = Self::blake2b_224(public_key_bytes);

        Ok(Self {
            signing_key,
            verifying_key,
            _pallas_secret: pallas_secret,
            pallas_public,
            payment_credential_hash,
        })
    }

    /// Compute Blake2b_224 hash (28 bytes)
    fn blake2b_224(data: &[u8]) -> [u8; 28] {
        let mut hasher = Blake2b::<blake2::digest::consts::U28>::new();
        hasher.update(data);
        let result = hasher.finalize();
        let mut hash = [0u8; 28];
        hash.copy_from_slice(&result);
        hash
    }

    /// Create from hex-encoded string
    pub fn from_string(hex_string: &str) -> Option<Self> {
        let hex_string = hex_string.strip_prefix("0x").unwrap_or(hex_string);
        let secret_bytes = hex::decode(hex_string).ok()?;
        Self::from_secret_key(&secret_bytes).ok()
    }

    /// Get the Cardano payment address (bech32 format) for testnet
    pub fn address_bech32_testnet(&self) -> String {
        self.address_bech32(Network::Testnet)
    }

    /// Get the Cardano payment address (bech32 format)
    pub fn address_bech32(&self, network: Network) -> String {
        // Create a Shelley payment address from the payment credential hash
        let payment_hash = Hash::<28>::from(self.payment_credential_hash);

        // Create payment part (key hash credential, no script)
        let payment_part = pallas_addresses::ShelleyPaymentPart::Key(payment_hash);

        // No staking delegation for now
        let delegation_part = pallas_addresses::ShelleyDelegationPart::Null;

        let shelley_addr = ShelleyAddress::new(network, payment_part, delegation_part);
        let cardano_addr = CardanoAddress::Shelley(shelley_addr);
        cardano_addr
            .to_bech32()
            .unwrap_or_else(|_| "invalid_address".to_string())
    }

    /// Get the payment credential as H256 (Hyperlane format)
    ///
    /// Format: 0x00000000 (prefix) + 28-byte Blake2b_224 hash
    pub fn address_h256(&self) -> H256 {
        let mut bytes = [0u8; 32];
        // Prefix 0x00000000 indicates payment credential
        bytes[0..4].copy_from_slice(&[0u8; 4]);
        // Copy 28-byte credential hash
        bytes[4..32].copy_from_slice(&self.payment_credential_hash);
        H256::from(bytes)
    }

    /// Get the raw payment credential hash (28 bytes)
    pub fn payment_credential_hash(&self) -> &[u8; 28] {
        &self.payment_credential_hash
    }

    /// Get the verification key
    pub fn verifying_key(&self) -> &VerifyingKey {
        &self.verifying_key
    }

    /// Get the Pallas public key
    pub fn pallas_public_key(&self) -> &PallasPublicKey {
        &self.pallas_public
    }

    /// Sign a message (for Cardano transactions)
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        use ed25519_dalek::Signer;
        let signature = self.signing_key.sign(message);
        signature.to_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keypair_creation() {
        // Test with a valid 32-byte key
        let secret_key = [1u8; 32];
        let keypair = Keypair::from_secret_key(&secret_key).unwrap();

        // Check that address_h256 has correct format
        let h256 = keypair.address_h256();
        let bytes = h256.as_bytes();

        // First 4 bytes should be 0x00000000 (payment credential prefix)
        assert_eq!(&bytes[0..4], &[0u8; 4]);

        // Remaining 28 bytes should be the credential hash
        assert_eq!(&bytes[4..32], keypair.payment_credential_hash());
    }

    #[test]
    fn test_from_hex_string() {
        let hex_key = "0x0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let keypair = Keypair::from_string(hex_key).unwrap();

        // Verify it produces a valid H256
        let h256 = keypair.address_h256();
        assert_eq!(h256.as_bytes()[0..4], [0u8; 4]); // Check prefix
    }

    #[test]
    fn test_address_generation() {
        let hex_key = "0x0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let keypair = Keypair::from_string(hex_key).unwrap();

        // Generate testnet address
        let testnet_addr = keypair.address_bech32(Network::Testnet);
        assert!(testnet_addr.starts_with("addr_test"));

        // Generate mainnet address
        let mainnet_addr = keypair.address_bech32(Network::Mainnet);
        assert!(mainnet_addr.starts_with("addr"));
    }
}

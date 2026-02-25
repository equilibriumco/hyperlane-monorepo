use hyperlane_core::{HyperlaneMessage, H256};
use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};

/// Domain identifier (chain ID in Hyperlane terms)
pub type Domain = u32;

/// 32-byte Hyperlane address
pub type HyperlaneAddress = [u8; 32];

/// Cardano script hash (28 bytes)
pub type ScriptHash = [u8; 28];

/// Policy ID (same as script hash, 28 bytes)
pub type PolicyId = [u8; 28];

/// Cardano address (bech32 string)
pub type CardanoAddress = String;

/// UTXO reference
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct UtxoRef {
    pub tx_hash: String,
    pub output_index: u32,
}

impl UtxoRef {
    pub fn new(tx_hash: String, output_index: u32) -> Self {
        Self {
            tx_hash,
            output_index,
        }
    }
}

/// UTXO locator using NFT marker
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UtxoLocator {
    pub policy_id: String,  // hex-encoded
    pub asset_name: String, // hex-encoded
}

/// Hyperlane message structure (matches Aiken types.ak)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub version: u8,
    pub nonce: u32,
    pub origin: Domain,
    pub sender: HyperlaneAddress,
    pub destination: Domain,
    pub recipient: HyperlaneAddress,
    pub body: Vec<u8>,
}

impl Message {
    /// Encode the message for hashing (matches Aiken encode_message)
    pub fn encode(&self) -> Vec<u8> {
        let mut encoded = Vec::new();

        // Version (1 byte)
        encoded.push(self.version);

        // Nonce (4 bytes, big-endian)
        encoded.extend_from_slice(&self.nonce.to_be_bytes());

        // Origin domain (4 bytes, big-endian)
        encoded.extend_from_slice(&self.origin.to_be_bytes());

        // Sender (32 bytes)
        encoded.extend_from_slice(&self.sender);

        // Destination domain (4 bytes, big-endian)
        encoded.extend_from_slice(&self.destination.to_be_bytes());

        // Recipient (32 bytes)
        encoded.extend_from_slice(&self.recipient);

        // Body (variable length)
        encoded.extend_from_slice(&self.body);

        encoded
    }

    /// Compute the message ID (keccak256 hash of encoded message)
    pub fn id(&self) -> [u8; 32] {
        let encoded = self.encode();
        let mut hasher = Keccak256::new();
        hasher.update(&encoded);
        hasher.finalize().into()
    }

    /// Convert from hyperlane-core HyperlaneMessage
    pub fn from_hyperlane_message(msg: &HyperlaneMessage) -> Self {
        Self {
            version: msg.version,
            nonce: msg.nonce,
            origin: msg.origin,
            sender: msg.sender.0,
            destination: msg.destination,
            recipient: msg.recipient.0,
            body: msg.body.clone(),
        }
    }
}

/// Merkle tree state stored in datum (matches Aiken MerkleTreeState)
/// Stores full branch state for incremental tree updates
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleTreeState {
    /// Branch hashes at each level (32 branches, each 32 bytes)
    pub branches: Vec<[u8; 32]>,
    /// Number of leaves inserted
    pub count: u32,
}

/// Mailbox datum structure (matches Aiken MailboxDatum)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailboxDatum {
    pub local_domain: Domain,
    pub default_ism: ScriptHash,
    pub owner: [u8; 28], // VerificationKeyHash
    pub outbound_nonce: u32,
    /// Full merkle tree state (branches + count)
    pub merkle_tree: MerkleTreeState,
    /// 32-byte Sparse Merkle Tree root for replay protection
    pub processed_tree_root: [u8; 32],
}

/// Mailbox redeemer (matches Aiken MailboxRedeemer)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MailboxRedeemer {
    Dispatch {
        destination: Domain,
        recipient: HyperlaneAddress,
        body: Vec<u8>,
        sender_ref: ([u8; 32], u32), // (tx_hash, output_index)
        hook_metadata: Vec<u8>,
    },
    Process {
        message: Message,
        metadata: Vec<u8>,
        message_id: [u8; 32],
        smt_proof: Vec<[u8; 32]>,
    },
    SetDefaultIsm {
        new_ism: ScriptHash,
    },
    TransferOwnership {
        new_owner: [u8; 28],
    },
}

/// Processed message marker datum (matches Aiken ProcessedMessageDatum)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessedMessageDatum {
    pub message_id: [u8; 32],
}

/// Multisig ISM datum (matches Aiken MultisigIsmDatum)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultisigIsmDatum {
    pub validators: Vec<(Domain, Vec<[u8; 32]>)>, // Validator public keys per domain
    pub thresholds: Vec<(Domain, u32)>,           // Threshold per domain
    pub owner: [u8; 28],                          // VerificationKeyHash
}

/// ECDSA secp256k1 signature wrapper (65 bytes: r || s || v)
/// Hyperlane validators use Ethereum-style ECDSA signatures
#[derive(Debug, Clone)]
pub struct Signature(pub [u8; 65]);

impl Serialize for Signature {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&hex::encode(self.0))
    }
}

impl<'de> Deserialize<'de> for Signature {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let bytes = hex::decode(&s).map_err(serde::de::Error::custom)?;
        if bytes.len() != 65 {
            return Err(serde::de::Error::custom("Signature must be 65 bytes"));
        }
        let mut arr = [0u8; 65];
        arr.copy_from_slice(&bytes);
        Ok(Signature(arr))
    }
}

/// Checkpoint data that validators sign (matches Aiken Checkpoint)
/// Hyperlane validators sign checkpoints with this structure:
/// 1. domain_hash = keccak256(origin || merkle_tree_hook || "HYPERLANE")
/// 2. digest = keccak256(domain_hash || merkle_root || merkle_index || message_id)
/// 3. signed = EIP-191(digest)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Origin domain (same as message origin)
    pub origin: Domain,
    /// Merkle root of the message tree (32 bytes)
    pub merkle_root: [u8; 32],
    /// Origin merkle tree hook address (32 bytes, typically the origin mailbox)
    /// This is used in the domain hash for validator signing
    pub origin_merkle_tree_hook: [u8; 32],
    /// Merkle tree index (nonce of the message in the tree)
    pub merkle_index: u32,
    /// Message ID (32 bytes) - keccak256 hash of the message
    pub message_id: [u8; 32],
}

/// Validator signature with recovered public key in both formats
/// The relayer recovers the public key off-chain and passes both formats for on-chain verification
#[derive(Debug, Clone)]
pub struct ValidatorSignature {
    /// Compressed public key (33 bytes: 0x02/0x03 prefix + x-coordinate)
    /// Used for verifyEcdsaSecp256k1Signature per CIP-49
    pub compressed_pubkey: [u8; 33],
    /// Uncompressed public key (64 bytes: x || y, no 0x04 prefix)
    /// Used to compute Ethereum address on-chain: keccak256(pubkey)[12:32]
    pub uncompressed_pubkey: [u8; 64],
    /// The 64-byte signature (r || s)
    pub signature: [u8; 64],
}

/// Ethereum address (20 bytes)
#[derive(Debug, Clone)]
pub struct EthAddress(pub [u8; 20]);

impl serde::Serialize for EthAddress {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for EthAddress {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes: Vec<u8> = serde::Deserialize::deserialize(deserializer)?;
        if bytes.len() != 20 {
            return Err(serde::de::Error::custom("expected 20 bytes"));
        }
        let mut arr = [0u8; 20];
        arr.copy_from_slice(&bytes);
        Ok(EthAddress(arr))
    }
}

/// Multisig ISM redeemer (matches Aiken MultisigIsmRedeemer)
///
/// Security model:
/// 1. Relayer recovers public keys from signatures off-chain (using v/recovery ID)
/// 2. ISM verifies each signature on-chain using verify_ecdsa_secp256k1_signature
/// 3. ISM computes Ethereum address from the verified public key
/// 4. ISM checks the address is in the trusted validators list
///
/// This provides cryptographic binding: an attacker cannot forge a signature
/// without the validator's private key.
#[derive(Debug, Clone)]
pub enum MultisigIsmRedeemer {
    Verify {
        checkpoint: Checkpoint,
        /// Validator signatures with recovered public keys
        validator_signatures: Vec<ValidatorSignature>,
    },
    SetValidators {
        domain: Domain,
        /// Ethereum addresses (20 bytes each) of trusted validators
        validators: Vec<EthAddress>,
    },
    SetThreshold {
        domain: Domain,
        threshold: u32,
    },
}

/// Hyperlane recipient datum wrapper (matches Aiken HyperlaneRecipientDatum)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperlaneRecipientDatum<T> {
    pub ism: Option<ScriptHash>,
    pub last_processed_nonce: Option<u32>,
    pub inner: T,
}

/// Hyperlane recipient redeemer (matches Aiken HyperlaneRecipientRedeemer)
///
/// SECURITY: HandleMessage includes the full Message and message_id.
/// Recipients MUST verify: keccak256(encode_message(message)) == message_id
/// This ensures the data was validated by the ISM (which signs the message_id).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HyperlaneRecipientRedeemer<T> {
    HandleMessage {
        /// The full message structure (for verification)
        message: Message,
        /// The message ID (keccak256 hash of encoded message)
        /// Recipients must verify message hashes to this
        message_id: [u8; 32],
    },
    ContractAction {
        action: T,
    },
}

/// Convert a Cardano script hash to Hyperlane address
pub fn script_hash_to_hyperlane_address(hash: &ScriptHash) -> HyperlaneAddress {
    let mut addr = [0u8; 32];
    // Script credential prefix: 0x02000000
    addr[0] = 0x02;
    addr[1] = 0x00;
    addr[2] = 0x00;
    addr[3] = 0x00;
    // Copy the 28-byte script hash
    addr[4..32].copy_from_slice(hash);
    addr
}

/// Convert a minting policy ID to Hyperlane address (0x01 prefix)
pub fn policy_id_to_hyperlane_address(id: &[u8; 28]) -> HyperlaneAddress {
    let mut addr = [0u8; 32];
    addr[0] = 0x01;
    addr[4..32].copy_from_slice(id);
    addr
}

/// Extract policy ID from Hyperlane address (if 0x01 prefix)
pub fn hyperlane_address_to_policy_id(addr: &HyperlaneAddress) -> Option<[u8; 28]> {
    if addr[0] == 0x01 && addr[1] == 0x00 && addr[2] == 0x00 && addr[3] == 0x00 {
        let mut id = [0u8; 28];
        id.copy_from_slice(&addr[4..32]);
        Some(id)
    } else {
        None
    }
}

/// Convert policy ID to H256 (with 0x01 prefix)
pub fn policy_id_to_h256(id: &[u8; 28]) -> H256 {
    let mut h = [0u8; 32];
    h[0] = 0x01;
    h[4..32].copy_from_slice(id);
    H256(h)
}

/// Extract policy ID from H256 (if 0x01 prefix)
pub fn h256_to_policy_id(h: &H256) -> Option<[u8; 28]> {
    if h.0[0] == 0x01 && h.0[1] == 0x00 && h.0[2] == 0x00 && h.0[3] == 0x00 {
        let mut id = [0u8; 28];
        id.copy_from_slice(&h.0[4..32]);
        Some(id)
    } else {
        None
    }
}

/// Convert a Hyperlane address to Cardano script hash
pub fn hyperlane_address_to_script_hash(addr: &HyperlaneAddress) -> Option<ScriptHash> {
    // Check if this is a script credential
    // Accept both 0x02000000 (canonical script prefix) and 0x00000000 (legacy/compat)
    let is_script_prefix = (addr[0] == 0x02 || addr[0] == 0x00)
        && addr[1] == 0x00
        && addr[2] == 0x00
        && addr[3] == 0x00;

    if is_script_prefix {
        let mut hash = [0u8; 28];
        hash.copy_from_slice(&addr[4..32]);
        Some(hash)
    } else {
        None
    }
}

/// Convert H256 to script hash (takes last 28 bytes)
pub fn h256_to_script_hash(h: &H256) -> ScriptHash {
    let mut hash = [0u8; 28];
    hash.copy_from_slice(&h.0[4..32]);
    hash
}

/// Convert script hash to H256 (with script prefix)
pub fn script_hash_to_h256(hash: &ScriptHash) -> H256 {
    let mut h = [0u8; 32];
    h[0] = 0x02;
    h[4..32].copy_from_slice(hash);
    H256(h)
}

// ============================================================================
// Warp Route Types
// ============================================================================

/// Warp token type (matches Aiken WarpTokenType)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WarpTokenType {
    /// Lock native Cardano tokens in vault
    Collateral {
        policy_id: String,
        asset_name: String,
        vault_locator: UtxoLocator,
    },
    /// Mint synthetic tokens
    Synthetic { minting_policy: ScriptHash },
    /// Native ADA
    Native { vault_locator: UtxoLocator },
}

/// Warp route configuration (matches Aiken WarpRouteConfig)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarpRouteConfig {
    pub token_type: WarpTokenType,
    /// Local token decimals (e.g., 6 for ADA)
    pub decimals: u8,
    /// Remote/wire format decimals (e.g., 18 for EVM chains)
    pub remote_decimals: u8,
    /// Remote routes: (domain, route_address)
    pub remote_routes: Vec<(Domain, HyperlaneAddress)>,
}

/// Warp route datum (matches Aiken WarpRouteDatum)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarpRouteDatum {
    pub config: WarpRouteConfig,
    pub owner: [u8; 28], // VerificationKeyHash
    pub total_bridged: i64,
    pub ism: Option<ScriptHash>,
}

/// Warp route redeemer (matches Aiken WarpRouteRedeemer)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WarpRouteRedeemer {
    TransferRemote {
        destination: Domain,
        recipient: HyperlaneAddress,
        amount: u64,
    },
    /// Receive tokens from another chain via Hyperlane message.
    /// Delivers tokens directly to the recipient address.
    ReceiveTransfer {
        message: Message,
        message_id: [u8; 32],
    },
    EnrollRemoteRoute {
        domain: Domain,
        route: HyperlaneAddress,
    },
}

/// Warp transfer body (encoded in message body)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarpTransferBody {
    pub recipient: Vec<u8>, // Cardano address bytes
    pub amount: u64,
}

impl WarpTransferBody {
    /// Encode transfer body for Hyperlane message
    pub fn encode(&self) -> Vec<u8> {
        let mut encoded = Vec::new();
        encoded.extend_from_slice(&self.recipient);
        encoded.extend_from_slice(&self.amount.to_be_bytes());
        encoded
    }

    /// Decode transfer body from Hyperlane message
    pub fn decode(body: &[u8]) -> Option<Self> {
        if body.len() < 8 {
            return None;
        }
        let amount_start = body.len() - 8;
        let recipient = body[..amount_start].to_vec();
        let mut amount_bytes = [0u8; 8];
        amount_bytes.copy_from_slice(&body[amount_start..]);
        let amount = u64::from_be_bytes(amount_bytes);
        Some(Self { recipient, amount })
    }
}

// ============================================================================
// Verified Message Types (Direct Delivery)
// ============================================================================

/// Verified message datum stored at the recipient script address (matches Aiken VerifiedMessageDatum).
/// The relayer creates this during mailbox Process and delivers it directly to the recipient script.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifiedMessageDatum {
    pub origin: Domain,
    pub sender: Vec<u8>,
    pub body: Vec<u8>,
    pub message_id: Vec<u8>,
    pub nonce: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_encoding() {
        let msg = Message {
            version: 3,
            nonce: 1,
            origin: 1,
            sender: [0u8; 32],
            destination: 2001,
            recipient: [1u8; 32],
            body: vec![0x48, 0x65, 0x6c, 0x6c, 0x6f], // "Hello"
        };

        let encoded = msg.encode();
        assert!(encoded.len() > 0);

        // Verify message ID is 32 bytes
        let id = msg.id();
        assert_eq!(id.len(), 32);
    }

    #[test]
    fn test_script_hash_conversion() {
        let hash: ScriptHash = [0x12; 28];
        let addr = script_hash_to_hyperlane_address(&hash);

        // Verify prefix
        assert_eq!(addr[0..4], [0x02, 0x00, 0x00, 0x00]);

        // Verify roundtrip
        let recovered = hyperlane_address_to_script_hash(&addr).unwrap();
        assert_eq!(recovered, hash);
    }

    #[test]
    fn test_hyperlane_address_to_script_hash_invalid() {
        // 0x01 prefix is now a valid policy ID prefix, not a script hash
        let mut addr: HyperlaneAddress = [0x00; 32];
        addr[0] = 0x01;
        assert!(hyperlane_address_to_script_hash(&addr).is_none());

        // Non-zero bytes in positions 1-3 also invalid
        let mut addr2: HyperlaneAddress = [0x00; 32];
        addr2[0] = 0x02;
        addr2[1] = 0x01; // Invalid: must be 0x02000000
        assert!(hyperlane_address_to_script_hash(&addr2).is_none());
    }

    #[test]
    fn test_policy_id_conversion() {
        let id: [u8; 28] = [0xab; 28];
        let addr = policy_id_to_hyperlane_address(&id);

        assert_eq!(addr[0..4], [0x01, 0x00, 0x00, 0x00]);

        let recovered = hyperlane_address_to_policy_id(&addr).unwrap();
        assert_eq!(recovered, id);
    }

    #[test]
    fn test_policy_id_h256_conversion() {
        let id: [u8; 28] = [0xcd; 28];
        let h = policy_id_to_h256(&id);

        assert_eq!(h.0[0], 0x01);
        assert_eq!(&h.0[4..32], &id);

        let recovered = h256_to_policy_id(&h).unwrap();
        assert_eq!(recovered, id);
    }

    #[test]
    fn test_policy_id_from_script_hash_returns_none() {
        let hash: ScriptHash = [0x12; 28];
        let addr = script_hash_to_hyperlane_address(&hash);
        // 0x02 prefix should not parse as policy ID
        assert!(hyperlane_address_to_policy_id(&addr).is_none());
    }

    #[test]
    fn test_warp_transfer_body_encoding() {
        let transfer = WarpTransferBody {
            recipient: vec![0x01, 0x02, 0x03, 0x04], // 4-byte recipient
            amount: 1_000_000,
        };

        let encoded = transfer.encode();
        assert_eq!(encoded.len(), 12); // 4 bytes recipient + 8 bytes amount

        // Decode and verify roundtrip
        let decoded = WarpTransferBody::decode(&encoded).unwrap();
        assert_eq!(decoded.recipient, transfer.recipient);
        assert_eq!(decoded.amount, transfer.amount);
    }

    #[test]
    fn test_warp_transfer_body_decode_invalid() {
        // Too short
        let short_body = vec![0x01, 0x02, 0x03];
        assert!(WarpTransferBody::decode(&short_body).is_none());
    }
}

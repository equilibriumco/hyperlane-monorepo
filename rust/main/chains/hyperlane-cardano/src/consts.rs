/// Hyperlane address prefix for policy-ID-based recipients (warp routes).
/// A 32-byte Hyperlane address with this leading byte encodes a 28-byte
/// Cardano minting policy ID in bytes 4..32.
pub const POLICY_ID_ADDR_PREFIX: u8 = 0x01;

/// Hyperlane address prefix for script-hash-based recipients (generic recipients).
/// A 32-byte Hyperlane address with this leading byte encodes a 28-byte
/// Cardano script hash in bytes 4..32.
pub const SCRIPT_HASH_ADDR_PREFIX: u8 = 0x02;

/// Minimum byte length of Hyperlane multisig ISM metadata.
///
/// Layout:
///   bytes  0-31 : origin merkle tree hook address
///   bytes 32-63 : signed checkpoint merkle root
///   bytes 64-67 : signed checkpoint index (u32 big-endian)
///   bytes 68+   : validator ECDSA signatures (ECDSA_SIG_LEN bytes each)
pub const MULTISIG_ISM_METADATA_MIN_LEN: usize = 68;

/// Byte length of one ECDSA secp256k1 signature as emitted by Hyperlane validators.
/// Format: r (32 bytes) || s (32 bytes) || v (1 byte, recovery ID + 27).
pub const ECDSA_SIG_LEN: usize = 65;

/// Byte length of a Hyperlane message ID (keccak256 hash).
pub const MESSAGE_ID_SIZE: usize = 32;

/// Byte length of a Cardano script hash / policy ID.
pub const SCRIPT_HASH_SIZE: usize = 28;

/// Cardano address header byte for a Type-7 enterprise script address on testnet.
/// Binary: 0111_0000 (type bits 0111, network tag 0 = testnet).
pub const CARDANO_SCRIPT_ADDR_TESTNET: u8 = 0x70;

/// Cardano address header byte for a Type-7 enterprise script address on mainnet.
/// Binary: 0111_0001 (type bits 0111, network tag 1 = mainnet).
pub const CARDANO_SCRIPT_ADDR_MAINNET: u8 = 0x71;

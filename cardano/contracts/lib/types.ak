use aiken/crypto.{Blake2b_224, Hash, VerificationKeyHash}
use aiken/primitive/bytearray

/// Core types for Hyperlane Cardano implementation
/// These types are shared across all contracts
/// Domain identifier (chain ID in Hyperlane terms)
/// Cardano mainnet: 2001
/// Cardano preview testnet: 2002
pub type Domain =
  Int

/// 32-byte addresses (Hyperlane standard)
/// For Cardano script hashes, left-pad with 4 bytes indicating credential type:
/// - 0x00000000: Payment credential
/// - 0x01000000: Minting policy
/// - 0x02000000: Validator/Script
pub type HyperlaneAddress =
  ByteArray

/// Policy ID for tokens
pub type PolicyId =
  Hash<Blake2b_224, ByteArray>

/// Asset name within a policy
pub type AssetName =
  ByteArray

/// Script hash type
pub type ScriptHash =
  Hash<Blake2b_224, ByteArray>

/// Message structure (matches Hyperlane spec)
/// This is the core data structure for cross-chain messages
pub type Message {
  /// Protocol version (currently 3)
  version: Int,
  /// Unique per-mailbox nonce
  nonce: Int,
  /// Source chain domain
  origin: Domain,
  /// Sender address on origin chain (32 bytes)
  sender: HyperlaneAddress,
  /// Destination chain domain (Cardano)
  destination: Domain,
  /// Recipient script hash (32 bytes, padded)
  recipient: HyperlaneAddress,
  /// Arbitrary message content
  body: ByteArray,
}

/// Encode a message to bytes for hashing
/// Format: version (1 byte) || nonce (4 bytes) || origin (4 bytes) || sender (32 bytes)
///         || destination (4 bytes) || recipient (32 bytes) || body (variable)
pub fn encode_message(msg: Message) -> ByteArray {
  let version_bytes = int_to_bytes(msg.version, 1)
  let nonce_bytes = int_to_bytes(msg.nonce, 4)
  let origin_bytes = int_to_bytes(msg.origin, 4)
  let destination_bytes = int_to_bytes(msg.destination, 4)

  version_bytes
    |> bytearray.concat(nonce_bytes)
    |> bytearray.concat(origin_bytes)
    |> bytearray.concat(msg.sender)
    |> bytearray.concat(destination_bytes)
    |> bytearray.concat(msg.recipient)
    |> bytearray.concat(msg.body)
}

/// Convert integer to big-endian bytes of specified length
pub fn int_to_bytes(n: Int, len: Int) -> ByteArray {
  int_to_bytes_helper(n, len, "")
}

fn int_to_bytes_helper(n: Int, remaining: Int, acc: ByteArray) -> ByteArray {
  if remaining <= 0 {
    acc
  } else {
    let byte = n % 256
    let rest = n / 256
    int_to_bytes_helper(rest, remaining - 1, bytearray.push(acc, byte))
  }
}

/// How to locate a UTXO (by NFT marker)
pub type UtxoLocator {
  policy_id: PolicyId,
  asset_name: AssetName,
}

/// Additional input required by recipient
pub type AdditionalInput {
  /// Human-readable identifier
  name: ByteArray,
  /// NFT marking the UTXO
  locator: UtxoLocator,
  /// True = must be spent, False = reference input only
  must_be_spent: Bool,
}

/// Recipient type determines how relayer constructs outputs
pub type RecipientType {
  /// State in, state out, no other effects
  Generic
  /// Mints/releases tokens to address specified in message body
  TokenReceiver {
    vault_locator: Option<UtxoLocator>,
    minting_policy: Option<ScriptHash>,
  }
  /// Defers message processing to a separate step
  /// The relayer creates a message UTXO with an NFT proving legitimacy
  /// Messages are processed separately (manually, via dApp, automated service, etc.)
  Deferred {
    /// Minting policy for message NFTs (proves message is legitimate)
    /// The policy only allows minting when mailbox is in inputs
    message_policy: ScriptHash,
  }
}

/// Registry entry for a recipient
pub type RecipientRegistration {
  /// Script hash of the recipient validator
  script_hash: ScriptHash,
  /// Owner who can modify/remove this registration
  /// This is the KEY security field - only this key can update the registration
  owner: VerificationKeyHash,
  /// NFT locator to find the state UTXO (contains datum)
  state_locator: UtxoLocator,
  /// NFT locator to find the reference script UTXO
  /// None = script is embedded in state UTXO's reference_script field
  reference_script_locator: Option<UtxoLocator>,
  additional_inputs: List<AdditionalInput>,
  recipient_type: RecipientType,
  /// Override default ISM for this recipient
  custom_ism: Option<ScriptHash>,
}

/// Standard wrapper for recipient datums
/// Recipients should use this pattern to be compatible with the relayer
pub type HyperlaneRecipientDatum<inner> {
  /// Custom ISM override
  ism: Option<ScriptHash>,
  /// For ordering (optional)
  last_processed_nonce: Option<Int>,
  /// Contract-specific state
  inner: inner,
}

/// Standard redeemer for recipients
///
/// SECURITY: Recipients MUST verify that keccak256(encode_message(message)) == message_id
/// This ensures the data was validated by the ISM (which signs the message_id).
/// Without this check, an attacker could provide fake origin/sender/body data.
pub type HyperlaneRecipientRedeemer<contract_redeemer> {
  /// Handle incoming Hyperlane message
  /// The recipient MUST verify: keccak256(encode_message(message)) == message_id
  HandleMessage {
    /// The full message structure (for verification)
    message: Message,
    /// The message ID (keccak256 hash of encoded message)
    /// This is what the ISM validated - recipient must verify message hashes to this
    message_id: ByteArray,
  }
  /// Contract-specific actions
  ContractAction { action: contract_redeemer }
}

// ============================================================================
// Deferred Recipient Types (for complex recipients with deferred processing)
// ============================================================================

/// Stored message datum for Deferred recipient pattern
/// This is stored in the message UTXO created by the relayer
/// The message is later processed and the message NFT burned
pub type StoredMessageDatum {
  /// Source chain domain
  origin: Domain,
  /// Sender address on origin chain (32 bytes)
  sender: HyperlaneAddress,
  /// Message payload
  body: ByteArray,
  /// Message ID (32 bytes) - used as NFT asset name
  message_id: ByteArray,
  /// Nonce from the original message (for ordering)
  nonce: Int,
}

/// Redeemer for message NFT minting policy
pub type MessageNftRedeemer {
  /// Mint a message NFT when storing a message
  /// Asset name = message_id (32 bytes)
  MintMessage
  /// Burn a message NFT when processing a stored message
  BurnMessage
}

/// Contract-specific redeemer for Deferred recipients
pub type DeferredAction {
  /// Process a stored message
  /// Consumes the message UTXO, burns the NFT, creates custom outputs
  ProcessStoredMessage {
    /// The message ID being processed (must match NFT asset name)
    message_id: ByteArray,
  }
}

// ============================================================================
// Mailbox Types
// ============================================================================

/// Merkle tree state stored in datum
/// Stores full branch state for incremental tree updates
/// This costs ~4.4 ADA more in minUTxO but enables proper on-chain merkle validation
pub type MerkleTreeState {
  /// Branch hashes at each level (32 branches, each 32 bytes)
  /// From leaf level to root level
  branches: List<ByteArray>,
  /// Number of leaves inserted
  count: Int,
}

/// Mailbox state stored in datum
pub type MailboxDatum {
  /// Cardano domain ID
  local_domain: Domain,
  /// Default ISM for message verification
  default_ism: ScriptHash,
  /// Owner who can update configuration
  owner: VerificationKeyHash,
  /// Next nonce for dispatched messages
  outbound_nonce: Int,
  /// Full merkle tree state (branches + count)
  merkle_tree: MerkleTreeState,
}

/// Mailbox redeemer actions
pub type MailboxRedeemer {
  /// Outbound: dispatch a message to another chain
  Dispatch { destination: Domain, recipient: HyperlaneAddress, body: ByteArray }
  /// Inbound: process a message from another chain
  Process { message: Message, metadata: ByteArray, message_id: ByteArray }
  /// Admin: set default ISM
  SetDefaultIsm { new_ism: ScriptHash }
  /// Admin: transfer ownership
  TransferOwnership { new_owner: VerificationKeyHash }
}

/// Processed message marker (separate UTXOs to avoid state bloat)
pub type ProcessedMessageDatum {
  message_id: ByteArray,
}

// ============================================================================
// ISM Types (Multisig ISM)
// ============================================================================

/// Multisig ISM configuration
pub type MultisigIsmDatum {
  /// Validators per origin domain: List of (domain, validators)
  validators: List<(Domain, List<ByteArray>)>,
  /// Threshold per origin domain: List of (domain, threshold)
  thresholds: List<(Domain, Int)>,
  /// Owner who can update configuration
  owner: VerificationKeyHash,
}

/// Checkpoint data that validators sign
/// Hyperlane validators sign checkpoints with this structure:
/// 1. domain_hash = keccak256(origin || merkle_tree_hook || "HYPERLANE")
/// 2. digest = keccak256(domain_hash || merkle_root || merkle_index || message_id)
/// 3. signed = EIP-191(digest)
pub type Checkpoint {
  /// Origin domain (same as message origin)
  origin: Domain,
  /// Merkle root of the message tree (32 bytes)
  merkle_root: ByteArray,
  /// Origin merkle tree hook address (32 bytes, typically the origin mailbox)
  /// This is used in the domain hash for validator signing
  origin_merkle_tree_hook: ByteArray,
  /// Merkle tree index (nonce of the message in the tree)
  merkle_index: Int,
  /// Message ID (32 bytes) - keccak256 hash of the message
  message_id: ByteArray,
}

/// Encode checkpoint domain hash
/// Format: origin (4 bytes big-endian) || merkle_tree_hook (32 bytes) || "HYPERLANE" (9 bytes)
pub fn encode_domain_hash(origin: Int, merkle_tree_hook: ByteArray) -> ByteArray {
  let origin_bytes = int_to_bytes(origin, 4)
  // "HYPERLANE" = 0x48595045524c414e45 (9 bytes)
  let hyperlane_suffix = #"48595045524c414e45"

  origin_bytes
    |> bytearray.concat(merkle_tree_hook)
    |> bytearray.concat(hyperlane_suffix)
}

/// Encode checkpoint for hashing (after domain hash is computed)
/// Format: domain_hash (32 bytes) || merkle_root (32 bytes) || merkle_index (4 bytes big-endian) || message_id (32 bytes)
pub fn encode_checkpoint(checkpoint: Checkpoint, domain_hash: ByteArray) -> ByteArray {
  let index_bytes = int_to_bytes(checkpoint.merkle_index, 4)

  domain_hash
    |> bytearray.concat(checkpoint.merkle_root)
    |> bytearray.concat(index_bytes)
    |> bytearray.concat(checkpoint.message_id)
}

/// Validator signature with recovered public key in both formats
/// The relayer recovers the public key off-chain and passes both formats for on-chain verification
pub type ValidatorSignature {
  /// Compressed public key (33 bytes: 0x02/0x03 prefix + x-coordinate)
  /// Used for verifyEcdsaSecp256k1Signature per CIP-49
  compressed_pubkey: ByteArray,
  /// Uncompressed public key (64 bytes: x || y, no 0x04 prefix)
  /// Used to compute Ethereum address on-chain: keccak256(pubkey)[12:32]
  uncompressed_pubkey: ByteArray,
  /// The 64-byte signature (r || s)
  signature: ByteArray,
}

/// Multisig ISM redeemer
pub type MultisigIsmRedeemer {
  /// Verification: verify validators signed the checkpoint
  /// The checkpoint commits to a merkle root containing the message
  ///
  /// Security model:
  /// 1. Relayer recovers public key from signature off-chain (using v/recovery ID)
  /// 2. ISM verifies the signature on-chain using verify_ecdsa_secp256k1_signature
  /// 3. ISM computes Ethereum address from the verified public key
  /// 4. ISM checks the address is in the trusted validators list
  ///
  /// This provides cryptographic binding: an attacker cannot forge a signature
  /// without the validator's private key.
  Verify {
    /// The checkpoint that validators signed
    checkpoint: Checkpoint,
    /// Validator signatures with recovered public keys
    validator_signatures: List<ValidatorSignature>,
  }
  /// Admin: set validators for a domain
  /// validators should be Ethereum addresses (20 bytes each)
  SetValidators { domain: Domain, validators: List<ByteArray> }
  /// Admin: set threshold for a domain
  SetThreshold { domain: Domain, threshold: Int }
}

// ============================================================================
// Registry Types
// ============================================================================

/// Registry datum
pub type RegistryDatum {
  registrations: List<RecipientRegistration>,
  /// Registry admin (can pause, upgrade, etc.)
  admin: VerificationKeyHash,
}

/// Registry redeemer actions
pub type RegistryRedeemer {
  /// Initial registration of a new recipient
  /// Requires: owner signature AND state NFT in inputs
  Register { registration: RecipientRegistration }

  /// Update an existing registration (owner only)
  /// Requires: current owner's signature
  UpdateRegistration {
    script_hash: ScriptHash,
    new_state_locator: Option<UtxoLocator>,
    new_reference_script_locator: Option<Option<UtxoLocator>>,
    new_additional_inputs: Option<List<AdditionalInput>>,
    new_recipient_type: Option<RecipientType>,
    new_custom_ism: Option<Option<ScriptHash>>,
  }

  /// Remove a registration (owner only)
  /// Requires: current owner's signature
  Unregister { script_hash: ScriptHash }

  /// Transfer registration ownership (both parties must sign)
  /// Requires: current owner AND new owner signatures
  TransferRegistrationOwnership {
    script_hash: ScriptHash,
    new_owner: VerificationKeyHash,
  }

  /// Admin-only registration (bypasses ownership check)
  /// Requires: registry admin signature
  AdminRegister { registration: RecipientRegistration }
}

// ============================================================================
// IGP Types (Interchain Gas Paymaster)
// ============================================================================

/// Gas oracle data per destination
pub type GasOracleConfig {
  /// Gas price in remote chain units
  gas_price: Int,
  /// Lovelace per remote gas unit (scaled)
  token_exchange_rate: Int,
}

/// IGP datum
pub type IgpDatum {
  owner: VerificationKeyHash,
  /// Address that receives collected fees
  beneficiary: ByteArray,
  /// Gas oracles per destination domain
  gas_oracles: List<(Domain, GasOracleConfig)>,
  /// Default gas limit for messages
  default_gas_limit: Int,
}

/// IGP redeemer
pub type IgpRedeemer {
  /// Pay for message gas
  PayForGas { message_id: ByteArray, destination: Domain, gas_amount: Int }
  /// Claim collected fees
  Claim { amount: Int }
  /// Admin: set gas oracle for a domain
  SetGasOracle { domain: Domain, config: GasOracleConfig }
}

// ============================================================================
// Warp Route Types (Token Bridge)
// ============================================================================

/// Warp token type
pub type WarpTokenType {
  /// Lock tokens in warp route UTXO, release on receive
  Collateral { policy_id: PolicyId, asset_name: AssetName }
  /// Mint synthetic tokens on receive, burn on send
  Synthetic { minting_policy: ScriptHash }
  /// Native ADA (locked in warp route UTXO)
  Native
}

/// Warp route configuration
pub type WarpRouteConfig {
  /// Token type being bridged
  token_type: WarpTokenType,
  /// Decimal places for local token on Cardano
  decimals: Int,
  /// Decimal places for remote token (wire format, typically 18 for EVM)
  remote_decimals: Int,
  /// Remote routes (other chains): List of (domain, route_address)
  remote_routes: List<(Domain, HyperlaneAddress)>,
}

/// Warp route datum
pub type WarpRouteDatum {
  config: WarpRouteConfig,
  owner: VerificationKeyHash,
  /// Total tokens bridged (for accounting)
  total_bridged: Int,
}

/// Warp route redeemer
pub type WarpRouteRedeemer {
  /// Send tokens to another chain
  TransferRemote {
    destination: Domain,
    recipient: HyperlaneAddress,
    amount: Int,
  }
  /// Receive tokens from another chain (via Hyperlane message)
  ReceiveTransfer { origin: Domain, sender: HyperlaneAddress, body: ByteArray }
  /// Admin: enroll a remote route
  EnrollRemoteRoute { domain: Domain, route: HyperlaneAddress }
}

/// Message body for warp transfers
pub type WarpTransferBody {
  /// Cardano address receiving tokens (bech32 or raw bytes)
  recipient: ByteArray,
  /// Amount of tokens
  amount: Int,
}

// ============================================================================
// Validator Announce Types
// ============================================================================

/// Validator announcement datum
/// Each validator creates a UTXO with this datum to announce their storage location
///
/// The validator_address is the 20-byte Ethereum address derived from the validator's
/// secp256k1 public key. This is the same address used to verify checkpoints on all
/// chains, enabling cross-chain validator discovery.
pub type ValidatorAnnounceDatum {
  /// Validator Ethereum address (20 bytes)
  /// Derived from secp256k1 pubkey: keccak256(uncompressed_pubkey)[12:32]
  /// This must match the address used for checkpoint signing
  validator_address: ByteArray,
  /// Mailbox policy ID this announcement applies to
  mailbox_policy_id: PolicyId,
  /// Mailbox domain (Cardano domain ID)
  mailbox_domain: Domain,
  /// Storage location URL (e.g., "s3://bucket/validator" or "file:///path")
  storage_location: ByteArray,
}

/// Validator announce redeemer
///
/// For announcements, the validator must provide an ECDSA secp256k1 signature
/// over the announcement digest. This proves they control the private key
/// corresponding to the announced Ethereum address.
pub type ValidatorAnnounceRedeemer {
  /// Create new announcement or update existing
  /// Requires ECDSA signature from the validator's secp256k1 key
  Announce {
    /// Storage location URL
    storage_location: ByteArray,
    /// Compressed secp256k1 public key (33 bytes: 0x02/0x03 prefix + x-coordinate)
    compressed_pubkey: ByteArray,
    /// Uncompressed secp256k1 public key (64 bytes: x || y, no 0x04 prefix)
    uncompressed_pubkey: ByteArray,
    /// ECDSA signature (64 bytes: r || s)
    signature: ByteArray,
  }
  /// Remove announcement (requires signature proving ownership)
  Revoke {
    /// Compressed secp256k1 public key (33 bytes)
    compressed_pubkey: ByteArray,
    /// Uncompressed secp256k1 public key (64 bytes)
    uncompressed_pubkey: ByteArray,
    /// ECDSA signature over revocation message
    signature: ByteArray,
  }
}

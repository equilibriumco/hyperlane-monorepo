//! CBOR encoding/decoding utilities for Cardano datums and redeemers

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

/// CBOR builder for Plutus data
pub struct CborBuilder {
    bytes: Vec<u8>,
}

impl CborBuilder {
    pub fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    /// Start an indefinite-length constructor (Constr n [...])
    pub fn start_constr(&mut self, index: u32) -> &mut Self {
        match index {
            0..=6 => {
                // Tags 121-127 for constructors 0-6
                self.bytes.push(0xd8);
                self.bytes.push((121 + index) as u8);
            }
            7..=127 => {
                // Tag 1280 + n for constructors 7-127
                self.bytes.push(0xd9);
                let tag = 1280 + index;
                self.bytes.extend_from_slice(&(tag as u16).to_be_bytes());
            }
            _ => {
                // General constructor with tag 102
                self.bytes.push(0xd8);
                self.bytes.push(102);
                self.start_list();
                self.uint(index as u64);
            }
        }
        self.bytes.push(0x9f); // indefinite array start
        self
    }

    /// End a constructor
    pub fn end_constr(&mut self) -> &mut Self {
        self.bytes.push(0xff); // break
        self
    }

    /// Start an indefinite-length list
    pub fn start_list(&mut self) -> &mut Self {
        self.bytes.push(0x9f);
        self
    }

    /// End a list
    pub fn end_list(&mut self) -> &mut Self {
        self.bytes.push(0xff);
        self
    }

    /// Add an unsigned integer
    pub fn uint(&mut self, n: u64) -> &mut Self {
        self.encode_uint(0, n);
        self
    }

    /// Add a signed integer
    pub fn int(&mut self, n: i64) -> &mut Self {
        if n >= 0 {
            self.encode_uint(0, n as u64);
        } else {
            self.encode_uint(1, (-1 - n) as u64);
        }
        self
    }

    /// Add a byte string from hex
    pub fn bytes_hex(&mut self, hex: &str) -> Result<&mut Self> {
        let data = hex::decode(hex).map_err(|e| anyhow!("Invalid hex: {}", e))?;
        self.encode_uint(2, data.len() as u64);
        self.bytes.extend_from_slice(&data);
        Ok(self)
    }

    /// Encode a major type with argument
    fn encode_uint(&mut self, major: u8, n: u64) {
        let major_bits = major << 5;
        match n {
            0..=23 => {
                self.bytes.push(major_bits | (n as u8));
            }
            24..=255 => {
                self.bytes.push(major_bits | 24);
                self.bytes.push(n as u8);
            }
            256..=65535 => {
                self.bytes.push(major_bits | 25);
                self.bytes.extend_from_slice(&(n as u16).to_be_bytes());
            }
            65536..=4294967295 => {
                self.bytes.push(major_bits | 26);
                self.bytes.extend_from_slice(&(n as u32).to_be_bytes());
            }
            _ => {
                self.bytes.push(major_bits | 27);
                self.bytes.extend_from_slice(&n.to_be_bytes());
            }
        }
    }

    /// Build and return the CBOR bytes
    pub fn build(self) -> Vec<u8> {
        self.bytes
    }
}

impl Default for CborBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a Mailbox datum with nested MerkleTreeState
///
/// Structure:
/// ```
/// MailboxDatum {
///   local_domain: Domain,
///   default_ism: ScriptHash,
///   owner: VerificationKeyHash,
///   outbound_nonce: Int,
///   merkle_tree: MerkleTreeState {
///     branches: List<ByteArray>,  // 32 branches, each 32 bytes
///     count: Int,
///   },
/// }
/// ```
pub fn build_mailbox_datum(
    local_domain: u32,
    default_ism_hash: &str,
    owner_pkh: &str,
    outbound_nonce: u32,
    branches: &[&str],  // 32 branch hashes (each 64 hex chars = 32 bytes)
    merkle_count: u32,
) -> Result<Vec<u8>> {
    let mut builder = CborBuilder::new();

    builder
        .start_constr(0)
        .uint(local_domain as u64);

    builder.bytes_hex(default_ism_hash)?;
    builder.bytes_hex(owner_pkh)?;

    builder.uint(outbound_nonce as u64);

    // MerkleTreeState { branches: List<ByteArray>, count: Int }
    builder.start_constr(0);

    // branches: List<ByteArray>
    builder.start_list();
    for branch in branches {
        builder.bytes_hex(branch)?;
    }
    builder.end_list();

    // count: Int
    builder.uint(merkle_count as u64);

    builder.end_constr(); // End MerkleTreeState

    builder.end_constr(); // End MailboxDatum

    Ok(builder.build())
}

/// Build an ISM datum
pub fn build_ism_datum(
    validators: &[(u32, Vec<String>)], // (domain, validator_addresses_hex)
    thresholds: &[(u32, u32)],         // (domain, threshold)
    owner_pkh: &str,
) -> Result<Vec<u8>> {
    let mut builder = CborBuilder::new();

    builder.start_constr(0);

    // Validators list: List<(Domain, List<Address>)>
    // NOTE: In Plutus/Aiken, tuples are encoded as plain CBOR arrays, NOT as Constr 0
    builder.start_list();
    for (domain, addrs) in validators {
        // Tuple is a plain array [domain, addrs], NOT Constr 0
        builder
            .start_list()
            .uint(*domain as u64)
            .start_list();
        for addr in addrs {
            builder.bytes_hex(addr)?;
        }
        builder.end_list().end_list();
    }
    builder.end_list();

    // Thresholds list: List<(Domain, Int)>
    builder.start_list();
    for (domain, threshold) in thresholds {
        // Tuple is a plain array [domain, threshold], NOT Constr 0
        builder
            .start_list()
            .uint(*domain as u64)
            .uint(*threshold as u64)
            .end_list();
    }
    builder.end_list();

    // Owner
    builder.bytes_hex(owner_pkh)?;

    builder.end_constr();

    Ok(builder.build())
}

/// Build a Registry datum
/// Note: admin_pkh is the registry admin, while each registration has its own owner
pub fn build_registry_datum(
    registrations: &[RegistrationData],
    admin_pkh: &str,
) -> Result<Vec<u8>> {
    let mut builder = CborBuilder::new();

    builder.start_constr(0);

    // Registrations list
    builder.start_list();
    for reg in registrations {
        builder.start_constr(0);

        // script_hash
        builder.bytes_hex(&reg.script_hash)?;

        // owner (VerificationKeyHash - the registration owner)
        builder.bytes_hex(&reg.owner)?;

        // state_locator (policy_id, asset_name)
        builder.start_constr(0);
        builder.bytes_hex(&reg.state_policy_id)?;
        builder.bytes_hex(&reg.state_asset_name)?;
        builder.end_constr();

        // reference_script_locator: Option<UtxoLocator>
        match (&reg.ref_script_policy_id, &reg.ref_script_asset_name) {
            (Some(policy), Some(asset)) => {
                // Some(locator) = Constr 0 [locator]
                builder.start_constr(0);
                builder.start_constr(0);
                builder.bytes_hex(policy)?;
                builder.bytes_hex(asset)?;
                builder.end_constr();
                builder.end_constr();
            }
            _ => {
                // None = Constr 1 []
                builder.start_constr(1).end_constr();
            }
        }

        // additional_inputs (empty list for now)
        builder.start_list().end_list();

        // recipient_type encoding:
        // - Generic = Constr 0 [] (empty)
        // - TokenReceiver = Constr 1 [vault_locator: Option, minting_policy: Option]
        // - Deferred = Constr 2 [message_policy: ScriptHash]
        match reg.recipient_type.to_lowercase().as_str() {
            "generic" => {
                builder.start_constr(0).end_constr();
            }
            "tokenreceiver" | "token-receiver" | "token_receiver" => {
                // TokenReceiver { vault_locator: Option<UtxoLocator>, minting_policy: Option<ScriptHash> }
                // For now, encode both as None
                builder.start_constr(1);
                builder.start_constr(1).end_constr(); // vault_locator: None
                builder.start_constr(1).end_constr(); // minting_policy: None
                builder.end_constr();
            }
            "deferred" => {
                // Deferred { message_policy: ScriptHash }
                builder.start_constr(2);
                if let Some(msg_policy) = &reg.deferred_message_policy {
                    builder.bytes_hex(msg_policy)?;
                } else {
                    return Err(anyhow!("Deferred recipient requires message_policy (deferred_message_policy field)"));
                }
                builder.end_constr();
            }
            _ => {
                builder.start_constr(0).end_constr(); // Default to Generic
            }
        }

        // custom_ism: Option<ScriptHash>
        match &reg.custom_ism {
            Some(ism_hash) => {
                // Some = constructor 0
                builder.start_constr(0);
                builder.bytes_hex(ism_hash)?;
                builder.end_constr();
            }
            None => {
                // None = constructor 1
                builder.start_constr(1).end_constr();
            }
        }

        builder.end_constr();
    }
    builder.end_list();

    // Admin (registry admin, not registration owner)
    builder.bytes_hex(admin_pkh)?;

    builder.end_constr();

    Ok(builder.build())
}

/// Registration data for building registry datums
#[derive(Clone)]
pub struct RegistrationData {
    pub script_hash: String,
    /// Owner who can modify/remove this registration (verification key hash, 28 bytes hex)
    pub owner: String,
    pub state_policy_id: String,
    pub state_asset_name: String,
    /// Reference script locator (optional)
    /// If Some, contains (policy_id, asset_name) for the reference script NFT
    pub ref_script_policy_id: Option<String>,
    pub ref_script_asset_name: Option<String>,
    /// Recipient type: "Generic" (0), "TokenReceiver" (1), or "Deferred" (2)
    pub recipient_type: String,
    /// Custom ISM script hash (optional)
    pub custom_ism: Option<String>,
    /// For Deferred recipients: the message NFT minting policy
    /// Required when recipient_type is "Deferred"
    pub deferred_message_policy: Option<String>,
}

/// Build a mint redeemer (empty constructor 0)
pub fn build_mint_redeemer() -> Vec<u8> {
    let mut builder = CborBuilder::new();
    builder.start_constr(0).end_constr();
    builder.build()
}

/// Build a Registry AdminRegister redeemer (admin-only, bypasses script ownership check)
/// Redeemer: AdminRegister { registration: RecipientRegistration }
/// AdminRegister is constructor 4 in RegistryRedeemer
pub fn build_registry_admin_register_redeemer(reg: &RegistrationData) -> Result<Vec<u8>> {
    let mut builder = CborBuilder::new();

    // AdminRegister is constructor 4 (Register=0, UpdateRegistration=1, Unregister=2, TransferRegistrationOwnership=3, AdminRegister=4)
    builder.start_constr(4);

    // RecipientRegistration structure
    builder.start_constr(0);

    // script_hash
    builder.bytes_hex(&reg.script_hash)?;

    // owner (VerificationKeyHash)
    builder.bytes_hex(&reg.owner)?;

    // state_locator (policy_id, asset_name)
    builder.start_constr(0);
    builder.bytes_hex(&reg.state_policy_id)?;
    builder.bytes_hex(&reg.state_asset_name)?;
    builder.end_constr();

    // reference_script_locator: Option<UtxoLocator>
    match (&reg.ref_script_policy_id, &reg.ref_script_asset_name) {
        (Some(policy), Some(asset)) => {
            // Some(locator) = Constr 0 [locator]
            builder.start_constr(0);
            builder.start_constr(0);
            builder.bytes_hex(policy)?;
            builder.bytes_hex(asset)?;
            builder.end_constr();
            builder.end_constr();
        }
        _ => {
            // None = Constr 1 []
            builder.start_constr(1).end_constr();
        }
    }

    // additional_inputs (empty list)
    builder.start_list().end_list();

    // recipient_type encoding:
    // - Generic = Constr 0 [] (empty)
    // - TokenReceiver = Constr 1 [vault_locator: Option, minting_policy: Option]
    // - Deferred = Constr 2 [message_policy: ScriptHash]
    match reg.recipient_type.to_lowercase().as_str() {
        "generic" => {
            builder.start_constr(0).end_constr();
        }
        "tokenreceiver" | "token-receiver" | "token_receiver" => {
            builder.start_constr(1);
            builder.start_constr(1).end_constr(); // vault_locator: None
            builder.start_constr(1).end_constr(); // minting_policy: None
            builder.end_constr();
        }
        "deferred" => {
            builder.start_constr(2);
            if let Some(msg_policy) = &reg.deferred_message_policy {
                builder.bytes_hex(msg_policy)?;
            } else {
                return Err(anyhow!("Deferred recipient requires message_policy (deferred_message_policy field)"));
            }
            builder.end_constr();
        }
        _ => {
            builder.start_constr(0).end_constr();
        }
    }

    // custom_ism: Option<ScriptHash>
    match &reg.custom_ism {
        Some(ism_hash) => {
            builder.start_constr(0);
            builder.bytes_hex(ism_hash)?;
            builder.end_constr();
        }
        None => {
            builder.start_constr(1).end_constr();
        }
    }

    builder.end_constr(); // End RecipientRegistration
    builder.end_constr(); // End AdminRegister

    Ok(builder.build())
}

/// Build a Registry Unregister redeemer
/// Redeemer: Unregister { script_hash: ScriptHash }
/// Unregister is constructor 2 (Register=0, UpdateRegistration=1, Unregister=2, TransferRegistrationOwnership=3, AdminRegister=4)
pub fn build_registry_unregister_redeemer(script_hash: &str) -> Result<Vec<u8>> {
    let mut builder = CborBuilder::new();

    // Unregister is constructor 2
    builder.start_constr(2);
    builder.bytes_hex(script_hash)?;
    builder.end_constr();

    Ok(builder.build())
}

/// Build a Mailbox SetDefaultIsm redeemer
/// Redeemer: SetDefaultIsm { new_ism: ScriptHash }
/// SetDefaultIsm is constructor index 2 in MailboxRedeemer
pub fn build_mailbox_set_default_ism_redeemer(new_ism_hash: &str) -> Result<Vec<u8>> {
    let mut builder = CborBuilder::new();

    // SetDefaultIsm is constructor 2
    builder.start_constr(2);
    builder.bytes_hex(new_ism_hash)?;
    builder.end_constr();

    Ok(builder.build())
}

/// Build a Mailbox Dispatch redeemer
/// Redeemer: Dispatch { destination: Domain, recipient: HyperlaneAddress, body: ByteArray }
/// Dispatch is constructor index 0 in MailboxRedeemer
pub fn build_mailbox_dispatch_redeemer(
    destination: u32,
    recipient_hex: &str, // 32 bytes hex (64 chars)
    body_hex: &str,      // variable length hex
) -> Result<Vec<u8>> {
    let mut builder = CborBuilder::new();

    // Dispatch is constructor 0
    builder.start_constr(0);
    builder.uint(destination as u64);
    builder.bytes_hex(recipient_hex)?;
    builder.bytes_hex(body_hex)?;
    builder.end_constr();

    Ok(builder.build())
}

/// Build a GenericRecipient datum
/// Structure: HyperlaneRecipientDatum<GenericRecipientInner>
/// HyperlaneRecipientDatum { ism: Option<ScriptHash>, last_processed_nonce: Option<Int>, inner: GenericRecipientInner }
/// GenericRecipientInner { messages_received: Int, last_message: Option<ByteArray> }
pub fn build_generic_recipient_datum(
    custom_ism: Option<&str>,
    messages_received: u32,
) -> Result<Vec<u8>> {
    let mut builder = CborBuilder::new();

    builder.start_constr(0);

    // ism: Option<ScriptHash>
    match custom_ism {
        Some(ism_hash) => {
            // Some = constructor 0 with value
            builder.start_constr(0);
            builder.bytes_hex(ism_hash)?;
            builder.end_constr();
        }
        None => {
            // None = constructor 1 with no fields
            builder.start_constr(1).end_constr();
        }
    }

    // last_processed_nonce: Option<Int> - start with None
    builder.start_constr(1).end_constr();

    // inner: GenericRecipientInner
    builder.start_constr(0);
    builder.uint(messages_received as u64); // messages_received: Int
    builder.start_constr(1).end_constr(); // last_message: None
    builder.end_constr();

    builder.end_constr();

    Ok(builder.build())
}

/// Build a DeferredRecipient datum for initialization
/// Structure: HyperlaneRecipientDatum<DeferredInner>
/// HyperlaneRecipientDatum { ism: Option<ScriptHash>, last_processed_nonce: Option<Int>, inner: DeferredInner }
/// DeferredInner { messages_stored: Int, messages_processed: Int }
pub fn build_deferred_recipient_datum(
    custom_ism: Option<&str>,
    messages_stored: u64,
    messages_processed: u64,
) -> Result<Vec<u8>> {
    let mut builder = CborBuilder::new();

    builder.start_constr(0);

    // ism: Option<ScriptHash>
    match custom_ism {
        Some(ism_hash) => {
            // Some = constructor 0 with value
            builder.start_constr(0);
            builder.bytes_hex(ism_hash)?;
            builder.end_constr();
        }
        None => {
            // None = constructor 1 with no fields
            builder.start_constr(1).end_constr();
        }
    }

    // last_processed_nonce: Option<Int> - start with None
    builder.start_constr(1).end_constr();

    // inner: DeferredInner = constructor 0 [messages_stored, messages_processed]
    builder.start_constr(0);
    builder.uint(messages_stored);
    builder.uint(messages_processed);
    builder.end_constr();

    builder.end_constr();

    Ok(builder.build())
}

// =============================================================================
// CBOR Decoder for Plutus Data
// =============================================================================

/// Decode CBOR hex to Cardano JSON datum format
/// This handles the Plutus data encoding used by Blockfrost
pub fn decode_plutus_datum(cbor_hex: &str) -> Result<Value> {
    let bytes = hex::decode(cbor_hex).map_err(|e| anyhow!("Invalid hex: {}", e))?;
    let mut decoder = CborDecoder::new(&bytes);
    decoder.decode_value()
}

struct CborDecoder<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> CborDecoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn peek(&self) -> Result<u8> {
        self.bytes
            .get(self.pos)
            .copied()
            .ok_or_else(|| anyhow!("Unexpected end of CBOR data"))
    }

    fn read_byte(&mut self) -> Result<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Ok(b)
    }

    fn read_bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.bytes.len() {
            return Err(anyhow!("Unexpected end of CBOR data"));
        }
        let slice = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    fn read_uint(&mut self, additional: u8) -> Result<u64> {
        match additional {
            0..=23 => Ok(additional as u64),
            24 => Ok(self.read_byte()? as u64),
            25 => {
                let bytes = self.read_bytes(2)?;
                Ok(u16::from_be_bytes([bytes[0], bytes[1]]) as u64)
            }
            26 => {
                let bytes = self.read_bytes(4)?;
                Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as u64)
            }
            27 => {
                let bytes = self.read_bytes(8)?;
                Ok(u64::from_be_bytes([
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                ]))
            }
            _ => Err(anyhow!("Invalid CBOR additional info: {}", additional)),
        }
    }

    fn decode_value(&mut self) -> Result<Value> {
        let initial = self.read_byte()?;
        let major = initial >> 5;
        let additional = initial & 0x1f;

        match major {
            0 => {
                // Unsigned integer
                let n = self.read_uint(additional)?;
                Ok(json!({"int": n}))
            }
            1 => {
                // Negative integer
                let n = self.read_uint(additional)?;
                let value = -1i64 - (n as i64);
                Ok(json!({"int": value}))
            }
            2 => {
                // Byte string
                let len = self.read_uint(additional)? as usize;
                let bytes = self.read_bytes(len)?;
                Ok(json!({"bytes": hex::encode(bytes)}))
            }
            3 => {
                // Text string
                let len = self.read_uint(additional)? as usize;
                let bytes = self.read_bytes(len)?;
                let text = String::from_utf8_lossy(bytes);
                Ok(json!({"string": text}))
            }
            4 => {
                // Array
                if additional == 31 {
                    // Indefinite-length array
                    let mut items = Vec::new();
                    loop {
                        if self.peek()? == 0xff {
                            self.read_byte()?; // consume break
                            break;
                        }
                        items.push(self.decode_value()?);
                    }
                    Ok(json!({"list": items}))
                } else {
                    let len = self.read_uint(additional)? as usize;
                    let mut items = Vec::new();
                    for _ in 0..len {
                        items.push(self.decode_value()?);
                    }
                    Ok(json!({"list": items}))
                }
            }
            5 => {
                // Map
                if additional == 31 {
                    // Indefinite-length map
                    let mut items = Vec::new();
                    loop {
                        if self.peek()? == 0xff {
                            self.read_byte()?;
                            break;
                        }
                        let key = self.decode_value()?;
                        let value = self.decode_value()?;
                        items.push(json!({"k": key, "v": value}));
                    }
                    Ok(json!({"map": items}))
                } else {
                    let len = self.read_uint(additional)? as usize;
                    let mut items = Vec::new();
                    for _ in 0..len {
                        let key = self.decode_value()?;
                        let value = self.decode_value()?;
                        items.push(json!({"k": key, "v": value}));
                    }
                    Ok(json!({"map": items}))
                }
            }
            6 => {
                // Tag - this is where Plutus constructors are encoded
                let tag = self.read_uint(additional)?;
                self.decode_tagged(tag)
            }
            7 => {
                // Simple values
                match additional {
                    20 => Ok(json!(false)),
                    21 => Ok(json!(true)),
                    22 => Ok(json!(null)),
                    _ => Err(anyhow!("Unsupported simple value: {}", additional)),
                }
            }
            _ => Err(anyhow!("Invalid CBOR major type: {}", major)),
        }
    }

    fn decode_tagged(&mut self, tag: u64) -> Result<Value> {
        match tag {
            // Plutus constructor tags 121-127 map to constructors 0-6
            121..=127 => {
                let constructor = tag - 121;
                let fields = self.decode_constructor_fields()?;
                Ok(json!({"constructor": constructor, "fields": fields}))
            }
            // Plutus constructor tags 1280-1400 map to constructors 7-127
            1280..=1400 => {
                let constructor = tag - 1280 + 7;
                let fields = self.decode_constructor_fields()?;
                Ok(json!({"constructor": constructor, "fields": fields}))
            }
            // General constructor (tag 102)
            102 => {
                // Format: [constructor_index, fields...]
                let initial = self.read_byte()?;
                let major = initial >> 5;
                let additional = initial & 0x1f;

                if major != 4 {
                    return Err(anyhow!("Expected array for tag 102 constructor"));
                }

                let items = if additional == 31 {
                    let mut items = Vec::new();
                    loop {
                        if self.peek()? == 0xff {
                            self.read_byte()?;
                            break;
                        }
                        items.push(self.decode_value()?);
                    }
                    items
                } else {
                    let len = self.read_uint(additional)? as usize;
                    let mut items = Vec::new();
                    for _ in 0..len {
                        items.push(self.decode_value()?);
                    }
                    items
                };

                if items.is_empty() {
                    return Err(anyhow!("Tag 102 constructor requires at least index"));
                }

                let constructor = items[0]
                    .get("int")
                    .and_then(|i| i.as_u64())
                    .ok_or_else(|| anyhow!("Invalid constructor index"))?;

                let fields: Vec<Value> = items.into_iter().skip(1).collect();
                Ok(json!({"constructor": constructor, "fields": fields}))
            }
            _ => {
                // Unknown tag, decode the content
                let content = self.decode_value()?;
                Ok(json!({"tag": tag, "content": content}))
            }
        }
    }

    fn decode_constructor_fields(&mut self) -> Result<Vec<Value>> {
        let initial = self.read_byte()?;
        let major = initial >> 5;
        let additional = initial & 0x1f;

        if major != 4 {
            return Err(anyhow!(
                "Expected array for constructor fields, got major {}",
                major
            ));
        }

        if additional == 31 {
            // Indefinite-length array
            let mut items = Vec::new();
            loop {
                if self.peek()? == 0xff {
                    self.read_byte()?;
                    break;
                }
                items.push(self.decode_value()?);
            }
            Ok(items)
        } else {
            let len = self.read_uint(additional)? as usize;
            let mut items = Vec::new();
            for _ in 0..len {
                items.push(self.decode_value()?);
            }
            Ok(items)
        }
    }
}

/// Try to normalize a datum value from Blockfrost
/// If it's a hex string, decode it as CBOR; otherwise return as-is
pub fn normalize_datum(datum: &Value) -> Result<Value> {
    if let Some(hex_str) = datum.as_str() {
        // It's a hex-encoded CBOR string
        decode_plutus_datum(hex_str)
    } else {
        // It's already a JSON object
        Ok(datum.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cbor_uint() {
        let mut builder = CborBuilder::new();
        builder.uint(0);
        assert_eq!(builder.build(), vec![0x00]);

        let mut builder = CborBuilder::new();
        builder.uint(23);
        assert_eq!(builder.build(), vec![0x17]);

        let mut builder = CborBuilder::new();
        builder.uint(24);
        assert_eq!(builder.build(), vec![0x18, 0x18]);

        let mut builder = CborBuilder::new();
        builder.uint(1000);
        assert_eq!(builder.build(), vec![0x19, 0x03, 0xe8]);
    }

    #[test]
    fn test_cbor_constr() {
        let mut builder = CborBuilder::new();
        builder.start_constr(0).uint(42).end_constr();
        let result = builder.build();
        // d8 79 9f 18 2a ff
        assert_eq!(result, vec![0xd8, 0x79, 0x9f, 0x18, 0x2a, 0xff]);
    }

    #[test]
    fn test_mint_redeemer() {
        let redeemer = build_mint_redeemer();
        // Constructor 0 with empty fields
        assert_eq!(redeemer, vec![0xd8, 0x79, 0x9f, 0xff]);
    }

    #[test]
    fn test_decode_plutus_datum() {
        // Constructor 0 with int 42: d8 79 9f 18 2a ff
        let datum = decode_plutus_datum("d8799f182aff").unwrap();
        assert_eq!(datum["constructor"], 0);
        assert_eq!(datum["fields"][0]["int"], 42);
    }
}

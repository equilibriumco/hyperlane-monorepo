//! CBOR encoding utilities for Cardano datums and redeemers

use anyhow::{anyhow, Result};

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

    /// Start an indefinite-length map
    pub fn start_map(&mut self) -> &mut Self {
        self.bytes.push(0xbf);
        self
    }

    /// End a map
    pub fn end_map(&mut self) -> &mut Self {
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

    /// Add a byte string
    pub fn bytes(&mut self, data: &[u8]) -> &mut Self {
        self.encode_uint(2, data.len() as u64);
        self.bytes.extend_from_slice(data);
        self
    }

    /// Add a byte string from hex
    pub fn bytes_hex(&mut self, hex: &str) -> Result<&mut Self> {
        let data = hex::decode(hex).map_err(|e| anyhow!("Invalid hex: {}", e))?;
        self.encode_uint(2, data.len() as u64);
        self.bytes.extend_from_slice(&data);
        Ok(self)
    }

    /// Add a text string
    pub fn text(&mut self, s: &str) -> &mut Self {
        self.encode_uint(3, s.len() as u64);
        self.bytes.extend_from_slice(s.as_bytes());
        self
    }

    /// Add raw CBOR bytes
    pub fn raw(&mut self, data: &[u8]) -> &mut Self {
        self.bytes.extend_from_slice(data);
        self
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

    /// Build and return as hex string
    pub fn build_hex(self) -> String {
        hex::encode(self.build())
    }
}

impl Default for CborBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a Mailbox datum
pub fn build_mailbox_datum(
    local_domain: u32,
    default_ism_hash: &str,
    owner_pkh: &str,
    outbound_nonce: u32,
    merkle_root: &str,
    merkle_count: u32,
) -> Result<Vec<u8>> {
    let mut builder = CborBuilder::new();

    builder
        .start_constr(0)
        .uint(local_domain as u64);

    builder.bytes_hex(default_ism_hash)?;
    builder.bytes_hex(owner_pkh)?;

    builder
        .uint(outbound_nonce as u64);

    builder.bytes_hex(merkle_root)?;

    builder
        .uint(merkle_count as u64)
        .end_constr();

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

/// Build an ISM SetValidators redeemer
pub fn build_ism_set_validators_redeemer(
    domain: u32,
    validators: &[String], // 20-byte validator addresses as hex
) -> Result<Vec<u8>> {
    let mut builder = CborBuilder::new();

    // SetValidators { domain: Domain, validators: List<ByteArray> }
    builder
        .start_constr(1) // SetValidators is constructor 1
        .uint(domain as u64)
        .start_list();

    for validator in validators {
        builder.bytes_hex(validator)?;
    }

    builder.end_list().end_constr();

    Ok(builder.build())
}

/// Build an ISM SetThreshold redeemer
pub fn build_ism_set_threshold_redeemer(domain: u32, threshold: u32) -> Vec<u8> {
    let mut builder = CborBuilder::new();

    builder
        .start_constr(2) // SetThreshold is constructor 2
        .uint(domain as u64)
        .uint(threshold as u64)
        .end_constr();

    builder.build()
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

        // recipient_type (GenericHandler = 0)
        builder.start_constr(0).end_constr();

        // custom_ism (None)
        builder.start_constr(1).end_constr();

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

    // recipient_type (GenericHandler = 0)
    builder.start_constr(0).end_constr();

    // custom_ism (None = constructor 1)
    builder.start_constr(1).end_constr();

    builder.end_constr(); // End RecipientRegistration
    builder.end_constr(); // End AdminRegister

    Ok(builder.build())
}

/// Build a Registry Register redeemer
/// Redeemer: Register { registration: RecipientRegistration }
pub fn build_registry_register_redeemer(reg: &RegistrationData) -> Result<Vec<u8>> {
    let mut builder = CborBuilder::new();

    // Register is constructor 0
    builder.start_constr(0);

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

    // recipient_type (GenericHandler = 0)
    builder.start_constr(0).end_constr();

    // custom_ism (None = constructor 1)
    builder.start_constr(1).end_constr();

    builder.end_constr(); // End RecipientRegistration
    builder.end_constr(); // End Register

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
    fn test_cbor_bytes() {
        let mut builder = CborBuilder::new();
        builder.bytes(&[1, 2, 3]);
        assert_eq!(builder.build(), vec![0x43, 0x01, 0x02, 0x03]);
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
}

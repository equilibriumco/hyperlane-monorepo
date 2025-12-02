//! Common types for the Cardano CLI

use serde::{Deserialize, Serialize};

/// Cardano network
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardanoNetwork {
    Mainnet,
    Preprod,
    Preview,
}

impl CardanoNetwork {
    pub fn as_str(&self) -> &'static str {
        match self {
            CardanoNetwork::Mainnet => "mainnet",
            CardanoNetwork::Preprod => "preprod",
            CardanoNetwork::Preview => "preview",
        }
    }
}

/// UTXO reference
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UtxoRef {
    pub tx_hash: String,
    pub output_index: u32,
}

impl UtxoRef {
    pub fn new(tx_hash: String, output_index: u32) -> Self {
        Self { tx_hash, output_index }
    }

    pub fn to_string(&self) -> String {
        format!("{}#{}", self.tx_hash, self.output_index)
    }

    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split('#').collect();
        if parts.len() == 2 {
            let output_index = parts[1].parse().ok()?;
            Some(Self {
                tx_hash: parts[0].to_string(),
                output_index,
            })
        } else {
            None
        }
    }
}

/// UTXO with value information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Utxo {
    pub tx_hash: String,
    pub output_index: u32,
    pub address: String,
    pub lovelace: u64,
    pub assets: Vec<Asset>,
    pub datum_hash: Option<String>,
    pub inline_datum: Option<serde_json::Value>,
    pub reference_script: Option<String>,
}

impl Utxo {
    pub fn utxo_ref(&self) -> UtxoRef {
        UtxoRef::new(self.tx_hash.clone(), self.output_index)
    }
}

/// Native asset
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub policy_id: String,
    pub asset_name: String,
    pub quantity: u64,
}

impl Asset {
    pub fn unit(&self) -> String {
        if self.asset_name.is_empty() {
            self.policy_id.clone()
        } else {
            format!("{}{}", self.policy_id, self.asset_name)
        }
    }
}

/// Script information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptInfo {
    pub hash: String,
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utxo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_nft_policy: Option<String>,
}

/// Deployment information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentInfo {
    pub network: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tx_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mailbox: Option<ScriptInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ism: Option<ScriptInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry: Option<ScriptInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub igp: Option<ScriptInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validator_announce: Option<ScriptInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warp_route: Option<ScriptInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vault: Option<ScriptInfo>,
}

impl DeploymentInfo {
    pub fn new(network: &str) -> Self {
        Self {
            network: network.to_string(),
            tx_id: None,
            mailbox: None,
            ism: None,
            registry: None,
            igp: None,
            validator_announce: None,
            warp_route: None,
            vault: None,
        }
    }
}

/// Multisig ISM datum structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultisigIsmDatum {
    /// Validators per domain: (domain_id, list of 20-byte validator addresses)
    pub validators: Vec<(u32, Vec<String>)>,
    /// Threshold per domain: (domain_id, threshold)
    pub thresholds: Vec<(u32, u32)>,
    /// Owner public key hash (28 bytes hex)
    pub owner: String,
}

/// Mailbox datum structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailboxDatum {
    pub local_domain: u32,
    pub default_ism: String, // script hash hex
    pub owner: String,       // pkh hex
    pub outbound_nonce: u32,
    pub merkle_root: String, // 32 bytes hex
    pub merkle_count: u32,
}

/// Registry recipient info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipientInfo {
    pub script_hash: String,
    pub state_policy_id: String,
    pub state_asset_name: String,
    pub recipient_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_ism: Option<String>,
    /// Reference script NFT policy ID (for reference script UTXO lookup)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ref_script_policy_id: Option<String>,
    /// Reference script NFT asset name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ref_script_asset_name: Option<String>,
}

/// Protocol parameters (subset we need)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolParams {
    pub tx_fee_per_byte: u64,
    pub tx_fee_fixed: u64,
    pub min_utxo_lovelace: u64,
    pub coins_per_utxo_byte: u64,
    pub collateral_percentage: u32,
    pub max_collateral_inputs: u32,
    pub max_tx_size: u32,
}

impl Default for ProtocolParams {
    fn default() -> Self {
        // Preview testnet defaults
        Self {
            tx_fee_per_byte: 44,
            tx_fee_fixed: 155381,
            min_utxo_lovelace: 1_000_000,
            coins_per_utxo_byte: 4310,
            collateral_percentage: 150,
            max_collateral_inputs: 3,
            max_tx_size: 16384,
        }
    }
}

/// Transaction submission result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxSubmitResult {
    pub tx_hash: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Relayer configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayerConfig {
    pub network: String,
    pub blockfrost_url: String,
    pub mailbox_policy_id: String,
    pub ism_policy_id: String,
    pub registry_policy_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub igp_policy_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validator_announce_policy_id: Option<String>,
}

//! CLI context and global configuration

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

use super::crypto::Keypair;
use super::plutus::PlutusBlueprint;
use super::types::{CardanoNetwork, DeploymentInfo};

/// CLI execution context
#[derive(Clone)]
pub struct CliContext {
    /// Cardano network
    pub network: CardanoNetwork,
    /// Blockfrost API key
    pub api_key: Option<String>,
    /// Signing keypair (loaded lazily)
    signing_key_path: Option<PathBuf>,
    /// Path to deployments directory
    pub deployments_dir: PathBuf,
    /// Path to contracts directory
    pub contracts_dir: PathBuf,
}

impl CliContext {
    pub fn new(
        network: &str,
        api_key: Option<&str>,
        signing_key: Option<&str>,
        deployments_dir: &str,
        contracts_dir: &str,
    ) -> Result<Self> {
        let network = match network.to_lowercase().as_str() {
            "mainnet" => CardanoNetwork::Mainnet,
            "preprod" => CardanoNetwork::Preprod,
            "preview" => CardanoNetwork::Preview,
            _ => return Err(anyhow!("Invalid network: {}. Use mainnet, preprod, or preview", network)),
        };

        Ok(Self {
            network,
            api_key: api_key.map(String::from),
            signing_key_path: signing_key.map(PathBuf::from),
            deployments_dir: PathBuf::from(deployments_dir),
            contracts_dir: PathBuf::from(contracts_dir),
        })
    }

    /// Get the Blockfrost API URL for this network
    pub fn blockfrost_url(&self) -> &str {
        match self.network {
            CardanoNetwork::Mainnet => "https://cardano-mainnet.blockfrost.io/api/v0",
            CardanoNetwork::Preprod => "https://cardano-preprod.blockfrost.io/api/v0",
            CardanoNetwork::Preview => "https://cardano-preview.blockfrost.io/api/v0",
        }
    }

    /// Get the network magic number
    pub fn network_magic(&self) -> u32 {
        match self.network {
            CardanoNetwork::Mainnet => 764824073,
            CardanoNetwork::Preprod => 1,
            CardanoNetwork::Preview => 2,
        }
    }

    /// Get pallas network type
    pub fn pallas_network(&self) -> pallas_addresses::Network {
        match self.network {
            CardanoNetwork::Mainnet => pallas_addresses::Network::Mainnet,
            CardanoNetwork::Preprod | CardanoNetwork::Preview => pallas_addresses::Network::Testnet,
        }
    }

    /// Get network ID for transaction building (0 = testnet, 1 = mainnet)
    pub fn network_id(&self) -> u8 {
        match self.network {
            CardanoNetwork::Mainnet => 1,
            CardanoNetwork::Preprod | CardanoNetwork::Preview => 0,
        }
    }

    /// Require an API key (error if not set)
    pub fn require_api_key(&self) -> Result<&str> {
        self.api_key
            .as_deref()
            .ok_or_else(|| anyhow!("Blockfrost API key required. Set --api-key or BLOCKFROST_API_KEY"))
    }

    /// Load the signing keypair
    pub fn load_signing_key(&self) -> Result<Keypair> {
        let path = self
            .signing_key_path
            .as_ref()
            .ok_or_else(|| anyhow!("Signing key required. Set --signing-key or CARDANO_SIGNING_KEY"))?;

        Keypair::from_file(path)
    }

    /// Load signing key from a specific path
    pub fn load_signing_key_from(&self, path: &Path) -> Result<Keypair> {
        Keypair::from_file(path)
    }

    /// Get path to network-specific deployment directory
    pub fn network_deployments_dir(&self) -> PathBuf {
        self.deployments_dir.join(self.network.as_str())
    }

    /// Ensure deployment directory exists
    pub fn ensure_deployments_dir(&self) -> Result<PathBuf> {
        let dir = self.network_deployments_dir();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create deployments directory: {:?}", dir))?;
        Ok(dir)
    }

    /// Load deployment info for current network
    pub fn load_deployment_info(&self) -> Result<DeploymentInfo> {
        let path = self.network_deployments_dir().join("deployment_info.json");
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read deployment info from {:?}", path))?;
        serde_json::from_str(&content)
            .with_context(|| "Failed to parse deployment info")
    }

    /// Save deployment info for current network
    pub fn save_deployment_info(&self, info: &DeploymentInfo) -> Result<()> {
        let dir = self.ensure_deployments_dir()?;
        let path = dir.join("deployment_info.json");
        let content = serde_json::to_string_pretty(info)?;
        std::fs::write(&path, content)
            .with_context(|| format!("Failed to write deployment info to {:?}", path))?;
        Ok(())
    }

    /// Get path to plutus.json
    pub fn plutus_json_path(&self) -> PathBuf {
        self.contracts_dir.join("plutus.json")
    }

    /// Load the Plutus blueprint (plutus.json)
    pub fn load_blueprint(&self) -> Result<PlutusBlueprint> {
        PlutusBlueprint::from_file(&self.plutus_json_path())
    }

    /// Get the CardanoScan URL for a transaction
    pub fn explorer_tx_url(&self, tx_hash: &str) -> String {
        match self.network {
            CardanoNetwork::Mainnet => format!("https://cardanoscan.io/transaction/{}", tx_hash),
            CardanoNetwork::Preprod => format!("https://preprod.cardanoscan.io/transaction/{}", tx_hash),
            CardanoNetwork::Preview => format!("https://preview.cardanoscan.io/transaction/{}", tx_hash),
        }
    }

    /// Get network as string
    pub fn network(&self) -> &str {
        self.network.as_str()
    }

    /// Get signing key path
    pub fn signing_key_path(&self) -> Option<&Path> {
        self.signing_key_path.as_deref()
    }
}

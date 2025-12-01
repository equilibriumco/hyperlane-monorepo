use hyperlane_core::config::{ConfigErrResultExt, ConfigPath, ConfigResult, FromRawConf};
use url::Url;

use crate::blockfrost_provider::CardanoNetwork;

/// Cardano connection configuration
#[derive(Debug, Clone)]
pub struct ConnectionConf {
    /// Blockfrost API URL (optional, defaults to mainnet/testnet based on network)
    pub url: Url,
    /// Blockfrost API key
    pub api_key: String,
    /// Cardano network
    pub network: CardanoNetwork,
    /// Mailbox policy ID (hex)
    pub mailbox_policy_id: String,
    /// Registry policy ID (hex)
    pub registry_policy_id: String,
    /// ISM policy ID (hex)
    pub ism_policy_id: String,
    /// IGP (Interchain Gas Paymaster) policy ID (hex)
    pub igp_policy_id: String,
    /// Validator Announce policy ID (hex)
    pub validator_announce_policy_id: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct RawConnectionConf {
    url: Option<String>,
    api_key: Option<String>,
    network: Option<String>,
    mailbox_policy_id: Option<String>,
    registry_policy_id: Option<String>,
    ism_policy_id: Option<String>,
    igp_policy_id: Option<String>,
    validator_announce_policy_id: Option<String>,
}

/// An error type when parsing a connection configuration.
#[derive(thiserror::Error, Debug)]
pub enum ConnectionConfError {
    /// Missing `url` for connection configuration
    #[error("Missing `url` for connection configuration")]
    MissingConnectionUrl,
    /// Invalid `url` for connection configuration
    #[error("Invalid `url` for connection configuration: `{0}` ({1})")]
    InvalidConnectionUrl(String, url::ParseError),
    /// Missing `api_key` for connection configuration
    #[error("Missing `api_key` for Blockfrost connection")]
    MissingApiKey,
    /// Invalid network
    #[error("Invalid network: {0}. Expected 'mainnet', 'preprod', or 'preview'")]
    InvalidNetwork(String),
    /// Missing policy ID
    #[error("Missing {0} policy ID")]
    MissingPolicyId(&'static str),
}

fn parse_network(s: &str) -> Result<CardanoNetwork, ConnectionConfError> {
    match s.to_lowercase().as_str() {
        "mainnet" => Ok(CardanoNetwork::Mainnet),
        "preprod" => Ok(CardanoNetwork::Preprod),
        "preview" => Ok(CardanoNetwork::Preview),
        _ => Err(ConnectionConfError::InvalidNetwork(s.to_string())),
    }
}

fn default_blockfrost_url(network: CardanoNetwork) -> Url {
    let url_str = match network {
        CardanoNetwork::Mainnet => "https://cardano-mainnet.blockfrost.io/api/v0",
        CardanoNetwork::Preprod => "https://cardano-preprod.blockfrost.io/api/v0",
        CardanoNetwork::Preview => "https://cardano-preview.blockfrost.io/api/v0",
    };
    Url::parse(url_str).expect("Invalid default Blockfrost URL")
}

impl FromRawConf<RawConnectionConf> for ConnectionConf {
    fn from_config_filtered(
        raw: RawConnectionConf,
        cwp: &ConfigPath,
        _filter: (),
        _agent_name: &str,
    ) -> ConfigResult<Self> {
        use ConnectionConfError::*;

        // Parse network first (default to preprod for testing)
        let network = match &raw.network {
            Some(n) => parse_network(n).into_config_result(|| cwp.join("network"))?,
            None => CardanoNetwork::Preprod,
        };

        // Get or default the URL
        let url = match raw.url {
            Some(url) => url
                .parse()
                .map_err(|e| InvalidConnectionUrl(url, e))
                .into_config_result(|| cwp.join("url"))?,
            None => default_blockfrost_url(network),
        };

        // API key is required
        let api_key = raw
            .api_key
            .ok_or(MissingApiKey)
            .into_config_result(|| cwp.join("api_key"))?;

        // Policy IDs are required
        let mailbox_policy_id = raw
            .mailbox_policy_id
            .ok_or(MissingPolicyId("mailbox"))
            .into_config_result(|| cwp.join("mailbox_policy_id"))?;

        let registry_policy_id = raw
            .registry_policy_id
            .ok_or(MissingPolicyId("registry"))
            .into_config_result(|| cwp.join("registry_policy_id"))?;

        let ism_policy_id = raw
            .ism_policy_id
            .ok_or(MissingPolicyId("ism"))
            .into_config_result(|| cwp.join("ism_policy_id"))?;

        let igp_policy_id = raw
            .igp_policy_id
            .ok_or(MissingPolicyId("igp"))
            .into_config_result(|| cwp.join("igp_policy_id"))?;

        let validator_announce_policy_id = raw
            .validator_announce_policy_id
            .ok_or(MissingPolicyId("validator_announce"))
            .into_config_result(|| cwp.join("validator_announce_policy_id"))?;

        Ok(Self {
            url,
            api_key,
            network,
            mailbox_policy_id,
            registry_policy_id,
            ism_policy_id,
            igp_policy_id,
            validator_announce_policy_id,
        })
    }
}


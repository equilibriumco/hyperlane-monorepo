use hyperlane_core::H256;

/// Placeholder signer for Midnight.
///
/// Real signing is performed by the Midnight handle-submitter subprocess
/// (see `ConnectionConf::toolkit_path` and the `relayer/` workspace in
/// `equilibriumco/hyperlane-midnight`). This struct exists only to satisfy
/// the `ChainSigner` + `BuildableWithSignerConf` trait surface in
/// `hyperlane-base`. The submitter reads the relayer wallet seed from the
/// `MIDNIGHT_RELAYER_SEED` env var.
#[derive(Clone, Debug, Default)]
pub struct MidnightSigner {
    address: String,
    address_h256: H256,
}

impl MidnightSigner {
    /// Construct a placeholder signer. The real signer lives inside the
    /// submitter subprocess; the agent-side struct stays empty.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the configured address (always empty for the placeholder).
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Returns the configured address as `H256` (always zero for the placeholder).
    pub fn address_h256(&self) -> H256 {
        self.address_h256
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    use crate::ConnectionConf;

    #[test]
    fn config_constructs() {
        let conf = ConnectionConf::new(
            Url::parse("http://localhost:8080/graphql").unwrap(),
            Some("/srv/hyperlane/relayer/dist/submit-handle.js".to_string()),
        );
        assert_eq!(
            conf.indexer_graphql_url.as_str(),
            "http://localhost:8080/graphql"
        );
        assert!(conf.toolkit_path.is_some());
    }

    #[test]
    fn signer_constructs() {
        let signer = MidnightSigner::new();
        assert_eq!(signer.address(), "");
        assert_eq!(signer.address_h256(), H256::zero());
    }
}

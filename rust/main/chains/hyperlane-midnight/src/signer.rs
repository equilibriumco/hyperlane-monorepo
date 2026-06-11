use hyperlane_core::H256;

/// Placeholder signer. Real signing happens inside the submitter
/// subprocess; this exists only to satisfy `ChainSigner` +
/// `BuildableWithSignerConf` in `hyperlane-base`.
#[derive(Clone, Debug, Default)]
pub struct MidnightSigner {
    address: String,
    address_h256: H256,
}

impl MidnightSigner {
    /// Construct a placeholder signer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the configured address.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Returns the configured address as `H256`.
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

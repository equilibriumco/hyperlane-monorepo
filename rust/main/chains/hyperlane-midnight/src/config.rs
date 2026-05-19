use url::Url;

/// Midnight connection configuration.
///
/// Contract addresses flow via the standard `CoreContractAddresses` struct
/// at the `ChainConf` level, not this struct. Native token metadata flows via
/// `ChainConf.native_token`.
#[derive(Clone, Debug)]
pub struct ConnectionConf {
    /// GraphQL URL for the Midnight indexer (primary data source for reads).
    pub indexer_graphql_url: Url,
    /// Filesystem path to the Midnight handle submitter binary used by the
    /// Classic `Mailbox::process` implementation. The submitter reads a
    /// JSON payload on stdin and writes a JSON envelope on stdout (see
    /// `toolkit.rs` for the protocol). In the devnet workflow this points
    /// at the `relayer/dist/submit-handle.js` entrypoint in
    /// `equilibriumco/hyperlane-midnight`. Optional — `process` returns
    /// `HyperlaneMidnightError::MissingSubmitterPath` if absent.
    pub toolkit_path: Option<String>,
}

impl ConnectionConf {
    /// Construct a new `ConnectionConf`.
    pub fn new(indexer_graphql_url: Url, toolkit_path: Option<String>) -> Self {
        Self {
            indexer_graphql_url,
            toolkit_path,
        }
    }
}

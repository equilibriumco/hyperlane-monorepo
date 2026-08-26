use url::Url;

/// Midnight connection configuration.
#[derive(Clone, Debug)]
pub struct ConnectionConf {
    /// GraphQL URL for the Midnight indexer.
    pub indexer_graphql_url: Url,
    /// Path to the handle submitter binary.
    pub toolkit_path: Option<String>,
}

impl ConnectionConf {
    /// Construct a new `ConnectionConf`. Validators and threshold are not
    /// configured here; the ISM reads them from chain state.
    pub fn new(indexer_graphql_url: Url, toolkit_path: Option<String>) -> Self {
        Self {
            indexer_graphql_url,
            toolkit_path,
        }
    }
}

use hyperlane_core::H160;
use url::Url;

/// Midnight connection configuration.
#[derive(Clone, Debug)]
pub struct ConnectionConf {
    /// GraphQL URL for the Midnight indexer.
    pub indexer_graphql_url: Url,
    /// Path to the handle submitter binary.
    pub toolkit_path: Option<String>,
    /// Validator addresses registered on the destination Midnight contract,
    /// in the same order the on-chain `validators` vector was set at
    /// construction. The Mailbox uses this list to sort the signatures it
    /// forwards to the submitter so the on-chain two-pointer multisig walk
    /// accepts them. Empty when not supplied — `process()` then passes
    /// signatures through in the order the relayer's metadata builder
    /// produced them (matches pre-sort behaviour).
    pub validators: Vec<H160>,
}

impl ConnectionConf {
    /// Construct a new `ConnectionConf` with no validator list set.
    pub fn new(indexer_graphql_url: Url, toolkit_path: Option<String>) -> Self {
        Self {
            indexer_graphql_url,
            toolkit_path,
            validators: Vec::new(),
        }
    }

    /// Builder: set the validator list used to sort multisig signatures.
    pub fn with_validators(mut self, validators: Vec<H160>) -> Self {
        self.validators = validators;
        self
    }
}

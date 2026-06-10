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
    ///
    /// TODO(#14): once a point-in-time state reader for the Midnight
    /// indexer client lands, drop this field and read the validator set
    /// from on-chain state directly. The `validators` + `threshold` pair
    /// must migrate together to avoid set/threshold drift.
    pub validators: Vec<H160>,
    /// Multisig threshold registered on the destination Midnight contract.
    /// Used by `MultisigIsm::validators_and_threshold` to tell the relayer's
    /// metadata builder how many signatures to fetch.
    ///
    /// TODO(#14): read this from chain state alongside `validators` once the
    /// state reader is implemented.
    pub threshold: u8,
}

impl ConnectionConf {
    /// Construct a new `ConnectionConf` with no validator list set and a
    /// zero threshold. Both must be set via builder methods before use.
    pub fn new(indexer_graphql_url: Url, toolkit_path: Option<String>) -> Self {
        Self {
            indexer_graphql_url,
            toolkit_path,
            validators: Vec::new(),
            threshold: 0,
        }
    }

    /// Builder: set the validator list used to sort multisig signatures.
    pub fn with_validators(mut self, validators: Vec<H160>) -> Self {
        self.validators = validators;
        self
    }

    /// Builder: set the multisig threshold reported to the relayer.
    pub fn with_threshold(mut self, threshold: u8) -> Self {
        self.threshold = threshold;
        self
    }
}

use hyperlane_core::U256;
use url::Url;

/// Midnight connection configuration.
#[derive(Clone, Debug)]
pub struct ConnectionConf {
    /// GraphQL URL for the Midnight indexer.
    pub indexer_graphql_url: Url,
    /// JSON-RPC URL of a Midnight node (the chain config's first `rpcUrls`
    /// entry, http(s) or ws(s)). The merkle tree reads cross-check the
    /// indexer's contract state against it; `None` turns the check off.
    pub node_rpc_url: Option<Url>,
    /// Path to the handle submitter binary.
    pub toolkit_path: Option<String>,
    /// The gas a delivery on this chain is reported to cost, in the origin
    /// IGP's gas unit, so the fee-quoting gas payment policy can hold a
    /// message to its payment. Zero (the default) reports no estimate, which
    /// leaves that policy permissive.
    pub delivery_gas: U256,
}

impl ConnectionConf {
    /// Construct a new `ConnectionConf`. Validators and threshold are not
    /// configured here; the ISM reads them from chain state.
    pub fn new(
        indexer_graphql_url: Url,
        node_rpc_url: Option<Url>,
        toolkit_path: Option<String>,
    ) -> Self {
        Self {
            indexer_graphql_url,
            node_rpc_url,
            toolkit_path,
            delivery_gas: U256::zero(),
        }
    }

    /// Set the reported delivery gas.
    pub fn with_delivery_gas(mut self, delivery_gas: U256) -> Self {
        self.delivery_gas = delivery_gas;
        self
    }
}

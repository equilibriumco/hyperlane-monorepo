//! JSON-RPC client for a Midnight node, used to cross-check the contract
//! state the indexer serves.

use std::time::Duration;

use ethers::providers::{JsonRpcClient, Ws};
use serde::Deserialize;
use url::Url;

use crate::HyperlaneMidnightError;

/// Budget for one read, connection included.
const NODE_RPC_TIMEOUT: Duration = Duration::from_secs(30);

/// Reads a contract's state from a node over http(s) or ws(s). Each read is
/// one request on a fresh connection, so nothing is held between polls.
#[derive(Debug, Clone)]
pub struct MidnightNodeClient {
    url: Url,
    http: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    result: Option<String>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

impl MidnightNodeClient {
    /// `url` is the chain config's first `rpcUrls` entry.
    pub fn new(url: Url) -> Self {
        Self {
            url,
            http: reqwest::Client::new(),
        }
    }

    /// The node's URL.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// The contract's full serialized state at the node's latest block: the
    /// same bytes the indexer serves as `contractAction.state`. The address is
    /// bare hex; the node refuses a `0x` prefix.
    pub async fn contract_state(&self, address: &str) -> Result<Vec<u8>, HyperlaneMidnightError> {
        let state_hex =
            tokio::time::timeout(NODE_RPC_TIMEOUT, self.request_contract_state(address))
                .await
                .map_err(|_| {
                    HyperlaneMidnightError::NodeRpc(format!(
                        "{} did not answer within {}s",
                        self.url,
                        NODE_RPC_TIMEOUT.as_secs()
                    ))
                })??;
        hex::decode(state_hex.trim_start_matches("0x")).map_err(|e| {
            HyperlaneMidnightError::StateDecode(format!("node state is not valid hex: {e}"))
        })
    }

    async fn request_contract_state(
        &self,
        address: &str,
    ) -> Result<String, HyperlaneMidnightError> {
        let params = [address];
        let rpc_error = |e: &dyn std::fmt::Display| {
            HyperlaneMidnightError::NodeRpc(format!("{}: {e}", self.url))
        };
        match self.url.scheme() {
            "ws" | "wss" => {
                let ws = Ws::connect(self.url.as_str())
                    .await
                    .map_err(|e| rpc_error(&e))?;
                ws.request::<_, String>("midnight_contractState", params)
                    .await
                    .map_err(|e| rpc_error(&e))
            }
            "http" | "https" => {
                let body = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "midnight_contractState",
                    "params": params,
                });
                let response: JsonRpcResponse = self
                    .http
                    .post(self.url.clone())
                    .json(&body)
                    .send()
                    .await
                    .and_then(reqwest::Response::error_for_status)
                    .map_err(|e| rpc_error(&e))?
                    .json()
                    .await
                    .map_err(|e| rpc_error(&e))?;
                match (response.result, response.error) {
                    (Some(result), _) => Ok(result),
                    (None, Some(error)) => Err(HyperlaneMidnightError::NodeRpc(format!(
                        "{}: {} (code {})",
                        self.url, error.message, error.code
                    ))),
                    (None, None) => Err(HyperlaneMidnightError::NodeRpc(format!(
                        "{}: response carries neither a result nor an error",
                        self.url
                    ))),
                }
            }
            scheme => Err(HyperlaneMidnightError::NodeRpc(format!(
                "{}: unsupported scheme {scheme}, expected http(s) or ws(s)",
                self.url
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn refuses_a_scheme_it_cannot_speak() {
        let client = MidnightNodeClient::new(Url::parse("ftp://node.example/rpc").unwrap());
        let err = client
            .contract_state("00")
            .await
            .expect_err("ftp is not a JSON-RPC transport");
        assert!(
            err.to_string().contains("unsupported scheme ftp"),
            "names the scheme: {err}"
        );
    }

    /// Live read through both transports, against the devnet:
    ///
    ///   MIDNIGHT_NODE_URL=http://127.0.0.1:9944 \
    ///   MIDNIGHT_NIGHT_ADDRESS=<deployed night address hex> \
    ///   cargo test -p hyperlane-midnight reads_live_contract_state -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "requires a running Midnight node with a deployed night contract"]
    async fn reads_live_contract_state_from_node() {
        let endpoint = std::env::var("MIDNIGHT_NODE_URL")
            .expect("set MIDNIGHT_NODE_URL to the node's JSON-RPC endpoint");
        let address = std::env::var("MIDNIGHT_NIGHT_ADDRESS")
            .expect("set MIDNIGHT_NIGHT_ADDRESS to the deployed night contract address (hex)");

        let client = MidnightNodeClient::new(Url::parse(&endpoint).expect("valid node URL"));
        let bytes = client
            .contract_state(address.trim_start_matches("0x"))
            .await
            .expect("read contract state from the live node");
        println!("node served {} bytes of state", bytes.len());
        crate::state_decode::decode_contract_state(&bytes).expect("node state decodes");

        let missing = client
            .contract_state(&"d0".repeat(32))
            .await
            .expect_err("an undeployed address is refused");
        assert!(
            missing.to_string().contains("Contract not present"),
            "the node's refusal is passed through: {missing}"
        );
    }
}

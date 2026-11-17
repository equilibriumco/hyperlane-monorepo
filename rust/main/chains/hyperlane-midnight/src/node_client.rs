//! JSON-RPC client for a Midnight node, used to cross-check the contract
//! state the indexer serves.

use std::time::Duration;

use async_trait::async_trait;
use ethers::providers::{JsonRpcClient, Ws};
use serde::Deserialize;
use serde_json::{json, Value};
use url::Url;

use crate::HyperlaneMidnightError;

/// Budget for one read, connection and both calls included.
const NODE_RPC_TIMEOUT: Duration = Duration::from_secs(30);

const FINALIZED_HEAD: &str = "chain_getFinalizedHead";
const CONTRACT_STATE: &str = "midnight_contractState";

/// Reads a contract's state from a node over http(s) or ws(s). Each read opens
/// a fresh connection, so nothing is held between polls.
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

/// One connection to the node. Both calls of a read go through it.
#[async_trait]
trait NodeRpc: Send + Sync {
    async fn call(&self, method: &str, params: Value) -> Result<String, String>;
}

#[async_trait]
impl NodeRpc for Ws {
    async fn call(&self, method: &str, params: Value) -> Result<String, String> {
        self.request::<_, String>(method, params)
            .await
            .map_err(|e| e.to_string())
    }
}

struct HttpRpc<'a> {
    client: &'a reqwest::Client,
    url: &'a Url,
}

#[async_trait]
impl NodeRpc for HttpRpc<'_> {
    async fn call(&self, method: &str, params: Value) -> Result<String, String> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let response: JsonRpcResponse = self
            .client
            .post(self.url.clone())
            .json(&body)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| e.to_string())?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        match (response.result, response.error) {
            (Some(result), _) => Ok(result),
            (None, Some(error)) => Err(format!("{} (code {})", error.message, error.code)),
            (None, None) => Err("response carries neither a result nor an error".to_string()),
        }
    }
}

/// The contract's state, as hex, at the node's finalized head. On failure,
/// the call that failed and why.
async fn finalized_state_hex(
    rpc: &dyn NodeRpc,
    address: &str,
) -> Result<String, (&'static str, String)> {
    let head = rpc
        .call(FINALIZED_HEAD, json!([]))
        .await
        .map_err(|e| (FINALIZED_HEAD, e))?;
    rpc.call(CONTRACT_STATE, json!([address, head]))
        .await
        .map_err(|e| (CONTRACT_STATE, e))
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

    /// The contract's full serialized state at the node's finalized block:
    /// the same bytes the indexer serves as `contractAction.state`. The
    /// address is bare hex; the node refuses a `0x` prefix.
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
        let read = match self.url.scheme() {
            "ws" | "wss" => {
                let ws = Ws::connect(self.url.as_str())
                    .await
                    .map_err(|e| HyperlaneMidnightError::NodeRpc(format!("{}: {e}", self.url)))?;
                finalized_state_hex(&ws, address).await
            }
            "http" | "https" => {
                let rpc = HttpRpc {
                    client: &self.http,
                    url: &self.url,
                };
                finalized_state_hex(&rpc, address).await
            }
            scheme => {
                return Err(HyperlaneMidnightError::NodeRpc(format!(
                    "{}: unsupported scheme {scheme}, expected http(s) or ws(s)",
                    self.url
                )))
            }
        };
        read.map_err(|(method, e)| {
            HyperlaneMidnightError::NodeRpc(format!("{}: {method}: {e}", self.url))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    const HEAD: &str = "0x32d79281f047c2184d69cf779e942a1d96ec51d323e2501b064930fed390039d";

    /// Answers the finalized head with `head` and any state read with `aa`,
    /// recording every call.
    struct StubNode {
        head: Result<String, String>,
        calls: Mutex<Vec<(String, Value)>>,
    }

    impl StubNode {
        fn new(head: Result<String, String>) -> Self {
            Self {
                head,
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<(String, Value)> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl NodeRpc for StubNode {
        async fn call(&self, method: &str, params: Value) -> Result<String, String> {
            self.calls
                .lock()
                .unwrap()
                .push((method.to_string(), params));
            match method {
                FINALIZED_HEAD => self.head.clone(),
                CONTRACT_STATE => Ok("aa".to_string()),
                other => Err(format!("unexpected method {other}")),
            }
        }
    }

    #[tokio::test]
    async fn reads_the_state_at_the_finalized_head() {
        let node = StubNode::new(Ok(HEAD.to_string()));
        let state = finalized_state_hex(&node, "0102")
            .await
            .expect("both calls answer");
        assert_eq!(state, "aa");
        assert_eq!(
            node.calls(),
            vec![
                (FINALIZED_HEAD.to_string(), json!([])),
                (CONTRACT_STATE.to_string(), json!(["0102", HEAD])),
            ],
            "the state is read at the hash the node named as finalized"
        );
    }

    #[tokio::test]
    async fn reads_no_state_without_a_finalized_head() {
        let node = StubNode::new(Err("connection reset".to_string()));
        let err = finalized_state_hex(&node, "0102")
            .await
            .expect_err("the head call failed");
        assert_eq!(err, (FINALIZED_HEAD, "connection reset".to_string()));
        assert_eq!(
            node.calls().len(),
            1,
            "no state read at the best block in its place"
        );
    }

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

    /// Live read through both transports, against the devnet. The URL may be
    /// http or ws; the other scheme is derived from it.
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
        let address = address.trim_start_matches("0x");

        let given = Url::parse(&endpoint).expect("valid node URL");
        let mut other = given.clone();
        let other_scheme = match given.scheme() {
            "http" => "ws",
            "https" => "wss",
            "ws" => "http",
            "wss" => "https",
            scheme => panic!("unsupported scheme {scheme}"),
        };
        other.set_scheme(other_scheme).expect("swap the scheme");

        for url in [given, other] {
            let client = MidnightNodeClient::new(url.clone());
            let bytes = client
                .contract_state(address)
                .await
                .expect("read contract state from the live node");
            println!("{url} served {} bytes of state", bytes.len());
            crate::state_decode::decode_contract_state(&bytes).expect("node state decodes");

            let missing = client
                .contract_state(&"d0".repeat(32))
                .await
                .expect_err("an undeployed address is refused");
            let text = missing.to_string();
            assert!(
                text.contains(CONTRACT_STATE) && text.contains("Contract not present"),
                "the node's refusal of the state read is passed through: {text}"
            );
        }
    }
}

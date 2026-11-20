//! JSON-RPC client for a Midnight node, used to check what the indexer serves
//! against the contract state the node holds.

use std::ops::RangeInclusive;
use std::time::Duration;

use ethers::providers::{Http, JsonRpcClient, Ws};
use serde::Deserialize;
use serde_json::json;
use url::Url;

use crate::HyperlaneMidnightError;

/// Budget for one read, connection and every call included.
const NODE_RPC_TIMEOUT: Duration = Duration::from_secs(30);

const FINALIZED_HEAD: &str = "chain_getFinalizedHead";
const HEADER: &str = "chain_getHeader";
const BLOCK_HASH: &str = "chain_getBlockHash";
const CONTRACT_STATE: &str = "midnight_contractState";

/// Reads a contract's state from a node over http(s) or ws(s). Each read opens
/// a fresh connection, so nothing is held between polls.
#[derive(Debug, Clone)]
pub struct MidnightNodeClient {
    url: Url,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct Header {
    number: String,
}

/// The contract's state, as hex, at the newest finalized block within
/// `blocks`, with that block's height. `None` when the node has not finalized
/// the first of them. On failure, the call that failed and why.
async fn finalized_state_hex<P: JsonRpcClient>(
    rpc: &P,
    address: &str,
    blocks: &RangeInclusive<u64>,
) -> Result<Option<(u64, String)>, (&'static str, String)> {
    let head: String = rpc
        .request(FINALIZED_HEAD, json!([]))
        .await
        .map_err(|e| (FINALIZED_HEAD, e.to_string()))?;
    let header: Header = rpc
        .request(HEADER, json!([head]))
        .await
        .map_err(|e| (HEADER, e.to_string()))?;
    let finalized = u64::from_str_radix(header.number.trim_start_matches("0x"), 16)
        .map_err(|e| (HEADER, format!("block number {}: {e}", header.number)))?;
    if finalized < *blocks.start() {
        return Ok(None);
    }
    let (height, hash) = if finalized <= *blocks.end() {
        (finalized, head)
    } else {
        // A block below the finalized head is final too, and the node names
        // the one on its chain.
        let hash: Option<String> = rpc
            .request(BLOCK_HASH, json!([blocks.end()]))
            .await
            .map_err(|e| (BLOCK_HASH, e.to_string()))?;
        let hash = hash.ok_or((BLOCK_HASH, format!("no block at height {}", blocks.end())))?;
        (*blocks.end(), hash)
    };
    let state = rpc
        .request(CONTRACT_STATE, json!([address, hash]))
        .await
        .map_err(|e| (CONTRACT_STATE, e.to_string()))?;
    Ok(Some((height, state)))
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

    /// The contract's full serialized state at the node's newest finalized
    /// block within `blocks`, with that block's height: the same bytes the
    /// indexer serves as `contractAction.state`. `None` when the node has not
    /// finalized the first of them. The address is bare hex; the node refuses
    /// a `0x` prefix.
    pub async fn finalized_contract_state(
        &self,
        address: &str,
        blocks: RangeInclusive<u64>,
    ) -> Result<Option<(u64, Vec<u8>)>, HyperlaneMidnightError> {
        let read = tokio::time::timeout(NODE_RPC_TIMEOUT, self.request_state(address, &blocks))
            .await
            .map_err(|_| {
                HyperlaneMidnightError::NodeRpc(format!(
                    "{} did not answer within {}s",
                    self.url,
                    NODE_RPC_TIMEOUT.as_secs()
                ))
            })??;
        let Some((height, state_hex)) = read else {
            return Ok(None);
        };
        let state = hex::decode(state_hex.trim_start_matches("0x")).map_err(|e| {
            HyperlaneMidnightError::StateDecode(format!("node state is not valid hex: {e}"))
        })?;
        Ok(Some((height, state)))
    }

    async fn request_state(
        &self,
        address: &str,
        blocks: &RangeInclusive<u64>,
    ) -> Result<Option<(u64, String)>, HyperlaneMidnightError> {
        let read = match self.url.scheme() {
            "ws" | "wss" => {
                let ws = Ws::connect(self.url.as_str())
                    .await
                    .map_err(|e| HyperlaneMidnightError::NodeRpc(format!("{}: {e}", self.url)))?;
                finalized_state_hex(&ws, address, blocks).await
            }
            "http" | "https" => {
                let http = Http::new_with_client(self.url.clone(), self.http.clone());
                finalized_state_hex(&http, address, blocks).await
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
    use ethers::providers::MockProvider;

    use super::*;

    const HEAD: &str = "0x32d79281f047c2184d69cf779e942a1d96ec51d323e2501b064930fed390039d";
    const EARLIER: &str = "0x9a1b0f3e5d7c2a4b6c8d0e1f2a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d";

    fn header(number: u64) -> serde_json::Value {
        json!({ "number": format!("{number:#x}"), "parentHash": HEAD })
    }

    #[tokio::test]
    async fn reads_the_state_at_the_finalized_head_within_the_blocks() {
        let node = MockProvider::new();
        // The mock answers the last pushed first.
        node.push::<String, _>("aa".to_string()).unwrap();
        node.push(header(26)).unwrap();
        node.push::<String, _>(HEAD.to_string()).unwrap();
        let read = finalized_state_hex(&node, "0102", &(20..=30))
            .await
            .expect("every call answers");
        assert_eq!(read, Some((26, "aa".to_string())));
        node.assert_request(FINALIZED_HEAD, json!([])).unwrap();
        node.assert_request(HEADER, json!([HEAD])).unwrap();
        node.assert_request(CONTRACT_STATE, json!(["0102", HEAD]))
            .expect("the state is read at the hash the node named as finalized");
    }

    #[tokio::test]
    async fn reads_the_state_at_the_last_of_the_blocks_when_the_head_is_past_them() {
        let node = MockProvider::new();
        node.push::<String, _>("bb".to_string()).unwrap();
        node.push::<String, _>(EARLIER.to_string()).unwrap();
        node.push(header(45)).unwrap();
        node.push::<String, _>(HEAD.to_string()).unwrap();
        let read = finalized_state_hex(&node, "0102", &(20..=30))
            .await
            .expect("every call answers");
        assert_eq!(read, Some((30, "bb".to_string())));
        node.assert_request(FINALIZED_HEAD, json!([])).unwrap();
        node.assert_request(HEADER, json!([HEAD])).unwrap();
        node.assert_request(BLOCK_HASH, json!([30])).unwrap();
        node.assert_request(CONTRACT_STATE, json!(["0102", EARLIER]))
            .expect("the state is read at the node's block of that height");
    }

    #[tokio::test]
    async fn reads_no_state_before_the_first_of_the_blocks_is_finalized() {
        let node = MockProvider::new();
        node.push(header(19)).unwrap();
        node.push::<String, _>(HEAD.to_string()).unwrap();
        let read = finalized_state_hex(&node, "0102", &(20..=30))
            .await
            .expect("the head and its header answer");
        assert_eq!(read, None);
        node.assert_request(FINALIZED_HEAD, json!([])).unwrap();
        node.assert_request(HEADER, json!([HEAD])).unwrap();
        assert!(
            node.assert_request(CONTRACT_STATE, json!(["0102", HEAD]))
                .is_err(),
            "no state read at a block before the ones asked for"
        );
    }

    #[tokio::test]
    async fn reads_no_state_without_a_finalized_head() {
        let node = MockProvider::new();
        let (method, _) = finalized_state_hex(&node, "0102", &(0..=30))
            .await
            .expect_err("the node gave no head");
        assert_eq!(method, FINALIZED_HEAD);
        node.assert_request(FINALIZED_HEAD, json!([])).unwrap();
        assert!(
            node.assert_request(CONTRACT_STATE, json!(["0102"]))
                .is_err(),
            "no state read at the best block in its place"
        );
    }

    #[tokio::test]
    async fn names_the_height_the_node_has_no_block_at() {
        let node = MockProvider::new();
        node.push(serde_json::Value::Null).unwrap();
        node.push(header(45)).unwrap();
        node.push::<String, _>(HEAD.to_string()).unwrap();
        let (method, reason) = finalized_state_hex(&node, "0102", &(20..=30))
            .await
            .expect_err("the node has no block at the height");
        assert_eq!(method, BLOCK_HASH);
        assert!(reason.contains("no block at height 30"), "{reason}");
    }

    #[tokio::test]
    async fn refuses_a_scheme_it_cannot_speak() {
        let client = MidnightNodeClient::new(Url::parse("ftp://node.example/rpc").unwrap());
        let err = client
            .finalized_contract_state("00", 0..=0)
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
            let (head, bytes) = client
                .finalized_contract_state(address, 0..=u64::MAX)
                .await
                .expect("read contract state from the live node")
                .expect("the node has a finalized block");
            println!(
                "{url} served {} bytes of state at block {head}",
                bytes.len()
            );
            crate::state_decode::decode_contract_state(&bytes).expect("node state decodes");

            // A block below the head, which the node has to look up by height.
            let earlier = head.saturating_sub(1);
            let (height, bytes) = client
                .finalized_contract_state(address, 0..=earlier)
                .await
                .expect("read contract state at an earlier block")
                .expect("the earlier block is finalized");
            assert_eq!(
                height, earlier,
                "the state is the one at the block asked for"
            );
            crate::state_decode::decode_contract_state(&bytes).expect("earlier state decodes");

            assert!(
                client
                    .finalized_contract_state(address, u64::MAX..=u64::MAX)
                    .await
                    .expect("the head and its header answer")
                    .is_none(),
                "no state for blocks the node has not finalized"
            );

            let missing = client
                .finalized_contract_state(&"d0".repeat(32), 0..=u64::MAX)
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

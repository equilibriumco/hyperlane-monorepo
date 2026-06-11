use serde::{Deserialize, Serialize};
use url::Url;

use crate::HyperlaneMidnightError;

/// HTTP client for the Midnight indexer's GraphQL endpoint.
#[derive(Debug, Clone)]
pub struct MidnightIndexerClient {
    endpoint: Url,
    http: reqwest::Client,
}

impl MidnightIndexerClient {
    /// Build a client pointed at the given GraphQL HTTP endpoint.
    pub fn new(endpoint: Url) -> Self {
        Self {
            endpoint,
            http: reqwest::Client::new(),
        }
    }

    /// Latest known block height, or `None` if the indexer has not observed any yet.
    pub async fn latest_block_height(&self) -> Result<Option<u64>, HyperlaneMidnightError> {
        let query = r#"query { block { height } }"#;
        let body = GraphqlRequest {
            query,
            variables: serde_json::Value::Null,
        };

        let response: GraphqlResponse<LatestBlock> = self.post(&body).await?;
        if let Some(errors) = response.errors {
            if !errors.is_empty() {
                return Err(HyperlaneMidnightError::IndexerGraphql(
                    errors
                        .into_iter()
                        .map(|e| e.message)
                        .collect::<Vec<_>>()
                        .join("; "),
                ));
            }
        }
        Ok(response.data.and_then(|d| d.block.map(|b| b.height)))
    }

    async fn post<T: for<'de> Deserialize<'de>>(
        &self,
        body: &GraphqlRequest<'_>,
    ) -> Result<GraphqlResponse<T>, HyperlaneMidnightError> {
        let response = self
            .http
            .post(self.endpoint.clone())
            .json(body)
            .send()
            .await?;
        let status = response.status();
        let bytes = response.bytes().await?;

        if !status.is_success() {
            return Err(HyperlaneMidnightError::IndexerGraphql(format!(
                "HTTP {status}: {}",
                String::from_utf8_lossy(&bytes)
            )));
        }

        let parsed: GraphqlResponse<T> = serde_json::from_slice(&bytes)?;
        Ok(parsed)
    }
}

#[derive(Debug, Serialize)]
struct GraphqlRequest<'a> {
    query: &'a str,
    variables: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct GraphqlResponse<T> {
    data: Option<T>,
    errors: Option<Vec<GraphqlError>>,
}

#[derive(Debug, Deserialize)]
struct GraphqlError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct LatestBlock {
    #[serde(default)]
    block: Option<BlockHeight>,
}

#[derive(Debug, Deserialize)]
struct BlockHeight {
    height: u64,
}

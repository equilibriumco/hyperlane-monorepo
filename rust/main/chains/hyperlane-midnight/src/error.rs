use hyperlane_core::ChainCommunicationError;

/// Errors produced by the `hyperlane-midnight` crate.
#[derive(Debug, thiserror::Error)]
pub enum HyperlaneMidnightError {
    /// Feature not implemented yet.
    #[error("Midnight: {0} not implemented yet")]
    NotImplemented(&'static str),

    /// The configured submitter binary path is empty.
    #[error("Midnight submitter path is not configured (set `toolkitPath` in the chain config)")]
    MissingSubmitterPath,

    /// Failed to spawn the submitter subprocess.
    #[error("failed to spawn submitter `{path}`: {source}")]
    SubmitterSpawn {
        /// Path that the agent attempted to spawn.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Submitter exited with a non-zero status code.
    #[error("submitter exited with status {status}; stderr: {stderr}")]
    SubmitterFailed {
        /// Process exit status (numeric for cross-platform).
        status: i32,
        /// Captured stderr from the subprocess.
        stderr: String,
    },

    /// Submitter produced output that could not be parsed.
    #[error("submitter returned malformed JSON: {message} (raw: {raw})")]
    SubmitterMalformed {
        /// Parser error message.
        message: String,
        /// Raw stdout the parser failed on (truncated upstream if very long).
        raw: String,
    },

    /// Submitter reported a structured error in its JSON response.
    #[error("submitter reported error: {kind}: {message}")]
    SubmitterReported {
        /// Short kind tag from the submitter (e.g. `proofTimeout`, `insufficientDust`).
        kind: String,
        /// Human-readable message from the submitter.
        message: String,
    },

    /// The indexer GraphQL endpoint returned an error.
    #[error("indexer GraphQL error: {0}")]
    IndexerGraphql(String),

    /// HTTP transport failure talking to the indexer.
    #[error("indexer HTTP error: {0}")]
    IndexerHttp(#[from] reqwest::Error),

    /// JSON failure in the indexer client path.
    #[error("indexer JSON error: {0}")]
    IndexerJson(#[from] serde_json::Error),

    /// Generic catch-all for anything else.
    #[error("{0}")]
    Other(String),
}

impl From<HyperlaneMidnightError> for ChainCommunicationError {
    fn from(value: HyperlaneMidnightError) -> Self {
        ChainCommunicationError::from_other(value)
    }
}

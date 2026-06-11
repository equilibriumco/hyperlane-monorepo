use hyperlane_core::ChainCommunicationError;

/// Errors produced by the `hyperlane-midnight` crate.
#[derive(Debug, thiserror::Error)]
pub enum HyperlaneMidnightError {
    /// Feature not implemented yet.
    #[error("Midnight: {0} not implemented yet")]
    NotImplemented(&'static str),

    /// Submitter path is unset.
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
        /// Process exit status.
        status: i32,
        /// Captured stderr.
        stderr: String,
    },

    /// Submitter did not exit within the wall-clock budget.
    #[error("submitter exceeded {elapsed_secs}s timeout (SIGKILLed)")]
    SubmitterTimeout {
        /// Wall-clock budget in seconds.
        elapsed_secs: u64,
    },

    /// Submitter stdout could not be parsed as JSON.
    #[error("submitter returned malformed JSON: {message} (raw: {raw})")]
    SubmitterMalformed {
        /// Parser error.
        message: String,
        /// Raw stdout.
        raw: String,
    },

    /// Submitter reported a structured error.
    #[error("submitter reported error: {kind}: {message}")]
    SubmitterReported {
        /// Short kind tag.
        kind: String,
        /// Human-readable message.
        message: String,
    },

    /// Indexer GraphQL error.
    #[error("indexer GraphQL error: {0}")]
    IndexerGraphql(String),

    /// HTTP transport failure talking to the indexer.
    #[error("indexer HTTP error: {0}")]
    IndexerHttp(#[from] reqwest::Error),

    /// JSON failure in the indexer client path.
    #[error("indexer JSON error: {0}")]
    IndexerJson(#[from] serde_json::Error),

    /// Catch-all.
    #[error("{0}")]
    Other(String),
}

impl From<HyperlaneMidnightError> for ChainCommunicationError {
    fn from(value: HyperlaneMidnightError) -> Self {
        ChainCommunicationError::from_other(value)
    }
}

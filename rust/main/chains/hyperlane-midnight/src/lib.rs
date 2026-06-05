//! Hyperlane Midnight chain integration.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod config;
mod error;
mod indexer_client;
mod mailbox;
mod provider;
mod signer;
mod toolkit;

#[cfg(test)]
mod cross_boundary_tests;

pub use config::ConnectionConf;
pub use error::HyperlaneMidnightError;
pub use indexer_client::MidnightIndexerClient;
pub use mailbox::MidnightMailbox;
pub use provider::MidnightProvider;
pub use signer::MidnightSigner;

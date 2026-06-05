//! Hyperlane Midnight chain integration.
//!
//! Issue #13 (T16) landed the scaffolding (config + signer placeholders +
//! framework wiring). Issue #20 adds destination-side `Mailbox` delivery:
//! the `MidnightMailbox` parses MessageIdMultisigIsmMetadata and shells out
//! to a submitter binary (Node script in `equilibriumco/hyperlane-midnight`)
//! to invoke the `handle` circuit on the deployed WarpRoute contract.
//!
//! Outbound dispatch, ValidatorAnnounce, IGP, and the full indexer client
//! arrive with issues #9 / #10 / #11 / #12 / #14 / #15 / #16 / #33.

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

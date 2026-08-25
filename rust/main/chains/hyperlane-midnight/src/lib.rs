//! Implementation of hyperlane for Midnight.
//!
//! Gated behind the `midnight` feature (off by default) because the native
//! state decode pulls in Midnight's ZK proving crates; without it, agents drop
//! that dependency tree and filter Midnight chains out of their config.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod application;
mod config;
mod error;
mod events;
mod indexer;
mod indexer_client;
mod interchain_gas;
pub mod ism;
mod mailbox;
mod merkle_tree_hook;
mod provider;
mod signer;
pub mod state_decode;
mod toolkit;
mod validator_announce;

#[cfg(test)]
mod cross_boundary_tests;
#[cfg(test)]
mod metadata_tests;

pub use config::ConnectionConf;
pub use error::HyperlaneMidnightError;
pub use indexer::{MidnightDeliveryIndexer, MidnightDispatchIndexer};
pub use indexer_client::MidnightIndexerClient;
pub use interchain_gas::MidnightInterchainGasPaymaster;
pub use mailbox::MidnightMailbox;
pub use merkle_tree_hook::MidnightMerkleTreeHook;
pub use provider::MidnightProvider;
pub use signer::MidnightSigner;
pub use validator_announce::MidnightValidatorAnnounce;

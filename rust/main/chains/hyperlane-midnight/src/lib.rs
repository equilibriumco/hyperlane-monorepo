//! Hyperlane Midnight chain integration.
//!
//! # Build feature
//!
//! This crate is an **optional** dependency of `hyperlane-base`, gated behind
//! the `midnight` feature (off by default), mirroring how `hyperlane-aleo` is
//! gated behind `aleo`. Agent binaries compile it -- and its heavy native
//! decode stack (`midnight-onchain-*` and the transitive ZK proving crates
//! `midnight-proofs` / `midnight-circuits` / `midnight-curves`) -- only when
//! built with `--features midnight` (e.g. `cargo build -p relayer --features
//! midnight`). A build without the feature drops the whole crate and that ZK
//! dependency tree, and `is_protocol_supported` filters Midnight chains out of
//! the agent config at parse time with an "enable with --features" warning.

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

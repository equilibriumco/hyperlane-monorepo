//! Direct detection of Cardano rollbacks.
//!
//! The validator already catches reorgs indirectly: it rebuilds the merkle tree
//! from indexed dispatches and panics when the root disagrees with the chain.
//! That only fires at checkpoint time, and only for rollbacks that change the
//! root — a rolled-back gas payment or delivery is invisible to it.
//!
//! This detector notices the rollback itself. It remembers a block it actually
//! read data from, and re-checks that the chain still has that same block at
//! that height. Blockfrost has no chain-sync subscription, so there is no
//! rollback signal to listen for; asking is the only option.

use crate::blockfrost_provider::BlockfrostProvider;
use hyperlane_core::H256;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, warn};

/// A block we read data from, kept so we can notice if it disappears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub height: u64,
    pub hash: H256,
}

/// What the chain reports at a height we previously anchored on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockAtHeight {
    /// The chain has a block there, with this hash.
    Hash(H256),
    /// The chain has no block there any more — it is shorter than it was.
    Missing,
    /// The provider could not say. Not evidence of anything.
    Unknown,
}

/// Why we believe a rollback happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rollback {
    /// A different block now occupies a height we already read.
    Replaced { height: u64, was: H256, now: H256 },
    /// A height we already read is now past the end of the chain.
    Truncated { height: u64, was: H256 },
}

/// Decide whether the chain rolled back under us.
///
/// A provider that simply fails to answer is deliberately not a rollback: this
/// verdict makes a validator shout, and crying wolf over an API hiccup trains
/// operators to ignore it.
fn verdict(anchor: &Anchor, observed: BlockAtHeight) -> Option<Rollback> {
    match observed {
        BlockAtHeight::Hash(hash) if hash == anchor.hash => None,
        BlockAtHeight::Hash(hash) => Some(Rollback::Replaced {
            height: anchor.height,
            was: anchor.hash,
            now: hash,
        }),
        BlockAtHeight::Missing => Some(Rollback::Truncated {
            height: anchor.height,
            was: anchor.hash,
        }),
        BlockAtHeight::Unknown => None,
    }
}

/// Watches one block at a time and reports when it stops being there.
#[derive(Debug)]
pub struct ReorgDetector {
    provider: Arc<BlockfrostProvider>,
    anchor: Mutex<Option<Anchor>>,
}

impl ReorgDetector {
    pub fn new(provider: Arc<BlockfrostProvider>) -> Self {
        Self {
            provider,
            anchor: Mutex::new(None),
        }
    }

    /// Remember a block we just read data from.
    ///
    /// Anchoring only on blocks that carried data is deliberate: a rollback of
    /// an empty block costs us nothing, and re-checking one would spend a
    /// request per scan on a chain that is mostly idle.
    pub async fn anchor(&self, height: u64, hash: H256) {
        let mut current = self.anchor.lock().await;
        match *current {
            Some(existing) if existing.height >= height => {}
            _ => *current = Some(Anchor { height, hash }),
        }
    }

    /// Re-check the anchored block. Logs and returns the rollback if there is
    /// one; the anchor is cleared so a single event is not reported forever.
    pub async fn check(&self) -> Option<Rollback> {
        let anchor = (*self.anchor.lock().await)?;

        let observed = match self.provider.get_block_by_height(anchor.height).await {
            Ok(block) => match hex::decode(&block.hash) {
                Ok(bytes) if bytes.len() == 32 => BlockAtHeight::Hash(H256::from_slice(&bytes)),
                _ => {
                    warn!(hash = %block.hash, "Unreadable block hash; treating as inconclusive");
                    BlockAtHeight::Unknown
                }
            },
            Err(e) => {
                let text = format!("{e:?}");
                if text.contains("404") || text.contains("Not Found") {
                    BlockAtHeight::Missing
                } else {
                    warn!(error = %text, "Could not re-check anchored block");
                    BlockAtHeight::Unknown
                }
            }
        };

        let rollback = verdict(&anchor, observed)?;
        error!(
            ?rollback,
            "Cardano rollback detected: a block this agent already read data from is no longer on \
             the chain. Messages indexed at or after this height may be based on transactions that \
             never happened."
        );
        *self.anchor.lock().await = None;
        Some(rollback)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor() -> Anchor {
        Anchor {
            height: 100,
            hash: H256::repeat_byte(0xaa),
        }
    }

    #[test]
    fn same_block_still_there_is_not_a_rollback() {
        let a = anchor();
        assert_eq!(verdict(&a, BlockAtHeight::Hash(a.hash)), None);
    }

    #[test]
    fn a_different_block_at_the_same_height_is_a_rollback() {
        let a = anchor();
        let now = H256::repeat_byte(0xbb);
        assert_eq!(
            verdict(&a, BlockAtHeight::Hash(now)),
            Some(Rollback::Replaced {
                height: 100,
                was: a.hash,
                now,
            })
        );
    }

    #[test]
    fn a_height_that_no_longer_exists_is_a_rollback() {
        let a = anchor();
        assert_eq!(
            verdict(&a, BlockAtHeight::Missing),
            Some(Rollback::Truncated {
                height: 100,
                was: a.hash,
            })
        );
    }

    /// A provider outage must not be reported as a chain rollback.
    #[test]
    fn an_unanswerable_provider_is_not_evidence() {
        assert_eq!(verdict(&anchor(), BlockAtHeight::Unknown), None);
    }

    #[tokio::test]
    async fn anchoring_only_moves_forward() {
        let provider = Arc::new(BlockfrostProvider::new(
            "test",
            crate::blockfrost_provider::CardanoNetwork::Preview,
            0,
        ));
        let detector = ReorgDetector::new(provider);

        detector.anchor(100, H256::repeat_byte(0xaa)).await;
        detector.anchor(50, H256::repeat_byte(0xbb)).await;

        let held = detector.anchor.lock().await.expect("anchored");
        assert_eq!(
            held.height, 100,
            "an older block must not replace a newer one"
        );
    }
}

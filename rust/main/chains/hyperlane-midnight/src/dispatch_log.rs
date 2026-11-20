//! The messages the WarpRoute contract has dispatched, followed from the
//! indexer's `HYP_DISPATCH` events.
//!
//! The contract stores no message. It keeps how many it has dispatched and a
//! running hash over their ids, and the events carry the messages. Only the
//! indexer serves events, so a log reads the events of each new block, in
//! order. Given a node, it holds a message only once the ids it read hash to
//! the count and the hash in the node's copy of the state, and reads over
//! again what the node does not back.

use std::collections::VecDeque;
use std::ops::RangeInclusive;
use std::time::Duration;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tokio::time::Instant;

use hyperlane_core::{accumulator::incremental::IncrementalMerkle, ChainResult, H256};

use crate::events::{
    address_hex, decode_dispatch_event, has_name, MidnightEventReader, HYP_DISPATCH,
};
use crate::state_decode::{decode_dispatch_state, DispatchState};
use crate::{HyperlaneMidnightError, MidnightNodeClient};

/// The contract's running hash after one more message: SHA-256 over the hash
/// so far and the message's id.
pub(crate) fn next_dispatch_hash(hash: H256, id: H256) -> H256 {
    let mut sha = Sha256::new();
    sha.update(hash);
    sha.update(id);
    H256::from_slice(&sha.finalize())
}

/// The node read a log is checked against. Behind a trait so the log can be
/// unit-tested with no node.
#[async_trait]
pub(crate) trait FinalizedDispatchState: Send + Sync {
    /// The contract's dispatch count and running hash at the node's newest
    /// finalized block within `blocks`, with that block's height. `None` when
    /// the node has not finalized the first of them.
    async fn finalized_dispatch_state(
        &self,
        address: &str,
        blocks: RangeInclusive<u64>,
    ) -> ChainResult<Option<(u64, DispatchState)>>;
}

#[async_trait]
impl FinalizedDispatchState for MidnightNodeClient {
    async fn finalized_dispatch_state(
        &self,
        address: &str,
        blocks: RangeInclusive<u64>,
    ) -> ChainResult<Option<(u64, DispatchState)>> {
        let Some((height, bytes)) = self.finalized_contract_state(address, blocks).await? else {
            return Ok(None);
        };
        Ok(Some((height, decode_dispatch_state(&bytes)?)))
    }
}

/// The indexer can answer an events query from a copy of the chain a few
/// blocks behind the one that named the tip, and a message in those blocks
/// would be missed for good. So each read starts this many blocks before the
/// last one ended, and skips the messages it has already.
const REREAD_BLOCKS: u32 = 10;

/// While the events do not add up, the least time between two reads that
/// start over at the newest held message. With nothing held yet that is the
/// whole history, which is not to be asked for on every poll.
const START_OVER_INTERVAL: Duration = Duration::from_secs(60);

/// A message the indexer served that is not held yet.
#[derive(Debug)]
struct Unchecked {
    id: H256,
    block: u32,
}

#[derive(Debug, Default)]
struct Read {
    /// The first block whose events have not been read.
    next_block: u32,
    /// The tree of the held messages.
    tree: IncrementalMerkle,
    /// The contract's running hash after the held messages.
    hash: H256,
    /// The held messages' ids by nonce, kept when there is a node.
    ids: Vec<H256>,
    /// The block of the newest held message.
    held_block: u32,
    /// The messages read and not held yet, oldest first.
    unchecked: VecDeque<Unchecked>,
    /// When a read last started over.
    started_over: Option<Instant>,
}

impl Read {
    fn next_nonce(&self) -> u32 {
        (self.tree.count() + self.unchecked.len()) as u32
    }

    /// The last block the held messages are every message up to.
    fn tip(&self) -> u32 {
        match self.unchecked.front() {
            Some(first) => first.block.saturating_sub(1),
            None => self.next_block.saturating_sub(1),
        }
    }

    /// Holds the `count` oldest unchecked messages.
    fn hold(&mut self, count: usize, keep_ids: bool) {
        for message in self.unchecked.drain(..count) {
            self.hash = next_dispatch_hash(self.hash, message.id);
            self.tree.ingest(message.id);
            if keep_ids {
                self.ids.push(message.id);
            }
            self.held_block = message.block;
        }
    }

    /// Forgets the messages not held and has the next read start at the block
    /// after the newest held one, unless a read started over less than
    /// `START_OVER_INTERVAL` ago.
    fn start_over(&mut self) {
        if self
            .started_over
            .is_some_and(|at| at.elapsed() < START_OVER_INTERVAL)
        {
            return;
        }
        self.started_over = Some(Instant::now());
        self.unchecked.clear();
        self.next_block = self.held_block.saturating_add(1);
    }
}

/// One contract's dispatched messages, as one agent process has read them.
/// It starts empty, so the first read covers the whole history and each later
/// one only the newest blocks.
///
/// It holds the messages it answers for. With no node those are all it read.
/// With one they are those the node backs, which are the ones a validator
/// stores and signs.
#[derive(Debug)]
pub(crate) struct DispatchLog<N = MidnightNodeClient> {
    address: String,
    node: Option<N>,
    read: Mutex<Read>,
}

impl<N: FinalizedDispatchState> DispatchLog<N> {
    pub(crate) fn new(address: H256, node: Option<N>) -> Self {
        Self {
            address: address_hex(&address),
            node,
            read: Mutex::new(Read::default()),
        }
    }

    /// How many messages are held, and the last block that count covers.
    pub(crate) async fn count_and_tip<R: MidnightEventReader>(
        &self,
        reader: &R,
    ) -> ChainResult<(u32, u32)> {
        let mut read = self.read.lock().await;
        self.advance(&mut read, reader).await?;
        Ok((read.tree.count() as u32, read.tip()))
    }

    /// The tree of the held messages.
    pub(crate) async fn tree<R: MidnightEventReader>(
        &self,
        reader: &R,
    ) -> ChainResult<IncrementalMerkle> {
        let mut read = self.read.lock().await;
        self.advance(&mut read, reader).await?;
        Ok(read.tree.clone())
    }

    /// `messages` without those the node does not back, as of the newest
    /// blocks. With no node, all of them.
    pub(crate) async fn held<R, T>(
        &self,
        reader: &R,
        messages: Vec<T>,
        nonce_and_id: impl Fn(&T) -> (u32, H256) + Send,
    ) -> ChainResult<Vec<T>>
    where
        R: MidnightEventReader,
        T: Send,
    {
        if self.node.is_none() {
            return Ok(messages);
        }
        let mut read = self.read.lock().await;
        self.advance(&mut read, reader).await?;
        Ok(messages
            .into_iter()
            .filter(|message| {
                let (nonce, id) = nonce_and_id(message);
                read.ids.get(nonce as usize) == Some(&id)
            })
            .collect())
    }

    /// Reads the new blocks' events and holds what can be held.
    async fn advance<R: MidnightEventReader>(
        &self,
        read: &mut Read,
        reader: &R,
    ) -> ChainResult<()> {
        self.follow(read, reader).await?;
        match &self.node {
            Some(node) => self.check(read, node).await,
            None => {
                read.hold(read.unchecked.len(), false);
                Ok(())
            }
        }
    }

    /// Reads the events of the blocks not read yet. A message past the next
    /// nonce, or from a block that was not asked for, is an error: nothing of
    /// the read is kept.
    async fn follow<R: MidnightEventReader>(&self, read: &mut Read, reader: &R) -> ChainResult<()> {
        let tip = reader.read_tip().await?;
        if tip < read.next_block {
            // A tip this far behind means the last one was not the chain's.
            if tip.saturating_add(REREAD_BLOCKS) < read.next_block {
                read.start_over();
            }
            return Ok(());
        }
        let from_block = read.next_block.saturating_sub(REREAD_BLOCKS);
        let events = reader.misc_events(&self.address, from_block, tip).await?;

        let mut next_nonce = read.next_nonce();
        let mut new = Vec::new();
        for event in events.iter().filter(|event| has_name(event, HYP_DISPATCH)) {
            let message = decode_dispatch_event(event)?;
            if message.nonce < next_nonce {
                continue;
            }
            let block = u32::try_from(event.block_height).unwrap_or(u32::MAX);
            if message.nonce > next_nonce || !(from_block..=tip).contains(&block) {
                read.start_over();
                return Err(HyperlaneMidnightError::DispatchSequence(format!(
                    "the events of blocks {from_block} to {tip} have the message with nonce {} in block {}, where nonce {next_nonce} comes next",
                    message.nonce, event.block_height
                ))
                .into());
            }
            new.push(Unchecked {
                id: message.id(),
                block,
            });
            next_nonce += 1;
        }
        read.unchecked.extend(new);
        read.next_block = tip.saturating_add(1);
        Ok(())
    }

    /// Holds the unchecked messages the node backs, at the newest block both
    /// it and the indexer have. The node is asked only when there is something
    /// to check, so an idle contract costs no state read.
    async fn check(&self, read: &mut Read, node: &N) -> ChainResult<()> {
        let Some(first) = read.unchecked.front() else {
            return Ok(());
        };
        let last_read = read.next_block.saturating_sub(1);
        let Some((height, held)) = node
            .finalized_dispatch_state(&self.address, u64::from(first.block)..=u64::from(last_read))
            .await?
        else {
            return Ok(());
        };

        let within = read
            .unchecked
            .iter()
            .take_while(|message| u64::from(message.block) <= height)
            .count();
        let count = read.tree.count() + within;
        let hash = read
            .unchecked
            .iter()
            .take(within)
            .fold(read.hash, |hash, message| {
                next_dispatch_hash(hash, message.id)
            });
        if count as u64 != u64::from(held.count) || hash != held.hash {
            read.start_over();
            return Err(HyperlaneMidnightError::IndexerNodeMismatch(format!(
                "up to block {height} the indexer's dispatch events make {count} messages with running hash {hash:?}, the node holds {} with {:?}",
                held.count, held.hash
            ))
            .into());
        }
        read.hold(within, true);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex as StdMutex};

    use hyperlane_core::{Encode as _, HyperlaneMessage};

    use super::*;
    use crate::events::test_util::misc_event;
    use crate::events::HYP_PROCESS;
    use crate::indexer_client::MiscEvent;

    const ADDRESS: H256 = H256::repeat_byte(0x42);

    fn message(nonce: u32) -> HyperlaneMessage {
        HyperlaneMessage {
            version: 3,
            nonce,
            origin: 1234,
            sender: H256::repeat_byte(0xAA),
            destination: 99,
            recipient: H256::repeat_byte(0xBB),
            body: vec![nonce as u8 + 1; 64],
        }
    }

    /// A dispatch of `message` in block `1000 + id`.
    fn dispatch(id: u64, message: &HyperlaneMessage) -> MiscEvent {
        misc_event(id, HYP_DISPATCH, &message.to_vec())
    }

    /// What the contract holds after `messages`.
    fn held(messages: &[HyperlaneMessage]) -> DispatchState {
        DispatchState {
            count: messages.len() as u32,
            hash: messages
                .iter()
                .fold(H256::zero(), |hash, m| next_dispatch_hash(hash, m.id())),
        }
    }

    fn tree_of(messages: &[HyperlaneMessage]) -> IncrementalMerkle {
        let mut tree = IncrementalMerkle::default();
        for message in messages {
            tree.ingest(message.id());
        }
        tree
    }

    /// An indexer whose tip and events a test changes between reads, and
    /// which records the block ranges it was asked for.
    #[derive(Default)]
    struct Indexer {
        tip: StdMutex<u32>,
        events: StdMutex<Vec<MiscEvent>>,
        /// Served whatever blocks are asked for.
        stray: StdMutex<Vec<MiscEvent>>,
        asked: StdMutex<Vec<(u32, u32)>>,
    }

    impl Indexer {
        fn at(tip: u32, events: Vec<MiscEvent>) -> Self {
            let indexer = Self::default();
            indexer.set(tip, events);
            indexer
        }

        fn set(&self, tip: u32, events: Vec<MiscEvent>) {
            *self.tip.lock().unwrap() = tip;
            *self.events.lock().unwrap() = events;
        }

        fn asked(&self) -> Vec<(u32, u32)> {
            self.asked.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl MidnightEventReader for Indexer {
        async fn read_tip(&self) -> ChainResult<u32> {
            Ok(*self.tip.lock().unwrap())
        }

        async fn misc_events(
            &self,
            _address: &str,
            from_block: u32,
            to_block: u32,
        ) -> ChainResult<Vec<MiscEvent>> {
            self.asked.lock().unwrap().push((from_block, to_block));
            Ok(self
                .events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| {
                    e.block_height >= u64::from(from_block) && e.block_height <= u64::from(to_block)
                })
                .chain(self.stray.lock().unwrap().iter())
                .cloned()
                .collect())
        }

        async fn misc_events_by_tx(
            &self,
            _address: &str,
            _tx_hash: &H256,
        ) -> ChainResult<Vec<MiscEvent>> {
            unreachable!("the log reads by block")
        }
    }

    /// A node finalized up to `finalized`, holding `states` by the block they
    /// took effect in. It records the block ranges it was asked about.
    #[derive(Debug)]
    struct Node {
        finalized: StdMutex<u64>,
        states: Vec<(u64, DispatchState)>,
        asked: StdMutex<Vec<RangeInclusive<u64>>>,
    }

    impl Node {
        fn new(finalized: u64, states: Vec<(u64, DispatchState)>) -> Arc<Self> {
            Arc::new(Self {
                finalized: StdMutex::new(finalized),
                states,
                asked: StdMutex::new(Vec::new()),
            })
        }

        fn finalize(&self, height: u64) {
            *self.finalized.lock().unwrap() = height;
        }

        fn asked(&self) -> Vec<RangeInclusive<u64>> {
            self.asked.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl FinalizedDispatchState for Arc<Node> {
        async fn finalized_dispatch_state(
            &self,
            _address: &str,
            blocks: RangeInclusive<u64>,
        ) -> ChainResult<Option<(u64, DispatchState)>> {
            self.asked.lock().unwrap().push(blocks.clone());
            let finalized = *self.finalized.lock().unwrap();
            if finalized < *blocks.start() {
                return Ok(None);
            }
            let height = finalized.min(*blocks.end());
            let state = self
                .states
                .iter()
                .rev()
                .find(|(since, _)| *since <= height)
                .map(|(_, state)| *state)
                .unwrap_or(held(&[]));
            Ok(Some((height, state)))
        }
    }

    fn log() -> DispatchLog<Arc<Node>> {
        DispatchLog::new(ADDRESS, None)
    }

    fn log_on(node: &Arc<Node>) -> DispatchLog<Arc<Node>> {
        DispatchLog::new(ADDRESS, Some(node.clone()))
    }

    // SHA-256 over 32 zero bytes and 32 bytes of 0x01, then over that and 32
    // bytes of 0x02: what the contract holds after those two ids, by the
    // contracts repo's own unit tests.
    #[test]
    fn the_running_hash_is_sha256_over_the_hash_so_far_and_the_id() {
        let first = next_dispatch_hash(H256::zero(), H256::repeat_byte(0x01));
        assert_eq!(
            format!("{first:x}"),
            "5c85955f709283ecce2b74f1b1552918819f390911816e7bb466805a38ab87f3"
        );
        let second = next_dispatch_hash(first, H256::repeat_byte(0x02));
        assert_eq!(
            format!("{second:x}"),
            "a7f2fad943905535b10ccf63c832802ed84eaffb15e4fb6bee86a817c35eb833"
        );
        assert_ne!(
            second,
            next_dispatch_hash(
                next_dispatch_hash(H256::zero(), H256::repeat_byte(0x02)),
                H256::repeat_byte(0x01)
            ),
            "the order of the ids matters"
        );
    }

    // The contract's own arithmetic against this one: the state the contracts
    // repo's compiled contract was left in by the fixture's two messages.
    #[test]
    fn the_fixture_state_holds_the_hash_of_the_fixture_messages() {
        let state =
            hex::decode(include_str!("../tests/fixtures/night-state-dispatched.hex").trim())
                .expect("fixture is valid hex");
        let messages = crate::events::test_util::dispatched_fixture();
        assert_eq!(messages.len(), 2, "fixture has two dispatches");
        assert_eq!(
            decode_dispatch_state(&state).expect("decode dispatch state"),
            held(&messages)
        );
    }

    #[tokio::test]
    async fn counts_the_dispatches_up_to_the_tip() {
        let (m0, m1) = (message(0), message(1));
        let indexer = Indexer::at(
            2000,
            vec![
                dispatch(1, &m0),
                misc_event(2, HYP_PROCESS, H256::repeat_byte(0x11).as_bytes()),
                dispatch(3, &m1),
            ],
        );
        let log = log();
        assert_eq!(log.count_and_tip(&indexer).await.unwrap(), (2, 2000));
    }

    #[tokio::test]
    async fn reads_the_history_once_and_then_only_the_newest_blocks() {
        let (m0, m1, m2) = (message(0), message(1), message(2));
        let indexer = Indexer::at(1002, vec![dispatch(1, &m0), dispatch(2, &m1)]);
        let log = log();
        assert_eq!(log.count_and_tip(&indexer).await.unwrap(), (2, 1002));

        // No new block: nothing is asked for.
        assert_eq!(log.count_and_tip(&indexer).await.unwrap(), (2, 1002));

        indexer.set(
            1030,
            vec![dispatch(1, &m0), dispatch(2, &m1), dispatch(25, &m2)],
        );
        assert_eq!(log.count_and_tip(&indexer).await.unwrap(), (3, 1030));

        indexer.set(
            1500,
            vec![dispatch(1, &m0), dispatch(2, &m1), dispatch(25, &m2)],
        );
        assert_eq!(log.count_and_tip(&indexer).await.unwrap(), (3, 1500));
        assert_eq!(
            indexer.asked(),
            vec![
                (0, 1002),
                (1003 - REREAD_BLOCKS, 1030),
                (1031 - REREAD_BLOCKS, 1500)
            ],
            "the history once, then the blocks since and a few before them"
        );
    }

    #[tokio::test]
    async fn picks_up_a_message_the_indexer_served_late() {
        let (m0, m1) = (message(0), message(1));
        // The tip is at block 1005, but the answer comes from a copy that has
        // not reached the second message's block, 1004, yet.
        let indexer = Indexer::at(1005, vec![dispatch(1, &m0)]);
        let log = log();
        assert_eq!(log.count_and_tip(&indexer).await.unwrap(), (1, 1005));

        indexer.set(1006, vec![dispatch(1, &m0), dispatch(4, &m1)]);
        assert_eq!(
            log.count_and_tip(&indexer).await.unwrap(),
            (2, 1006),
            "a block read before is read again, and the message it holds is counted once"
        );
    }

    #[tokio::test]
    async fn keeps_its_count_when_the_indexer_answers_with_an_older_tip() {
        let indexer = Indexer::at(1005, vec![dispatch(1, &message(0))]);
        let log = log();
        assert_eq!(log.count_and_tip(&indexer).await.unwrap(), (1, 1005));

        *indexer.tip.lock().unwrap() = 1003;
        assert_eq!(
            log.count_and_tip(&indexer).await.unwrap(),
            (1, 1005),
            "the count still belongs to the last block read"
        );
    }

    #[tokio::test]
    async fn starts_over_after_a_tip_far_past_the_chain() {
        let (m0, m1) = (message(0), message(1));
        let indexer = Indexer::at(1010, vec![dispatch(1, &m0)]);
        let log = log();
        assert_eq!(log.count_and_tip(&indexer).await.unwrap(), (1, 1010));

        *indexer.tip.lock().unwrap() = 900_000;
        assert_eq!(log.count_and_tip(&indexer).await.unwrap(), (1, 900_000));

        // The chain is at block 1020, so the blocks since the newest held
        // message are read over.
        indexer.set(1020, vec![dispatch(1, &m0), dispatch(15, &m1)]);
        assert_eq!(log.count_and_tip(&indexer).await.unwrap(), (1, 1001));
        assert_eq!(log.count_and_tip(&indexer).await.unwrap(), (2, 1020));
        assert_eq!(indexer.asked().last(), Some(&(1002 - REREAD_BLOCKS, 1020)));
    }

    #[tokio::test]
    async fn a_message_past_the_next_nonce_is_an_error_and_is_read_again() {
        let (m0, m1, m2) = (message(0), message(1), message(2));
        let indexer = Indexer::at(1010, vec![dispatch(1, &m0), dispatch(3, &m2)]);
        let log = log();

        let err = log
            .count_and_tip(&indexer)
            .await
            .expect_err("nonce 1 is missing");
        let text = err.to_string();
        assert!(
            text.contains("the events of blocks 0 to 1010 have the message with nonce 2 in block 1003, where nonce 1 comes next"),
            "names the blocks and both nonces: {text}"
        );

        // Nothing of the broken read was kept, so the same blocks are read
        // again once the indexer serves every message.
        indexer.set(
            1010,
            vec![dispatch(1, &m0), dispatch(2, &m1), dispatch(3, &m2)],
        );
        assert_eq!(log.count_and_tip(&indexer).await.unwrap(), (3, 1010));
        assert_eq!(indexer.asked(), vec![(0, 1010), (0, 1010)]);
    }

    #[tokio::test]
    async fn a_gap_is_read_again_from_the_newest_message_held() {
        let (m0, m1, m2) = (message(0), message(1), message(2));
        let indexer = Indexer::at(1100, vec![dispatch(1, &m0)]);
        let log = log();
        assert_eq!(log.count_and_tip(&indexer).await.unwrap(), (1, 1100));

        // The second message, in block 1050, was never served, and is too far
        // back for the next read to cover. The third gives the gap away.
        indexer.set(1200, vec![dispatch(1, &m0), dispatch(150, &m2)]);
        log.count_and_tip(&indexer)
            .await
            .expect_err("nonce 1 is missing");

        indexer.set(
            1200,
            vec![dispatch(1, &m0), dispatch(50, &m1), dispatch(150, &m2)],
        );
        assert_eq!(log.count_and_tip(&indexer).await.unwrap(), (3, 1200));
        assert_eq!(
            indexer.asked().last(),
            Some(&(1002 - REREAD_BLOCKS, 1200)),
            "read again from the block after the newest message held"
        );
    }

    #[tokio::test]
    async fn a_message_from_a_block_that_was_not_asked_for_is_an_error() {
        let (m0, m1) = (message(0), message(1));
        // Block 1900 is past the tip the indexer named.
        let indexer = Indexer::at(1010, vec![dispatch(1, &m0)]);
        let log = log();
        assert_eq!(log.count_and_tip(&indexer).await.unwrap(), (1, 1010));

        let mut stray = dispatch(900, &m1);
        indexer.set(1020, vec![dispatch(1, &m0)]);
        indexer.stray.lock().unwrap().push(stray.clone());
        let err = log
            .count_and_tip(&indexer)
            .await
            .expect_err("block 1900 was not asked for");
        assert!(
            err.to_string().contains("nonce 1 in block 1900"),
            "names the block: {err}"
        );

        // Served from a block within the read, it is taken.
        stray.block_height = 1015;
        indexer.stray.lock().unwrap().clear();
        indexer.set(1020, vec![dispatch(1, &m0), stray]);
        assert_eq!(log.count_and_tip(&indexer).await.unwrap(), (2, 1020));
    }

    #[tokio::test]
    async fn a_dispatch_that_does_not_decode_is_an_error() {
        let mut bad = misc_event(1, HYP_DISPATCH, &[0xFFu8; 16]);
        bad.payload.truncate(16);
        let indexer = Indexer::at(1010, vec![bad]);
        let log = log();
        assert!(log.count_and_tip(&indexer).await.is_err());
    }

    #[tokio::test]
    async fn with_no_node_every_message_served_is_held() {
        let messages = [message(0), message(1), message(2)];
        let indexer = Indexer::at(
            1010,
            vec![
                dispatch(1, &messages[0]),
                dispatch(2, &messages[1]),
                dispatch(3, &messages[2]),
            ],
        );
        let log = log();
        let tree = log.tree(&indexer).await.unwrap();
        assert_eq!(tree, tree_of(&messages));

        // Nothing to hold a message to, so none is left out.
        let unknown = (7, H256::repeat_byte(0x77));
        let held = log.held(&indexer, vec![unknown], |m| *m).await.unwrap();
        assert_eq!(held, vec![unknown]);
    }

    #[tokio::test]
    async fn the_tree_holds_what_the_node_holds_too() {
        let messages = [message(0), message(1)];
        let indexer = Indexer::at(
            1010,
            vec![dispatch(1, &messages[0]), dispatch(2, &messages[1])],
        );
        let node = Node::new(
            1010,
            vec![(1001, held(&messages[..1])), (1002, held(&messages))],
        );
        let log = log_on(&node);

        let tree = log.tree(&indexer).await.unwrap();
        assert_eq!(tree, tree_of(&messages));
        assert_eq!(
            node.asked(),
            vec![1001..=1010],
            "from the first unchecked message's block to the last block read"
        );
    }

    #[tokio::test]
    async fn asks_the_node_only_when_there_is_a_new_message() {
        let (m0, m1) = (message(0), message(1));
        let indexer = Indexer::at(1010, vec![dispatch(1, &m0)]);
        let node = Node::new(
            2000,
            vec![
                (1001, held(std::slice::from_ref(&m0))),
                (1015, held(&[m0.clone(), m1.clone()])),
            ],
        );
        let log = log_on(&node);

        log.tree(&indexer).await.unwrap();
        assert_eq!(node.asked().len(), 1);

        // New blocks with no dispatch in them.
        indexer.set(1012, vec![dispatch(1, &m0)]);
        log.tree(&indexer).await.unwrap();
        assert_eq!(log.count_and_tip(&indexer).await.unwrap(), (1, 1012));
        assert_eq!(node.asked().len(), 1, "nothing to check, nothing asked");

        indexer.set(1020, vec![dispatch(1, &m0), dispatch(15, &m1)]);
        let tree = log.tree(&indexer).await.unwrap();
        assert_eq!(tree, tree_of(&[m0, m1]));
        assert_eq!(node.asked(), vec![1001..=1010, 1015..=1020]);
    }

    #[tokio::test]
    async fn an_empty_contract_has_an_empty_tree_and_asks_the_node_nothing() {
        let indexer = Indexer::at(1010, vec![]);
        let node = Node::new(1010, vec![]);
        let log = log_on(&node);
        let tree = log.tree(&indexer).await.unwrap();
        assert_eq!(tree.count(), 0);
        assert!(node.asked().is_empty());
    }

    #[tokio::test]
    async fn holds_a_message_once_the_node_has_finalized_it() {
        let messages = [message(0), message(1)];
        let indexer = Indexer::at(
            1010,
            vec![dispatch(1, &messages[0]), dispatch(5, &messages[1])],
        );
        let node = Node::new(
            1000,
            vec![(1001, held(&messages[..1])), (1005, held(&messages))],
        );
        let log = log_on(&node);

        // The node is behind the first message.
        let tree = log.tree(&indexer).await.unwrap();
        assert_eq!(tree.count(), 0, "nothing the node has not finalized");
        assert_eq!(
            log.count_and_tip(&indexer).await.unwrap(),
            (0, 1000),
            "the count stops before the first message not held"
        );

        // It has finalized the first message's block, not the second's.
        node.finalize(1003);
        let tree = log.tree(&indexer).await.unwrap();
        assert_eq!(tree, tree_of(&messages[..1]));
        assert_eq!(log.count_and_tip(&indexer).await.unwrap(), (1, 1004));
        let served = vec![(0, messages[0].id()), (1, messages[1].id())];
        assert_eq!(
            log.held(&indexer, served.clone(), |m| *m).await.unwrap(),
            served[..1],
            "the second message is not handed on yet"
        );

        node.finalize(1005);
        let tree = log.tree(&indexer).await.unwrap();
        assert_eq!(tree, tree_of(&messages));
        assert_eq!(log.count_and_tip(&indexer).await.unwrap(), (2, 1010));
    }

    #[tokio::test]
    async fn checks_at_the_last_block_read_when_the_node_is_ahead_of_the_indexer() {
        let messages = [message(0), message(1)];
        // The indexer has read up to block 1003; the node is past the second
        // message, in block 1005.
        let indexer = Indexer::at(1003, vec![dispatch(1, &messages[0])]);
        let node = Node::new(
            1008,
            vec![(1001, held(&messages[..1])), (1005, held(&messages))],
        );
        let log = log_on(&node);

        let tree = log.tree(&indexer).await.unwrap();
        assert_eq!(
            tree,
            tree_of(&messages[..1]),
            "held to the node's state at block 1003, not at its head"
        );
        assert_eq!(node.asked(), vec![1001..=1003]);
    }

    #[tokio::test]
    async fn a_message_the_node_does_not_hold_is_an_error_and_never_held() {
        let real = [message(0), message(1)];
        let fabricated = message(2);
        let indexer = Indexer::at(1010, vec![dispatch(1, &real[0]), dispatch(2, &real[1])]);
        let node = Node::new(1020, vec![(1001, held(&real[..1])), (1002, held(&real))]);
        let log = log_on(&node);
        let tree = log.tree(&indexer).await.unwrap();
        assert_eq!(tree, tree_of(&real));

        indexer.set(
            1015,
            vec![
                dispatch(1, &real[0]),
                dispatch(2, &real[1]),
                dispatch(12, &fabricated),
            ],
        );
        for _ in 0..2 {
            let err = log
                .tree(&indexer)
                .await
                .expect_err("the node holds two messages");
            let text = err.to_string();
            assert!(
                text.contains("indexer and node disagree: up to block 1015 the indexer's dispatch events make 3 messages")
                    && text.contains("the node holds 2 with"),
                "names the block and both counts: {text}"
            );
        }
        // A validator's database is filled from the count and the messages
        // handed on, so both are refused the same way.
        assert!(log.count_and_tip(&indexer).await.is_err());
        let served = vec![(1, real[1].id()), (2, fabricated.id())];
        assert!(log.held(&indexer, served, |m| *m).await.is_err());
    }

    #[tokio::test]
    async fn a_changed_message_is_an_error_and_never_held() {
        let real = [message(0), message(1)];
        let mut changed = message(1);
        changed.body[63] ^= 0x01;
        let indexer = Indexer::at(1010, vec![dispatch(1, &real[0]), dispatch(2, &changed)]);
        let node = Node::new(1010, vec![(1001, held(&real[..1])), (1002, held(&real))]);
        let log = log_on(&node);

        let err = log
            .tree(&indexer)
            .await
            .expect_err("the node holds another message at nonce 1");
        let text = err.to_string();
        assert!(
            text.contains("make 2 messages with running hash")
                && text.contains("the node holds 2 with"),
            "the counts agree and the hashes do not: {text}"
        );

        // Served the real message, the log holds both. The changed one is
        // then left out wherever it is served again.
        indexer.set(1010, vec![dispatch(1, &real[0]), dispatch(2, &real[1])]);
        assert_eq!(log.tree(&indexer).await.unwrap(), tree_of(&real));
        let served = vec![(0, real[0].id()), (1, changed.id())];
        assert_eq!(
            log.held(&indexer, served.clone(), |m| *m).await.unwrap(),
            served[..1]
        );
    }

    #[tokio::test]
    async fn a_message_the_indexer_left_out_is_an_error_until_it_is_served() {
        let real = [message(0), message(1)];
        // The indexer serves the second message's block, 1050, with no event
        // in it, too far back for the next read to cover.
        let indexer = Indexer::at(1100, vec![dispatch(1, &real[0])]);
        let node = Node::new(1100, vec![(1001, held(&real[..1])), (1050, held(&real))]);
        let log = log_on(&node);

        let err = log
            .tree(&indexer)
            .await
            .expect_err("the node holds one more message");
        assert!(
            err.to_string()
                .contains("make 1 messages with running hash"),
            "{err}"
        );

        // The blocks since the newest held message are read again.
        indexer.set(1101, vec![dispatch(1, &real[0]), dispatch(50, &real[1])]);
        let tree = log.tree(&indexer).await.unwrap();
        assert_eq!(tree, tree_of(&real));
    }

    #[tokio::test(start_paused = true)]
    async fn reads_over_again_until_the_indexer_serves_what_the_node_holds() {
        let real = [message(0), message(1), message(2)];
        let mut fabricated = message(2);
        fabricated.recipient = H256::repeat_byte(0xFA);
        let honest = vec![dispatch(1, &real[0]), dispatch(2, &real[1])];
        let lying = [honest.clone(), vec![dispatch(12, &fabricated)]].concat();
        let indexer = Indexer::at(1010, honest.clone());
        let node = Node::new(
            2000,
            vec![
                (1001, held(&real[..1])),
                (1002, held(&real[..2])),
                (1030, held(&real)),
            ],
        );
        let log = log_on(&node);
        assert_eq!(log.tree(&indexer).await.unwrap(), tree_of(&real[..2]));

        // The first disagreement starts the read over at once, and the
        // indexer serves the fabricated message again.
        indexer.set(1015, lying.clone());
        log.tree(&indexer).await.expect_err("fabricated");
        log.tree(&indexer).await.expect_err("fabricated again");
        assert_eq!(
            indexer.asked()[1..],
            [(1011 - REREAD_BLOCKS, 1015), (1003 - REREAD_BLOCKS, 1015)],
            "the second read starts after the newest held message"
        );

        // It no longer serves it, but the log read it within the last
        // minute, so it does not start over yet.
        indexer.set(1016, honest.clone());
        log.tree(&indexer)
            .await
            .expect_err("the fabricated message is still the one read");
        assert_eq!(indexer.asked().last(), Some(&(1016 - REREAD_BLOCKS, 1016)));

        tokio::time::advance(START_OVER_INTERVAL).await;
        log.tree(&indexer)
            .await
            .expect_err("this disagreement starts the read over");
        assert_eq!(log.tree(&indexer).await.unwrap(), tree_of(&real[..2]));
        assert_eq!(indexer.asked().last(), Some(&(1003 - REREAD_BLOCKS, 1016)));

        // A real message at the fabricated one's nonce is held like any other.
        indexer.set(1035, [honest, vec![dispatch(30, &real[2])]].concat());
        assert_eq!(log.tree(&indexer).await.unwrap(), tree_of(&real));
        assert_eq!(log.count_and_tip(&indexer).await.unwrap(), (3, 1035));
        let served = vec![(2, fabricated.id()), (2, real[2].id())];
        assert_eq!(
            log.held(&indexer, served.clone(), |m| *m).await.unwrap(),
            served[1..],
            "the fabricated message is never handed on"
        );
    }
}

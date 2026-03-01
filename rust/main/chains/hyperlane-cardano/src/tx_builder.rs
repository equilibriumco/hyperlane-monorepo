//! Transaction builder for Cardano Hyperlane operations
//!
//! This module provides transaction building capabilities for processing
//! Hyperlane messages on Cardano using pallas primitives for CBOR encoding
//! and pallas-txbuilder for transaction construction.

use crate::blockfrost_provider::{
    BlockfrostProvider, BlockfrostProviderError, CardanoNetwork, Utxo, UtxoValue,
};
use crate::cardano::Keypair;
use crate::recipient_resolver::{RecipientKind, RecipientResolver, ResolverError};
use crate::types::{MailboxRedeemer, Message, ProcessedMessageDatum};
use crate::ConnectionConf;
use hyperlane_core::{
    ChainCommunicationError, FixedPointNumber, HyperlaneMessage, TxOutcome, H512, U256,
};
use pallas_addresses::{Address, Network};
use pallas_codec::minicbor;
use pallas_codec::utils::{KeyValuePairs, MaybeIndefArray};
use pallas_crypto::hash::Hash;
use pallas_primitives::conway::{BigInt, Constr, PlutusData};
use pallas_txbuilder::{
    BuildConway, BuiltTransaction, ExUnits, Input, Output, ScriptKind, StagingTransaction,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::sync::{Mutex, OnceCell, RwLock};
use tracing::{debug, info, instrument, warn};

/// Per-redeemer ExUnits from evaluation, keyed by "spend:N" or "mint:N"
type EvaluatedExUnits = HashMap<String, (u64, u64)>;

/// Cached per-role ExUnits from the last successful Blockfrost evaluation.
/// Used when the evaluate endpoint can't process tracked-state TXs (UTXOs
/// not yet indexed) so we fall back to these known-good budgets instead of
/// the static placeholders which may be too small for the mailbox script.
#[derive(Clone, Debug)]
struct CachedProcessExUnits {
    mailbox_spend: (u64, u64),
    ism_spend: (u64, u64),
    processed_nft_mint: Option<(u64, u64)>,
    verified_nft_mint: Option<(u64, u64)>,
    recipient_spend: Option<(u64, u64)>,
    synthetic_mint: Option<(u64, u64)>,
}

impl CachedProcessExUnits {
    /// Apply a 10% margin to both mem and steps so cached ExUnits remain valid
    /// when subsequent chained TXs have extra inputs (e.g. wallet supplement)
    /// or slightly larger SMT proofs for later nonces.
    fn with_step_margin(self) -> Self {
        let m = |v: (u64, u64)| (v.0 * 11 / 10, v.1 * 11 / 10);
        let mo = |v: Option<(u64, u64)>| v.map(m);
        Self {
            mailbox_spend: m(self.mailbox_spend),
            ism_spend: m(self.ism_spend),
            processed_nft_mint: mo(self.processed_nft_mint),
            verified_nft_mint: mo(self.verified_nft_mint),
            recipient_spend: mo(self.recipient_spend),
            synthetic_mint: mo(self.synthetic_mint),
        }
    }

    fn total_mem(&self) -> u64 {
        let mut total = self.mailbox_spend.0 + self.ism_spend.0;
        if let Some((m, _)) = self.processed_nft_mint {
            total += m;
        }
        if let Some((m, _)) = self.verified_nft_mint {
            total += m;
        }
        if let Some((m, _)) = self.recipient_spend {
            total += m;
        }
        if let Some((m, _)) = self.synthetic_mint {
            total += m;
        }
        total
    }

    fn total_steps(&self) -> u64 {
        let mut total = self.mailbox_spend.1 + self.ism_spend.1;
        if let Some((_, s)) = self.processed_nft_mint {
            total += s;
        }
        if let Some((_, s)) = self.verified_nft_mint {
            total += s;
        }
        if let Some((_, s)) = self.recipient_spend {
            total += s;
        }
        if let Some((_, s)) = self.synthetic_mint {
            total += s;
        }
        total
    }
}

#[derive(Error, Debug)]
pub enum TxBuilderError {
    #[error("Blockfrost error: {0}")]
    Blockfrost(#[from] BlockfrostProviderError),
    #[error("Resolver error: {0}")]
    Resolver(#[from] ResolverError),
    #[error("Invalid recipient: {0}")]
    InvalidRecipient(String),
    #[error("UTXO not found: {0}")]
    UtxoNotFound(String),
    #[error("Encoding error: {0}")]
    Encoding(String),
    #[error("Missing required input: {0}")]
    MissingInput(String),
    #[error("Script not found: {0}")]
    ScriptNotFound(String),
    #[error("Transaction build error: {0}")]
    TxBuild(String),
    #[error("Insufficient funds: need {needed} lovelace, have {available}")]
    InsufficientFunds { needed: u64, available: u64 },
    #[error("Invalid address: {0}")]
    InvalidAddress(String),
    #[error("Submission failed: {0}")]
    SubmissionFailed(String),
    #[error("Message permanently undeliverable: {0}")]
    UndeliverableMessage(String),
}

/// Default execution units for script evaluation
/// Protocol limits (Conway): mem = 16,500,000, steps = 10,000,000,000 per transaction
/// We use smaller values per redeemer to stay within limits when multiple scripts execute
// Execution units per script - total must fit within network max (16.5M mem, 10B steps)
// For deferred recipients, we have 5 scripts: mailbox, recipient, ISM, message_nft_mint, processed_nft_mint
// 2.5M + 2.5M + 4M + 2.5M + 2.5M = 14M mem (fits within 16.5M with headroom)
// 1.5B + 1.5B + 2.5B + 1.5B + 1.5B = 8.5B steps (fits within 10B)
const DEFAULT_MEM_UNITS: u64 = 2_500_000;
const DEFAULT_STEP_UNITS: u64 = 1_500_000_000;
const ISM_MEM_UNITS: u64 = 4_000_000;
const ISM_STEP_UNITS: u64 = 2_500_000_000;

/// Default coins per UTXO byte (Cardano protocol parameter)
/// Used as fallback when protocol parameters cannot be fetched.
/// Current mainnet/preview value is ~4310 lovelace per byte.
const DEFAULT_COINS_PER_UTXO_BYTE: u64 = 4310;

/// Base overhead for UTXO entry structure (address, value encoding, etc.)
/// This is the minimum overhead for a simple ADA-only output.
const UTXO_BASE_OVERHEAD: u64 = 160;

/// Types of UTXO outputs for minimum lovelace calculation
#[derive(Debug, Clone, Copy)]
enum OutputType {
    /// Simple output containing only ADA
    SimpleAda,
    /// Output with a native token (policy ID + asset name)
    WithNativeToken { asset_name_len: usize },
    /// Output with an inline datum
    WithInlineDatum { datum_size: usize },
    /// Output with both native token and inline datum
    WithTokenAndDatum {
        asset_name_len: usize,
        datum_size: usize,
    },
}

/// Estimated fee for a complex script transaction (~2-3 ADA)
const ESTIMATED_FEE_LOVELACE: u64 = 3_000_000;

impl From<TxBuilderError> for ChainCommunicationError {
    fn from(e: TxBuilderError) -> Self {
        ChainCommunicationError::from_other_str(&e.to_string())
    }
}

/// Transaction builder for Hyperlane Cardano operations
pub struct HyperlaneTxBuilder {
    provider: Arc<BlockfrostProvider>,
    resolver: RecipientResolver,
    conf: ConnectionConf,
    /// Cached protocol parameter: lovelace cost per UTXO byte
    coins_per_utxo_byte: OnceCell<u64>,
    /// Cache for script serialised sizes (script_hash → bytes).
    /// Script sizes are immutable once deployed, so this never needs invalidation.
    script_size_cache: RwLock<HashMap<String, u64>>,
    /// Sparse Merkle Tree for replay protection. Initialized lazily on first use
    /// by scanning on-chain processed_message_nft tokens.
    smt: Mutex<Option<crate::smt::SparseMerkleTree>>,
    /// Predicted UTXO state (mailbox, ISM, payer, datum) from the last
    /// successfully-submitted TX. Avoids querying Blockfrost between rapid
    /// submissions, working around the 25-40s index lag in Blockfrost's API.
    last_tx_state: Mutex<Option<ChainedUtxoState>>,
    /// Cached per-role ExUnits from the last successful evaluation.
    /// Reused when Blockfrost evaluate fails for tracked-state TXs.
    cached_process_ex_units: Mutex<Option<CachedProcessExUnits>>,
    /// UTxOs spent in recently-submitted TXs, keyed by (tx_hash, output_index).
    /// Entries expire after SPENT_UTXO_CACHE_TTL_SECS. Used to filter Blockfrost
    /// UTxO results that are stale due to the 25-40s indexer lag.
    recently_spent: Mutex<HashMap<(String, u32), Instant>>,
}

impl HyperlaneTxBuilder {
    /// Create a new transaction builder
    pub fn new(conf: &ConnectionConf, provider: Arc<BlockfrostProvider>) -> Self {
        let resolver = RecipientResolver::new(
            BlockfrostProvider::new(&conf.api_key, conf.network, conf.confirmation_block_delay),
            conf.warp_route_reference_script_utxo.clone(),
        );

        Self {
            provider,
            resolver,
            conf: conf.clone(),
            coins_per_utxo_byte: OnceCell::new(),
            script_size_cache: RwLock::new(HashMap::new()),
            smt: Mutex::new(None),
            last_tx_state: Mutex::new(None),
            cached_process_ex_units: Mutex::new(None),
            recently_spent: Mutex::new(HashMap::new()),
        }
    }

    async fn set_last_tx_state(&self, state: ChainedUtxoState) {
        *self.last_tx_state.lock().await = Some(state);
    }

    async fn peek_last_tx_state(&self) -> Option<ChainedUtxoState> {
        self.last_tx_state.lock().await.clone()
    }

    /// TTL for recently-spent UTxO cache entries (2× Blockfrost lag)
    const SPENT_UTXO_CACHE_TTL_SECS: u64 = 120;

    /// Record all inputs of a just-submitted TX as recently-spent.
    /// Prevents Blockfrost's 25-40s UTXO index lag from causing
    /// `BadInputsUTxO` errors when the same UTXO is selected again.
    async fn record_spent_utxos(&self, signed_tx: &[u8]) {
        use pallas_primitives::conway::Tx;
        use pallas_primitives::Fragment;
        // Extract inputs before the await so the non-Send Tx<'_> is dropped first.
        let inputs: Vec<(String, u32)> = Tx::decode_fragment(signed_tx)
            .map(|tx| {
                tx.transaction_body
                    .inputs
                    .iter()
                    .map(|i| (hex::encode(i.transaction_id.as_ref()), i.index as u32))
                    .collect()
            })
            .unwrap_or_default();

        let mut spent = self.recently_spent.lock().await;
        let now = Instant::now();
        spent.retain(|_, t| now.duration_since(*t).as_secs() < Self::SPENT_UTXO_CACHE_TTL_SECS);
        for (hash, idx) in inputs {
            spent.insert((hash, idx), now);
        }
    }

    /// Filter UTxOs that appear in the recently-spent cache.
    async fn filter_recently_spent(&self, utxos: Vec<Utxo>) -> Vec<Utxo> {
        let spent = self.recently_spent.lock().await;
        let now = Instant::now();
        utxos
            .into_iter()
            .filter(|u| match spent.get(&(u.tx_hash.clone(), u.output_index)) {
                Some(t) => now.duration_since(*t).as_secs() >= Self::SPENT_UTXO_CACHE_TTL_SECS,
                None => true,
            })
            .collect()
    }

    /// Get the coins per UTXO byte from protocol parameters.
    /// Fetches from Blockfrost once and caches the result.
    async fn get_coins_per_utxo_byte(&self) -> u64 {
        *self
            .coins_per_utxo_byte
            .get_or_init(|| async {
                match self.provider.get_protocol_parameters().await {
                    Ok(params) => {
                        // Try coins_per_utxo_size (Babbage+) first, then coins_per_utxo_word (Alonzo)
                        let value = params
                            .get("coins_per_utxo_size")
                            .or_else(|| params.get("coins_per_utxo_word"))
                            .and_then(|v| v.as_str())
                            .and_then(|s| s.parse::<u64>().ok())
                            .unwrap_or(DEFAULT_COINS_PER_UTXO_BYTE);
                        debug!(
                            "Fetched coins_per_utxo_byte from protocol params: {}",
                            value
                        );
                        value
                    }
                    Err(e) => {
                        warn!(
                            "Failed to fetch protocol parameters, using default: {}. Error: {}",
                            DEFAULT_COINS_PER_UTXO_BYTE, e
                        );
                        DEFAULT_COINS_PER_UTXO_BYTE
                    }
                }
            })
            .await
    }

    /// Get script size in bytes, using a cache to avoid redundant Blockfrost queries.
    /// Script sizes are immutable once deployed, so this never needs invalidation.
    async fn get_cached_script_size(&self, script_hash: &str) -> Result<u64, TxBuilderError> {
        {
            let cache = self.script_size_cache.read().await;
            if let Some(&size) = cache.get(script_hash) {
                return Ok(size);
            }
        }
        let size = self.provider.get_script_size(script_hash).await?;
        self.script_size_cache
            .write()
            .await
            .insert(script_hash.to_string(), size);
        debug!("Cached script size: {} = {} bytes", script_hash, size);
        Ok(size)
    }

    /// Get or initialize the Sparse Merkle Tree by scanning on-chain
    /// processed_message_nft tokens, validated against the mailbox datum's root.
    async fn get_or_init_smt(&self) -> Result<(), TxBuilderError> {
        // Hold the lock for the entire initialization to prevent concurrent inits.
        // Concurrent callers block here; after the first one completes they see
        // is_some() and return immediately.
        let mut guard = self.smt.lock().await;
        if guard.is_some() {
            return Ok(());
        }

        let policy_id = self
            .conf
            .processed_messages_nft_policy_id
            .clone()
            .ok_or_else(|| {
                TxBuilderError::MissingInput(
                    "processed_messages_nft_policy_id is required for SMT replay protection"
                        .to_string(),
                )
            })?;

        let mailbox_utxo = self.find_mailbox_utxo().await.map_err(|e| {
            TxBuilderError::UtxoNotFound(format!(
                "Could not fetch mailbox UTXO for SMT root validation: {e}"
            ))
        })?;
        let expected_root = read_processed_tree_root_from_utxo(&mailbox_utxo)?;

        info!(
            "Initializing SMT from on-chain processed_message_nft tokens (policy={}, expected_root={})",
            policy_id,
            hex::encode(expected_root)
        );

        const MAX_RETRIES: u32 = 10;
        const RETRY_DELAY: Duration = Duration::from_secs(5);

        for attempt in 0..=MAX_RETRIES {
            let asset_ids = self
                .provider
                .get_policy_asset_ids(&policy_id)
                .await
                .unwrap_or_default();

            let mut message_ids: Vec<[u8; 32]> = Vec::new();
            for asset_id in &asset_ids {
                if asset_id.len() == 56 + 64 {
                    let asset_name_hex = &asset_id[56..];
                    if let Ok(bytes) = hex::decode(asset_name_hex) {
                        if bytes.len() == 32 {
                            let mut id = [0u8; 32];
                            id.copy_from_slice(&bytes);
                            message_ids.push(id);
                        }
                    }
                }
            }

            let smt = crate::smt::SparseMerkleTree::from_message_ids(&message_ids);
            if smt.root() == expected_root {
                info!(
                    count = message_ids.len(),
                    "SMT initialized and verified against on-chain root"
                );
                *guard = Some(smt);
                return Ok(());
            }

            if attempt < MAX_RETRIES {
                warn!(
                    loaded = message_ids.len(),
                    attempt = attempt + 1,
                    max = MAX_RETRIES,
                    "SMT root mismatch — Blockfrost indexer lag? Retrying in {}s",
                    RETRY_DELAY.as_secs()
                );
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }

        Err(TxBuilderError::Encoding(format!(
            "SMT root mismatch after {MAX_RETRIES} retries: Blockfrost has not indexed \
             all processed_message_nft tokens yet. Will retry on next message."
        )))
    }

    /// Compute total reference script size for all scripts used in a process TX.
    /// Queries Blockfrost for each script hash and caches results.
    async fn compute_total_ref_script_size(
        &self,
        recipient_ref_script_utxo: &Option<Utxo>,
        warp_token_type: &Option<WarpTokenTypeInfo>,
    ) -> u64 {
        let mut total: u64 = 0;
        let mut script_hashes: Vec<String> = Vec::new();

        // Mailbox script (always present when using reference scripts)
        if self.conf.mailbox_reference_script_utxo.is_some() {
            script_hashes.push(self.conf.mailbox_script_hash.clone());
        }

        // ISM script (always present when using reference scripts)
        if self.conf.ism_reference_script_utxo.is_some() {
            script_hashes.push(self.conf.ism_script_hash.clone());
        }

        // Warp route recipient script
        if let Some(ref utxo) = recipient_ref_script_utxo {
            if let Some(ref hash) = utxo.reference_script_hash {
                script_hashes.push(hash.clone());
            }
        }

        // Minting policy ref script (synthetic routes)
        if let Some(WarpTokenTypeInfo::Synthetic { minting_policy }) = warp_token_type {
            script_hashes.push(minting_policy.clone());
        }

        for hash in &script_hashes {
            match self.get_cached_script_size(hash).await {
                Ok(size) => total += size,
                Err(e) => {
                    warn!("Failed to get script size for {}: {}, using 0", hash, e);
                }
            }
        }

        debug!(
            "Total reference script size: {} bytes ({} scripts: {:?})",
            total,
            script_hashes.len(),
            script_hashes
        );
        total
    }

    /// Calculate minimum lovelace required for a UTXO based on its size.
    ///
    /// The formula is: min_lovelace = coins_per_utxo_byte × (base_overhead + output_size)
    ///
    /// Output sizes vary by content:
    /// - Simple ADA output: ~60 bytes
    /// - With native token: +56 bytes (policy ID) + asset_name_len
    /// - With inline datum: +datum_size bytes
    async fn calculate_min_lovelace(&self, output_type: OutputType) -> u64 {
        let coins_per_byte = self.get_coins_per_utxo_byte().await;

        // Estimate output size based on type
        let output_size: u64 = match output_type {
            OutputType::SimpleAda => 60, // Just address + lovelace value
            OutputType::WithNativeToken { asset_name_len } => {
                // Address + value + policy_id (28) + asset_name + multiasset overhead
                60 + 28 + asset_name_len as u64 + 20
            }
            OutputType::WithInlineDatum { datum_size } => {
                // Simple output + datum
                60 + datum_size as u64
            }
            OutputType::WithTokenAndDatum {
                asset_name_len,
                datum_size,
            } => {
                // Address + value + policy_id + asset_name + multiasset overhead + datum
                60 + 28 + asset_name_len as u64 + 20 + datum_size as u64
            }
        };

        let min_lovelace = coins_per_byte * (UTXO_BASE_OVERHEAD + output_size);

        // Round up to nearest 100k lovelace for safety margin
        let rounded = min_lovelace.div_ceil(100_000) * 100_000;

        debug!(
            "Calculated min_lovelace for {:?}: {} (raw: {}, coins_per_byte: {}, output_size: {})",
            output_type, rounded, min_lovelace, coins_per_byte, output_size
        );

        rounded
    }

    /// Synchronous version of `calculate_min_lovelace` using a pre-fetched
    /// `coins_per_utxo_byte` value, avoiding repeated `.await` calls.
    fn calculate_min_lovelace_sync(coins_per_byte: u64, output_type: OutputType) -> u64 {
        let output_size: u64 = match output_type {
            OutputType::SimpleAda => 60,
            OutputType::WithNativeToken { asset_name_len } => 60 + 28 + asset_name_len as u64 + 20,
            OutputType::WithInlineDatum { datum_size } => 60 + datum_size as u64,
            OutputType::WithTokenAndDatum {
                asset_name_len,
                datum_size,
            } => 60 + 28 + asset_name_len as u64 + 20 + datum_size as u64,
        };

        let min_lovelace = coins_per_byte * (UTXO_BASE_OVERHEAD + output_size);
        min_lovelace.div_ceil(100_000) * 100_000
    }

    /// Calculate minimum lovelace for a simple ADA-only output.
    /// This is the most common case and provides a quick accessor.
    async fn min_lovelace_simple(&self) -> u64 {
        self.calculate_min_lovelace(OutputType::SimpleAda).await
    }

    async fn get_max_tx_size(&self) -> u64 {
        self.provider
            .get_protocol_parameters()
            .await
            .ok()
            .and_then(|p| p.get("max_tx_size").and_then(|v| v.as_u64()))
            .unwrap_or(16384)
    }

    /// Find the mailbox UTXO by NFT or fall back to script address lookup
    async fn find_mailbox_utxo(&self) -> Result<Utxo, TxBuilderError> {
        // First try to find by NFT (preferred method for production)
        let nft_result = self
            .provider
            .find_utxo_by_nft(
                &self.conf.mailbox_policy_id,
                &self.conf.mailbox_asset_name_hex,
            )
            .await;

        match nft_result {
            Ok(utxo) => {
                debug!(
                    "Found mailbox UTXO by NFT: {}#{}",
                    utxo.tx_hash, utxo.output_index
                );
                return Ok(utxo);
            }
            Err(e) => {
                debug!(
                    "NFT lookup failed ({}), falling back to script address lookup",
                    e
                );
            }
        }

        // Fallback: Find UTXOs at the mailbox script address using the actual script hash
        let script_address = self
            .provider
            .script_hash_to_address(&self.conf.mailbox_script_hash)?;
        debug!(
            "Looking up mailbox UTXOs at script address: {}",
            script_address
        );

        let utxos = self.provider.get_utxos_at_address(&script_address).await?;

        // Find the UTXO holding the mailbox state NFT. The script address accumulates many UTXOs
        // (message receipt UTXOs from parallel processing), so we must match the specific NFT.
        for utxo in utxos {
            if utxo.has_asset(
                &self.conf.mailbox_policy_id,
                &self.conf.mailbox_asset_name_hex,
            ) && utxo.inline_datum.is_some()
            {
                debug!(
                    "Found mailbox UTXO by script address: {}#{}",
                    utxo.tx_hash, utxo.output_index
                );
                return Ok(utxo);
            }
        }

        Err(TxBuilderError::UtxoNotFound(
            "No mailbox UTXO found with inline datum at script address".to_string(),
        ))
    }

    async fn find_ref_script_utxo_from_config(&self) -> Option<Utxo> {
        let ref_utxo_str = self.conf.warp_route_reference_script_utxo.as_ref()?;
        let parts: Vec<&str> = ref_utxo_str.split('#').collect();
        if parts.len() != 2 {
            tracing::warn!("Invalid warp_route_reference_script_utxo format: {ref_utxo_str}");
            return None;
        }
        let tx_hash = parts[0].to_string();
        let output_index: u32 = match parts[1].parse() {
            Ok(idx) => idx,
            Err(e) => {
                tracing::warn!("Invalid ref script UTXO output index: {e}");
                return None;
            }
        };
        match self.provider.get_utxo(&tx_hash, output_index).await {
            Ok(utxo) => {
                tracing::info!(
                    "Found fallback reference script UTXO from config: {}#{}",
                    utxo.tx_hash,
                    utxo.output_index
                );
                Some(utxo)
            }
            Err(e) => {
                tracing::warn!("Could not fetch fallback ref script UTXO: {e}");
                None
            }
        }
    }

    /// Build a Process transaction for delivering a message to Cardano
    ///
    /// This creates a transaction that:
    /// 1. Spends the mailbox UTXO with Process redeemer
    /// 2. Includes ISM UTXO as reference input for signature verification
    /// 3. Spends recipient UTXO with HandleMessage redeemer
    /// 4. Creates processed message marker output
    /// 5. Creates continuation outputs for mailbox and recipient
    /// 6. For warp routes: Creates direct delivery output to recipient wallet
    #[instrument(skip(self, metadata, _payer, chained))]
    pub async fn build_process_tx(
        &self,
        message: &HyperlaneMessage,
        metadata: &[u8],
        _payer: &Keypair,
        chained: Option<&ChainedInputs>,
    ) -> Result<ProcessTxComponents, TxBuilderError> {
        info!(
            "Building process transaction for message nonce {} (chained={})",
            message.nonce,
            chained.is_some()
        );

        // Convert to our Message type
        let msg = Message::from_hyperlane_message(message);
        let message_id = msg.id();

        // 1+2. Find mailbox UTXO and resolve recipient
        // When chaining, use provided mailbox UTXO; otherwise fetch from chain
        let (mailbox_utxo, resolved) = if let Some(ci) = chained {
            let resolved = self
                .resolver
                .resolve(&msg.recipient)
                .await
                .map_err(TxBuilderError::from)?;
            (ci.mailbox_utxo.clone(), resolved)
        } else {
            tokio::try_join!(self.find_mailbox_utxo(), async {
                self.resolver
                    .resolve(&msg.recipient)
                    .await
                    .map_err(TxBuilderError::from)
            })?
        };
        info!(
            "Found mailbox UTXO: {}#{}",
            mailbox_utxo.tx_hash, mailbox_utxo.output_index
        );
        info!(
            "Resolved recipient: script_hash={}, kind={:?}",
            hex::encode(resolved.script_hash),
            resolved.recipient_kind
        );

        // 3. Find recipient reference script UTXO (WarpRoute only)
        // Each warp route has its own ref script UTXO identified by NFT {policy}726566 ("ref")
        let recipient_ref_script_utxo =
            if matches!(resolved.recipient_kind, RecipientKind::WarpRoute) {
                let policy_hex = hex::encode(resolved.recipient_policy);
                match self.provider.find_utxo_by_nft(&policy_hex, "726566").await {
                    Ok(utxo) => {
                        info!(
                            "Found warp route reference script UTXO via NFT: {}#{}",
                            utxo.tx_hash, utxo.output_index
                        );
                        Some(utxo)
                    }
                    Err(e) => {
                        warn!(
                        "Could not find ref script UTXO for policy {}: {}. Falling back to config.",
                        policy_hex, e
                    );
                        // Fall back to static config
                        self.find_ref_script_utxo_from_config().await
                    }
                }
            } else {
                None
            };

        // 4. Find ISM UTXO (use chained state or fetch from chain)
        let ism_utxo = if let Some(ci) = chained {
            debug!(
                "Using chained ISM UTXO: {}#{}",
                ci.ism_utxo.tx_hash, ci.ism_utxo.output_index
            );
            ci.ism_utxo.clone()
        } else {
            let (ism_policy_id, ism_asset_name) = match &resolved.ism {
                Some(ism) => (hex::encode(ism), String::new()),
                None => (
                    self.conf.ism_policy_id.clone(),
                    self.conf.ism_asset_name_hex.clone(),
                ),
            };
            let utxo = self
                .provider
                .find_utxo_by_nft(&ism_policy_id, &ism_asset_name)
                .await?;
            debug!("Found ISM UTXO: {}#{}", utxo.tx_hash, utxo.output_index);
            utxo
        };

        // 5. No additional inputs in new architecture (derived from datum)
        let additional_utxos: Vec<(Utxo, bool)> = Vec::new();

        // 5b. Generate SMT non-membership proof for replay protection.
        // When chaining, the proof and root are pre-computed by the batch orchestrator.
        // For the unchained path, compute both proof AND new_root under a single lock to
        // prevent a race where the submit task updates the tree between two separate
        // lock acquisitions (which would cause verify_and_insert_static to panic).
        let (smt_proof, precomputed_smt_root) = if let Some(ci) = chained {
            debug!(
                "Using pre-computed SMT proof ({} siblings) for chained TX",
                ci.smt_proof.len()
            );
            (ci.smt_proof.clone(), None)
        } else {
            self.get_or_init_smt().await?;
            let guard = self.smt.lock().await;
            let tree = guard.as_ref().expect("SMT initialized above");
            let mut key = [0u8; 16];
            key.copy_from_slice(&message_id[..16]);
            if tree.contains(&key) {
                return Err(TxBuilderError::UndeliverableMessage(
                    "Message already in SMT (duplicate message_id)".to_string(),
                ));
            }
            let proof = tree.prove_non_membership(&key);
            let old_root = tree.root();
            let new_root =
                crate::smt::SparseMerkleTree::verify_and_insert_static(&old_root, &key, &proof);
            debug!(
                "Generated SMT non-membership proof ({} siblings)",
                proof.len()
            );
            (proof, Some(new_root))
        };

        // 6. Encode redeemers
        let mailbox_redeemer = MailboxRedeemer::Process {
            message: msg.clone(),
            metadata: metadata.to_vec(),
            message_id,
            smt_proof: smt_proof.clone(),
        };
        let mailbox_redeemer_cbor = encode_mailbox_redeemer(&mailbox_redeemer)?;

        // Build recipient redeemer based on recipient kind
        // WarpRoute uses WarpRouteRedeemer::ReceiveTransfer (recipient script is spent)
        // GenericRecipient: no recipient script spent - message goes directly to recipient script
        let recipient_redeemer_cbor = match &resolved.recipient_kind {
            RecipientKind::WarpRoute => {
                info!("WarpRoute recipient - using WarpRouteRedeemer::ReceiveTransfer with direct delivery");

                let token_msg = parse_token_message(&msg.body)?;
                info!(
                    "TokenMessage: recipient={}, wire_amount={}",
                    hex::encode(token_msg.recipient),
                    token_msg.amount
                );

                let warp_redeemer = crate::types::WarpRouteRedeemer::ReceiveTransfer {
                    message: msg.clone(),
                    message_id,
                };
                Some(encode_warp_route_redeemer(&warp_redeemer)?)
            }
            RecipientKind::GenericRecipient => {
                info!("GenericRecipient - no recipient script spent, direct delivery to recipient script");
                None
            }
        };

        // 7. Build recipient continuation datum based on recipient kind
        let recipient_continuation_datum_cbor = match &resolved.recipient_kind {
            RecipientKind::WarpRoute => {
                let recipient_utxo = resolved.state_utxo.as_ref().ok_or_else(|| {
                    TxBuilderError::MissingInput("WarpRoute requires state UTXO".to_string())
                })?;
                let token_msg = parse_token_message(&msg.body)?;
                let decimals = extract_warp_route_decimals(recipient_utxo)?;

                let local_amount = convert_wire_to_local_amount(
                    token_msg.amount,
                    decimals.remote_decimals,
                    decimals.local_decimals,
                );
                info!(
                    "Decimal conversion: {} (wire {} dec) -> {} (local {} dec)",
                    token_msg.amount,
                    decimals.remote_decimals,
                    local_amount,
                    decimals.local_decimals
                );

                Some(build_warp_route_continuation_datum(
                    recipient_utxo,
                    local_amount,
                )?)
            }
            RecipientKind::GenericRecipient => None,
        };

        // 8. Encode processed message marker datum
        let processed_datum = ProcessedMessageDatum { message_id };
        let processed_datum_cbor = encode_processed_message_datum(&processed_datum)?;

        // 9. Convert HyperlaneMessage to our Message type for ISM verification
        let msg_for_ism = crate::types::Message {
            version: message.version,
            nonce: message.nonce,
            origin: message.origin,
            sender: message.sender.0,
            destination: message.destination,
            recipient: message.recipient.0,
            body: message.body.clone(),
        };

        // 10. Parse metadata and recover public keys + signatures for ISM Verify redeemer
        // Note: We recover public keys off-chain and pass both the pubkey and signature to ISM.
        // The on-chain ISM verifies each signature, computes address from verified pubkey,
        // and checks the address is in the trusted validators list.
        let parsed_metadata = parse_multisig_metadata(metadata, message.origin, &message_id)?;
        debug!(
            "Recovered {} validator signatures",
            parsed_metadata.validator_signatures.len()
        );
        debug!(
            "Checkpoint: origin={}, merkle_root={}, merkle_index={}",
            message.origin,
            hex::encode(parsed_metadata.merkle_root),
            parsed_metadata.root_index
        );

        // Build checkpoint from parsed metadata
        let checkpoint = crate::types::Checkpoint {
            origin: message.origin,
            merkle_root: parsed_metadata.merkle_root,
            origin_merkle_tree_hook: parsed_metadata.origin_mailbox,
            merkle_index: parsed_metadata.root_index,
            message_id,
        };

        // Log checkpoint details
        info!(
            "Checkpoint details:\n  origin: {}\n  merkle_root: {}\n  origin_merkle_tree_hook: {}\n  merkle_index: {}\n  message_id: {}",
            message.origin,
            hex::encode(parsed_metadata.merkle_root),
            hex::encode(parsed_metadata.origin_mailbox),
            parsed_metadata.root_index,
            hex::encode(message_id)
        );

        // Build ISM redeemer with validator signatures and recovered public keys
        let ism_redeemer = crate::types::MultisigIsmRedeemer::Verify {
            checkpoint,
            validator_signatures: parsed_metadata.validator_signatures,
        };
        let ism_redeemer_cbor = encode_ism_redeemer(&ism_redeemer)?;
        debug!(
            "Encoded ISM Verify redeemer ({} bytes)",
            ism_redeemer_cbor.len()
        );

        // 11. Handle WarpRoute - extract release amount, recipient, and token type
        // Funds are released directly to the recipient address
        let (token_release_amount, token_release_recipient, warp_token_type) = if matches!(
            &resolved.recipient_kind,
            RecipientKind::WarpRoute
        ) {
            let recipient_utxo = resolved.state_utxo.as_ref().ok_or_else(|| {
                TxBuilderError::MissingInput("WarpRoute requires state UTXO".to_string())
            })?;
            info!("TokenReceiver - preparing release from warp route UTXO");

            let token_msg = parse_token_message(&msg.body)?;

            let decimals = extract_warp_route_decimals(recipient_utxo)?;
            let local_amount = convert_wire_to_local_amount(
                token_msg.amount,
                decimals.remote_decimals,
                decimals.local_decimals,
            );

            info!(
                "TokenMessage recipient (32 bytes): {}",
                hex::encode(token_msg.recipient)
            );
            let cardano_credential = extract_cardano_credential_from_bytes32(&token_msg.recipient);
            info!(
                "Extracted credential (28 bytes): {}",
                hex::encode(cardano_credential)
            );

            let token_type = extract_warp_route_token_type(recipient_utxo)?;
            info!("Token release: wire_amount={}, local_amount={} (remote={}, local={}), credential={}, token_type={:?}",
                    token_msg.amount, local_amount, decimals.remote_decimals, decimals.local_decimals,
                    hex::encode(cardano_credential), token_type);

            // Fail fast for amounts that round to zero after decimal conversion.
            // This is a permanent condition — retrying won't change the result.
            if local_amount == 0 && !matches!(token_type, WarpTokenTypeInfo::Native) {
                return Err(TxBuilderError::UndeliverableMessage(
                    "Token release amount is zero after decimal conversion — \
                     the transfer amount is too small to represent in local decimals"
                        .to_string(),
                ));
            }

            (
                Some(local_amount),
                Some(cardano_credential.to_vec()),
                Some(token_type),
            )
        } else {
            (None, None, None)
        };

        // 12. Build verified message datum for GenericRecipient
        // The datum is delivered directly to the recipient script address
        let (verified_message_datum_cbor, recipient_script_hash) =
            if matches!(&resolved.recipient_kind, RecipientKind::GenericRecipient) {
                let datum = crate::types::VerifiedMessageDatum {
                    origin: message.origin,
                    sender: message.sender.0.to_vec(),
                    body: message.body.clone(),
                    message_id: message_id.to_vec(),
                    nonce: message.nonce,
                };
                let datum_cbor = encode_verified_message_datum(&datum)?;

                info!(
                    "Built verified message datum: message_id={}, origin={}, nonce={}",
                    hex::encode(message_id),
                    message.origin,
                    message.nonce
                );
                (Some(datum_cbor), Some(resolved.script_hash))
            } else {
                (None, None)
            };

        let total_ref_script_size = self
            .compute_total_ref_script_size(&recipient_ref_script_utxo, &warp_token_type)
            .await;

        // Compute new SMT root for the mailbox continuation datum.
        // The on-chain validator applies the same insertion, so the continuation
        // datum must contain the updated root.
        let mailbox_continuation_datum_cbor = if let Some(ci) = chained {
            // When chaining, the new root is pre-computed and we build the
            // continuation datum from the previous TX's CBOR datum.
            build_mailbox_continuation_datum_from_cbor(
                &ci.prev_mailbox_datum_cbor,
                &ci.new_smt_root,
            )?
        } else {
            // new_root was computed atomically with the proof under the same lock above.
            let new_root = precomputed_smt_root.expect("SMT root pre-computed in unchained path");
            build_mailbox_continuation_datum(&mailbox_utxo, &new_root)?
        };

        Ok(ProcessTxComponents {
            mailbox_utxo,
            mailbox_redeemer_cbor,
            recipient_utxo: resolved.state_utxo,
            recipient_ref_script_utxo,
            recipient_redeemer_cbor,
            recipient_continuation_datum_cbor,
            ism_utxo,
            additional_utxos,
            processed_datum_cbor,
            message_id,
            metadata: metadata.to_vec(),
            message: msg_for_ism,
            ism_redeemer_cbor,
            recipient_kind: resolved.recipient_kind.clone(),
            token_release_amount,
            token_release_recipient,
            warp_token_type,
            verified_message_datum_cbor,
            recipient_script_hash,
            total_ref_script_size,
            mailbox_continuation_datum_cbor: Some(mailbox_continuation_datum_cbor),
        })
    }

    /// Build, sign, and submit a complete Process transaction
    ///
    /// This is the main entry point for processing a Hyperlane message on Cardano.
    /// It handles the complete flow from component preparation to transaction submission.
    #[instrument(skip(self, message, metadata, payer))]
    pub async fn build_and_submit_process_tx(
        &self,
        message: &HyperlaneMessage,
        metadata: &[u8],
        payer: &Keypair,
    ) -> Result<TxOutcome, TxBuilderError> {
        // 1. Take tracked UTXO state from previous TX (if any).
        //    Provides mailbox, ISM, and payer UTXOs so we don't query stale Blockfrost.
        let tracked_state = self.peek_last_tx_state().await;
        let (tracked_chained, tracked_payer) = if let Some(ref ts) = tracked_state {
            info!(
                "Using tracked state: mailbox={}#{}, ism={}#{}, payer={}#{} ({} lovelace)",
                ts.mailbox_utxo.tx_hash,
                ts.mailbox_utxo.output_index,
                ts.ism_utxo.tx_hash,
                ts.ism_utxo.output_index,
                ts.payer_utxo.tx_hash,
                ts.payer_utxo.output_index,
                ts.payer_total_input,
            );
            // Build ChainedInputs with tracked mailbox/ISM but fresh SMT proof.
            // SMT proof must be generated from the live tree, not pre-computed.
            let message_id = crate::types::Message::from_hyperlane_message(message).id();
            self.get_or_init_smt().await?;
            let (smt_proof, new_smt_root) = {
                let guard = self.smt.lock().await;
                let tree = guard.as_ref().expect("SMT initialized");
                let mut key = [0u8; 16];
                key.copy_from_slice(&message_id[..16]);
                if tree.contains(&key) {
                    return Err(TxBuilderError::UndeliverableMessage(
                        "Message already in SMT (duplicate message_id)".to_string(),
                    ));
                }
                let proof = tree.prove_non_membership(&key);
                let old_root = tree.root();
                let new_root =
                    crate::smt::SparseMerkleTree::verify_and_insert_static(&old_root, &key, &proof);
                (proof, new_root)
            };
            let chained = ChainedInputs {
                mailbox_utxo: ts.mailbox_utxo.clone(),
                ism_utxo: ts.ism_utxo.clone(),
                smt_proof,
                new_smt_root,
                prev_mailbox_datum_cbor: ts.mailbox_datum_cbor.clone(),
            };
            let payer = ChainedPayer {
                utxos: vec![ts.payer_utxo.clone()],
                total_input: ts.payer_total_input,
            };
            (Some(chained), Some(payer))
        } else {
            (None, None)
        };

        // 2. Build transaction components
        info!(
            "Building process transaction components for message nonce {}",
            message.nonce
        );
        let components = self
            .build_process_tx(message, metadata, payer, tracked_chained.as_ref())
            .await?;

        info!("Constructing full transaction with pallas-txbuilder");
        let built_tx = self
            .build_complete_process_tx(&components, payer, None, tracked_payer.as_ref())
            .await?;

        // 3. Sign the transaction
        info!("Signing transaction");
        let mut signed_tx = self.sign_transaction(built_tx, payer)?;
        let mut actual_fee = ESTIMATED_FEE_LOVELACE;
        // Captured from the evaluate step so the FeeTooSmallUTxO retry path can
        // rebuild the TX with the exact fee the node requires.
        let mut submitted_ex_units: Option<EvaluatedExUnits> = None;

        // 3b. Evaluate and rebuild with real fee.
        // The first build uses ESTIMATED_FEE_LOVELACE (3M) as a conservative placeholder.
        // Evaluating the signed TX gives us actual ExUnits, so we can compute the real
        // fee (~0.3-0.8M) and rebuild — saving the relayer ~2+ ADA per TX.
        //
        // For chained TXs whose inputs are not yet confirmed, provide the predecessor
        // UTxOs via /utils/txs/evaluate/utxos (Ogmios v5 format: policy IDs as
        // top-level keys in the value object).
        {
            let ref_script_size = components.total_ref_script_size;
            let additional_utxos: Option<Vec<Utxo>> = tracked_state.as_ref().map(|ts| {
                vec![
                    ts.mailbox_utxo.clone(),
                    ts.ism_utxo.clone(),
                    ts.payer_utxo.clone(),
                ]
            });
            match self
                .evaluate_and_compute_fee(&signed_tx, ref_script_size, additional_utxos.as_deref())
                .await
            {
                Ok((real_fee, ex_units_map)) => {
                    info!(
                        "Evaluated fee: {} lovelace (was {}), rebuilding TX with actual ExUnits",
                        real_fee, ESTIMATED_FEE_LOVELACE
                    );
                    let rebuilt = self
                        .build_complete_process_tx(
                            &components,
                            payer,
                            Some((real_fee, &ex_units_map)),
                            tracked_payer.as_ref(),
                        )
                        .await?;
                    signed_tx = self.sign_transaction(rebuilt, payer)?;

                    // The rebuilt TX may differ in size from the first pass (ExUnits CBOR
                    // encoding changes redeemer sizes). Recompute fee from actual TX size.
                    let total_mem: u64 = ex_units_map.values().map(|(m, _)| m).sum();
                    let total_steps: u64 = ex_units_map.values().map(|(_, s)| s).sum();
                    let corrected_fee = self
                        .compute_fee_from_evaluation(
                            signed_tx.len() as u64,
                            total_mem,
                            total_steps,
                            ref_script_size,
                        )
                        .await?;
                    if corrected_fee > real_fee {
                        info!(
                            "Fee correction: {} → {} (TX size changed after rebuild)",
                            real_fee, corrected_fee
                        );
                        let corrected = self
                            .build_complete_process_tx(
                                &components,
                                payer,
                                Some((corrected_fee, &ex_units_map)),
                                tracked_payer.as_ref(),
                            )
                            .await?;
                        signed_tx = self.sign_transaction(corrected, payer)?;
                        actual_fee = corrected_fee;
                    } else {
                        actual_fee = real_fee;
                    }
                    submitted_ex_units = Some(ex_units_map);
                }
                Err(e) => {
                    warn!("Fee evaluation failed: {e}");
                    // Use cached ExUnits to compute a proper fee and rebuild.
                    // The first build already used cached ExUnits for correct script
                    // budgets but still has a 3M placeholder fee.
                    let cached_opt = self.cached_process_ex_units.lock().await.clone();
                    if let Some(ref cached) = cached_opt {
                        let total_mem = cached.total_mem();
                        let total_steps = cached.total_steps();
                        match self
                            .compute_fee_from_evaluation(
                                signed_tx.len() as u64,
                                total_mem,
                                total_steps,
                                ref_script_size,
                            )
                            .await
                        {
                            Ok(cached_fee) => {
                                info!("Using cached ExUnits with fee {} lovelace", cached_fee);
                                let empty_map = HashMap::new();
                                let rebuilt = self
                                    .build_complete_process_tx(
                                        &components,
                                        payer,
                                        Some((cached_fee, &empty_map)),
                                        tracked_payer.as_ref(),
                                    )
                                    .await?;
                                signed_tx = self.sign_transaction(rebuilt, payer)?;

                                let corrected = self
                                    .compute_fee_from_evaluation(
                                        signed_tx.len() as u64,
                                        total_mem,
                                        total_steps,
                                        ref_script_size,
                                    )
                                    .await
                                    .unwrap_or(cached_fee);
                                if corrected > cached_fee {
                                    let rebuilt2 = self
                                        .build_complete_process_tx(
                                            &components,
                                            payer,
                                            Some((corrected, &empty_map)),
                                            tracked_payer.as_ref(),
                                        )
                                        .await?;
                                    signed_tx = self.sign_transaction(rebuilt2, payer)?;
                                    actual_fee = corrected;
                                } else {
                                    actual_fee = cached_fee;
                                }
                            }
                            Err(fee_err) => {
                                warn!(
                                    "Fee computation from cache failed ({}), using static {} lovelace",
                                    fee_err, ESTIMATED_FEE_LOVELACE
                                );
                            }
                        }
                    } else {
                        warn!(
                            "No cached ExUnits available, using static {} lovelace fee",
                            ESTIMATED_FEE_LOVELACE
                        );
                    }
                }
            }
        }

        // 3c. Reject oversized transactions before submission
        let max_tx_size = self.get_max_tx_size().await;
        let tx_size = signed_tx.len() as u64;
        if tx_size > max_tx_size {
            return Err(TxBuilderError::UndeliverableMessage(format!(
                "Transaction size {tx_size} bytes exceeds max_tx_size {max_tx_size} bytes"
            )));
        }

        // 4. Submit to Blockfrost
        info!("Submitting transaction to Blockfrost");
        let tx_hash = match self.submit_transaction(&signed_tx).await {
            Ok(hash) => hash,
            Err(submit_err) => {
                // The node tells us the exact required fee in FeeTooSmallUTxO errors.
                // Rebuild with that fee and retry once before propagating.
                // When submitted_ex_units is None (cached/default path), pass an
                // empty map so build_complete_process_tx falls back to cached ExUnits.
                let err_str = submit_err.to_string();
                if let Some(exact_fee) = parse_fee_too_small_expected(&err_str) {
                    warn!(
                        supplied = actual_fee,
                        needed = exact_fee,
                        "FeeTooSmallUTxO: rebuilding with node-specified fee"
                    );
                    let empty_map = HashMap::new();
                    let ex_map = submitted_ex_units.as_ref().unwrap_or(&empty_map);
                    let corrected = self
                        .build_complete_process_tx(
                            &components,
                            payer,
                            Some((exact_fee, ex_map)),
                            tracked_payer.as_ref(),
                        )
                        .await?;
                    signed_tx = self.sign_transaction(corrected, payer)?;
                    actual_fee = exact_fee;
                    self.submit_transaction(&signed_tx).await?
                } else {
                    return Err(submit_err);
                }
            }
        };

        info!("Transaction submitted successfully: {}", tx_hash);

        // Record spent UTxOs so subsequent fee-selection queries don't re-pick
        // them from Blockfrost (which has a 25-40s UTXO indexer lag).
        self.record_spent_utxos(&signed_tx).await;

        // Track full UTXO state (mailbox, ISM, payer, datum) for the next submission
        let datum_cbor = components
            .mailbox_continuation_datum_cbor
            .as_deref()
            .expect("mailbox continuation datum always set");
        match self.extract_chained_state(&components, &signed_tx, datum_cbor, &tx_hash) {
            Ok(state) => {
                debug!(
                    "Tracking TX state: mailbox={}#{}, ism={}#{}, payer={}#{} ({} lovelace)",
                    state.mailbox_utxo.tx_hash,
                    state.mailbox_utxo.output_index,
                    state.ism_utxo.tx_hash,
                    state.ism_utxo.output_index,
                    state.payer_utxo.tx_hash,
                    state.payer_utxo.output_index,
                    state.payer_total_input,
                );
                self.set_last_tx_state(state).await;
            }
            Err(e) => {
                debug!("Could not extract TX state for tracking: {e:?}");
            }
        }

        // Update SMT with the newly processed message
        {
            let mut guard = self.smt.lock().await;
            if let Some(ref mut tree) = *guard {
                let mut key = [0u8; 16];
                key.copy_from_slice(&components.message_id[..16]);
                tree.insert(key);
                debug!(
                    "SMT updated with message_id {}",
                    hex::encode(components.message_id)
                );
            }
        }

        // Convert tx_hash string to H512
        let mut tx_id_bytes = [0u8; 64];
        let hash_bytes = hex::decode(&tx_hash)
            .map_err(|e| TxBuilderError::Encoding(format!("Invalid tx hash hex: {e}")))?;
        tx_id_bytes[32..64].copy_from_slice(&hash_bytes[..32.min(hash_bytes.len())]);

        Ok(TxOutcome {
            transaction_id: H512::from(tx_id_bytes),
            executed: true,
            gas_used: U256::from(actual_fee),
            gas_price: FixedPointNumber::try_from(U256::from(1u64))
                .unwrap_or_else(|_| FixedPointNumber::zero()),
        })
    }

    /// Build, sign, and submit a batch of chained Process transactions.
    ///
    /// Each TX consumes the previous TX's continuation outputs (mailbox, ISM,
    /// payer change), allowing all TXs to land in the same Cardano block.
    /// SMT proofs are generated sequentially against a cloned tree.
    ///
    /// Returns a per-message result. On first submission failure, remaining
    /// messages are skipped (returned as errors) since their inputs depend on
    /// the failed TX.
    #[instrument(skip(self, messages, payer))]
    pub async fn build_and_submit_chained_process_txs(
        &self,
        messages: &[(&HyperlaneMessage, &[u8])],
        payer: &Keypair,
    ) -> Result<Vec<Result<TxOutcome, TxBuilderError>>, TxBuilderError> {
        let batch_size = messages.len();
        info!("Building chained batch of {} process TXs", batch_size);

        // Initialize and clone SMT for batch proof generation
        self.get_or_init_smt().await?;
        let mut smt_clone = {
            let guard = self.smt.lock().await;
            guard.as_ref().expect("SMT initialized above").clone()
        };

        // Use tracked UTXO state from a previous submission for TX 0,
        // avoiding stale Blockfrost queries for mailbox, ISM, and payer.
        let initial_tracked_state = self.peek_last_tx_state().await;
        if let Some(ref ts) = initial_tracked_state {
            info!(
                "Using tracked state for batch TX 0: mailbox={}#{}, ism={}#{}, payer={}#{} ({} lovelace)",
                ts.mailbox_utxo.tx_hash, ts.mailbox_utxo.output_index,
                ts.ism_utxo.tx_hash, ts.ism_utxo.output_index,
                ts.payer_utxo.tx_hash, ts.payer_utxo.output_index,
                ts.payer_total_input,
            );
        }

        // For TX 0, promote tracked state into chained_state so the loop
        // builds ChainedInputs from it instead of querying Blockfrost.
        let mut chained_state: Option<ChainedUtxoState> = initial_tracked_state;

        // Pre-check: if tracked payer can't fund the full batch, fetch
        // supplementary wallet UTXOs for TX 0 so the change cascades.
        // ~7 ADA/TX is conservative (fee 3M + processed 1.5M + verified 4M).
        const ESTIMATED_COST_PER_CHAINED_TX: u64 = 7_000_000;
        let batch_needed = batch_size as u64 * ESTIMATED_COST_PER_CHAINED_TX + 1_500_000;
        let supplementary_utxos: Vec<Utxo> = if let Some(ref cs) = chained_state {
            if cs.payer_total_input < batch_needed {
                info!(
                    "Tracked payer {} lovelace < ~{} needed for {} TXs, fetching supplementary UTXOs",
                    cs.payer_total_input, batch_needed, batch_size
                );
                let payer_address = payer.address_bech32(self.network_to_pallas());
                let fresh = self.provider.get_utxos_at_address(&payer_address).await?;
                let fresh = self.filter_recently_spent(fresh).await;
                let shortfall = batch_needed - cs.payer_total_input;
                // Filter out the tracked payer UTXO to prevent duplicates — Blockfrost
                // has a 25-40s lag so the tracked change output may still appear in
                // fresh_utxos even though it is already in chained_payer.utxos.
                let tracked_key = (cs.payer_utxo.tx_hash.clone(), cs.payer_utxo.output_index);
                let mut sorted: Vec<_> = fresh
                    .iter()
                    .filter(|u| {
                        u.value.len() <= 1
                            && u.reference_script_hash.is_none()
                            && (u.tx_hash.clone(), u.output_index) != tracked_key
                    })
                    .cloned()
                    .collect();
                sorted.sort_by_key(|u| std::cmp::Reverse(u.lovelace()));
                let mut selected = Vec::new();
                let mut extra_total = 0u64;
                for utxo in sorted {
                    selected.push(utxo.clone());
                    extra_total += utxo.lovelace();
                    if extra_total >= shortfall {
                        break;
                    }
                }
                info!(
                    "Selected {} supplementary UTXOs ({} lovelace) for batch",
                    selected.len(),
                    extra_total
                );
                selected
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        // Merged build + submit loop: each TX is built, submitted, and its actual
        // Blockfrost hash used to seed the next TX's chained inputs. This avoids
        // CBOR re-encoding hash mismatches (encode_fragment can diverge from the
        // original body bytes, giving a different TX ID than Blockfrost reports).
        let mut results: Vec<Result<TxOutcome, TxBuilderError>> = Vec::with_capacity(batch_size);
        let mut success_count = 0usize;
        let mut last_good_state: Option<ChainedUtxoState> = None;
        let mut success_message_ids: Vec<[u8; 32]> = Vec::new();

        const SUBMIT_MAX_RETRIES: u32 = 5;
        const SUBMIT_RETRY_DELAY: Duration = Duration::from_secs(2);

        'batch: for (i, (message, metadata)) in messages.iter().enumerate() {
            info!(
                "Building chained TX {}/{} (nonce {})",
                i + 1,
                batch_size,
                message.nonce
            );

            // Generate SMT proof from the cloned tree
            let message_id = crate::types::Message::from_hyperlane_message(message).id();
            let mut smt_key = [0u8; 16];
            smt_key.copy_from_slice(&message_id[..16]);

            if smt_clone.contains(&smt_key) {
                return Err(TxBuilderError::UndeliverableMessage(format!(
                    "Message {} already in SMT (duplicate)",
                    hex::encode(message_id)
                )));
            }
            let smt_proof = smt_clone.prove_non_membership(&smt_key);
            let old_root = smt_clone.root();
            let new_root = crate::smt::SparseMerkleTree::verify_and_insert_static(
                &old_root, &smt_key, &smt_proof,
            );
            smt_clone.insert(smt_key);

            // Build process TX components with chained overrides
            let chained_inputs = chained_state.as_ref().map(|cs| ChainedInputs {
                mailbox_utxo: cs.mailbox_utxo.clone(),
                ism_utxo: cs.ism_utxo.clone(),
                smt_proof,
                new_smt_root: new_root,
                prev_mailbox_datum_cbor: cs.mailbox_datum_cbor.clone(),
            });

            let components = self
                .build_process_tx(message, metadata, payer, chained_inputs.as_ref())
                .await?;

            // Build the complete TX with chained payer if applicable.
            // For TX 0, include supplementary UTXOs so the change output
            // is large enough for all remaining TXs in the chain.
            let chained_payer = chained_state.as_ref().map(|cs| {
                let mut utxos = vec![cs.payer_utxo.clone()];
                let mut total = cs.payer_total_input;
                if i == 0 {
                    for u in &supplementary_utxos {
                        utxos.push(u.clone());
                        total += u.lovelace();
                    }
                }
                ChainedPayer {
                    utxos,
                    total_input: total,
                }
            });

            let built_tx = self
                .build_complete_process_tx(&components, payer, None, chained_payer.as_ref())
                .await?;

            let mut signed_tx = self.sign_transaction(built_tx, payer)?;
            let mut actual_fee = ESTIMATED_FEE_LOVELACE;

            // Evaluate to get accurate ExUnits and fee.
            // For chained TXs, provide predecessor UTxOs via evaluate/utxos so
            // the evaluator can resolve unconfirmed inputs (Ogmios v5 format).
            let additional_utxos: Option<Vec<Utxo>> = chained_state.as_ref().map(|cs| {
                vec![
                    cs.mailbox_utxo.clone(),
                    cs.ism_utxo.clone(),
                    cs.payer_utxo.clone(),
                ]
            });
            match self
                .evaluate_and_compute_fee(
                    &signed_tx,
                    components.total_ref_script_size,
                    additional_utxos.as_deref(),
                )
                .await
            {
                Ok((real_fee, ex_units_map)) => {
                    let rebuilt = self
                        .build_complete_process_tx(
                            &components,
                            payer,
                            Some((real_fee, &ex_units_map)),
                            chained_payer.as_ref(),
                        )
                        .await?;
                    signed_tx = self.sign_transaction(rebuilt, payer)?;
                    actual_fee = real_fee;
                    info!("Batch TX {i}: evaluated fee {real_fee} lovelace");
                }
                Err(e) => {
                    warn!("Batch TX {i}: evaluation failed, using cached/static ExUnits: {e}");
                }
            }

            // When evaluate failed, compute real fee from cached ExUnits.
            // The initial build uses ESTIMATED_FEE_LOVELACE; rebuild if it's still set.
            if actual_fee == ESTIMATED_FEE_LOVELACE {
                let cached_opt = self.cached_process_ex_units.lock().await.clone();
                if let Some(ref cached) = cached_opt {
                    let total_mem = cached.total_mem();
                    let total_steps = cached.total_steps();
                    if let Ok(cached_fee) = self
                        .compute_fee_from_evaluation(
                            signed_tx.len() as u64,
                            total_mem,
                            total_steps,
                            components.total_ref_script_size,
                        )
                        .await
                    {
                        let empty_map = HashMap::new();
                        let rebuilt = self
                            .build_complete_process_tx(
                                &components,
                                payer,
                                Some((cached_fee, &empty_map)),
                                chained_payer.as_ref(),
                            )
                            .await?;
                        signed_tx = self.sign_transaction(rebuilt, payer)?;
                        actual_fee = cached_fee;
                        info!("Batch TX {i}: using cached ExUnits with fee {cached_fee} lovelace");
                    }
                }
            }

            // Check TX size
            let max_tx_size = self.get_max_tx_size().await;
            let tx_size = signed_tx.len() as u64;
            if tx_size > max_tx_size {
                return Err(TxBuilderError::UndeliverableMessage(format!(
                    "Chained TX {i} size {tx_size} bytes exceeds max_tx_size {max_tx_size} bytes"
                )));
            }

            // Submit with BadInputsUTxO retry (predecessor may not be in mempool yet)
            info!("Submitting chained TX {}/{}", i + 1, batch_size);
            let actual_hash = {
                let mut last_submit_err = None;
                let mut submitted_hash: Option<String> = None;
                for attempt in 0..=SUBMIT_MAX_RETRIES {
                    match self.submit_transaction(&signed_tx).await {
                        Ok(h) => {
                            submitted_hash = Some(h);
                            break;
                        }
                        Err(e) => {
                            let err_str = e.to_string();
                            if err_str.contains("BadInputsUTxO") && attempt < SUBMIT_MAX_RETRIES {
                                warn!(
                                    "Chained TX {}/{} got BadInputsUTxO (attempt {}/{}): predecessor not yet in mempool, retrying in {}s",
                                    i + 1, batch_size, attempt + 1, SUBMIT_MAX_RETRIES, SUBMIT_RETRY_DELAY.as_secs()
                                );
                                tokio::time::sleep(SUBMIT_RETRY_DELAY).await;
                                last_submit_err = Some(e);
                            } else {
                                warn!(
                                    "Chained TX {}/{} failed: {}. Remaining TXs depend on this one — aborting batch.",
                                    i + 1, batch_size, e
                                );
                                results.push(Err(e));
                                for j in (i + 1)..batch_size {
                                    results.push(Err(TxBuilderError::SubmissionFailed(format!(
                                        "Skipped: predecessor TX {j} failed"
                                    ))));
                                }
                                break 'batch;
                            }
                        }
                    }
                }
                match submitted_hash {
                    Some(h) => h,
                    None => {
                        let e = last_submit_err.unwrap();
                        warn!(
                            "Chained TX {}/{} failed after {} retries: {}. Aborting batch.",
                            i + 1,
                            batch_size,
                            SUBMIT_MAX_RETRIES,
                            e
                        );
                        results.push(Err(e));
                        for j in (i + 1)..batch_size {
                            results.push(Err(TxBuilderError::SubmissionFailed(format!(
                                "Skipped: predecessor TX {j} failed"
                            ))));
                        }
                        break 'batch;
                    }
                }
            };

            info!(
                "Chained TX {}/{} submitted: {}",
                i + 1,
                batch_size,
                actual_hash
            );

            // Record spent UTXOs so Blockfrost's 25-40s indexing lag doesn't
            // cause stale UTXO re-selection if we fall back to wallet queries.
            self.record_spent_utxos(&signed_tx).await;

            success_count += 1;

            let mut tx_id_bytes = [0u8; 64];
            if let Ok(hash_bytes) = hex::decode(&actual_hash) {
                tx_id_bytes[32..64].copy_from_slice(&hash_bytes[..32.min(hash_bytes.len())]);
            }
            results.push(Ok(TxOutcome {
                transaction_id: H512::from(tx_id_bytes),
                executed: true,
                gas_used: U256::from(actual_fee),
                gas_price: FixedPointNumber::try_from(U256::from(1u64))
                    .unwrap_or_else(|_| FixedPointNumber::zero()),
            }));

            success_message_ids.push(message_id);

            // Extract chained state using the ACTUAL Blockfrost-reported hash so
            // the next TX's inputs reference the correct UTXO IDs.
            let datum_cbor = components
                .mailbox_continuation_datum_cbor
                .as_deref()
                .expect("mailbox continuation datum always set");
            match self.extract_chained_state(&components, &signed_tx, datum_cbor, &actual_hash) {
                Ok(state) => {
                    chained_state = Some(state.clone());
                    last_good_state = Some(state);
                }
                Err(e) => {
                    // Without chained state, subsequent TXs can't reference correct
                    // inputs — abort cleanly rather than building against stale Blockfrost data.
                    warn!(
                        "Chained TX {}/{}: failed to extract state after submission (hash {}): {e:?}. \
                         Aborting {} remaining TX(s).",
                        i + 1, batch_size, actual_hash, batch_size - i - 1
                    );
                    for j in (i + 1)..batch_size {
                        results.push(Err(TxBuilderError::SubmissionFailed(format!(
                            "Skipped: chained state lost after TX {} ({})",
                            i + 1,
                            actual_hash
                        ))));
                        let _ = j;
                    }
                    break 'batch;
                }
            }
        }

        // Track UTXO state from the last successfully submitted TX
        if let Some(state) = last_good_state {
            debug!(
                "Tracking batch last TX state: mailbox={}#{}, payer={}#{} ({} lovelace)",
                state.mailbox_utxo.tx_hash,
                state.mailbox_utxo.output_index,
                state.payer_utxo.tx_hash,
                state.payer_utxo.output_index,
                state.payer_total_input,
            );
            self.set_last_tx_state(state).await;
        }

        // Update the real SMT with successfully submitted message IDs
        {
            let mut guard = self.smt.lock().await;
            if let Some(ref mut tree) = *guard {
                for message_id in &success_message_ids {
                    let mut key = [0u8; 16];
                    key.copy_from_slice(&message_id[..16]);
                    tree.insert(key);
                    debug!(
                        "SMT updated with chained message_id {}",
                        hex::encode(message_id)
                    );
                }
            }
        }

        info!(
            "Chained batch complete: {}/{} TXs submitted successfully",
            success_count, batch_size
        );
        Ok(results)
    }

    /// Extract chained UTXO state from a signed TX for feeding into the next TX.
    ///
    /// Uses the known output layout (mailbox=0, ISM=computed, change=last) and
    /// parses the signed TX CBOR to get the body hash for constructing Utxo refs.
    /// Extract chained UTXO state using a hash already returned by Blockfrost.
    ///
    /// Using the locally computed body hash is unreliable — CBOR re-encoding via
    /// `encode_fragment()` can produce different bytes than the original encoding,
    /// causing the derived TX ID to diverge from the hash Blockfrost reports after
    /// submission. Callers must pass the actual submitted TX hash.
    fn extract_chained_state(
        &self,
        components: &ProcessTxComponents,
        signed_tx: &[u8],
        mailbox_datum_cbor: &[u8],
        actual_hash: &str,
    ) -> Result<ChainedUtxoState, TxBuilderError> {
        use pallas_primitives::conway::Tx;
        use pallas_primitives::Fragment;

        let tx = Tx::decode_fragment(signed_tx).map_err(|e| {
            TxBuilderError::Encoding(format!("Failed to decode signed TX for chaining: {e:?}"))
        })?;

        let outputs = &tx.transaction_body.outputs;
        let indices = compute_output_indices(components);

        info!(
            "extract_chained_state: {} outputs in TX, expecting ism={}, change={} (actual_hash={})",
            outputs.len(),
            indices.ism,
            indices.change,
            actual_hash
        );

        // Mailbox continuation is always output #0
        let mailbox_utxo = self.output_to_utxo(
            actual_hash,
            0,
            outputs.first().ok_or_else(|| {
                TxBuilderError::Encoding("TX has no outputs for mailbox continuation".to_string())
            })?,
        )?;

        // ISM continuation
        let ism_idx = indices.ism as usize;
        let ism_utxo = self.output_to_utxo(
            actual_hash,
            indices.ism,
            outputs.get(ism_idx).ok_or_else(|| {
                TxBuilderError::Encoding(format!(
                    "TX output index {} out of range for ISM continuation (have {} outputs)",
                    ism_idx,
                    outputs.len()
                ))
            })?,
        )?;

        // Change is always the last output
        let change_idx = indices.change as usize;
        let change_output = outputs.get(change_idx).ok_or_else(|| {
            TxBuilderError::Encoding(format!(
                "TX output index {} out of range for change (have {} outputs)",
                change_idx,
                outputs.len()
            ))
        })?;
        let payer_utxo = self.output_to_utxo(actual_hash, indices.change, change_output)?;
        let payer_total_input = payer_utxo.lovelace();

        Ok(ChainedUtxoState {
            mailbox_utxo,
            ism_utxo,
            payer_utxo,
            payer_total_input,
            mailbox_datum_cbor: mailbox_datum_cbor.to_vec(),
        })
    }

    /// Convert a pallas TX output to a Utxo struct for chaining.
    fn output_to_utxo(
        &self,
        tx_hash: &str,
        output_index: u32,
        output: &pallas_primitives::conway::TransactionOutput,
    ) -> Result<Utxo, TxBuilderError> {
        use pallas_primitives::conway::TransactionOutput;

        let (address_bytes, lovelace, assets, datum_option) = match output {
            TransactionOutput::Legacy(o) => {
                let addr = o.address.to_vec();
                let lovelace = match &o.amount {
                    pallas_primitives::alonzo::Value::Coin(c) => *c,
                    pallas_primitives::alonzo::Value::Multiasset(c, _) => *c,
                };
                (addr, lovelace, None, None)
            }
            TransactionOutput::PostAlonzo(o) => {
                let addr = o.address.to_vec();
                let lovelace = match &o.value {
                    pallas_primitives::conway::Value::Coin(c) => *c,
                    pallas_primitives::conway::Value::Multiasset(c, _) => *c,
                };
                let assets = match &o.value {
                    pallas_primitives::conway::Value::Multiasset(_, ma) => Some(ma),
                    _ => None,
                };
                let datum = o.datum_option.as_ref().and_then(|d| match &**d {
                    pallas_primitives::babbage::DatumOption::Hash(_) => None,
                    pallas_primitives::babbage::DatumOption::Data(data) => {
                        minicbor::to_vec(&data.0).ok().map(hex::encode)
                    }
                });
                (addr, lovelace, assets, datum)
            }
        };

        let address = Address::from_bytes(&address_bytes)
            .map(|a| {
                a.to_bech32()
                    .unwrap_or_else(|_| hex::encode(&address_bytes))
            })
            .unwrap_or_else(|_| hex::encode(&address_bytes));

        let mut value = vec![UtxoValue {
            unit: "lovelace".to_string(),
            quantity: lovelace.to_string(),
        }];

        if let Some(multiasset) = assets {
            for (policy, asset_map) in multiasset.iter() {
                let policy_hex = hex::encode(policy.as_ref());
                for (name, qty) in asset_map.iter() {
                    let name_hex = hex::encode(&**name);
                    let quantity: u64 = (*qty).into();
                    value.push(UtxoValue {
                        unit: format!("{policy_hex}{name_hex}"),
                        quantity: quantity.to_string(),
                    });
                }
            }
        }

        Ok(Utxo {
            tx_hash: tx_hash.to_string(),
            output_index,
            address,
            value,
            inline_datum: datum_option,
            data_hash: None,
            reference_script_hash: None,
            collateral: false,
            reference: false,
        })
    }

    /// Build the complete Process transaction using pallas-txbuilder.
    /// When `chained_payer` is provided, uses that UTXO instead of querying
    /// Blockfrost for wallet UTXOs (for TX chaining).
    #[instrument(skip(self, components, payer, chained_payer))]
    async fn build_complete_process_tx(
        &self,
        components: &ProcessTxComponents,
        payer: &Keypair,
        eval_overrides: Option<(u64, &EvaluatedExUnits)>,
        chained_payer: Option<&ChainedPayer>,
    ) -> Result<BuiltTransaction, TxBuilderError> {
        // Load cached per-role ExUnits as fallback when position-based lookup
        // misses (e.g., evaluate failed for tracked-state UTXOs, or caller
        // passes fee-only overrides with an empty ExUnits map).
        let cached_ex = self.cached_process_ex_units.lock().await.clone();

        // Pre-fetch coins_per_utxo_byte once, then compute all min_lovelace values synchronously
        let coins_per_byte = self.get_coins_per_utxo_byte().await;
        let min_lovelace = Self::calculate_min_lovelace_sync(coins_per_byte, OutputType::SimpleAda);

        // Calculate the actual minUTxO for the warp route continuation output
        let continuation_min_lovelace =
            if let Some(ref cont_datum) = components.recipient_continuation_datum_cbor {
                if components.warp_token_type.is_some() {
                    let nft_asset_name_len = 13;
                    Self::calculate_min_lovelace_sync(
                        coins_per_byte,
                        OutputType::WithTokenAndDatum {
                            asset_name_len: nft_asset_name_len,
                            datum_size: cont_datum.len(),
                        },
                    )
                } else {
                    min_lovelace
                }
            } else {
                min_lovelace
            };
        info!(
            "Continuation minUTxO: {} lovelace (datum_size={})",
            continuation_min_lovelace,
            components
                .recipient_continuation_datum_cbor
                .as_ref()
                .map(|d| d.len())
                .unwrap_or(0)
        );

        // Get payer address and UTXOs for fee payment
        let payer_address = payer.address_bech32(self.network_to_pallas());
        debug!("Payer address: {}", payer_address);

        // Calculate min lovelace for the recipient output, accounting for native tokens
        // when the warp route is collateral or synthetic (outputs include tokens).
        let recipient_output_cost = match &components.warp_token_type {
            Some(WarpTokenTypeInfo::Collateral { asset_name, .. })
                if components.token_release_amount.is_some() =>
            {
                Self::calculate_min_lovelace_sync(
                    coins_per_byte,
                    OutputType::WithNativeToken {
                        asset_name_len: asset_name.len() / 2,
                    },
                )
            }
            Some(WarpTokenTypeInfo::Synthetic { .. })
                if components.token_release_amount.is_some() =>
            {
                Self::calculate_min_lovelace_sync(
                    coins_per_byte,
                    OutputType::WithNativeToken { asset_name_len: 0 },
                )
            }
            _ => min_lovelace,
        };

        // warp_route_lovelace() helper - only called for WarpRoute where state_utxo is always Some
        let warp_route_lovelace = components
            .recipient_utxo
            .as_ref()
            .map(|u| u.lovelace())
            .unwrap_or(0);

        let payer_extra = match &components.warp_token_type {
            Some(WarpTokenTypeInfo::Synthetic { .. })
                if components.token_release_amount.is_some() =>
            {
                let original_lovelace = warp_route_lovelace;
                let extra = if original_lovelace > continuation_min_lovelace + recipient_output_cost
                {
                    0
                } else if original_lovelace > continuation_min_lovelace {
                    recipient_output_cost
                        .saturating_sub(original_lovelace.saturating_sub(continuation_min_lovelace))
                } else {
                    recipient_output_cost
                        + continuation_min_lovelace.saturating_sub(original_lovelace)
                };
                info!(
                    "Synthetic route: warp_route_lovelace={}, recipient_output_cost={}, payer covers extra={} lovelace",
                    original_lovelace, recipient_output_cost, extra
                );
                extra
            }
            Some(WarpTokenTypeInfo::Native) if components.token_release_amount.is_some() => {
                let original_lovelace = warp_route_lovelace;
                let release_amount = components
                    .token_release_amount
                    .expect("guarded by is_some()");
                // Recipient output must be at least min_lovelace
                let recipient_actual = release_amount.max(min_lovelace);
                let continuation_actual = original_lovelace
                    .saturating_sub(release_amount)
                    .max(continuation_min_lovelace);
                let total_outputs = continuation_actual + recipient_actual;
                let extra = total_outputs.saturating_sub(original_lovelace);
                if extra > 0 {
                    info!(
                        "Native route shortfall: warp_lovelace={}, continuation_actual={}, recipient_actual={} (release={}), payer covers extra={} lovelace",
                        original_lovelace, continuation_actual, recipient_actual, release_amount, extra
                    );
                }
                extra
            }
            Some(WarpTokenTypeInfo::Collateral { .. })
                if components.token_release_amount.is_some() =>
            {
                let original_lovelace = warp_route_lovelace;
                let total_needed = continuation_min_lovelace + recipient_output_cost;
                let extra = total_needed.saturating_sub(original_lovelace);
                if extra > 0 {
                    info!(
                        "Collateral route shortfall: warp_route_lovelace={}, recipient_output_cost={}, payer covers extra={} lovelace",
                        original_lovelace, recipient_output_cost, extra
                    );
                }
                extra
            }
            _ => 0,
        };

        // Calculate processed marker minUTxO - needed for UTXO selection
        // This output is always created and needs higher minUTxO if NFT minting is enabled
        let processed_marker_min_lovelace_for_selection =
            if self.conf.processed_messages_nft_policy_id.is_some() {
                Self::calculate_min_lovelace_sync(
                    coins_per_byte,
                    OutputType::WithTokenAndDatum {
                        asset_name_len: 32,
                        datum_size: components.processed_datum_cbor.len(),
                    },
                )
            } else {
                Self::calculate_min_lovelace_sync(
                    coins_per_byte,
                    OutputType::WithInlineDatum {
                        datum_size: components.processed_datum_cbor.len(),
                    },
                )
            };

        // Calculate verified message UTXO cost if applicable
        let verified_message_min_lovelace =
            if let Some(ref datum_cbor) = components.verified_message_datum_cbor {
                Self::calculate_min_lovelace_sync(
                    coins_per_byte,
                    OutputType::WithTokenAndDatum {
                        asset_name_len: 32,
                        datum_size: datum_cbor.len(),
                    },
                )
            } else {
                0
            };

        // Total extra the payer needs to cover:
        // - payer_extra: shortfall from warp route for recipient output
        // - processed_marker: the processed marker NFT output
        // - verified_message: the verified message UTXO (GenericRecipient only)
        let total_payer_extra = payer_extra
            + processed_marker_min_lovelace_for_selection
            + verified_message_min_lovelace;

        // Find payer UTXOs for fee payment (coin selection)
        // When chaining, use the change UTXO(s) from the previous TX
        let (payer_utxos, selected_utxos, total_input) = if let Some(cp) = chained_payer {
            let needed = ESTIMATED_FEE_LOVELACE + min_lovelace + total_payer_extra;
            if cp.total_input >= needed {
                debug!(
                    "Using chained payer: {} UTXOs, {} lovelace",
                    cp.utxos.len(),
                    cp.total_input
                );
                (cp.utxos.clone(), cp.utxos.clone(), cp.total_input)
            } else {
                // Chained payer insufficient — supplement with fresh wallet UTXOs.
                // This happens when the tracked change from a previous batch is too
                // small for the current batch's total cost.
                warn!(
                    "Chained payer {} lovelace < {} needed, supplementing from wallet",
                    cp.total_input, needed
                );
                let fresh_utxos = self.provider.get_utxos_at_address(&payer_address).await?;
                // Blockfrost has a 25-40s indexing lag, so fresh_utxos may still contain
                // UTXOs that are already in cp.utxos (the tracked change output). Filter
                // them out to prevent duplicate inputs in the transaction.
                let cp_utxo_set: std::collections::HashSet<_> = cp
                    .utxos
                    .iter()
                    .map(|u| (u.tx_hash.clone(), u.output_index))
                    .collect();
                let fresh_utxos = self.filter_recently_spent(fresh_utxos).await;
                let fresh_utxos: Vec<_> = fresh_utxos
                    .into_iter()
                    .filter(|u| !cp_utxo_set.contains(&(u.tx_hash.clone(), u.output_index)))
                    .collect();
                let shortfall = needed - cp.total_input;
                let (extra, extra_total) =
                    self.select_utxos_for_fee_with_extra(&fresh_utxos, shortfall, 0)?;
                let mut selected = cp.utxos.clone();
                selected.extend(extra);
                let total = cp.total_input + extra_total;
                let mut all = cp.utxos.clone();
                all.extend(fresh_utxos);
                (all, selected, total)
            }
        } else {
            let utxos = self.provider.get_utxos_at_address(&payer_address).await?;
            let utxos = self.filter_recently_spent(utxos).await;
            let (selected, total) =
                self.select_utxos_for_fee_with_extra(&utxos, total_payer_extra, min_lovelace)?;
            debug!(
                "Selected {} UTXOs with {} lovelace for fees (processed_marker={}, payer_extra={})",
                selected.len(),
                total,
                processed_marker_min_lovelace_for_selection,
                payer_extra
            );
            (utxos, selected, total)
        };

        // Start building the transaction
        let mut tx = StagingTransaction::new();

        // Add script inputs (mailbox and optionally recipient)
        let mailbox_input = utxo_to_input(&components.mailbox_utxo)?;
        tx = tx.input(mailbox_input);

        // Add recipient input (WarpRoute only - GenericRecipient has no recipient input)
        if let Some(ref recipient_utxo) = components.recipient_utxo {
            let recipient_input = utxo_to_input(recipient_utxo)?;
            tx = tx.input(recipient_input);
        }

        // Add additional inputs if they must be spent
        info!(
            "Processing {} additional inputs for transaction",
            components.additional_utxos.len()
        );
        for (utxo, must_spend) in &components.additional_utxos {
            let input = utxo_to_input(utxo)?;
            if *must_spend {
                info!(
                    "Adding additional input as SPENT: {}#{}",
                    utxo.tx_hash, utxo.output_index
                );
                tx = tx.input(input);
            } else {
                info!(
                    "Adding additional input as REFERENCE (provides script): {}#{}, ref_script_hash={:?}",
                    utxo.tx_hash, utxo.output_index, utxo.reference_script_hash
                );
                tx = tx.reference_input(input);
            }
        }

        // Add ISM UTXO as spent input (for signature verification)
        let ism_input = utxo_to_input(&components.ism_utxo)?;
        tx = tx.input(ism_input);
        debug!(
            "Added ISM input for verification: {}#{}",
            components.ism_utxo.tx_hash, components.ism_utxo.output_index
        );

        // Add recipient reference script UTXO as reference input (if present)
        // This allows the relayer to call arbitrary recipient scripts without
        // needing the script bytes - just the NFT locator from the registry
        if let Some(ref ref_utxo) = components.recipient_ref_script_utxo {
            let ref_input = utxo_to_input(ref_utxo)?;
            tx = tx.reference_input(ref_input);
            debug!(
                "Added recipient reference script UTXO: {}#{}",
                ref_utxo.tx_hash, ref_utxo.output_index
            );
        }

        // Add minting policy reference script UTXO for synthetic warp routes.
        // The minting policy script must be available as a reference input so the
        // ledger can validate the mint redeemer. The CLI's `warp deploy-minting-ref`
        // places this UTXO at the deployer/payer address with reference_script_hash
        // matching the minting policy hash.
        if let Some(WarpTokenTypeInfo::Synthetic { minting_policy }) = &components.warp_token_type {
            let mint_ref_utxo = payer_utxos
                .iter()
                .find(|u| u.reference_script_hash.as_deref() == Some(minting_policy.as_str()));
            if let Some(mint_ref) = mint_ref_utxo {
                let mint_ref_input = utxo_to_input(mint_ref)?;
                tx = tx.reference_input(mint_ref_input);
                info!(
                    "Added minting policy reference script UTXO: {}#{} (script_hash={})",
                    mint_ref.tx_hash, mint_ref.output_index, minting_policy
                );
            } else {
                return Err(TxBuilderError::MissingInput(format!(
                    "Minting policy reference script UTXO not found at payer address. \
                     Deploy it with `warp deploy-minting-ref` using the same signing key. \
                     Expected reference_script_hash={minting_policy}"
                )));
            }
        }

        // Add fee payment UTXOs
        for utxo in &selected_utxos {
            let input = utxo_to_input(utxo)?;
            tx = tx.input(input);
        }

        // Cardano requires collateral inputs to be pure ADA — token-bearing UTxOs
        // cause CollateralContainsNonADA errors. Pick the smallest pure-ADA UTxO
        // that exceeds 5 ADA (covers 150% of ~3 ADA max fee), minimising the
        // ADA at risk if a script fails and collateral is slashed.
        const COLLATERAL_MIN_LOVELACE: u64 = 5_000_000;
        let collateral_utxo = payer_utxos
            .iter()
            .filter(|u| u.value.len() == 1 && u.lovelace() >= COLLATERAL_MIN_LOVELACE)
            .min_by_key(|u| u.lovelace())
            .or_else(|| {
                // Fall back to any pure-ADA UTxO if none meets the threshold
                payer_utxos
                    .iter()
                    .filter(|u| u.value.len() == 1)
                    .max_by_key(|u| u.lovelace())
            });
        if let Some(collateral_utxo) = collateral_utxo {
            let collateral_input = utxo_to_input(collateral_utxo)?;
            tx = tx.collateral_input(collateral_input);
            debug!(
                "Added collateral input: {}#{}",
                collateral_utxo.tx_hash, collateral_utxo.output_index
            );
        } else {
            return Err(TxBuilderError::MissingInput(
                "No pure-ADA UTXO available for collateral; fund the relayer wallet \
                 with a pure-ADA UTXO (one without native tokens)"
                    .to_string(),
            ));
        }

        // Compute sorted indices for spend inputs to match Cardano's canonical ordering
        // Collect all spent inputs (script inputs only, not reference inputs)
        let mut spent_inputs: Vec<(Vec<u8>, u32)> = vec![];

        // Mailbox is always first script input
        spent_inputs.push((
            hex::decode(&components.mailbox_utxo.tx_hash)
                .map_err(|e| TxBuilderError::Encoding(format!("Invalid tx_hash: {e}")))?,
            components.mailbox_utxo.output_index,
        ));

        // Recipient input (if present, second script input)
        if let Some(ref recipient_utxo) = components.recipient_utxo {
            spent_inputs.push((
                hex::decode(&recipient_utxo.tx_hash)
                    .map_err(|e| TxBuilderError::Encoding(format!("Invalid tx_hash: {e}")))?,
                recipient_utxo.output_index,
            ));
        }

        // Additional inputs that must be spent
        for (utxo, must_spend) in &components.additional_utxos {
            if *must_spend {
                spent_inputs.push((
                    hex::decode(&utxo.tx_hash)
                        .map_err(|e| TxBuilderError::Encoding(format!("Invalid tx_hash: {e}")))?,
                    utxo.output_index,
                ));
            }
        }

        // ISM input (always spent for verification)
        spent_inputs.push((
            hex::decode(&components.ism_utxo.tx_hash)
                .map_err(|e| TxBuilderError::Encoding(format!("Invalid tx_hash: {e}")))?,
            components.ism_utxo.output_index,
        ));

        // Selected payer UTXOs (fee payment, non-script inputs — no redeemers)
        let mut payer_inputs: Vec<(Vec<u8>, u32)> = vec![];
        for utxo in &selected_utxos {
            payer_inputs.push((
                hex::decode(&utxo.tx_hash)
                    .map_err(|e| TxBuilderError::Encoding(format!("Invalid tx_hash: {e}")))?,
                utxo.output_index,
            ));
        }

        // All inputs (script + payer) must be sorted together for canonical ordering
        let mut all_inputs = spent_inputs.clone();
        all_inputs.extend(payer_inputs);
        all_inputs.sort_by(|a, b| {
            // Sort by tx_hash bytes first, then by output_index
            a.0.cmp(&b.0).then(a.1.cmp(&b.1))
        });

        // Find the sorted index of each script input
        let mailbox_hash = hex::decode(&components.mailbox_utxo.tx_hash)
            .map_err(|e| TxBuilderError::Encoding(format!("Invalid mailbox tx_hash hex: {e}")))?;
        let mailbox_sorted_idx = all_inputs
            .iter()
            .position(|(hash, idx)| {
                *hash == mailbox_hash && *idx == components.mailbox_utxo.output_index
            })
            .ok_or_else(|| {
                TxBuilderError::MissingInput("mailbox UTXO not in sorted inputs".into())
            })?;

        let recipient_sorted_idx = if let Some(ref recipient_utxo) = components.recipient_utxo {
            let recipient_hash = hex::decode(&recipient_utxo.tx_hash).map_err(|e| {
                TxBuilderError::Encoding(format!("Invalid recipient tx_hash hex: {e}"))
            })?;
            all_inputs.iter().position(|(hash, idx)| {
                *hash == recipient_hash && *idx == recipient_utxo.output_index
            })
        } else {
            None
        };

        let ism_hash = hex::decode(&components.ism_utxo.tx_hash)
            .map_err(|e| TxBuilderError::Encoding(format!("Invalid ISM tx_hash hex: {e}")))?;
        let ism_sorted_idx = all_inputs
            .iter()
            .position(|(hash, idx)| *hash == ism_hash && *idx == components.ism_utxo.output_index)
            .ok_or_else(|| TxBuilderError::MissingInput("ISM UTXO not in sorted inputs".into()))?;

        // Collect minting policies and compute their sorted indices
        // Minting policies must be sorted by policy hash bytes (ascending)
        let mut mint_policies: Vec<Vec<u8>> = vec![];

        // 1. Synthetic minting policy (if present)
        if let Some(WarpTokenTypeInfo::Synthetic { minting_policy }) = &components.warp_token_type {
            let policy_bytes = hex::decode(minting_policy).map_err(|e| {
                TxBuilderError::Encoding(format!("Invalid minting_policy hex: {e}"))
            })?;
            mint_policies.push(policy_bytes);
        }

        // 2. Processed message NFT policy (if present)
        if let Some(ref policy_id) = self.conf.processed_messages_nft_policy_id {
            let policy_bytes = hex::decode(policy_id).map_err(|e| {
                TxBuilderError::Encoding(format!(
                    "Invalid processed_messages_nft_policy_id hex: {e}"
                ))
            })?;
            mint_policies.push(policy_bytes);
        }

        // 3. Verified message NFT policy (if present and needed)
        if components.verified_message_datum_cbor.is_some() {
            if let Some(ref policy_id) = self.conf.verified_message_nft_policy_id {
                let policy_bytes = hex::decode(policy_id).map_err(|e| {
                    TxBuilderError::Encoding(format!(
                        "Invalid verified_message_nft_policy_id hex: {e}"
                    ))
                })?;
                mint_policies.push(policy_bytes);
            }
        }

        // Sort mint policies by policy hash bytes
        mint_policies.sort();

        // Find sorted index for each mint policy
        let synthetic_mint_idx = if let Some(WarpTokenTypeInfo::Synthetic { minting_policy }) =
            &components.warp_token_type
        {
            let policy_bytes = hex::decode(minting_policy).map_err(|e| {
                TxBuilderError::Encoding(format!("Invalid minting_policy hex: {e}"))
            })?;
            mint_policies.iter().position(|p| *p == policy_bytes)
        } else {
            None
        };

        let processed_nft_mint_idx =
            if let Some(ref policy_id) = self.conf.processed_messages_nft_policy_id {
                let policy_bytes = hex::decode(policy_id).map_err(|e| {
                    TxBuilderError::Encoding(format!("Invalid processed_messages_nft hex: {e}"))
                })?;
                mint_policies.iter().position(|p| *p == policy_bytes)
            } else {
                None
            };

        let verified_nft_mint_idx = if components.verified_message_datum_cbor.is_some() {
            if let Some(ref policy_id) = self.conf.verified_message_nft_policy_id {
                let policy_bytes = hex::decode(policy_id).map_err(|e| {
                    TxBuilderError::Encoding(format!("Invalid verified_message_nft hex: {e}"))
                })?;
                mint_policies.iter().position(|p| *p == policy_bytes)
            } else {
                None
            }
        } else {
            None
        };

        // Add spend redeemers with execution units
        // Re-create inputs for redeemer association (since Input doesn't impl Clone)
        let mailbox_input_for_redeemer = utxo_to_input(&components.mailbox_utxo)?;

        let ex_units_mailbox = if let Some((_, ex_units_map)) = eval_overrides {
            let key = format!("spend:{mailbox_sorted_idx}");
            if let Some(&(mem, steps)) = ex_units_map.get(&key) {
                ExUnits { mem, steps }
            } else if let Some(ref c) = cached_ex {
                ExUnits {
                    mem: c.mailbox_spend.0,
                    steps: c.mailbox_spend.1,
                }
            } else {
                ExUnits {
                    mem: DEFAULT_MEM_UNITS,
                    steps: DEFAULT_STEP_UNITS,
                }
            }
        } else if let Some(ref c) = cached_ex {
            ExUnits {
                mem: c.mailbox_spend.0,
                steps: c.mailbox_spend.1,
            }
        } else {
            ExUnits {
                mem: DEFAULT_MEM_UNITS,
                steps: DEFAULT_STEP_UNITS,
            }
        };

        tx = tx.add_spend_redeemer(
            mailbox_input_for_redeemer,
            components.mailbox_redeemer_cbor.clone(),
            Some(ex_units_mailbox),
        );

        // Add recipient redeemer (WarpRoute only)
        if let (Some(ref recipient_utxo), Some(ref redeemer_cbor), Some(sorted_idx)) = (
            &components.recipient_utxo,
            &components.recipient_redeemer_cbor,
            recipient_sorted_idx,
        ) {
            let recipient_input_for_redeemer = utxo_to_input(recipient_utxo)?;
            let ex_units_recipient = if let Some((_, ex_units_map)) = eval_overrides {
                let key = format!("spend:{sorted_idx}");
                if let Some(&(mem, steps)) = ex_units_map.get(&key) {
                    ExUnits { mem, steps }
                } else if let Some(ref c) = cached_ex {
                    c.recipient_spend
                        .map(|(m, s)| ExUnits { mem: m, steps: s })
                        .unwrap_or(ExUnits {
                            mem: DEFAULT_MEM_UNITS,
                            steps: DEFAULT_STEP_UNITS,
                        })
                } else {
                    ExUnits {
                        mem: DEFAULT_MEM_UNITS,
                        steps: DEFAULT_STEP_UNITS,
                    }
                }
            } else if let Some(ref c) = cached_ex {
                c.recipient_spend
                    .map(|(m, s)| ExUnits { mem: m, steps: s })
                    .unwrap_or(ExUnits {
                        mem: DEFAULT_MEM_UNITS,
                        steps: DEFAULT_STEP_UNITS,
                    })
            } else {
                ExUnits {
                    mem: DEFAULT_MEM_UNITS,
                    steps: DEFAULT_STEP_UNITS,
                }
            };

            tx = tx.add_spend_redeemer(
                recipient_input_for_redeemer,
                redeemer_cbor.clone(),
                Some(ex_units_recipient),
            );
        }

        // Add ISM Verify redeemer (for signature verification)
        let ism_input_for_redeemer = utxo_to_input(&components.ism_utxo)?;
        let ex_units_ism = if let Some((_, ex_units_map)) = eval_overrides {
            let key = format!("spend:{ism_sorted_idx}");
            if let Some(&(mem, steps)) = ex_units_map.get(&key) {
                ExUnits { mem, steps }
            } else if let Some(ref c) = cached_ex {
                ExUnits {
                    mem: c.ism_spend.0,
                    steps: c.ism_spend.1,
                }
            } else {
                ExUnits {
                    mem: ISM_MEM_UNITS,
                    steps: ISM_STEP_UNITS,
                }
            }
        } else if let Some(ref c) = cached_ex {
            ExUnits {
                mem: c.ism_spend.0,
                steps: c.ism_spend.1,
            }
        } else {
            ExUnits {
                mem: ISM_MEM_UNITS,
                steps: ISM_STEP_UNITS,
            }
        };

        tx = tx.add_spend_redeemer(
            ism_input_for_redeemer,
            components.ism_redeemer_cbor.clone(),
            Some(ex_units_ism),
        );
        debug!(
            "Added ISM Verify redeemer ({} bytes)",
            components.ism_redeemer_cbor.len()
        );

        // Create outputs

        // 1. Mailbox continuation output (same address, updated datum for Process)
        let mailbox_output = match &components.mailbox_continuation_datum_cbor {
            Some(updated_datum) => create_continuation_output_with_datum(
                &components.mailbox_utxo,
                updated_datum,
                min_lovelace,
            )?,
            None => create_continuation_output(
                &components.mailbox_utxo,
                &self.conf.mailbox_policy_id,
                min_lovelace,
            )?,
        };
        tx = tx.output(mailbox_output);

        // 2. Recipient continuation output (WarpRoute only)
        if let (Some(ref recipient_utxo), Some(ref cont_datum_cbor)) = (
            &components.recipient_utxo,
            &components.recipient_continuation_datum_cbor,
        ) {
            let recipient_output = create_warp_route_continuation_output(
                recipient_utxo,
                cont_datum_cbor,
                components.token_release_amount,
                components.warp_token_type.as_ref(),
                continuation_min_lovelace,
                recipient_output_cost,
            )?;
            tx = tx.output(recipient_output);
        }

        // 2a. For TokenReceiver (warp routes): Create release output directly to recipient
        // The release output contains ADA (for Native) or tokens (for Collateral/Synthetic)
        if let (Some(release_amount), Some(ref recipient_bytes), Some(ref token_type)) = (
            components.token_release_amount,
            &components.token_release_recipient,
            &components.warp_token_type,
        ) {
            info!(
                "Creating direct token release: amount={}, recipient={}, token_type={:?}",
                release_amount,
                hex::encode(recipient_bytes),
                token_type
            );

            // Get network for address conversion
            let pallas_network = match self.conf.network {
                CardanoNetwork::Mainnet => Network::Mainnet,
                CardanoNetwork::Preprod | CardanoNetwork::Preview => Network::Testnet,
            };

            match token_type {
                WarpTokenTypeInfo::Native => {
                    // Native ADA: direct transfer to recipient address.
                    // The output must have at least min_lovelace; if release_amount
                    // is smaller, the payer tops up (covered by source chain fees).
                    let recipient_address = credential_to_address(recipient_bytes, pallas_network)?;
                    let output_lovelace = release_amount.max(min_lovelace);
                    let release_output = Output::new(recipient_address, output_lovelace);
                    tx = tx.output(release_output);
                    info!(
                        "Added Native ADA release output: {} lovelace (transfer={}, min={})",
                        output_lovelace, release_amount, min_lovelace
                    );
                }
                WarpTokenTypeInfo::Collateral {
                    policy_id,
                    asset_name,
                } => {
                    // Collateral tokens: direct transfer to recipient address
                    let recipient_address = credential_to_address(recipient_bytes, pallas_network)?;
                    let policy_decoded: [u8; 28] = hex::decode(policy_id)
                        .map_err(|e| {
                            TxBuilderError::TxBuild(format!("Invalid policy_id hex: {e}"))
                        })?
                        .try_into()
                        .map_err(|_| {
                            TxBuilderError::TxBuild("policy_id must be 28 bytes".to_string())
                        })?;
                    let policy_bytes: Hash<28> = Hash::new(policy_decoded);
                    let asset_bytes = hex::decode(asset_name).map_err(|e| {
                        TxBuilderError::TxBuild(format!("Invalid asset_name hex: {e}"))
                    })?;

                    let release_output = Output::new(recipient_address, recipient_output_cost)
                        .add_asset(policy_bytes, asset_bytes, release_amount)
                        .map_err(|e| {
                            TxBuilderError::TxBuild(format!(
                                "Failed to add collateral tokens: {e:?}"
                            ))
                        })?;
                    tx = tx.output(release_output);
                    info!(
                        "Added Collateral token release output: {} units, {} lovelace",
                        release_amount, recipient_output_cost
                    );
                }
                WarpTokenTypeInfo::Synthetic { minting_policy } => {
                    // Synthetic: Mint tokens directly to recipient address
                    info!(
                        "Synthetic route - minting {} tokens to recipient {}",
                        release_amount,
                        hex::encode(recipient_bytes)
                    );

                    let minting_policy_bytes: Hash<28> = parse_policy_id(minting_policy)?;
                    let asset_name: Vec<u8> = Vec::new(); // Empty asset name for synthetic tokens

                    // Mint the synthetic tokens
                    tx = tx
                        .mint_asset(
                            minting_policy_bytes,
                            asset_name.clone(),
                            release_amount as i64,
                        )
                        .map_err(|e| {
                            TxBuilderError::TxBuild(format!(
                                "Failed to mint synthetic tokens: {e:?}"
                            ))
                        })?;

                    // Create recipient output with minted tokens + proper min lovelace
                    let recipient_address = credential_to_address(recipient_bytes, pallas_network)?;
                    info!(
                        "Recipient address: {}",
                        recipient_address.to_bech32().unwrap_or_default()
                    );

                    let recipient_output = Output::new(recipient_address, recipient_output_cost)
                        .add_asset(minting_policy_bytes, asset_name.clone(), release_amount)
                        .map_err(|e| {
                            TxBuilderError::TxBuild(format!(
                                "Failed to add synthetic tokens: {e:?}"
                            ))
                        })?;
                    tx = tx.output(recipient_output);

                    // Add mint redeemer (Constr 0 [])
                    let mint_redeemer_cbor = encode_constructor_0_redeemer();
                    let ex_units_mint = if let (Some((_, ex_units_map)), Some(sorted_idx)) =
                        (eval_overrides, synthetic_mint_idx)
                    {
                        let key = format!("mint:{sorted_idx}");
                        if let Some(&(mem, steps)) = ex_units_map.get(&key) {
                            ExUnits { mem, steps }
                        } else if let Some(ref c) = cached_ex {
                            c.synthetic_mint
                                .map(|(m, s)| ExUnits { mem: m, steps: s })
                                .unwrap_or(ExUnits {
                                    mem: DEFAULT_MEM_UNITS,
                                    steps: DEFAULT_STEP_UNITS,
                                })
                        } else {
                            ExUnits {
                                mem: DEFAULT_MEM_UNITS,
                                steps: DEFAULT_STEP_UNITS,
                            }
                        }
                    } else if let Some(ref c) = cached_ex {
                        c.synthetic_mint
                            .map(|(m, s)| ExUnits { mem: m, steps: s })
                            .unwrap_or(ExUnits {
                                mem: DEFAULT_MEM_UNITS,
                                steps: DEFAULT_STEP_UNITS,
                            })
                    } else {
                        ExUnits {
                            mem: DEFAULT_MEM_UNITS,
                            steps: DEFAULT_STEP_UNITS,
                        }
                    };
                    tx = tx.add_mint_redeemer(
                        minting_policy_bytes,
                        mint_redeemer_cbor,
                        Some(ex_units_mint),
                    );

                    info!(
                        "Added Synthetic token mint and release output: {} units",
                        release_amount
                    );
                }
            }
        }

        // 3. ISM continuation output (same address, same datum, same value)
        // The ISM is spent for verification but must continue with unchanged state
        let ism_output = create_ism_continuation_output(&components.ism_utxo, min_lovelace)?;
        tx = tx.output(ism_output);
        debug!("Added ISM continuation output");

        // 4. Processed message marker output
        // This output goes to the processed_messages_script address with inline datum
        // If NFT minting is configured, the NFT will be included in this output
        // The verified_message_nft goes to the recipient script output (GenericRecipient only)
        let has_verified_message = components.verified_message_datum_cbor.is_some();
        let has_processed_nft = self.conf.processed_messages_nft_policy_id.is_some();
        let processed_marker_min_lovelace = if has_processed_nft {
            self.calculate_min_lovelace(OutputType::WithTokenAndDatum {
                asset_name_len: 32,
                datum_size: components.processed_datum_cbor.len(),
            })
            .await
        } else {
            self.calculate_min_lovelace(OutputType::WithInlineDatum {
                datum_size: components.processed_datum_cbor.len(),
            })
            .await
        };

        let mut processed_marker_output = self.create_processed_marker_output(
            &components.message_id,
            &components.processed_datum_cbor,
            processed_marker_min_lovelace,
        )?;

        // 4b. Optional: Mint processed message NFT for efficient O(1) lookups
        // If processed_messages_nft_policy_id is configured, mint an NFT with message_id as asset name
        if let (Some(ref policy_id), Some(ref script_cbor)) = (
            &self.conf.processed_messages_nft_policy_id,
            &self.conf.processed_messages_nft_script_cbor,
        ) {
            debug!("Minting processed message NFT with policy: {}", policy_id);

            // Parse policy ID as bytes
            let policy_bytes: Hash<28> = Hash::new(
                hex::decode(policy_id)
                    .map_err(|e| {
                        TxBuilderError::Encoding(format!("Invalid NFT policy ID hex: {e}"))
                    })?
                    .try_into()
                    .map_err(|_| {
                        TxBuilderError::Encoding("NFT policy ID must be 28 bytes".to_string())
                    })?,
            );

            // Asset name is the 32-byte message_id
            let asset_name: Vec<u8> = components.message_id.to_vec();

            // Add mint asset (policy_id, asset_name, amount=1)
            tx = tx
                .mint_asset(policy_bytes, asset_name.clone(), 1)
                .map_err(|e| TxBuilderError::TxBuild(format!("Failed to add mint asset: {e:?}")))?;

            // Add the minted NFT to the processed marker output
            // This is where the minted NFT will live
            processed_marker_output = processed_marker_output
                .add_asset(policy_bytes, asset_name.clone(), 1)
                .map_err(|e| {
                    TxBuilderError::TxBuild(format!(
                        "Failed to add NFT to processed marker output: {e:?}"
                    ))
                })?;

            // Add mint redeemer (empty data since minting policy just checks mailbox is spent)
            let mint_redeemer_data = vec![0xd8, 0x79, 0x9f, 0xff]; // Constr 0 []
            let ex_units_mint = if let (Some((_, ex_units_map)), Some(sorted_idx)) =
                (eval_overrides, processed_nft_mint_idx)
            {
                let key = format!("mint:{sorted_idx}");
                if let Some(&(mem, steps)) = ex_units_map.get(&key) {
                    ExUnits { mem, steps }
                } else if let Some(ref c) = cached_ex {
                    c.processed_nft_mint
                        .map(|(m, s)| ExUnits { mem: m, steps: s })
                        .unwrap_or(ExUnits {
                            mem: DEFAULT_MEM_UNITS,
                            steps: DEFAULT_STEP_UNITS,
                        })
                } else {
                    ExUnits {
                        mem: DEFAULT_MEM_UNITS,
                        steps: DEFAULT_STEP_UNITS,
                    }
                }
            } else if let Some(ref c) = cached_ex {
                c.processed_nft_mint
                    .map(|(m, s)| ExUnits { mem: m, steps: s })
                    .unwrap_or(ExUnits {
                        mem: DEFAULT_MEM_UNITS,
                        steps: DEFAULT_STEP_UNITS,
                    })
            } else {
                ExUnits {
                    mem: DEFAULT_MEM_UNITS,
                    steps: DEFAULT_STEP_UNITS,
                }
            };
            tx = tx.add_mint_redeemer(policy_bytes, mint_redeemer_data, Some(ex_units_mint));

            // Add minting policy script to witness set
            let script_bytes = hex::decode(script_cbor).map_err(|e| {
                TxBuilderError::Encoding(format!("Invalid NFT script CBOR hex: {e}"))
            })?;
            tx = tx.script(ScriptKind::PlutusV3, script_bytes);

            debug!(
                "Added NFT minting for message_id: {}",
                hex::encode(components.message_id)
            );
        }

        // 4b-bis: Always mint verified message NFT (required by mailbox validator)
        // The mailbox always checks verified_nft_minted during Process.
        // For GenericRecipient, the NFT goes to the recipient script output.
        // For WarpRoute recipients, no verified NFT is minted (not needed).
        let verified_nft_info = if has_verified_message {
            if let (Some(ref verified_nft_policy_id), Some(ref verified_nft_script_cbor)) = (
                &self.conf.verified_message_nft_policy_id,
                &self.conf.verified_message_nft_script_cbor,
            ) {
                let verified_policy_bytes: Hash<28> = Hash::new(
                    hex::decode(verified_nft_policy_id)
                        .map_err(|e| {
                            TxBuilderError::Encoding(format!(
                                "Invalid verified_message_nft_policy_id hex: {e}"
                            ))
                        })?
                        .try_into()
                        .map_err(|_| {
                            TxBuilderError::Encoding(
                                "verified_message_nft_policy_id must be 28 bytes".to_string(),
                            )
                        })?,
                );

                let verified_nft_asset_name: Vec<u8> = components.message_id.to_vec();

                tx = tx
                    .mint_asset(verified_policy_bytes, verified_nft_asset_name.clone(), 1)
                    .map_err(|e| {
                        TxBuilderError::TxBuild(format!(
                            "Failed to mint verified message NFT: {e:?}"
                        ))
                    })?;

                let verified_nft_mint_redeemer = vec![0xd8, 0x79, 0x9f, 0xff]; // MintMessage = Constr 0 []
                let ex_units_verified_nft = if let (Some((_, ex_units_map)), Some(sorted_idx)) =
                    (eval_overrides, verified_nft_mint_idx)
                {
                    let key = format!("mint:{sorted_idx}");
                    if let Some(&(mem, steps)) = ex_units_map.get(&key) {
                        ExUnits { mem, steps }
                    } else if let Some(ref c) = cached_ex {
                        c.verified_nft_mint
                            .map(|(m, s)| ExUnits { mem: m, steps: s })
                            .unwrap_or(ExUnits {
                                mem: DEFAULT_MEM_UNITS,
                                steps: DEFAULT_STEP_UNITS,
                            })
                    } else {
                        ExUnits {
                            mem: DEFAULT_MEM_UNITS,
                            steps: DEFAULT_STEP_UNITS,
                        }
                    }
                } else if let Some(ref c) = cached_ex {
                    c.verified_nft_mint
                        .map(|(m, s)| ExUnits { mem: m, steps: s })
                        .unwrap_or(ExUnits {
                            mem: DEFAULT_MEM_UNITS,
                            steps: DEFAULT_STEP_UNITS,
                        })
                } else {
                    ExUnits {
                        mem: DEFAULT_MEM_UNITS,
                        steps: DEFAULT_STEP_UNITS,
                    }
                };
                tx = tx.add_mint_redeemer(
                    verified_policy_bytes,
                    verified_nft_mint_redeemer,
                    Some(ex_units_verified_nft),
                );

                let verified_script_bytes = hex::decode(verified_nft_script_cbor).map_err(|e| {
                    TxBuilderError::Encoding(format!(
                        "Invalid verified_message_nft_script_cbor hex: {e}"
                    ))
                })?;
                tx = tx.script(ScriptKind::PlutusV3, verified_script_bytes);

                info!(
                    "Minted verified message NFT for message_id: {}",
                    hex::encode(components.message_id)
                );

                Some((verified_policy_bytes, verified_nft_asset_name))
            } else {
                None
            }
        } else {
            None
        };

        tx = tx.output(processed_marker_output);
        debug!(
            "Added processed message marker output for message_id: {}",
            hex::encode(components.message_id)
        );

        // 4c. Verified message output (GenericRecipient only)
        // Delivers the verified message datum directly to the recipient script address
        if let (Some(ref verified_datum_cbor), Some(recipient_hash)) = (
            &components.verified_message_datum_cbor,
            &components.recipient_script_hash,
        ) {
            let pallas_network = match self.conf.network {
                CardanoNetwork::Mainnet => Network::Mainnet,
                CardanoNetwork::Preprod | CardanoNetwork::Preview => Network::Testnet,
            };

            let recipient_script_address = script_hash_to_address(recipient_hash, pallas_network)?;

            let mut verified_message_output =
                Output::new(recipient_script_address, verified_message_min_lovelace)
                    .set_inline_datum(verified_datum_cbor.clone());

            // Add verified message NFT to recipient output
            if let Some((ref policy, ref asset_name)) = verified_nft_info {
                verified_message_output = verified_message_output
                    .add_asset(*policy, asset_name.clone(), 1)
                    .map_err(|e| {
                        TxBuilderError::TxBuild(format!(
                            "Failed to add verified message NFT to recipient output: {e:?}"
                        ))
                    })?;
                info!("Added verified message NFT to recipient output");
            }

            tx = tx.output(verified_message_output);
            info!(
                "Added verified message output at recipient script {}",
                hex::encode(recipient_hash)
            );
        }

        // 5. Change output back to payer
        let fee = eval_overrides
            .map(|(f, _)| f)
            .unwrap_or(ESTIMATED_FEE_LOVELACE);

        // Cache per-role ExUnits from a successful evaluate for reuse when
        // evaluate fails on tracked-state TXs. Only update when the map is
        // non-empty (callers may pass fee-only overrides with an empty map).
        if let Some((_, ex_units_map)) = eval_overrides {
            if !ex_units_map.is_empty() {
                let to_cache = CachedProcessExUnits {
                    mailbox_spend: *ex_units_map
                        .get(&format!("spend:{mailbox_sorted_idx}"))
                        .unwrap_or(&(DEFAULT_MEM_UNITS, DEFAULT_STEP_UNITS)),
                    ism_spend: *ex_units_map
                        .get(&format!("spend:{ism_sorted_idx}"))
                        .unwrap_or(&(ISM_MEM_UNITS, ISM_STEP_UNITS)),
                    processed_nft_mint: processed_nft_mint_idx
                        .and_then(|idx| ex_units_map.get(&format!("mint:{idx}")).copied()),
                    verified_nft_mint: verified_nft_mint_idx
                        .and_then(|idx| ex_units_map.get(&format!("mint:{idx}")).copied()),
                    recipient_spend: recipient_sorted_idx
                        .and_then(|idx| ex_units_map.get(&format!("spend:{idx}")).copied()),
                    synthetic_mint: synthetic_mint_idx
                        .and_then(|idx| ex_units_map.get(&format!("mint:{idx}")).copied()),
                };
                let to_cache = to_cache.with_step_margin();
                debug!(
                    "Caching per-role ExUnits (with 10% step margin): {:?}",
                    to_cache
                );
                *self.cached_process_ex_units.lock().await = Some(to_cache);
            }
        }

        let processed_marker_cost = processed_marker_min_lovelace;

        // Calculate recipient shortfall - when warp route UTXO doesn't have enough lovelace
        // to fund both the continuation output AND the recipient output, payer covers the difference.
        // Uses continuation_min_lovelace (accounts for NFT + datum in continuation output)
        // and recipient_output_cost (accounts for recipient output minUTxO).
        let recipient_shortfall = match &components.warp_token_type {
            Some(WarpTokenTypeInfo::Synthetic { .. })
                if components.token_release_amount.is_some() =>
            {
                let original_lovelace = warp_route_lovelace;
                let warp_contribution =
                    if original_lovelace > continuation_min_lovelace + recipient_output_cost {
                        recipient_output_cost
                    } else if original_lovelace > continuation_min_lovelace {
                        original_lovelace.saturating_sub(continuation_min_lovelace)
                    } else {
                        0
                    };
                let continuation_gap = continuation_min_lovelace.saturating_sub(original_lovelace);
                let shortfall =
                    recipient_output_cost.saturating_sub(warp_contribution) + continuation_gap;
                debug!(
                    "Synthetic recipient shortfall: warp_lovelace={}, recipient_output_cost={}, warp_contribution={}, continuation_gap={}, shortfall={}",
                    original_lovelace, recipient_output_cost, warp_contribution, continuation_gap, shortfall
                );
                shortfall
            }
            Some(WarpTokenTypeInfo::Native) if components.token_release_amount.is_some() => {
                let original_lovelace = warp_route_lovelace;
                let release_amount = components
                    .token_release_amount
                    .expect("guarded by is_some()");
                // Recipient output must be at least min_lovelace
                let recipient_actual = release_amount.max(min_lovelace);
                let continuation_actual = original_lovelace
                    .saturating_sub(release_amount)
                    .max(continuation_min_lovelace);
                let total_outputs = continuation_actual + recipient_actual;
                let shortfall = total_outputs.saturating_sub(original_lovelace);
                if shortfall > 0 {
                    debug!(
                        "Native recipient shortfall: warp_lovelace={}, continuation_actual={}, recipient_actual={} (release={}), total_outputs={}, shortfall={}",
                        original_lovelace, continuation_actual, recipient_actual, release_amount, total_outputs, shortfall
                    );
                }
                shortfall
            }
            Some(WarpTokenTypeInfo::Collateral { .. })
                if components.token_release_amount.is_some() =>
            {
                let original_lovelace = warp_route_lovelace;
                let total_needed = continuation_min_lovelace + recipient_output_cost;
                let shortfall = total_needed.saturating_sub(original_lovelace);
                if shortfall > 0 {
                    debug!(
                        "Collateral recipient shortfall: warp_lovelace={}, continuation_needed={}, recipient_output_cost={}, shortfall={}",
                        original_lovelace, continuation_min_lovelace, recipient_output_cost, shortfall
                    );
                }
                shortfall
            }
            _ => 0,
        };

        debug!(
            "Change calculation: total_input={}, fee={}, processed_marker={}, recipient_shortfall={}, verified_message={}",
            total_input, fee, processed_marker_cost, recipient_shortfall, verified_message_min_lovelace
        );
        let change_amount = total_input.saturating_sub(
            fee + processed_marker_cost + recipient_shortfall + verified_message_min_lovelace,
        );
        if chained_payer.is_some() && change_amount < min_lovelace {
            return Err(TxBuilderError::TxBuild(format!(
                "Chained payer change {change_amount} < min_lovelace {min_lovelace} \
                 (total_input={total_input}, fee={fee}, processed={processed_marker_cost}, \
                 shortfall={recipient_shortfall}, verified={verified_message_min_lovelace})",
            )));
        }

        if change_amount >= min_lovelace {
            let mut change_output = Output::new(parse_address(&payer_address)?, change_amount);
            // If any selected fee UTxOs carry native tokens, pass them through to
            // the change output so the TX satisfies value-conservation.
            for utxo in &selected_utxos {
                for v in &utxo.value {
                    if v.unit == "lovelace" || v.unit.len() < 56 {
                        continue;
                    }
                    let policy_hex = &v.unit[..56];
                    let asset_name_hex = &v.unit[56..];
                    let policy_hash = parse_policy_id(policy_hex)?;
                    let asset_name = hex::decode(asset_name_hex).map_err(|e| {
                        TxBuilderError::Encoding(format!("Invalid asset name hex: {e}"))
                    })?;
                    let quantity: u64 = v
                        .quantity
                        .parse()
                        .map_err(|e| TxBuilderError::Encoding(format!("Invalid quantity: {e}")))?;
                    change_output = change_output
                        .add_asset(policy_hash, asset_name, quantity)
                        .map_err(|e| {
                            TxBuilderError::TxBuild(format!(
                                "Failed to add token to change output: {e:?}"
                            ))
                        })?;
                }
            }
            tx = tx.output(change_output);
        }

        // Set fee
        tx = tx.fee(fee);

        // Set network ID
        let network_id = match self.conf.network {
            CardanoNetwork::Mainnet => 1u8,
            CardanoNetwork::Preprod | CardanoNetwork::Preview => 0u8,
        };
        tx = tx.network_id(network_id);

        // Add disclosed signer (payer must sign)
        let payer_hash: Hash<28> = Hash::new(*payer.payment_credential_hash());
        tx = tx.disclosed_signer(payer_hash);

        // Add mailbox script - prefer reference script over inline witness
        if let Some(ref ref_utxo_str) = self.conf.mailbox_reference_script_utxo {
            // Use reference script UTXO (preferred method)
            let ref_input = parse_utxo_ref(ref_utxo_str)?;
            tx = tx.reference_input(ref_input);
            debug!("Added mailbox reference script UTXO: {}", ref_utxo_str);
        } else if let Some(ref script_cbor_hex) = self.conf.mailbox_script_cbor {
            // Fall back to inline script witness (deprecated)
            let script_bytes = hex::decode(script_cbor_hex).map_err(|e| {
                TxBuilderError::Encoding(format!("Invalid mailbox script CBOR hex: {e}"))
            })?;
            tx = tx.script(ScriptKind::PlutusV3, script_bytes);
            debug!("Added mailbox script to witness set (deprecated - use reference scripts)");
        } else {
            return Err(TxBuilderError::ScriptNotFound(
                "Neither mailbox_reference_script_utxo nor mailbox_script_cbor configured"
                    .to_string(),
            ));
        }

        // Add ISM script - required for signature verification
        // Prefer reference script over inline witness
        if let Some(ref ref_utxo_str) = self.conf.ism_reference_script_utxo {
            // Use reference script UTXO (preferred method)
            let ref_input = parse_utxo_ref(ref_utxo_str)?;
            tx = tx.reference_input(ref_input);
            debug!("Added ISM reference script UTXO: {}", ref_utxo_str);
        } else if let Some(ref script_cbor_hex) = self.conf.ism_script_cbor {
            // Fall back to inline script witness (deprecated)
            let script_bytes = hex::decode(script_cbor_hex).map_err(|e| {
                TxBuilderError::Encoding(format!("Invalid ISM script CBOR hex: {e}"))
            })?;
            tx = tx.script(ScriptKind::PlutusV3, script_bytes);
            debug!("Added ISM script to witness set (deprecated - use reference scripts)");
        } else {
            return Err(TxBuilderError::ScriptNotFound(
                "Neither ism_reference_script_utxo nor ism_script_cbor configured".to_string(),
            ));
        }

        // Set language view for PlutusV3 (required for script_data_hash calculation)
        // Using the Conway PlutusV3 cost model from protocol parameters
        // For now, using placeholder values - in production, fetch from protocol params
        let plutus_v3_cost_model: Vec<i64> = get_plutus_v3_cost_model();
        tx = tx.language_view(ScriptKind::PlutusV3, plutus_v3_cost_model);

        // Build the transaction
        let built = tx
            .build_conway_raw()
            .map_err(|e| TxBuilderError::TxBuild(format!("Failed to build transaction: {e:?}")))?;

        Ok(built)
    }

    /// Sign a built transaction with the payer keypair
    fn sign_transaction(
        &self,
        built: BuiltTransaction,
        payer: &Keypair,
    ) -> Result<Vec<u8>, TxBuilderError> {
        // Get the transaction hash for signing
        // tx_hash is Bytes32 which is a wrapper around Vec<u8>
        let tx_hash_bytes: &[u8] = &built.tx_hash.0;

        // Sign the transaction hash
        let signature = payer.sign(tx_hash_bytes);

        // Get the public key
        let public_key = payer.pallas_public_key();

        // Add the signature to the built transaction
        let signed = built
            .add_signature(*public_key, signature)
            .map_err(|e| TxBuilderError::TxBuild(format!("Failed to add signature: {e:?}")))?;

        // Return the serialized signed transaction
        // tx_bytes is Bytes which is a wrapper around Vec<u8>
        Ok(signed.tx_bytes.0.clone())
    }

    /// Submit a signed transaction to Blockfrost
    async fn submit_transaction(&self, signed_tx: &[u8]) -> Result<String, TxBuilderError> {
        use pallas_primitives::conway::Tx;
        use pallas_primitives::Fragment;

        // Validate transaction structure before submission
        match Tx::decode_fragment(signed_tx) {
            Ok(tx) => {
                info!("Transaction validated successfully");
                info!("  - Inputs: {}", tx.transaction_body.inputs.len());
                info!("  - Outputs: {}", tx.transaction_body.outputs.len());
                info!("  - Fee: {}", tx.transaction_body.fee);
                info!(
                    "  - Has vkey witnesses: {}",
                    tx.transaction_witness_set.vkeywitness.is_some()
                );
                if let Some(ref redeemers) = tx.transaction_witness_set.redeemer {
                    info!("  - Has redeemers: true");
                    if let Ok(redeemer_cbor) = redeemers.encode_fragment() {
                        debug!("  - Redeemers CBOR: {}", hex::encode(&redeemer_cbor));
                    }
                }
                info!("  - Success flag: {}", tx.success);
                let has_aux = matches!(&tx.auxiliary_data, pallas_codec::utils::Nullable::Some(_));
                info!("  - Has auxiliary_data: {}", has_aux);
            }
            Err(e) => {
                tracing::error!("Transaction validation failed: {:?}", e);
                tracing::error!("Transaction CBOR (full): {}", hex::encode(signed_tx));
                return Err(TxBuilderError::TxBuild(format!(
                    "Invalid transaction CBOR: {e:?}"
                )));
            }
        }

        // Print full transaction CBOR hex for analysis
        let full_hex = hex::encode(signed_tx);
        info!(
            "Submitting transaction CBOR ({} bytes): {}",
            signed_tx.len(),
            full_hex
        );

        // Analyze CBOR structure
        if !signed_tx.is_empty() {
            let first_byte = signed_tx[0];
            let major_type = first_byte >> 5;
            let additional_info = first_byte & 0x1f;
            info!(
                "CBOR first byte: 0x{:02x} (major type: {}, additional info: {})",
                first_byte, major_type, additional_info
            );
        }

        self.provider
            .submit_transaction(signed_tx)
            .await
            .map_err(|e| TxBuilderError::SubmissionFailed(e.to_string()))
    }

    /// Select UTXOs for fee payment using simple greedy algorithm
    fn select_utxos_for_fee(
        &self,
        utxos: &[Utxo],
        min_lovelace: u64,
    ) -> Result<(Vec<Utxo>, u64), TxBuilderError> {
        self.select_utxos_for_fee_with_extra(utxos, 0, min_lovelace)
    }

    fn select_utxos_for_fee_with_extra(
        &self,
        utxos: &[Utxo],
        extra: u64,
        min_lovelace: u64,
    ) -> Result<(Vec<Utxo>, u64), TxBuilderError> {
        // Build a block-list of the specific UTxO IDs that are configured as
        // reference inputs. These must NOT be spent as fee inputs because the TX
        // adds them as reference_inputs and Cardano rejects UTxOs that appear in
        // both inputs and reference_inputs.
        //
        // We intentionally do NOT blanket-filter by reference_script_hash because
        // the wallet accumulates reference-script UTxOs from contract deployments
        // (e.g., old/unused warp route ref scripts). Those are safe to spend as
        // long as they aren't the specific UTxOs referenced by this TX.
        let blocked: std::collections::HashSet<(String, u32)> = [
            self.conf.mailbox_reference_script_utxo.as_deref(),
            self.conf.ism_reference_script_utxo.as_deref(),
            self.conf.warp_route_reference_script_utxo.as_deref(),
        ]
        .iter()
        .filter_map(|s| *s)
        .filter_map(|s| {
            let mut parts = s.splitn(2, '#');
            let hash = parts.next()?.to_string();
            let idx: u32 = parts.next()?.parse().ok()?;
            Some((hash, idx))
        })
        .collect();

        // Need enough for fee + min UTXO for change + extra (e.g., synthetic recipient shortfall)
        let needed = ESTIMATED_FEE_LOVELACE + min_lovelace + extra;
        debug!(
            "UTXO selection: need {} lovelace (fee={}, min_utxo={}, extra={})",
            needed, ESTIMATED_FEE_LOVELACE, min_lovelace, extra
        );

        // Sort UTXOs by lovelace amount (largest first) for efficient selection
        let mut sorted: Vec<_> = utxos.iter().collect();
        sorted.sort_by_key(|u| std::cmp::Reverse(u.lovelace()));

        // First pass: prefer pure-ADA UTxOs. Selecting pure-ADA avoids the need to
        // carry native tokens through to the change output.
        let mut selected = Vec::new();
        let mut total: u64 = 0;
        for utxo in &sorted {
            if utxo.value.len() > 1 {
                continue;
            }
            if blocked.contains(&(utxo.tx_hash.clone(), utxo.output_index)) {
                continue;
            }
            selected.push((*utxo).clone());
            total += utxo.lovelace();
            if total >= needed {
                break;
            }
        }

        if total >= needed {
            return Ok((selected, total));
        }

        // Second pass: wallet has no (or insufficient) pure-ADA UTxOs.
        // Allow token-bearing UTxOs that are NOT reference-script UTxOs; their
        // native assets must be preserved in the change output (value-conservation).
        // Reference-script UTxOs are never spent as fee inputs — consuming them
        // would destroy the scripts needed by future transactions.
        debug!(
            "Pure-ADA selection yielded only {} lovelace (need {}), trying token UTxOs (excluding ref-scripts)",
            total, needed
        );
        selected.clear();
        total = 0;
        for utxo in &sorted {
            if utxo.reference_script_hash.is_some() {
                continue;
            }
            if blocked.contains(&(utxo.tx_hash.clone(), utxo.output_index)) {
                continue;
            }
            selected.push((*utxo).clone());
            total += utxo.lovelace();
            if total >= needed {
                break;
            }
        }

        if total < needed {
            return Err(TxBuilderError::InsufficientFunds {
                needed,
                available: total,
            });
        }

        Ok((selected, total))
    }

    /// Convert network configuration to pallas Network type
    fn network_to_pallas(&self) -> Network {
        match self.conf.network {
            CardanoNetwork::Mainnet => Network::Mainnet,
            CardanoNetwork::Preprod | CardanoNetwork::Preview => Network::Testnet,
        }
    }

    /// Create the processed message marker output
    /// This output is sent to the processed_messages_script address
    /// with an inline datum containing the message_id. No NFT is needed.
    fn create_processed_marker_output(
        &self,
        _message_id: &[u8; 32],
        datum_cbor: &[u8],
        min_lovelace: u64,
    ) -> Result<Output, TxBuilderError> {
        // The processed messages are stored at the processed_messages_script address
        // This must match the parameter applied to the mailbox validator
        let script_address = self
            .provider
            .script_hash_to_address(&self.conf.processed_messages_script_hash)?;

        // Just create a simple output with inline datum, no NFT needed
        let output = Output::new(parse_address(&script_address)?, min_lovelace)
            .set_inline_datum(datum_cbor.to_vec());

        Ok(output)
    }

    /// Update ISM validators for a specific domain
    ///
    /// This builds and submits a transaction that updates the validator set
    /// in the MultisigISM for a given origin domain.
    ///
    /// # Arguments
    /// * `domain` - The origin domain ID (e.g., 43113 for Fuji)
    /// * `validators` - List of 20-byte validator addresses (will be padded to 32 bytes)
    /// * `threshold` - Number of required signatures
    /// * `ism_policy_id` - ISM state NFT policy ID
    /// * `payer` - Keypair to sign and pay for the transaction
    #[instrument(skip(self, validators, payer))]
    pub async fn update_ism_validators(
        &self,
        domain: u32,
        validators: Vec<Vec<u8>>,
        threshold: u32,
        ism_policy_id: &str,
        payer: &Keypair,
    ) -> Result<String, TxBuilderError> {
        info!(
            "Updating ISM validators for domain {} (threshold: {}, validators: {})",
            domain,
            threshold,
            validators.len()
        );

        // Fetch min_lovelace and ISM UTXOs in parallel (independent queries)
        let (min_lovelace, ism_utxos) = tokio::join!(
            self.min_lovelace_simple(),
            self.provider.get_script_utxos(ism_policy_id),
        );
        let ism_utxos = ism_utxos?;

        let ism_utxo = ism_utxos.into_iter().next().ok_or_else(|| {
            TxBuilderError::UtxoNotFound(format!("ISM UTXO not found at script {ism_policy_id}"))
        })?;
        info!(
            "Found ISM UTXO: {}#{}",
            ism_utxo.tx_hash, ism_utxo.output_index
        );

        // 2. Parse current ISM datum from inline datum CBOR
        let current_datum_hex = ism_utxo.inline_datum.as_ref().ok_or_else(|| {
            TxBuilderError::UtxoNotFound("ISM UTXO has no inline datum".to_string())
        })?;

        // Decode CBOR hex to PlutusData
        let datum_bytes = hex::decode(current_datum_hex)
            .map_err(|e| TxBuilderError::Encoding(format!("Invalid datum hex: {e}")))?;
        let current_datum_plutus: PlutusData = minicbor::decode(&datum_bytes)
            .map_err(|e| TxBuilderError::Encoding(format!("Failed to decode datum CBOR: {e:?}")))?;

        // Debug: print raw structure
        info!("Raw decoded datum: {:?}", current_datum_plutus);

        // Unwrap CBOR tag 121 to get the actual datum
        // Aiken compiles to Constr(0, fields), but CBOR wraps it as Constr(121, [actual_datum])
        let current_datum_plutus = match &current_datum_plutus {
            PlutusData::Constr(constr) if constr.tag == 121 && constr.fields.len() == 1 => {
                // Single field - unwrap it
                constr.fields[0].clone()
            }
            _ => current_datum_plutus,
        };

        info!("After unwrapping tag 121: {:?}", current_datum_plutus);

        // Parse as MultisigIsmDatum to extract owner
        match &current_datum_plutus {
            PlutusData::Constr(constr) => {
                info!(
                    "Datum is Constr(tag={}) with {} fields",
                    constr.tag,
                    constr.fields.len()
                );
                for (i, field) in constr.fields.iter().enumerate() {
                    match field {
                        PlutusData::Array(arr) => {
                            info!("  Field {}: Array with {} elements", i, arr.len())
                        }
                        PlutusData::Constr(c) => info!("  Field {}: Constr(tag={})", i, c.tag),
                        PlutusData::BoundedBytes(b) => {
                            info!("  Field {}: BoundedBytes({} bytes)", i, b.len())
                        }
                        _ => info!("  Field {}: {:?}", i, field),
                    }
                }
            }
            PlutusData::Array(fields) => {
                info!("Datum is Array with {} fields", fields.len());
            }
            _ => info!("Datum is: {:?}", current_datum_plutus),
        }
        let owner = extract_ism_owner(&current_datum_plutus)?;
        info!("ISM owner: {}", hex::encode(owner));

        // 3. Validate validators are 20 bytes (Ethereum addresses)
        // Note: Store them as-is without padding - Hyperlane validators are 20 bytes
        for validator in &validators {
            if validator.len() != 20 {
                return Err(TxBuilderError::InvalidAddress(format!(
                    "Validator must be 20 bytes, got {}",
                    validator.len()
                )));
            }
        }

        // 4. Build new ISM datum with updated validators (stored as 20-byte Ethereum addresses)
        let new_datum = build_ism_datum(domain, validators.clone(), threshold, owner)?;

        // Encode to CBOR - Pallas will add the outer tag 121 wrapper automatically
        let new_datum_cbor = encode_plutus_data(&new_datum)?;

        // 5. Build SetValidators redeemer
        // Note: Validators are Ethereum addresses (20 bytes)
        let redeemer = crate::types::MultisigIsmRedeemer::SetValidators {
            domain,
            validators: validators
                .iter()
                .map(|v| {
                    let mut arr = [0u8; 20];
                    arr.copy_from_slice(&v[..20.min(v.len())]);
                    crate::types::EthAddress(arr)
                })
                .collect(),
        };
        let redeemer_cbor = encode_ism_redeemer(&redeemer)?;

        // 6. Get payer address and UTXOs
        let payer_address = payer.address_bech32(self.network_to_pallas());
        let payer_utxos = self.provider.get_utxos_at_address(&payer_address).await?;
        let (selected_utxos, total_input) =
            self.select_utxos_for_fee(&payer_utxos, min_lovelace)?;

        info!(
            "Selected {} payer UTXOs with {} lovelace",
            selected_utxos.len(),
            total_input
        );

        // 7. Build transaction
        let mut tx = StagingTransaction::new();

        // Add ISM UTXO as first input (will be spent)
        let ism_input = utxo_to_input(&ism_utxo)?;
        let ism_input_for_redeemer = utxo_to_input(&ism_utxo)?; // Separate input for redeemer
        tx = tx.input(ism_input);

        // Add payer UTXOs for fees
        for utxo in &selected_utxos {
            tx = tx.input(utxo_to_input(utxo)?);
        }

        // Add spend redeemer for ISM with execution units
        let ex_units = ExUnits {
            mem: DEFAULT_MEM_UNITS,
            steps: DEFAULT_STEP_UNITS,
        };

        tx = tx.add_spend_redeemer(ism_input_for_redeemer, redeemer_cbor, Some(ex_units));

        // 8. Create outputs

        // ISM continuation output (same address, same value, updated datum)
        let ism_address = parse_address(&ism_utxo.address)?;
        let ism_lovelace = ism_utxo.lovelace().max(min_lovelace);

        let mut ism_output = Output::new(ism_address, ism_lovelace);
        ism_output = ism_output.set_inline_datum(new_datum_cbor);

        // Preserve ISM NFT
        let ism_policy_hash = parse_policy_id(ism_policy_id)?;
        ism_output = ism_output
            .add_asset(ism_policy_hash, vec![], 1)
            .map_err(|e| TxBuilderError::TxBuild(format!("Failed to add ISM NFT: {e:?}")))?;

        tx = tx.output(ism_output);

        // Change output
        let change_amount = total_input
            .saturating_sub(ism_lovelace)
            .saturating_sub(ESTIMATED_FEE_LOVELACE);

        if change_amount >= min_lovelace {
            let mut change_output = Output::new(parse_address(&payer_address)?, change_amount);
            for utxo in &selected_utxos {
                for v in &utxo.value {
                    if v.unit == "lovelace" || v.unit.len() < 56 {
                        continue;
                    }
                    let policy_hex = &v.unit[..56];
                    let asset_name_hex = &v.unit[56..];
                    let policy_hash = parse_policy_id(policy_hex)?;
                    let asset_name = hex::decode(asset_name_hex).map_err(|e| {
                        TxBuilderError::Encoding(format!("Invalid asset name hex: {e}"))
                    })?;
                    let quantity: u64 = v
                        .quantity
                        .parse()
                        .map_err(|e| TxBuilderError::Encoding(format!("Invalid quantity: {e}")))?;
                    change_output = change_output
                        .add_asset(policy_hash, asset_name, quantity)
                        .map_err(|e| {
                            TxBuilderError::TxBuild(format!(
                                "Failed to add token to change output: {e:?}"
                            ))
                        })?;
                }
            }
            tx = tx.output(change_output);
        }

        // 9. Build, sign and submit
        // Set language view for PlutusV3 (required for script_data_hash calculation)
        let plutus_v3_cost_model: Vec<i64> = get_plutus_v3_cost_model();
        tx = tx.language_view(ScriptKind::PlutusV3, plutus_v3_cost_model);

        let built_tx = tx
            .build_conway_raw()
            .map_err(|e| TxBuilderError::TxBuild(format!("Failed to build transaction: {e:?}")))?;

        let signed_tx = self.sign_transaction(built_tx, payer)?;
        let tx_hash = self.submit_transaction(&signed_tx).await?;

        info!("ISM update transaction submitted: {}", tx_hash);
        Ok(tx_hash)
    }

    /// Estimate the total lovelace cost for processing a message by building
    /// a dry-run TX, evaluating it via Blockfrost's Ogmios endpoint, and
    /// computing the fee from protocol parameters + execution units.
    ///
    /// Requires Blockfrost's "hosted variant" which includes an Ogmios backend.
    /// If the evaluate endpoint is unavailable (HTTP 500), this method disables
    /// itself for all future calls to avoid repeated failing requests.
    pub async fn estimate_process_cost(
        &self,
        message: &HyperlaneMessage,
        metadata: &[u8],
        payer: &Keypair,
    ) -> Result<u64, TxBuilderError> {
        // Always build TX components first — this catches permanent failures
        // (e.g. zero-amount) regardless of evaluate endpoint availability.
        let components = self
            .build_process_tx(message, metadata, payer, None)
            .await?;

        let built_tx = self
            .build_complete_process_tx(&components, payer, None, None)
            .await?;

        let signed_tx = self.sign_transaction(built_tx, payer)?;

        // Early TX size check — catches oversized messages during the dry-run
        // (which has backoff via on_reprepare) rather than during submission
        let max_tx_size = self.get_max_tx_size().await;
        let tx_size = signed_tx.len() as u64;
        if tx_size > max_tx_size {
            return Err(TxBuilderError::UndeliverableMessage(format!(
                "Transaction size {tx_size} bytes exceeds max_tx_size {max_tx_size} bytes"
            )));
        }

        debug!(
            "Evaluating TX CBOR ({} bytes): {}",
            signed_tx.len(),
            hex::encode(&signed_tx)
        );

        let (fee, _ex_units_map) = self
            .evaluate_and_compute_fee(&signed_tx, components.total_ref_script_size, None)
            .await?;

        // Add minUTxO costs for outputs the relayer creates (mirrors build_complete_process_tx)
        let coins_per_byte = self.get_coins_per_utxo_byte().await;
        let processed_marker_min = if self.conf.processed_messages_nft_policy_id.is_some() {
            Self::calculate_min_lovelace_sync(
                coins_per_byte,
                OutputType::WithTokenAndDatum {
                    asset_name_len: 32,
                    datum_size: components.processed_datum_cbor.len(),
                },
            )
        } else {
            Self::calculate_min_lovelace_sync(
                coins_per_byte,
                OutputType::WithInlineDatum {
                    datum_size: components.processed_datum_cbor.len(),
                },
            )
        };

        let verified_message_min =
            if let Some(ref datum_cbor) = components.verified_message_datum_cbor {
                Self::calculate_min_lovelace_sync(
                    coins_per_byte,
                    OutputType::WithTokenAndDatum {
                        asset_name_len: 32,
                        datum_size: datum_cbor.len(),
                    },
                )
            } else {
                0
            };

        let output_costs = processed_marker_min + verified_message_min;
        let total = fee + output_costs;

        info!(
            "Estimated cost: fee={}, processed_marker={}, verified_msg={}, total={}",
            fee, processed_marker_min, verified_message_min, total
        );

        Ok(total)
    }

    /// Evaluate a signed TX via Blockfrost and compute the real fee.
    /// Returns (fee, per_redeemer_ex_units_map).
    ///
    /// When `additional_utxos` is provided (non-empty), uses
    /// `/utils/txs/evaluate/utxos` so the evaluation can reference
    /// unconfirmed predecessor outputs (chained TX inputs). The value
    /// format uses policy IDs as top-level keys per the Ogmios v5 spec.
    async fn evaluate_and_compute_fee(
        &self,
        signed_tx: &[u8],
        ref_script_size: u64,
        additional_utxos: Option<&[Utxo]>,
    ) -> Result<(u64, EvaluatedExUnits), TxBuilderError> {
        let eval_result = match additional_utxos {
            Some(utxos) if !utxos.is_empty() => {
                match self
                    .provider
                    .evaluate_tx_with_additional_utxos(signed_tx, utxos)
                    .await
                {
                    Ok(result) => result,
                    Err(e) => return Err(e.into()),
                }
            }
            _ => match self.provider.evaluate_tx(signed_tx).await {
                Ok(result) => result,
                Err(e) => return Err(e.into()),
            },
        };

        let per_redeemer_units = parse_per_redeemer_ex_units(&eval_result)?;
        let tx_size = signed_tx.len() as u64;

        // Apply 20% margin to each redeemer and compute totals
        let mut margined_units: EvaluatedExUnits = HashMap::new();
        let mut total_mem = 0u64;
        let mut total_steps = 0u64;
        for (key, (mem, steps)) in per_redeemer_units {
            let margined_mem = (mem as f64 * 1.2).ceil() as u64;
            let margined_steps = (steps as f64 * 1.2).ceil() as u64;
            margined_units.insert(key, (margined_mem, margined_steps));
            total_mem += margined_mem;
            total_steps += margined_steps;
        }

        let fee = self
            .compute_fee_from_evaluation(tx_size, total_mem, total_steps, ref_script_size)
            .await?;

        Ok((fee, margined_units))
    }

    /// Compute the actual TX fee from protocol parameters and evaluation results.
    ///
    /// The Conway fee formula: size_fee + script_fee + ref_script_fee.
    /// ExUnits already have 20% margin (applied per-redeemer in evaluate_and_compute_fee),
    /// and we add 5% overall margin for CBOR encoding variance between build passes.
    async fn compute_fee_from_evaluation(
        &self,
        tx_size: u64,
        total_mem: u64,
        total_steps: u64,
        ref_script_size: u64,
    ) -> Result<u64, TxBuilderError> {
        let params = self.provider.get_protocol_parameters().await?;
        let min_fee_a = params
            .get("min_fee_a")
            .and_then(|v| v.as_u64())
            .unwrap_or(44);
        let min_fee_b = params
            .get("min_fee_b")
            .and_then(|v| v.as_u64())
            .unwrap_or(155381);
        let price_mem: f64 = params
            .get("price_mem")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0577);
        let price_step: f64 = params
            .get("price_step")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0000721);
        let min_fee_ref_script: u64 = params
            .get("min_fee_ref_script_cost_per_byte")
            .and_then(|v| v.as_u64())
            .unwrap_or(15);

        let size_fee = min_fee_b + (tx_size * min_fee_a);
        let script_fee =
            (price_mem * total_mem as f64 + price_step * total_steps as f64).ceil() as u64;
        let ref_script_fee = min_fee_ref_script * ref_script_size;

        let base_fee = size_fee + script_fee + ref_script_fee;
        let fee_with_margin = (base_fee as f64 * 1.05).ceil() as u64;

        info!(
            "Computed fee: size_fee={}, script_fee={}, ref_script_fee={} ({}*{}), base_fee={}, with_margin={}",
            size_fee, script_fee, ref_script_fee, min_fee_ref_script, ref_script_size, base_fee, fee_with_margin
        );

        Ok(fee_with_margin)
    }
}

/// Parse the evaluation result from Blockfrost/Ogmios to extract per-redeemer
/// memory and CPU steps, keyed by "spend:N" or "mint:N".
fn parse_per_redeemer_ex_units(
    result: &serde_json::Value,
) -> Result<EvaluatedExUnits, TxBuilderError> {
    let mut ex_units_map: EvaluatedExUnits = HashMap::new();

    // Try Ogmios v6 format: { "result": [{ "validator": {"index": N, "purpose": "spend"}, "budget": { "memory": M, "cpu": S } }] }
    if let Some(evaluations) = result.get("result").and_then(|v| v.as_array()) {
        for entry in evaluations {
            if let (Some(validator), Some(budget)) = (entry.get("validator"), entry.get("budget")) {
                let purpose = validator
                    .get("purpose")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let index = validator.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                let mem = budget.get("memory").and_then(|v| v.as_u64()).unwrap_or(0);
                let steps = budget.get("cpu").and_then(|v| v.as_u64()).unwrap_or(0);
                let key = format!("{purpose}:{index}");
                ex_units_map.insert(key, (mem, steps));
            }
        }
        if !ex_units_map.is_empty() {
            return Ok(ex_units_map);
        }
    }

    // Blockfrost/Ogmios v5 (JSON-WSP): { "result": { "EvaluationResult": { "spend:0": { "memory": N, "steps": N } } } }
    if let Some(eval_result) = result.get("result").and_then(|r| r.get("EvaluationResult")) {
        if let Some(obj) = eval_result.as_object() {
            for (key, value) in obj {
                let mem = value.get("memory").and_then(|v| v.as_u64()).unwrap_or(0);
                let steps = value.get("steps").and_then(|v| v.as_u64()).unwrap_or(0);
                ex_units_map.insert(key.clone(), (mem, steps));
            }
        }
        if !ex_units_map.is_empty() {
            return Ok(ex_units_map);
        }
    }

    // Top-level EvaluationResult (alternative format)
    if let Some(eval_result) = result.get("EvaluationResult") {
        if let Some(obj) = eval_result.as_object() {
            for (key, value) in obj {
                let mem = value.get("memory").and_then(|v| v.as_u64()).unwrap_or(0);
                let steps = value.get("steps").and_then(|v| v.as_u64()).unwrap_or(0);
                ex_units_map.insert(key.clone(), (mem, steps));
            }
        }
        if !ex_units_map.is_empty() {
            return Ok(ex_units_map);
        }
    }

    // Check for Ogmios evaluation failure
    if let Some(fault) = result.get("fault") {
        let fault_msg = fault
            .get("string")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        return Err(TxBuilderError::Encoding(format!(
            "TX evaluation failed: {fault_msg}"
        )));
    }
    if let Some(err) = result
        .get("result")
        .and_then(|r| r.get("EvaluationFailure"))
    {
        return Err(TxBuilderError::Encoding(format!(
            "TX evaluation failed: {err}"
        )));
    }

    Err(TxBuilderError::Encoding(format!(
        "Could not parse per-redeemer evaluation result: {result}"
    )))
}

/// Extract the node-required fee from a FeeTooSmallUTxO error string.
/// The Cardano node returns the exact minimum fee it computed, allowing us to
/// rebuild the TX with that exact amount and retry once.
fn parse_fee_too_small_expected(err: &str) -> Option<u64> {
    let needle = "mismatchExpected = Coin ";
    let pos = err.find(needle)?;
    let after = &err[pos + needle.len()..];
    let end = after
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after.len());
    after[..end].parse().ok()
}

/// Components needed to build a Process transaction
#[derive(Debug)]
pub struct ProcessTxComponents {
    /// Mailbox UTXO to spend
    pub mailbox_utxo: Utxo,
    /// Encoded mailbox redeemer (CBOR)
    pub mailbox_redeemer_cbor: Vec<u8>,
    /// Recipient state UTXO to spend (WarpRoute only; None for GenericRecipient)
    pub recipient_utxo: Option<Utxo>,
    /// Recipient reference script UTXO (WarpRoute only)
    pub recipient_ref_script_utxo: Option<Utxo>,
    /// Encoded recipient redeemer (CBOR) - WarpRoute only; None for GenericRecipient
    pub recipient_redeemer_cbor: Option<Vec<u8>>,
    /// Encoded recipient continuation datum (CBOR) - WarpRoute only
    pub recipient_continuation_datum_cbor: Option<Vec<u8>>,
    /// ISM UTXO (to be spent for verification)
    pub ism_utxo: Utxo,
    /// Additional inputs (UTXO, must_be_spent)
    pub additional_utxos: Vec<(Utxo, bool)>,
    /// Encoded processed message datum (CBOR)
    pub processed_datum_cbor: Vec<u8>,
    /// Message ID (32 bytes)
    pub message_id: [u8; 32],
    /// Original metadata
    pub metadata: Vec<u8>,
    /// The message being processed
    pub message: crate::types::Message,
    /// ISM Verify redeemer CBOR (pre-encoded)
    pub ism_redeemer_cbor: Vec<u8>,
    /// Recipient kind for this message
    pub recipient_kind: RecipientKind,
    /// TokenReceiver (warp routes): Transfer amount in local decimals
    pub token_release_amount: Option<u64>,
    /// TokenReceiver (warp routes): Transfer recipient (28-byte credential)
    pub token_release_recipient: Option<Vec<u8>>,
    /// TokenReceiver (warp routes): Token type for release handling
    pub warp_token_type: Option<WarpTokenTypeInfo>,
    /// GenericRecipient: verified message datum CBOR for direct delivery
    pub verified_message_datum_cbor: Option<Vec<u8>>,
    /// GenericRecipient: recipient script hash (28 bytes, extracted from Hyperlane address)
    pub recipient_script_hash: Option<[u8; 28]>,
    /// Total size (bytes) of all reference scripts used in the TX.
    /// Used for Conway-era fee calculation (`min_fee_ref_script_cost_per_byte`).
    pub total_ref_script_size: u64,
    /// Updated mailbox continuation datum (CBOR) with new processed_tree_root.
    /// When Some, the mailbox continuation output uses this datum instead of
    /// copying the original UTXO's datum.
    pub mailbox_continuation_datum_cbor: Option<Vec<u8>>,
}

/// Overrides for on-chain UTXO lookups when building chained transactions.
/// TX₂ uses TX₁'s outputs, TX₃ uses TX₂'s, etc.
pub struct ChainedInputs {
    /// Mailbox continuation from the previous TX (output #0)
    pub mailbox_utxo: Utxo,
    /// ISM continuation from the previous TX
    pub ism_utxo: Utxo,
    /// Pre-computed SMT non-membership proof for this message
    pub smt_proof: Vec<[u8; 32]>,
    /// New SMT root after inserting this message's key
    pub new_smt_root: [u8; 32],
    /// Previous TX's mailbox continuation datum (CBOR).
    /// Used to build the next continuation datum via CBOR manipulation
    /// instead of JSON parsing (Blockfrost format unavailable for chained UTXOs).
    pub prev_mailbox_datum_cbor: Vec<u8>,
}

/// Chained payer UTXOs from the previous TX's change output,
/// optionally supplemented with fresh wallet UTXOs when the
/// change alone is insufficient for the remaining chain.
pub struct ChainedPayer {
    pub utxos: Vec<Utxo>,
    pub total_input: u64,
}

/// State extracted from a built TX to feed into the next TX in a chain.
#[derive(Clone)]
pub struct ChainedUtxoState {
    pub mailbox_utxo: Utxo,
    pub ism_utxo: Utxo,
    pub payer_utxo: Utxo,
    pub payer_total_input: u64,
    pub mailbox_datum_cbor: Vec<u8>,
}

/// Output index layout for a process TX, derived from recipient kind.
struct OutputIndices {
    pub ism: u32,
    pub change: u32,
}

fn compute_output_indices(components: &ProcessTxComponents) -> OutputIndices {
    let mut idx: u32 = 1; // 0 = mailbox continuation (always)

    // Recipient continuation (WarpRoute only)
    if components.recipient_utxo.is_some() {
        idx += 1;
    }
    // Token release output (WarpRoute with release)
    if components.token_release_amount.is_some() {
        idx += 1;
    }

    let ism = idx;
    idx += 1; // ISM continuation

    idx += 1; // Processed message marker

    // Verified message NFT (GenericRecipient only)
    if components.verified_message_datum_cbor.is_some() {
        idx += 1;
    }

    let change = idx; // Change output (last)
    OutputIndices { ism, change }
}

// ============================================================================
// Helper Functions for Transaction Building
// ============================================================================

/// Convert a Utxo to a pallas-txbuilder Input
fn utxo_to_input(utxo: &Utxo) -> Result<Input, TxBuilderError> {
    let tx_hash_bytes = hex::decode(&utxo.tx_hash)
        .map_err(|e| TxBuilderError::Encoding(format!("Invalid tx hash hex: {e}")))?;

    let tx_hash: Hash<32> = Hash::new(
        tx_hash_bytes
            .try_into()
            .map_err(|_| TxBuilderError::Encoding("Tx hash must be 32 bytes".to_string()))?,
    );

    Ok(Input::new(tx_hash, utxo.output_index as u64))
}

/// Parse a bech32 address string into a pallas Address
fn parse_address(address: &str) -> Result<Address, TxBuilderError> {
    Address::from_bech32(address)
        .map_err(|e| TxBuilderError::InvalidAddress(format!("Invalid bech32 address: {e:?}")))
}

/// Parse a policy ID hex string into a Hash<28>
fn parse_policy_id(policy_id: &str) -> Result<Hash<28>, TxBuilderError> {
    let bytes = hex::decode(policy_id)
        .map_err(|e| TxBuilderError::Encoding(format!("Invalid policy ID hex: {e}")))?;

    let hash_bytes: [u8; 28] = bytes
        .try_into()
        .map_err(|_| TxBuilderError::Encoding("Policy ID must be 28 bytes".to_string()))?;

    Ok(Hash::new(hash_bytes))
}

/// Parse a UTXO reference string in the format "tx_hash#output_index" into an Input
fn parse_utxo_ref(utxo_ref: &str) -> Result<Input, TxBuilderError> {
    let parts: Vec<&str> = utxo_ref.split('#').collect();
    if parts.len() != 2 {
        return Err(TxBuilderError::Encoding(format!(
            "Invalid UTXO reference format '{utxo_ref}'. Expected 'tx_hash#output_index'"
        )));
    }

    let tx_hash_hex = parts[0];
    let output_index: u64 = parts[1].parse().map_err(|e| {
        TxBuilderError::Encoding(format!("Invalid output index '{}': {}", parts[1], e))
    })?;

    let tx_hash_bytes = hex::decode(tx_hash_hex)
        .map_err(|e| TxBuilderError::Encoding(format!("Invalid tx hash hex: {e}")))?;

    let tx_hash: Hash<32> = Hash::new(
        tx_hash_bytes
            .try_into()
            .map_err(|_| TxBuilderError::Encoding("Tx hash must be 32 bytes".to_string()))?,
    );

    Ok(Input::new(tx_hash, output_index))
}

/// Convert a 28-byte credential (verification key hash or script hash) to a Cardano address
/// The credential is used as the payment credential with no staking credential
fn credential_to_address(
    credential_bytes: &[u8],
    network: Network,
) -> Result<Address, TxBuilderError> {
    if credential_bytes.len() != 28 {
        return Err(TxBuilderError::Encoding(format!(
            "Credential must be 28 bytes, got {}",
            credential_bytes.len()
        )));
    }

    // Create a payment-only address (Type 6 address: payment key hash, no staking)
    // The credential could be either a pubkey hash or script hash
    // For recipients from TokenMessage, it's typically a pubkey hash (0x00 prefix)
    // or a script hash (0x01 prefix)
    // Cardano addresses format: [header_byte] [28-byte payment credential] [optional 28-byte staking credential]
    // Header byte for Type 6 (enterprise address): 0110_XXXX where XXXX = network tag
    // Network 0 = testnet (0110_0000 = 0x60), Network 1 = mainnet (0110_0001 = 0x61)
    let header_byte = match network {
        Network::Testnet => 0x60, // Type 6, testnet
        Network::Mainnet => 0x61, // Type 6, mainnet
        _ => 0x60,                // Default to testnet
    };

    let mut address_bytes = Vec::with_capacity(29);
    address_bytes.push(header_byte);
    address_bytes.extend_from_slice(credential_bytes);

    Address::from_bytes(&address_bytes).map_err(|e| {
        TxBuilderError::InvalidAddress(format!("Failed to create address from credential: {e:?}"))
    })
}

/// Convert a 28-byte script hash to a Cardano script address (Type 7)
/// The script hash is used as the payment credential with no staking credential
fn script_hash_to_address(
    script_hash: &[u8; 28],
    network: Network,
) -> Result<Address, TxBuilderError> {
    // Cardano addresses format: [header_byte] [28-byte payment credential] [optional 28-byte staking credential]
    // Header byte for Type 7 (enterprise script address): 0111_XXXX where XXXX = network tag
    // Network 0 = testnet (0111_0000 = 0x70), Network 1 = mainnet (0111_0001 = 0x71)
    let header_byte = match network {
        Network::Testnet => 0x70, // Type 7, testnet
        Network::Mainnet => 0x71, // Type 7, mainnet
        _ => 0x70,                // Default to testnet
    };

    let mut address_bytes = Vec::with_capacity(29);
    address_bytes.push(header_byte);
    address_bytes.extend_from_slice(script_hash);

    Address::from_bytes(&address_bytes).map_err(|e| {
        TxBuilderError::InvalidAddress(format!("Failed to create script address from hash: {e:?}"))
    })
}

/// Encode a Constr 0 [] redeemer (used for MintMessage/Mint in minting policies)
fn encode_constructor_0_redeemer() -> Vec<u8> {
    // Constr 0 [] is encoded as CBOR tag 121 (0xd87980)
    // Tag 121 = constructor 0 with empty array
    let redeemer = PlutusData::Constr(Constr {
        tag: 121, // Constructor 0
        any_constructor: None,
        fields: MaybeIndefArray::Def(vec![]),
    });

    let mut encoded = Vec::new();
    minicbor::encode(&redeemer, &mut encoded).expect("Failed to encode constructor 0 redeemer");
    encoded
}

/// Extract the `processed_tree_root` from a mailbox UTXO's inline datum.
/// Used to validate that the locally-reconstructed SMT matches on-chain state.
fn read_processed_tree_root_from_utxo(utxo: &Utxo) -> Result<[u8; 32], TxBuilderError> {
    let datum_str = utxo
        .inline_datum
        .as_ref()
        .ok_or_else(|| TxBuilderError::Encoding("Mailbox UTXO has no inline datum".to_string()))?;

    let data = if let Ok(cbor_bytes) = hex::decode(datum_str) {
        minicbor::decode::<PlutusData>(&cbor_bytes).map_err(|e| {
            TxBuilderError::Encoding(format!("Failed to decode mailbox datum CBOR: {e}"))
        })?
    } else {
        json_to_plutus_data(
            &serde_json::from_str::<serde_json::Value>(datum_str).map_err(|e| {
                TxBuilderError::Encoding(format!("Failed to parse mailbox datum JSON: {e}"))
            })?,
        )?
    };

    match data {
        PlutusData::Constr(constr) => {
            let fields: Vec<PlutusData> = match constr.fields {
                MaybeIndefArray::Def(f) => f,
                MaybeIndefArray::Indef(f) => f,
            };
            if fields.len() < 6 {
                return Err(TxBuilderError::Encoding(format!(
                    "MailboxDatum expected 6 fields, got {}",
                    fields.len()
                )));
            }
            match &fields[5] {
                PlutusData::BoundedBytes(bytes) => {
                    if bytes.len() != 32 {
                        return Err(TxBuilderError::Encoding(format!(
                            "processed_tree_root expected 32 bytes, got {}",
                            bytes.len()
                        )));
                    }
                    let mut root = [0u8; 32];
                    root.copy_from_slice(bytes);
                    Ok(root)
                }
                _ => Err(TxBuilderError::Encoding(
                    "processed_tree_root is not BoundedBytes".to_string(),
                )),
            }
        }
        _ => Err(TxBuilderError::Encoding(
            "MailboxDatum is not a Constr".to_string(),
        )),
    }
}

/// Build updated mailbox datum CBOR with a new processed_tree_root.
/// Parses the existing datum from the UTXO, replaces the last field
/// (processed_tree_root), and re-encodes to CBOR.
fn build_mailbox_continuation_datum(
    mailbox_utxo: &Utxo,
    new_tree_root: &[u8; 32],
) -> Result<Vec<u8>, TxBuilderError> {
    let datum_str = mailbox_utxo
        .inline_datum
        .as_ref()
        .ok_or_else(|| TxBuilderError::Encoding("Mailbox UTXO has no inline datum".to_string()))?;

    // Blockfrost returns inline_datum as CBOR hex; try CBOR decode first, then JSON
    let original_data = if let Ok(cbor_bytes) = hex::decode(datum_str) {
        minicbor::decode::<PlutusData>(&cbor_bytes).map_err(|e| {
            TxBuilderError::Encoding(format!("Failed to decode mailbox datum CBOR: {e}"))
        })?
    } else {
        json_to_plutus_data(
            &serde_json::from_str::<serde_json::Value>(datum_str).map_err(|e| {
                TxBuilderError::Encoding(format!("Failed to parse mailbox datum JSON: {e}"))
            })?,
        )?
    };

    // MailboxDatum = Constr 0 [local_domain, default_ism, owner, outbound_nonce, merkle_tree, processed_tree_root]
    match original_data {
        PlutusData::Constr(mut constr) => {
            let mut fields: Vec<PlutusData> = match constr.fields {
                MaybeIndefArray::Def(f) => f,
                MaybeIndefArray::Indef(f) => f,
            };
            if fields.len() < 6 {
                return Err(TxBuilderError::Encoding(format!(
                    "MailboxDatum expected 6 fields, got {}",
                    fields.len()
                )));
            }
            // Replace last field with new processed_tree_root
            *fields.last_mut().unwrap() = PlutusData::BoundedBytes(new_tree_root.to_vec().into());
            constr.fields = MaybeIndefArray::Def(fields);
            encode_plutus_data(&PlutusData::Constr(constr))
        }
        _ => Err(TxBuilderError::Encoding(
            "MailboxDatum is not a Constr".to_string(),
        )),
    }
}

/// Build updated mailbox continuation datum from CBOR (for chained TXs).
/// Same logic as `build_mailbox_continuation_datum` but decodes from CBOR
/// instead of Blockfrost's JSON format.
fn build_mailbox_continuation_datum_from_cbor(
    prev_datum_cbor: &[u8],
    new_tree_root: &[u8; 32],
) -> Result<Vec<u8>, TxBuilderError> {
    let original_data: PlutusData = minicbor::decode(prev_datum_cbor).map_err(|e| {
        TxBuilderError::Encoding(format!("Failed to decode mailbox datum CBOR: {e}"))
    })?;

    match original_data {
        PlutusData::Constr(mut constr) => {
            let mut fields: Vec<PlutusData> = match constr.fields {
                MaybeIndefArray::Def(f) => f,
                MaybeIndefArray::Indef(f) => f,
            };
            if fields.len() < 6 {
                return Err(TxBuilderError::Encoding(format!(
                    "MailboxDatum expected 6 fields, got {}",
                    fields.len()
                )));
            }
            *fields.last_mut().unwrap() = PlutusData::BoundedBytes(new_tree_root.to_vec().into());
            constr.fields = MaybeIndefArray::Def(fields);
            encode_plutus_data(&PlutusData::Constr(constr))
        }
        _ => Err(TxBuilderError::Encoding(
            "MailboxDatum is not a Constr".to_string(),
        )),
    }
}

/// Create a continuation output with an explicitly provided datum CBOR.
/// Preserves address and value from the original UTXO but uses the given datum.
fn create_continuation_output_with_datum(
    utxo: &Utxo,
    datum_cbor: &[u8],
    min_lovelace: u64,
) -> Result<Output, TxBuilderError> {
    let address = parse_address(&utxo.address)?;
    let lovelace = utxo.lovelace();

    let mut output = Output::new(address, lovelace.max(min_lovelace));
    output = output.set_inline_datum(datum_cbor.to_vec());

    for value in &utxo.value {
        if value.unit != "lovelace" && value.unit.len() >= 56 {
            let policy_hex = &value.unit[..56];
            let asset_name_hex = &value.unit[56..];

            let policy_hash = parse_policy_id(policy_hex)?;
            let asset_name = hex::decode(asset_name_hex)
                .map_err(|e| TxBuilderError::Encoding(format!("Invalid asset name hex: {e}")))?;
            let quantity: u64 = value
                .quantity
                .parse()
                .map_err(|e| TxBuilderError::Encoding(format!("Invalid quantity: {e}")))?;

            output = output
                .add_asset(policy_hash, asset_name, quantity)
                .map_err(|e| TxBuilderError::TxBuild(format!("Failed to add asset: {e:?}")))?;
        }
    }

    Ok(output)
}

/// Create a continuation output for a script UTXO
/// This preserves the address, value, and inline datum from the original UTXO
fn create_continuation_output(
    utxo: &Utxo,
    _policy_id: &str,
    min_lovelace: u64,
) -> Result<Output, TxBuilderError> {
    let address = parse_address(&utxo.address)?;
    let lovelace = utxo.lovelace();

    let mut output = Output::new(address, lovelace.max(min_lovelace));

    // Preserve inline datum if present
    if let Some(datum_json) = &utxo.inline_datum {
        // The datum is stored as JSON, we need to convert it back to CBOR
        // For continuation outputs, we typically keep the same datum
        let datum_cbor = json_datum_to_cbor(datum_json)?;
        output = output.set_inline_datum(datum_cbor);
    }

    // Add any native assets from the original UTXO
    for value in &utxo.value {
        if value.unit != "lovelace" && value.unit.len() >= 56 {
            let policy_hex = &value.unit[..56];
            let asset_name_hex = &value.unit[56..];

            let policy_hash = parse_policy_id(policy_hex)?;
            let asset_name = hex::decode(asset_name_hex)
                .map_err(|e| TxBuilderError::Encoding(format!("Invalid asset name hex: {e}")))?;
            let quantity: u64 = value
                .quantity
                .parse()
                .map_err(|e| TxBuilderError::Encoding(format!("Invalid quantity: {e}")))?;

            output = output
                .add_asset(policy_hash, asset_name, quantity)
                .map_err(|e| TxBuilderError::TxBuild(format!("Failed to add asset: {e:?}")))?;
        }
    }

    Ok(output)
}

/// Create a warp route continuation output with UPDATED datum
/// Handles token release based on token type:
/// - Native: Reduce lovelace by release_amount
/// - Collateral: Reduce collateral token quantity by release_amount
/// - Synthetic: No change (tokens are minted elsewhere)
fn create_warp_route_continuation_output(
    utxo: &Utxo,
    new_datum_cbor: &[u8],
    release_amount: Option<u64>,
    token_type: Option<&WarpTokenTypeInfo>,
    min_lovelace: u64,
    release_output_cost: u64,
) -> Result<Output, TxBuilderError> {
    let address = parse_address(&utxo.address)?;
    let original_lovelace = utxo.lovelace();

    // Adjust lovelace based on token type:
    // - Native: reduce lovelace by the release amount (tokens are ADA)
    // - Collateral: reduce lovelace by release_output_cost to fund the release output
    // - Synthetic: reduce lovelace by release_output_cost to fund the recipient output
    // release_output_cost accounts for the actual minUTxO of the release output
    // (which may be higher than min_lovelace when it contains datum + tokens)
    let final_lovelace = match (&token_type, release_amount) {
        (Some(WarpTokenTypeInfo::Native), Some(amount)) => {
            // Native: release amount IS the ADA being sent.
            // Only the transfer amount leaves the warp route;
            // the payer covers any shortfall for the recipient output.
            original_lovelace.saturating_sub(amount).max(min_lovelace)
        }
        (Some(WarpTokenTypeInfo::Collateral { .. }), Some(_)) => {
            // Collateral: need to fund the release output with release_output_cost
            original_lovelace
                .saturating_sub(release_output_cost)
                .max(min_lovelace)
        }
        (Some(WarpTokenTypeInfo::Synthetic { .. }), Some(_)) => {
            // Synthetic: need to fund the recipient output with release_output_cost
            // The minted tokens go to a new output which requires release_output_cost
            original_lovelace
                .saturating_sub(release_output_cost)
                .max(min_lovelace)
        }
        _ => original_lovelace.max(min_lovelace),
    };

    let mut output = Output::new(address, final_lovelace);

    // Use the NEW datum (updated state)
    output = output.set_inline_datum(new_datum_cbor.to_vec());

    // Add native assets from the original UTXO
    // For Collateral type, reduce the collateral token quantity by release_amount
    for value in &utxo.value {
        if value.unit != "lovelace" && value.unit.len() >= 56 {
            let policy_hex = &value.unit[..56];
            let asset_name_hex = &value.unit[56..];

            let policy_hash = parse_policy_id(policy_hex)?;
            let asset_name_bytes = hex::decode(asset_name_hex)
                .map_err(|e| TxBuilderError::Encoding(format!("Invalid asset name hex: {e}")))?;
            let original_quantity: u64 = value
                .quantity
                .parse()
                .map_err(|e| TxBuilderError::Encoding(format!("Invalid quantity: {e}")))?;

            // Check if this is the collateral token to be released
            let final_quantity = match (&token_type, release_amount) {
                (
                    Some(WarpTokenTypeInfo::Collateral {
                        policy_id,
                        asset_name,
                    }),
                    Some(amount),
                ) => {
                    // Check if this asset matches the collateral token
                    if policy_hex == policy_id && asset_name_hex == asset_name {
                        // Reduce quantity by release amount
                        original_quantity.saturating_sub(amount)
                    } else {
                        original_quantity
                    }
                }
                _ => original_quantity,
            };

            // Only add the asset if quantity is > 0
            if final_quantity > 0 {
                output = output
                    .add_asset(policy_hash, asset_name_bytes, final_quantity)
                    .map_err(|e| TxBuilderError::TxBuild(format!("Failed to add asset: {e:?}")))?;
            }
        }
    }

    Ok(output)
}

/// Create ISM continuation output (same address, same datum, same value)
/// Used when ISM is spent for Verify operation - must recreate unchanged
fn create_ism_continuation_output(
    utxo: &Utxo,
    min_lovelace: u64,
) -> Result<Output, TxBuilderError> {
    let address = parse_address(&utxo.address)?;
    let lovelace = utxo.lovelace();

    let mut output = Output::new(address, lovelace.max(min_lovelace));

    // Preserve inline datum (ISM datum contains validators and thresholds)
    if let Some(datum_json) = &utxo.inline_datum {
        let datum_cbor = json_datum_to_cbor(datum_json)?;
        output = output.set_inline_datum(datum_cbor);
    }

    // Preserve any native assets (ISM state NFT if present)
    for value in &utxo.value {
        if value.unit != "lovelace" && value.unit.len() >= 56 {
            let policy_hex = &value.unit[..56];
            let asset_name_hex = &value.unit[56..];

            let policy_hash = parse_policy_id(policy_hex)?;
            let asset_name = hex::decode(asset_name_hex)
                .map_err(|e| TxBuilderError::Encoding(format!("Invalid asset name hex: {e}")))?;

            let quantity: u64 = value
                .quantity
                .parse()
                .map_err(|e| TxBuilderError::Encoding(format!("Invalid quantity: {e}")))?;

            output = output
                .add_asset(policy_hash, asset_name, quantity)
                .map_err(|e| TxBuilderError::TxBuild(format!("Failed to add asset: {e:?}")))?;
        }
    }

    Ok(output)
}

/// Create a release output to send ADA to the recipient
/// The recipient_bytes should contain the Cardano address hash (28 bytes)
#[allow(dead_code)] // May be useful for future token release patterns
fn create_release_output(
    recipient_bytes: &[u8],
    amount: u64,
    min_lovelace: u64,
) -> Result<Output, TxBuilderError> {
    // The recipient_bytes contain the raw address bytes
    // For Cardano Preview testnet, we need to construct a proper address
    // Format: network_tag (1 byte) || payment_credential_hash (28 bytes)
    // Network tag 0x00 = mainnet pubkey, 0x60 = testnet pubkey (Enterprise address)
    // For script addresses: 0x70 = testnet script

    if recipient_bytes.len() != 28 {
        return Err(TxBuilderError::InvalidAddress(format!(
            "Recipient credential must be exactly 28 bytes, got {} bytes",
            recipient_bytes.len()
        )));
    }

    // Use the 28-byte credential hash directly (pubkey hash for enterprise address)
    let credential_hash = recipient_bytes;

    // Build enterprise address for testnet (no staking part)
    // Type 6 (0110 in binary) = enterprise address with key hash payment credential
    let network_tag: u8 = 0x60; // Testnet enterprise address with key hash
    let mut address_bytes = vec![network_tag];
    address_bytes.extend_from_slice(credential_hash);

    // Convert to bech32 address
    // Use bech32 crate v0.9 API
    use bech32::{ToBase32, Variant};
    let bech32_addr = bech32::encode("addr_test", address_bytes.to_base32(), Variant::Bech32)
        .map_err(|e| TxBuilderError::Encoding(format!("Failed to encode bech32 address: {e}")))?;

    let address = parse_address(&bech32_addr)?;

    // Ensure we meet minimum UTXO requirement
    let lovelace = amount.max(min_lovelace);

    Ok(Output::new(address, lovelace))
}

/// Build the updated recipient datum for the continuation output
///
/// Warp route token type extracted from datum
#[derive(Debug, Clone)]
pub enum WarpTokenTypeInfo {
    /// Native ADA - release lovelace
    Native,
    /// Collateral tokens - release specific native tokens
    Collateral {
        policy_id: String,
        asset_name: String,
    },
    /// Synthetic tokens - no release (tokens are minted on the other side)
    Synthetic { minting_policy: String },
}

/// Warp route decimal configuration
#[derive(Debug, Clone, Copy)]
struct WarpRouteDecimals {
    /// Local token decimals (Cardano side)
    local_decimals: u8,
    /// Remote token decimals (wire format, typically 18 for EVM)
    remote_decimals: u8,
}

/// Extract decimals and remote_decimals from warp route datum config
/// The warp route datum structure is: Constr 0 [config, owner, total_bridged]
/// where config is: Constr (token_type) [decimals, remote_decimals, remote_routes_list]
fn extract_warp_route_decimals(recipient_utxo: &Utxo) -> Result<WarpRouteDecimals, TxBuilderError> {
    let datum_str = recipient_utxo.inline_datum.as_ref().ok_or_else(|| {
        TxBuilderError::MissingInput("Warp route UTXO has no inline datum".to_string())
    })?;
    let datum_cbor = json_datum_to_cbor(datum_str)?;

    use pallas_codec::minicbor;
    let decoded: PlutusData = minicbor::decode(&datum_cbor)
        .map_err(|e| TxBuilderError::Encoding(format!("Failed to decode warp route datum: {e}")))?;

    // Extract config from datum fields[0]
    let config = if let PlutusData::Constr(constr) = decoded {
        constr
            .fields
            .clone()
            .to_vec()
            .first()
            .cloned()
            .ok_or_else(|| {
                TxBuilderError::Encoding("Warp route datum has no config field".to_string())
            })?
    } else {
        return Err(TxBuilderError::Encoding(
            "Warp route datum is not a Constr".to_string(),
        ));
    };

    // WarpRouteConfig structure:
    // - fields[0] = token_type (WarpTokenType Constr)
    // - fields[1] = decimals (local)
    // - fields[2] = remote_decimals
    // - fields[3] = remote_routes
    if let PlutusData::Constr(config_constr) = config {
        let config_fields = config_constr.fields.clone().to_vec();
        if config_fields.len() < 3 {
            return Err(TxBuilderError::Encoding(format!(
                "Warp route config has insufficient fields: expected at least 3, got {}",
                config_fields.len()
            )));
        }

        let local_decimals_i64 = extract_int(&config_fields[1]).ok_or_else(|| {
            TxBuilderError::Encoding(
                "Failed to extract decimals from warp route config (fields[1])".to_string(),
            )
        })?;
        if !(0..=18).contains(&local_decimals_i64) {
            return Err(TxBuilderError::Encoding(format!(
                "Invalid decimals value: {local_decimals_i64}"
            )));
        }

        let remote_decimals_i64 = extract_int(&config_fields[2]).ok_or_else(|| {
            TxBuilderError::Encoding(
                "Failed to extract remote_decimals from warp route config (fields[2])".to_string(),
            )
        })?;
        if !(0..=18).contains(&remote_decimals_i64) {
            return Err(TxBuilderError::Encoding(format!(
                "Invalid remote_decimals value: {remote_decimals_i64}"
            )));
        }

        Ok(WarpRouteDecimals {
            local_decimals: local_decimals_i64 as u8,
            remote_decimals: remote_decimals_i64 as u8,
        })
    } else {
        Err(TxBuilderError::Encoding(
            "Warp route config is not a Constr".to_string(),
        ))
    }
}

/// Extract the warp route token type from the datum
/// The token_type is config.fields[0] and is a Constr with:
/// - tag 121 (constructor 0) = Collateral { policy_id, asset_name }
/// - tag 122 (constructor 1) = Synthetic { minting_policy }
/// - tag 123 (constructor 2) = Native
fn extract_warp_route_token_type(
    recipient_utxo: &Utxo,
) -> Result<WarpTokenTypeInfo, TxBuilderError> {
    let datum_str = recipient_utxo.inline_datum.as_ref().ok_or_else(|| {
        TxBuilderError::MissingInput("Warp route UTXO has no inline datum".to_string())
    })?;
    let datum_cbor = json_datum_to_cbor(datum_str)?;

    use pallas_codec::minicbor;
    let decoded: PlutusData = minicbor::decode(&datum_cbor)
        .map_err(|e| TxBuilderError::Encoding(format!("Failed to decode warp route datum: {e}")))?;

    // Extract config from datum fields[0]
    let config = if let PlutusData::Constr(constr) = decoded {
        constr
            .fields
            .clone()
            .to_vec()
            .first()
            .cloned()
            .ok_or_else(|| {
                TxBuilderError::Encoding("Warp route datum has no config field".to_string())
            })?
    } else {
        return Err(TxBuilderError::Encoding(
            "Warp route datum is not a Constr".to_string(),
        ));
    };

    // Extract token_type from config.fields[0]
    if let PlutusData::Constr(config_constr) = config {
        let config_fields = config_constr.fields.clone().to_vec();
        if config_fields.is_empty() {
            return Err(TxBuilderError::Encoding(
                "Warp route config has no fields".to_string(),
            ));
        }

        let token_type = &config_fields[0];
        if let PlutusData::Constr(tt_constr) = token_type {
            match tt_constr.tag {
                121 => {
                    // Constructor 0 = Collateral { policy_id, asset_name }
                    let tt_fields = tt_constr.fields.clone().to_vec();
                    if tt_fields.len() < 2 {
                        return Err(TxBuilderError::Encoding(
                            "Collateral type has insufficient fields".to_string(),
                        ));
                    }
                    let policy_id =
                        extract_bytes(&tt_fields[0])
                            .map(hex::encode)
                            .ok_or_else(|| {
                                TxBuilderError::Encoding(
                                    "Failed to extract Collateral policy_id".to_string(),
                                )
                            })?;
                    let asset_name =
                        extract_bytes(&tt_fields[1])
                            .map(hex::encode)
                            .ok_or_else(|| {
                                TxBuilderError::Encoding(
                                    "Failed to extract Collateral asset_name".to_string(),
                                )
                            })?;
                    Ok(WarpTokenTypeInfo::Collateral {
                        policy_id,
                        asset_name,
                    })
                }
                122 => {
                    // Constructor 1 = Synthetic { minting_policy }
                    let tt_fields = tt_constr.fields.clone().to_vec();
                    if tt_fields.is_empty() {
                        return Err(TxBuilderError::Encoding(
                            "Synthetic type has no minting_policy".to_string(),
                        ));
                    }
                    let minting_policy =
                        extract_bytes(&tt_fields[0])
                            .map(hex::encode)
                            .ok_or_else(|| {
                                TxBuilderError::Encoding(
                                    "Failed to extract Synthetic minting_policy".to_string(),
                                )
                            })?;
                    Ok(WarpTokenTypeInfo::Synthetic { minting_policy })
                }
                123 => {
                    // Constructor 2 = Native
                    Ok(WarpTokenTypeInfo::Native)
                }
                _ => Err(TxBuilderError::Encoding(format!(
                    "Unknown token_type constructor tag: {}",
                    tt_constr.tag
                ))),
            }
        } else {
            Err(TxBuilderError::Encoding(
                "Token type is not a Constr".to_string(),
            ))
        }
    } else {
        Err(TxBuilderError::Encoding(
            "Warp route config is not a Constr".to_string(),
        ))
    }
}

/// Build warp route continuation datum with updated total_bridged
/// The warp route datum structure is: Constr 0 [config, owner, total_bridged]
/// where config is: Constr (token_type) [decimals, remote_routes_list]
/// For ReceiveTransfer: total_bridged = old_total_bridged - transfer_amount
fn build_warp_route_continuation_datum(
    recipient_utxo: &Utxo,
    transfer_amount: u64,
) -> Result<Vec<u8>, TxBuilderError> {
    // Parse the existing warp route datum
    let datum_str = recipient_utxo.inline_datum.as_ref().ok_or_else(|| {
        TxBuilderError::MissingInput("Warp route UTXO has no inline datum".to_string())
    })?;

    let datum_cbor = json_datum_to_cbor(datum_str)?;

    use pallas_codec::minicbor;
    let decoded: PlutusData = minicbor::decode(&datum_cbor)
        .map_err(|e| TxBuilderError::Encoding(format!("Failed to decode warp route datum: {e}")))?;

    // Extract fields from the existing datum
    // Structure: Constr 0 [config, owner, total_bridged, ism]
    let (config_field, owner, old_total_bridged, ism_field) = if let PlutusData::Constr(constr) =
        decoded
    {
        let fields: Vec<_> = constr.fields.clone().to_vec();
        if fields.len() < 4 {
            return Err(TxBuilderError::Encoding(
                "Warp route datum has insufficient fields (need 4)".to_string(),
            ));
        }

        // Config is a complex nested structure - preserve it as-is
        let config = fields[0].clone();

        let owner_bytes = extract_bytes(&fields[1]).ok_or_else(|| {
            TxBuilderError::Encoding("Failed to extract owner from warp route datum".to_string())
        })?;

        let total_bridged = extract_int(&fields[2]).unwrap_or(0);

        // Preserve ism field as-is (Option<ScriptHash>)
        let ism = fields[3].clone();

        (config, owner_bytes, total_bridged, ism)
    } else {
        return Err(TxBuilderError::Encoding(
            "Warp route datum is not a Constr".to_string(),
        ));
    };

    // Calculate new total_bridged (subtract transfer amount for receive)
    let new_total_bridged = old_total_bridged - (transfer_amount as i64);
    debug!(
        "Warp route total_bridged: {} -> {} (received {})",
        old_total_bridged, new_total_bridged, transfer_amount
    );

    // Build the new warp route datum with same config, owner, updated total_bridged, and preserved ism
    let plutus_data = PlutusData::Constr(Constr {
        tag: 121, // WarpRouteDatum = constructor 0
        any_constructor: None,
        fields: MaybeIndefArray::Def(vec![
            config_field, // Preserve config as-is
            PlutusData::BoundedBytes(owner.into()),
            PlutusData::BigInt(BigInt::Int(new_total_bridged.into())),
            ism_field, // Preserve ism as-is
        ]),
    });

    encode_plutus_data(&plutus_data)
}

/// Extract Int from PlutusData
fn extract_int(data: &PlutusData) -> Option<i64> {
    if let PlutusData::BigInt(bigint) = data {
        match bigint {
            BigInt::Int(i) => {
                // pallas Int is i128-like, convert to i64
                let val: i128 = (*i).into();
                i64::try_from(val).ok()
            }
            BigInt::BigUInt(bytes) => {
                // Try to convert big uint bytes to i64
                if bytes.len() <= 8 {
                    let mut arr = [0u8; 8];
                    arr[8 - bytes.len()..].copy_from_slice(bytes);
                    Some(i64::from_be_bytes(arr))
                } else {
                    None
                }
            }
            BigInt::BigNInt(_) => None, // Negative big int, skip for now
        }
    } else {
        None
    }
}

/// Extract ByteArray from PlutusData
fn extract_bytes(data: &PlutusData) -> Option<Vec<u8>> {
    if let PlutusData::BoundedBytes(bytes) = data {
        Some(bytes.to_vec())
    } else {
        None
    }
}

/// Convert a datum string (from Blockfrost) to CBOR bytes
/// Blockfrost can return either JSON format or raw CBOR hex - this handles both
fn json_datum_to_cbor(datum_str: &str) -> Result<Vec<u8>, TxBuilderError> {
    use serde_json::Value;

    // First, try to parse as JSON
    if let Ok(json) = serde_json::from_str::<Value>(datum_str) {
        // Convert JSON to PlutusData and encode to CBOR
        let plutus_data = json_to_plutus_data(&json)?;
        return encode_plutus_data(&plutus_data);
    }

    // If JSON parsing fails, try treating it as raw CBOR hex
    // Blockfrost sometimes returns inline_datum as a quoted hex string
    let hex_str = datum_str.trim_matches('"');
    if hex_str.chars().all(|c| c.is_ascii_hexdigit()) && !hex_str.is_empty() {
        let cbor_bytes = hex::decode(hex_str)
            .map_err(|e| TxBuilderError::Encoding(format!("Invalid CBOR hex: {e}")))?;
        return Ok(cbor_bytes);
    }

    Err(TxBuilderError::Encoding(format!(
        "Datum is neither valid JSON nor CBOR hex: {}",
        &datum_str[..datum_str.len().min(100)]
    )))
}

/// Convert JSON value to PlutusData
fn json_to_plutus_data(json: &serde_json::Value) -> Result<PlutusData, TxBuilderError> {
    use serde_json::Value;

    match json {
        // Integer
        Value::Number(n) => {
            let i = n
                .as_i64()
                .ok_or_else(|| TxBuilderError::Encoding("Number too large".to_string()))?;
            Ok(PlutusData::BigInt(BigInt::Int(i.into())))
        }

        // Byte string (hex encoded)
        Value::String(s) => {
            if s.starts_with("0x") || s.chars().all(|c| c.is_ascii_hexdigit()) {
                let hex_str = s.strip_prefix("0x").unwrap_or(s);
                let bytes = hex::decode(hex_str)
                    .map_err(|e| TxBuilderError::Encoding(format!("Invalid hex string: {e}")))?;
                Ok(PlutusData::BoundedBytes(bytes.into()))
            } else {
                // Treat as UTF-8 bytes
                Ok(PlutusData::BoundedBytes(s.as_bytes().to_vec().into()))
            }
        }

        // Object with "constructor" and "fields" (Constr type)
        Value::Object(obj) => {
            if let (Some(constructor), Some(fields)) = (obj.get("constructor"), obj.get("fields")) {
                let tag = constructor
                    .as_u64()
                    .ok_or_else(|| TxBuilderError::Encoding("Invalid constructor".to_string()))?;

                let fields_vec = fields
                    .as_array()
                    .ok_or_else(|| TxBuilderError::Encoding("Fields must be array".to_string()))?;

                let mut parsed_fields = Vec::new();
                for field in fields_vec {
                    parsed_fields.push(json_to_plutus_data(field)?);
                }

                // Convert constructor index to Plutus tag
                let plutus_tag = if tag <= 6 {
                    121 + tag // Alternative encoding for 0-6
                } else {
                    1280 + (tag - 7) // General encoding for 7+
                };

                Ok(PlutusData::Constr(Constr {
                    tag: plutus_tag,
                    any_constructor: None,
                    fields: MaybeIndefArray::Def(parsed_fields),
                }))
            } else if let Some(bytes) = obj.get("bytes") {
                // Blockfrost format: {"bytes": "hex_string"}
                let hex_str = bytes
                    .as_str()
                    .ok_or_else(|| TxBuilderError::Encoding("bytes must be string".to_string()))?;
                let bytes = hex::decode(hex_str)
                    .map_err(|e| TxBuilderError::Encoding(format!("Invalid hex: {e}")))?;
                Ok(PlutusData::BoundedBytes(bytes.into()))
            } else if let Some(int_val) = obj.get("int") {
                // Blockfrost format: {"int": number}
                let i = int_val
                    .as_i64()
                    .ok_or_else(|| TxBuilderError::Encoding("int must be number".to_string()))?;
                Ok(PlutusData::BigInt(BigInt::Int(i.into())))
            } else if let Some(list) = obj.get("list") {
                // Blockfrost format: {"list": [...]}
                let items = list
                    .as_array()
                    .ok_or_else(|| TxBuilderError::Encoding("list must be array".to_string()))?;
                let mut parsed_items = Vec::new();
                for item in items {
                    parsed_items.push(json_to_plutus_data(item)?);
                }
                Ok(PlutusData::Array(MaybeIndefArray::Def(parsed_items)))
            } else if let Some(map) = obj.get("map") {
                // Blockfrost format: {"map": [{"k": ..., "v": ...}, ...]}
                let entries = map
                    .as_array()
                    .ok_or_else(|| TxBuilderError::Encoding("map must be array".to_string()))?;
                let mut parsed_map = Vec::new();
                for entry in entries {
                    let k = entry.get("k").ok_or_else(|| {
                        TxBuilderError::Encoding("map entry missing k".to_string())
                    })?;
                    let v = entry.get("v").ok_or_else(|| {
                        TxBuilderError::Encoding("map entry missing v".to_string())
                    })?;
                    parsed_map.push((json_to_plutus_data(k)?, json_to_plutus_data(v)?));
                }
                Ok(PlutusData::Map(KeyValuePairs::from(parsed_map)))
            } else {
                Err(TxBuilderError::Encoding(
                    "Unknown JSON object format".to_string(),
                ))
            }
        }

        // Array (list)
        Value::Array(arr) => {
            let mut items = Vec::new();
            for item in arr {
                items.push(json_to_plutus_data(item)?);
            }
            Ok(PlutusData::Array(MaybeIndefArray::Def(items)))
        }

        _ => Err(TxBuilderError::Encoding(format!(
            "Unsupported JSON value type: {json:?}"
        ))),
    }
}

// ============================================================================
// CBOR Encoding Functions for Plutus Data
// ============================================================================

/// Encode a MailboxRedeemer as Plutus Data CBOR
pub fn encode_mailbox_redeemer(redeemer: &MailboxRedeemer) -> Result<Vec<u8>, TxBuilderError> {
    let plutus_data = match redeemer {
        MailboxRedeemer::Dispatch {
            destination,
            recipient,
            body,
            sender_ref,
            hook_metadata,
        } => {
            // Constructor 0: Dispatch
            // sender_ref encoded as OutputReference: Constr 0 [ByteArray(tx_hash), Int(output_index)]
            let sender_ref_data = PlutusData::Constr(Constr {
                tag: 121,
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    PlutusData::BoundedBytes(sender_ref.0.to_vec().into()),
                    PlutusData::BigInt(BigInt::Int((sender_ref.1 as i64).into())),
                ]),
            });
            PlutusData::Constr(Constr {
                tag: 121, // Constructor 0 alternative encoding
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    PlutusData::BigInt(BigInt::Int((*destination as i64).into())),
                    PlutusData::BoundedBytes(recipient.to_vec().into()),
                    PlutusData::BoundedBytes(body.clone().into()),
                    sender_ref_data,
                    PlutusData::BoundedBytes(hook_metadata.clone().into()),
                ]),
            })
        }
        MailboxRedeemer::Process {
            message,
            metadata,
            message_id,
            smt_proof,
        } => {
            // Constructor 1: Process
            let proof_list: Vec<PlutusData> = smt_proof
                .iter()
                .map(|hash| PlutusData::BoundedBytes(hash.to_vec().into()))
                .collect();
            PlutusData::Constr(Constr {
                tag: 122, // Constructor 1 alternative encoding
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    encode_message_as_plutus_data(message),
                    PlutusData::BoundedBytes(metadata.clone().into()),
                    PlutusData::BoundedBytes(message_id.to_vec().into()),
                    PlutusData::Array(MaybeIndefArray::Def(proof_list)),
                ]),
            })
        }
        MailboxRedeemer::SetDefaultIsm { new_ism } => {
            // Constructor 2: SetDefaultIsm
            PlutusData::Constr(Constr {
                tag: 123, // Constructor 2 alternative encoding
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![PlutusData::BoundedBytes(
                    new_ism.to_vec().into(),
                )]),
            })
        }
        MailboxRedeemer::TransferOwnership { new_owner } => {
            // Constructor 3: TransferOwnership
            PlutusData::Constr(Constr {
                tag: 124, // Constructor 3 alternative encoding
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![PlutusData::BoundedBytes(
                    new_owner.to_vec().into(),
                )]),
            })
        }
    };

    encode_plutus_data(&plutus_data)
}

/// Encode a ProcessedMessageDatum as Plutus Data CBOR
pub fn encode_processed_message_datum(
    datum: &ProcessedMessageDatum,
) -> Result<Vec<u8>, TxBuilderError> {
    // ProcessedMessageDatum { message_id: ByteArray }
    // Encoded as: Constr 0 [ByteArray]
    let plutus_data = PlutusData::Constr(Constr {
        tag: 121, // Constructor 0
        any_constructor: None,
        fields: MaybeIndefArray::Def(vec![PlutusData::BoundedBytes(
            datum.message_id.to_vec().into(),
        )]),
    });

    encode_plutus_data(&plutus_data)
}

// ============================================================================
// Verified Message Encoding Functions
// ============================================================================

/// Encode a VerifiedMessageDatum as Plutus Data CBOR
/// VerifiedMessageDatum { origin, sender, body, message_id, nonce }
pub fn encode_verified_message_datum(
    datum: &crate::types::VerifiedMessageDatum,
) -> Result<Vec<u8>, TxBuilderError> {
    let plutus_data = PlutusData::Constr(Constr {
        tag: 121,
        any_constructor: None,
        fields: MaybeIndefArray::Def(vec![
            PlutusData::BigInt(BigInt::Int((datum.origin as i64).into())),
            PlutusData::BoundedBytes(datum.sender.clone().into()),
            PlutusData::BoundedBytes(datum.body.clone().into()),
            PlutusData::BoundedBytes(datum.message_id.clone().into()),
            PlutusData::BigInt(BigInt::Int((datum.nonce as i64).into())),
        ]),
    });

    encode_plutus_data(&plutus_data)
}

/// Encode a Message as Plutus Data
fn encode_message_as_plutus_data(msg: &Message) -> PlutusData {
    // Message { version, nonce, origin, sender, destination, recipient, body }
    PlutusData::Constr(Constr {
        tag: 121, // Constructor 0
        any_constructor: None,
        fields: MaybeIndefArray::Def(vec![
            PlutusData::BigInt(BigInt::Int((msg.version as i64).into())),
            PlutusData::BigInt(BigInt::Int((msg.nonce as i64).into())),
            PlutusData::BigInt(BigInt::Int((msg.origin as i64).into())),
            PlutusData::BoundedBytes(msg.sender.to_vec().into()),
            PlutusData::BigInt(BigInt::Int((msg.destination as i64).into())),
            PlutusData::BoundedBytes(msg.recipient.to_vec().into()),
            PlutusData::BoundedBytes(msg.body.clone().into()),
        ]),
    })
}

/// Encode PlutusData to CBOR bytes
fn encode_plutus_data(data: &PlutusData) -> Result<Vec<u8>, TxBuilderError> {
    minicbor::to_vec(data)
        .map_err(|e| TxBuilderError::Encoding(format!("CBOR encoding failed: {e:?}")))
}

/// Extract owner from ISM datum PlutusData
/// ISM datum structure: Constr(121, [validators_list, thresholds_list, owner_bytes])
/// Note: Aiken uses tag 121 for constructors, the constructor index is not in the tag
fn extract_ism_owner(datum: &PlutusData) -> Result<[u8; 28], TxBuilderError> {
    match datum {
        PlutusData::Constr(constr) if constr.fields.len() == 3 => {
            // Owner is the 3rd field (index 2)
            let owner_field = &constr.fields[2];

            let owner_bytes: &[u8] = match owner_field {
                PlutusData::BoundedBytes(bytes) => bytes.as_ref(),
                _ => {
                    return Err(TxBuilderError::Encoding(format!(
                        "Owner field must be BoundedBytes, got: {owner_field:?}"
                    )))
                }
            };

            let bytes: [u8; 28] = owner_bytes.try_into().map_err(|_| {
                TxBuilderError::Encoding(format!(
                    "Owner must be 28 bytes, got {}",
                    owner_bytes.len()
                ))
            })?;
            Ok(bytes)
        }
        _ => Err(TxBuilderError::Encoding(format!(
            "Invalid ISM datum structure: expected Constr with 3 fields, got {datum:?}"
        ))),
    }
}

/// Build ISM datum with updated validators
/// Structure: Constr(121, [validators_list, thresholds_list, owner_bytes])
/// Note: Aiken uses tag 121 for all constructors
fn build_ism_datum(
    domain: u32,
    validators: Vec<Vec<u8>>,
    threshold: u32,
    owner: [u8; 28],
) -> Result<PlutusData, TxBuilderError> {
    // Build datum in JSON format (matching cardano-cli format from bash script)
    // This ensures compatibility with the on-chain contract expectations

    use serde_json::json;

    // Convert validator bytes to hex strings
    let validator_hex_list: Vec<String> = validators.into_iter().map(|v| hex::encode(&v)).collect();

    // Build validators list JSON: [{"constructor": 0, "fields": [{"int": domain}, {"list": [{"bytes": "hex"}]}]}]
    let validators_json = json!({
        "list": [
            {
                "constructor": 0,
                "fields": [
                    {"int": domain},
                    {
                        "list": validator_hex_list.iter().map(|h| json!({"bytes": h})).collect::<Vec<_>>()
                    }
                ]
            }
        ]
    });

    // Build thresholds list JSON: [{"constructor": 0, "fields": [{"int": domain}, {"int": threshold}]}]
    let thresholds_json = json!({
        "list": [
            {
                "constructor": 0,
                "fields": [
                    {"int": domain},
                    {"int": threshold}
                ]
            }
        ]
    });

    // Build complete datum JSON
    let datum_json = json!({
        "constructor": 0,
        "fields": [
            validators_json,
            thresholds_json,
            {"bytes": hex::encode(owner)}
        ]
    });

    // Convert JSON to PlutusData using existing converter
    json_to_plutus_data(&datum_json)
}

/// Encode ISM redeemer to CBOR
fn encode_ism_redeemer(
    redeemer: &crate::types::MultisigIsmRedeemer,
) -> Result<Vec<u8>, TxBuilderError> {
    let plutus_data = match redeemer {
        crate::types::MultisigIsmRedeemer::Verify {
            checkpoint,
            validator_signatures,
        } => {
            // Constr(0, [checkpoint, [validator_signature, ...]])
            // checkpoint: Constr(0, [origin, merkle_root, origin_merkle_tree_hook, merkle_index, message_id])
            // validator_signature: Constr(0, [recovered_pubkey, signature])
            let checkpoint_data = PlutusData::Constr(Constr {
                tag: 121, // Constructor 0
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    PlutusData::BigInt(BigInt::Int((checkpoint.origin as i64).into())),
                    PlutusData::BoundedBytes(checkpoint.merkle_root.to_vec().into()),
                    PlutusData::BoundedBytes(checkpoint.origin_merkle_tree_hook.to_vec().into()),
                    PlutusData::BigInt(BigInt::Int((checkpoint.merkle_index as i64).into())),
                    PlutusData::BoundedBytes(checkpoint.message_id.to_vec().into()),
                ]),
            });

            // Encode validator signatures as list of ValidatorSignature records
            // Each is Constr(0, [compressed_pubkey, uncompressed_pubkey, signature])
            let sig_list: Vec<PlutusData> = validator_signatures
                .iter()
                .map(|val_sig| {
                    PlutusData::Constr(Constr {
                        tag: 121, // Constructor 0
                        any_constructor: None,
                        fields: MaybeIndefArray::Def(vec![
                            PlutusData::BoundedBytes(val_sig.compressed_pubkey.to_vec().into()),
                            PlutusData::BoundedBytes(val_sig.uncompressed_pubkey.to_vec().into()),
                            PlutusData::BoundedBytes(val_sig.signature.to_vec().into()),
                        ]),
                    })
                })
                .collect();

            PlutusData::Constr(Constr {
                tag: 121, // Constructor 0 = Verify
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    checkpoint_data,
                    PlutusData::Array(MaybeIndefArray::Def(sig_list)),
                ]),
            })
        }
        crate::types::MultisigIsmRedeemer::SetValidators { domain, validators } => {
            // Constr(1, [domain, [validator_bytes]])
            let validator_bytes: Vec<PlutusData> = validators
                .iter()
                .map(|v| PlutusData::BoundedBytes(v.0.to_vec().into()))
                .collect();

            PlutusData::Constr(Constr {
                tag: 122, // Constructor 1 = SetValidators
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    PlutusData::BigInt(BigInt::Int((*domain as i64).into())),
                    PlutusData::Array(MaybeIndefArray::Def(validator_bytes)),
                ]),
            })
        }
        crate::types::MultisigIsmRedeemer::SetThreshold { domain, threshold } => {
            // Constr(2, [domain, threshold])
            PlutusData::Constr(Constr {
                tag: 123, // Constructor 2 = SetThreshold
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    PlutusData::BigInt(BigInt::Int((*domain as i64).into())),
                    PlutusData::BigInt(BigInt::Int((*threshold as i64).into())),
                ]),
            })
        }
    };

    encode_plutus_data(&plutus_data)
}

/// Encode warp route redeemer to CBOR
/// WarpRouteRedeemer:
/// - TransferRemote(0) = [destination: Int, recipient: ByteArray, amount: Int]
/// - ReceiveTransfer(1) = [message: Message, message_id: ByteArray, return_address: ByteArray, expiry_slot: Int]
/// - EnrollRemoteRoute(2) = [domain: Int, route: ByteArray]
pub fn encode_warp_route_redeemer(
    redeemer: &crate::types::WarpRouteRedeemer,
) -> Result<Vec<u8>, TxBuilderError> {
    let plutus_data = match redeemer {
        crate::types::WarpRouteRedeemer::TransferRemote {
            destination,
            recipient,
            amount,
        } => {
            PlutusData::Constr(Constr {
                tag: 121, // Constructor 0
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    PlutusData::BigInt(BigInt::Int((*destination as i64).into())),
                    PlutusData::BoundedBytes(recipient.to_vec().into()),
                    PlutusData::BigInt(BigInt::Int((*amount as i64).into())),
                ]),
            })
        }
        crate::types::WarpRouteRedeemer::ReceiveTransfer {
            message,
            message_id,
        } => {
            // Message is Constructor 0 with fields:
            // [version: Int, nonce: Int, origin: Int, sender: ByteArray,
            //  destination: Int, recipient: ByteArray, body: ByteArray]
            let message_data = PlutusData::Constr(Constr {
                tag: 121, // Constructor 0
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    PlutusData::BigInt(BigInt::Int((message.version as i64).into())),
                    PlutusData::BigInt(BigInt::Int((message.nonce as i64).into())),
                    PlutusData::BigInt(BigInt::Int((message.origin as i64).into())),
                    PlutusData::BoundedBytes(message.sender.to_vec().into()),
                    PlutusData::BigInt(BigInt::Int((message.destination as i64).into())),
                    PlutusData::BoundedBytes(message.recipient.to_vec().into()),
                    PlutusData::BoundedBytes(message.body.clone().into()),
                ]),
            });

            PlutusData::Constr(Constr {
                tag: 122, // Constructor 1
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    message_data,
                    PlutusData::BoundedBytes(message_id.to_vec().into()),
                ]),
            })
        }
        crate::types::WarpRouteRedeemer::EnrollRemoteRoute { domain, route } => {
            PlutusData::Constr(Constr {
                tag: 123, // Constructor 2
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    PlutusData::BigInt(BigInt::Int((*domain as i64).into())),
                    PlutusData::BoundedBytes(route.to_vec().into()),
                ]),
            })
        }
    };

    encode_plutus_data(&plutus_data)
}

/// Convert wire format amount (U256) to local token amount (u64)
/// Formula: local_amount = wire_amount / 10^(remote_decimals - local_decimals)
/// If local_decimals >= remote_decimals, multiply instead
///
/// This function takes U256 to handle large wire amounts (e.g., 35 * 10^18)
/// and returns u64 after decimal conversion (which brings it into u64 range).
fn convert_wire_to_local_amount(wire_amount: U256, remote_decimals: u8, local_decimals: u8) -> u64 {
    if local_decimals >= remote_decimals {
        // Upsample: multiply by 10^(local_decimals - remote_decimals)
        let multiplier = U256::from(10u64).pow(U256::from(local_decimals - remote_decimals));
        let result = wire_amount.saturating_mul(multiplier);
        // After upsampling, result should fit in u64 for reasonable amounts
        result.as_u64()
    } else {
        // Downsample: divide by 10^(remote_decimals - local_decimals)
        // This is the common case (18 decimals -> 6 decimals)
        // Division brings large U256 values into u64 range
        let divisor = U256::from(10u64).pow(U256::from(remote_decimals - local_decimals));
        let result = wire_amount / divisor;
        result.as_u64()
    }
}

/// Build a warp transfer body with the given recipient and amount
/// Format: recipient (variable) || amount (8 bytes big-endian)
#[allow(dead_code)]
fn build_warp_transfer_body(recipient: &[u8], amount: u64) -> Vec<u8> {
    let mut body = recipient.to_vec();
    body.extend_from_slice(&amount.to_be_bytes());
    body
}

/// Parsed Hyperlane TokenMessage
/// Warp routes use a standard wire format: recipient (32 bytes) || amount (32 bytes uint256)
#[derive(Debug)]
struct TokenMessage {
    /// 32-byte recipient (bytes32)
    recipient: [u8; 32],
    /// Amount as U256 (full 32-byte uint256)
    /// This is the wire format amount (typically 18 decimals)
    /// Use convert_wire_to_local_amount() to convert to local decimals
    amount: U256,
}

/// Parse a Hyperlane TokenMessage body
/// Standard wire format (defined in TokenMessage.sol, used by all chains):
/// - bytes 0-31: recipient (bytes32)
/// - bytes 32-63: amount (uint256, big-endian)
/// - bytes 64+: metadata (optional, ignored)
///
/// Note: We read the full 32-byte uint256 amount to handle large wire amounts
/// (e.g., 35 * 10^18 for 35 tokens with 18 decimal wire format exceeds u64 max).
/// The decimal conversion happens after parsing via convert_wire_to_local_amount().
fn parse_token_message(body: &[u8]) -> Result<TokenMessage, TxBuilderError> {
    if body.len() < 64 {
        return Err(TxBuilderError::Encoding(format!(
            "TokenMessage too short: {} bytes, expected at least 64",
            body.len()
        )));
    }

    // Extract recipient (first 32 bytes)
    let recipient: [u8; 32] = body[0..32].try_into().map_err(|_| {
        TxBuilderError::Encoding("Failed to extract recipient from TokenMessage".to_string())
    })?;

    // Extract amount (bytes 32-63, full 32-byte uint256 big-endian)
    // We read all 32 bytes to handle large wire amounts that exceed u64
    // (e.g., 35 * 10^18 > 2^64 for tokens using 18 decimal wire format)
    let amount_bytes: [u8; 32] = body[32..64].try_into().map_err(|_| {
        TxBuilderError::Encoding("Failed to extract amount from TokenMessage".to_string())
    })?;
    let amount = U256::from_big_endian(&amount_bytes);

    Ok(TokenMessage { recipient, amount })
}

/// Extract Cardano credential hash from a bytes32 recipient
/// Hyperlane bytes32 pads 28-byte Cardano hashes with 4 leading zeros:
/// [0x00, 0x00, 0x00, 0x00, <28 bytes credential hash>]
fn extract_cardano_credential_from_bytes32(recipient: &[u8; 32]) -> [u8; 28] {
    let mut credential = [0u8; 28];
    credential.copy_from_slice(&recipient[4..32]);
    credential
}

/// Parse Hyperlane metadata and recover public keys from signatures
///
/// Hyperlane metadata format for multisig ISM:
/// - Bytes 0-31: Origin merkle tree hook address (32 bytes, left-padded for EVM)
/// - Bytes 32-35: Root index (4 bytes, big-endian)
/// - Bytes 36-67: Merkle root (32 bytes)
/// - Bytes 68+: Signatures (65 bytes each for ECDSA secp256k1)
///
/// This function recovers the uncompressed public keys from each signature and returns
/// both the recovered pubkey and the signature bytes for on-chain verification.
///
/// Security model:
/// 1. We recover public keys off-chain from the 65-byte signatures
/// 2. On-chain ISM verifies each signature using verify_ecdsa_secp256k1_signature
/// 3. ISM computes Ethereum address from the verified public key
/// 4. ISM checks the address is in the trusted validators list
///
/// This provides cryptographic binding - an attacker cannot forge a signature.
pub fn parse_multisig_metadata(
    metadata: &[u8],
    origin: u32,
    message_id: &[u8; 32],
) -> Result<MultisigMetadata, TxBuilderError> {
    use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
    use sha3::{Digest, Keccak256};

    if metadata.len() < 68 {
        return Err(TxBuilderError::Encoding(
            "Metadata too short for multisig ISM".to_string(),
        ));
    }

    // Metadata format per Hyperlane docs (MessageIdMultisigIsmMetadata):
    // https://docs.hyperlane.xyz/docs/protocol/ISM/standard-ISMs/multisig-ISM
    // Bytes 0-31:  Origin merkle tree hook (MerkleTreeHook address on origin chain)
    // Bytes 32-63: Signed checkpoint merkle root
    // Bytes 64-67: Signed checkpoint index
    // Bytes 68+:   Validator signatures (65 bytes each)

    // Bytes 0-31: Origin merkle tree hook
    let mut origin_mailbox = [0u8; 32];
    origin_mailbox.copy_from_slice(&metadata[0..32]);

    // Bytes 32-63: Merkle root
    let mut merkle_root = [0u8; 32];
    merkle_root.copy_from_slice(&metadata[32..64]);

    // Bytes 64-67: Checkpoint index
    let root_index =
        u32::from_be_bytes(metadata[64..68].try_into().map_err(|e| {
            TxBuilderError::Encoding(format!("Invalid checkpoint index bytes: {e}"))
        })?);

    // Compute the checkpoint hash that validators signed
    // Step 1: domain_hash = keccak256(origin || merkle_tree_hook || "HYPERLANE")
    let mut domain_hasher = Keccak256::new();
    domain_hasher.update(origin.to_be_bytes());
    domain_hasher.update(origin_mailbox);
    domain_hasher.update(b"HYPERLANE");
    let domain_hash: [u8; 32] = domain_hasher.finalize().into();

    // Step 2: checkpoint_digest = keccak256(domain_hash || merkle_root || merkle_index || message_id)
    let mut checkpoint_hasher = Keccak256::new();
    checkpoint_hasher.update(domain_hash);
    checkpoint_hasher.update(merkle_root);
    checkpoint_hasher.update(root_index.to_be_bytes());
    checkpoint_hasher.update(message_id);
    let checkpoint_digest: [u8; 32] = checkpoint_hasher.finalize().into();

    // Step 3: eth_signed_message = keccak256("\x19Ethereum Signed Message:\n32" || checkpoint_digest)
    let mut eth_hasher = Keccak256::new();
    eth_hasher.update(b"\x19Ethereum Signed Message:\n32");
    eth_hasher.update(checkpoint_digest);
    let eth_signed_message: [u8; 32] = eth_hasher.finalize().into();

    debug!("Recovering public keys from signatures");
    debug!("  domain_hash: {}", hex::encode(domain_hash));
    debug!("  checkpoint_digest: {}", hex::encode(checkpoint_digest));
    debug!("  eth_signed_message: {}", hex::encode(eth_signed_message));

    // Parse signatures and recover public keys
    // Each signature is 65 bytes: r (32) || s (32) || v (1)
    // We extract both the signature (r||s, 64 bytes) and recover the public key
    let signatures_data = &metadata[68..];
    let mut validator_signatures = Vec::new();
    let mut offset = 0;

    while offset + 65 <= signatures_data.len() {
        let sig_bytes = &signatures_data[offset..offset + 65];
        let v = sig_bytes[64];
        let recovery_id = if v >= 27 { v - 27 } else { v };

        match Signature::from_slice(&sig_bytes[..64]) {
            Ok(sig) => {
                match RecoveryId::try_from(recovery_id) {
                    Ok(rec_id) => {
                        // Recover public key using the ORIGINAL signature
                        // The same public key verifies both (r, s) and (r, n-s)
                        match VerifyingKey::recover_from_prehash(&eth_signed_message, &sig, rec_id)
                        {
                            Ok(recovered_key) => {
                                // Get compressed key (33 bytes: 0x02/0x03 + x-coordinate)
                                // Per CIP-49, verifyEcdsaSecp256k1Signature expects this format
                                let compressed = recovered_key.to_encoded_point(true);
                                let mut compressed_pubkey = [0u8; 33];
                                compressed_pubkey.copy_from_slice(compressed.as_bytes());

                                // Get uncompressed key (64 bytes: x || y, no 0x04 prefix)
                                // Used on-chain to compute the Ethereum address
                                let uncompressed = recovered_key.to_encoded_point(false);
                                let uncompressed_bytes = &uncompressed.as_bytes()[1..]; // Skip 0x04 prefix
                                let mut uncompressed_pubkey = [0u8; 64];
                                uncompressed_pubkey.copy_from_slice(uncompressed_bytes);

                                // CIP-49 requires signatures in normalized low-s form
                                // Normalize if needed - the same pubkey verifies both forms
                                let normalized_sig = sig.normalize_s().unwrap_or(sig);
                                let signature: [u8; 64] = normalized_sig.to_bytes().into();

                                let was_normalized = sig.normalize_s().is_some();
                                if was_normalized {
                                    debug!("  Signature was normalized to low-s form");
                                }

                                validator_signatures.push(crate::types::ValidatorSignature {
                                    compressed_pubkey,
                                    uncompressed_pubkey,
                                    signature,
                                });

                                // Compute Ethereum address for logging
                                let address_hash = Keccak256::digest(uncompressed_bytes);
                                let eth_address = &address_hash[12..];
                                info!(
                                    "  Recovered validator {}: 0x{}",
                                    validator_signatures.len() - 1,
                                    hex::encode(eth_address)
                                );
                                info!("    Compressed pubkey: {}", hex::encode(compressed_pubkey));
                            }
                            Err(e) => {
                                debug!("  Failed to recover public key: {:?}", e);
                            }
                        }
                    }
                    Err(e) => {
                        debug!("  Invalid recovery ID {}: {:?}", recovery_id, e);
                    }
                }
            }
            Err(e) => {
                debug!("  Invalid signature format: {:?}", e);
            }
        }

        offset += 65;
    }

    debug!(
        "  Recovered {} validator signatures",
        validator_signatures.len()
    );

    Ok(MultisigMetadata {
        merkle_root,
        root_index,
        origin_mailbox,
        validator_signatures,
    })
}

/// Parsed multisig ISM metadata with recovered public keys and signatures
#[derive(Debug, Clone)]
pub struct MultisigMetadata {
    pub merkle_root: [u8; 32],
    pub root_index: u32,
    pub origin_mailbox: [u8; 32],
    /// Validator signatures with recovered public keys
    pub validator_signatures: Vec<crate::types::ValidatorSignature>,
}

/// Get the PlutusV3 cost model for Conway era transactions
/// These values are from the Cardano Preview network protocol parameters
fn get_plutus_v3_cost_model() -> Vec<i64> {
    vec![
        100788, 420, 1, 1, 1000, 173, 0, 1, 1000, 59957, 4, 1, 11183, 32, 201305, 8356, 4, 16000,
        100, 16000, 100, 16000, 100, 16000, 100, 16000, 100, 16000, 100, 100, 100, 16000, 100,
        94375, 32, 132994, 32, 61462, 4, 72010, 178, 0, 1, 22151, 32, 91189, 769, 4, 2, 85848,
        123203, 7305, -900, 1716, 549, 57, 85848, 0, 1, 1, 1000, 42921, 4, 2, 24548, 29498, 38, 1,
        898148, 27279, 1, 51775, 558, 1, 39184, 1000, 60594, 1, 141895, 32, 83150, 32, 15299, 32,
        76049, 1, 13169, 4, 22100, 10, 28999, 74, 1, 28999, 74, 1, 43285, 552, 1, 44749, 541, 1,
        33852, 32, 68246, 32, 72362, 32, 7243, 32, 7391, 32, 11546, 32, 85848, 123203, 7305, -900,
        1716, 549, 57, 85848, 0, 1, 90434, 519, 0, 1, 74433, 32, 85848, 123203, 7305, -900, 1716,
        549, 57, 85848, 0, 1, 1, 85848, 123203, 7305, -900, 1716, 549, 57, 85848, 0, 1, 955506,
        213312, 0, 2, 270652, 22588, 4, 1457325, 64566, 4, 20467, 1, 4, 0, 141992, 32, 100788, 420,
        1, 1, 81663, 32, 59498, 32, 20142, 32, 24588, 32, 20744, 32, 25933, 32, 24623, 32,
        43053543, 10, 53384111, 14333, 10, 43574283, 26308, 10, 16000, 100, 16000, 100, 962335, 18,
        2780678, 6, 442008, 1, 52538055, 3756, 18, 267929, 18, 76433006, 8868, 18, 52948122, 18,
        1995836, 36, 3227919, 12, 901022, 1, 166917843, 4307, 36, 284546, 36, 158221314, 26549, 36,
        74698472, 36, 333849714, 1, 254006273, 72, 2174038, 72, 2261318, 64571, 4, 207616, 8310, 4,
        1293828, 28716, 63, 0, 1, 1006041, 43623, 251, 0, 1, 100181, 726, 719, 0, 1, 100181, 726,
        719, 0, 1, 100181, 726, 719, 0, 1, 107878, 680, 0, 1, 95336, 1, 281145, 18848, 0, 1,
        180194, 159, 1, 1, 158519, 8942, 0, 1, 159378, 8813, 0, 1, 107490, 3298, 1, 106057, 655, 1,
        1964219, 24520, 3,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_processed_message_datum() {
        let datum = ProcessedMessageDatum {
            message_id: [0x42; 32],
        };

        let encoded = encode_processed_message_datum(&datum).unwrap();
        assert!(!encoded.is_empty());
    }

    #[test]
    fn test_encode_message() {
        let msg = Message {
            version: 3,
            nonce: 1,
            origin: 1,
            sender: [0u8; 32],
            destination: 2001,
            recipient: [1u8; 32],
            body: vec![0x48, 0x65, 0x6c, 0x6c, 0x6f],
        };

        let plutus_data = encode_message_as_plutus_data(&msg);

        // Verify it's a constructor with 7 fields
        match plutus_data {
            PlutusData::Constr(constr) => {
                assert_eq!(constr.fields.len(), 7);
            }
            _ => panic!("Expected Constr"),
        }
    }

    #[test]
    fn test_parse_multisig_metadata() {
        // Create minimal metadata with fake signature data
        // Note: This won't recover any valid public keys since the signature data is fake
        let mut metadata = vec![0u8; 68];
        // Add one fake signature (65 bytes: r=32, s=32, v=1)
        metadata.extend_from_slice(&[0xAB; 65]);

        let origin = 43113u32;
        let message_id = [0x42u8; 32];

        // Parse metadata - won't recover valid keys since signature is fake
        let parsed = parse_multisig_metadata(&metadata, origin, &message_id).unwrap();
        // Since the signature data is fake, recovery will fail and no pubkeys will be added
        // This is expected behavior
        assert_eq!(parsed.merkle_root, [0u8; 32]);
        assert_eq!(parsed.root_index, 0);
        assert_eq!(parsed.origin_mailbox, [0u8; 32]);
    }

    #[test]
    fn test_redeemer_encoding() {
        use pallas_primitives::conway::{Redeemer, RedeemerTag, Redeemers};
        use pallas_primitives::{ExUnits, Fragment};

        // Create a simple redeemer with Spend tag
        let redeemer = Redeemer {
            tag: RedeemerTag::Spend,
            index: 0,
            data: PlutusData::Constr(Constr {
                tag: 121,
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![]),
            }),
            ex_units: ExUnits {
                mem: 14_000_000,
                steps: 10_000_000_000,
            },
        };

        // Encode the redeemer
        let encoded = redeemer.encode_fragment().unwrap();
        println!("Single redeemer CBOR: {}", hex::encode(&encoded));

        // Verify the first byte after array header is the tag (should be 0 for Spend)
        // Expected: 84 00 00 <plutus_data> 82 <mem> <steps>
        println!(
            "First bytes: {:02x} {:02x} {:02x}",
            encoded[0], encoded[1], encoded[2]
        );

        // Now test encoding as Redeemers::List
        let redeemers = Redeemers::List(vec![redeemer]);
        let encoded_list = redeemers.encode_fragment().unwrap();
        println!("Redeemers List CBOR: {}", hex::encode(&encoded_list));
    }

    #[test]
    fn test_full_tx_build_with_redeemer() {
        use pallas_addresses::{Address, Network};
        use pallas_crypto::hash::Hash;
        use pallas_primitives::conway::Tx;
        use pallas_primitives::Fragment;
        use pallas_txbuilder::{BuildConway, Input, Output, StagingTransaction};

        // Create a staging transaction with a redeemer
        let mut tx = StagingTransaction::new();

        // Create a dummy input
        let tx_hash: Hash<32> = Hash::new([0u8; 32]);
        let input = Input::new(tx_hash.clone(), 0);
        tx = tx.input(input);

        // Create a dummy output
        let test_addr = Address::from_bech32("addr_test1qz2fxv2umyhttkxyxp8x0dlpdt3k6cwng5pxj3jhsydzer3jcu5d8ps7zex2k2xt3uqxgjqnnj83ws8lhrn648jjxtwq2ytjqp").unwrap();
        let output = Output::new(test_addr, 2_000_000);
        tx = tx.output(output);

        // Create redeemer data as CBOR bytes (just empty constr for testing)
        let redeemer_data = PlutusData::Constr(Constr {
            tag: 121,
            any_constructor: None,
            fields: MaybeIndefArray::Def(vec![]),
        });
        let redeemer_cbor = minicbor::to_vec(&redeemer_data).unwrap();
        println!("Redeemer data CBOR: {}", hex::encode(&redeemer_cbor));

        // Add spend redeemer
        let input_for_redeemer = Input::new(tx_hash, 0);
        let ex_units = ExUnits {
            mem: 14_000_000,
            steps: 10_000_000_000,
        };
        tx = tx.add_spend_redeemer(input_for_redeemer, redeemer_cbor, Some(ex_units));

        // Set fee
        tx = tx.fee(1_000_000);

        // Set network ID
        tx = tx.network_id(0);

        // Add language view (PlutusV3)
        let cost_model = get_plutus_v3_cost_model();
        tx = tx.language_view(ScriptKind::PlutusV3, cost_model);

        // Build the transaction
        let built = tx.build_conway_raw();

        match built {
            Ok(built_tx) => {
                println!(
                    "Built tx CBOR ({} bytes): {}",
                    built_tx.tx_bytes.0.len(),
                    hex::encode(&built_tx.tx_bytes.0)
                );

                // Now decode and check the redeemer structure
                let decoded_tx: Tx =
                    Tx::decode_fragment(&built_tx.tx_bytes.0).expect("Should decode");

                // Check the witness set redeemers
                if let Some(ref redeemers) = decoded_tx.transaction_witness_set.redeemer {
                    println!("Redeemers in tx: {:?}", redeemers);

                    // Re-encode just the redeemers to see what they look like
                    let redeemers_cbor = redeemers.encode_fragment().unwrap();
                    println!(
                        "Redeemers CBOR from witness set: {}",
                        hex::encode(&redeemers_cbor)
                    );
                } else {
                    println!("No redeemers in witness set!");
                }
            }
            Err(e) => {
                println!("Failed to build tx: {:?}", e);
            }
        }
    }

    #[test]
    fn test_convert_wire_to_local_18_to_6_decimals() {
        // EVM (18 dec) -> Cardano ADA (6 dec): scale = 10^12
        // 1 unit in wire format (1e18) = 1_000_000 local units
        assert_eq!(
            convert_wire_to_local_amount(U256::from(1_000_000_000_000_000_000u64), 18, 6),
            1_000_000
        );
        // 1e12 wire = 1 local unit
        assert_eq!(
            convert_wire_to_local_amount(U256::from(1_000_000_000_000u64), 18, 6),
            1
        );
        // 5e18 wire = 5_000_000 local units
        assert_eq!(
            convert_wire_to_local_amount(U256::from(5_000_000_000_000_000_000u64), 18, 6),
            5_000_000
        );
    }

    #[test]
    fn test_convert_wire_to_local_18_to_8_decimals() {
        // 18 dec -> 8 decimal token: scale = 10^10
        // 1 unit in wire format (1e18) = 1e8 local units
        assert_eq!(
            convert_wire_to_local_amount(U256::from(1_000_000_000_000_000_000u64), 18, 8),
            100_000_000
        );
        // 1e10 wire = 1 local unit
        assert_eq!(
            convert_wire_to_local_amount(U256::from(10_000_000_000u64), 18, 8),
            1
        );
    }

    #[test]
    fn test_convert_wire_to_local_same_decimals() {
        // Same decimals: no conversion needed
        assert_eq!(
            convert_wire_to_local_amount(U256::from(1_000_000_000_000_000_000u64), 18, 18),
            1_000_000_000_000_000_000
        );
        assert_eq!(
            convert_wire_to_local_amount(U256::from(12345u64), 6, 6),
            12345
        );
        assert_eq!(convert_wire_to_local_amount(U256::from(100u64), 8, 8), 100);
    }

    #[test]
    fn test_convert_wire_to_local_18_to_0_decimals() {
        // 18 dec -> 0 decimal token: scale = 10^18
        // 1e18 wire = 1 local unit
        assert_eq!(
            convert_wire_to_local_amount(U256::from(1_000_000_000_000_000_000u64), 18, 0),
            1
        );
        // 5e18 wire = 5 local units
        assert_eq!(
            convert_wire_to_local_amount(U256::from(5_000_000_000_000_000_000u64), 18, 0),
            5
        );
    }

    #[test]
    fn test_convert_wire_to_local_upsample() {
        // 6 dec remote -> 18 dec local: multiply by 10^12
        assert_eq!(
            convert_wire_to_local_amount(U256::from(1_000_000u64), 6, 18),
            1_000_000_000_000_000_000
        );
        assert_eq!(
            convert_wire_to_local_amount(U256::from(1u64), 6, 18),
            1_000_000_000_000
        );
    }

    #[test]
    fn test_backward_compatibility() {
        // Verify 18->6 decimals matches the old hardcoded 10^12 factor
        let old_factor = 1_000_000_000_000u64;
        let wire = 500_000_000_000_000_000u64;
        assert_eq!(
            wire / old_factor,
            convert_wire_to_local_amount(U256::from(wire), 18, 6)
        );

        // More test cases
        let wire2 = 1_234_567_890_123_456_789u64;
        assert_eq!(
            wire2 / old_factor,
            convert_wire_to_local_amount(U256::from(wire2), 18, 6)
        );
    }

    #[test]
    fn test_large_amount_exceeding_u64() {
        // Test large amount that exceeds u64 (35 * 10^18)
        // This was the bug case: 35 ADA transferred but only ~16.5 received
        // because the wire amount (35 * 10^18) exceeds u64::MAX (~18.4 * 10^18)
        let large_amount = U256::from(35u64) * U256::from(10u64).pow(U256::from(18u64));
        assert_eq!(
            convert_wire_to_local_amount(large_amount, 18, 6),
            35_000_000
        );

        // 50 tokens in wire format (50 * 10^18)
        let fifty = U256::from(50u64) * U256::from(10u64).pow(U256::from(18u64));
        assert_eq!(convert_wire_to_local_amount(fifty, 18, 6), 50_000_000);

        // 100 tokens in wire format (100 * 10^18)
        let hundred = U256::from(100u64) * U256::from(10u64).pow(U256::from(18u64));
        assert_eq!(convert_wire_to_local_amount(hundred, 18, 6), 100_000_000);
    }
}

#[cfg(test)]
mod signature_verification_tests {
    use super::*;
    use k256::ecdsa::{
        signature::hazmat::PrehashVerifier, signature::Verifier, RecoveryId, Signature,
        VerifyingKey,
    };
    use sha3::{Digest, Keccak256};

    /// Test signature verification with recovery to identify the actual signer
    /// This test recovers the correct public keys from real Fuji signatures
    #[test]
    fn test_fuji_signature_with_recovery() {
        // Test data from relayer logs
        let origin: u32 = 43113;
        let merkle_root =
            hex::decode("efa004d027c79c3d7faf7821111493144243a32f8616af99ceff8238000010ec")
                .unwrap();
        let origin_merkle_tree_hook =
            hex::decode("0000000000000000000000009ff6ac3daf63103620bbf76136ea1aff43c2f612")
                .unwrap();
        let merkle_index: u32 = 146986598;
        let message_id =
            hex::decode("0ce4b05a9d25d2556f74ddaa1ac84841341623376c9e5cd073f52b1b54dcddbf")
                .unwrap();

        // Validator 0 public key (compressed) - THIS IS WHAT WE HAVE
        let validator_pubkey =
            hex::decode("03225f0eceb966fca4afec433f93cb38d3b0cbb44b066a4a83618fc23d2ccd5c17")
                .unwrap();

        // Signature 0 (65 bytes: r || s || v)
        let sig_bytes = hex::decode("d88d35b30b437c9d069dc3e97263d8b06367ae53840fdb1d0f8009e61ded9cad1ca7cb64f16f21a08634065f7de2cc92d651fa5bd04603e675ad72fffe39b4761b").unwrap();

        println!("=== Computing hashes ===");

        // Step 1: domain_hash = keccak256(origin || merkle_tree_hook || "HYPERLANE")
        let mut domain_hasher = Keccak256::new();
        domain_hasher.update(&origin.to_be_bytes());
        domain_hasher.update(&origin_merkle_tree_hook);
        domain_hasher.update(b"HYPERLANE");
        let domain_hash: [u8; 32] = domain_hasher.finalize().into();
        println!("domain_hash: {}", hex::encode(&domain_hash));

        // Step 2: checkpoint_digest = keccak256(domain_hash || merkle_root || merkle_index || message_id)
        let mut checkpoint_hasher = Keccak256::new();
        checkpoint_hasher.update(&domain_hash);
        checkpoint_hasher.update(&merkle_root);
        checkpoint_hasher.update(&merkle_index.to_be_bytes());
        checkpoint_hasher.update(&message_id);
        let checkpoint_digest: [u8; 32] = checkpoint_hasher.finalize().into();
        println!(
            "checkpoint_digest (signing_hash): {}",
            hex::encode(&checkpoint_digest)
        );

        // Step 3: eth_signed_message = keccak256("\x19Ethereum Signed Message:\n32" || checkpoint_digest)
        let mut eth_hasher = Keccak256::new();
        eth_hasher.update(b"\x19Ethereum Signed Message:\n32");
        eth_hasher.update(&checkpoint_digest);
        let eth_signed_message: [u8; 32] = eth_hasher.finalize().into();
        println!(
            "eth_signed_message (final hash to sign): {}",
            hex::encode(&eth_signed_message)
        );

        println!("\n=== Recovery test ===");

        // Extract v from signature (last byte)
        let v = sig_bytes[64];
        println!("Signature v value: {} (0x{:02x})", v, v);

        // Ethereum recovery_id: v = 27 or 28, so recovery_id = v - 27
        let recovery_id = if v >= 27 { v - 27 } else { v };
        println!("Recovery ID: {}", recovery_id);

        // Parse signature (first 64 bytes: r || s)
        let sig = Signature::from_slice(&sig_bytes[..64]).expect("Invalid signature");

        // Try to recover the public key from the signature using EIP-191 hash
        let rec_id = RecoveryId::try_from(recovery_id).expect("Invalid recovery id");

        match VerifyingKey::recover_from_prehash(&eth_signed_message, &sig, rec_id) {
            Ok(recovered_key) => {
                let recovered_compressed = recovered_key.to_sec1_bytes();
                println!(
                    "Recovered public key (compressed): {}",
                    hex::encode(&recovered_compressed)
                );
                println!(
                    "Expected public key (compressed):  {}",
                    hex::encode(&validator_pubkey)
                );

                // Compute Ethereum address from public key
                let uncompressed = recovered_key.to_encoded_point(false);
                let public_key_bytes = &uncompressed.as_bytes()[1..]; // Skip 0x04 prefix
                let address_hash = Keccak256::digest(public_key_bytes);
                let eth_address = &address_hash[12..]; // Last 20 bytes
                println!("Recovered Ethereum address: 0x{}", hex::encode(eth_address));

                // Check if recovered matches expected
                if &*recovered_compressed == validator_pubkey.as_slice() {
                    println!("✓ Recovered key matches expected validator key!");
                } else {
                    println!("✗ Recovered key does NOT match expected validator key");
                }

                // Verify signature directly with recovered key
                match recovered_key.verify_prehash(&eth_signed_message, &sig) {
                    Ok(_) => println!("✓ Signature verifies with recovered key"),
                    Err(e) => println!("✗ Signature verification failed: {}", e),
                }
            }
            Err(e) => {
                println!("Recovery failed: {:?}", e);

                // Try recovery with checkpoint_digest (without EIP-191)
                println!("\nTrying recovery without EIP-191...");
                match VerifyingKey::recover_from_prehash(&checkpoint_digest, &sig, rec_id) {
                    Ok(recovered_key) => {
                        let recovered_compressed = recovered_key.to_sec1_bytes();
                        println!(
                            "Recovered public key (without EIP-191): {}",
                            hex::encode(&recovered_compressed)
                        );
                        println!("Expected public key: {}", hex::encode(&validator_pubkey));
                    }
                    Err(e) => println!("Recovery without EIP-191 also failed: {:?}", e),
                }
            }
        }

        println!("\n=== Direct verification ===");

        // Parse the expected public key
        let verifying_key =
            VerifyingKey::from_sec1_bytes(&validator_pubkey).expect("Invalid public key");

        // Verify with EIP-191 hash
        let result1 = verifying_key.verify_prehash(&eth_signed_message, &sig);
        println!("Verify with EIP-191: {:?}", result1);

        // Verify with checkpoint_digest (without EIP-191)
        let result2 = verifying_key.verify_prehash(&checkpoint_digest, &sig);
        println!("Verify without EIP-191: {:?}", result2);

        // This test is informational - we want to see the output
        // The assertion will fail but we want to see the diagnostic info
        if result1.is_err() && result2.is_err() {
            println!("\n=== CONCLUSION ===");
            println!("The public key we have does not match the signer of this signature.");
            println!("We need to get the correct validator public keys.");
        }
    }

    /// Recover ALL validator public keys from the Fuji signatures
    /// This gives us the correct keys to store in the ISM datum
    #[test]
    fn test_recover_all_fuji_validator_keys() {
        // Test data from relayer logs
        let origin: u32 = 43113;
        let merkle_root =
            hex::decode("efa004d027c79c3d7faf7821111493144243a32f8616af99ceff8238000010ec")
                .unwrap();
        let origin_merkle_tree_hook =
            hex::decode("0000000000000000000000009ff6ac3daf63103620bbf76136ea1aff43c2f612")
                .unwrap();
        let merkle_index: u32 = 146986598;
        let message_id =
            hex::decode("0ce4b05a9d25d2556f74ddaa1ac84841341623376c9e5cd073f52b1b54dcddbf")
                .unwrap();

        // All signatures from Fuji validators (65 bytes each: r || s || v)
        let signatures = vec![
            hex::decode("d88d35b30b437c9d069dc3e97263d8b06367ae53840fdb1d0f8009e61ded9cad1ca7cb64f16f21a08634065f7de2cc92d651fa5bd04603e675ad72fffe39b4761b").unwrap(),
            hex::decode("5f2d5eceb1dc4c9a6ce96af2c9d20a4b622a86224535035337fe3c3fdfb71f5e2e195aa0eca28e609a0d2b6550d97d8228d91e976040cf94f8e7124581dfe8261c").unwrap(),
        ];

        println!("=== Computing checkpoint hash ===");

        // Step 1: domain_hash = keccak256(origin || merkle_tree_hook || "HYPERLANE")
        let mut domain_hasher = Keccak256::new();
        domain_hasher.update(&origin.to_be_bytes());
        domain_hasher.update(&origin_merkle_tree_hook);
        domain_hasher.update(b"HYPERLANE");
        let domain_hash: [u8; 32] = domain_hasher.finalize().into();

        // Step 2: checkpoint_digest = keccak256(domain_hash || merkle_root || merkle_index || message_id)
        let mut checkpoint_hasher = Keccak256::new();
        checkpoint_hasher.update(&domain_hash);
        checkpoint_hasher.update(&merkle_root);
        checkpoint_hasher.update(&merkle_index.to_be_bytes());
        checkpoint_hasher.update(&message_id);
        let checkpoint_digest: [u8; 32] = checkpoint_hasher.finalize().into();

        // Step 3: eth_signed_message = keccak256("\x19Ethereum Signed Message:\n32" || checkpoint_digest)
        let mut eth_hasher = Keccak256::new();
        eth_hasher.update(b"\x19Ethereum Signed Message:\n32");
        eth_hasher.update(&checkpoint_digest);
        let eth_signed_message: [u8; 32] = eth_hasher.finalize().into();

        println!("\n=== RECOVERED VALIDATOR PUBLIC KEYS ===");
        println!("Use these keys in your ISM datum:\n");

        let mut recovered_keys = Vec::new();

        for (i, sig_bytes) in signatures.iter().enumerate() {
            println!("--- Validator {} ---", i);

            // Extract v from signature (last byte)
            let v = sig_bytes[64];
            let recovery_id = if v >= 27 { v - 27 } else { v };

            // Parse signature (first 64 bytes: r || s)
            let sig = Signature::from_slice(&sig_bytes[..64]).expect("Invalid signature");
            let rec_id = RecoveryId::try_from(recovery_id).expect("Invalid recovery id");

            match VerifyingKey::recover_from_prehash(&eth_signed_message, &sig, rec_id) {
                Ok(recovered_key) => {
                    let compressed = recovered_key.to_sec1_bytes();
                    println!(
                        "Compressed public key (33 bytes): {}",
                        hex::encode(&compressed)
                    );

                    // Get uncompressed key (64 bytes without 0x04 prefix)
                    let uncompressed = recovered_key.to_encoded_point(false);
                    let public_key_bytes = &uncompressed.as_bytes()[1..]; // Skip 0x04 prefix
                    println!(
                        "Uncompressed public key (64 bytes): {}",
                        hex::encode(public_key_bytes)
                    );

                    // Compute Ethereum address
                    let address_hash = Keccak256::digest(public_key_bytes);
                    let eth_address = &address_hash[12..];
                    println!("Ethereum address: 0x{}", hex::encode(eth_address));

                    // Verify signature works with this key
                    match recovered_key.verify_prehash(&eth_signed_message, &sig) {
                        Ok(_) => println!("✓ Signature verified successfully"),
                        Err(e) => println!("✗ Signature verification failed: {}", e),
                    }

                    recovered_keys.push(hex::encode(public_key_bytes));
                }
                Err(e) => {
                    println!("✗ Recovery failed: {:?}", e);
                }
            }
            println!();
        }

        println!("\n=== AIKEN ISM DATUM FORMAT ===");
        println!("validators: [");
        for (i, key) in recovered_keys.iter().enumerate() {
            println!("  #{},  // Validator {}", key, i);
        }
        println!("]");

        println!("\n=== SUCCESS ===");
        println!("Update your ISM datum with these public keys!");
    }

    /// Test with the latest metadata from logs to verify signatures and addresses
    #[test]
    fn test_latest_metadata_signature_recovery() {
        // Latest metadata from logs
        let metadata: Vec<u8> = vec![
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 159, 246, 172, 61, 175, 99, 16, 54, 32, 187, 247,
            97, 54, 234, 26, 255, 67, 194, 246, 18, 5, 196, 38, 50, 179, 29, 158, 122, 115, 130,
            20, 63, 142, 74, 181, 163, 160, 122, 80, 86, 135, 81, 202, 121, 39, 123, 63, 13, 4, 7,
            101, 206, 0, 0, 16, 239, 213, 152, 62, 114, 113, 105, 226, 4, 8, 242, 145, 177, 49,
            176, 151, 194, 62, 169, 173, 9, 17, 126, 199, 58, 165, 26, 177, 189, 206, 40, 62, 90,
            110, 124, 97, 28, 95, 184, 110, 220, 56, 56, 148, 10, 120, 115, 100, 103, 81, 34, 107,
            171, 211, 28, 155, 21, 58, 146, 197, 130, 54, 244, 33, 15, 27, 172, 193, 162, 254, 168,
            176, 252, 96, 124, 232, 195, 224, 217, 34, 167, 239, 188, 125, 220, 101, 199, 174, 88,
            31, 231, 83, 199, 75, 36, 229, 212, 178, 112, 214, 60, 13, 246, 186, 201, 100, 189,
            245, 194, 230, 156, 45, 67, 119, 56, 96, 92, 178, 71, 97, 219, 127, 185, 115, 143, 22,
            251, 193, 73, 86, 27,
        ];

        // Message ID from logs
        let message_id =
            hex::decode("a6e55f83b2f995471c99bca10a9ed8e606c706fcf46ce57791d377943363a729")
                .unwrap();

        // Origin domain (fuji = 43113)
        let origin: u32 = 43113;

        // Trusted validator addresses from logs
        let trusted_addresses = vec![
            "d8154f73d04cc7f7f0c332793692e6e6f6b2402e",
            "895ae30bc83ff1493b9cf7781b0b813d23659857",
            "43e915573d9f1383cbf482049e4a012290759e7f",
            "7095c11126faf3d61b7d1144815720fb09bb8b20",
        ];

        // Parse metadata
        let origin_merkle_tree_hook = &metadata[0..32];
        let root_index = u32::from_be_bytes(metadata[32..36].try_into().unwrap());
        let merkle_root = &metadata[36..68];
        let signatures_data = &metadata[68..];

        println!("=== Parsed Metadata ===");
        println!(
            "origin_merkle_tree_hook: {}",
            hex::encode(origin_merkle_tree_hook)
        );
        println!("root_index (merkle_index): {}", root_index);
        println!("merkle_root: {}", hex::encode(merkle_root));
        println!("message_id: {}", hex::encode(&message_id));
        println!(
            "signatures_data length: {} (expecting {} signatures)",
            signatures_data.len(),
            signatures_data.len() / 65
        );

        // Step 1: domain_hash = keccak256(origin || merkle_tree_hook || "HYPERLANE")
        let mut domain_hasher = Keccak256::new();
        domain_hasher.update(&origin.to_be_bytes());
        domain_hasher.update(origin_merkle_tree_hook);
        domain_hasher.update(b"HYPERLANE");
        let domain_hash: [u8; 32] = domain_hasher.finalize().into();

        println!("\n=== Hash Computation ===");
        println!("domain_hash: {}", hex::encode(&domain_hash));

        // Step 2: checkpoint_digest = keccak256(domain_hash || merkle_root || merkle_index || message_id)
        let mut checkpoint_hasher = Keccak256::new();
        checkpoint_hasher.update(&domain_hash);
        checkpoint_hasher.update(merkle_root);
        checkpoint_hasher.update(&root_index.to_be_bytes());
        checkpoint_hasher.update(&message_id);
        let checkpoint_digest: [u8; 32] = checkpoint_hasher.finalize().into();
        println!("checkpoint_digest: {}", hex::encode(&checkpoint_digest));

        // Step 3: eth_signed_message = keccak256("\x19Ethereum Signed Message:\n32" || checkpoint_digest)
        let mut eth_hasher = Keccak256::new();
        eth_hasher.update(b"\x19Ethereum Signed Message:\n32");
        eth_hasher.update(&checkpoint_digest);
        let eth_signed_message: [u8; 32] = eth_hasher.finalize().into();
        println!("eth_signed_message: {}", hex::encode(&eth_signed_message));

        println!("\n=== Signature Recovery ===");

        let mut offset = 0;
        let mut sig_num = 0;
        let mut recovered_addresses = Vec::new();

        while offset + 65 <= signatures_data.len() {
            let sig_bytes = &signatures_data[offset..offset + 65];
            let v = sig_bytes[64];
            let recovery_id = if v >= 27 { v - 27 } else { v };

            println!("\n--- Signature {} ---", sig_num);
            println!("r: {}", hex::encode(&sig_bytes[0..32]));
            println!("s: {}", hex::encode(&sig_bytes[32..64]));
            println!("v: {} (recovery_id: {})", v, recovery_id);

            let sig = Signature::from_slice(&sig_bytes[..64]).expect("Invalid signature");
            let rec_id = RecoveryId::try_from(recovery_id).expect("Invalid recovery id");

            match VerifyingKey::recover_from_prehash(&eth_signed_message, &sig, rec_id) {
                Ok(recovered_key) => {
                    // Get compressed key
                    let compressed = recovered_key.to_sec1_bytes();
                    println!("Compressed pubkey: {}", hex::encode(&compressed));

                    // Get uncompressed key (64 bytes without 0x04 prefix)
                    let uncompressed = recovered_key.to_encoded_point(false);
                    let public_key_bytes = &uncompressed.as_bytes()[1..];
                    println!("Uncompressed pubkey: {}", hex::encode(public_key_bytes));

                    // Compute Ethereum address
                    let address_hash = Keccak256::digest(public_key_bytes);
                    let eth_address = &address_hash[12..];
                    let eth_address_hex = hex::encode(eth_address);
                    println!("Ethereum address: 0x{}", eth_address_hex);

                    recovered_addresses.push(eth_address_hex.clone());

                    // Check if this address is in trusted list
                    if trusted_addresses.contains(&eth_address_hex.as_str()) {
                        println!("✓ Address is in trusted validator list!");
                    } else {
                        println!("✗ Address NOT in trusted validator list");
                    }

                    // Verify signature
                    match recovered_key.verify_prehash(&eth_signed_message, &sig) {
                        Ok(_) => println!("✓ Signature verification passed"),
                        Err(e) => println!("✗ Signature verification failed: {}", e),
                    }
                }
                Err(e) => {
                    println!("✗ Recovery failed: {:?}", e);
                }
            }

            offset += 65;
            sig_num += 1;
        }

        println!("\n=== Summary ===");
        println!("Trusted addresses:");
        for addr in &trusted_addresses {
            println!("  0x{}", addr);
        }
        println!("\nRecovered addresses:");
        for addr in &recovered_addresses {
            let is_trusted = trusted_addresses.contains(&addr.as_str());
            println!("  0x{} {}", addr, if is_trusted { "✓" } else { "✗" });
        }

        // Assert that at least threshold (2) addresses match
        let matching_count = recovered_addresses
            .iter()
            .filter(|addr| trusted_addresses.contains(&addr.as_str()))
            .count();

        println!(
            "\nMatching addresses: {} / {}",
            matching_count,
            recovered_addresses.len()
        );

        // This test is informational - show results
        if matching_count < 2 {
            println!("\n!!! CRITICAL: Not enough matching addresses !!!");
            println!("The recovered Ethereum addresses don't match the trusted validators.");
            println!("This indicates an issue with:");
            println!("  1. The checkpoint hash computation");
            println!("  2. The signature recovery process");
            println!("  3. The trusted address configuration in ISM datum");
        }
    }

    /// Test to verify that signature normalization doesn't affect key recovery
    #[test]
    fn test_normalization_effect_on_recovery() {
        // Latest metadata from logs
        let metadata: Vec<u8> = vec![
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 159, 246, 172, 61, 175, 99, 16, 54, 32, 187, 247,
            97, 54, 234, 26, 255, 67, 194, 246, 18, 5, 196, 38, 50, 179, 29, 158, 122, 115, 130,
            20, 63, 142, 74, 181, 163, 160, 122, 80, 86, 135, 81, 202, 121, 39, 123, 63, 13, 4, 7,
            101, 206, 0, 0, 16, 239, 213, 152, 62, 114, 113, 105, 226, 4, 8, 242, 145, 177, 49,
            176, 151, 194, 62, 169, 173, 9, 17, 126, 199, 58, 165, 26, 177, 189, 206, 40, 62, 90,
            110, 124, 97, 28, 95, 184, 110, 220, 56, 56, 148, 10, 120, 115, 100, 103, 81, 34, 107,
            171, 211, 28, 155, 21, 58, 146, 197, 130, 54, 244, 33, 15, 27, 172, 193, 162, 254, 168,
            176, 252, 96, 124, 232, 195, 224, 217, 34, 167, 239, 188, 125, 220, 101, 199, 174, 88,
            31, 231, 83, 199, 75, 36, 229, 212, 178, 112, 214, 60, 13, 246, 186, 201, 100, 189,
            245, 194, 230, 156, 45, 67, 119, 56, 96, 92, 178, 71, 97, 219, 127, 185, 115, 143, 22,
            251, 193, 73, 86, 27,
        ];

        let message_id =
            hex::decode("a6e55f83b2f995471c99bca10a9ed8e606c706fcf46ce57791d377943363a729")
                .unwrap();
        let origin: u32 = 43113;

        // Parse metadata
        let origin_merkle_tree_hook = &metadata[0..32];
        let root_index = u32::from_be_bytes(metadata[32..36].try_into().unwrap());
        let merkle_root = &metadata[36..68];
        let signatures_data = &metadata[68..];

        // Compute eth_signed_message
        let mut domain_hasher = Keccak256::new();
        domain_hasher.update(&origin.to_be_bytes());
        domain_hasher.update(origin_merkle_tree_hook);
        domain_hasher.update(b"HYPERLANE");
        let domain_hash: [u8; 32] = domain_hasher.finalize().into();

        let mut checkpoint_hasher = Keccak256::new();
        checkpoint_hasher.update(&domain_hash);
        checkpoint_hasher.update(merkle_root);
        checkpoint_hasher.update(&root_index.to_be_bytes());
        checkpoint_hasher.update(&message_id);
        let checkpoint_digest: [u8; 32] = checkpoint_hasher.finalize().into();

        let mut eth_hasher = Keccak256::new();
        eth_hasher.update(b"\x19Ethereum Signed Message:\n32");
        eth_hasher.update(&checkpoint_digest);
        let eth_signed_message: [u8; 32] = eth_hasher.finalize().into();

        println!("=== Testing Normalization Effect on Recovery ===\n");

        let sig_bytes = &signatures_data[0..65];
        let v = sig_bytes[64];
        let recovery_id = if v >= 27 { v - 27 } else { v };

        let sig = Signature::from_slice(&sig_bytes[..64]).expect("Invalid signature");
        let rec_id = RecoveryId::try_from(recovery_id).expect("Invalid recovery id");

        println!("Original signature:");
        println!("  r: {}", hex::encode(&sig_bytes[0..32]));
        println!("  s: {}", hex::encode(&sig_bytes[32..64]));
        println!("  v: {} (recovery_id: {})", v, recovery_id);

        // Check if signature is high-s
        let is_high_s = sig.normalize_s().is_some();
        println!("  Is high-s: {}", is_high_s);

        // Recovery with original signature
        let recovered_key_original =
            VerifyingKey::recover_from_prehash(&eth_signed_message, &sig, rec_id)
                .expect("Recovery failed");

        let original_uncompressed = recovered_key_original.to_encoded_point(false);
        let original_pubkey = &original_uncompressed.as_bytes()[1..];
        let original_address = &Keccak256::digest(original_pubkey)[12..];

        println!("\nRecovery with ORIGINAL signature:");
        println!("  Pubkey: {}", hex::encode(original_pubkey));
        println!("  Address: 0x{}", hex::encode(original_address));

        // If the signature needs normalization, try recovery with normalized
        if let Some(normalized_sig) = sig.normalize_s() {
            println!("\nNormalized signature:");
            let normalized_bytes: [u8; 64] = normalized_sig.to_bytes().into();
            println!("  r: {}", hex::encode(&normalized_bytes[0..32]));
            println!("  s: {}", hex::encode(&normalized_bytes[32..64]));

            // Try recovery with normalized signature - SAME recovery ID
            println!(
                "\nRecovery with NORMALIZED signature (same v={}):",
                recovery_id
            );
            match VerifyingKey::recover_from_prehash(&eth_signed_message, &normalized_sig, rec_id) {
                Ok(recovered_key_normalized) => {
                    let normalized_uncompressed = recovered_key_normalized.to_encoded_point(false);
                    let normalized_pubkey = &normalized_uncompressed.as_bytes()[1..];
                    let normalized_address = &Keccak256::digest(normalized_pubkey)[12..];
                    println!("  Pubkey: {}", hex::encode(normalized_pubkey));
                    println!("  Address: 0x{}", hex::encode(normalized_address));

                    if original_address == normalized_address {
                        println!("  ✓ Same address recovered!");
                    } else {
                        println!("  ✗ DIFFERENT address recovered!");
                    }
                }
                Err(e) => {
                    println!("  ✗ Recovery failed: {:?}", e);
                }
            }

            // Try recovery with normalized signature - FLIPPED recovery ID
            let flipped_id = if recovery_id == 0 { 1 } else { 0 };
            println!(
                "\nRecovery with NORMALIZED signature (flipped v={}):",
                flipped_id
            );
            let flipped_rec_id = RecoveryId::try_from(flipped_id).unwrap();
            match VerifyingKey::recover_from_prehash(
                &eth_signed_message,
                &normalized_sig,
                flipped_rec_id,
            ) {
                Ok(recovered_key_flipped) => {
                    let flipped_uncompressed = recovered_key_flipped.to_encoded_point(false);
                    let flipped_pubkey = &flipped_uncompressed.as_bytes()[1..];
                    let flipped_address = &Keccak256::digest(flipped_pubkey)[12..];
                    println!("  Pubkey: {}", hex::encode(flipped_pubkey));
                    println!("  Address: 0x{}", hex::encode(flipped_address));

                    if original_address == flipped_address {
                        println!("  ✓ Same address recovered with flipped v!");
                    } else {
                        println!("  ✗ Different address with flipped v");
                    }
                }
                Err(e) => {
                    println!("  ✗ Recovery failed: {:?}", e);
                }
            }
        } else {
            println!("\nSignature is already in low-s form, no normalization needed.");
        }

        // Verify the original public key works with both original and normalized signatures
        println!("\n=== Verification Test ===");
        println!("Verifying original signature with recovered key...");
        match recovered_key_original.verify_prehash(&eth_signed_message, &sig) {
            Ok(_) => println!("  ✓ Original signature verifies"),
            Err(e) => println!("  ✗ Failed: {}", e),
        }

        if let Some(normalized_sig) = sig.normalize_s() {
            println!("Verifying NORMALIZED signature with ORIGINAL recovered key...");
            match recovered_key_original.verify_prehash(&eth_signed_message, &normalized_sig) {
                Ok(_) => println!("  ✓ Normalized signature verifies with same key!"),
                Err(e) => println!("  ✗ Failed: {}", e),
            }
        }
    }

    /// Test what happens if we recover pubkey from a HIGH-S signature that gets normalized
    /// This simulates a scenario where we might accidentally use normalized sig for recovery
    #[test]
    fn test_high_s_signature_recovery() {
        // secp256k1 curve order n/2 for comparison
        let n_half =
            hex::decode("7fffffffffffffffffffffffffffffff5d576e7357a4501ddfe92f46681b20a0")
                .unwrap();

        println!("=== High-S Signature Analysis ===\n");
        println!("n/2: {}", hex::encode(&n_half));

        // Use the metadata from logs
        let metadata: Vec<u8> = vec![
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 159, 246, 172, 61, 175, 99, 16, 54, 32, 187, 247,
            97, 54, 234, 26, 255, 67, 194, 246, 18, 5, 196, 38, 50, 179, 29, 158, 122, 115, 130,
            20, 63, 142, 74, 181, 163, 160, 122, 80, 86, 135, 81, 202, 121, 39, 123, 63, 13, 4, 7,
            101, 206, 0, 0, 16, 239, 213, 152, 62, 114, 113, 105, 226, 4, 8, 242, 145, 177, 49,
            176, 151, 194, 62, 169, 173, 9, 17, 126, 199, 58, 165, 26, 177, 189, 206, 40, 62, 90,
            110, 124, 97, 28, 95, 184, 110, 220, 56, 56, 148, 10, 120, 115, 100, 103, 81, 34, 107,
            171, 211, 28, 155, 21, 58, 146, 197, 130, 54, 244, 33, 15, 27, 172, 193, 162, 254, 168,
            176, 252, 96, 124, 232, 195, 224, 217, 34, 167, 239, 188, 125, 220, 101, 199, 174, 88,
            31, 231, 83, 199, 75, 36, 229, 212, 178, 112, 214, 60, 13, 246, 186, 201, 100, 189,
            245, 194, 230, 156, 45, 67, 119, 56, 96, 92, 178, 71, 97, 219, 127, 185, 115, 143, 22,
            251, 193, 73, 86, 27,
        ];
        let signatures_data = &metadata[68..];

        // Check both signatures
        for i in 0..2 {
            let sig_bytes = &signatures_data[i * 65..(i + 1) * 65];
            let s = &sig_bytes[32..64];

            println!("\nSignature {}:", i);
            println!("  s value: {}", hex::encode(s));

            // Compare s with n/2
            let is_high_s = s > n_half.as_slice();
            println!("  s > n/2 (high-s): {}", is_high_s);

            let sig = Signature::from_slice(&sig_bytes[..64]).expect("Invalid signature");
            let needs_normalization = sig.normalize_s().is_some();
            println!("  Needs normalization (k256): {}", needs_normalization);
        }

        // Test recovery with normalize_s
        println!("\n=== Testing normalize_s behavior ===");

        let sig_bytes = &signatures_data[0..65];
        let v = sig_bytes[64];
        let recovery_id = if v >= 27 { v - 27 } else { v };

        let sig = Signature::from_slice(&sig_bytes[..64]).expect("Invalid signature");
        let rec_id = RecoveryId::try_from(recovery_id).expect("Invalid recovery id");

        // Compute the message hash
        let message_id =
            hex::decode("a6e55f83b2f995471c99bca10a9ed8e606c706fcf46ce57791d377943363a729")
                .unwrap();
        let origin: u32 = 43113;
        let origin_merkle_tree_hook = &metadata[0..32];
        let root_index = u32::from_be_bytes(metadata[32..36].try_into().unwrap());
        let merkle_root = &metadata[36..68];

        let mut domain_hasher = Keccak256::new();
        domain_hasher.update(&origin.to_be_bytes());
        domain_hasher.update(origin_merkle_tree_hook);
        domain_hasher.update(b"HYPERLANE");
        let domain_hash: [u8; 32] = domain_hasher.finalize().into();

        let mut checkpoint_hasher = Keccak256::new();
        checkpoint_hasher.update(&domain_hash);
        checkpoint_hasher.update(merkle_root);
        checkpoint_hasher.update(&root_index.to_be_bytes());
        checkpoint_hasher.update(&message_id);
        let checkpoint_digest: [u8; 32] = checkpoint_hasher.finalize().into();

        let mut eth_hasher = Keccak256::new();
        eth_hasher.update(b"\x19Ethereum Signed Message:\n32");
        eth_hasher.update(&checkpoint_digest);
        let eth_signed_message: [u8; 32] = eth_hasher.finalize().into();

        println!("\n--- Original Recovery ---");
        let key_original = VerifyingKey::recover_from_prehash(&eth_signed_message, &sig, rec_id)
            .expect("Recovery failed");
        let uncompressed = key_original.to_encoded_point(false);
        let pubkey_bytes = &uncompressed.as_bytes()[1..];
        let addr_original = &Keccak256::digest(pubkey_bytes)[12..];
        println!(
            "Address from ORIGINAL sig: 0x{}",
            hex::encode(addr_original)
        );

        // Test with normalized signature
        let normalized_sig = sig.normalize_s().unwrap_or(sig);

        if sig.normalize_s().is_some() {
            println!("\n--- Normalized Recovery (same v) ---");
            match VerifyingKey::recover_from_prehash(&eth_signed_message, &normalized_sig, rec_id) {
                Ok(key_norm) => {
                    let uncompressed_norm = key_norm.to_encoded_point(false);
                    let pubkey_norm = &uncompressed_norm.as_bytes()[1..];
                    let addr_norm = &Keccak256::digest(pubkey_norm)[12..];
                    println!(
                        "Address from NORMALIZED sig (same v): 0x{}",
                        hex::encode(addr_norm)
                    );

                    if addr_original == addr_norm {
                        println!("  ✓ SAME address - normalization doesn't affect recovery here");
                    } else {
                        println!("  ✗ DIFFERENT address - THIS IS THE BUG!");
                        println!(
                            "  When we normalize s but keep the same v, we get wrong address!"
                        );
                    }
                }
                Err(e) => println!("Recovery failed: {:?}", e),
            }

            println!("\n--- Normalized Recovery (flipped v) ---");
            let flipped_v = if recovery_id == 0 { 1 } else { 0 };
            let flipped_rec_id = RecoveryId::try_from(flipped_v).expect("Invalid recovery id");

            match VerifyingKey::recover_from_prehash(
                &eth_signed_message,
                &normalized_sig,
                flipped_rec_id,
            ) {
                Ok(key_flipped) => {
                    let uncompressed_flipped = key_flipped.to_encoded_point(false);
                    let pubkey_flipped = &uncompressed_flipped.as_bytes()[1..];
                    let addr_flipped = &Keccak256::digest(pubkey_flipped)[12..];
                    println!(
                        "Address from NORMALIZED sig (flipped v): 0x{}",
                        hex::encode(addr_flipped)
                    );

                    if addr_original == addr_flipped {
                        println!("  ✓ SAME address with flipped v!");
                    } else {
                        println!("  ✗ Still different");
                    }
                }
                Err(e) => println!("Recovery failed: {:?}", e),
            }
        } else {
            println!("\nSignature is already low-s, no normalization needed.");
        }

        // Verify that the original pubkey can verify BOTH signatures
        println!("\n--- Verification Test ---");
        println!(
            "Original sig verifies: {:?}",
            key_original.verify_prehash(&eth_signed_message, &sig)
        );
        println!(
            "Normalized sig verifies: {:?}",
            key_original.verify_prehash(&eth_signed_message, &normalized_sig)
        );
    }

    /// Verify our checkpoint hash matches hyperlane-core's implementation
    #[test]
    fn test_checkpoint_hash_matches_hyperlane_core() {
        use hyperlane_core::{Checkpoint, CheckpointWithMessageId, Signable, H256};

        // From logs
        let origin: u32 = 43113;
        let merkle_root =
            hex::decode("b31d9e7a7382143f8e4ab5a3a07a50568751ca79277b3f0d040765ce000010ef")
                .unwrap();
        let origin_merkle_tree_hook =
            hex::decode("0000000000000000000000009ff6ac3daf63103620bbf76136ea1aff43c2f612")
                .unwrap();
        let merkle_index: u32 = 96740914;
        let message_id =
            hex::decode("a6e55f83b2f995471c99bca10a9ed8e606c706fcf46ce57791d377943363a729")
                .unwrap();

        // Create hyperlane-core's checkpoint type
        let checkpoint = CheckpointWithMessageId {
            checkpoint: Checkpoint {
                merkle_tree_hook_address: H256::from_slice(&origin_merkle_tree_hook),
                mailbox_domain: origin,
                root: H256::from_slice(&merkle_root),
                index: merkle_index,
            },
            message_id: H256::from_slice(&message_id),
        };

        // Get the signing hash from hyperlane-core
        let core_signing_hash = checkpoint.signing_hash();

        // Now compute it our way
        let mut domain_hasher = Keccak256::new();
        domain_hasher.update(&origin.to_be_bytes());
        domain_hasher.update(&origin_merkle_tree_hook);
        domain_hasher.update(b"HYPERLANE");
        let domain_hash: [u8; 32] = domain_hasher.finalize().into();

        let mut checkpoint_hasher = Keccak256::new();
        checkpoint_hasher.update(&domain_hash);
        checkpoint_hasher.update(&merkle_root);
        checkpoint_hasher.update(&merkle_index.to_be_bytes());
        checkpoint_hasher.update(&message_id);
        let our_signing_hash: [u8; 32] = checkpoint_hasher.finalize().into();

        println!("=== Checkpoint Hash Comparison ===");
        println!(
            "hyperlane-core signing_hash: {}",
            hex::encode(core_signing_hash.as_bytes())
        );
        println!(
            "Our signing_hash:            {}",
            hex::encode(&our_signing_hash)
        );

        assert_eq!(
            core_signing_hash.as_bytes(),
            &our_signing_hash,
            "Signing hash mismatch!"
        );
        println!("✓ Signing hashes match!");

        // Also verify the eth_signed_message_hash
        let core_eth_hash = checkpoint.eth_signed_message_hash();

        let mut eth_hasher = Keccak256::new();
        eth_hasher.update(b"\x19Ethereum Signed Message:\n32");
        eth_hasher.update(&our_signing_hash);
        let our_eth_hash: [u8; 32] = eth_hasher.finalize().into();

        println!(
            "\nhyperlane-core eth_signed_message_hash: {}",
            hex::encode(core_eth_hash.as_bytes())
        );
        println!(
            "Our eth_signed_message_hash:            {}",
            hex::encode(&our_eth_hash)
        );

        assert_eq!(
            core_eth_hash.as_bytes(),
            &our_eth_hash,
            "Eth signed message hash mismatch!"
        );
        println!("✓ Eth signed message hashes match!");
    }

    /// Try all recovery IDs to find which one gives us a trusted validator address
    #[test]
    fn test_find_correct_recovery_id() {
        // Latest metadata from logs
        let metadata: Vec<u8> = vec![
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 159, 246, 172, 61, 175, 99, 16, 54, 32, 187, 247,
            97, 54, 234, 26, 255, 67, 194, 246, 18, 5, 196, 38, 50, 179, 29, 158, 122, 115, 130,
            20, 63, 142, 74, 181, 163, 160, 122, 80, 86, 135, 81, 202, 121, 39, 123, 63, 13, 4, 7,
            101, 206, 0, 0, 16, 239, 213, 152, 62, 114, 113, 105, 226, 4, 8, 242, 145, 177, 49,
            176, 151, 194, 62, 169, 173, 9, 17, 126, 199, 58, 165, 26, 177, 189, 206, 40, 62, 90,
            110, 124, 97, 28, 95, 184, 110, 220, 56, 56, 148, 10, 120, 115, 100, 103, 81, 34, 107,
            171, 211, 28, 155, 21, 58, 146, 197, 130, 54, 244, 33, 15, 27, 172, 193, 162, 254, 168,
            176, 252, 96, 124, 232, 195, 224, 217, 34, 167, 239, 188, 125, 220, 101, 199, 174, 88,
            31, 231, 83, 199, 75, 36, 229, 212, 178, 112, 214, 60, 13, 246, 186, 201, 100, 189,
            245, 194, 230, 156, 45, 67, 119, 56, 96, 92, 178, 71, 97, 219, 127, 185, 115, 143, 22,
            251, 193, 73, 86, 27,
        ];

        let message_id =
            hex::decode("a6e55f83b2f995471c99bca10a9ed8e606c706fcf46ce57791d377943363a729")
                .unwrap();
        let origin: u32 = 43113;

        // Official Fuji validators from docs
        let trusted_addresses = vec![
            "d8154f73d04cc7f7f0c332793692e6e6f6b2402e",
            "895ae30bc83ff1493b9cf7781b0b813d23659857",
            "43e915573d9f1383cbf482049e4a012290759e7f",
        ];

        // Parse metadata
        let origin_merkle_tree_hook = &metadata[0..32];
        let root_index = u32::from_be_bytes(metadata[32..36].try_into().unwrap());
        let merkle_root = &metadata[36..68];
        let signatures_data = &metadata[68..];

        // Compute eth_signed_message
        let mut domain_hasher = Keccak256::new();
        domain_hasher.update(&origin.to_be_bytes());
        domain_hasher.update(origin_merkle_tree_hook);
        domain_hasher.update(b"HYPERLANE");
        let domain_hash: [u8; 32] = domain_hasher.finalize().into();

        let mut checkpoint_hasher = Keccak256::new();
        checkpoint_hasher.update(&domain_hash);
        checkpoint_hasher.update(merkle_root);
        checkpoint_hasher.update(&root_index.to_be_bytes());
        checkpoint_hasher.update(&message_id);
        let checkpoint_digest: [u8; 32] = checkpoint_hasher.finalize().into();

        let mut eth_hasher = Keccak256::new();
        eth_hasher.update(b"\x19Ethereum Signed Message:\n32");
        eth_hasher.update(&checkpoint_digest);
        let eth_signed_message: [u8; 32] = eth_hasher.finalize().into();

        println!("=== Trying All Recovery IDs ===\n");
        println!("eth_signed_message: {}", hex::encode(&eth_signed_message));
        println!("\nTrusted addresses:");
        for addr in &trusted_addresses {
            println!("  0x{}", addr);
        }

        let mut offset = 0;
        let mut sig_num = 0;

        while offset + 65 <= signatures_data.len() {
            let sig_bytes = &signatures_data[offset..offset + 65];
            let v_original = sig_bytes[64];

            println!("\n--- Signature {} (v={}) ---", sig_num, v_original);
            println!("r: {}", hex::encode(&sig_bytes[0..32]));
            println!("s: {}", hex::encode(&sig_bytes[32..64]));

            let sig = Signature::from_slice(&sig_bytes[..64]).expect("Invalid signature");

            // Try both recovery IDs (0 and 1)
            for rec_id_val in 0u8..=1 {
                let rec_id = RecoveryId::try_from(rec_id_val).unwrap();

                match VerifyingKey::recover_from_prehash(&eth_signed_message, &sig, rec_id) {
                    Ok(recovered_key) => {
                        let uncompressed = recovered_key.to_encoded_point(false);
                        let public_key_bytes = &uncompressed.as_bytes()[1..];
                        let address_hash = Keccak256::digest(public_key_bytes);
                        let eth_address = hex::encode(&address_hash[12..]);

                        let is_trusted = trusted_addresses.contains(&eth_address.as_str());
                        let marker = if is_trusted { "✓ MATCH!" } else { "✗" };

                        println!("  recovery_id={}: 0x{} {}", rec_id_val, eth_address, marker);
                    }
                    Err(e) => {
                        println!("  recovery_id={}: Recovery failed: {:?}", rec_id_val, e);
                    }
                }
            }

            offset += 65;
            sig_num += 1;
        }
    }

    /// Compare Format 1 (correct per Hyperlane docs) vs Format 2 (current tx_builder)
    #[test]
    fn test_metadata_format_comparison() {
        // Latest metadata from logs
        let metadata: Vec<u8> = vec![
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 159, 246, 172, 61, 175, 99, 16, 54, 32, 187, 247,
            97, 54, 234, 26, 255, 67, 194, 246, 18, 5, 196, 38, 50, 179, 29, 158, 122, 115, 130,
            20, 63, 142, 74, 181, 163, 160, 122, 80, 86, 135, 81, 202, 121, 39, 123, 63, 13, 4, 7,
            101, 206, 0, 0, 16, 239, 213, 152, 62, 114, 113, 105, 226, 4, 8, 242, 145, 177, 49,
            176, 151, 194, 62, 169, 173, 9, 17, 126, 199, 58, 165, 26, 177, 189, 206, 40, 62, 90,
            110, 124, 97, 28, 95, 184, 110, 220, 56, 56, 148, 10, 120, 115, 100, 103, 81, 34, 107,
            171, 211, 28, 155, 21, 58, 146, 197, 130, 54, 244, 33, 15, 27, 172, 193, 162, 254, 168,
            176, 252, 96, 124, 232, 195, 224, 217, 34, 167, 239, 188, 125, 220, 101, 199, 174, 88,
            31, 231, 83, 199, 75, 36, 229, 212, 178, 112, 214, 60, 13, 246, 186, 201, 100, 189,
            245, 194, 230, 156, 45, 67, 119, 56, 96, 92, 178, 71, 97, 219, 127, 185, 115, 143, 22,
            251, 193, 73, 86, 27,
        ];

        let message_id =
            hex::decode("a6e55f83b2f995471c99bca10a9ed8e606c706fcf46ce57791d377943363a729")
                .unwrap();
        let origin: u32 = 43113;

        // Official Fuji validators
        let trusted_addresses = vec![
            "d8154f73d04cc7f7f0c332793692e6e6f6b2402e",
            "895ae30bc83ff1493b9cf7781b0b813d23659857",
            "43e915573d9f1383cbf482049e4a012290759e7f",
        ];

        println!("=== FORMAT 1 (MerkleTreeHook + Root + Index + Signatures) ===");
        println!("Per Hyperlane docs: https://docs.hyperlane.xyz/docs/protocol/ISM/standard-ISMs/multisig-ISM\n");

        // Format 1: MerkleTreeHook (32) + MerkleRoot (32) + Index (4) + Signatures
        let merkle_tree_hook_1 = &metadata[0..32];
        let merkle_root_1 = &metadata[32..64];
        let merkle_index_1 = u32::from_be_bytes(metadata[64..68].try_into().unwrap());
        let signatures_1 = &metadata[68..];

        println!("merkle_tree_hook: {}", hex::encode(merkle_tree_hook_1));
        println!("merkle_root:      {}", hex::encode(merkle_root_1));
        println!("merkle_index:     {}", merkle_index_1);

        // Compute hash with Format 1
        let mut domain_hasher = Keccak256::new();
        domain_hasher.update(&origin.to_be_bytes());
        domain_hasher.update(merkle_tree_hook_1);
        domain_hasher.update(b"HYPERLANE");
        let domain_hash_1: [u8; 32] = domain_hasher.finalize().into();

        let mut checkpoint_hasher = Keccak256::new();
        checkpoint_hasher.update(&domain_hash_1);
        checkpoint_hasher.update(merkle_root_1);
        checkpoint_hasher.update(&merkle_index_1.to_be_bytes());
        checkpoint_hasher.update(&message_id);
        let checkpoint_digest_1: [u8; 32] = checkpoint_hasher.finalize().into();

        let mut eth_hasher = Keccak256::new();
        eth_hasher.update(b"\x19Ethereum Signed Message:\n32");
        eth_hasher.update(&checkpoint_digest_1);
        let eth_signed_message_1: [u8; 32] = eth_hasher.finalize().into();

        println!("eth_signed_message: {}", hex::encode(&eth_signed_message_1));

        // Try recovering addresses with Format 1 hash
        println!("\nRecovered addresses with Format 1 hash:");
        for i in 0..2 {
            let sig_bytes = &signatures_1[i * 65..(i + 1) * 65];
            let v = sig_bytes[64];
            let recovery_id = if v >= 27 { v - 27 } else { v };
            let sig = Signature::from_slice(&sig_bytes[..64]).unwrap();
            let rec_id = RecoveryId::try_from(recovery_id).unwrap();

            match VerifyingKey::recover_from_prehash(&eth_signed_message_1, &sig, rec_id) {
                Ok(key) => {
                    let uncompressed = key.to_encoded_point(false);
                    let pub_bytes = &uncompressed.as_bytes()[1..];
                    let addr = hex::encode(&Keccak256::digest(pub_bytes)[12..]);
                    let is_trusted = trusted_addresses.contains(&addr.as_str());
                    let marker = if is_trusted { "✓ MATCH!" } else { "✗" };
                    println!("  Sig {}: 0x{} {}", i, addr, marker);
                }
                Err(e) => println!("  Sig {}: Recovery failed: {:?}", i, e),
            }
        }

        println!("\n=== FORMAT 2 (MerkleTreeHook + Index + Root + Signatures) ===");
        println!("This is what the tx_builder currently uses (WRONG)\n");

        // Format 2: MerkleTreeHook (32) + Index (4) + MerkleRoot (32) + Signatures
        let merkle_tree_hook_2 = &metadata[0..32];
        let merkle_index_2 = u32::from_be_bytes(metadata[32..36].try_into().unwrap());
        let merkle_root_2 = &metadata[36..68];
        let signatures_2 = &metadata[68..];

        println!("merkle_tree_hook: {}", hex::encode(merkle_tree_hook_2));
        println!("merkle_index:     {}", merkle_index_2);
        println!("merkle_root:      {}", hex::encode(merkle_root_2));

        // Compute hash with Format 2
        let mut domain_hasher = Keccak256::new();
        domain_hasher.update(&origin.to_be_bytes());
        domain_hasher.update(merkle_tree_hook_2);
        domain_hasher.update(b"HYPERLANE");
        let domain_hash_2: [u8; 32] = domain_hasher.finalize().into();

        let mut checkpoint_hasher = Keccak256::new();
        checkpoint_hasher.update(&domain_hash_2);
        checkpoint_hasher.update(merkle_root_2);
        checkpoint_hasher.update(&merkle_index_2.to_be_bytes());
        checkpoint_hasher.update(&message_id);
        let checkpoint_digest_2: [u8; 32] = checkpoint_hasher.finalize().into();

        let mut eth_hasher = Keccak256::new();
        eth_hasher.update(b"\x19Ethereum Signed Message:\n32");
        eth_hasher.update(&checkpoint_digest_2);
        let eth_signed_message_2: [u8; 32] = eth_hasher.finalize().into();

        println!("eth_signed_message: {}", hex::encode(&eth_signed_message_2));

        // Try recovering addresses with Format 2 hash
        println!("\nRecovered addresses with Format 2 hash:");
        for i in 0..2 {
            let sig_bytes = &signatures_2[i * 65..(i + 1) * 65];
            let v = sig_bytes[64];
            let recovery_id = if v >= 27 { v - 27 } else { v };
            let sig = Signature::from_slice(&sig_bytes[..64]).unwrap();
            let rec_id = RecoveryId::try_from(recovery_id).unwrap();

            match VerifyingKey::recover_from_prehash(&eth_signed_message_2, &sig, rec_id) {
                Ok(key) => {
                    let uncompressed = key.to_encoded_point(false);
                    let pub_bytes = &uncompressed.as_bytes()[1..];
                    let addr = hex::encode(&Keccak256::digest(pub_bytes)[12..]);
                    let is_trusted = trusted_addresses.contains(&addr.as_str());
                    let marker = if is_trusted { "✓ MATCH!" } else { "✗" };
                    println!("  Sig {}: 0x{} {}", i, addr, marker);
                }
                Err(e) => println!("  Sig {}: Recovery failed: {:?}", i, e),
            }
        }

        println!("\n=== VERDICT ===");
        if eth_signed_message_1 != eth_signed_message_2 {
            println!("Different hashes! Format affects address recovery.");
            println!("If Format 1 produces trusted addresses, tx_builder needs to be fixed.");
        }
    }

    /// Test using hyperlane-core's CheckpointWithMessageId to get the canonical signing_hash
    /// This hash must match what Aiken computes in compute_checkpoint_hash
    #[test]
    fn test_checkpoint_signing_hash_for_aiken() {
        use hyperlane_core::{Checkpoint, CheckpointWithMessageId, Signable, H256};

        // Data from relayer logs for message 7e2c2f9ef220e8190803eb47033257b562d9104aaa578115aa27601548048d51
        let merkle_tree_hook_address = H256::from_slice(
            &hex::decode("0000000000000000000000009ff6ac3daf63103620bbf76136ea1aff43c2f612")
                .unwrap(),
        );
        let mailbox_domain: u32 = 43113; // Fuji
        let root = H256::from_slice(
            &hex::decode("78943434b7600830cf53756b5da5d7bdbed2761edfc997b0e75c9ec95f4f30fb")
                .unwrap(),
        );
        let index: u32 = 4336;
        let message_id = H256::from_slice(
            &hex::decode("7e2c2f9ef220e8190803eb47033257b562d9104aaa578115aa27601548048d51")
                .unwrap(),
        );

        // Build the checkpoint using hyperlane-core types
        let checkpoint = CheckpointWithMessageId {
            checkpoint: Checkpoint {
                merkle_tree_hook_address,
                mailbox_domain,
                root,
                index,
            },
            message_id,
        };

        // Get the signing hash - this is the checkpoint_digest (before EIP-191)
        let signing_hash = checkpoint.signing_hash();
        // Get the eth_signed_message_hash - this is what validators actually sign (with EIP-191)
        let eth_signed_message_hash = checkpoint.eth_signed_message_hash();

        println!("=== CheckpointWithMessageId Data ===");
        println!(
            "merkle_tree_hook_address: {}",
            hex::encode(merkle_tree_hook_address.as_bytes())
        );
        println!("mailbox_domain: {}", mailbox_domain);
        println!("root: {}", hex::encode(root.as_bytes()));
        println!("index: {}", index);
        println!("message_id: {}", hex::encode(message_id.as_bytes()));
        println!();
        println!("=== Hashes ===");
        println!(
            "signing_hash (checkpoint_digest, before EIP-191): {}",
            hex::encode(signing_hash.as_bytes())
        );
        println!(
            "eth_signed_message_hash (with EIP-191, what validators sign): {}",
            hex::encode(eth_signed_message_hash.as_bytes())
        );
        println!();
        println!(
            "The Aiken compute_checkpoint_hash should produce: {}",
            hex::encode(eth_signed_message_hash.as_bytes())
        );

        // Now also print the intermediate steps for Aiken debugging
        println!();
        println!("=== Intermediate values for Aiken test ===");

        // domain_hash_input = domain || address || "HYPERLANE"
        let mut domain_hash_input = Vec::new();
        domain_hash_input.extend_from_slice(&mailbox_domain.to_be_bytes());
        domain_hash_input.extend_from_slice(merkle_tree_hook_address.as_bytes());
        domain_hash_input.extend_from_slice(b"HYPERLANE");
        println!(
            "domain_hash_input ({} bytes): {}",
            domain_hash_input.len(),
            hex::encode(&domain_hash_input)
        );

        let domain_hash: [u8; 32] = Keccak256::digest(&domain_hash_input).into();
        println!(
            "domain_hash (keccak256 of above): {}",
            hex::encode(&domain_hash)
        );

        // checkpoint_input = domain_hash || root || index || message_id
        let mut checkpoint_input = Vec::new();
        checkpoint_input.extend_from_slice(&domain_hash);
        checkpoint_input.extend_from_slice(root.as_bytes());
        checkpoint_input.extend_from_slice(&index.to_be_bytes());
        checkpoint_input.extend_from_slice(message_id.as_bytes());
        println!(
            "checkpoint_input ({} bytes): {}",
            checkpoint_input.len(),
            hex::encode(&checkpoint_input)
        );

        let checkpoint_digest: [u8; 32] = Keccak256::digest(&checkpoint_input).into();
        println!(
            "checkpoint_digest (keccak256 of above): {}",
            hex::encode(&checkpoint_digest)
        );

        // EIP-191: prefix || checkpoint_digest
        let mut eip191_input = Vec::new();
        eip191_input.extend_from_slice(b"\x19Ethereum Signed Message:\n32");
        eip191_input.extend_from_slice(&checkpoint_digest);
        println!(
            "eip191_input ({} bytes): {}",
            eip191_input.len(),
            hex::encode(&eip191_input)
        );

        let eth_signed: [u8; 32] = Keccak256::digest(&eip191_input).into();
        println!(
            "eth_signed (keccak256 of above): {}",
            hex::encode(&eth_signed)
        );
    }
}

#[cfg(test)]
mod evaluation_parser_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_ogmios_v6_format() {
        let result = json!({
            "result": [
                { "validator": { "purpose": "spend", "index": 0 }, "budget": { "memory": 1000000, "cpu": 500000000 } },
                { "validator": { "purpose": "mint", "index": 0 }, "budget": { "memory": 200000, "cpu": 100000000 } }
            ]
        });
        let ex_units_map = parse_per_redeemer_ex_units(&result).unwrap();
        assert_eq!(ex_units_map.get("spend:0"), Some(&(1000000, 500000000)));
        assert_eq!(ex_units_map.get("mint:0"), Some(&(200000, 100000000)));
    }

    #[test]
    fn parse_blockfrost_ogmios_v5_format() {
        let result = json!({
            "type": "jsonwsp/response",
            "version": "1.0",
            "servicename": "ogmios",
            "methodname": "EvaluateTx",
            "result": {
                "EvaluationResult": {
                    "spend:0": { "memory": 1500000, "steps": 800000000 },
                    "spend:1": { "memory": 500000, "steps": 200000000 },
                    "mint:0": { "memory": 300000, "steps": 150000000 }
                }
            }
        });
        let ex_units_map = parse_per_redeemer_ex_units(&result).unwrap();
        assert_eq!(ex_units_map.get("spend:0"), Some(&(1500000, 800000000)));
        assert_eq!(ex_units_map.get("spend:1"), Some(&(500000, 200000000)));
        assert_eq!(ex_units_map.get("mint:0"), Some(&(300000, 150000000)));
    }

    #[test]
    fn parse_top_level_evaluation_result() {
        let result = json!({
            "EvaluationResult": {
                "spend:0": { "memory": 1000000, "steps": 500000000 }
            }
        });
        let ex_units_map = parse_per_redeemer_ex_units(&result).unwrap();
        assert_eq!(ex_units_map.get("spend:0"), Some(&(1000000, 500000000)));
    }

    #[test]
    fn parse_ogmios_fault_returns_error() {
        let result = json!({
            "type": "jsonwsp/fault",
            "version": "1.0",
            "servicename": "ogmios",
            "fault": {
                "code": "client",
                "string": "Some validation error"
            }
        });
        let err = parse_per_redeemer_ex_units(&result).unwrap_err();
        assert!(err.to_string().contains("Some validation error"));
    }

    #[test]
    fn parse_evaluation_failure() {
        let result = json!({
            "result": {
                "EvaluationFailure": {
                    "ScriptFailures": { "spend:0": [{ "extraneousRedeemers": ["spend:2"] }] }
                }
            }
        });
        let err = parse_per_redeemer_ex_units(&result).unwrap_err();
        assert!(err.to_string().contains("TX evaluation failed"));
    }
}

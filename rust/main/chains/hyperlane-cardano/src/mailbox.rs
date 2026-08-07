use crate::blockfrost_provider::{BlockfrostProvider, Utxo};
use crate::cardano::Keypair;
use crate::provider::CardanoProvider;
use crate::recipient_resolver::RecipientResolver;
use crate::tx_builder::{HyperlaneTxBuilder, ProcessTxComponents, TxBuilderError};
use crate::types::{script_hash_to_h256, MailboxDatum, MerkleTreeState};
use crate::ConnectionConf;
use async_trait::async_trait;
use hyperlane_core::accumulator::incremental::IncrementalMerkle;
use hyperlane_core::accumulator::TREE_DEPTH;
use hyperlane_core::{
    BatchItem, BatchResult, ChainCommunicationError, ChainResult, ContractLocator,
    FixedPointNumber, HyperlaneChain, HyperlaneContract, HyperlaneDomain, HyperlaneMessage,
    HyperlaneProvider, Mailbox, Metadata, QueueOperation, ReorgPeriod, TxCostEstimate, TxOutcome,
    H256, U256,
};
use serde_json::Value;
use std::fmt::{Debug, Formatter};
use std::sync::Arc;
use tokio::sync::OnceCell;
use tracing::{debug, info, warn};

pub struct CardanoMailbox {
    /// The mailbox minting policy hash - serves as both inbox and outbox address on Cardano
    pub outbox: H256,
    domain: HyperlaneDomain,
    provider: Arc<BlockfrostProvider>,
    conf: ConnectionConf,
    payer: Option<Keypair>,
    tx_builder: HyperlaneTxBuilder,
    resolver: RecipientResolver,
    /// Cached default ISM address. Initialized exactly once on the first successful fetch;
    /// concurrent callers await the first one rather than all fetching independently.
    cached_default_ism: Arc<OnceCell<H256>>,
}

impl CardanoMailbox {
    pub fn new(
        conf: &ConnectionConf,
        locator: ContractLocator,
        payer: Option<Keypair>,
    ) -> ChainResult<Self> {
        let provider = Arc::new(BlockfrostProvider::new(
            &conf.api_key,
            conf.network,
            conf.confirmation_block_delay,
        ));
        let tx_builder = HyperlaneTxBuilder::new(conf, provider.clone());
        let resolver = RecipientResolver::new(
            BlockfrostProvider::new(&conf.api_key, conf.network, conf.confirmation_block_delay),
            conf.warp_route_reference_script_utxo.clone(),
            conf.network,
        );

        Ok(CardanoMailbox {
            domain: locator.domain.clone(),
            outbox: locator.address,
            provider,
            conf: conf.clone(),
            payer,
            tx_builder,
            resolver,
            cached_default_ism: Arc::new(OnceCell::new()),
        })
    }

    /// Build the transaction components for processing a message
    ///
    /// This prepares all the UTXOs, redeemers, and datums needed for a Process transaction.
    /// The caller can use these components with pallas-txbuilder to construct the full transaction.
    /// For WarpRoute recipients, tokens are delivered directly to the recipient wallet.
    pub async fn build_process_tx_components(
        &self,
        message: &HyperlaneMessage,
        metadata: &[u8],
        payer: &crate::cardano::Keypair,
    ) -> ChainResult<ProcessTxComponents> {
        self.tx_builder
            .build_process_tx(message, metadata, payer, None)
            .await
            .map_err(ChainCommunicationError::from_other)
    }

    pub async fn finalized_block_number(&self) -> Result<u32, ChainCommunicationError> {
        let finalized_block_number = self
            .provider
            .get_latest_block()
            .await
            .map_err(ChainCommunicationError::from_other)?;
        Ok(finalized_block_number as u32)
    }

    /// Find the mailbox UTXO by its state NFT or script address
    ///
    /// First tries to find by NFT (preferred method). If no NFT is found,
    /// falls back to looking up UTXOs at the mailbox script address.
    async fn find_mailbox_utxo(&self) -> ChainResult<Utxo> {
        // First try to find by NFT (preferred method for production)
        // Asset name is configured from deployment info (e.g., "4d61696c626f78205374617465" for "Mailbox State")
        let mailbox_asset_name = &self.conf.mailbox_asset_name_hex;
        let nft_result = self
            .provider
            .find_utxo_by_nft(&self.conf.mailbox_policy_id, mailbox_asset_name)
            .await;

        match nft_result {
            Ok(utxo) => {
                info!(
                    "Found mailbox UTXO by NFT: {}#{}",
                    utxo.tx_hash, utxo.output_index
                );
                return Ok(utxo);
            }
            Err(e) => {
                // Log that NFT lookup failed, will try script address lookup
                info!(
                    "NFT lookup failed ({}), falling back to script address lookup",
                    e
                );
            }
        }

        // Fallback: Find UTXOs at the mailbox script address using the actual script hash
        let script_address = self
            .provider
            .script_hash_to_address(&self.conf.mailbox_script_hash)
            .map_err(|e| {
                ChainCommunicationError::from_other_str(&format!(
                    "Failed to compute mailbox script address: {e}"
                ))
            })?;

        info!(
            "Looking up mailbox UTXOs at script address: {}",
            script_address
        );

        let utxos = self
            .provider
            .get_utxos_at_address(&script_address)
            .await
            .map_err(|e| {
                ChainCommunicationError::from_other_str(&format!(
                    "Failed to get UTXOs at mailbox address: {e}"
                ))
            })?;

        // Find the UTXO holding the mailbox state NFT among all UTXOs at the script address.
        // The script address accumulates many UTXOs over time (e.g. message receipt UTXOs from
        // parallel inbound processing), so we must check for the specific state NFT rather than
        // picking any UTXO with an inline datum.
        for utxo in utxos {
            if utxo.has_asset(
                &self.conf.mailbox_policy_id,
                &self.conf.mailbox_asset_name_hex,
            ) && utxo.inline_datum.is_some()
            {
                info!(
                    "Found mailbox UTXO by script address: {}#{}",
                    utxo.tx_hash, utxo.output_index
                );
                return Ok(utxo);
            }
        }

        Err(ChainCommunicationError::from_other_str(
            "No mailbox UTXO found with inline datum at script address",
        ))
    }

    /// Parse mailbox datum from UTXO
    ///
    /// Handles both JSON-formatted datum and raw CBOR hex from Blockfrost.
    /// If inline_datum is CBOR hex, fetches JSON representation via data_hash.
    async fn parse_mailbox_datum(&self, utxo: &Utxo) -> ChainResult<MailboxDatum> {
        let inline_datum = utxo.inline_datum.as_ref().ok_or_else(|| {
            ChainCommunicationError::from_other_str("Mailbox UTXO has no inline datum")
        })?;

        // First try parsing as JSON (may already be JSON from some API responses)
        if let Ok(datum_json) = serde_json::from_str::<Value>(inline_datum) {
            return Self::parse_mailbox_datum_json(&datum_json);
        }

        // If inline_datum is CBOR hex (starts with hex chars), fetch JSON via data_hash
        let data_hash = utxo.data_hash.as_ref().ok_or_else(|| {
            ChainCommunicationError::from_other_str(
                "Mailbox UTXO has CBOR datum but no data_hash for JSON lookup",
            )
        })?;

        debug!("Fetching datum JSON via data_hash: {}", data_hash);
        let datum_json_str = self.provider.get_datum(data_hash).await.map_err(|e| {
            ChainCommunicationError::from_other_str(&format!("Failed to fetch datum JSON: {e}"))
        })?;

        let datum_json: Value = serde_json::from_str(&datum_json_str).map_err(|e| {
            ChainCommunicationError::from_other_str(&format!(
                "Failed to parse fetched datum JSON: {e}"
            ))
        })?;

        // Blockfrost wraps the datum in a `json_value` field
        let inner_json = datum_json.get("json_value").unwrap_or(&datum_json);

        Self::parse_mailbox_datum_json(inner_json)
    }

    /// Parse mailbox datum from Blockfrost's JSON format
    /// Parse a `MailboxDatum` from Blockfrost's Plutus JSON.
    ///
    /// Field order matches the Aiken type, `version` first:
    /// `[version, local_domain, default_ism, owner, outbound_nonce,
    /// merkle_tree, processed_tree_root]`.
    fn parse_mailbox_datum_json(json: &Value) -> ChainResult<MailboxDatum> {
        // Blockfrost returns datum as JSON with Plutus data structure
        // Format: { "fields": [...], "constructor": N }
        let fields = json
            .get("fields")
            .and_then(|f| f.as_array())
            .ok_or_else(|| {
                ChainCommunicationError::from_other_str("Invalid mailbox datum: missing fields")
            })?;

        if fields.len() < 5 {
            return Err(ChainCommunicationError::from_other_str(
                "Invalid mailbox datum: insufficient fields (need at least 5)",
            ));
        }

        // Parse local_domain (field 1 — field 0 is the datum layout version)
        let local_domain = fields
            .get(1)
            .and_then(|f| f.get("int"))
            .and_then(|i| i.as_u64())
            .ok_or_else(|| {
                ChainCommunicationError::from_other_str("Invalid local_domain in mailbox datum")
            })? as u32;

        // Parse default_ism (field 2) - 28-byte script hash
        let default_ism_hex = fields
            .get(2)
            .and_then(|f| f.get("bytes"))
            .and_then(|b| b.as_str())
            .ok_or_else(|| {
                ChainCommunicationError::from_other_str("Invalid default_ism in mailbox datum")
            })?;
        let default_ism_bytes = hex::decode(default_ism_hex).map_err(|e| {
            ChainCommunicationError::from_other_str(&format!("Failed to decode default_ism: {e}"))
        })?;
        let default_ism: [u8; 28] = default_ism_bytes
            .try_into()
            .map_err(|_| ChainCommunicationError::from_other_str("Invalid default_ism length"))?;

        // Parse owner (field 3) - 28-byte verification key hash
        let owner_hex = fields
            .get(3)
            .and_then(|f| f.get("bytes"))
            .and_then(|b| b.as_str())
            .ok_or_else(|| {
                ChainCommunicationError::from_other_str("Invalid owner in mailbox datum")
            })?;
        let owner_bytes = hex::decode(owner_hex).map_err(|e| {
            ChainCommunicationError::from_other_str(&format!("Failed to decode owner: {e}"))
        })?;
        let owner: [u8; 28] = owner_bytes
            .try_into()
            .map_err(|_| ChainCommunicationError::from_other_str("Invalid owner length"))?;

        // Parse outbound_nonce (field 4)
        let outbound_nonce = fields
            .get(4)
            .and_then(|f| f.get("int"))
            .and_then(|i| i.as_u64())
            .ok_or_else(|| {
                ChainCommunicationError::from_other_str("Invalid outbound_nonce in mailbox datum")
            })? as u32;

        // Parse merkle_tree (field 5) - nested MerkleTreeState structure
        // Format: { "constructor": 0, "fields": [{ "list": [...branches...] }, { "int": count }] }
        let merkle_tree = Self::parse_merkle_tree_state(fields.get(5).ok_or_else(|| {
            ChainCommunicationError::from_other_str("Missing merkle_tree in mailbox datum")
        })?)?;

        let processed_tree_root = fields
            .get(6)
            .and_then(|v| v.get("bytes"))
            .and_then(|v| v.as_str())
            .and_then(|s| hex::decode(s).ok())
            .and_then(|bytes| {
                if bytes.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(&bytes);
                    Some(arr)
                } else {
                    None
                }
            })
            .unwrap_or(crate::smt::EMPTY_ROOT);

        Ok(MailboxDatum {
            local_domain,
            default_ism,
            owner,
            outbound_nonce,
            merkle_tree,
            processed_tree_root,
        })
    }

    /// Parse MerkleTreeState from Blockfrost's JSON format
    fn parse_merkle_tree_state(json: &Value) -> ChainResult<MerkleTreeState> {
        // MerkleTreeState format: { "constructor": 0, "fields": [branches_list, count] }
        let fields = json
            .get("fields")
            .and_then(|f| f.as_array())
            .ok_or_else(|| {
                ChainCommunicationError::from_other_str(
                    "Invalid merkle_tree: missing fields in MerkleTreeState",
                )
            })?;

        if fields.len() < 2 {
            return Err(ChainCommunicationError::from_other_str(
                "Invalid merkle_tree: insufficient fields in MerkleTreeState",
            ));
        }

        // Parse branches (field 0) - list of 32-byte hashes
        let branches_list = fields
            .first()
            .and_then(|f| f.get("list"))
            .and_then(|l| l.as_array())
            .ok_or_else(|| {
                ChainCommunicationError::from_other_str(
                    "Invalid merkle_tree: missing branches list",
                )
            })?;

        let mut branches = Vec::with_capacity(branches_list.len());
        for (i, branch_item) in branches_list.iter().enumerate() {
            let branch_hex = branch_item
                .get("bytes")
                .and_then(|b| b.as_str())
                .ok_or_else(|| {
                    ChainCommunicationError::from_other_str(&format!(
                        "Invalid merkle_tree: invalid branch at index {i}"
                    ))
                })?;
            let branch_bytes = hex::decode(branch_hex).map_err(|e| {
                ChainCommunicationError::from_other_str(&format!(
                    "Failed to decode branch {i}: {e}"
                ))
            })?;
            let branch: [u8; 32] = branch_bytes.try_into().map_err(|_| {
                ChainCommunicationError::from_other_str(&format!(
                    "Invalid branch length at index {i}"
                ))
            })?;
            branches.push(branch);
        }

        // Parse count (field 1)
        let count = fields
            .get(1)
            .and_then(|f| f.get("int"))
            .and_then(|i| i.as_u64())
            .ok_or_else(|| {
                ChainCommunicationError::from_other_str("Invalid merkle_tree: missing count")
            })? as u32;

        Ok(MerkleTreeState { branches, count })
    }

    /// The merkle tree as the mailbox holds it *right now*, with the indexing
    /// tip. Use only where the live view is what is wanted; anything that gets
    /// signed or attested must go through [`Self::tree_at_reorg_period`].
    pub async fn live_tree_and_tip(&self) -> ChainResult<(IncrementalMerkle, u32)> {
        // Fetch mailbox UTXO and tip in parallel (independent queries)
        let (utxo, tip) =
            tokio::try_join!(self.find_mailbox_utxo(), self.finalized_block_number(),)?;
        let datum = self.parse_mailbox_datum(&utxo).await?;

        Ok((Self::tree_from_datum(&datum), tip))
    }

    /// How many blocks behind the tip to read, given a configured reorg period.
    ///
    /// Never shallower than `confirmation_block_delay`: below that the
    /// provider's asset index may not have caught up, so a shallower read
    /// would fail to find state that exists rather than read older state.
    ///
    /// Ouroboros Praos offers no finality tags, so a `Tag` reorg period is
    /// rejected rather than silently reinterpreted as some block count.
    fn read_depth(reorg_period: &ReorgPeriod, confirmation_block_delay: u32) -> ChainResult<u32> {
        let blocks = match reorg_period {
            ReorgPeriod::None => 0,
            ReorgPeriod::Blocks(blocks) => blocks.get(),
            ReorgPeriod::Tag(_) => {
                return Err(ChainCommunicationError::InvalidReorgPeriod(
                    reorg_period.clone(),
                ))
            }
        };
        Ok(blocks.max(confirmation_block_delay))
    }

    /// The merkle tree as of `reorg_period` blocks behind the tip, paired with
    /// the height the state actually came from.
    ///
    /// Reading behind the tip is what makes a signed checkpoint safe: a root
    /// taken from the live tip can be rolled back out of existence after it has
    /// been signed, and a signature cannot be withdrawn.
    pub async fn tree_at_reorg_period(
        &self,
        reorg_period: &ReorgPeriod,
    ) -> ChainResult<(IncrementalMerkle, u32)> {
        let depth = Self::read_depth(reorg_period, self.conf.confirmation_block_delay)?;
        let tip = self
            .provider
            .get_tip()
            .await
            .map_err(ChainCommunicationError::from_other)?;
        self.tree_at_height(tip.saturating_sub(depth as u64)).await
    }

    /// The merkle tree as the mailbox held it at `height`.
    ///
    /// Cardano exposes only the current UTXO set, so past state has to be
    /// reconstructed: find the last transaction that moved the mailbox state
    /// NFT at or before `height`, and read the datum it produced. This is the
    /// hand-rolled equivalent of an `eth_call` pinned to a block.
    pub async fn tree_at_height(&self, height: u64) -> ChainResult<(IncrementalMerkle, u32)> {
        let asset = format!(
            "{}{}",
            self.conf.mailbox_policy_id, self.conf.mailbox_asset_name_hex
        );

        let tx = self
            .provider
            .find_asset_tx_at_or_before(&asset, height)
            .await
            .map_err(ChainCommunicationError::from_other)?
            .ok_or_else(|| {
                ChainCommunicationError::from_other_str(&format!(
                    "No mailbox state found at or before block {height}"
                ))
            })?;

        let utxos = self
            .provider
            .get_transaction_utxos(&tx.tx_hash)
            .await
            .map_err(ChainCommunicationError::from_other)?;

        let utxo = utxos
            .outputs
            .into_iter()
            .find(|output| output.value.iter().any(|value| value.unit == asset))
            .ok_or_else(|| {
                ChainCommunicationError::from_other_str(&format!(
                    "Transaction {} moved the mailbox state NFT but produced no output holding it",
                    tx.tx_hash
                ))
            })?;

        let datum = self.parse_mailbox_datum(&utxo).await?;
        debug!(
            height = tx.block_height,
            requested = height,
            count = datum.merkle_tree.count,
            "Read mailbox state behind the tip"
        );

        Ok((Self::tree_from_datum(&datum), tx.block_height as u32))
    }

    fn tree_from_datum(datum: &MailboxDatum) -> IncrementalMerkle {
        let mut branch = [H256::zero(); TREE_DEPTH];
        for (i, datum_branch) in datum.merkle_tree.branches.iter().enumerate() {
            if i < TREE_DEPTH {
                branch[i] = H256::from_slice(datum_branch);
            }
        }
        IncrementalMerkle::new(branch, datum.merkle_tree.count as usize)
    }
}

impl HyperlaneContract for CardanoMailbox {
    fn address(&self) -> H256 {
        // On Cardano, this represents the mailbox minting policy hash
        self.outbox
    }
}

impl HyperlaneChain for CardanoMailbox {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(CardanoProvider::new(&self.conf, self.domain.clone()))
    }
}

impl Debug for CardanoMailbox {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self as &dyn HyperlaneContract)
    }
}

#[async_trait]
impl Mailbox for CardanoMailbox {
    async fn count(&self, reorg_period: &ReorgPeriod) -> ChainResult<u32> {
        self.tree_at_reorg_period(reorg_period)
            .await
            .map(|(tree, _)| tree.count() as u32)
    }

    async fn delivered(&self, id: H256) -> ChainResult<bool> {
        let message_id_bytes: [u8; 32] = id.0;
        self.tx_builder
            .is_delivered(&message_id_bytes)
            .await
            .map_err(ChainCommunicationError::from_other)
    }

    async fn default_ism(&self) -> ChainResult<H256> {
        // Fetch exactly once; concurrent callers await the first successful result.
        self.cached_default_ism
            .get_or_try_init(|| async {
                let utxo = self.find_mailbox_utxo().await?;
                let datum = self.parse_mailbox_datum(&utxo).await?;

                let mut h = [0u8; 32];
                h[0] = 0x02;
                h[4..32].copy_from_slice(&datum.default_ism);
                Ok::<H256, ChainCommunicationError>(H256(h))
            })
            .await
            .copied()
    }

    async fn recipient_ism(&self, recipient: H256) -> ChainResult<H256> {
        let recipient_bytes: [u8; 32] = recipient.into();
        match self.resolver.resolve(&recipient_bytes).await {
            Ok(resolved) => {
                if let Some(ism) = resolved.ism {
                    return Ok(script_hash_to_h256(&ism.script_hash));
                }
            }
            Err(e) => {
                debug!("Could not resolve recipient ISM, using default: {e}");
            }
        }
        self.default_ism().await
    }

    async fn process(
        &self,
        message: &HyperlaneMessage,
        metadata: &Metadata,
        _tx_gas_limit: Option<U256>,
    ) -> ChainResult<TxOutcome> {
        // Check if we have a payer keypair (required for signing)
        let payer = self.payer.as_ref().ok_or_else(|| {
            ChainCommunicationError::from_other_str(
                "No payer keypair configured for Cardano mailbox. \
                 Set a payer keypair to enable message processing.",
            )
        })?;

        info!(
            "Processing Hyperlane message nonce {} from origin {} to destination {}",
            message.nonce, message.origin, message.destination
        );

        // Build, sign, and submit the process transaction
        let outcome = self
            .tx_builder
            .build_and_submit_process_tx(message, metadata, payer)
            .await
            .map_err(|e| match &e {
                TxBuilderError::UndeliverableMessage(_) => {
                    ChainCommunicationError::SimulationFailed(e.to_string())
                }
                _ => ChainCommunicationError::from_other(e),
            })?;

        info!(
            "Message processed successfully. Transaction: {:?}",
            outcome.transaction_id
        );

        Ok(outcome)
    }

    fn supports_batching(&self) -> bool {
        false
    }

    async fn process_batch<'a>(&self, ops: Vec<&'a QueueOperation>) -> ChainResult<BatchResult> {
        let payer = self.payer.as_ref().ok_or_else(|| {
            ChainCommunicationError::from_other_str(
                "No payer keypair configured for batch processing",
            )
        })?;

        let items: Vec<BatchItem<HyperlaneMessage>> = ops
            .iter()
            .map(|op| op.try_batch())
            .collect::<ChainResult<_>>()?;

        let max = self.conf.max_batch_size as usize;
        let batch_items: Vec<_> = items.iter().take(max).collect();

        info!(
            "Processing batch of {} messages (max {})",
            batch_items.len(),
            max
        );

        let messages: Vec<(&HyperlaneMessage, &[u8])> = batch_items
            .iter()
            .map(|item| (&item.data, item.submission_data.metadata.as_ref()))
            .collect();

        let results = self
            .tx_builder
            .build_and_submit_chained_process_txs(&messages, payer)
            .await
            .map_err(ChainCommunicationError::from_other)?;

        let mut failed_indexes: Vec<usize> = results
            .iter()
            .enumerate()
            .filter_map(|(i, r)| if r.is_err() { Some(i) } else { None })
            .collect();

        // Messages beyond max_batch_size are also "failed" (not attempted)
        for i in max..ops.len() {
            failed_indexes.push(i);
        }

        let outcome = results.into_iter().find_map(|r| r.ok());

        Ok(BatchResult {
            outcome,
            failed_indexes,
        })
    }

    async fn process_estimate_costs(
        &self,
        message: &HyperlaneMessage,
        metadata: &Metadata,
    ) -> ChainResult<TxCostEstimate> {
        // Cardano gas is denominated 1:1 in lovelace: gas_price = 1, so a Hyperlane
        // `gasLimit` reads directly as "lovelace of estimated Cardano delivery cost"
        // (no opaque bytes/44 scaling). The origin IGP converts lovelace -> origin
        // token via the ADA/origin-token exchange rate. The relayer's
        // onChainFeeQuoting policy compares this gas_limit against the gas_amount
        // from payForGas — both are lovelace, so they are directly commensurable.
        let one_lovelace_price = FixedPointNumber::try_from(U256::from(1u64)).map_err(|e| {
            ChainCommunicationError::from_other_str(&format!("Failed to convert gas price: {e}"))
        })?;

        // Try dynamic estimation via Blockfrost TX evaluation
        if let Some(payer) = self.payer.as_ref() {
            match self
                .tx_builder
                .estimate_process_cost(message, metadata, payer)
                .await
            {
                Ok(estimated_lovelace) => {
                    info!(
                        "Dynamic cost estimate for nonce {}: {} lovelace",
                        message.nonce, estimated_lovelace
                    );
                    return Ok(TxCostEstimate {
                        gas_limit: U256::from(estimated_lovelace),
                        gas_price: one_lovelace_price,
                        l2_gas_limit: None,
                    });
                }
                Err(e) => {
                    if matches!(e, TxBuilderError::UndeliverableMessage(_)) {
                        warn!(
                            "Nonce {} is permanently undeliverable: {}",
                            message.nonce, e
                        );
                        return Err(ChainCommunicationError::SimulationFailed(e.to_string()));
                    }
                    debug!(
                        "Dynamic cost estimation unavailable for nonce {}, using static fallback: {}",
                        message.nonce, e
                    );
                }
            }
        }

        // Fallback (Blockfrost evaluate unavailable): conservative static estimate,
        // in lovelace, by recipient type.
        let recipient_bytes = message.recipient.as_bytes();
        let estimated_lovelace = if recipient_bytes.first() == Some(&0x01) {
            // Warp routes: ledger fee + recipient token-output minUTxO the relayer
            // fronts on a mint/release. 3 ADA conservatively covers both.
            3_000_000u64
        } else {
            // Script recipients: fee + verified_message UTXO.
            // The verified_message UTXO stores the full body in its inline datum
            // and grows at ~4400 lovelace/byte (coins_per_utxo_byte + CBOR overhead).
            let body_len = message.body.len() as u64;
            4_000_000 + 4_400 * body_len
        };

        Ok(TxCostEstimate {
            gas_limit: U256::from(estimated_lovelace),
            gas_price: one_lovelace_price,
            l2_gas_limit: None,
        })
    }

    async fn process_calldata(
        &self,
        message: &HyperlaneMessage,
        metadata: &Metadata,
    ) -> ChainResult<Vec<u8>> {
        // Encode the message in Hyperlane wire format
        let mut message_bytes = Vec::new();
        message_bytes.extend_from_slice(&[message.version]);
        message_bytes.extend_from_slice(&message.nonce.to_be_bytes());
        message_bytes.extend_from_slice(&message.origin.to_be_bytes());
        message_bytes.extend_from_slice(message.sender.as_bytes());
        message_bytes.extend_from_slice(&message.destination.to_be_bytes());
        message_bytes.extend_from_slice(message.recipient.as_bytes());
        message_bytes.extend_from_slice(&message.body);

        // Create CardanoTxCalldata structure expected by the lander adapter
        // This must be JSON-serialized for serde_json::from_slice in the adapter
        let calldata = serde_json::json!({
            "message": message_bytes,
            "metadata": metadata.to_vec(),
        });

        serde_json::to_vec(&calldata).map_err(|e| {
            ChainCommunicationError::from_other_str(&format!(
                "Failed to serialize CardanoTxCalldata: {e}"
            ))
        })
    }

    fn delivered_calldata(&self, message_id: H256) -> ChainResult<Option<Vec<u8>>> {
        // Return the message_id as calldata for delivery check
        Ok(Some(message_id.as_bytes().to_vec()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyperlane_core::accumulator::INITIAL_ROOT;
    use serde_json::json;

    const TEST_DEFAULT_ISM: &str = "1111111111111111111111111111111111111111111111111111111a";
    const TEST_OWNER: &str = "2222222222222222222222222222222222222222222222222222222b";
    const TEST_SMT_ROOT: &str = "3333333333333333333333333333333333333333333333333333333333333333";

    /// A mailbox datum exactly as the Aiken type lays it out, `version` first.
    ///
    /// `version` is a parameter rather than a constant so a test can give it a
    /// value distinguishable from `local_domain` — the two are adjacent ints,
    /// and reading one where the other belongs is otherwise invisible.
    fn create_test_mailbox_datum_json(
        version: u32,
        local_domain: u32,
        outbound_nonce: u32,
        branches: &[[u8; 32]],
        merkle_count: u32,
    ) -> serde_json::Value {
        let branches_list: Vec<_> = branches
            .iter()
            .map(|b| json!({"bytes": hex::encode(b)}))
            .collect();

        json!({
            "constructor": 0,
            "fields": [
                {"int": version},
                {"int": local_domain},
                {"bytes": TEST_DEFAULT_ISM},
                {"bytes": TEST_OWNER},
                {"int": outbound_nonce},
                {
                    "constructor": 0,
                    "fields": [
                        {"list": branches_list},
                        {"int": merkle_count}
                    ]
                },
                {"bytes": TEST_SMT_ROOT}
            ]
        })
    }

    fn parse(datum: &serde_json::Value) -> MailboxDatum {
        CardanoMailbox::parse_mailbox_datum_json(datum).expect("datum should parse")
    }

    /// Helper to create zero branches (32 zero hashes)
    fn zero_branches() -> Vec<[u8; 32]> {
        vec![[0u8; 32]; TREE_DEPTH]
    }

    /// The depth a checkpoint read backs off by. Getting this wrong is not a
    /// wrong number on a dashboard — too shallow means signing a root that can
    /// still be rolled back out from under the signature.
    mod read_depth {
        use super::*;
        use std::num::NonZeroU32;

        const DELAY: u32 = 5;

        #[test]
        fn configured_period_is_honored_when_deeper_than_the_indexing_delay() {
            let period = ReorgPeriod::Blocks(NonZeroU32::new(20).unwrap());
            assert_eq!(CardanoMailbox::read_depth(&period, DELAY).unwrap(), 20);
        }

        /// Below the indexing delay the provider may not have the block yet, so
        /// a shallower read fails to find state rather than reading older state.
        #[test]
        fn indexing_delay_is_the_floor() {
            let period = ReorgPeriod::Blocks(NonZeroU32::new(2).unwrap());
            assert_eq!(CardanoMailbox::read_depth(&period, DELAY).unwrap(), DELAY);
        }

        #[test]
        fn absent_period_still_backs_off_by_the_delay() {
            assert_eq!(
                CardanoMailbox::read_depth(&ReorgPeriod::None, DELAY).unwrap(),
                DELAY
            );
        }

        /// Praos has no finality tags. Accepting one would mean silently
        /// choosing a block count the operator never asked for.
        #[test]
        fn finality_tags_are_rejected_rather_than_reinterpreted() {
            let period = ReorgPeriod::Tag("finalized".into());
            assert!(CardanoMailbox::read_depth(&period, DELAY).is_err());
        }
    }

    /// Every field, read through the real parser. Asserting on all of them at
    /// once is what makes a layout shift show up: adding a field moves each
    /// subsequent one, and a test that checks a single field would keep passing
    /// for the fields it does not look at.
    #[test]
    fn parses_every_field_at_its_own_offset() {
        let mut branches = zero_branches();
        branches[0] = [0xab; 32];

        let datum = parse(&create_test_mailbox_datum_json(0, 2003, 7, &branches, 42));

        assert_eq!(datum.local_domain, 2003);
        assert_eq!(
            datum.default_ism,
            hex::decode(TEST_DEFAULT_ISM).unwrap()[..]
        );
        assert_eq!(datum.owner, hex::decode(TEST_OWNER).unwrap()[..]);
        assert_eq!(datum.outbound_nonce, 7);
        assert_eq!(datum.merkle_tree.count, 42);
        assert_eq!(datum.merkle_tree.branches[0], [0xab; 32]);
        assert_eq!(
            datum.processed_tree_root,
            hex::decode(TEST_SMT_ROOT).unwrap()[..]
        );
    }

    /// The regression this file previously had no guard for. `version` was
    /// added ahead of `local_domain`, and the parser kept reading the domain
    /// from field 0 — silently yielding the version number instead.
    ///
    /// A version equal to the domain would let that bug pass, so they differ.
    #[test]
    fn local_domain_is_read_past_the_version_field() {
        let datum = parse(&create_test_mailbox_datum_json(
            0,
            2003,
            0,
            &zero_branches(),
            0,
        ));

        assert_eq!(
            datum.local_domain, 2003,
            "local_domain must come from field 1; field 0 is the datum layout version"
        );
    }

    /// A future migration bumps `version` without touching anything else, so a
    /// parser anchored to the wrong field would start returning the new version
    /// as the domain.
    #[test]
    fn bumping_the_version_does_not_change_the_domain() {
        let at_v0 = parse(&create_test_mailbox_datum_json(
            0,
            2003,
            0,
            &zero_branches(),
            0,
        ));
        let at_v1 = parse(&create_test_mailbox_datum_json(
            1,
            2003,
            0,
            &zero_branches(),
            0,
        ));

        assert_eq!(at_v0.local_domain, at_v1.local_domain);
    }

    /// Datums shorter than the current layout are rejected rather than parsed
    /// into whatever the offsets happen to land on.
    #[test]
    fn a_truncated_datum_is_rejected() {
        let truncated = json!({
            "constructor": 0,
            "fields": [{"int": 0}, {"int": 2003}, {"bytes": TEST_DEFAULT_ISM}]
        });

        assert!(CardanoMailbox::parse_mailbox_datum_json(&truncated).is_err());
    }

    #[test]
    fn test_empty_tree_has_initial_root() {
        // For an empty tree (count = 0), the root should be the INITIAL_ROOT
        // This is the keccak256 merkle root of an empty tree with 32 levels of zero hashes
        let initial_root_hex = "27ae5ba08d7291c96c8cbddcc148bf48a6d68c7974b94356f53754ef6171d757";

        // Verify INITIAL_ROOT matches expected value
        assert_eq!(
            hex::encode(INITIAL_ROOT.as_bytes()),
            initial_root_hex,
            "INITIAL_ROOT constant should match expected empty tree root"
        );

        // Also verify that an IncrementalMerkle with zero branches computes this root
        let empty_tree = IncrementalMerkle::default();
        assert_eq!(empty_tree.root(), INITIAL_ROOT);
    }

    #[test]
    fn test_incremental_merkle_with_real_branches_produces_correct_root() {
        // This test verifies that when we store real branches in the datum,
        // tree.root() produces the correct merkle root

        // Simulate inserting a message into a tree
        let mut real_tree = IncrementalMerkle::default();
        let message_id = H256::from_slice(&[0xab; 32]);
        real_tree.ingest(message_id);

        let real_root = real_tree.root();
        let real_branches = real_tree.branch().clone();
        let count = real_tree.count();

        // Now create a new tree from the stored branches (simulating datum parsing)
        let restored_tree = IncrementalMerkle::new(real_branches, count);

        // The restored tree should compute the SAME root
        assert_eq!(
            restored_tree.root(),
            real_root,
            "Tree restored from branches should have same root"
        );

        // And it should NOT equal the empty tree root
        assert_ne!(real_root, INITIAL_ROOT);
    }

    #[test]
    fn test_merkle_root_h256_conversion() {
        // Test that we can convert between hex string and H256 correctly
        let root_hex = "27ae5ba08d7291c96c8cbddcc148bf48a6d68c7974b94356f53754ef6171d757";
        let root_bytes = hex::decode(root_hex).unwrap();

        let h256_root = H256::from_slice(&root_bytes);

        assert_eq!(hex::encode(h256_root.as_bytes()), root_hex);
        assert_eq!(h256_root, INITIAL_ROOT);
    }

    #[test]
    fn test_checkpoint_index_calculation() {
        // Test that checkpoint index is count - 1 (0-indexed)
        // Empty tree (count=0) should have index 0 (saturating_sub prevents underflow)
        assert_eq!(0u32.saturating_sub(1), 0);

        // Tree with 1 message should have index 0
        assert_eq!(1u32.saturating_sub(1), 0);

        // Tree with 5 messages should have index 4
        assert_eq!(5u32.saturating_sub(1), 4);
    }

    #[test]
    fn test_branch_to_h256_conversion() {
        // Test converting branch bytes from datum to H256 for IncrementalMerkle
        let branch_bytes: [u8; 32] = [0xab; 32];
        let h256_branch = H256::from_slice(&branch_bytes);

        assert_eq!(h256_branch.as_bytes(), &branch_bytes);
    }
}

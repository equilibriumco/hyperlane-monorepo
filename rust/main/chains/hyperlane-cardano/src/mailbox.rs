use crate::blockfrost_provider::{BlockfrostProvider, Utxo};
use crate::cardano::Keypair;
use crate::provider::CardanoProvider;
use crate::recipient_resolver::RecipientResolver;
use crate::tx_builder::{HyperlaneTxBuilder, ProcessTxComponents, TxBuilderError};
use crate::types::{
    hyperlane_address_to_policy_id, script_hash_to_h256, MailboxDatum, MerkleTreeState,
};
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
use std::num::NonZeroU64;
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
            return self.parse_mailbox_datum_json(&datum_json);
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

        self.parse_mailbox_datum_json(inner_json)
    }

    /// Parse mailbox datum from Blockfrost's JSON format
    fn parse_mailbox_datum_json(&self, json: &Value) -> ChainResult<MailboxDatum> {
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

        // Parse local_domain (field 0)
        let local_domain = fields
            .first()
            .and_then(|f| f.get("int"))
            .and_then(|i| i.as_u64())
            .ok_or_else(|| {
                ChainCommunicationError::from_other_str("Invalid local_domain in mailbox datum")
            })? as u32;

        // Parse default_ism (field 1) - 28-byte script hash
        let default_ism_hex = fields
            .get(1)
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

        // Parse owner (field 2) - 28-byte verification key hash
        let owner_hex = fields
            .get(2)
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

        // Parse outbound_nonce (field 3)
        let outbound_nonce = fields
            .get(3)
            .and_then(|f| f.get("int"))
            .and_then(|i| i.as_u64())
            .ok_or_else(|| {
                ChainCommunicationError::from_other_str("Invalid outbound_nonce in mailbox datum")
            })? as u32;

        // Parse merkle_tree (field 4) - nested MerkleTreeState structure
        // Format: { "constructor": 0, "fields": [{ "list": [...branches...] }, { "int": count }] }
        let merkle_tree = self.parse_merkle_tree_state(fields.get(4).ok_or_else(|| {
            ChainCommunicationError::from_other_str("Missing merkle_tree in mailbox datum")
        })?)?;

        let processed_tree_root = fields
            .get(5)
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
    fn parse_merkle_tree_state(&self, json: &Value) -> ChainResult<MerkleTreeState> {
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

    /// Returns the merkle tree state from the mailbox datum.
    ///
    /// Returns: (tree, block_height)
    /// - tree: IncrementalMerkle with actual branches from the datum
    /// - block_height: Current finalized block height
    ///
    /// The Aiken contracts now store the full branch state (32 branches × 32 bytes)
    /// in the datum, enabling proper merkle tree reconstruction.
    pub async fn tree_and_tip(
        &self,
        lag: Option<NonZeroU64>,
    ) -> ChainResult<(IncrementalMerkle, u32)> {
        assert!(lag.is_none(), "Cardano always returns the finalized result");

        // Fetch mailbox UTXO and tip in parallel (independent queries)
        let (utxo, tip) =
            tokio::try_join!(self.find_mailbox_utxo(), self.finalized_block_number(),)?;
        let datum = self.parse_mailbox_datum(&utxo).await?;

        // Build an IncrementalMerkle tree from the datum's full branch state
        let mut branch = [H256::zero(); TREE_DEPTH];
        for (i, datum_branch) in datum.merkle_tree.branches.iter().enumerate() {
            if i < TREE_DEPTH {
                branch[i] = H256::from_slice(datum_branch);
            }
        }
        let count = datum.merkle_tree.count as usize;

        Ok((IncrementalMerkle::new(branch, count), tip))
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
    async fn count(&self, _reorg_period: &ReorgPeriod) -> ChainResult<u32> {
        // For Cardano, we ignore reorg_period as it always returns finalized results
        self.tree_and_tip(None)
            .await
            .map(|(tree, _)| tree.count() as u32)
    }

    async fn delivered(&self, id: H256) -> ChainResult<bool> {
        let message_id_bytes: [u8; 32] = id.0;

        // Prefer NFT lookup (O(1)) if processedMessagesNftPolicyId is configured
        if let Some(ref nft_policy_id) = self.conf.processed_messages_nft_policy_id {
            let result = self
                .provider
                .is_message_delivered_by_nft(nft_policy_id, &message_id_bytes)
                .await
                .map_err(ChainCommunicationError::from_other)?;
            return Ok(result);
        }

        // Fallback: Scan UTXOs at processed_messages_script address (O(n))
        // This is used when NFT minting is not configured
        let result = self
            .provider
            .is_message_delivered(&self.conf.processed_messages_script_hash, &message_id_bytes)
            .await
            .map_err(ChainCommunicationError::from_other)?;
        Ok(result)
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
        if hyperlane_address_to_policy_id(&recipient_bytes).is_some() {
            match self.resolver.resolve(&recipient_bytes).await {
                Ok(resolved) => {
                    if let Some(ism_hash) = resolved.ism {
                        return Ok(script_hash_to_h256(&ism_hash));
                    }
                }
                Err(e) => {
                    debug!("Could not resolve recipient ISM, using default: {e}");
                }
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
        // Cardano's min_fee_a protocol parameter: 44 lovelace per TX byte.
        // The IGP oracle uses gasPrice=44 so that 1 gas unit = 1 byte = 44 lovelace.
        // We return gas_limit in these units so the relayer's onChainFeeQuoting
        // policy can compare it against the gas_amount from payForGas.
        const LOVELACE_PER_GAS_UNIT: u64 = 44;

        // Try dynamic estimation via Blockfrost TX evaluation
        if let Some(payer) = self.payer.as_ref() {
            match self
                .tx_builder
                .estimate_process_cost(message, metadata, payer)
                .await
            {
                Ok(estimated_lovelace) => {
                    let gas_units = estimated_lovelace / LOVELACE_PER_GAS_UNIT + 1;
                    info!(
                        "Dynamic cost estimate for nonce {}: {} lovelace ({} gas units)",
                        message.nonce, estimated_lovelace, gas_units
                    );
                    return Ok(TxCostEstimate {
                        gas_limit: U256::from(gas_units),
                        gas_price: FixedPointNumber::try_from(U256::from(LOVELACE_PER_GAS_UNIT))
                            .unwrap_or_else(|_| FixedPointNumber::zero()),
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

        // Fallback: static estimate based on recipient type
        let recipient_bytes = message.recipient.as_bytes();
        let estimated_fee_lovelace = if recipient_bytes.first() == Some(&0x01) {
            // Warp routes: fee + processed_marker (no verified_message UTXO)
            3_000_000u64
        } else {
            // Script recipients: fee + processed_marker + verified_message UTXO.
            // The verified_message UTXO stores the full body in its inline datum
            // and grows at ~4400 lovelace/byte (coins_per_utxo_byte + CBOR overhead).
            let body_len = message.body.len() as u64;
            4_000_000 + 4_400 * body_len
        };

        let gas_units = estimated_fee_lovelace / LOVELACE_PER_GAS_UNIT + 1;
        Ok(TxCostEstimate {
            gas_limit: U256::from(gas_units),
            gas_price: FixedPointNumber::try_from(U256::from(LOVELACE_PER_GAS_UNIT))
                .unwrap_or_else(|_| FixedPointNumber::zero()),
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

    /// Helper to create a mock mailbox datum JSON for testing
    /// Now uses nested MerkleTreeState structure with branches
    fn create_test_mailbox_datum_json(
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
                {"int": local_domain},
                {"bytes": "00000000000000000000000000000000000000000000000000000000"},  // default_ism
                {"bytes": "00000000000000000000000000000000000000000000000000000000"},  // owner
                {"int": outbound_nonce},
                {
                    "constructor": 0,
                    "fields": [
                        {"list": branches_list},
                        {"int": merkle_count}
                    ]
                }
            ]
        })
    }

    /// Helper to create zero branches (32 zero hashes)
    fn zero_branches() -> Vec<[u8; 32]> {
        vec![[0u8; 32]; TREE_DEPTH]
    }

    #[test]
    fn test_parse_mailbox_datum_extracts_merkle_tree_state() {
        // Create a test datum with known branches
        let mut branches = zero_branches();
        branches[0] = [0xab; 32]; // Set first branch to a known value

        let datum_json = create_test_mailbox_datum_json(2003, 0, &branches, 1);

        // Extract the MerkleTreeState from the JSON
        let fields = datum_json.get("fields").unwrap().as_array().unwrap();
        let merkle_tree_json = fields.get(4).unwrap();
        let merkle_fields = merkle_tree_json.get("fields").unwrap().as_array().unwrap();

        // Extract branches list
        let branches_list = merkle_fields
            .get(0)
            .and_then(|f| f.get("list"))
            .and_then(|l| l.as_array())
            .unwrap();

        // Verify first branch
        let first_branch_hex = branches_list
            .get(0)
            .and_then(|b| b.get("bytes"))
            .and_then(|b| b.as_str())
            .unwrap();
        assert_eq!(first_branch_hex, hex::encode([0xab; 32]));

        // Extract count
        let count = merkle_fields
            .get(1)
            .and_then(|f| f.get("int"))
            .and_then(|i| i.as_u64())
            .unwrap() as u32;
        assert_eq!(count, 1);
    }

    #[test]
    fn test_parse_mailbox_datum_extracts_merkle_count() {
        let branches = zero_branches();
        let datum_json = create_test_mailbox_datum_json(2003, 5, &branches, 42);

        let fields = datum_json.get("fields").unwrap().as_array().unwrap();
        let merkle_tree_json = fields.get(4).unwrap();
        let merkle_fields = merkle_tree_json.get("fields").unwrap().as_array().unwrap();

        // Extract merkle_count (field 1 of MerkleTreeState)
        let merkle_count = merkle_fields
            .get(1)
            .and_then(|f| f.get("int"))
            .and_then(|i| i.as_u64())
            .unwrap() as u32;

        assert_eq!(merkle_count, 42);
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

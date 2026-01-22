//! Cardano lander adapter implementation.
//!
//! This adapter enables the Hyperlane relayer to submit Process transactions
//! to deliver messages to Cardano.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use uuid::Uuid;

use hyperlane_base::{settings::ChainConf, CoreMetrics};
use hyperlane_cardano::{
    BlockfrostProvider, CardanoMailbox, ConnectionConf, HyperlaneTxBuilder, Keypair,
};
use hyperlane_core::{
    ChainCommunicationError, ChainResult, ContractLocator, HyperlaneMessage, H256, H512,
};

use crate::{
    adapter::{
        chains::cardano::{CardanoTxCalldata, CardanoTxPrecursor, Precursor},
        AdaptsChain, GasLimit, TxBuildingResult,
    },
    payload::PayloadDetails,
    transaction::{Transaction, TransactionUuid, VmSpecificTxData},
    DispatcherMetrics, FullPayload, LanderError, TransactionDropReason, TransactionStatus,
};

/// Default estimated fee in lovelace (3 ADA)
const DEFAULT_FEE_ESTIMATE: u64 = 3_000_000;

/// Cardano lander adapter for submitting Process transactions.
pub struct CardanoAdapter {
    /// Connection configuration for Cardano
    pub connection_conf: ConnectionConf,
    /// Blockfrost provider for querying chain state
    pub provider: Arc<BlockfrostProvider>,
    /// Transaction builder for constructing Process transactions
    pub tx_builder: HyperlaneTxBuilder,
    /// Keypair for signing transactions
    pub signer: Keypair,
    /// Estimated block time for the chain
    pub estimated_block_time: Duration,
}

impl CardanoAdapter {
    /// Create a new Cardano adapter from configuration.
    pub fn from_conf(
        conf: &ChainConf,
        _core_metrics: &CoreMetrics,
        connection_conf: &ConnectionConf,
    ) -> Result<Self, LanderError> {
        // Get the signer keypair from the chain configuration
        let signer = create_signer(conf)?;

        // Create the Blockfrost provider
        let provider = Arc::new(BlockfrostProvider::new(
            &connection_conf.api_key,
            connection_conf.network,
        ));

        // Create the transaction builder
        let tx_builder = HyperlaneTxBuilder::new(connection_conf, provider.clone());

        Ok(Self {
            connection_conf: connection_conf.clone(),
            provider,
            tx_builder,
            signer,
            estimated_block_time: conf.estimated_block_time,
        })
    }
}

#[async_trait]
impl AdaptsChain for CardanoAdapter {
    async fn estimate_gas_limit(
        &self,
        _payload: &FullPayload,
    ) -> Result<Option<GasLimit>, LanderError> {
        // Cardano fees are deterministic based on transaction size and script execution units
        // Return a conservative estimate
        Ok(Some(DEFAULT_FEE_ESTIMATE.into()))
    }

    async fn build_transactions(&self, payloads: &[FullPayload]) -> Vec<TxBuildingResult> {
        let mut build_txs = Vec::new();

        for full_payload in payloads {
            // Deserialize the payload data as CardanoTxCalldata
            let calldata: CardanoTxCalldata = match serde_json::from_slice(&full_payload.data) {
                Ok(c) => c,
                Err(err) => {
                    tracing::error!(?err, "Failed to deserialize CardanoTxCalldata");
                    build_txs.push(TxBuildingResult {
                        payloads: vec![full_payload.details.clone()],
                        maybe_tx: None,
                    });
                    continue;
                }
            };

            // Create the precursor from calldata
            let precursor = CardanoTxPrecursor::from(calldata);

            // Build the Transaction struct
            let tx = Transaction {
                uuid: TransactionUuid::new(Uuid::new_v4()),
                tx_hashes: vec![],
                vm_specific_data: VmSpecificTxData::Cardano(Box::new(precursor)),
                payload_details: vec![full_payload.details.clone()],
                status: TransactionStatus::PendingInclusion,
                submission_attempts: 0,
                creation_timestamp: chrono::Utc::now(),
                last_submission_attempt: None,
                last_status_check: None,
            };

            build_txs.push(TxBuildingResult {
                payloads: vec![full_payload.details.clone()],
                maybe_tx: Some(tx),
            });
        }

        build_txs
    }

    async fn simulate_tx(&self, tx: &mut Transaction) -> Result<Vec<PayloadDetails>, LanderError> {
        tracing::info!(?tx, "simulating Cardano transaction");

        // Parse the message from the precursor
        let precursor = tx.precursor();
        let message = match decode_hyperlane_message(&precursor.message_bytes) {
            Ok(m) => m,
            Err(err) => {
                tracing::error!(?err, "Failed to decode HyperlaneMessage");
                return Ok(tx.payload_details.clone());
            }
        };

        // Try to build the transaction components to verify it can be built
        match self
            .tx_builder
            .build_process_tx(&message, &precursor.metadata)
            .await
        {
            Ok(_components) => {
                tracing::info!(
                    "Cardano transaction simulation successful for message nonce {}",
                    message.nonce
                );
                // Update fee estimate in precursor
                let precursor = tx.precursor_mut();
                precursor.fee_estimate = Some(DEFAULT_FEE_ESTIMATE);
                Ok(Vec::new())
            }
            Err(err) => {
                tracing::error!(?err, "Cardano transaction simulation failed");
                // Return the payload as reverted
                Ok(tx.payload_details.clone())
            }
        }
    }

    async fn estimate_tx(&self, tx: &mut Transaction) -> Result<(), LanderError> {
        // Set a conservative fee estimate
        let precursor = tx.precursor_mut();
        precursor.fee_estimate = Some(DEFAULT_FEE_ESTIMATE);
        Ok(())
    }

    async fn submit(&self, tx: &mut Transaction) -> Result<(), LanderError> {
        tracing::info!(?tx, "submitting Cardano transaction");

        let precursor = tx.precursor();

        // Decode the message
        let message = decode_hyperlane_message(&precursor.message_bytes)
            .map_err(|_e| LanderError::PayloadNotFound)?;

        // Build, sign, and submit the transaction using the tx_builder
        let outcome = self
            .tx_builder
            .build_and_submit_process_tx(&message, &precursor.metadata, &self.signer)
            .await
            .map_err(|e| {
                tracing::error!(?e, "Failed to submit Cardano transaction");
                LanderError::NetworkError(e.to_string())
            })?;

        // Extract tx hash from outcome
        let tx_hash = outcome.transaction_id;
        if !tx.tx_hashes.contains(&tx_hash) {
            tx.tx_hashes.push(tx_hash);
        }

        // Update precursor with tx hash
        let precursor = tx.precursor_mut();
        precursor.tx_hash = Some(tx_hash);

        tracing::info!(tx_uuid=?tx.uuid, ?tx_hash, "submitted Cardano transaction");
        Ok(())
    }

    async fn get_tx_hash_status(&self, hash: H512) -> Result<TransactionStatus, LanderError> {
        // Extract the 32-byte tx hash from the H512
        // Cardano tx hashes are 32 bytes, stored in the lower half of H512
        let hash_bytes: [u8; 64] = hash.0;
        let tx_hash_hex = hex::encode(&hash_bytes[32..64]);

        // Query Blockfrost for transaction status
        match self.provider.get_transaction_utxos(&tx_hash_hex).await {
            Ok(_tx_info) => {
                // Transaction found - check confirmation status
                // For simplicity, if the transaction is found, consider it finalized
                // (Cardano has deterministic finality after a few blocks)
                Ok(TransactionStatus::Finalized)
            }
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("404") || err_str.contains("not found") {
                    // Transaction not yet on chain
                    Err(LanderError::TxHashNotFound(tx_hash_hex))
                } else {
                    // Network error
                    Err(LanderError::NetworkError(err_str))
                }
            }
        }
    }

    async fn tx_ready_for_resubmission(&self, tx: &Transaction) -> bool {
        // Cardano uses a UTXO model - once a transaction is submitted,
        // it cannot be "replaced" like in Ethereum. The UTXOs are consumed.
        //
        // If the transaction has already been submitted (has a tx_hash),
        // we should NOT try to rebuild and resubmit - we should just wait
        // for it to be confirmed or check if it failed.
        //
        // Only allow resubmission if:
        // 1. The transaction has never been submitted (no tx_hash)
        // 2. OR the transaction hash is not found on-chain (tx was dropped from mempool)

        let precursor = tx.precursor();

        // If no tx_hash, this is a fresh transaction that needs to be submitted
        if precursor.tx_hash.is_none() {
            return true;
        }

        // If we have a tx_hash, check if it's on-chain
        let tx_hash = precursor.tx_hash.unwrap();
        match self.get_tx_hash_status(tx_hash).await {
            Ok(TransactionStatus::Finalized) => {
                // Transaction is already on-chain, no need to resubmit
                tracing::info!(
                    ?tx_hash,
                    "Transaction already confirmed on-chain, skipping resubmission"
                );
                false
            }
            Err(LanderError::TxHashNotFound(_)) => {
                // Transaction not on-chain - might have been dropped from mempool
                // However, we still shouldn't rebuild because the message might have been
                // delivered by the original transaction that's still propagating.
                // Wait a reasonable time before considering resubmission.
                if let Some(last_attempt) = tx.last_submission_attempt {
                    let elapsed = chrono::Utc::now() - last_attempt;
                    // Wait at least 2 minutes (6 Cardano slots) before considering resubmission
                    let min_wait = chrono::Duration::seconds(120);
                    if elapsed < min_wait {
                        tracing::debug!(
                            ?tx_hash,
                            elapsed_secs = elapsed.num_seconds(),
                            "Transaction not yet confirmed, waiting before resubmission"
                        );
                        return false;
                    }
                }
                // Enough time has passed, allow resubmission
                tracing::info!(
                    ?tx_hash,
                    "Transaction not found on-chain after timeout, allowing resubmission"
                );
                true
            }
            Err(e) => {
                // Network error - don't resubmit, just wait
                tracing::warn!(
                    ?e,
                    "Error checking transaction status, skipping resubmission"
                );
                false
            }
            _ => {
                // Any other status means transaction is being processed
                false
            }
        }
    }

    async fn reverted_payloads(
        &self,
        tx: &Transaction,
    ) -> Result<Vec<PayloadDetails>, LanderError> {
        // Check if the message was actually delivered by querying the mailbox
        let precursor = tx.precursor();

        let message = match decode_hyperlane_message(&precursor.message_bytes) {
            Ok(m) => m,
            Err(_) => return Ok(tx.payload_details.clone()),
        };

        // Check if message was delivered
        let message_id = message.id();

        // Prefer NFT lookup (O(1)) if processedMessagesNftPolicyId is configured
        let delivered = if let Some(ref nft_policy_id) =
            self.connection_conf.processed_messages_nft_policy_id
        {
            self.provider
                .is_message_delivered_by_nft(nft_policy_id, &message_id.0)
                .await
                .unwrap_or(false)
        } else {
            // Fallback: Scan UTXOs at processed_messages_script address (O(n))
            self.provider
                .is_message_delivered(
                    &self.connection_conf.processed_messages_script_hash,
                    &message_id.0,
                )
                .await
                .unwrap_or(false)
        };

        if delivered {
            Ok(Vec::new())
        } else {
            Ok(tx.payload_details.clone())
        }
    }

    fn estimated_block_time(&self) -> &Duration {
        &self.estimated_block_time
    }

    fn max_batch_size(&self) -> u32 {
        // Cardano transactions are processed one at a time
        1
    }

    fn update_vm_specific_metrics(&self, _tx: &Transaction, _metrics: &DispatcherMetrics) {
        // No specific metrics for Cardano yet
    }

    async fn nonce_gap_exists(&self) -> bool {
        // Cardano uses UTXOs, not account nonces
        false
    }

    async fn replace_tx(&self, _tx: &Transaction) -> Result<(), LanderError> {
        // Not applicable for Cardano UTXO model
        Ok(())
    }
}

/// Create a Cardano signer from chain configuration.
fn create_signer(conf: &ChainConf) -> Result<Keypair, LanderError> {
    use hyperlane_base::settings::SignerConf;

    let signer_conf = conf.signer.as_ref().ok_or_else(|| {
        LanderError::ConfigError("No signer configured for Cardano chain".to_string())
    })?;

    match signer_conf {
        SignerConf::HexKey { key } => {
            // key is already H256 with raw bytes (config parsing does hex decode)
            let key_bytes = key.as_bytes();

            let keypair = Keypair::from_secret_key(key_bytes).map_err(|e| {
                LanderError::ConfigError(format!("Failed to create Cardano keypair: {:?}", e))
            })?;

            Ok(keypair)
        }
        SignerConf::CardanoKey { key } => {
            // CardanoKey also contains H256 with raw bytes
            let key_bytes = key.as_bytes();

            let keypair = Keypair::from_secret_key(key_bytes).map_err(|e| {
                LanderError::ConfigError(format!("Failed to create Cardano keypair: {:?}", e))
            })?;

            Ok(keypair)
        }
        _ => Err(LanderError::ConfigError(
            "Cardano only supports HexKey or CardanoKey signer type".to_string(),
        )),
    }
}

/// Decode a HyperlaneMessage from its serialized bytes.
fn decode_hyperlane_message(bytes: &[u8]) -> Result<HyperlaneMessage, String> {
    // HyperlaneMessage encoding format:
    // - version: 1 byte
    // - nonce: 4 bytes (big endian)
    // - origin: 4 bytes (big endian)
    // - sender: 32 bytes
    // - destination: 4 bytes (big endian)
    // - recipient: 32 bytes
    // - body: remaining bytes

    if bytes.len() < 77 {
        return Err("Message too short".to_string());
    }

    let version = bytes[0];
    let nonce = u32::from_be_bytes(bytes[1..5].try_into().unwrap());
    let origin = u32::from_be_bytes(bytes[5..9].try_into().unwrap());
    let sender = H256::from_slice(&bytes[9..41]);
    let destination = u32::from_be_bytes(bytes[41..45].try_into().unwrap());
    let recipient = H256::from_slice(&bytes[45..77]);
    let body = bytes[77..].to_vec();

    Ok(HyperlaneMessage {
        version,
        nonce,
        origin,
        sender,
        destination,
        recipient,
        body,
    })
}

use crate::blockfrost_provider::BlockfrostProvider;
use crate::{CardanoMailbox, CardanoNetwork, ConnectionConf};
use async_trait::async_trait;
use hyperlane_core::{
    ChainResult, ContractLocator, HyperlaneMessage, Indexed, Indexer, LogMeta,
    SequenceAwareIndexer, H256, H512, U256,
};
use serde_json::Value;
use sha3::Digest;
use std::ops::RangeInclusive;
use std::sync::Arc;
use tracing::{debug, info, warn};

#[derive(Debug)]
pub struct CardanoMailboxIndexer {
    provider: Arc<BlockfrostProvider>,
    mailbox: CardanoMailbox,
    conf: ConnectionConf,
}

impl CardanoMailboxIndexer {
    pub fn new(conf: &ConnectionConf, locator: ContractLocator) -> ChainResult<Self> {
        let provider = BlockfrostProvider::new(&conf.api_key, conf.network);
        let mailbox = CardanoMailbox::new(conf, locator, None)?;
        Ok(Self {
            provider: Arc::new(provider),
            mailbox,
            conf: conf.clone(),
        })
    }

    async fn get_finalized_block_number(&self) -> ChainResult<u32> {
        self.mailbox.finalized_block_number().await
    }

    /// Get the mailbox script address
    fn get_mailbox_address(&self) -> ChainResult<String> {
        self.provider
            .script_hash_to_address(&self.conf.mailbox_policy_id)
            .map_err(hyperlane_core::ChainCommunicationError::from_other)
    }

    /// Get the local domain ID from configuration
    fn get_local_domain(&self) -> u32 {
        match self.conf.network {
            CardanoNetwork::Mainnet => 2001,
            CardanoNetwork::Preprod => 2002,
            CardanoNetwork::Preview => 2003,
        }
    }

    /// Parse a Dispatch redeemer from Blockfrost's JSON format to extract message data
    fn parse_dispatch_redeemer(
        &self,
        json: &Value,
        sender: H256,
        nonce: u32,
    ) -> Option<HyperlaneMessage> {
        // Dispatch redeemer format (constructor 0):
        // { "constructor": 0, "fields": [destination, recipient, body] }
        let constructor = json.get("constructor")?.as_u64()?;
        if constructor != 0 {
            return None; // Not a Dispatch redeemer
        }

        let fields = json.get("fields")?.as_array()?;
        if fields.len() < 3 {
            return None;
        }

        // Parse destination
        let destination = fields.get(0)?.get("int")?.as_u64()? as u32;

        // Parse recipient (32 bytes)
        let recipient_hex = fields.get(1)?.get("bytes")?.as_str()?;
        let recipient_bytes = hex::decode(recipient_hex).ok()?;
        if recipient_bytes.len() != 32 {
            return None;
        }
        let mut recipient = [0u8; 32];
        recipient.copy_from_slice(&recipient_bytes);

        // Parse body
        let body_hex = fields.get(2)?.get("bytes")?.as_str()?;
        let body = hex::decode(body_hex).ok()?;

        Some(HyperlaneMessage {
            version: 3, // Hyperlane protocol version
            nonce,
            origin: self.get_local_domain(),
            sender,
            destination,
            recipient: H256::from(recipient),
            body,
        })
    }

    /// Extract the sender address from transaction inputs
    /// The sender is the first input's address, converted to a Hyperlane address
    fn extract_sender_from_tx(&self, tx_utxos: &crate::blockfrost_provider::TransactionUtxos) -> H256 {
        // Get the first input's address
        if let Some(first_input) = tx_utxos.inputs.first() {
            // Try to extract script hash from the address
            // Cardano addresses can be verified using their credential
            // For simplicity, we hash the address string to get a unique identifier
            let address_bytes = first_input.address.as_bytes();

            // Create a Hyperlane address from the Cardano address
            // For script addresses, extract the script hash
            // For key addresses, hash the address
            if first_input.address.starts_with("addr") {
                // Try to decode as a Shelley address and extract credential
                let mut sender_bytes = [0u8; 32];
                // Use keccak256 hash of the address string as a fallback
                let hash = sha3::Keccak256::digest(address_bytes);
                sender_bytes.copy_from_slice(&hash);
                return H256::from(sender_bytes);
            }
        }

        H256::zero()
    }

    /// Parse the nonce from a mailbox datum
    fn parse_mailbox_nonce(&self, datum_json: &Value) -> Option<u32> {
        // MailboxDatum format:
        // { "constructor": 0, "fields": [local_domain, default_ism, owner, outbound_nonce, merkle_root, merkle_count] }
        let fields = datum_json.get("fields")?.as_array()?;
        if fields.len() < 4 {
            return None;
        }

        // outbound_nonce is at index 3
        let nonce = fields.get(3)?.get("int")?.as_u64()? as u32;
        Some(nonce)
    }

    /// Extract the nonce from transaction outputs (the new mailbox datum after dispatch)
    fn extract_nonce_from_outputs(&self, tx_utxos: &crate::blockfrost_provider::TransactionUtxos) -> u32 {
        // Look for the mailbox output and extract the nonce from its datum
        // The nonce in the output is already incremented, so subtract 1 to get the message nonce
        for output in &tx_utxos.outputs {
            if let Some(inline_datum) = &output.inline_datum {
                if let Ok(datum_json) = serde_json::from_str::<Value>(inline_datum) {
                    if let Some(nonce) = self.parse_mailbox_nonce(&datum_json) {
                        // The output nonce is incremented, so the message nonce is one less
                        return nonce.saturating_sub(1);
                    }
                }
            }
        }

        // If we can't find the nonce, log a warning and return 0
        warn!("Could not extract nonce from mailbox output datum");
        0
    }

    /// Parse ProcessedMessageDatum from inline datum
    fn parse_processed_message_datum(&self, json: &Value) -> Option<H256> {
        // ProcessedMessageDatum format:
        // { "constructor": 0, "fields": [message_id] }
        let fields = json.get("fields")?.as_array()?;
        let message_id_hex = fields.get(0)?.get("bytes")?.as_str()?;
        let message_id_bytes = hex::decode(message_id_hex).ok()?;
        if message_id_bytes.len() != 32 {
            return None;
        }
        let mut message_id = [0u8; 32];
        message_id.copy_from_slice(&message_id_bytes);
        Some(H256::from(message_id))
    }
}

#[async_trait]
impl Indexer<HyperlaneMessage> for CardanoMailboxIndexer {
    async fn fetch_logs_in_range(
        &self,
        range: RangeInclusive<u32>,
    ) -> ChainResult<Vec<(Indexed<HyperlaneMessage>, LogMeta)>> {
        let from = *range.start();
        let to = *range.end();

        info!(
            "Fetching Cardano HyperlaneMessage logs from block {} to {}",
            from, to
        );

        // Get mailbox script address
        let mailbox_address = self.get_mailbox_address()?;
        debug!("Mailbox address: {}", mailbox_address);

        // Get transactions at mailbox address in the block range
        let transactions = self
            .provider
            .get_address_transactions(&mailbox_address, Some(from as u64), Some(to as u64))
            .await
            .map_err(hyperlane_core::ChainCommunicationError::from_other)?;

        info!(
            "Found {} transactions at mailbox in block range {} to {}",
            transactions.len(),
            from,
            to
        );

        let mut results = Vec::new();

        for tx_info in transactions {
            // Get transaction redeemers to find Dispatch actions
            let redeemers = match self
                .provider
                .get_transaction_redeemers(&tx_info.tx_hash)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    debug!(
                        "Could not get redeemers for tx {}: {}",
                        tx_info.tx_hash, e
                    );
                    continue;
                }
            };

            // Find redeemers that are for spending (not minting)
            for redeemer in redeemers {
                if redeemer.purpose != "Spend" {
                    continue;
                }

                // Get the redeemer datum content
                let redeemer_datum = match self
                    .provider
                    .get_redeemer_datum(&redeemer.redeemer_data_hash)
                    .await
                {
                    Ok(d) => d,
                    Err(e) => {
                        debug!(
                            "Could not get redeemer datum for tx {}: {}",
                            tx_info.tx_hash, e
                        );
                        continue;
                    }
                };

                // Get transaction UTXOs to extract sender
                let tx_utxos = match self
                    .provider
                    .get_transaction_utxos(&tx_info.tx_hash)
                    .await
                {
                    Ok(u) => u,
                    Err(e) => {
                        debug!(
                            "Could not get UTXOs for tx {}: {}",
                            tx_info.tx_hash, e
                        );
                        continue;
                    }
                };

                // Extract sender from first input
                let sender = self.extract_sender_from_tx(&tx_utxos);

                // Try to extract nonce from mailbox output datum
                let nonce = self.extract_nonce_from_outputs(&tx_utxos);

                if let Some(message) = self.parse_dispatch_redeemer(&redeemer_datum, sender, nonce) {
                    let message_id = message.id();
                    let indexed = Indexed::new(message);

                    let log_meta = LogMeta {
                        address: H256::zero(), // Cardano doesn't have contract addresses like EVM
                        block_number: tx_info.block_height,
                        block_hash: H256::from_slice(
                            &hex::decode(&tx_info.tx_hash.get(0..64).unwrap_or(""))
                                .unwrap_or_else(|_| vec![0u8; 32]),
                        ),
                        transaction_id: H512::from_slice(&{
                            let mut bytes = [0u8; 64];
                            let tx_bytes = hex::decode(&tx_info.tx_hash).unwrap_or_else(|_| vec![0u8; 32]);
                            bytes[..tx_bytes.len().min(64)].copy_from_slice(&tx_bytes[..tx_bytes.len().min(64)]);
                            bytes
                        }),
                        transaction_index: tx_info.tx_index as u64,
                        log_index: U256::from(redeemer.tx_index),
                    };

                    info!(
                        "Found dispatched message in tx {}, message_id: {}",
                        tx_info.tx_hash,
                        hex::encode(message_id.as_bytes())
                    );
                    results.push((indexed, log_meta));
                }
            }
        }

        Ok(results)
    }

    async fn get_finalized_block_number(&self) -> ChainResult<u32> {
        self.get_finalized_block_number().await
    }
}

#[async_trait]
impl SequenceAwareIndexer<HyperlaneMessage> for CardanoMailboxIndexer {
    async fn latest_sequence_count_and_tip(&self) -> ChainResult<(Option<u32>, u32)> {
        self.mailbox
            .tree_and_tip(None)
            .await
            .map(|(tree, tip)| (Some(tree.count() as u32), tip))
    }
}

// H256 indexer is used by the scraper agent to index delivered message IDs
// Queries processed message marker UTXOs from the mailbox script address
#[async_trait]
impl Indexer<H256> for CardanoMailboxIndexer {
    async fn fetch_logs_in_range(
        &self,
        range: RangeInclusive<u32>,
    ) -> ChainResult<Vec<(Indexed<H256>, LogMeta)>> {
        let from = *range.start();
        let to = *range.end();

        info!(
            "Fetching Cardano delivered message IDs from block {} to {}",
            from, to
        );

        // For delivered messages, we look for processed message marker UTXOs
        // These are created when a Process transaction is executed
        // The marker contains the message_id in its datum

        // Get the processed messages script address
        // This is typically a script that holds marker NFTs
        let processed_script_address = self
            .provider
            .script_hash_to_address(&self.conf.mailbox_policy_id)
            .map_err(hyperlane_core::ChainCommunicationError::from_other)?;

        // Get transactions that created processed message markers
        let transactions = self
            .provider
            .get_address_transactions(&processed_script_address, Some(from as u64), Some(to as u64))
            .await
            .map_err(hyperlane_core::ChainCommunicationError::from_other)?;

        let mut results = Vec::new();

        for tx_info in transactions {
            // Get transaction UTXOs to find outputs with processed message datums
            let tx_utxos = match self
                .provider
                .get_transaction_utxos(&tx_info.tx_hash)
                .await
            {
                Ok(u) => u,
                Err(e) => {
                    debug!("Could not get UTXOs for tx {}: {}", tx_info.tx_hash, e);
                    continue;
                }
            };

            // Check each output for processed message markers
            for output in tx_utxos.outputs {
                if let Some(inline_datum) = &output.inline_datum {
                    // Try to parse the datum as JSON
                    if let Ok(datum_json) = serde_json::from_str::<Value>(inline_datum) {
                        if let Some(message_id) = self.parse_processed_message_datum(&datum_json) {
                            let indexed = Indexed::new(message_id);

                            let log_meta = LogMeta {
                                address: H256::zero(),
                                block_number: tx_info.block_height,
                                block_hash: H256::from_slice(
                                    &hex::decode(&tx_info.tx_hash.get(0..64).unwrap_or(""))
                                        .unwrap_or_else(|_| vec![0u8; 32]),
                                ),
                                transaction_id: H512::from_slice(&{
                                    let mut bytes = [0u8; 64];
                                    let tx_bytes =
                                        hex::decode(&tx_info.tx_hash).unwrap_or_else(|_| vec![0u8; 32]);
                                    bytes[..tx_bytes.len().min(64)]
                                        .copy_from_slice(&tx_bytes[..tx_bytes.len().min(64)]);
                                    bytes
                                }),
                                transaction_index: tx_info.tx_index as u64,
                                log_index: U256::from(output.output_index),
                            };

                            info!(
                                "Found delivered message in tx {}, message_id: {}",
                                tx_info.tx_hash,
                                hex::encode(message_id.as_bytes())
                            );
                            results.push((indexed, log_meta));
                        }
                    }
                }
            }
        }

        Ok(results)
    }

    async fn get_finalized_block_number(&self) -> ChainResult<u32> {
        self.get_finalized_block_number().await
    }
}

#[async_trait]
impl SequenceAwareIndexer<H256> for CardanoMailboxIndexer {
    async fn latest_sequence_count_and_tip(&self) -> ChainResult<(Option<u32>, u32)> {
        // Cardano delivery indexing not yet fully implemented
        // Return None for sequence count and current block tip
        let tip = self.get_finalized_block_number().await?;
        Ok((None, tip))
    }
}

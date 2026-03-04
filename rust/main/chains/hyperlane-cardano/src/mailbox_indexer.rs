use crate::blockfrost_provider::{AddressTransaction, BlockfrostProvider};
use crate::{CardanoMailbox, CardanoNetwork, ConnectionConf};
use async_trait::async_trait;
use bech32::FromBase32;
use ciborium::Value as CborValue;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use hyperlane_core::{
    ChainResult, ContractLocator, HyperlaneMessage, Indexed, Indexer, LogMeta,
    SequenceAwareIndexer, H256, H512, U256,
};
use serde_json::Value as JsonValue;
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
        let provider =
            BlockfrostProvider::new(&conf.api_key, conf.network, conf.confirmation_block_delay);
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
        // Use the mailbox script hash from config for address derivation
        self.provider
            .script_hash_to_address(&self.conf.mailbox_script_hash)
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

    /// Parse a Dispatch redeemer from Blockfrost's JSON format to extract message data.
    /// The sender_ref field (4th field) identifies the specific input to use as sender.
    fn parse_dispatch_redeemer(
        &self,
        json: &JsonValue,
        tx_utxos: &crate::blockfrost_provider::TransactionUtxos,
        nonce: u32,
    ) -> Option<HyperlaneMessage> {
        // Dispatch redeemer format (constructor 0):
        // { "constructor": 0, "fields": [destination, recipient, body, sender_ref, hook_metadata] }
        let constructor = json.get("constructor")?.as_u64()?;
        if constructor != 0 {
            return None; // Not a Dispatch redeemer
        }

        let fields = json.get("fields")?.as_array()?;
        if fields.len() < 3 {
            return None;
        }

        // Parse destination
        let destination = fields.first()?.get("int")?.as_u64()? as u32;

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

        // Parse sender_ref (4th field) if present: Constr 0 [tx_hash, output_index]
        let sender_ref = fields.get(3).and_then(|sr| {
            let sr_fields = sr.get("fields")?.as_array()?;
            let tx_hash = sr_fields.first()?.get("bytes")?.as_str()?;
            let output_index = sr_fields.get(1)?.get("int")?.as_u64()? as u32;
            Some((tx_hash.to_string(), output_index))
        });

        let sender = self.extract_sender_from_tx(tx_utxos, sender_ref.as_ref());

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

    /// Extract the sender address from transaction inputs.
    /// When sender_ref is provided, looks up that specific input directly.
    /// Falls back to the heuristic (first script input excluding mailbox) for old TXs.
    ///
    /// The on-chain Aiken mailbox computes sender as:
    /// - Payment key credential: `0x00000000 || vkh`
    /// - Script with state NFT: `0x01000000 || nft_policy_id`
    /// - Script without state NFT: `0x02000000 || script_hash`
    ///
    /// A "state NFT" is a non-ADA asset with empty asset name and quantity 1.
    fn extract_sender_from_tx(
        &self,
        tx_utxos: &crate::blockfrost_provider::TransactionUtxos,
        sender_ref: Option<&(String, u32)>,
    ) -> H256 {
        let spent_inputs: Vec<_> = tx_utxos
            .inputs
            .iter()
            .filter(|input| !input.collateral && !input.reference)
            .collect();

        // If sender_ref is provided, look up the specific input
        let first_input = if let Some((ref_tx_hash, ref_output_index)) = sender_ref {
            spent_inputs
                .iter()
                .find(|input| {
                    input.tx_hash == *ref_tx_hash && input.output_index == *ref_output_index
                })
                .copied()
        } else {
            // Fallback heuristic for backwards compatibility
            let mailbox_address = self.get_mailbox_address().ok();
            let mut sorted_inputs = spent_inputs.clone();
            sorted_inputs.sort_by(|a, b| match a.tx_hash.cmp(&b.tx_hash) {
                std::cmp::Ordering::Equal => a.output_index.cmp(&b.output_index),
                other => other,
            });

            let sender_input = sorted_inputs
                .iter()
                .find(|input| {
                    if let Some(ref mailbox_addr) = mailbox_address {
                        if &input.address == mailbox_addr {
                            return false;
                        }
                    }
                    input.address.starts_with("addr_test1w") || input.address.starts_with("addr1w")
                })
                .copied();

            sender_input.or_else(|| sorted_inputs.first().copied())
        };

        if let Some(first_input) = first_input {
            if first_input.address.starts_with("addr") {
                if let Ok((_, data_5bit, _)) = bech32::decode(&first_input.address) {
                    if let Ok(data_8bit) = Vec::<u8>::from_base32(&data_5bit) {
                        if data_8bit.len() >= 29 {
                            let header = data_8bit[0];
                            let credential = &data_8bit[1..29];
                            let is_script = (header >> 4) & 1 == 1;

                            let mut sender_bytes = [0u8; 32];

                            if is_script {
                                // Check for a state NFT in the input's value.
                                // A state NFT is a non-ADA policy with empty asset name
                                // (unit == 56-char policy_id hex) and quantity 1.
                                let nft_policy = first_input.value.iter().find_map(|v| {
                                    if v.unit != "lovelace"
                                        && v.unit.len() == 56
                                        && v.quantity == "1"
                                    {
                                        Some(&v.unit)
                                    } else {
                                        None
                                    }
                                });

                                if let Some(policy_hex) = nft_policy {
                                    // State NFT found: use 0x01000000 || policy_id
                                    sender_bytes[0] = 0x01;
                                    if let Ok(policy_bytes) = hex::decode(policy_hex) {
                                        if policy_bytes.len() == 28 {
                                            sender_bytes[4..32].copy_from_slice(&policy_bytes);
                                        }
                                    }
                                } else {
                                    // Pure script, no state NFT: 0x02000000 || script_hash
                                    sender_bytes[0] = 0x02;
                                    sender_bytes[4..32].copy_from_slice(credential);
                                }
                            } else {
                                // Payment key: 0x00000000 || vkh
                                sender_bytes[4..32].copy_from_slice(credential);
                            }

                            info!(
                                "Extracted sender: tx={}#{}, sender=0x{}",
                                first_input.tx_hash,
                                first_input.output_index,
                                hex::encode(sender_bytes)
                            );

                            return H256::from(sender_bytes);
                        }
                    }
                }
            }
        }

        H256::zero()
    }

    /// Parse the nonce from a mailbox datum (JSON format)
    fn parse_mailbox_nonce_json(&self, datum_json: &JsonValue) -> Option<u32> {
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

    /// Parse the nonce from a mailbox datum (CBOR format)
    /// MailboxDatum CBOR structure: Constr 0 [local_domain, default_ism, owner, outbound_nonce, merkle_root, merkle_count]
    fn parse_mailbox_nonce_cbor(&self, cbor_hex: &str) -> Option<u32> {
        let cbor_bytes = hex::decode(cbor_hex).ok()?;
        let value: CborValue = ciborium::from_reader(&cbor_bytes[..]).ok()?;

        // Extract the tagged value (Constr 0 = tag 121)
        let fields = match &value {
            CborValue::Tag(121, inner) => match inner.as_ref() {
                CborValue::Array(arr) => arr,
                _ => return None,
            },
            _ => return None,
        };

        if fields.len() < 4 {
            return None;
        }

        // outbound_nonce is at index 3
        match &fields[3] {
            CborValue::Integer(n) => {
                let nonce: i128 = (*n).into();
                Some(nonce as u32)
            }
            _ => None,
        }
    }

    /// Extract the nonce from transaction outputs (the new mailbox datum after dispatch)
    fn extract_nonce_from_outputs(
        &self,
        tx_utxos: &crate::blockfrost_provider::TransactionUtxos,
    ) -> u32 {
        // Look for the mailbox output and extract the nonce from its datum
        // The nonce in the output is already incremented, so subtract 1 to get the message nonce
        for output in &tx_utxos.outputs {
            if let Some(inline_datum) = &output.inline_datum {
                // Try JSON format first, then CBOR
                let nonce = if let Ok(datum_json) = serde_json::from_str::<JsonValue>(inline_datum)
                {
                    self.parse_mailbox_nonce_json(&datum_json)
                } else {
                    self.parse_mailbox_nonce_cbor(inline_datum)
                };

                if let Some(n) = nonce {
                    // The output nonce is incremented, so the message nonce is one less
                    return n.saturating_sub(1);
                }
            }
        }

        // If we can't find the nonce, log a warning and return 0
        warn!("Could not extract nonce from mailbox output datum");
        0
    }

    /// Parse ProcessedMessageDatum from inline datum
    fn parse_processed_message_datum(&self, json: &JsonValue) -> Option<H256> {
        // ProcessedMessageDatum format:
        // { "constructor": 0, "fields": [message_id] }
        let fields = json.get("fields")?.as_array()?;
        let message_id_hex = fields.first()?.get("bytes")?.as_str()?;
        let message_id_bytes = hex::decode(message_id_hex).ok()?;
        if message_id_bytes.len() != 32 {
            return None;
        }
        let mut message_id = [0u8; 32];
        message_id.copy_from_slice(&message_id_bytes);
        Some(H256::from(message_id))
    }

    async fn process_dispatch_transaction(
        &self,
        tx_info: &AddressTransaction,
    ) -> Vec<(Indexed<HyperlaneMessage>, LogMeta)> {
        let mut results = Vec::new();

        info!("Processing transaction: {}", tx_info.tx_hash);

        let redeemers = match self
            .provider
            .get_transaction_redeemers(&tx_info.tx_hash)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                info!("Could not get redeemers for tx {}: {}", tx_info.tx_hash, e);
                return results;
            }
        };

        info!(
            "Found {} redeemers for tx {}",
            redeemers.len(),
            tx_info.tx_hash
        );

        for redeemer in redeemers {
            info!(
                "Redeemer purpose: {}, data_hash: {}",
                redeemer.purpose, redeemer.redeemer_data_hash
            );
            if redeemer.purpose != "spend" && redeemer.purpose != "Spend" {
                info!("Skipping non-spend redeemer");
                continue;
            }

            let redeemer_datum = match self
                .provider
                .get_redeemer_datum(&redeemer.redeemer_data_hash)
                .await
            {
                Ok(d) => d,
                Err(e) => {
                    info!(
                        "Could not get redeemer datum for tx {}: {}",
                        tx_info.tx_hash, e
                    );
                    continue;
                }
            };

            info!("Got redeemer datum: {:?}", redeemer_datum);

            let tx_utxos = match self.provider.get_transaction_utxos(&tx_info.tx_hash).await {
                Ok(u) => u,
                Err(e) => {
                    info!("Could not get UTXOs for tx {}: {}", tx_info.tx_hash, e);
                    continue;
                }
            };

            let nonce = self.extract_nonce_from_outputs(&tx_utxos);
            info!("Extracted nonce: {}", nonce);

            if let Some(message) = self.parse_dispatch_redeemer(&redeemer_datum, &tx_utxos, nonce) {
                let message_id = message.id();
                let indexed: Indexed<HyperlaneMessage> = message.into();
                info!(
                    "Created indexed message with nonce: {}, sequence: {:?}",
                    nonce, indexed.sequence
                );

                let log_meta = LogMeta {
                    address: H256::zero(),
                    block_number: tx_info.block_height,
                    block_hash: H256::from_slice(
                        &hex::decode(tx_info.tx_hash.get(0..64).unwrap_or(""))
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

        results
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

        // Get mailbox script address
        let mailbox_address = self.get_mailbox_address()?;

        info!(
            "Fetching Cardano HyperlaneMessage logs from block {} to {} at address {}",
            from, to, mailbox_address
        );

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

        let futs: FuturesUnordered<_> = transactions
            .iter()
            .map(|tx_info| self.process_dispatch_transaction(tx_info))
            .collect();
        let results: Vec<Vec<_>> = futs.collect().await;

        Ok(results.into_iter().flatten().collect())
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
            .script_hash_to_address(&self.conf.processed_messages_script_hash)
            .map_err(hyperlane_core::ChainCommunicationError::from_other)?;

        // Get transactions that created processed message markers
        let transactions = self
            .provider
            .get_address_transactions(
                &processed_script_address,
                Some(from as u64),
                Some(to as u64),
            )
            .await
            .map_err(hyperlane_core::ChainCommunicationError::from_other)?;

        let mut results = Vec::new();

        for tx_info in transactions {
            // Get transaction UTXOs to find outputs with processed message datums
            let tx_utxos = match self.provider.get_transaction_utxos(&tx_info.tx_hash).await {
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
                    if let Ok(datum_json) = serde_json::from_str::<JsonValue>(inline_datum) {
                        if let Some(message_id) = self.parse_processed_message_datum(&datum_json) {
                            let indexed = Indexed::new(message_id);

                            let log_meta = LogMeta {
                                address: H256::zero(),
                                block_number: tx_info.block_height,
                                block_hash: H256::from_slice(
                                    &hex::decode(tx_info.tx_hash.get(0..64).unwrap_or(""))
                                        .unwrap_or_else(|_| vec![0u8; 32]),
                                ),
                                transaction_id: H512::from_slice(&{
                                    let mut bytes = [0u8; 64];
                                    let tx_bytes = hex::decode(&tx_info.tx_hash)
                                        .unwrap_or_else(|_| vec![0u8; 32]);
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

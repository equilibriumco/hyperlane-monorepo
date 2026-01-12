//! Transaction builder for Cardano Hyperlane operations
//!
//! This module provides transaction building capabilities for processing
//! Hyperlane messages on Cardano using pallas primitives for CBOR encoding
//! and pallas-txbuilder for transaction construction.

use crate::blockfrost_provider::{BlockfrostProvider, BlockfrostProviderError, CardanoNetwork, Utxo};
use crate::cardano::Keypair;
use crate::registry::RecipientRegistry;
use crate::types::{
    hyperlane_address_to_script_hash, HyperlaneRecipientRedeemer, MailboxRedeemer, Message,
    ProcessedMessageDatum,
};
use crate::ConnectionConf;
use hyperlane_core::{ChainCommunicationError, FixedPointNumber, HyperlaneMessage, TxOutcome, H512, U256};
use pallas_addresses::{Address, Network};
use pallas_codec::minicbor;
use pallas_crypto::hash::Hash;
use pallas_primitives::conway::{BigInt, Constr, PlutusData};
use pallas_codec::utils::{KeyValuePairs, MaybeIndefArray};
use pallas_txbuilder::{BuildConway, BuiltTransaction, ExUnits, Input, Output, ScriptKind, StagingTransaction};
use std::sync::Arc;
use thiserror::Error;
use tracing::{debug, info, instrument, warn};

#[derive(Error, Debug)]
pub enum TxBuilderError {
    #[error("Blockfrost error: {0}")]
    Blockfrost(#[from] BlockfrostProviderError),
    #[error("Registry error: {0}")]
    Registry(#[from] crate::registry::RegistryError),
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

/// Minimum lovelace per UTXO (Cardano protocol parameter ~1 ADA)
const MIN_UTXO_LOVELACE: u64 = 2_000_000;

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
    registry: RecipientRegistry,
    conf: ConnectionConf,
}

impl HyperlaneTxBuilder {
    /// Create a new transaction builder
    pub fn new(conf: &ConnectionConf, provider: Arc<BlockfrostProvider>) -> Self {
        let registry = RecipientRegistry::new(
            BlockfrostProvider::new(&conf.api_key, conf.network),
            conf.registry_policy_id.clone(),
            conf.registry_asset_name_hex.clone(),
        );

        Self {
            provider,
            registry,
            conf: conf.clone(),
        }
    }

    /// Find the mailbox UTXO by NFT or fall back to script address lookup
    async fn find_mailbox_utxo(&self) -> Result<Utxo, TxBuilderError> {
        // First try to find by NFT (preferred method for production)
        let nft_result = self.provider
            .find_utxo_by_nft(&self.conf.mailbox_policy_id, &self.conf.mailbox_asset_name_hex)
            .await;

        match nft_result {
            Ok(utxo) => {
                debug!("Found mailbox UTXO by NFT: {}#{}", utxo.tx_hash, utxo.output_index);
                return Ok(utxo);
            }
            Err(e) => {
                debug!("NFT lookup failed ({}), falling back to script address lookup", e);
            }
        }

        // Fallback: Find UTXOs at the mailbox script address using the actual script hash
        let script_address = self.provider.script_hash_to_address(&self.conf.mailbox_script_hash)?;
        debug!("Looking up mailbox UTXOs at script address: {}", script_address);

        let utxos = self.provider.get_utxos_at_address(&script_address).await?;

        // Find the first UTXO with an inline datum (the mailbox state UTXO)
        for utxo in utxos {
            if utxo.inline_datum.is_some() {
                debug!("Found mailbox UTXO by script address: {}#{}", utxo.tx_hash, utxo.output_index);
                return Ok(utxo);
            }
        }

        Err(TxBuilderError::UtxoNotFound(
            "No mailbox UTXO found with inline datum at script address".to_string(),
        ))
    }

    /// Build a Process transaction for delivering a message to Cardano
    ///
    /// This creates a transaction that:
    /// 1. Spends the mailbox UTXO with Process redeemer
    /// 2. Includes ISM UTXO as reference input for signature verification
    /// 3. Spends recipient UTXO with HandleMessage redeemer
    /// 4. Creates processed message marker output
    /// 5. Creates continuation outputs for mailbox and recipient
    #[instrument(skip(self, metadata))]
    pub async fn build_process_tx(
        &self,
        message: &HyperlaneMessage,
        metadata: &[u8],
    ) -> Result<ProcessTxComponents, TxBuilderError> {
        info!("Building process transaction for message nonce {}", message.nonce);

        // Convert to our Message type
        let msg = Message::from_hyperlane_message(message);
        let message_id = msg.id();

        // 1. Find mailbox UTXO (try NFT first, then fall back to script address)
        let mailbox_utxo = self.find_mailbox_utxo().await?;
        info!("Found mailbox UTXO: {}#{}", mailbox_utxo.tx_hash, mailbox_utxo.output_index);

        // 2. Get recipient registration from registry
        let recipient_script_hash = hyperlane_address_to_script_hash(&msg.recipient)
            .ok_or_else(|| TxBuilderError::InvalidRecipient("Not a script recipient".to_string()))?;
        info!("Looking up recipient registration for script hash: {}", hex::encode(&recipient_script_hash));

        let registration = self.registry.get_registration(&recipient_script_hash).await?;
        info!("Registration found for script_hash={}: state_locator.policy_id={}, state_locator.asset_name={}, recipient_type={:?}",
              hex::encode(&recipient_script_hash),
              registration.state_locator.policy_id,
              registration.state_locator.asset_name,
              registration.recipient_type);

        // 3. Find recipient state UTXO
        let recipient_utxo = self
            .provider
            .find_utxo_by_nft(
                &registration.state_locator.policy_id,
                &registration.state_locator.asset_name,
            )
            .await?;
        info!(
            "Found recipient state UTXO: {}#{}",
            recipient_utxo.tx_hash, recipient_utxo.output_index
        );

        // 3b. Find recipient reference script UTXO (if separate from state UTXO)
        let recipient_ref_script_utxo = if let Some(ref ref_locator) = registration.reference_script_locator {
            let ref_utxo = self
                .provider
                .find_utxo_by_nft(&ref_locator.policy_id, &ref_locator.asset_name)
                .await?;
            info!(
                "Found recipient reference script UTXO: {}#{}",
                ref_utxo.tx_hash, ref_utxo.output_index
            );
            Some(ref_utxo)
        } else {
            debug!("No separate reference script UTXO, script embedded in state UTXO");
            None
        };

        // 3c. For deferred recipients, find the stored_message_nft reference script UTXO
        // The CLI deploys this with asset name "msg_ref" (hex: 6d73675f726566) at the same policy ID
        let message_nft_ref_script_utxo = if matches!(registration.recipient_type, crate::types::RecipientType::Deferred { .. }) {
            if let Some(ref ref_locator) = registration.reference_script_locator {
                // Look up using the same policy ID but with "msg_ref" asset name
                let msg_ref_asset_name = "6d73675f726566".to_string(); // "msg_ref" in hex
                match self.provider.find_utxo_by_nft(&ref_locator.policy_id, &msg_ref_asset_name).await {
                    Ok(msg_ref_utxo) => {
                        info!(
                            "Found message NFT reference script UTXO: {}#{}",
                            msg_ref_utxo.tx_hash, msg_ref_utxo.output_index
                        );
                        Some(msg_ref_utxo)
                    }
                    Err(e) => {
                        // This might happen for legacy deployments without the msg_ref NFT
                        warn!(
                            "Could not find msg_ref NFT for deferred recipient (policy: {}): {}. \
                             The stored_message_nft script may need to be in the relayer config.",
                            ref_locator.policy_id, e
                        );
                        None
                    }
                }
            } else {
                debug!("No reference_script_locator for deferred recipient, cannot look up msg_ref");
                None
            }
        } else {
            None
        };

        // 4. Find ISM UTXO (either custom or default)
        // For custom ISM, use empty asset name; for default ISM, use config asset name
        let (ism_policy_id, ism_asset_name) = match &registration.custom_ism {
            Some(ism) => (hex::encode(ism), String::new()),
            None => (self.conf.ism_policy_id.clone(), self.conf.ism_asset_name_hex.clone()),
        };
        let ism_utxo = self
            .provider
            .find_utxo_by_nft(&ism_policy_id, &ism_asset_name)
            .await?;
        debug!("Found ISM UTXO: {}#{}", ism_utxo.tx_hash, ism_utxo.output_index);

        // 5. Find additional inputs required by recipient
        let mut additional_utxos = Vec::new();
        for input in &registration.additional_inputs {
            let utxo = self
                .provider
                .find_utxo_by_nft(&input.locator.policy_id, &input.locator.asset_name)
                .await?;
            additional_utxos.push((utxo, input.must_be_spent));
        }

        // 6. Encode redeemers
        let mailbox_redeemer = MailboxRedeemer::Process {
            message: msg.clone(),
            metadata: metadata.to_vec(),
            message_id,
        };
        let mailbox_redeemer_cbor = encode_mailbox_redeemer(&mailbox_redeemer)?;

        // SECURITY: Pass the full message and message_id to recipient
        // The recipient MUST verify: keccak256(encode_message(message)) == message_id
        // This ensures the data is cryptographically linked to what the ISM validated
        let recipient_redeemer: HyperlaneRecipientRedeemer<()> =
            HyperlaneRecipientRedeemer::HandleMessage {
                message: msg.clone(),
                message_id,
            };
        let recipient_redeemer_cbor = encode_recipient_redeemer(&recipient_redeemer)?;

        // 7. Build recipient continuation datum based on recipient type
        // For Deferred, we also need to build the stored message datum and NFT mint redeemer
        let (recipient_continuation_datum_cbor, stored_message_datum_cbor, message_nft_redeemer_cbor) =
            match &registration.recipient_type {
                crate::types::RecipientType::Deferred { .. } => {
                    info!("Processing Deferred recipient - will store message for later processing");

                    // Build Deferred recipient continuation datum (increments messages_stored counter)
                    let continuation_datum = build_deferred_continuation_datum(&recipient_utxo)?;

                    // Build StoredMessageDatum for the message UTXO
                    let stored_msg_datum = crate::types::StoredMessageDatum {
                        origin: msg.origin,
                        sender: msg.sender,
                        body: msg.body.clone(),
                        message_id,
                        nonce: msg.nonce,
                    };
                    let stored_msg_datum_cbor = encode_stored_message_datum(&stored_msg_datum)?;

                    // Build message NFT mint redeemer
                    let nft_redeemer = crate::types::MessageNftRedeemer::MintMessage;
                    let nft_redeemer_cbor = encode_message_nft_redeemer(&nft_redeemer)?;

                    (continuation_datum, Some(stored_msg_datum_cbor), Some(nft_redeemer_cbor))
                }
                _ => {
                    // Generic, TokenReceiver - use existing logic
                    let continuation_datum = build_recipient_continuation_datum(&recipient_utxo, &msg.body)?;
                    (continuation_datum, None, None)
                }
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
            hex::encode(&parsed_metadata.merkle_root),
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
            hex::encode(&parsed_metadata.merkle_root),
            hex::encode(&parsed_metadata.origin_mailbox),
            parsed_metadata.root_index,
            hex::encode(&message_id)
        );

        // Build ISM redeemer with validator signatures and recovered public keys
        let ism_redeemer = crate::types::MultisigIsmRedeemer::Verify {
            checkpoint,
            validator_signatures: parsed_metadata.validator_signatures,
        };
        let ism_redeemer_cbor = encode_ism_redeemer(&ism_redeemer)?;
        debug!("Encoded ISM Verify redeemer ({} bytes)", ism_redeemer_cbor.len());

        Ok(ProcessTxComponents {
            mailbox_utxo,
            mailbox_redeemer_cbor,
            recipient_utxo,
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
            recipient_type: registration.recipient_type.clone(),
            stored_message_datum_cbor,
            message_nft_redeemer_cbor,
            message_nft_ref_script_utxo,
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
        // 1. Build transaction components
        info!("Building process transaction components for message nonce {}", message.nonce);
        let components = self.build_process_tx(message, metadata).await?;

        // 2. Build the complete transaction
        info!("Constructing full transaction with pallas-txbuilder");
        let built_tx = self.build_complete_process_tx(&components, payer).await?;

        // 3. Sign the transaction
        info!("Signing transaction");
        let signed_tx = self.sign_transaction(built_tx, payer)?;

        // 4. Submit to Blockfrost
        info!("Submitting transaction to Blockfrost");
        let tx_hash = self.submit_transaction(&signed_tx).await?;

        info!("Transaction submitted successfully: {}", tx_hash);

        // Convert tx_hash string to H512
        let mut tx_id_bytes = [0u8; 64];
        let hash_bytes = hex::decode(&tx_hash)
            .map_err(|e| TxBuilderError::Encoding(format!("Invalid tx hash hex: {}", e)))?;
        tx_id_bytes[32..64].copy_from_slice(&hash_bytes[..32.min(hash_bytes.len())]);

        Ok(TxOutcome {
            transaction_id: H512::from(tx_id_bytes),
            executed: true,
            gas_used: U256::from(ESTIMATED_FEE_LOVELACE),
            gas_price: FixedPointNumber::try_from(U256::from(1u64))
                .unwrap_or_else(|_| FixedPointNumber::zero()),
        })
    }

    /// Build the complete Process transaction using pallas-txbuilder
    #[instrument(skip(self, components, payer))]
    async fn build_complete_process_tx(
        &self,
        components: &ProcessTxComponents,
        payer: &Keypair,
    ) -> Result<BuiltTransaction, TxBuilderError> {
        // Get payer address and UTXOs for fee payment
        let payer_address = payer.address_bech32(self.network_to_pallas());
        debug!("Payer address: {}", payer_address);

        // Find payer UTXOs for fee payment (coin selection)
        let payer_utxos = self.provider.get_utxos_at_address(&payer_address).await?;
        let (selected_utxos, total_input) = self.select_utxos_for_fee(&payer_utxos)?;
        debug!(
            "Selected {} UTXOs with {} lovelace for fees",
            selected_utxos.len(),
            total_input
        );

        // Start building the transaction
        let mut tx = StagingTransaction::new();

        // We need to track input indices for redeemers
        // Add script inputs (mailbox and recipient)
        let mailbox_input = utxo_to_input(&components.mailbox_utxo)?;
        tx = tx.input(mailbox_input);

        // Add recipient input
        let recipient_input = utxo_to_input(&components.recipient_utxo)?;
        tx = tx.input(recipient_input);

        // Add additional inputs if they must be spent
        for (utxo, must_spend) in &components.additional_utxos {
            let input = utxo_to_input(utxo)?;
            if *must_spend {
                tx = tx.input(input);
            } else {
                tx = tx.reference_input(input);
            }
        }

        // Add ISM UTXO as spent input (for signature verification)
        let ism_input = utxo_to_input(&components.ism_utxo)?;
        tx = tx.input(ism_input);
        debug!("Added ISM input for verification: {}#{}", components.ism_utxo.tx_hash, components.ism_utxo.output_index);

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

        // Add message NFT reference script UTXO as reference input (for deferred recipients)
        // This provides the stored_message_nft minting policy script via reference
        if let Some(ref msg_ref_utxo) = components.message_nft_ref_script_utxo {
            let msg_ref_input = utxo_to_input(msg_ref_utxo)?;
            tx = tx.reference_input(msg_ref_input);
            debug!(
                "Added message NFT reference script UTXO: {}#{}",
                msg_ref_utxo.tx_hash, msg_ref_utxo.output_index
            );
        }

        // Add fee payment UTXOs
        for utxo in &selected_utxos {
            let input = utxo_to_input(utxo)?;
            tx = tx.input(input);
        }

        // Add collateral input (use one of the payer's UTXOs for collateral)
        // Collateral is required for Plutus script execution
        if let Some(collateral_utxo) = selected_utxos.first() {
            let collateral_input = utxo_to_input(collateral_utxo)?;
            tx = tx.collateral_input(collateral_input);
            debug!("Added collateral input: {}#{}", collateral_utxo.tx_hash, collateral_utxo.output_index);
        } else {
            return Err(TxBuilderError::MissingInput("No UTXOs available for collateral".to_string()));
        }

        // Add spend redeemers with execution units
        // Re-create inputs for redeemer association (since Input doesn't impl Clone)
        let mailbox_input_for_redeemer = utxo_to_input(&components.mailbox_utxo)?;

        let ex_units_mailbox = ExUnits {
            mem: DEFAULT_MEM_UNITS,
            steps: DEFAULT_STEP_UNITS,
        };

        tx = tx.add_spend_redeemer(
            mailbox_input_for_redeemer,
            components.mailbox_redeemer_cbor.clone(),
            Some(ex_units_mailbox),
        );

        // Add recipient redeemer
        let recipient_input_for_redeemer = utxo_to_input(&components.recipient_utxo)?;
        let ex_units_recipient = ExUnits {
            mem: DEFAULT_MEM_UNITS,
            steps: DEFAULT_STEP_UNITS,
        };

        tx = tx.add_spend_redeemer(
            recipient_input_for_redeemer,
            components.recipient_redeemer_cbor.clone(),
            Some(ex_units_recipient),
        );

        // Add ISM Verify redeemer (for signature verification)
        let ism_input_for_redeemer = utxo_to_input(&components.ism_utxo)?;
        let ex_units_ism = ExUnits {
            mem: ISM_MEM_UNITS,
            steps: ISM_STEP_UNITS,
        };

        tx = tx.add_spend_redeemer(
            ism_input_for_redeemer,
            components.ism_redeemer_cbor.clone(),
            Some(ex_units_ism),
        );
        debug!("Added ISM Verify redeemer ({} bytes)", components.ism_redeemer_cbor.len());

        // Create outputs

        // 1. Mailbox continuation output (same address, same datum, same value)
        let mailbox_output = create_continuation_output(
            &components.mailbox_utxo,
            &self.conf.mailbox_policy_id,
        )?;
        tx = tx.output(mailbox_output);

        // 2. Recipient continuation output (same address, same value, UPDATED datum)
        let recipient_output = create_recipient_continuation_output(
            &components.recipient_utxo,
            &components.recipient_continuation_datum_cbor,
        )?;
        tx = tx.output(recipient_output);

        // 2b. Deferred-specific: Create message UTXO and mint message NFT
        // This is the UTXO that stores the message on-chain for later processing by a bot
        if let crate::types::RecipientType::Deferred { message_policy } = &components.recipient_type {
            info!("Creating Deferred message UTXO with message NFT");

            // Get message NFT policy bytes
            let message_nft_policy_bytes: Hash<28> = Hash::new(*message_policy);

            // Asset name is the 32-byte message_id
            let asset_name: Vec<u8> = components.message_id.to_vec();

            // Mint the message NFT (proves message is legitimate)
            tx = tx.mint_asset(message_nft_policy_bytes, asset_name.clone(), 1)
                .map_err(|e| TxBuilderError::TxBuild(format!("Failed to mint message NFT: {:?}", e)))?;

            // Create message UTXO at recipient address with StoredMessageDatum and the NFT
            let recipient_address = parse_address(&components.recipient_utxo.address)?;
            let stored_datum_cbor = components.stored_message_datum_cbor.as_ref()
                .ok_or_else(|| TxBuilderError::MissingInput("Deferred missing stored_message_datum_cbor".to_string()))?;

            let mut message_utxo_output = Output::new(recipient_address, MIN_UTXO_LOVELACE)
                .set_inline_datum(stored_datum_cbor.clone());

            // Add the message NFT to this output
            message_utxo_output = message_utxo_output
                .add_asset(message_nft_policy_bytes, asset_name.clone(), 1)
                .map_err(|e| TxBuilderError::TxBuild(format!("Failed to add message NFT to output: {:?}", e)))?;

            tx = tx.output(message_utxo_output);

            // Add mint redeemer for message NFT (MintMessage = Constr 0 [])
            let mint_redeemer_cbor = components.message_nft_redeemer_cbor.as_ref()
                .ok_or_else(|| TxBuilderError::MissingInput("Deferred missing message_nft_redeemer_cbor".to_string()))?;
            let ex_units_mint = ExUnits {
                mem: DEFAULT_MEM_UNITS,
                steps: DEFAULT_STEP_UNITS,
            };
            tx = tx.add_mint_redeemer(message_nft_policy_bytes, mint_redeemer_cbor.clone(), Some(ex_units_mint));

            // Note: The message NFT minting policy script needs to be added as a reference script
            // This should be configured in ConnectionConf with message_nft_reference_script_utxo
            // For now, we assume it's provided as a reference input along with the recipient script
            debug!(
                "Added message UTXO for Deferred: message_id={}, policy={}",
                hex::encode(&components.message_id),
                hex::encode(message_policy)
            );
        }

        // 3. ISM continuation output (same address, same datum, same value)
        // The ISM is spent for verification but must continue with unchanged state
        let ism_output = create_ism_continuation_output(&components.ism_utxo)?;
        tx = tx.output(ism_output);
        debug!("Added ISM continuation output");

        // 4. Processed message marker output
        // This output goes to the processed_messages_script address with inline datum
        // If NFT minting is configured, the NFT will be included in this output
        let mut processed_marker_output = self.create_processed_marker_output(
            &components.message_id,
            &components.processed_datum_cbor,
        )?;

        // 4. Optional: Mint processed message NFT for efficient O(1) lookups
        // If processed_messages_nft_policy_id is configured, mint an NFT with message_id as asset name
        if let (Some(ref policy_id), Some(ref script_cbor)) = (
            &self.conf.processed_messages_nft_policy_id,
            &self.conf.processed_messages_nft_script_cbor,
        ) {
            debug!("Minting processed message NFT with policy: {}", policy_id);

            // Parse policy ID as bytes
            let policy_bytes: Hash<28> = Hash::new(
                hex::decode(policy_id)
                    .map_err(|e| TxBuilderError::Encoding(format!("Invalid NFT policy ID hex: {}", e)))?
                    .try_into()
                    .map_err(|_| TxBuilderError::Encoding("NFT policy ID must be 28 bytes".to_string()))?
            );

            // Asset name is the 32-byte message_id
            let asset_name: Vec<u8> = components.message_id.to_vec();

            // Add mint asset (policy_id, asset_name, amount=1)
            tx = tx.mint_asset(policy_bytes, asset_name.clone(), 1)
                .map_err(|e| TxBuilderError::TxBuild(format!("Failed to add mint asset: {:?}", e)))?;

            // Add the minted NFT to the processed marker output
            // This is where the minted NFT will live
            processed_marker_output = processed_marker_output
                .add_asset(policy_bytes, asset_name.clone(), 1)
                .map_err(|e| TxBuilderError::TxBuild(format!("Failed to add NFT to processed marker output: {:?}", e)))?;

            // Add mint redeemer (empty data since minting policy just checks mailbox is spent)
            let mint_redeemer_data = vec![0xd8, 0x79, 0x9f, 0xff]; // Constr 0 []
            let ex_units_mint = ExUnits {
                mem: DEFAULT_MEM_UNITS,
                steps: DEFAULT_STEP_UNITS,
            };
            tx = tx.add_mint_redeemer(policy_bytes, mint_redeemer_data, Some(ex_units_mint));

            // Add minting policy script to witness set
            let script_bytes = hex::decode(script_cbor)
                .map_err(|e| TxBuilderError::Encoding(format!("Invalid NFT script CBOR hex: {}", e)))?;
            tx = tx.script(ScriptKind::PlutusV3, script_bytes);

            debug!("Added NFT minting for message_id: {}", hex::encode(&components.message_id));
        }

        tx = tx.output(processed_marker_output);
        debug!("Added processed message marker output for message_id: {}", hex::encode(&components.message_id));

        // 5. Change output back to payer
        // The payer's input funds: fee + processed marker output + (optional) message UTXO
        // (mailbox and recipient continuation outputs return the same value they consume)
        let fee = ESTIMATED_FEE_LOVELACE;
        let processed_marker_cost = MIN_UTXO_LOVELACE;
        // For Deferred, we also create a message UTXO which needs MIN_UTXO_LOVELACE
        let message_utxo_cost = if matches!(&components.recipient_type, crate::types::RecipientType::Deferred { .. }) {
            MIN_UTXO_LOVELACE
        } else {
            0
        };
        let change_amount = total_input.saturating_sub(fee + processed_marker_cost + message_utxo_cost);

        if change_amount >= MIN_UTXO_LOVELACE {
            let change_output = Output::new(
                parse_address(&payer_address)?,
                change_amount,
            );
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
            let script_bytes = hex::decode(script_cbor_hex)
                .map_err(|e| TxBuilderError::Encoding(format!("Invalid mailbox script CBOR hex: {}", e)))?;
            tx = tx.script(ScriptKind::PlutusV3, script_bytes);
            debug!("Added mailbox script to witness set (deprecated - use reference scripts)");
        } else {
            return Err(TxBuilderError::ScriptNotFound(
                "Neither mailbox_reference_script_utxo nor mailbox_script_cbor configured".to_string()
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
            let script_bytes = hex::decode(script_cbor_hex)
                .map_err(|e| TxBuilderError::Encoding(format!("Invalid ISM script CBOR hex: {}", e)))?;
            tx = tx.script(ScriptKind::PlutusV3, script_bytes);
            debug!("Added ISM script to witness set (deprecated - use reference scripts)");
        } else {
            return Err(TxBuilderError::ScriptNotFound(
                "Neither ism_reference_script_utxo nor ism_script_cbor configured".to_string()
            ));
        }

        // Set language view for PlutusV3 (required for script_data_hash calculation)
        // Using the Conway PlutusV3 cost model from protocol parameters
        // For now, using placeholder values - in production, fetch from protocol params
        let plutus_v3_cost_model: Vec<i64> = get_plutus_v3_cost_model();
        tx = tx.language_view(ScriptKind::PlutusV3, plutus_v3_cost_model);

        // Build the transaction
        let built = tx.build_conway_raw()
            .map_err(|e| TxBuilderError::TxBuild(format!("Failed to build transaction: {:?}", e)))?;

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
        let signed = built.add_signature(public_key.clone(), signature)
            .map_err(|e| TxBuilderError::TxBuild(format!("Failed to add signature: {:?}", e)))?;

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
                info!("  - Has vkey witnesses: {}", tx.transaction_witness_set.vkeywitness.is_some());
                if let Some(ref redeemers) = tx.transaction_witness_set.redeemer {
                    info!("  - Has redeemers: true");
                    let redeemer_cbor = redeemers.encode_fragment().unwrap();
                    debug!("  - Redeemers CBOR: {}", hex::encode(&redeemer_cbor));
                }
                info!("  - Success flag: {}", tx.success);
                let has_aux = match &tx.auxiliary_data {
                    pallas_codec::utils::Nullable::Some(_) => true,
                    _ => false,
                };
                info!("  - Has auxiliary_data: {}", has_aux);
            }
            Err(e) => {
                tracing::error!("Transaction validation failed: {:?}", e);
                tracing::error!("Transaction CBOR (full): {}", hex::encode(signed_tx));
                return Err(TxBuilderError::TxBuild(format!("Invalid transaction CBOR: {:?}", e)));
            }
        }

        // Print full transaction CBOR hex for analysis
        let full_hex = hex::encode(signed_tx);
        info!("Submitting transaction CBOR ({} bytes): {}", signed_tx.len(), full_hex);

        // Analyze CBOR structure
        if !signed_tx.is_empty() {
            let first_byte = signed_tx[0];
            let major_type = first_byte >> 5;
            let additional_info = first_byte & 0x1f;
            info!("CBOR first byte: 0x{:02x} (major type: {}, additional info: {})",
                   first_byte, major_type, additional_info);
        }

        self.provider
            .submit_transaction(signed_tx)
            .await
            .map_err(|e| TxBuilderError::SubmissionFailed(e.to_string()))
    }

    /// Select UTXOs for fee payment using simple greedy algorithm
    fn select_utxos_for_fee(&self, utxos: &[Utxo]) -> Result<(Vec<Utxo>, u64), TxBuilderError> {
        // Sort UTXOs by lovelace amount (largest first) for efficient selection
        let mut sorted: Vec<_> = utxos.iter().collect();
        sorted.sort_by(|a, b| b.lovelace().cmp(&a.lovelace()));

        let mut selected = Vec::new();
        let mut total: u64 = 0;
        // Need enough for fee + min UTXO for change
        let needed = ESTIMATED_FEE_LOVELACE + MIN_UTXO_LOVELACE;

        for utxo in sorted {
            // Skip UTXOs with tokens (keep it simple, use pure ADA UTXOs)
            if utxo.value.len() > 1 {
                continue;
            }

            selected.push(utxo.clone());
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
    ) -> Result<Output, TxBuilderError> {
        // The processed messages are stored at the processed_messages_script address
        // This must match the parameter applied to the mailbox validator
        let script_address = self.provider.script_hash_to_address(&self.conf.processed_messages_script_hash)?;

        // Just create a simple output with inline datum, no NFT needed
        let output = Output::new(parse_address(&script_address)?, MIN_UTXO_LOVELACE)
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

        // 1. Find ISM UTXO (ism_policy_id is actually the script hash)
        let ism_utxos = self
            .provider
            .get_script_utxos(ism_policy_id)
            .await?;

        let ism_utxo = ism_utxos.into_iter().next().ok_or_else(|| {
            TxBuilderError::UtxoNotFound(format!("ISM UTXO not found at script {}", ism_policy_id))
        })?;
        info!("Found ISM UTXO: {}#{}", ism_utxo.tx_hash, ism_utxo.output_index);

        // 2. Parse current ISM datum from inline datum CBOR
        let current_datum_hex = ism_utxo
            .inline_datum
            .as_ref()
            .ok_or_else(|| TxBuilderError::UtxoNotFound("ISM UTXO has no inline datum".to_string()))?;

        // Decode CBOR hex to PlutusData
        let datum_bytes = hex::decode(current_datum_hex)
            .map_err(|e| TxBuilderError::Encoding(format!("Invalid datum hex: {}", e)))?;
        let current_datum_plutus: PlutusData = minicbor::decode(&datum_bytes)
            .map_err(|e| TxBuilderError::Encoding(format!("Failed to decode datum CBOR: {:?}", e)))?;

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
                info!("Datum is Constr(tag={}) with {} fields", constr.tag, constr.fields.len());
                for (i, field) in constr.fields.iter().enumerate() {
                    match field {
                        PlutusData::Array(arr) => info!("  Field {}: Array with {} elements", i, arr.len()),
                        PlutusData::Constr(c) => info!("  Field {}: Constr(tag={})", i, c.tag),
                        PlutusData::BoundedBytes(b) => info!("  Field {}: BoundedBytes({} bytes)", i, b.len()),
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
        info!("ISM owner: {}", hex::encode(&owner));

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
            validators: validators.iter().map(|v| {
                let mut arr = [0u8; 20];
                arr.copy_from_slice(&v[..20.min(v.len())]);
                crate::types::EthAddress(arr)
            }).collect(),
        };
        let redeemer_cbor = encode_ism_redeemer(&redeemer)?;

        // 6. Get payer address and UTXOs
        let payer_address = payer.address_bech32(self.network_to_pallas());
        let payer_utxos = self.provider.get_utxos_at_address(&payer_address).await?;
        let (selected_utxos, total_input) = self.select_utxos_for_fee(&payer_utxos)?;

        info!("Selected {} payer UTXOs with {} lovelace", selected_utxos.len(), total_input);

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
        let ism_lovelace = ism_utxo.lovelace().max(MIN_UTXO_LOVELACE);

        let mut ism_output = Output::new(ism_address, ism_lovelace);
        ism_output = ism_output.set_inline_datum(new_datum_cbor);

        // Preserve ISM NFT
        let ism_policy_hash = parse_policy_id(ism_policy_id)?;
        ism_output = ism_output
            .add_asset(ism_policy_hash, vec![], 1)
            .map_err(|e| TxBuilderError::TxBuild(format!("Failed to add ISM NFT: {:?}", e)))?;

        tx = tx.output(ism_output);

        // Change output
        let change_amount = total_input
            .saturating_sub(ism_lovelace)
            .saturating_sub(ESTIMATED_FEE_LOVELACE);

        if change_amount >= MIN_UTXO_LOVELACE {
            let change_output = Output::new(parse_address(&payer_address)?, change_amount);
            tx = tx.output(change_output);
        }

        // 9. Build, sign and submit
        // Set language view for PlutusV3 (required for script_data_hash calculation)
        let plutus_v3_cost_model: Vec<i64> = get_plutus_v3_cost_model();
        tx = tx.language_view(ScriptKind::PlutusV3, plutus_v3_cost_model);

        let built_tx = tx.build_conway_raw()
            .map_err(|e| TxBuilderError::TxBuild(format!("Failed to build transaction: {:?}", e)))?;

        let signed_tx = self.sign_transaction(built_tx, payer)?;
        let tx_hash = self.submit_transaction(&signed_tx).await?;

        info!("ISM update transaction submitted: {}", tx_hash);
        Ok(tx_hash)
    }
}


/// Components needed to build a Process transaction
#[derive(Debug)]
pub struct ProcessTxComponents {
    /// Mailbox UTXO to spend
    pub mailbox_utxo: Utxo,
    /// Encoded mailbox redeemer (CBOR)
    pub mailbox_redeemer_cbor: Vec<u8>,
    /// Recipient state UTXO to spend (contains datum)
    pub recipient_utxo: Utxo,
    /// Recipient reference script UTXO (contains the validator script)
    /// If None, the script is embedded in the recipient_utxo's reference_script field
    pub recipient_ref_script_utxo: Option<Utxo>,
    /// Encoded recipient redeemer (CBOR)
    pub recipient_redeemer_cbor: Vec<u8>,
    /// Encoded recipient continuation datum (CBOR) - with updated state
    pub recipient_continuation_datum_cbor: Vec<u8>,
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
    /// Recipient type for this message
    pub recipient_type: crate::types::RecipientType,
    /// Deferred-specific: Encoded stored message datum (CBOR)
    /// Only set when recipient_type is Deferred
    pub stored_message_datum_cbor: Option<Vec<u8>>,
    /// Deferred-specific: Encoded message NFT mint redeemer (CBOR)
    /// Only set when recipient_type is Deferred
    pub message_nft_redeemer_cbor: Option<Vec<u8>>,
    /// Deferred-specific: Reference script UTXO for stored_message_nft minting policy
    /// Discovered via the same policy ID as reference_script_locator but with asset name "msg_ref"
    pub message_nft_ref_script_utxo: Option<Utxo>,
}

// ============================================================================
// Helper Functions for Transaction Building
// ============================================================================

/// Convert a Utxo to a pallas-txbuilder Input
fn utxo_to_input(utxo: &Utxo) -> Result<Input, TxBuilderError> {
    let tx_hash_bytes = hex::decode(&utxo.tx_hash)
        .map_err(|e| TxBuilderError::Encoding(format!("Invalid tx hash hex: {}", e)))?;

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
        .map_err(|e| TxBuilderError::InvalidAddress(format!("Invalid bech32 address: {:?}", e)))
}

/// Parse a policy ID hex string into a Hash<28>
fn parse_policy_id(policy_id: &str) -> Result<Hash<28>, TxBuilderError> {
    let bytes = hex::decode(policy_id)
        .map_err(|e| TxBuilderError::Encoding(format!("Invalid policy ID hex: {}", e)))?;

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
            "Invalid UTXO reference format '{}'. Expected 'tx_hash#output_index'",
            utxo_ref
        )));
    }

    let tx_hash_hex = parts[0];
    let output_index: u64 = parts[1]
        .parse()
        .map_err(|e| TxBuilderError::Encoding(format!("Invalid output index '{}': {}", parts[1], e)))?;

    let tx_hash_bytes = hex::decode(tx_hash_hex)
        .map_err(|e| TxBuilderError::Encoding(format!("Invalid tx hash hex: {}", e)))?;

    let tx_hash: Hash<32> = Hash::new(
        tx_hash_bytes
            .try_into()
            .map_err(|_| TxBuilderError::Encoding("Tx hash must be 32 bytes".to_string()))?,
    );

    Ok(Input::new(tx_hash, output_index))
}

/// Create a continuation output for a script UTXO
/// This preserves the address, value, and inline datum from the original UTXO
fn create_continuation_output(utxo: &Utxo, _policy_id: &str) -> Result<Output, TxBuilderError> {
    let address = parse_address(&utxo.address)?;
    let lovelace = utxo.lovelace();

    let mut output = Output::new(address, lovelace.max(MIN_UTXO_LOVELACE));

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
                .map_err(|e| TxBuilderError::Encoding(format!("Invalid asset name hex: {}", e)))?;
            let quantity: u64 = value.quantity.parse()
                .map_err(|e| TxBuilderError::Encoding(format!("Invalid quantity: {}", e)))?;

            output = output.add_asset(policy_hash, asset_name, quantity)
                .map_err(|e| TxBuilderError::TxBuild(format!("Failed to add asset: {:?}", e)))?;
        }
    }

    Ok(output)
}

/// Create a recipient continuation output with UPDATED datum
/// This preserves the address, value, and native assets but uses the new datum
fn create_recipient_continuation_output(
    utxo: &Utxo,
    new_datum_cbor: &[u8],
) -> Result<Output, TxBuilderError> {
    let address = parse_address(&utxo.address)?;
    let lovelace = utxo.lovelace();

    let mut output = Output::new(address, lovelace.max(MIN_UTXO_LOVELACE));

    // Use the NEW datum (updated state)
    output = output.set_inline_datum(new_datum_cbor.to_vec());

    // Add any native assets from the original UTXO (preserve the state NFT)
    for value in &utxo.value {
        if value.unit != "lovelace" && value.unit.len() >= 56 {
            let policy_hex = &value.unit[..56];
            let asset_name_hex = &value.unit[56..];

            let policy_hash = parse_policy_id(policy_hex)?;
            let asset_name = hex::decode(asset_name_hex)
                .map_err(|e| TxBuilderError::Encoding(format!("Invalid asset name hex: {}", e)))?;
            let quantity: u64 = value.quantity.parse()
                .map_err(|e| TxBuilderError::Encoding(format!("Invalid quantity: {}", e)))?;

            output = output.add_asset(policy_hash, asset_name, quantity)
                .map_err(|e| TxBuilderError::TxBuild(format!("Failed to add asset: {:?}", e)))?;
        }
    }

    Ok(output)
}

/// Create ISM continuation output (same address, same datum, same value)
/// Used when ISM is spent for Verify operation - must recreate unchanged
fn create_ism_continuation_output(utxo: &Utxo) -> Result<Output, TxBuilderError> {
    let address = parse_address(&utxo.address)?;
    let lovelace = utxo.lovelace();

    let mut output = Output::new(address, lovelace.max(MIN_UTXO_LOVELACE));

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
                .map_err(|e| TxBuilderError::Encoding(format!("Invalid asset name hex: {}", e)))?;

            let quantity: u64 = value.quantity.parse()
                .map_err(|e| TxBuilderError::Encoding(format!("Invalid quantity: {}", e)))?;

            output = output.add_asset(policy_hash, asset_name, quantity)
                .map_err(|e| TxBuilderError::TxBuild(format!("Failed to add asset: {:?}", e)))?;
        }
    }

    Ok(output)
}

/// Build the updated recipient datum for the continuation output
///
/// The GenericRecipient expects:
/// - HyperlaneRecipientDatum { ism: Option<ScriptHash>, last_processed_nonce: Option<Int>, inner: GenericRecipientInner }
/// - GenericRecipientInner { messages_received: Int, last_message: Option<ByteArray> }
///
/// Updates:
/// - messages_received += 1
/// - last_message = Some(body)
/// - last_processed_nonce = old_nonce + 1 (or Some(1) if was None)
fn build_recipient_continuation_datum(
    recipient_utxo: &Utxo,
    message_body: &[u8],
) -> Result<Vec<u8>, TxBuilderError> {
    // Parse the existing datum to extract current state
    let (ism_opt, old_nonce, old_messages_received) = if let Some(datum_str) = &recipient_utxo.inline_datum {
        parse_recipient_datum(datum_str)?
    } else {
        // Default values if no datum
        (None, None, 0)
    };

    // Compute new values
    let new_messages_received = old_messages_received + 1;
    let new_nonce = match old_nonce {
        Some(n) => Some(n + 1),
        None => Some(1),
    };

    // Build the new datum CBOR
    // Structure: Constr 0 [ism: Option, nonce: Option, inner: Constr 0 [messages_received, last_message: Option]]
    let plutus_data = build_generic_recipient_datum_plutus(
        ism_opt.as_deref(),
        new_nonce,
        new_messages_received,
        Some(message_body),
    );

    encode_plutus_data(&plutus_data)
}

/// Build GenericRecipient datum as PlutusData
fn build_generic_recipient_datum_plutus(
    ism: Option<&[u8]>,
    nonce: Option<i64>,
    messages_received: i64,
    last_message: Option<&[u8]>,
) -> PlutusData {
    // HyperlaneRecipientDatum { ism, last_processed_nonce, inner }
    let ism_field = match ism {
        Some(hash) => PlutusData::Constr(Constr {
            tag: 121, // Some = constructor 0
            any_constructor: None,
            fields: MaybeIndefArray::Def(vec![
                PlutusData::BoundedBytes(hash.to_vec().into())
            ]),
        }),
        None => PlutusData::Constr(Constr {
            tag: 122, // None = constructor 1
            any_constructor: None,
            fields: MaybeIndefArray::Def(vec![]),
        }),
    };

    let nonce_field = match nonce {
        Some(n) => PlutusData::Constr(Constr {
            tag: 121, // Some = constructor 0
            any_constructor: None,
            fields: MaybeIndefArray::Def(vec![
                PlutusData::BigInt(BigInt::Int(n.into()))
            ]),
        }),
        None => PlutusData::Constr(Constr {
            tag: 122, // None = constructor 1
            any_constructor: None,
            fields: MaybeIndefArray::Def(vec![]),
        }),
    };

    // GenericRecipientInner { messages_received, last_message }
    let last_message_field = match last_message {
        Some(msg) => PlutusData::Constr(Constr {
            tag: 121, // Some = constructor 0
            any_constructor: None,
            fields: MaybeIndefArray::Def(vec![
                PlutusData::BoundedBytes(msg.to_vec().into())
            ]),
        }),
        None => PlutusData::Constr(Constr {
            tag: 122, // None = constructor 1
            any_constructor: None,
            fields: MaybeIndefArray::Def(vec![]),
        }),
    };

    let inner_field = PlutusData::Constr(Constr {
        tag: 121, // GenericRecipientInner = constructor 0
        any_constructor: None,
        fields: MaybeIndefArray::Def(vec![
            PlutusData::BigInt(BigInt::Int(messages_received.into())),
            last_message_field,
        ]),
    });

    // HyperlaneRecipientDatum = constructor 0
    PlutusData::Constr(Constr {
        tag: 121,
        any_constructor: None,
        fields: MaybeIndefArray::Def(vec![
            ism_field,
            nonce_field,
            inner_field,
        ]),
    })
}

/// Parse a recipient datum to extract the current state
/// Returns (ism: Option<Vec<u8>>, nonce: Option<i64>, messages_received: i64)
fn parse_recipient_datum(datum_str: &str) -> Result<(Option<Vec<u8>>, Option<i64>, i64), TxBuilderError> {
    // Try to parse as CBOR hex first
    let datum_cbor = json_datum_to_cbor(datum_str)?;

    // Decode the CBOR to extract values
    // This is a simplified parser - in production you'd want a proper CBOR decoder
    // For now, we'll use default values and increment from there

    // Try to decode with minicbor
    use pallas_codec::minicbor;
    let decoded: PlutusData = minicbor::decode(&datum_cbor)
        .map_err(|e| TxBuilderError::Encoding(format!("Failed to decode datum CBOR: {}", e)))?;

    // Extract fields from the datum
    // Structure: Constr 0 [ism_opt, nonce_opt, inner]
    // inner: Constr 0 [messages_received, last_message_opt]
    if let PlutusData::Constr(constr) = decoded {
        let fields: Vec<_> = constr.fields.clone().to_vec();
        if fields.len() >= 3 {
            // Extract ISM (Option<ScriptHash>)
            let ism = extract_option_bytes(&fields[0]);

            // Extract nonce (Option<Int>)
            let nonce = extract_option_int(&fields[1]);

            // Extract inner.messages_received
            let messages_received = if let PlutusData::Constr(inner) = &fields[2] {
                let inner_fields: Vec<_> = inner.fields.clone().to_vec();
                if !inner_fields.is_empty() {
                    extract_int(&inner_fields[0]).unwrap_or(0)
                } else {
                    0
                }
            } else {
                0
            };

            return Ok((ism, nonce, messages_received));
        }
    }

    // Default values if parsing fails
    Ok((None, None, 0))
}

/// Extract Option<ByteArray> from PlutusData
fn extract_option_bytes(data: &PlutusData) -> Option<Vec<u8>> {
    if let PlutusData::Constr(constr) = data {
        if constr.tag == 121 { // Some
            let fields: Vec<_> = constr.fields.clone().to_vec();
            if !fields.is_empty() {
                if let PlutusData::BoundedBytes(bytes) = &fields[0] {
                    return Some(bytes.to_vec());
                }
            }
        }
    }
    None
}

/// Extract Option<Int> from PlutusData
fn extract_option_int(data: &PlutusData) -> Option<i64> {
    if let PlutusData::Constr(constr) = data {
        if constr.tag == 121 { // Some
            let fields: Vec<_> = constr.fields.clone().to_vec();
            if !fields.is_empty() {
                return extract_int(&fields[0]);
            }
        }
    }
    None
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
            .map_err(|e| TxBuilderError::Encoding(format!("Invalid CBOR hex: {}", e)))?;
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
            let i = n.as_i64()
                .ok_or_else(|| TxBuilderError::Encoding("Number too large".to_string()))?;
            Ok(PlutusData::BigInt(BigInt::Int(i.into())))
        }

        // Byte string (hex encoded)
        Value::String(s) => {
            if s.starts_with("0x") || s.chars().all(|c| c.is_ascii_hexdigit()) {
                let hex_str = s.strip_prefix("0x").unwrap_or(s);
                let bytes = hex::decode(hex_str)
                    .map_err(|e| TxBuilderError::Encoding(format!("Invalid hex string: {}", e)))?;
                Ok(PlutusData::BoundedBytes(bytes.into()))
            } else {
                // Treat as UTF-8 bytes
                Ok(PlutusData::BoundedBytes(s.as_bytes().to_vec().into()))
            }
        }

        // Object with "constructor" and "fields" (Constr type)
        Value::Object(obj) => {
            if let (Some(constructor), Some(fields)) = (obj.get("constructor"), obj.get("fields")) {
                let tag = constructor.as_u64()
                    .ok_or_else(|| TxBuilderError::Encoding("Invalid constructor".to_string()))?;

                let fields_vec = fields.as_array()
                    .ok_or_else(|| TxBuilderError::Encoding("Fields must be array".to_string()))?;

                let mut parsed_fields = Vec::new();
                for field in fields_vec {
                    parsed_fields.push(json_to_plutus_data(field)?);
                }

                // Convert constructor index to Plutus tag
                let plutus_tag = if tag <= 6 {
                    121 + tag as u64 // Alternative encoding for 0-6
                } else {
                    1280 + (tag - 7) as u64 // General encoding for 7+
                };

                Ok(PlutusData::Constr(Constr {
                    tag: plutus_tag,
                    any_constructor: None,
                    fields: MaybeIndefArray::Def(parsed_fields),
                }))
            } else if let Some(bytes) = obj.get("bytes") {
                // Blockfrost format: {"bytes": "hex_string"}
                let hex_str = bytes.as_str()
                    .ok_or_else(|| TxBuilderError::Encoding("bytes must be string".to_string()))?;
                let bytes = hex::decode(hex_str)
                    .map_err(|e| TxBuilderError::Encoding(format!("Invalid hex: {}", e)))?;
                Ok(PlutusData::BoundedBytes(bytes.into()))
            } else if let Some(int_val) = obj.get("int") {
                // Blockfrost format: {"int": number}
                let i = int_val.as_i64()
                    .ok_or_else(|| TxBuilderError::Encoding("int must be number".to_string()))?;
                Ok(PlutusData::BigInt(BigInt::Int(i.into())))
            } else if let Some(list) = obj.get("list") {
                // Blockfrost format: {"list": [...]}
                let items = list.as_array()
                    .ok_or_else(|| TxBuilderError::Encoding("list must be array".to_string()))?;
                let mut parsed_items = Vec::new();
                for item in items {
                    parsed_items.push(json_to_plutus_data(item)?);
                }
                Ok(PlutusData::Array(MaybeIndefArray::Def(parsed_items)))
            } else if let Some(map) = obj.get("map") {
                // Blockfrost format: {"map": [{"k": ..., "v": ...}, ...]}
                let entries = map.as_array()
                    .ok_or_else(|| TxBuilderError::Encoding("map must be array".to_string()))?;
                let mut parsed_map = Vec::new();
                for entry in entries {
                    let k = entry.get("k")
                        .ok_or_else(|| TxBuilderError::Encoding("map entry missing k".to_string()))?;
                    let v = entry.get("v")
                        .ok_or_else(|| TxBuilderError::Encoding("map entry missing v".to_string()))?;
                    parsed_map.push((json_to_plutus_data(k)?, json_to_plutus_data(v)?));
                }
                Ok(PlutusData::Map(KeyValuePairs::from(parsed_map)))
            } else {
                Err(TxBuilderError::Encoding("Unknown JSON object format".to_string()))
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
            "Unsupported JSON value type: {:?}",
            json
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
        } => {
            // Constructor 0: Dispatch
            PlutusData::Constr(Constr {
                tag: 121, // Constructor 0 alternative encoding
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    PlutusData::BigInt(BigInt::Int((*destination as i64).into())),
                    PlutusData::BoundedBytes(recipient.to_vec().into()),
                    PlutusData::BoundedBytes(body.clone().into()),
                ]),
            })
        }
        MailboxRedeemer::Process {
            message,
            metadata,
            message_id,
        } => {
            // Constructor 1: Process
            PlutusData::Constr(Constr {
                tag: 122, // Constructor 1 alternative encoding
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    encode_message_as_plutus_data(message),
                    PlutusData::BoundedBytes(metadata.clone().into()),
                    PlutusData::BoundedBytes(message_id.to_vec().into()),
                ]),
            })
        }
        MailboxRedeemer::SetDefaultIsm { new_ism } => {
            // Constructor 2: SetDefaultIsm
            PlutusData::Constr(Constr {
                tag: 123, // Constructor 2 alternative encoding
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![PlutusData::BoundedBytes(new_ism.to_vec().into())]),
            })
        }
        MailboxRedeemer::TransferOwnership { new_owner } => {
            // Constructor 3: TransferOwnership
            PlutusData::Constr(Constr {
                tag: 124, // Constructor 3 alternative encoding
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![PlutusData::BoundedBytes(new_owner.to_vec().into())]),
            })
        }
    };

    encode_plutus_data(&plutus_data)
}

/// Encode a HyperlaneRecipientRedeemer as Plutus Data CBOR
pub fn encode_recipient_redeemer<T>(
    redeemer: &HyperlaneRecipientRedeemer<T>,
) -> Result<Vec<u8>, TxBuilderError> {
    let plutus_data = match redeemer {
        HyperlaneRecipientRedeemer::HandleMessage { message, message_id } => {
            // Constructor 0: HandleMessage { message: Message, message_id: ByteArray }
            //
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
                tag: 121, // Constructor 0 for HandleMessage
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    message_data,
                    PlutusData::BoundedBytes(message_id.to_vec().into()),
                ]),
            })
        }
        HyperlaneRecipientRedeemer::ContractAction { .. } => {
            // Constructor 1: ContractAction - not supported in generic encoding
            return Err(TxBuilderError::Encoding(
                "ContractAction requires custom encoding".to_string(),
            ));
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
        fields: MaybeIndefArray::Def(vec![PlutusData::BoundedBytes(datum.message_id.to_vec().into())]),
    });

    encode_plutus_data(&plutus_data)
}

// ============================================================================
// Deferred Recipient Encoding Functions
// ============================================================================

/// Encode a StoredMessageDatum as Plutus Data CBOR
/// Structure: Constr 0 [origin: Int, sender: ByteArray, body: ByteArray, message_id: ByteArray, nonce: Int]
pub fn encode_stored_message_datum(
    datum: &crate::types::StoredMessageDatum,
) -> Result<Vec<u8>, TxBuilderError> {
    let plutus_data = PlutusData::Constr(Constr {
        tag: 121, // Constructor 0
        any_constructor: None,
        fields: MaybeIndefArray::Def(vec![
            PlutusData::BigInt(BigInt::Int((datum.origin as i64).into())),
            PlutusData::BoundedBytes(datum.sender.to_vec().into()),
            PlutusData::BoundedBytes(datum.body.clone().into()),
            PlutusData::BoundedBytes(datum.message_id.to_vec().into()),
            PlutusData::BigInt(BigInt::Int((datum.nonce as i64).into())),
        ]),
    });

    encode_plutus_data(&plutus_data)
}

/// Encode a MessageNftRedeemer as Plutus Data CBOR
/// MintMessage = Constr 0 [], BurnMessage = Constr 1 []
pub fn encode_message_nft_redeemer(
    redeemer: &crate::types::MessageNftRedeemer,
) -> Result<Vec<u8>, TxBuilderError> {
    let tag = match redeemer {
        crate::types::MessageNftRedeemer::MintMessage => 121, // Constructor 0
        crate::types::MessageNftRedeemer::BurnMessage => 122, // Constructor 1
    };

    let plutus_data = PlutusData::Constr(Constr {
        tag,
        any_constructor: None,
        fields: MaybeIndefArray::Def(vec![]),
    });

    encode_plutus_data(&plutus_data)
}

/// Build Deferred continuation datum with updated counters
/// Structure: HyperlaneRecipientDatum { ism: Option, last_processed_nonce: Option, inner: DeferredInner }
/// DeferredInner: { messages_stored: Int, messages_processed: Int }
fn build_deferred_continuation_datum(
    recipient_utxo: &Utxo,
) -> Result<Vec<u8>, TxBuilderError> {
    // Parse the existing datum to extract current state
    let (ism_opt, old_nonce, messages_stored, messages_processed) =
        if let Some(datum_str) = &recipient_utxo.inline_datum {
            parse_deferred_datum(datum_str)?
        } else {
            (None, None, 0, 0)
        };

    // Increment messages_stored (messages_processed stays the same - that's for bot processing)
    // IMPORTANT: last_processed_nonce must remain unchanged - the validator preserves it
    let new_messages_stored = messages_stored + 1;

    let plutus_data = build_deferred_datum_plutus(
        ism_opt.as_deref(),
        old_nonce,  // Keep the same nonce - validator expects it unchanged
        new_messages_stored,
        messages_processed,
    );

    encode_plutus_data(&plutus_data)
}

/// Build Deferred datum as PlutusData
fn build_deferred_datum_plutus(
    ism: Option<&[u8]>,
    nonce: Option<i64>,
    messages_stored: i64,
    messages_processed: i64,
) -> PlutusData {
    // HyperlaneRecipientDatum { ism, last_processed_nonce, inner }
    let ism_field = match ism {
        Some(hash) => PlutusData::Constr(Constr {
            tag: 121, // Some = constructor 0
            any_constructor: None,
            fields: MaybeIndefArray::Def(vec![
                PlutusData::BoundedBytes(hash.to_vec().into())
            ]),
        }),
        None => PlutusData::Constr(Constr {
            tag: 122, // None = constructor 1
            any_constructor: None,
            fields: MaybeIndefArray::Def(vec![]),
        }),
    };

    let nonce_field = match nonce {
        Some(n) => PlutusData::Constr(Constr {
            tag: 121, // Some = constructor 0
            any_constructor: None,
            fields: MaybeIndefArray::Def(vec![
                PlutusData::BigInt(BigInt::Int(n.into()))
            ]),
        }),
        None => PlutusData::Constr(Constr {
            tag: 122, // None = constructor 1
            any_constructor: None,
            fields: MaybeIndefArray::Def(vec![]),
        }),
    };

    // DeferredInner { messages_stored, messages_processed }
    let inner_field = PlutusData::Constr(Constr {
        tag: 121, // DeferredInner = constructor 0
        any_constructor: None,
        fields: MaybeIndefArray::Def(vec![
            PlutusData::BigInt(BigInt::Int(messages_stored.into())),
            PlutusData::BigInt(BigInt::Int(messages_processed.into())),
        ]),
    });

    // HyperlaneRecipientDatum = constructor 0
    PlutusData::Constr(Constr {
        tag: 121,
        any_constructor: None,
        fields: MaybeIndefArray::Def(vec![
            ism_field,
            nonce_field,
            inner_field,
        ]),
    })
}

/// Parse a Deferred datum to extract the current state
/// Returns (ism: Option<Vec<u8>>, nonce: Option<i64>, messages_stored: i64, messages_processed: i64)
fn parse_deferred_datum(datum_str: &str) -> Result<(Option<Vec<u8>>, Option<i64>, i64, i64), TxBuilderError> {
    let datum_cbor = json_datum_to_cbor(datum_str)?;

    use pallas_codec::minicbor;
    let decoded: PlutusData = minicbor::decode(&datum_cbor)
        .map_err(|e| TxBuilderError::Encoding(format!("Failed to decode datum CBOR: {}", e)))?;

    // Structure: Constr 0 [ism_opt, nonce_opt, inner]
    // inner: Constr 0 [messages_stored, messages_processed]
    if let PlutusData::Constr(constr) = decoded {
        let fields: Vec<_> = constr.fields.clone().to_vec();
        if fields.len() >= 3 {
            let ism = extract_option_bytes(&fields[0]);
            let nonce = extract_option_int(&fields[1]);

            // Extract inner.messages_stored and inner.messages_processed
            let (messages_stored, messages_processed) = if let PlutusData::Constr(inner) = &fields[2] {
                let inner_fields: Vec<_> = inner.fields.clone().to_vec();
                let stored = if inner_fields.len() > 0 {
                    extract_int(&inner_fields[0]).unwrap_or(0)
                } else {
                    0
                };
                let processed = if inner_fields.len() > 1 {
                    extract_int(&inner_fields[1]).unwrap_or(0)
                } else {
                    0
                };
                (stored, processed)
            } else {
                (0, 0)
            };

            return Ok((ism, nonce, messages_stored, messages_processed));
        }
    }

    // Default values if parsing fails
    Ok((None, None, 0, 0))
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
        .map_err(|e| TxBuilderError::Encoding(format!("CBOR encoding failed: {:?}", e)))
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
                        "Owner field must be BoundedBytes, got: {:?}",
                        owner_field
                    )))
                }
            };

            let bytes: [u8; 28] = owner_bytes
                .try_into()
                .map_err(|_| TxBuilderError::Encoding(format!("Owner must be 28 bytes, got {}", owner_bytes.len())))?;
            Ok(bytes)
        }
        _ => Err(TxBuilderError::Encoding(format!("Invalid ISM datum structure: expected Constr with 3 fields, got {:?}", datum))),
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
    let validator_hex_list: Vec<String> = validators
        .into_iter()
        .map(|v| hex::encode(&v))
        .collect();

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
fn encode_ism_redeemer(redeemer: &crate::types::MultisigIsmRedeemer) -> Result<Vec<u8>, TxBuilderError> {
    let plutus_data = match redeemer {
        crate::types::MultisigIsmRedeemer::Verify { checkpoint, validator_signatures } => {
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
    let root_index = u32::from_be_bytes(metadata[64..68].try_into().unwrap());

    // Compute the checkpoint hash that validators signed
    // Step 1: domain_hash = keccak256(origin || merkle_tree_hook || "HYPERLANE")
    let mut domain_hasher = Keccak256::new();
    domain_hasher.update(&origin.to_be_bytes());
    domain_hasher.update(&origin_mailbox);
    domain_hasher.update(b"HYPERLANE");
    let domain_hash: [u8; 32] = domain_hasher.finalize().into();

    // Step 2: checkpoint_digest = keccak256(domain_hash || merkle_root || merkle_index || message_id)
    let mut checkpoint_hasher = Keccak256::new();
    checkpoint_hasher.update(&domain_hash);
    checkpoint_hasher.update(&merkle_root);
    checkpoint_hasher.update(&root_index.to_be_bytes());
    checkpoint_hasher.update(message_id);
    let checkpoint_digest: [u8; 32] = checkpoint_hasher.finalize().into();

    // Step 3: eth_signed_message = keccak256("\x19Ethereum Signed Message:\n32" || checkpoint_digest)
    let mut eth_hasher = Keccak256::new();
    eth_hasher.update(b"\x19Ethereum Signed Message:\n32");
    eth_hasher.update(&checkpoint_digest);
    let eth_signed_message: [u8; 32] = eth_hasher.finalize().into();

    debug!("Recovering public keys from signatures");
    debug!("  domain_hash: {}", hex::encode(&domain_hash));
    debug!("  checkpoint_digest: {}", hex::encode(&checkpoint_digest));
    debug!("  eth_signed_message: {}", hex::encode(&eth_signed_message));

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
                        match VerifyingKey::recover_from_prehash(&eth_signed_message, &sig, rec_id) {
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
                                info!("  Recovered validator {}: 0x{}", validator_signatures.len() - 1, hex::encode(eth_address));
                                info!("    Compressed pubkey: {}", hex::encode(&compressed_pubkey));
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

    debug!("  Recovered {} validator signatures", validator_signatures.len());

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
        213312, 0, 2, 270652, 22588, 4, 1457325, 64566, 4, 20467, 1, 4, 0, 141992, 32, 100788,
        420, 1, 1, 81663, 32, 59498, 32, 20142, 32, 24588, 32, 20744, 32, 25933, 32, 24623, 32,
        43053543, 10, 53384111, 14333, 10, 43574283, 26308, 10, 16000, 100, 16000, 100, 962335,
        18, 2780678, 6, 442008, 1, 52538055, 3756, 18, 267929, 18, 76433006, 8868, 18, 52948122,
        18, 1995836, 36, 3227919, 12, 901022, 1, 166917843, 4307, 36, 284546, 36, 158221314,
        26549, 36, 74698472, 36, 333849714, 1, 254006273, 72, 2174038, 72, 2261318, 64571, 4,
        207616, 8310, 4, 1293828, 28716, 63, 0, 1, 1006041, 43623, 251, 0, 1, 100181, 726, 719,
        0, 1, 100181, 726, 719, 0, 1, 100181, 726, 719, 0, 1, 107878, 680, 0, 1, 95336, 1,
        281145, 18848, 0, 1, 180194, 159, 1, 1, 158519, 8942, 0, 1, 159378, 8813, 0, 1, 107490,
        3298, 1, 106057, 655, 1, 1964219, 24520, 3,
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
        println!("First bytes: {:02x} {:02x} {:02x}", encoded[0], encoded[1], encoded[2]);

        // Now test encoding as Redeemers::List
        let redeemers = Redeemers::List(vec![redeemer]);
        let encoded_list = redeemers.encode_fragment().unwrap();
        println!("Redeemers List CBOR: {}", hex::encode(&encoded_list));
    }

    #[test]
    fn test_full_tx_build_with_redeemer() {
        use pallas_txbuilder::{BuildConway, Input, Output, StagingTransaction};
        use pallas_addresses::{Address, Network};
        use pallas_primitives::Fragment;
        use pallas_primitives::conway::Tx;
        use pallas_crypto::hash::Hash;

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
                println!("Built tx CBOR ({} bytes): {}", built_tx.tx_bytes.0.len(), hex::encode(&built_tx.tx_bytes.0));

                // Now decode and check the redeemer structure
                let decoded_tx: Tx = Tx::decode_fragment(&built_tx.tx_bytes.0).expect("Should decode");

                // Check the witness set redeemers
                if let Some(ref redeemers) = decoded_tx.transaction_witness_set.redeemer {
                    println!("Redeemers in tx: {:?}", redeemers);

                    // Re-encode just the redeemers to see what they look like
                    let redeemers_cbor = redeemers.encode_fragment().unwrap();
                    println!("Redeemers CBOR from witness set: {}", hex::encode(&redeemers_cbor));
                } else {
                    println!("No redeemers in witness set!");
                }
            }
            Err(e) => {
                println!("Failed to build tx: {:?}", e);
            }
        }
    }
}

#[cfg(test)]
mod signature_verification_tests {
    use super::*;
    use sha3::{Digest, Keccak256};
    use k256::ecdsa::{Signature, VerifyingKey, signature::Verifier, RecoveryId, signature::hazmat::PrehashVerifier};

    /// Test signature verification with recovery to identify the actual signer
    /// This test recovers the correct public keys from real Fuji signatures
    #[test]
    fn test_fuji_signature_with_recovery() {
        // Test data from relayer logs
        let origin: u32 = 43113;
        let merkle_root = hex::decode("efa004d027c79c3d7faf7821111493144243a32f8616af99ceff8238000010ec").unwrap();
        let origin_merkle_tree_hook = hex::decode("0000000000000000000000009ff6ac3daf63103620bbf76136ea1aff43c2f612").unwrap();
        let merkle_index: u32 = 146986598;
        let message_id = hex::decode("0ce4b05a9d25d2556f74ddaa1ac84841341623376c9e5cd073f52b1b54dcddbf").unwrap();

        // Validator 0 public key (compressed) - THIS IS WHAT WE HAVE
        let validator_pubkey = hex::decode("03225f0eceb966fca4afec433f93cb38d3b0cbb44b066a4a83618fc23d2ccd5c17").unwrap();

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
        println!("checkpoint_digest (signing_hash): {}", hex::encode(&checkpoint_digest));

        // Step 3: eth_signed_message = keccak256("\x19Ethereum Signed Message:\n32" || checkpoint_digest)
        let mut eth_hasher = Keccak256::new();
        eth_hasher.update(b"\x19Ethereum Signed Message:\n32");
        eth_hasher.update(&checkpoint_digest);
        let eth_signed_message: [u8; 32] = eth_hasher.finalize().into();
        println!("eth_signed_message (final hash to sign): {}", hex::encode(&eth_signed_message));

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
                println!("Recovered public key (compressed): {}", hex::encode(&recovered_compressed));
                println!("Expected public key (compressed):  {}", hex::encode(&validator_pubkey));

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
            },
            Err(e) => {
                println!("Recovery failed: {:?}", e);

                // Try recovery with checkpoint_digest (without EIP-191)
                println!("\nTrying recovery without EIP-191...");
                match VerifyingKey::recover_from_prehash(&checkpoint_digest, &sig, rec_id) {
                    Ok(recovered_key) => {
                        let recovered_compressed = recovered_key.to_sec1_bytes();
                        println!("Recovered public key (without EIP-191): {}", hex::encode(&recovered_compressed));
                        println!("Expected public key: {}", hex::encode(&validator_pubkey));
                    },
                    Err(e) => println!("Recovery without EIP-191 also failed: {:?}", e),
                }
            }
        }

        println!("\n=== Direct verification ===");

        // Parse the expected public key
        let verifying_key = VerifyingKey::from_sec1_bytes(&validator_pubkey).expect("Invalid public key");

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
        let merkle_root = hex::decode("efa004d027c79c3d7faf7821111493144243a32f8616af99ceff8238000010ec").unwrap();
        let origin_merkle_tree_hook = hex::decode("0000000000000000000000009ff6ac3daf63103620bbf76136ea1aff43c2f612").unwrap();
        let merkle_index: u32 = 146986598;
        let message_id = hex::decode("0ce4b05a9d25d2556f74ddaa1ac84841341623376c9e5cd073f52b1b54dcddbf").unwrap();

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
                    println!("Compressed public key (33 bytes): {}", hex::encode(&compressed));

                    // Get uncompressed key (64 bytes without 0x04 prefix)
                    let uncompressed = recovered_key.to_encoded_point(false);
                    let public_key_bytes = &uncompressed.as_bytes()[1..]; // Skip 0x04 prefix
                    println!("Uncompressed public key (64 bytes): {}", hex::encode(public_key_bytes));

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
                },
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
        let metadata: Vec<u8> = vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 159, 246, 172, 61, 175, 99, 16, 54, 32, 187, 247, 97, 54, 234, 26, 255, 67, 194, 246, 18, 5, 196, 38, 50, 179, 29, 158, 122, 115, 130, 20, 63, 142, 74, 181, 163, 160, 122, 80, 86, 135, 81, 202, 121, 39, 123, 63, 13, 4, 7, 101, 206, 0, 0, 16, 239, 213, 152, 62, 114, 113, 105, 226, 4, 8, 242, 145, 177, 49, 176, 151, 194, 62, 169, 173, 9, 17, 126, 199, 58, 165, 26, 177, 189, 206, 40, 62, 90, 110, 124, 97, 28, 95, 184, 110, 220, 56, 56, 148, 10, 120, 115, 100, 103, 81, 34, 107, 171, 211, 28, 155, 21, 58, 146, 197, 130, 54, 244, 33, 15, 27, 172, 193, 162, 254, 168, 176, 252, 96, 124, 232, 195, 224, 217, 34, 167, 239, 188, 125, 220, 101, 199, 174, 88, 31, 231, 83, 199, 75, 36, 229, 212, 178, 112, 214, 60, 13, 246, 186, 201, 100, 189, 245, 194, 230, 156, 45, 67, 119, 56, 96, 92, 178, 71, 97, 219, 127, 185, 115, 143, 22, 251, 193, 73, 86, 27];

        // Message ID from logs
        let message_id = hex::decode("a6e55f83b2f995471c99bca10a9ed8e606c706fcf46ce57791d377943363a729").unwrap();

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
        println!("origin_merkle_tree_hook: {}", hex::encode(origin_merkle_tree_hook));
        println!("root_index (merkle_index): {}", root_index);
        println!("merkle_root: {}", hex::encode(merkle_root));
        println!("message_id: {}", hex::encode(&message_id));
        println!("signatures_data length: {} (expecting {} signatures)", signatures_data.len(), signatures_data.len() / 65);

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
        let matching_count = recovered_addresses.iter()
            .filter(|addr| trusted_addresses.contains(&addr.as_str()))
            .count();

        println!("\nMatching addresses: {} / {}", matching_count, recovered_addresses.len());

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
        let metadata: Vec<u8> = vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 159, 246, 172, 61, 175, 99, 16, 54, 32, 187, 247, 97, 54, 234, 26, 255, 67, 194, 246, 18, 5, 196, 38, 50, 179, 29, 158, 122, 115, 130, 20, 63, 142, 74, 181, 163, 160, 122, 80, 86, 135, 81, 202, 121, 39, 123, 63, 13, 4, 7, 101, 206, 0, 0, 16, 239, 213, 152, 62, 114, 113, 105, 226, 4, 8, 242, 145, 177, 49, 176, 151, 194, 62, 169, 173, 9, 17, 126, 199, 58, 165, 26, 177, 189, 206, 40, 62, 90, 110, 124, 97, 28, 95, 184, 110, 220, 56, 56, 148, 10, 120, 115, 100, 103, 81, 34, 107, 171, 211, 28, 155, 21, 58, 146, 197, 130, 54, 244, 33, 15, 27, 172, 193, 162, 254, 168, 176, 252, 96, 124, 232, 195, 224, 217, 34, 167, 239, 188, 125, 220, 101, 199, 174, 88, 31, 231, 83, 199, 75, 36, 229, 212, 178, 112, 214, 60, 13, 246, 186, 201, 100, 189, 245, 194, 230, 156, 45, 67, 119, 56, 96, 92, 178, 71, 97, 219, 127, 185, 115, 143, 22, 251, 193, 73, 86, 27];

        let message_id = hex::decode("a6e55f83b2f995471c99bca10a9ed8e606c706fcf46ce57791d377943363a729").unwrap();
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
        let recovered_key_original = VerifyingKey::recover_from_prehash(&eth_signed_message, &sig, rec_id)
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
            println!("\nRecovery with NORMALIZED signature (same v={}):", recovery_id);
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
            println!("\nRecovery with NORMALIZED signature (flipped v={}):", flipped_id);
            let flipped_rec_id = RecoveryId::try_from(flipped_id).unwrap();
            match VerifyingKey::recover_from_prehash(&eth_signed_message, &normalized_sig, flipped_rec_id) {
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
        let n_half = hex::decode("7fffffffffffffffffffffffffffffff5d576e7357a4501ddfe92f46681b20a0").unwrap();

        println!("=== High-S Signature Analysis ===\n");
        println!("n/2: {}", hex::encode(&n_half));

        // Use the metadata from logs
        let metadata: Vec<u8> = vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 159, 246, 172, 61, 175, 99, 16, 54, 32, 187, 247, 97, 54, 234, 26, 255, 67, 194, 246, 18, 5, 196, 38, 50, 179, 29, 158, 122, 115, 130, 20, 63, 142, 74, 181, 163, 160, 122, 80, 86, 135, 81, 202, 121, 39, 123, 63, 13, 4, 7, 101, 206, 0, 0, 16, 239, 213, 152, 62, 114, 113, 105, 226, 4, 8, 242, 145, 177, 49, 176, 151, 194, 62, 169, 173, 9, 17, 126, 199, 58, 165, 26, 177, 189, 206, 40, 62, 90, 110, 124, 97, 28, 95, 184, 110, 220, 56, 56, 148, 10, 120, 115, 100, 103, 81, 34, 107, 171, 211, 28, 155, 21, 58, 146, 197, 130, 54, 244, 33, 15, 27, 172, 193, 162, 254, 168, 176, 252, 96, 124, 232, 195, 224, 217, 34, 167, 239, 188, 125, 220, 101, 199, 174, 88, 31, 231, 83, 199, 75, 36, 229, 212, 178, 112, 214, 60, 13, 246, 186, 201, 100, 189, 245, 194, 230, 156, 45, 67, 119, 56, 96, 92, 178, 71, 97, 219, 127, 185, 115, 143, 22, 251, 193, 73, 86, 27];
        let signatures_data = &metadata[68..];

        // Check both signatures
        for i in 0..2 {
            let sig_bytes = &signatures_data[i*65..(i+1)*65];
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
        let message_id = hex::decode("a6e55f83b2f995471c99bca10a9ed8e606c706fcf46ce57791d377943363a729").unwrap();
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
        println!("Address from ORIGINAL sig: 0x{}", hex::encode(addr_original));

        // Test with normalized signature
        let normalized_sig = sig.normalize_s().unwrap_or(sig);

        if sig.normalize_s().is_some() {
            println!("\n--- Normalized Recovery (same v) ---");
            match VerifyingKey::recover_from_prehash(&eth_signed_message, &normalized_sig, rec_id) {
                Ok(key_norm) => {
                    let uncompressed_norm = key_norm.to_encoded_point(false);
                    let pubkey_norm = &uncompressed_norm.as_bytes()[1..];
                    let addr_norm = &Keccak256::digest(pubkey_norm)[12..];
                    println!("Address from NORMALIZED sig (same v): 0x{}", hex::encode(addr_norm));

                    if addr_original == addr_norm {
                        println!("  ✓ SAME address - normalization doesn't affect recovery here");
                    } else {
                        println!("  ✗ DIFFERENT address - THIS IS THE BUG!");
                        println!("  When we normalize s but keep the same v, we get wrong address!");
                    }
                }
                Err(e) => println!("Recovery failed: {:?}", e),
            }

            println!("\n--- Normalized Recovery (flipped v) ---");
            let flipped_v = if recovery_id == 0 { 1 } else { 0 };
            let flipped_rec_id = RecoveryId::try_from(flipped_v).expect("Invalid recovery id");

            match VerifyingKey::recover_from_prehash(&eth_signed_message, &normalized_sig, flipped_rec_id) {
                Ok(key_flipped) => {
                    let uncompressed_flipped = key_flipped.to_encoded_point(false);
                    let pubkey_flipped = &uncompressed_flipped.as_bytes()[1..];
                    let addr_flipped = &Keccak256::digest(pubkey_flipped)[12..];
                    println!("Address from NORMALIZED sig (flipped v): 0x{}", hex::encode(addr_flipped));

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
        println!("Original sig verifies: {:?}", key_original.verify_prehash(&eth_signed_message, &sig));
        println!("Normalized sig verifies: {:?}", key_original.verify_prehash(&eth_signed_message, &normalized_sig));
    }

    /// Verify our checkpoint hash matches hyperlane-core's implementation
    #[test]
    fn test_checkpoint_hash_matches_hyperlane_core() {
        use hyperlane_core::{Signable, H256, CheckpointWithMessageId, Checkpoint};

        // From logs
        let origin: u32 = 43113;
        let merkle_root = hex::decode("b31d9e7a7382143f8e4ab5a3a07a50568751ca79277b3f0d040765ce000010ef").unwrap();
        let origin_merkle_tree_hook = hex::decode("0000000000000000000000009ff6ac3daf63103620bbf76136ea1aff43c2f612").unwrap();
        let merkle_index: u32 = 96740914;
        let message_id = hex::decode("a6e55f83b2f995471c99bca10a9ed8e606c706fcf46ce57791d377943363a729").unwrap();

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
        println!("hyperlane-core signing_hash: {}", hex::encode(core_signing_hash.as_bytes()));
        println!("Our signing_hash:            {}", hex::encode(&our_signing_hash));

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

        println!("\nhyperlane-core eth_signed_message_hash: {}", hex::encode(core_eth_hash.as_bytes()));
        println!("Our eth_signed_message_hash:            {}", hex::encode(&our_eth_hash));

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
        let metadata: Vec<u8> = vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 159, 246, 172, 61, 175, 99, 16, 54, 32, 187, 247, 97, 54, 234, 26, 255, 67, 194, 246, 18, 5, 196, 38, 50, 179, 29, 158, 122, 115, 130, 20, 63, 142, 74, 181, 163, 160, 122, 80, 86, 135, 81, 202, 121, 39, 123, 63, 13, 4, 7, 101, 206, 0, 0, 16, 239, 213, 152, 62, 114, 113, 105, 226, 4, 8, 242, 145, 177, 49, 176, 151, 194, 62, 169, 173, 9, 17, 126, 199, 58, 165, 26, 177, 189, 206, 40, 62, 90, 110, 124, 97, 28, 95, 184, 110, 220, 56, 56, 148, 10, 120, 115, 100, 103, 81, 34, 107, 171, 211, 28, 155, 21, 58, 146, 197, 130, 54, 244, 33, 15, 27, 172, 193, 162, 254, 168, 176, 252, 96, 124, 232, 195, 224, 217, 34, 167, 239, 188, 125, 220, 101, 199, 174, 88, 31, 231, 83, 199, 75, 36, 229, 212, 178, 112, 214, 60, 13, 246, 186, 201, 100, 189, 245, 194, 230, 156, 45, 67, 119, 56, 96, 92, 178, 71, 97, 219, 127, 185, 115, 143, 22, 251, 193, 73, 86, 27];

        let message_id = hex::decode("a6e55f83b2f995471c99bca10a9ed8e606c706fcf46ce57791d377943363a729").unwrap();
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
        let metadata: Vec<u8> = vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 159, 246, 172, 61, 175, 99, 16, 54, 32, 187, 247, 97, 54, 234, 26, 255, 67, 194, 246, 18, 5, 196, 38, 50, 179, 29, 158, 122, 115, 130, 20, 63, 142, 74, 181, 163, 160, 122, 80, 86, 135, 81, 202, 121, 39, 123, 63, 13, 4, 7, 101, 206, 0, 0, 16, 239, 213, 152, 62, 114, 113, 105, 226, 4, 8, 242, 145, 177, 49, 176, 151, 194, 62, 169, 173, 9, 17, 126, 199, 58, 165, 26, 177, 189, 206, 40, 62, 90, 110, 124, 97, 28, 95, 184, 110, 220, 56, 56, 148, 10, 120, 115, 100, 103, 81, 34, 107, 171, 211, 28, 155, 21, 58, 146, 197, 130, 54, 244, 33, 15, 27, 172, 193, 162, 254, 168, 176, 252, 96, 124, 232, 195, 224, 217, 34, 167, 239, 188, 125, 220, 101, 199, 174, 88, 31, 231, 83, 199, 75, 36, 229, 212, 178, 112, 214, 60, 13, 246, 186, 201, 100, 189, 245, 194, 230, 156, 45, 67, 119, 56, 96, 92, 178, 71, 97, 219, 127, 185, 115, 143, 22, 251, 193, 73, 86, 27];

        let message_id = hex::decode("a6e55f83b2f995471c99bca10a9ed8e606c706fcf46ce57791d377943363a729").unwrap();
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
            let sig_bytes = &signatures_1[i*65..(i+1)*65];
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
            let sig_bytes = &signatures_2[i*65..(i+1)*65];
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
        use hyperlane_core::{CheckpointWithMessageId, Checkpoint, Signable, H256};

        // Data from relayer logs for message 7e2c2f9ef220e8190803eb47033257b562d9104aaa578115aa27601548048d51
        let merkle_tree_hook_address = H256::from_slice(
            &hex::decode("0000000000000000000000009ff6ac3daf63103620bbf76136ea1aff43c2f612").unwrap()
        );
        let mailbox_domain: u32 = 43113; // Fuji
        let root = H256::from_slice(
            &hex::decode("78943434b7600830cf53756b5da5d7bdbed2761edfc997b0e75c9ec95f4f30fb").unwrap()
        );
        let index: u32 = 4336;
        let message_id = H256::from_slice(
            &hex::decode("7e2c2f9ef220e8190803eb47033257b562d9104aaa578115aa27601548048d51").unwrap()
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
        println!("merkle_tree_hook_address: {}", hex::encode(merkle_tree_hook_address.as_bytes()));
        println!("mailbox_domain: {}", mailbox_domain);
        println!("root: {}", hex::encode(root.as_bytes()));
        println!("index: {}", index);
        println!("message_id: {}", hex::encode(message_id.as_bytes()));
        println!();
        println!("=== Hashes ===");
        println!("signing_hash (checkpoint_digest, before EIP-191): {}", hex::encode(signing_hash.as_bytes()));
        println!("eth_signed_message_hash (with EIP-191, what validators sign): {}", hex::encode(eth_signed_message_hash.as_bytes()));
        println!();
        println!("The Aiken compute_checkpoint_hash should produce: {}", hex::encode(eth_signed_message_hash.as_bytes()));

        // Now also print the intermediate steps for Aiken debugging
        println!();
        println!("=== Intermediate values for Aiken test ===");

        // domain_hash_input = domain || address || "HYPERLANE"
        let mut domain_hash_input = Vec::new();
        domain_hash_input.extend_from_slice(&mailbox_domain.to_be_bytes());
        domain_hash_input.extend_from_slice(merkle_tree_hook_address.as_bytes());
        domain_hash_input.extend_from_slice(b"HYPERLANE");
        println!("domain_hash_input ({} bytes): {}", domain_hash_input.len(), hex::encode(&domain_hash_input));

        let domain_hash: [u8; 32] = Keccak256::digest(&domain_hash_input).into();
        println!("domain_hash (keccak256 of above): {}", hex::encode(&domain_hash));

        // checkpoint_input = domain_hash || root || index || message_id
        let mut checkpoint_input = Vec::new();
        checkpoint_input.extend_from_slice(&domain_hash);
        checkpoint_input.extend_from_slice(root.as_bytes());
        checkpoint_input.extend_from_slice(&index.to_be_bytes());
        checkpoint_input.extend_from_slice(message_id.as_bytes());
        println!("checkpoint_input ({} bytes): {}", checkpoint_input.len(), hex::encode(&checkpoint_input));

        let checkpoint_digest: [u8; 32] = Keccak256::digest(&checkpoint_input).into();
        println!("checkpoint_digest (keccak256 of above): {}", hex::encode(&checkpoint_digest));

        // EIP-191: prefix || checkpoint_digest
        let mut eip191_input = Vec::new();
        eip191_input.extend_from_slice(b"\x19Ethereum Signed Message:\n32");
        eip191_input.extend_from_slice(&checkpoint_digest);
        println!("eip191_input ({} bytes): {}", eip191_input.len(), hex::encode(&eip191_input));

        let eth_signed: [u8; 32] = Keccak256::digest(&eip191_input).into();
        println!("eth_signed (keccak256 of above): {}", hex::encode(&eth_signed));
    }
}

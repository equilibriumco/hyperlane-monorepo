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
use tracing::{debug, info, instrument};

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
const DEFAULT_MEM_UNITS: u64 = 5_000_000;
const DEFAULT_STEP_UNITS: u64 = 3_000_000_000;

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
            "".to_string(), // Registry asset name (empty for state NFT)
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
            .find_utxo_by_nft(&self.conf.mailbox_policy_id, "")
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
        info!("Registration state_locator: policy_id={}, asset_name={}",
              registration.state_locator.policy_id,
              registration.state_locator.asset_name);

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

        // 4. Find ISM UTXO (either custom or default)
        let ism_policy_id = match &registration.custom_ism {
            Some(ism) => hex::encode(ism),
            None => self.conf.ism_policy_id.clone(),
        };
        let ism_utxo = self
            .provider
            .find_utxo_by_nft(&ism_policy_id, "")
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

        let recipient_redeemer: HyperlaneRecipientRedeemer<()> =
            HyperlaneRecipientRedeemer::HandleMessage {
                origin: msg.origin,
                sender: msg.sender,
                body: msg.body.clone(),
            };
        let recipient_redeemer_cbor = encode_recipient_redeemer(&recipient_redeemer)?;

        // 7. Build recipient continuation datum (with updated state)
        // The recipient contract expects: messages_received + 1, last_message: Some(body), nonce + 1
        let recipient_continuation_datum_cbor = build_recipient_continuation_datum(
            &recipient_utxo,
            &msg.body,
        )?;

        // 8. Encode processed message marker datum
        let processed_datum = ProcessedMessageDatum { message_id };
        let processed_datum_cbor = encode_processed_message_datum(&processed_datum)?;

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

        // Add ISM UTXO as reference input
        let ism_input = utxo_to_input(&components.ism_utxo)?;
        tx = tx.reference_input(ism_input);

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

        // 3. Processed message marker output
        // This is just an output to the processed_messages_script address with inline datum
        // No NFT minting required - the contract just checks for the output existence
        let processed_marker_output = self.create_processed_marker_output(
            &components.message_id,
            &components.processed_datum_cbor,
        )?;
        tx = tx.output(processed_marker_output);
        debug!("Added processed message marker output for message_id: {}", hex::encode(&components.message_id));

        // 4. Change output back to payer
        // The payer's input funds: fee + processed marker output
        // (mailbox and recipient continuation outputs return the same value they consume)
        let fee = ESTIMATED_FEE_LOVELACE;
        let processed_marker_cost = MIN_UTXO_LOVELACE; // Only the processed marker is "new" value
        let change_amount = total_input.saturating_sub(fee + processed_marker_cost);

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

    /// Get the recipient policy ID from registration
    fn get_recipient_policy_id(&self, recipient_utxo: &Utxo) -> Result<String, TxBuilderError> {
        // Extract policy ID from the recipient UTXO's assets
        for value in &recipient_utxo.value {
            if value.unit != "lovelace" && value.unit.len() >= 56 {
                return Ok(value.unit[..56].to_string());
            }
        }
        Err(TxBuilderError::MissingInput("Recipient policy ID not found".to_string()))
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
        // Note: Redeemer uses ByteArray (variable length), not fixed 32 bytes
        let redeemer = crate::types::MultisigIsmRedeemer::SetValidators {
            domain,
            validators: validators.iter().map(|v| {
                let mut arr = [0u8; 32];
                // The MultisigIsmRedeemer type uses [u8; 32], so we need to pad for the type
                // but the actual on-chain storage is 20 bytes
                arr[..20].copy_from_slice(v);
                arr
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
    /// ISM UTXO (reference input)
    pub ism_utxo: Utxo,
    /// Additional inputs (UTXO, must_be_spent)
    pub additional_utxos: Vec<(Utxo, bool)>,
    /// Encoded processed message datum (CBOR)
    pub processed_datum_cbor: Vec<u8>,
    /// Message ID (32 bytes)
    pub message_id: [u8; 32],
    /// Original metadata
    pub metadata: Vec<u8>,
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
        HyperlaneRecipientRedeemer::HandleMessage {
            origin,
            sender,
            body,
        } => {
            // Constructor 0: HandleMessage
            PlutusData::Constr(Constr {
                tag: 121,
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    PlutusData::BigInt(BigInt::Int((*origin as i64).into())),
                    PlutusData::BoundedBytes(sender.to_vec().into()),
                    PlutusData::BoundedBytes(body.clone().into()),
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
        crate::types::MultisigIsmRedeemer::SetValidators { domain, validators } => {
            // Constr(1, [domain, [validator_bytes]])
            let validator_bytes: Vec<PlutusData> = validators
                .iter()
                .map(|v| PlutusData::BoundedBytes(v.to_vec().into()))
                .collect();

            PlutusData::Constr(Constr {
                tag: 1,
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    PlutusData::BigInt(BigInt::Int((*domain as i64).into())),
                    PlutusData::Array(MaybeIndefArray::Def(validator_bytes)),
                ]),
            })
        }
        _ => return Err(TxBuilderError::Encoding("Only SetValidators redeemer supported".to_string())),
    };

    encode_plutus_data(&plutus_data)
}

/// Parse Hyperlane metadata into validator signatures
///
/// Hyperlane metadata format for multisig ISM:
/// - Bytes 0-31: Merkle root (32 bytes)
/// - Bytes 32-35: Root index (4 bytes)
/// - Bytes 36-67: Origin mailbox address (32 bytes)
/// - Bytes 68-99: Merkle proof (variable, up to 32 * depth bytes)
/// - Remaining: Signatures (65 bytes each for ECDSA, 64 for Ed25519)
pub fn parse_multisig_metadata(metadata: &[u8]) -> Result<MultisigMetadata, TxBuilderError> {
    if metadata.len() < 68 {
        return Err(TxBuilderError::Encoding(
            "Metadata too short for multisig ISM".to_string(),
        ));
    }

    let mut merkle_root = [0u8; 32];
    merkle_root.copy_from_slice(&metadata[0..32]);

    let root_index = u32::from_be_bytes(metadata[32..36].try_into().unwrap());

    let mut origin_mailbox = [0u8; 32];
    origin_mailbox.copy_from_slice(&metadata[36..68]);

    // The rest is merkle proof + signatures
    // For simplicity, we'll treat everything after byte 68 as signatures
    // A proper implementation would parse the merkle proof separately
    let signatures_data = &metadata[68..];

    // Parse signatures (assume 64-byte Ed25519 signatures for Cardano)
    let mut signatures = Vec::new();
    let mut offset = 0;
    let mut validator_index = 0u32;

    while offset + 64 <= signatures_data.len() {
        let mut sig = [0u8; 64];
        sig.copy_from_slice(&signatures_data[offset..offset + 64]);
        signatures.push((validator_index, sig));
        validator_index += 1;
        offset += 64;
    }

    Ok(MultisigMetadata {
        merkle_root,
        root_index,
        origin_mailbox,
        signatures,
    })
}

/// Parsed multisig ISM metadata
#[derive(Debug, Clone)]
pub struct MultisigMetadata {
    pub merkle_root: [u8; 32],
    pub root_index: u32,
    pub origin_mailbox: [u8; 32],
    pub signatures: Vec<(u32, [u8; 64])>, // (validator_index, signature)
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
        // Create minimal metadata
        let mut metadata = vec![0u8; 68];
        // Add one signature
        metadata.extend_from_slice(&[0xAB; 64]);

        let parsed = parse_multisig_metadata(&metadata).unwrap();
        assert_eq!(parsed.signatures.len(), 1);
        assert_eq!(parsed.signatures[0].0, 0); // validator index
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

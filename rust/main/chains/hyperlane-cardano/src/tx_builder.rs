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
use pallas_codec::utils::KeyValuePairs;
use pallas_txbuilder::{BuildBabbage, BuiltTransaction, ExUnits, Input, Output, StagingTransaction};
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

/// Default execution units for script evaluation (conservative estimates)
const DEFAULT_MEM_UNITS: u64 = 14_000_000;
const DEFAULT_STEP_UNITS: u64 = 10_000_000_000;

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

        // Fallback: Find UTXOs at the mailbox script address
        let script_address = self.provider.script_hash_to_address(&self.conf.mailbox_policy_id)?;
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
        debug!("Found mailbox UTXO: {}#{}", mailbox_utxo.tx_hash, mailbox_utxo.output_index);

        // 2. Get recipient registration from registry
        let recipient_script_hash = hyperlane_address_to_script_hash(&msg.recipient)
            .ok_or_else(|| TxBuilderError::InvalidRecipient("Not a script recipient".to_string()))?;

        let registration = self.registry.get_registration(&recipient_script_hash).await?;

        // 3. Find recipient state UTXO
        let recipient_utxo = self
            .provider
            .find_utxo_by_nft(
                &registration.state_locator.policy_id,
                &registration.state_locator.asset_name,
            )
            .await?;
        debug!(
            "Found recipient UTXO: {}#{}",
            recipient_utxo.tx_hash, recipient_utxo.output_index
        );

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

        // 7. Encode processed message marker datum
        let processed_datum = ProcessedMessageDatum { message_id };
        let processed_datum_cbor = encode_processed_message_datum(&processed_datum)?;

        Ok(ProcessTxComponents {
            mailbox_utxo,
            mailbox_redeemer_cbor,
            recipient_utxo,
            recipient_redeemer_cbor,
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
        // Add script inputs (mailbox, recipient)
        let mailbox_input = utxo_to_input(&components.mailbox_utxo)?;
        tx = tx.input(mailbox_input);

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

        // Add fee payment UTXOs
        for utxo in &selected_utxos {
            let input = utxo_to_input(utxo)?;
            tx = tx.input(input);
        }

        // Add spend redeemers with execution units
        // Re-create inputs for redeemer association (since Input doesn't impl Clone)
        let mailbox_input_for_redeemer = utxo_to_input(&components.mailbox_utxo)?;
        let recipient_input_for_redeemer = utxo_to_input(&components.recipient_utxo)?;

        let ex_units_mailbox = ExUnits {
            mem: DEFAULT_MEM_UNITS,
            steps: DEFAULT_STEP_UNITS,
        };

        let ex_units_recipient = ExUnits {
            mem: DEFAULT_MEM_UNITS,
            steps: DEFAULT_STEP_UNITS,
        };

        tx = tx.add_spend_redeemer(
            mailbox_input_for_redeemer,
            components.mailbox_redeemer_cbor.clone(),
            Some(ex_units_mailbox),
        );

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

        // 2. Recipient continuation output (same address, same value, updated datum would be set by script)
        let recipient_output = create_continuation_output(
            &components.recipient_utxo,
            &self.get_recipient_policy_id(components)?,
        )?;
        tx = tx.output(recipient_output);

        // 3. Processed message marker output
        let processed_marker_output = self.create_processed_marker_output(
            &components.message_id,
            &components.processed_datum_cbor,
        )?;
        tx = tx.output(processed_marker_output);

        // 4. Change output back to payer
        let fee = ESTIMATED_FEE_LOVELACE;
        let output_total = MIN_UTXO_LOVELACE * 3; // 3 outputs with min UTXO
        let change_amount = total_input.saturating_sub(fee + output_total);

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

        // Build the transaction
        let built = tx.build_babbage_raw()
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
    fn get_recipient_policy_id(&self, components: &ProcessTxComponents) -> Result<String, TxBuilderError> {
        // Extract policy ID from the recipient UTXO's assets
        for value in &components.recipient_utxo.value {
            if value.unit != "lovelace" && value.unit.len() >= 56 {
                return Ok(value.unit[..56].to_string());
            }
        }
        Err(TxBuilderError::MissingInput("Recipient policy ID not found".to_string()))
    }

    /// Create the processed message marker output
    fn create_processed_marker_output(
        &self,
        message_id: &[u8; 32],
        datum_cbor: &[u8],
    ) -> Result<Output, TxBuilderError> {
        // The processed messages script address
        let script_address = self.provider.script_hash_to_address(&self.conf.mailbox_policy_id)?;

        let output = Output::new(parse_address(&script_address)?, MIN_UTXO_LOVELACE)
            .set_inline_datum(datum_cbor.to_vec());

        // Add the processed message marker NFT
        let asset_name = message_id.to_vec();
        let policy_hash = parse_policy_id(&self.conf.mailbox_policy_id)?;

        output.add_asset(policy_hash, asset_name, 1)
            .map_err(|e| TxBuilderError::TxBuild(format!("Failed to add asset: {:?}", e)))
    }
}


/// Components needed to build a Process transaction
#[derive(Debug)]
pub struct ProcessTxComponents {
    /// Mailbox UTXO to spend
    pub mailbox_utxo: Utxo,
    /// Encoded mailbox redeemer (CBOR)
    pub mailbox_redeemer_cbor: Vec<u8>,
    /// Recipient UTXO to spend
    pub recipient_utxo: Utxo,
    /// Encoded recipient redeemer (CBOR)
    pub recipient_redeemer_cbor: Vec<u8>,
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

/// Convert a JSON datum (from Blockfrost) to CBOR bytes
/// This is a simplified conversion - full implementation would need proper Plutus data parsing
fn json_datum_to_cbor(json_str: &str) -> Result<Vec<u8>, TxBuilderError> {
    use serde_json::Value;

    let json: Value = serde_json::from_str(json_str)
        .map_err(|e| TxBuilderError::Encoding(format!("Invalid datum JSON: {}", e)))?;

    // Convert JSON to PlutusData and encode to CBOR
    let plutus_data = json_to_plutus_data(&json)?;
    encode_plutus_data(&plutus_data)
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
                    fields: parsed_fields,
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
                Ok(PlutusData::Array(parsed_items))
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
            Ok(PlutusData::Array(items))
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
                fields: vec![
                    PlutusData::BigInt(BigInt::Int((*destination as i64).into())),
                    PlutusData::BoundedBytes(recipient.to_vec().into()),
                    PlutusData::BoundedBytes(body.clone().into()),
                ],
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
                fields: vec![
                    encode_message_as_plutus_data(message),
                    PlutusData::BoundedBytes(metadata.clone().into()),
                    PlutusData::BoundedBytes(message_id.to_vec().into()),
                ],
            })
        }
        MailboxRedeemer::SetDefaultIsm { new_ism } => {
            // Constructor 2: SetDefaultIsm
            PlutusData::Constr(Constr {
                tag: 123, // Constructor 2 alternative encoding
                any_constructor: None,
                fields: vec![PlutusData::BoundedBytes(new_ism.to_vec().into())],
            })
        }
        MailboxRedeemer::TransferOwnership { new_owner } => {
            // Constructor 3: TransferOwnership
            PlutusData::Constr(Constr {
                tag: 124, // Constructor 3 alternative encoding
                any_constructor: None,
                fields: vec![PlutusData::BoundedBytes(new_owner.to_vec().into())],
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
                fields: vec![
                    PlutusData::BigInt(BigInt::Int((*origin as i64).into())),
                    PlutusData::BoundedBytes(sender.to_vec().into()),
                    PlutusData::BoundedBytes(body.clone().into()),
                ],
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
        fields: vec![PlutusData::BoundedBytes(datum.message_id.to_vec().into())],
    });

    encode_plutus_data(&plutus_data)
}

/// Encode a Message as Plutus Data
fn encode_message_as_plutus_data(msg: &Message) -> PlutusData {
    // Message { version, nonce, origin, sender, destination, recipient, body }
    PlutusData::Constr(Constr {
        tag: 121, // Constructor 0
        any_constructor: None,
        fields: vec![
            PlutusData::BigInt(BigInt::Int((msg.version as i64).into())),
            PlutusData::BigInt(BigInt::Int((msg.nonce as i64).into())),
            PlutusData::BigInt(BigInt::Int((msg.origin as i64).into())),
            PlutusData::BoundedBytes(msg.sender.to_vec().into()),
            PlutusData::BigInt(BigInt::Int((msg.destination as i64).into())),
            PlutusData::BoundedBytes(msg.recipient.to_vec().into()),
            PlutusData::BoundedBytes(msg.body.clone().into()),
        ],
    })
}

/// Encode PlutusData to CBOR bytes
fn encode_plutus_data(data: &PlutusData) -> Result<Vec<u8>, TxBuilderError> {
    minicbor::to_vec(data)
        .map_err(|e| TxBuilderError::Encoding(format!("CBOR encoding failed: {:?}", e)))
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
}

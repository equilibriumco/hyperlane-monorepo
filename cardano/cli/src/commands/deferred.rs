//! Deferred message processing commands
//!
//! This module provides commands for processing deferred messages.
//! These commands are specifically for the example_deferred_recipient pattern
//! and demonstrate how to process stored messages.
//!
//! ## Example Usage
//!
//! ```bash
//! # List pending deferred messages
//! hyperlane-cardano deferred list \
//!     --recipient-hash a2bb1196e0ae5d1473db18b2ecfbc9f91318814b7a5006a14a39b43b \
//!     --message-nft-policy abc123...
//!
//! # Process a specific deferred message
//! hyperlane-cardano deferred process \
//!     --message-utxo "txhash#0" \
//!     --recipient-state-policy abc123... \
//!     --message-nft-policy def456... \
//!     --recipient-ref-script "txhash#1"
//! ```

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};
use colored::Colorize;

use crate::utils::blockfrost::BlockfrostClient;
use crate::utils::cbor::{normalize_datum, CborBuilder};
use crate::utils::context::CliContext;
use crate::utils::plutus::script_hash_to_address;
use crate::utils::tx_builder::HyperlaneTxBuilder;

#[derive(Args)]
pub struct DeferredArgs {
    #[command(subcommand)]
    command: DeferredCommands,
}

#[derive(Subcommand)]
enum DeferredCommands {
    /// List pending deferred messages at a recipient address
    ///
    /// This command queries for UTXOs containing message NFTs,
    /// which represent messages waiting to be processed.
    List {
        /// Recipient script hash (28 bytes hex, as shown in registry)
        #[arg(long)]
        recipient_hash: String,

        /// Message NFT policy ID (hex)
        #[arg(long)]
        message_nft_policy: String,

        /// Output format (table or json)
        #[arg(long, default_value = "table")]
        format: String,

        /// Show message body decoded as UTF-8
        #[arg(long)]
        show_body: bool,
    },

    /// Process a deferred message (example_deferred_recipient pattern)
    ///
    /// This command demonstrates how to build a transaction that:
    /// 1. Spends the message UTXO (contains StoredMessageDatum)
    /// 2. Burns the message NFT (proves message consumption)
    /// 3. Updates the recipient state (messages_processed += 1)
    ///
    /// NOTE: This is an EXAMPLE for the example_deferred_recipient contract.
    /// Real implementations would add custom outputs based on the message content.
    Process {
        /// Message UTXO to process (format: txhash#index)
        #[arg(long)]
        message_utxo: String,

        /// Recipient state NFT policy ID (to find state UTXO)
        #[arg(long)]
        recipient_state_policy: String,

        /// Recipient state NFT asset name (hex, empty for unit)
        #[arg(long, default_value = "")]
        recipient_state_asset: String,

        /// Message NFT policy ID (for burning)
        #[arg(long)]
        message_nft_policy: String,

        /// Reference script UTXO for recipient validator (format: txhash#index)
        #[arg(long)]
        recipient_ref_script: Option<String>,

        /// Reference script UTXO for message NFT policy (format: txhash#index)
        #[arg(long)]
        nft_ref_script: Option<String>,

        /// Dry run (don't submit transaction)
        #[arg(long)]
        dry_run: bool,
    },

    /// Show details of a specific message UTXO
    Show {
        /// Message UTXO (format: txhash#index)
        #[arg(long)]
        message_utxo: String,
    },

    /// Show status of a deferred recipient (messages stored/processed)
    Status {
        /// Recipient state NFT policy ID (hex)
        #[arg(long)]
        state_policy: String,

        /// Recipient state NFT asset name (hex, empty for unit)
        #[arg(long, default_value = "")]
        state_asset: String,
    },
}

pub async fn execute(ctx: &CliContext, args: DeferredArgs) -> Result<()> {
    match args.command {
        DeferredCommands::List {
            recipient_hash,
            message_nft_policy,
            format,
            show_body,
        } => list_messages(ctx, &recipient_hash, &message_nft_policy, &format, show_body).await,

        DeferredCommands::Process {
            message_utxo,
            recipient_state_policy,
            recipient_state_asset,
            message_nft_policy,
            recipient_ref_script,
            nft_ref_script,
            dry_run,
        } => {
            process_message(
                ctx,
                &message_utxo,
                &recipient_state_policy,
                &recipient_state_asset,
                &message_nft_policy,
                recipient_ref_script,
                nft_ref_script,
                dry_run,
            )
            .await
        }

        DeferredCommands::Show { message_utxo } => show_message(ctx, &message_utxo).await,

        DeferredCommands::Status {
            state_policy,
            state_asset,
        } => show_status(ctx, &state_policy, &state_asset).await,
    }
}

/// List pending deferred messages at a recipient address
async fn list_messages(
    ctx: &CliContext,
    recipient_hash: &str,
    message_nft_policy: &str,
    format: &str,
    show_body: bool,
) -> Result<()> {
    println!("{}", "Listing pending deferred messages...".cyan());

    // Convert script hash to bech32 address
    let recipient_hash = recipient_hash.strip_prefix("0x").unwrap_or(recipient_hash);
    if recipient_hash.len() != 56 {
        return Err(anyhow!("Invalid script hash: expected 56 hex chars (28 bytes), got {}", recipient_hash.len()));
    }
    let recipient_address = script_hash_to_address(recipient_hash, ctx.pallas_network())?;

    println!("  Script Hash: {}", recipient_hash);
    println!("  Address: {}", recipient_address);
    println!("  NFT Policy: {}", message_nft_policy);

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    // Query UTXOs at the recipient address
    let utxos = client.get_utxos(&recipient_address).await?;

    // Filter for UTXOs containing the message NFT
    let message_utxos: Vec<_> = utxos
        .iter()
        .filter(|utxo| {
            utxo.assets
                .iter()
                .any(|asset| asset.policy_id == message_nft_policy)
        })
        .collect();

    if message_utxos.is_empty() {
        println!("\n{}", "No pending messages found.".yellow());
        return Ok(());
    }

    println!(
        "\n{} {} pending message(s):",
        "Found".green(),
        message_utxos.len()
    );

    if format == "json" {
        // JSON output
        let json_output: Vec<serde_json::Value> = message_utxos
            .iter()
            .map(|utxo| {
                let message_id = utxo
                    .assets
                    .iter()
                    .find(|asset| asset.policy_id == message_nft_policy)
                    .map(|asset| asset.asset_name.clone())
                    .unwrap_or_default();

                serde_json::json!({
                    "utxo": format!("{}#{}", utxo.tx_hash, utxo.output_index),
                    "message_id": message_id,
                    "lovelace": utxo.lovelace,
                    "has_datum": utxo.inline_datum.is_some(),
                })
            })
            .collect();

        println!("{}", serde_json::to_string_pretty(&json_output)?);
    } else {
        // Table output - show each message on multiple lines for readability
        for (i, utxo) in message_utxos.iter().enumerate() {
            let message_id = utxo
                .assets
                .iter()
                .find(|asset| asset.policy_id == message_nft_policy)
                .map(|asset| asset.asset_name.clone())
                .unwrap_or_else(|| "unknown".to_string());

            if i > 0 {
                println!();
            }
            println!("{}", format!("Message {}:", i + 1).green());
            println!("  UTXO:       {}#{}", utxo.tx_hash, utxo.output_index);
            println!("  Message ID: {}", message_id);
            println!("  Lovelace:   {}", utxo.lovelace);

            // Show body if requested
            if show_body {
                if let Some(datum_json) = &utxo.inline_datum {
                    let datum_str = serde_json::to_string(datum_json).unwrap_or_default();
                    if let Ok(parsed) = parse_stored_message_datum(&datum_str) {
                        println!("  Origin:     {}", parsed.origin);
                        println!("  Nonce:      {}", parsed.nonce);
                        if let Some(decoded) = decode_body_utf8(&parsed.body) {
                            println!("  Body:       {}", decoded.cyan());
                        } else {
                            println!("  Body (hex): {}...", &parsed.body[..parsed.body.len().min(64)]);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Show details of a specific message UTXO
async fn show_message(ctx: &CliContext, message_utxo: &str) -> Result<()> {
    println!("{}", "Fetching message details...".cyan());

    let (tx_hash, output_index) = parse_utxo_ref(message_utxo)?;

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    // Get the transaction UTXOs and find our output
    let tx_utxos = client.get_tx_utxos(&tx_hash).await?;
    let utxo_entry = tx_utxos
        .outputs
        .iter()
        .find(|o| o.output_index == output_index)
        .ok_or_else(|| anyhow!("Output {} not found in tx {}", output_index, tx_hash))?;

    println!("\n{}", "Message UTXO Details:".green());
    println!("  TX Hash: {}", tx_hash);
    println!("  Output Index: {}", output_index);
    println!("  Address: {}", utxo_entry.address);

    // Calculate lovelace
    let lovelace: u64 = utxo_entry
        .amount
        .iter()
        .find(|a| a.unit == "lovelace")
        .map(|a| a.quantity.parse().unwrap_or(0))
        .unwrap_or(0);
    println!("  Lovelace: {}", lovelace);

    // Show assets
    let assets: Vec<_> = utxo_entry
        .amount
        .iter()
        .filter(|a| a.unit != "lovelace")
        .collect();

    if !assets.is_empty() {
        println!("\n  Assets:");
        for asset in &assets {
            let (policy, name) = if asset.unit.len() > 56 {
                (&asset.unit[..56], &asset.unit[56..])
            } else {
                (asset.unit.as_str(), "")
            };
            println!("    - {}.{}: {}", policy, name, asset.quantity);
        }
    }

    // Parse and show datum
    if let Some(datum_json) = &utxo_entry.inline_datum {
        println!("\n  {}", "StoredMessageDatum:".green());
        let datum_str = serde_json::to_string(datum_json)?;
        if let Ok(parsed) = parse_stored_message_datum(&datum_str) {
            println!("    Origin: {}", parsed.origin);
            println!("    Sender: {}", parsed.sender);
            println!("    Message ID: {}", parsed.message_id);
            println!("    Nonce: {}", parsed.nonce);
            println!("    Body ({} bytes hex): {}", parsed.body.len() / 2, parsed.body);

            // Try to decode body as UTF-8
            if let Some(decoded) = decode_body_utf8(&parsed.body) {
                println!("    Body (UTF-8): {}", decoded.cyan());
            }
        } else {
            println!("    (Failed to parse datum)");
            println!("    Raw: {}", datum_json);
        }
    } else {
        println!("\n  {}", "No inline datum found".yellow());
    }

    Ok(())
}

/// Show status of a deferred recipient
async fn show_status(
    ctx: &CliContext,
    state_policy: &str,
    state_asset: &str,
) -> Result<()> {
    println!("{}", "Fetching deferred recipient status...".cyan());

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    // Find state UTXO by NFT
    let state_utxo = client
        .find_utxo_by_asset(state_policy, state_asset)
        .await?
        .ok_or_else(|| {
            anyhow!(
                "State UTXO not found with policy {} and asset {}",
                state_policy,
                state_asset
            )
        })?;

    println!("\n{}", "Deferred Recipient Status:".green());
    println!("{}", "═".repeat(60));

    // Show UTXO info
    println!("\n{}", "State UTXO:".cyan());
    println!("  TX Hash:  {}#{}", state_utxo.tx_hash, state_utxo.output_index);
    println!("  Address:  {}", state_utxo.address);
    println!("  Lovelace: {}", state_utxo.lovelace);

    // Parse datum
    if let Some(datum_json) = &state_utxo.inline_datum {
        let datum_str = serde_json::to_string(datum_json)?;
        match parse_deferred_recipient_datum(&datum_str) {
            Ok((ism_opt, nonce_opt, messages_stored, messages_processed)) => {
                println!("\n{}", "Message Counters:".cyan());
                println!("  Messages Stored:    {}", messages_stored);
                println!("  Messages Processed: {}", messages_processed);

                let pending = messages_stored - messages_processed;
                if pending > 0 {
                    println!("  {} {}", "Pending:".yellow(), pending);
                } else {
                    println!("  {} 0", "Pending:".green());
                }

                println!("\n{}", "Configuration:".cyan());
                if let Some(ism) = ism_opt {
                    println!("  Custom ISM: {}", ism);
                } else {
                    println!("  Custom ISM: None (uses default)");
                }
                if let Some(nonce) = nonce_opt {
                    println!("  Last Processed Nonce: {}", nonce);
                } else {
                    println!("  Last Processed Nonce: None");
                }
            }
            Err(e) => {
                println!("\n{}", "Could not parse datum:".yellow());
                println!("  Error: {}", e);
            }
        }
    } else {
        println!("\n{}", "No inline datum found".yellow());
    }

    println!("\n{}", "═".repeat(60));

    Ok(())
}

/// Process a deferred message (burn NFT, update state)
async fn process_message(
    ctx: &CliContext,
    message_utxo_ref: &str,
    recipient_state_policy: &str,
    recipient_state_asset: &str,
    message_nft_policy: &str,
    recipient_ref_script: Option<String>,
    nft_ref_script: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Processing deferred message...".cyan());
    println!(
        "\n{} This command is for the example_deferred_recipient pattern.",
        "NOTE:".yellow()
    );
    println!("Real implementations would add custom outputs based on message content.\n");

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);
    let tx_builder = HyperlaneTxBuilder::new(&client, ctx.pallas_network());

    // Load signing key
    let keypair = ctx.load_signing_key()?;
    let payer_address = keypair.address_bech32(ctx.pallas_network());
    println!("  Payer: {}", payer_address);

    // 1. Fetch message UTXO by finding it at the address
    let (msg_tx_hash, msg_output_index) = parse_utxo_ref(message_utxo_ref)?;

    // Get the tx outputs to find the message UTXO address
    let tx_utxos = client.get_tx_utxos(&msg_tx_hash).await?;
    let msg_utxo_entry = tx_utxos
        .outputs
        .iter()
        .find(|o| o.output_index == msg_output_index)
        .ok_or_else(|| anyhow!("Message UTXO not found: {}", message_utxo_ref))?;

    // Now get the UTXO from that address
    let recipient_address = &msg_utxo_entry.address;
    let recipient_utxos = client.get_utxos(recipient_address).await?;
    let message_utxo = recipient_utxos
        .iter()
        .find(|u| u.tx_hash == msg_tx_hash && u.output_index == msg_output_index)
        .ok_or_else(|| anyhow!("Message UTXO already spent or not found"))?;

    println!("\n{}", "Message UTXO:".green());
    println!("  {}#{}", message_utxo.tx_hash, message_utxo.output_index);

    // Extract message ID from the NFT asset name
    let message_id = message_utxo
        .assets
        .iter()
        .find(|asset| asset.policy_id == message_nft_policy)
        .map(|asset| asset.asset_name.clone())
        .ok_or_else(|| anyhow!("Message NFT not found in UTXO"))?;

    println!("  Message ID: {}", message_id);

    // Parse stored message datum
    let datum_json = message_utxo
        .inline_datum
        .as_ref()
        .ok_or_else(|| anyhow!("Message UTXO has no inline datum"))?;

    let datum_str = serde_json::to_string(datum_json)?;
    let stored_msg = parse_stored_message_datum(&datum_str)?;
    println!("  Origin: {}", stored_msg.origin);
    println!("  Nonce: {}", stored_msg.nonce);

    // 2. Fetch recipient state UTXO
    let recipient_state_utxo = client
        .find_utxo_by_asset(recipient_state_policy, recipient_state_asset)
        .await?
        .ok_or_else(|| {
            anyhow!(
                "Recipient state UTXO not found with policy {}",
                recipient_state_policy
            )
        })?;

    println!("\n{}", "Recipient State UTXO:".green());
    println!(
        "  {}#{}",
        recipient_state_utxo.tx_hash, recipient_state_utxo.output_index
    );

    // Parse recipient state datum
    let state_datum_json = recipient_state_utxo
        .inline_datum
        .as_ref()
        .ok_or_else(|| anyhow!("Recipient state UTXO has no inline datum"))?;

    let state_datum_str = serde_json::to_string(state_datum_json)?;
    let (ism_opt, nonce_opt, messages_stored, messages_processed) =
        parse_deferred_recipient_datum(&state_datum_str)?;

    println!("  Messages Stored: {}", messages_stored);
    println!("  Messages Processed: {}", messages_processed);

    // 3. Find fee UTXOs
    let fee_utxos = client.get_utxos(&payer_address).await?;
    if fee_utxos.is_empty() {
        return Err(anyhow!("No UTXOs found at payer address for fees"));
    }

    let fee_utxo = fee_utxos
        .iter()
        .find(|u| u.assets.is_empty() && u.lovelace >= 5_000_000)
        .ok_or_else(|| anyhow!("No suitable fee UTXO found (need >= 5 ADA)"))?;

    println!("\n{}", "Fee UTXO:".green());
    println!("  {}#{}", fee_utxo.tx_hash, fee_utxo.output_index);

    // 4. Find reference scripts if not provided
    // Reference scripts have NFTs with same policy as state NFT but different asset names:
    // - "726566" ("ref") for recipient validator
    // - "6d73675f726566" ("msg_ref") for message NFT policy
    let recipient_ref_script = match recipient_ref_script {
        Some(s) => Some(s),
        None => {
            // Find UTXO with "ref" NFT (asset name "726566")
            let ref_asset_name = "726566";
            match client.find_utxo_by_asset(recipient_state_policy, ref_asset_name).await? {
                Some(utxo) => {
                    let ref_str = format!("{}#{}", utxo.tx_hash, utxo.output_index);
                    println!("\n{}", "Auto-discovered Recipient Ref Script:".green());
                    println!("  {}", ref_str);
                    Some(ref_str)
                }
                None => {
                    println!("\n{}", "Warning: Could not find recipient reference script UTXO".yellow());
                    None
                }
            }
        }
    };

    let nft_ref_script = match nft_ref_script {
        Some(s) => Some(s),
        None => {
            // Find UTXO with "msg_ref" NFT (asset name "6d73675f726566")
            let msg_ref_asset_name = "6d73675f726566";
            match client.find_utxo_by_asset(recipient_state_policy, msg_ref_asset_name).await? {
                Some(utxo) => {
                    let ref_str = format!("{}#{}", utxo.tx_hash, utxo.output_index);
                    println!("{}", "Auto-discovered NFT Ref Script:".green());
                    println!("  {}", ref_str);
                    Some(ref_str)
                }
                None => {
                    println!("{}", "Warning: Could not find message NFT reference script UTXO".yellow());
                    None
                }
            }
        }
    };

    // Validate we have reference scripts
    if recipient_ref_script.is_none() {
        return Err(anyhow!(
            "Could not find recipient reference script UTXO. \
            Deploy it with 'init recipient --deferred' or provide --recipient-ref-script"
        ));
    }
    if nft_ref_script.is_none() {
        return Err(anyhow!(
            "Could not find message NFT reference script UTXO. \
            Deploy it with 'init recipient --deferred' or provide --nft-ref-script"
        ));
    }

    // 5. Build the transaction
    println!("\n{}", "Building transaction...".cyan());

    // Build redeemer: ContractAction { action: ProcessStoredMessage { message_id } }
    let recipient_redeemer = build_process_stored_message_redeemer(&message_id)?;
    println!("  Built recipient redeemer (ProcessStoredMessage)");

    // Build NFT burn redeemer: BurnMessage (constructor 1)
    let nft_redeemer = build_message_nft_burn_redeemer();
    println!("  Built NFT burn redeemer (BurnMessage)");

    // Build new recipient state datum (messages_processed + 1)
    let new_state_datum = build_deferred_recipient_datum(
        ism_opt.as_deref(),
        nonce_opt,
        messages_stored,
        messages_processed + 1,
    )?;
    println!(
        "  Built new state datum (messages_processed: {} -> {})",
        messages_processed,
        messages_processed + 1
    );

    // Build the transaction using the tx_builder
    let built_tx = tx_builder
        .build_deferred_process_tx(
            &keypair,
            fee_utxo,
            message_utxo,
            &recipient_state_utxo,
            message_nft_policy,
            &message_id,
            &recipient_redeemer,
            &nft_redeemer,
            &new_state_datum,
            recipient_ref_script.as_deref(),
            nft_ref_script.as_deref(),
        )
        .await?;

    println!("  Transaction built");

    if dry_run {
        println!("\n{}", "[Dry run - transaction not submitted]".yellow());
        println!("\nTransaction hash: {}", hex::encode(built_tx.tx_hash.0));
        return Ok(());
    }

    // Sign transaction
    let signed_tx = tx_builder.sign_tx(built_tx, &keypair)?;
    println!("  Transaction signed ({} bytes)", signed_tx.len());

    // Submit
    println!("\n{}", "Submitting transaction...".cyan());
    let tx_hash = client.submit_tx(&signed_tx).await?;

    println!("\n{}", "Transaction submitted successfully!".green());
    println!("  TX Hash: {}", tx_hash);
    println!("\n  View on explorer: {}", ctx.explorer_tx_url(&tx_hash));

    Ok(())
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Parse a UTXO reference string (format: "txhash#index")
fn parse_utxo_ref(s: &str) -> Result<(String, u32)> {
    let parts: Vec<&str> = s.split('#').collect();
    if parts.len() != 2 {
        return Err(anyhow!(
            "Invalid UTXO reference format. Expected 'txhash#index', got '{}'",
            s
        ));
    }
    let tx_hash = parts[0].to_string();
    let output_index: u32 = parts[1]
        .parse()
        .map_err(|_| anyhow!("Invalid output index: {}", parts[1]))?;
    Ok((tx_hash, output_index))
}

/// Parsed stored message datum
#[derive(Debug)]
struct StoredMessage {
    origin: u32,
    sender: String,
    body: String,
    message_id: String,
    nonce: u32,
}

/// Parse StoredMessageDatum from JSON or CBOR hex
/// Structure: Constr 0 [origin: Int, sender: ByteArray, body: ByteArray, message_id: ByteArray, nonce: Int]
fn parse_stored_message_datum(json_str: &str) -> Result<StoredMessage> {
    let raw_json: serde_json::Value = serde_json::from_str(json_str)?;

    // Normalize the datum (convert CBOR hex to JSON if needed)
    let json = normalize_datum(&raw_json)?;

    let fields = json
        .get("fields")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow!("Invalid StoredMessageDatum: missing fields (json: {})", json))?;

    if fields.len() < 5 {
        return Err(anyhow!(
            "Invalid StoredMessageDatum: expected 5 fields, got {}",
            fields.len()
        ));
    }

    let origin = fields[0]
        .get("int")
        .and_then(|i| i.as_u64())
        .ok_or_else(|| anyhow!("Invalid origin"))? as u32;

    let sender = fields[1]
        .get("bytes")
        .and_then(|b| b.as_str())
        .ok_or_else(|| anyhow!("Invalid sender"))?
        .to_string();

    let body = fields[2]
        .get("bytes")
        .and_then(|b| b.as_str())
        .ok_or_else(|| anyhow!("Invalid body"))?
        .to_string();

    let message_id = fields[3]
        .get("bytes")
        .and_then(|b| b.as_str())
        .ok_or_else(|| anyhow!("Invalid message_id"))?
        .to_string();

    let nonce = fields[4]
        .get("int")
        .and_then(|i| i.as_u64())
        .ok_or_else(|| anyhow!("Invalid nonce"))? as u32;

    Ok(StoredMessage {
        origin,
        sender,
        body,
        message_id,
        nonce,
    })
}

/// Parse deferred recipient datum from JSON or CBOR hex
/// Structure: Constr 0 [ism: Option, nonce: Option, inner: { messages_stored, messages_processed }]
fn parse_deferred_recipient_datum(
    json_str: &str,
) -> Result<(Option<String>, Option<i64>, i64, i64)> {
    let raw_json: serde_json::Value = serde_json::from_str(json_str)?;

    // Normalize the datum (convert CBOR hex to JSON if needed)
    let json = normalize_datum(&raw_json)?;

    let fields = json
        .get("fields")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow!("Invalid recipient datum: missing fields (json: {})", json))?;

    if fields.len() < 3 {
        return Err(anyhow!("Invalid recipient datum: expected 3 fields"));
    }

    // Parse ISM (Option)
    let ism_opt = if fields[0].get("constructor") == Some(&serde_json::json!(0)) {
        fields[0]
            .get("fields")
            .and_then(|f| f.as_array())
            .and_then(|f| f.first())
            .and_then(|v| v.get("bytes"))
            .and_then(|b| b.as_str())
            .map(|s| s.to_string())
    } else {
        None
    };

    // Parse nonce (Option)
    let nonce_opt = if fields[1].get("constructor") == Some(&serde_json::json!(0)) {
        fields[1]
            .get("fields")
            .and_then(|f| f.as_array())
            .and_then(|f| f.first())
            .and_then(|v| v.get("int"))
            .and_then(|i| i.as_i64())
    } else {
        None
    };

    // Parse inner (DeferredInner)
    let inner = fields[2]
        .get("fields")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow!("Invalid inner datum"))?;

    let messages_stored = inner
        .first()
        .and_then(|v| v.get("int"))
        .and_then(|i| i.as_i64())
        .unwrap_or(0);

    let messages_processed = inner
        .get(1)
        .and_then(|v| v.get("int"))
        .and_then(|i| i.as_i64())
        .unwrap_or(0);

    Ok((ism_opt, nonce_opt, messages_stored, messages_processed))
}

/// Build ProcessStoredMessage redeemer
/// Structure: ContractAction { action: ProcessStoredMessage { message_id } }
/// ContractAction = constructor 1
/// ProcessStoredMessage = constructor 0
fn build_process_stored_message_redeemer(message_id_hex: &str) -> Result<Vec<u8>> {
    let mut builder = CborBuilder::new();

    // ContractAction = constructor 1
    builder.start_constr(1);

    // action: ProcessStoredMessage = constructor 0
    builder.start_constr(0);

    // message_id: ByteArray (32 bytes)
    builder.bytes_hex(message_id_hex)?;

    builder.end_constr(); // end ProcessStoredMessage
    builder.end_constr(); // end ContractAction

    Ok(builder.build())
}

/// Build message NFT burn redeemer
/// BurnMessage = constructor 1 (MintMessage = 0, BurnMessage = 1)
fn build_message_nft_burn_redeemer() -> Vec<u8> {
    let mut builder = CborBuilder::new();
    builder.start_constr(1).end_constr();
    builder.build()
}

/// Build deferred recipient datum
/// Structure: HyperlaneRecipientDatum { ism: Option, last_processed_nonce: Option, inner: DeferredInner }
fn build_deferred_recipient_datum(
    ism: Option<&str>,
    nonce: Option<i64>,
    messages_stored: i64,
    messages_processed: i64,
) -> Result<Vec<u8>> {
    let mut builder = CborBuilder::new();

    // HyperlaneRecipientDatum = constructor 0
    builder.start_constr(0);

    // ism: Option<ScriptHash>
    if let Some(ism_hash) = ism {
        // Some = constructor 0
        builder.start_constr(0);
        builder.bytes_hex(ism_hash)?;
        builder.end_constr();
    } else {
        // None = constructor 1
        builder.start_constr(1).end_constr();
    }

    // last_processed_nonce: Option<Int>
    if let Some(n) = nonce {
        // Some = constructor 0
        builder.start_constr(0);
        builder.int(n);
        builder.end_constr();
    } else {
        // None = constructor 1
        builder.start_constr(1).end_constr();
    }

    // inner: DeferredInner = constructor 0 [messages_stored, messages_processed]
    builder.start_constr(0);
    builder.int(messages_stored);
    builder.int(messages_processed);
    builder.end_constr();

    builder.end_constr(); // end HyperlaneRecipientDatum

    Ok(builder.build())
}

/// Try to decode a hex-encoded message body as UTF-8
/// Returns None if the hex is invalid or the bytes aren't valid UTF-8
fn decode_body_utf8(hex_body: &str) -> Option<String> {
    let bytes = hex::decode(hex_body).ok()?;

    // Try to decode as UTF-8
    match String::from_utf8(bytes.clone()) {
        Ok(s) => {
            // Check if it looks like printable text (not binary garbage)
            if s.chars().all(|c| c.is_ascii_graphic() || c.is_ascii_whitespace()) {
                Some(s)
            } else {
                // Contains non-printable characters, might be binary
                None
            }
        }
        Err(_) => None,
    }
}

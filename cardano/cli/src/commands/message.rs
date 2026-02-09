//! Message redemption commands
//!
//! Commands for listing, receiving, and expiring messages stored in
//! message redemption UTXOs. These UTXOs are created by the relayer
//! during message delivery (Process TX) and can be received by the
//! recipient or expired by the relayer.

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};
use colored::Colorize;

use crate::utils::blockfrost::BlockfrostClient;
use crate::utils::cbor::{normalize_datum, CborBuilder};
use crate::utils::context::CliContext;
use crate::utils::plutus::{apply_validator_param, encode_script_hash_param, script_hash_to_address};
use crate::utils::tx_builder::HyperlaneTxBuilder;

#[derive(Args)]
pub struct MessageArgs {
    #[command(subcommand)]
    command: MessageCommands,
}

#[derive(Subcommand)]
enum MessageCommands {
    /// List pending messages at the message redemption script address
    List {
        /// Message redemption script hash (28 bytes hex)
        #[arg(long)]
        redemption_hash: String,

        /// Stored message NFT policy ID (hex)
        #[arg(long)]
        message_nft_policy: String,

        /// Output format (table or json)
        #[arg(long, default_value = "table")]
        format: String,

        /// Show message body decoded as UTF-8
        #[arg(long)]
        show_body: bool,
    },

    /// Show details of a specific message UTXO
    Show {
        /// Message UTXO (format: txhash#index)
        #[arg(long)]
        message_utxo: String,
    },

    /// Receive a message (recipient receives, ADA returns to relayer)
    ///
    /// The receive transaction:
    /// 1. Spends the message redemption UTXO
    /// 2. Burns the stored message NFT
    /// 3. Sends ADA back to the relayer (return_address in datum)
    /// 4. Requires the recipient contract to be spent (proves authorization)
    Receive {
        /// Message UTXO to receive (format: txhash#index)
        #[arg(long)]
        message_utxo: String,

        /// Stored message NFT policy ID (for burning)
        #[arg(long)]
        message_nft_policy: String,

        /// Recipient state NFT policy ID (to find recipient UTXO)
        #[arg(long)]
        recipient_state_policy: String,

        /// Recipient state NFT asset name (hex, empty for unit)
        #[arg(long, default_value = "")]
        recipient_state_asset: String,

        /// Reference script UTXO for message redemption validator (format: txhash#index)
        #[arg(long)]
        redemption_ref_script: Option<String>,

        /// Reference script UTXO for message NFT policy (format: txhash#index)
        #[arg(long)]
        nft_ref_script: Option<String>,

        /// CBOR hex of recipient spend redeemer (for script-based recipients)
        #[arg(long)]
        recipient_redeemer: Option<String>,

        /// CBOR hex of updated state datum (replaces existing datum if provided)
        #[arg(long)]
        recipient_new_datum: Option<String>,

        /// Reference script UTXO for recipient validator (format: txhash#index)
        #[arg(long)]
        recipient_ref_script: Option<String>,

        /// Dry run (don't submit transaction)
        #[arg(long)]
        dry_run: bool,
    },

    /// Expire a message (relayer reclaims after expiry slot)
    ///
    /// After the expiry slot, the relayer can reclaim the ADA
    /// from the message redemption UTXO without recipient involvement.
    Expire {
        /// Message UTXO to expire (format: txhash#index)
        #[arg(long)]
        message_utxo: String,

        /// Stored message NFT policy ID (for burning)
        #[arg(long)]
        message_nft_policy: String,

        /// Reference script UTXO for message redemption validator (format: txhash#index)
        #[arg(long)]
        redemption_ref_script: Option<String>,

        /// Reference script UTXO for message NFT policy (format: txhash#index)
        #[arg(long)]
        nft_ref_script: Option<String>,

        /// Dry run (don't submit transaction)
        #[arg(long)]
        dry_run: bool,
    },
}

pub async fn execute(ctx: &CliContext, args: MessageArgs) -> Result<()> {
    match args.command {
        MessageCommands::List {
            redemption_hash,
            message_nft_policy,
            format,
            show_body,
        } => {
            list_messages(ctx, &redemption_hash, &message_nft_policy, &format, show_body).await
        }

        MessageCommands::Show { message_utxo } => show_message(ctx, &message_utxo).await,

        MessageCommands::Receive {
            message_utxo,
            message_nft_policy,
            recipient_state_policy,
            recipient_state_asset,
            redemption_ref_script,
            nft_ref_script,
            recipient_redeemer,
            recipient_new_datum,
            recipient_ref_script,
            dry_run,
        } => {
            receive_message(
                ctx,
                &message_utxo,
                &message_nft_policy,
                &recipient_state_policy,
                &recipient_state_asset,
                redemption_ref_script,
                nft_ref_script,
                recipient_redeemer,
                recipient_new_datum,
                recipient_ref_script,
                dry_run,
            )
            .await
        }

        MessageCommands::Expire {
            message_utxo,
            message_nft_policy,
            redemption_ref_script,
            nft_ref_script,
            dry_run,
        } => {
            expire_message(
                ctx,
                &message_utxo,
                &message_nft_policy,
                redemption_ref_script,
                nft_ref_script,
                dry_run,
            )
            .await
        }
    }
}

async fn list_messages(
    ctx: &CliContext,
    redemption_hash: &str,
    message_nft_policy: &str,
    format: &str,
    show_body: bool,
) -> Result<()> {
    println!("{}", "Listing pending message redemptions...".cyan());

    let redemption_hash = redemption_hash
        .strip_prefix("0x")
        .unwrap_or(redemption_hash);
    if redemption_hash.len() != 56 {
        return Err(anyhow!(
            "Invalid script hash: expected 56 hex chars (28 bytes), got {}",
            redemption_hash.len()
        ));
    }
    let redemption_address = script_hash_to_address(redemption_hash, ctx.pallas_network())?;

    println!("  Script Hash: {}", redemption_hash);
    println!("  Address: {}", redemption_address);
    println!("  NFT Policy: {}", message_nft_policy);

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let utxos = client.get_utxos(&redemption_address).await?;

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

            if show_body {
                if let Some(datum_json) = &utxo.inline_datum {
                    let datum_str = serde_json::to_string(datum_json).unwrap_or_default();
                    if let Ok(parsed) = parse_message_redemption_datum(&datum_str) {
                        println!("  Origin:     {}", parsed.origin);
                        println!("  Nonce:      {}", parsed.nonce);
                        println!(
                            "  Expiry:     slot {}",
                            parsed.expiry_slot
                        );
                        if let Some(decoded) = decode_body_utf8(&parsed.body) {
                            println!("  Body:       {}", decoded.cyan());
                        } else {
                            println!(
                                "  Body (hex): {}...",
                                &parsed.body[..parsed.body.len().min(64)]
                            );
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

async fn show_message(ctx: &CliContext, message_utxo: &str) -> Result<()> {
    println!("{}", "Fetching message details...".cyan());

    let (tx_hash, output_index) = parse_utxo_ref(message_utxo)?;

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let tx_utxos = client.get_tx_utxos(&tx_hash).await?;
    let utxo_entry = tx_utxos
        .outputs
        .iter()
        .find(|o| o.output_index == output_index)
        .ok_or_else(|| anyhow!("Output {} not found in tx {}", output_index, tx_hash))?;

    println!("\n{}", "Message Redemption UTXO Details:".green());
    println!("  TX Hash: {}", tx_hash);
    println!("  Output Index: {}", output_index);
    println!("  Address: {}", utxo_entry.address);

    let lovelace: u64 = utxo_entry
        .amount
        .iter()
        .find(|a| a.unit == "lovelace")
        .map(|a| a.quantity.parse().unwrap_or(0))
        .unwrap_or(0);
    println!("  Lovelace: {}", lovelace);

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

    if let Some(datum_json) = &utxo_entry.inline_datum {
        println!("\n  {}", "MessageRedemptionDatum:".green());
        let datum_str = serde_json::to_string(datum_json)?;
        if let Ok(parsed) = parse_message_redemption_datum(&datum_str) {
            println!("    Origin: {}", parsed.origin);
            println!("    Sender: {}", parsed.sender);
            println!("    Message ID: {}", parsed.message_id);
            println!("    Nonce: {}", parsed.nonce);
            println!("    Recipient Policy: {}", parsed.recipient_policy);
            println!("    Return Address: {}", parsed.return_address);
            println!("    Expiry Slot: {}", parsed.expiry_slot);
            println!(
                "    Body ({} bytes hex): {}",
                parsed.body.len() / 2,
                parsed.body
            );

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

#[allow(clippy::too_many_arguments)]
async fn receive_message(
    ctx: &CliContext,
    message_utxo_ref: &str,
    message_nft_policy: &str,
    recipient_state_policy: &str,
    recipient_state_asset: &str,
    redemption_ref_script: Option<String>,
    nft_ref_script: Option<String>,
    recipient_redeemer: Option<String>,
    recipient_new_datum: Option<String>,
    recipient_ref_script: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Receiving message (returning ADA to relayer)...".cyan());

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);
    let tx_builder = HyperlaneTxBuilder::new(&client, ctx.pallas_network());

    let keypair = ctx.load_signing_key()?;
    let payer_address = keypair.address_bech32(ctx.pallas_network());
    println!("  Receiver: {}", payer_address);

    // 1. Fetch message UTXO
    let (msg_tx_hash, msg_output_index) = parse_utxo_ref(message_utxo_ref)?;
    let tx_utxos = client.get_tx_utxos(&msg_tx_hash).await?;
    let msg_utxo_entry = tx_utxos
        .outputs
        .iter()
        .find(|o| o.output_index == msg_output_index)
        .ok_or_else(|| anyhow!("Message UTXO not found: {}", message_utxo_ref))?;

    let msg_address = &msg_utxo_entry.address;
    let msg_utxos = client.get_utxos(msg_address).await?;
    let message_utxo = msg_utxos
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
        .ok_or_else(|| anyhow!("Stored message NFT not found in UTXO"))?;

    println!("  Message ID: {}", message_id);

    // Parse datum to get return_address
    let datum_json = message_utxo
        .inline_datum
        .as_ref()
        .ok_or_else(|| anyhow!("Message UTXO has no inline datum"))?;
    let datum_str = serde_json::to_string(datum_json)?;
    let parsed_datum = parse_message_redemption_datum(&datum_str)?;
    println!("  Origin: {}", parsed_datum.origin);
    println!("  Nonce: {}", parsed_datum.nonce);
    println!("  Return Address (relayer): {}", parsed_datum.return_address);
    println!("  Expiry Slot: {}", parsed_datum.expiry_slot);

    // 2. Fetch recipient state UTXO (must be spent to prove authorization)
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

    // 3. Find fee UTXOs
    let fee_utxos = client.get_utxos(&payer_address).await?;
    let fee_utxo = fee_utxos
        .iter()
        .find(|u| u.assets.is_empty() && u.lovelace >= 5_000_000 && u.reference_script.is_none())
        .ok_or_else(|| {
            anyhow!("No suitable fee UTXO found (need >= 5 ADA without tokens or reference scripts)")
        })?;

    println!("\n{}", "Fee UTXO:".green());
    println!("  {}#{}", fee_utxo.tx_hash, fee_utxo.output_index);

    // 4. Build the receive transaction
    println!("\n{}", "Building receive transaction...".cyan());

    // Receive redeemer: constructor 0 (ClaimMessage)
    let receive_redeemer = build_receive_redeemer();
    println!("  Built receive redeemer (ClaimMessage)");

    // NFT burn redeemer: constructor 1 (BurnMessage)
    let nft_redeemer = build_nft_burn_redeemer();
    println!("  Built NFT burn redeemer");

    let recipient_redeemer_bytes = recipient_redeemer
        .as_deref()
        .map(hex::decode)
        .transpose()
        .map_err(|e| anyhow!("Invalid recipient-redeemer hex: {}", e))?;

    let new_state_datum_bytes = recipient_new_datum
        .as_deref()
        .map(hex::decode)
        .transpose()
        .map_err(|e| anyhow!("Invalid recipient-new-datum hex: {}", e))?;

    // Load inline scripts if reference scripts are not provided
    let deployment_info = ctx.load_deployment_info()?;
    let mailbox_info = deployment_info
        .mailbox
        .as_ref()
        .ok_or_else(|| anyhow!("Mailbox not found in deployment_info.json"))?;
    let mailbox_policy = mailbox_info
        .state_nft_policy
        .as_ref()
        .ok_or_else(|| anyhow!("Missing mailbox.stateNftPolicy in deployment_info.json"))?;

    let redemption_inline_script = if redemption_ref_script.is_none() {
        println!("  Loading message_redemption inline script");
        let nft_policy_param = encode_script_hash_param(message_nft_policy)?;
        let nft_policy_hex = hex::encode(&nft_policy_param);
        let applied = apply_validator_param(
            &ctx.contracts_dir,
            "message_redemption",
            "message_redemption",
            &nft_policy_hex,
        )?;
        let script_bytes = hex::decode(&applied.compiled_code)?;
        Some(script_bytes)
    } else {
        None
    };

    let nft_inline_script = if nft_ref_script.is_none() {
        println!("  Loading stored_message_nft inline script");
        let mailbox_policy_param = encode_script_hash_param(mailbox_policy)?;
        let mailbox_policy_hex = hex::encode(&mailbox_policy_param);
        let applied = apply_validator_param(
            &ctx.contracts_dir,
            "stored_message_nft",
            "stored_message_nft",
            &mailbox_policy_hex,
        )?;
        let script_bytes = hex::decode(&applied.compiled_code)?;
        Some(script_bytes)
    } else {
        None
    };

    let built_tx = tx_builder
        .build_message_receive_tx(
            &keypair,
            fee_utxo,
            message_utxo,
            &recipient_state_utxo,
            message_nft_policy,
            &message_id,
            &parsed_datum.return_address,
            &receive_redeemer,
            &nft_redeemer,
            redemption_ref_script.as_deref(),
            nft_ref_script.as_deref(),
            redemption_inline_script.as_deref(),
            nft_inline_script.as_deref(),
            recipient_redeemer_bytes.as_deref(),
            new_state_datum_bytes.as_deref(),
            recipient_ref_script.as_deref(),
            None,
        )
        .await?;

    println!("  Transaction built");

    if dry_run {
        println!("\n{}", "[Dry run - transaction not submitted]".yellow());
        println!("\nTransaction hash: {}", hex::encode(built_tx.tx_hash.0));
        return Ok(());
    }

    let signed_tx = tx_builder.sign_tx(built_tx, &keypair)?;
    println!("  Transaction signed ({} bytes)", signed_tx.len());

    println!("\n{}", "Submitting transaction...".cyan());
    let tx_hash = client.submit_and_confirm(&signed_tx, ctx.no_wait).await?;

    println!("\n{}", "Message received successfully!".green());
    println!("  View on explorer: {}", ctx.explorer_tx_url(&tx_hash));

    Ok(())
}

async fn expire_message(
    ctx: &CliContext,
    message_utxo_ref: &str,
    message_nft_policy: &str,
    redemption_ref_script: Option<String>,
    nft_ref_script: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!(
        "{}",
        "Expiring message (relayer reclaiming after expiry)...".cyan()
    );

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);
    let tx_builder = HyperlaneTxBuilder::new(&client, ctx.pallas_network());

    let keypair = ctx.load_signing_key()?;
    let payer_address = keypair.address_bech32(ctx.pallas_network());
    println!("  Relayer: {}", payer_address);

    // 1. Fetch message UTXO
    let (msg_tx_hash, msg_output_index) = parse_utxo_ref(message_utxo_ref)?;
    let tx_utxos = client.get_tx_utxos(&msg_tx_hash).await?;
    let msg_utxo_entry = tx_utxos
        .outputs
        .iter()
        .find(|o| o.output_index == msg_output_index)
        .ok_or_else(|| anyhow!("Message UTXO not found: {}", message_utxo_ref))?;

    let msg_address = &msg_utxo_entry.address;
    let msg_utxos = client.get_utxos(msg_address).await?;
    let message_utxo = msg_utxos
        .iter()
        .find(|u| u.tx_hash == msg_tx_hash && u.output_index == msg_output_index)
        .ok_or_else(|| anyhow!("Message UTXO already spent or not found"))?;

    println!("\n{}", "Message UTXO:".green());
    println!("  {}#{}", message_utxo.tx_hash, message_utxo.output_index);

    let message_id = message_utxo
        .assets
        .iter()
        .find(|asset| asset.policy_id == message_nft_policy)
        .map(|asset| asset.asset_name.clone())
        .ok_or_else(|| anyhow!("Stored message NFT not found in UTXO"))?;

    println!("  Message ID: {}", message_id);

    // Parse datum to verify expiry
    let datum_json = message_utxo
        .inline_datum
        .as_ref()
        .ok_or_else(|| anyhow!("Message UTXO has no inline datum"))?;
    let datum_str = serde_json::to_string(datum_json)?;
    let parsed_datum = parse_message_redemption_datum(&datum_str)?;
    println!("  Expiry Slot: {}", parsed_datum.expiry_slot);
    println!("  Return Address: {}", parsed_datum.return_address);

    // Check current slot against expiry
    let current_slot = client.get_latest_slot().await?;
    if current_slot < parsed_datum.expiry_slot {
        return Err(anyhow!(
            "Cannot expire yet: current slot {} < expiry slot {}. \
            Wait {} more slots (~{} seconds).",
            current_slot,
            parsed_datum.expiry_slot,
            parsed_datum.expiry_slot - current_slot,
            parsed_datum.expiry_slot - current_slot
        ));
    }

    // 2. Find fee UTXO
    let fee_utxos = client.get_utxos(&payer_address).await?;
    let fee_utxo = fee_utxos
        .iter()
        .find(|u| u.assets.is_empty() && u.lovelace >= 5_000_000 && u.reference_script.is_none())
        .ok_or_else(|| {
            anyhow!("No suitable fee UTXO found (need >= 5 ADA without tokens or reference scripts)")
        })?;

    println!("\n{}", "Fee UTXO:".green());
    println!("  {}#{}", fee_utxo.tx_hash, fee_utxo.output_index);

    // 3. Build the expire transaction
    println!("\n{}", "Building expire transaction...".cyan());

    // Expire redeemer: constructor 1 (ExpireMessage)
    let expire_redeemer = build_expire_redeemer();
    println!("  Built expire redeemer (ExpireMessage)");

    let nft_redeemer = build_nft_burn_redeemer();
    println!("  Built NFT burn redeemer");

    let built_tx = tx_builder
        .build_message_expire_tx(
            &keypair,
            fee_utxo,
            message_utxo,
            message_nft_policy,
            &message_id,
            &parsed_datum.return_address,
            &expire_redeemer,
            &nft_redeemer,
            parsed_datum.expiry_slot,
            redemption_ref_script.as_deref(),
            nft_ref_script.as_deref(),
        )
        .await?;

    println!("  Transaction built");

    if dry_run {
        println!("\n{}", "[Dry run - transaction not submitted]".yellow());
        println!("\nTransaction hash: {}", hex::encode(built_tx.tx_hash.0));
        return Ok(());
    }

    let signed_tx = tx_builder.sign_tx(built_tx, &keypair)?;
    println!("  Transaction signed ({} bytes)", signed_tx.len());

    println!("\n{}", "Submitting transaction...".cyan());
    let tx_hash = client.submit_and_confirm(&signed_tx, ctx.no_wait).await?;

    println!("\n{}", "Message expired successfully!".green());
    println!("  View on explorer: {}", ctx.explorer_tx_url(&tx_hash));

    Ok(())
}

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

#[derive(Debug)]
struct MessageRedemptionParsed {
    origin: u32,
    sender: String,
    body: String,
    message_id: String,
    nonce: u32,
    recipient_policy: String,
    return_address: String,
    expiry_slot: u64,
}

/// Parse MessageRedemptionDatum from JSON or CBOR hex
/// Structure: Constr 0 [origin, sender, body, message_id, nonce,
///            recipient_policy, return_address, expiry_slot]
fn parse_message_redemption_datum(json_str: &str) -> Result<MessageRedemptionParsed> {
    let raw_json: serde_json::Value = serde_json::from_str(json_str)?;

    let json = normalize_datum(&raw_json)?;

    let fields = json
        .get("fields")
        .and_then(|f| f.as_array())
        .ok_or_else(|| {
            anyhow!(
                "Invalid MessageRedemptionDatum: missing fields (json: {})",
                json
            )
        })?;

    if fields.len() < 8 {
        return Err(anyhow!(
            "Invalid MessageRedemptionDatum: expected 8 fields, got {}",
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

    let recipient_policy = fields[5]
        .get("bytes")
        .and_then(|b| b.as_str())
        .ok_or_else(|| anyhow!("Invalid recipient_policy"))?
        .to_string();

    let return_address = fields[6]
        .get("bytes")
        .and_then(|b| b.as_str())
        .ok_or_else(|| anyhow!("Invalid return_address"))?
        .to_string();

    let expiry_slot = fields[7]
        .get("int")
        .and_then(|i| i.as_u64())
        .ok_or_else(|| anyhow!("Invalid expiry_slot"))?;

    Ok(MessageRedemptionParsed {
        origin,
        sender,
        body,
        message_id,
        nonce,
        recipient_policy,
        return_address,
        expiry_slot,
    })
}

/// Build Receive redeemer: ClaimMessage = constructor 0
fn build_receive_redeemer() -> Vec<u8> {
    let mut builder = CborBuilder::new();
    builder.start_constr(0).end_constr();
    builder.build()
}

/// Build Expire redeemer: ExpireMessage = constructor 1
fn build_expire_redeemer() -> Vec<u8> {
    let mut builder = CborBuilder::new();
    builder.start_constr(1).end_constr();
    builder.build()
}

/// Build message NFT burn redeemer: BurnMessage = constructor 1
fn build_nft_burn_redeemer() -> Vec<u8> {
    let mut builder = CborBuilder::new();
    builder.start_constr(1).end_constr();
    builder.build()
}

fn decode_body_utf8(hex_body: &str) -> Option<String> {
    let bytes = hex::decode(hex_body).ok()?;

    match String::from_utf8(bytes) {
        Ok(s) => {
            if s.chars()
                .all(|c| c.is_ascii_graphic() || c.is_ascii_whitespace())
            {
                Some(s)
            } else {
                None
            }
        }
        Err(_) => None,
    }
}

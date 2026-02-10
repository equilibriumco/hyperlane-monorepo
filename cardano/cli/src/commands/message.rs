//! Message delivery commands
//!
//! Commands for listing and receiving verified messages delivered to
//! recipient script addresses. Messages are created by the mailbox during
//! Process TX and delivered directly to the recipient's address with a
//! verified_message_nft token.

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};
use colored::Colorize;

use crate::utils::blockfrost::BlockfrostClient;
use crate::utils::cbor::{normalize_datum, CborBuilder};
use crate::utils::context::CliContext;
use crate::utils::plutus::{apply_validator_param, encode_script_hash_param};
use crate::utils::tx_builder::HyperlaneTxBuilder;

/// Auto-derived message infrastructure from deployment_info.json
pub struct MessageInfra {
    pub verified_message_nft_policy: String,
}

/// Derive verified_message_nft_policy from deployment_info.
///
/// The policy is read from `mailbox.appliedParameters` where
/// `name == "verified_message_nft_policy"`.
pub fn resolve_message_infra(ctx: &CliContext) -> Result<MessageInfra> {
    let deployment_info = ctx.load_deployment_info()?;
    let mailbox_info = deployment_info
        .mailbox
        .as_ref()
        .ok_or_else(|| anyhow!("Mailbox not found in deployment_info.json"))?;

    let verified_message_nft_policy = mailbox_info
        .applied_parameters
        .iter()
        .find(|p| p.name == "verified_message_nft_policy")
        .map(|p| p.value.clone())
        .ok_or_else(|| {
            anyhow!("verified_message_nft_policy not found in mailbox.appliedParameters")
        })?;

    Ok(MessageInfra {
        verified_message_nft_policy,
    })
}

#[derive(Args)]
pub struct MessageArgs {
    #[command(subcommand)]
    command: MessageCommands,
}

#[derive(Subcommand)]
enum MessageCommands {
    /// List pending messages at a recipient script address
    List {
        /// Recipient script address to check for messages
        #[arg(long)]
        recipient_address: String,

        /// Verified message NFT policy ID (auto-derived if omitted)
        #[arg(long)]
        message_nft_policy: Option<String>,

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

    /// Receive a message (spends message UTXO, burns NFT)
    ///
    /// The receive transaction:
    /// 1. Spends the verified message UTXO at recipient's address
    /// 2. Burns the verified_message_nft
    /// 3. Spends the recipient's state UTXO (proves authorization)
    /// 4. Optionally updates recipient state
    Receive {
        /// Message UTXO to receive (format: txhash#index)
        #[arg(long)]
        message_utxo: String,

        /// Recipient state NFT policy ID (to find recipient UTXO)
        #[arg(long)]
        recipient_policy: String,

        /// Verified message NFT policy ID (auto-derived if omitted)
        #[arg(long)]
        message_nft_policy: Option<String>,

        /// Recipient state NFT asset name (hex, empty for unit)
        #[arg(long, default_value = "")]
        recipient_state_asset: String,

        /// Reference script UTXO for message NFT policy (format: txhash#index)
        #[arg(long)]
        nft_ref_script: Option<String>,

        /// CBOR hex of recipient spend redeemer (for script-based recipients)
        #[arg(long)]
        recipient_redeemer: Option<String>,

        /// CBOR hex of updated state datum (replaces existing datum if provided)
        #[arg(long)]
        recipient_new_datum: Option<String>,

        /// Reference script UTXO for recipient validator (auto-discovered if omitted)
        #[arg(long)]
        recipient_ref_script: Option<String>,

        /// Dry run (don't submit transaction)
        #[arg(long)]
        dry_run: bool,
    },
}

pub async fn execute(ctx: &CliContext, args: MessageArgs) -> Result<()> {
    match args.command {
        MessageCommands::List {
            recipient_address,
            message_nft_policy,
            format,
            show_body,
        } => {
            let nft_policy = resolve_nft_policy(ctx, message_nft_policy)?;
            list_messages(ctx, &recipient_address, &nft_policy, &format, show_body).await
        }

        MessageCommands::Show { message_utxo } => show_message(ctx, &message_utxo).await,

        MessageCommands::Receive {
            message_utxo,
            recipient_policy,
            message_nft_policy,
            recipient_state_asset,
            nft_ref_script,
            recipient_redeemer,
            recipient_new_datum,
            recipient_ref_script,
            dry_run,
        } => {
            let nft_policy = resolve_nft_policy(ctx, message_nft_policy)?;
            receive_message(
                ctx,
                &message_utxo,
                &nft_policy,
                &recipient_policy,
                &recipient_state_asset,
                nft_ref_script,
                recipient_redeemer,
                recipient_new_datum,
                recipient_ref_script,
                dry_run,
            )
            .await
        }
    }
}

fn resolve_nft_policy(ctx: &CliContext, override_policy: Option<String>) -> Result<String> {
    if let Some(p) = override_policy {
        return Ok(p);
    }
    println!("{}", "Auto-deriving verified message NFT policy from deployment_info...".dimmed());
    let infra = resolve_message_infra(ctx)?;
    Ok(infra.verified_message_nft_policy)
}

async fn list_messages(
    ctx: &CliContext,
    recipient_address: &str,
    message_nft_policy: &str,
    format: &str,
    show_body: bool,
) -> Result<()> {
    println!("{}", "Listing pending verified messages...".cyan());

    println!("  Recipient Address: {}", recipient_address);
    println!("  Verified Message NFT Policy: {}", message_nft_policy);

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let utxos = client.get_utxos(recipient_address).await?;

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
                    if let Ok(parsed) = parse_verified_message_datum(&datum_str) {
                        println!("  Origin:     {}", parsed.origin);
                        println!("  Sender:     {}", parsed.sender);
                        println!("  Nonce:      {}", parsed.nonce);
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

    println!("\n{}", "Verified Message UTXO Details:".green());
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
        println!("\n  {}", "VerifiedMessageDatum:".green());
        let datum_str = serde_json::to_string(datum_json)?;
        if let Ok(parsed) = parse_verified_message_datum(&datum_str) {
            println!("    Origin: {}", parsed.origin);
            println!("    Sender: {}", parsed.sender);
            println!("    Message ID: {}", parsed.message_id);
            println!("    Nonce: {}", parsed.nonce);
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
    recipient_policy: &str,
    recipient_state_asset: &str,
    nft_ref_script: Option<String>,
    recipient_redeemer: Option<String>,
    recipient_new_datum: Option<String>,
    recipient_ref_script: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Receiving verified message...".cyan());

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
        .ok_or_else(|| anyhow!("Verified message NFT not found in UTXO"))?;

    println!("  Message ID: {}", message_id);

    // Parse datum
    let datum_json = message_utxo
        .inline_datum
        .as_ref()
        .ok_or_else(|| anyhow!("Message UTXO has no inline datum"))?;
    let datum_str = serde_json::to_string(datum_json)?;
    let parsed_datum = parse_verified_message_datum(&datum_str)?;
    println!("  Origin: {}", parsed_datum.origin);
    println!("  Sender: {}", parsed_datum.sender);
    println!("  Nonce: {}", parsed_datum.nonce);

    // 2. Fetch recipient state UTXO (must be spent to prove authorization)
    let recipient_state_utxo = client
        .find_utxo_by_asset(recipient_policy, recipient_state_asset)
        .await?
        .ok_or_else(|| {
            anyhow!(
                "Recipient state UTXO not found with policy {}",
                recipient_policy
            )
        })?;

    println!("\n{}", "Recipient State UTXO:".green());
    println!(
        "  {}#{}",
        recipient_state_utxo.tx_hash, recipient_state_utxo.output_index
    );

    // 2b. Auto-discover recipient reference script if not provided
    let recipient_ref_script = if recipient_ref_script.is_some() {
        recipient_ref_script
    } else {
        // Look for ref script NFT: same policy, asset name "726566" ("ref")
        match client.find_utxo_by_asset(recipient_policy, "726566").await? {
            Some(ref_utxo) => {
                let ref_str = format!("{}#{}", ref_utxo.tx_hash, ref_utxo.output_index);
                println!("  Auto-discovered recipient ref script: {}", ref_str);
                Some(ref_str)
            }
            None => None,
        }
    };

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

    // Load inline script for NFT if reference script is not provided
    let deployment_info = ctx.load_deployment_info()?;
    let mailbox_info = deployment_info
        .mailbox
        .as_ref()
        .ok_or_else(|| anyhow!("Mailbox not found in deployment_info.json"))?;
    let mailbox_policy = mailbox_info
        .state_nft_policy
        .as_ref()
        .ok_or_else(|| anyhow!("Missing mailbox.stateNftPolicy in deployment_info.json"))?;

    let nft_inline_script = if nft_ref_script.is_none() {
        println!("  Loading verified_message_nft inline script");
        let mailbox_policy_param = encode_script_hash_param(mailbox_policy)?;
        let mailbox_policy_hex = hex::encode(&mailbox_policy_param);
        let applied = apply_validator_param(
            &ctx.contracts_dir,
            "verified_message_nft",
            "verified_message_nft",
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
            &nft_redeemer,
            nft_ref_script.as_deref(),
            nft_inline_script.as_deref(),
            recipient_redeemer_bytes.as_deref(),
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


pub fn parse_utxo_ref(s: &str) -> Result<(String, u32)> {
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
pub struct VerifiedMessageParsed {
    pub origin: u32,
    pub sender: String,
    pub body: String,
    pub message_id: String,
    pub nonce: u32,
}

/// Parse VerifiedMessageDatum from JSON or CBOR hex
/// Structure: Constr 0 [origin, sender, body, message_id, nonce]
pub fn parse_verified_message_datum(json_str: &str) -> Result<VerifiedMessageParsed> {
    let raw_json: serde_json::Value = serde_json::from_str(json_str)?;

    let json = normalize_datum(&raw_json)?;

    let fields = json
        .get("fields")
        .and_then(|f| f.as_array())
        .ok_or_else(|| {
            anyhow!(
                "Invalid VerifiedMessageDatum: missing fields (json: {})",
                json
            )
        })?;

    if fields.len() < 5 {
        return Err(anyhow!(
            "Invalid VerifiedMessageDatum: expected 5 fields, got {}",
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

    Ok(VerifiedMessageParsed {
        origin,
        sender,
        body,
        message_id,
        nonce,
    })
}

/// Build message NFT burn redeemer: BurnMessage = constructor 1
fn build_nft_burn_redeemer() -> Vec<u8> {
    let mut builder = CborBuilder::new();
    builder.start_constr(1).end_constr();
    builder.build()
}

pub fn decode_body_utf8(hex_body: &str) -> Option<String> {
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

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
use crate::utils::tx_builder::{calibrate_ex_units, RedeemerRef, PLACEHOLDER_EX_UNITS};

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
    /// Dispatch an arbitrary message to a remote domain
    ///
    /// The sender stamped into the message is derived from the wallet UTXO
    /// spent as `sender_ref`, so it cannot be chosen. This is the generic
    /// path; warp routes dispatch through `warp transfer` instead.
    Dispatch {
        /// Destination domain ID
        #[arg(long)]
        destination: u32,

        /// Recipient address, 32 bytes hex
        #[arg(long)]
        recipient: String,

        /// Message body, hex encoded
        #[arg(long)]
        body: String,

        /// Application gas for the destination. Omit to dispatch without
        /// paying interchain gas, in which case the relayer will hold the
        /// message until someone pays for it.
        #[arg(long)]
        gas_limit: Option<u64>,

        /// Print the transaction without submitting it
        #[arg(long)]
        dry_run: bool,
    },

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

        MessageCommands::Dispatch {
            destination,
            recipient,
            body,
            gas_limit,
            dry_run,
        } => dispatch(ctx, destination, &recipient, &body, gas_limit, dry_run).await,
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
    println!(
        "{}",
        "Auto-deriving verified message NFT policy from deployment_info...".dimmed()
    );
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
        match client
            .find_utxo_by_asset(recipient_policy, "726566")
            .await?
        {
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
            anyhow!(
                "No suitable fee UTXO found (need >= 5 ADA without tokens or reference scripts)"
            )
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

/// Dispatch an arbitrary message to a remote domain.
///
/// This is the generic sender path: any wallet can dispatch, and the mailbox
/// derives the sender from the input spent as `sender_ref` rather than taking
/// it as a parameter, so it cannot be forged. A wallet sender is stamped as
/// `0x00000000 || key_hash`. Warp routes have their own path through
/// `warp transfer`, which additionally moves tokens.
async fn dispatch(
    ctx: &CliContext,
    destination: u32,
    recipient: &str,
    body: &str,
    gas_limit: Option<u64>,
    dry_run: bool,
) -> Result<()> {
    use pallas_crypto::hash::Hash;
    use pallas_txbuilder::{BuildConway, Input, Output, ScriptKind, StagingTransaction};

    use crate::commands::igp::{
        build_pay_for_gas_redeemer, calculate_gas_payment, get_igp_policy, parse_igp_datum,
    };
    use crate::commands::warp::{
        compute_message_id_for_transfer, parse_mailbox_datum_for_transfer,
        update_merkle_tree_for_transfer,
    };
    use crate::utils::cbor::{
        build_igp_datum, build_mailbox_datum, build_mailbox_dispatch_redeemer,
    };

    println!(
        "\n{}",
        "═══════════════════════════════════════════════════════════════".cyan()
    );
    println!("{}", "Dispatching Message".cyan().bold());
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════".cyan()
    );

    let recipient = recipient.strip_prefix("0x").unwrap_or(recipient);
    if recipient.len() != 64 {
        return Err(anyhow!(
            "Recipient must be 32 bytes (64 hex chars), got {} chars",
            recipient.len()
        ));
    }
    let body = body.strip_prefix("0x").unwrap_or(body);
    hex::decode(body).map_err(|e| anyhow!("Body must be hex: {e}"))?;

    let api_key = ctx.require_api_key()?;
    let keypair = ctx.load_signing_key()?;
    let payer_pkh = keypair.verification_key_hash_hex();
    let payer_address = keypair.address_bech32(ctx.pallas_network());
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let deployment = ctx.load_deployment_info()?;
    let mailbox_info = deployment
        .mailbox
        .as_ref()
        .ok_or_else(|| anyhow!("Mailbox not found in deployment_info.json"))?;
    let mailbox_policy = mailbox_info
        .state_nft
        .as_ref()
        .map(|n| n.policy_id.clone())
        .ok_or_else(|| anyhow!("Mailbox state NFT policy not found"))?;

    println!("\n{}", "Step 1: Reading mailbox state...".cyan());
    let mailbox_utxo = client
        .find_utxo_by_asset(&mailbox_policy, "")
        .await?
        .ok_or_else(|| anyhow!("Mailbox UTXO not found with policy {mailbox_policy}"))?;
    let mailbox_datum_val = mailbox_utxo
        .inline_datum
        .as_ref()
        .ok_or_else(|| anyhow!("Mailbox UTXO has no inline datum"))?;
    let mailbox_data = parse_mailbox_datum_for_transfer(mailbox_datum_val)?;
    println!("  Local Domain: {}", mailbox_data.local_domain);
    println!("  Outbound Nonce: {}", mailbox_data.outbound_nonce);

    // The sender is derived from an input we spend, so a wallet UTXO doubles as
    // the fee source and the sender reference.
    println!("\n{}", "Step 2: Selecting sender UTXO...".cyan());
    let payer_utxos = client.get_utxos(&payer_address).await?;
    let sender_utxo = payer_utxos
        .iter()
        .filter(|u| u.assets.is_empty() && u.lovelace >= 10_000_000)
        .max_by_key(|u| u.lovelace)
        .ok_or_else(|| {
            anyhow!("No ADA-only wallet UTXO of at least 10 ADA to use as fee and sender")
        })?;
    let collateral_utxo = payer_utxos
        .iter()
        .find(|u| {
            u.assets.is_empty()
                && u.lovelace >= 5_000_000
                && !(u.tx_hash == sender_utxo.tx_hash && u.output_index == sender_utxo.output_index)
        })
        .ok_or_else(|| anyhow!("No second ADA-only UTXO of at least 5 ADA for collateral"))?;
    println!(
        "  Sender UTXO: {}#{}",
        sender_utxo.tx_hash, sender_utxo.output_index
    );

    // A wallet sender is 0x00000000 || key hash, matching the mailbox's
    // get_sender_address for a VerificationKey credential.
    let sender_hex = format!("00000000{payer_pkh}");
    println!("  Sender: 0x{sender_hex}");

    println!("\n{}", "Step 3: Building message...".cyan());
    let message_id = compute_message_id_for_transfer(
        3,
        mailbox_data.outbound_nonce,
        mailbox_data.local_domain,
        &sender_hex,
        destination,
        recipient,
        body,
    )?;
    println!("  Destination: {destination}");
    println!("  Recipient: 0x{recipient}");
    println!("  Body: {} bytes", body.len() / 2);
    println!("  Message ID: 0x{message_id}");

    let new_merkle = update_merkle_tree_for_transfer(
        &mailbox_data.merkle_branches,
        mailbox_data.merkle_count,
        &message_id,
    )?;

    let branches_refs: Vec<&str> = new_merkle.branches.iter().map(|s| s.as_str()).collect();
    let new_mailbox_datum = build_mailbox_datum(
        mailbox_data.local_domain,
        &mailbox_data.default_ism,
        &mailbox_data.owner,
        mailbox_data.outbound_nonce + 1,
        &branches_refs,
        new_merkle.count,
        &mailbox_data.processed_tree_root,
    )?;

    let mailbox_redeemer = build_mailbox_dispatch_redeemer(
        destination,
        recipient,
        body,
        &sender_utxo.tx_hash,
        sender_utxo.output_index,
        &[],
    )?;

    // Optional interchain gas, paid in the same transaction so the dispatch
    // cannot land unpaid. Omitting --gas-limit dispatches without paying, which
    // is valid but leaves the message held until someone pays for it.
    let igp_data = if let Some(gas_lim) = gas_limit {
        println!("\n{}", "Step 4: Preparing gas payment...".cyan());
        let igp_policy_id = get_igp_policy(ctx, None)?;
        let igp_utxo = client
            .find_utxo_by_asset(&igp_policy_id, "")
            .await?
            .ok_or_else(|| anyhow!("IGP UTXO not found with policy {igp_policy_id}"))?;
        let igp_datum_val = igp_utxo
            .inline_datum
            .as_ref()
            .ok_or_else(|| anyhow!("IGP UTXO has no inline datum"))?;
        let (owner, beneficiary, gas_oracles) = parse_igp_datum(igp_datum_val)?;

        let (gas_price, exchange_rate, gas_overhead) = gas_oracles
            .iter()
            .find(|(d, _, _, _)| *d == destination)
            .map(|(_, gp, er, oh)| (*gp, *er, *oh))
            .ok_or_else(|| {
                anyhow!(
                    "No gas oracle configured for domain {destination}. \
                     Set one with `igp set-oracle --domain {destination} ...` first."
                )
            })?;

        // The contract adds the overhead itself; the redeemer carries only the
        // application gas.
        let total_gas = gas_lim + gas_overhead;
        let igp_payment = calculate_gas_payment(total_gas, gas_price, exchange_rate);
        println!("  Application gas: {gas_lim}");
        println!("  Overhead: {gas_overhead}");
        println!("  Payment: {igp_payment} lovelace");

        let igp_redeemer =
            build_pay_for_gas_redeemer(&hex::decode(&message_id)?, destination, gas_lim);
        let igp_redeemer = pallas_codec::minicbor::to_vec(&igp_redeemer)
            .map_err(|e| anyhow!("Failed to encode IGP redeemer: {e:?}"))?;
        let new_igp_datum = build_igp_datum(
            &hex::encode(&owner),
            &hex::encode(&beneficiary),
            &gas_oracles,
        )?;
        Some((
            igp_utxo,
            igp_policy_id,
            igp_payment,
            igp_redeemer,
            new_igp_datum,
        ))
    } else {
        println!(
            "\n  {}",
            "No --gas-limit: dispatching without paying interchain gas".yellow()
        );
        None
    };

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        println!("  Message ID: 0x{message_id}");
        return Ok(());
    }

    println!("\n{}", "Step 5: Building transaction...".cyan());
    let current_slot = client.get_latest_slot().await?;
    let validity_end = current_slot + 7200;
    let cost_model = client.get_plutusv3_cost_model().await?;

    let mailbox_tx_hash: [u8; 32] = hex::decode(&mailbox_utxo.tx_hash)?
        .try_into()
        .map_err(|_| anyhow!("Invalid mailbox tx hash"))?;
    let sender_tx_hash: [u8; 32] = hex::decode(&sender_utxo.tx_hash)?
        .try_into()
        .map_err(|_| anyhow!("Invalid sender tx hash"))?;
    let collateral_tx_hash: [u8; 32] = hex::decode(&collateral_utxo.tx_hash)?
        .try_into()
        .map_err(|_| anyhow!("Invalid collateral tx hash"))?;
    let payer_pkh_bytes: [u8; 28] = hex::decode(&payer_pkh)?
        .try_into()
        .map_err(|_| anyhow!("Invalid payer key hash"))?;

    let mailbox_addr = pallas_addresses::Address::from_bech32(&mailbox_utxo.address)?;
    let payer_addr = pallas_addresses::Address::from_bech32(&payer_address)?;

    let mailbox_policy_bytes: [u8; 28] = hex::decode(&mailbox_policy)?
        .try_into()
        .map_err(|_| anyhow!("Invalid mailbox policy"))?;
    let mailbox_asset_name = mailbox_utxo
        .assets
        .iter()
        .find(|a| a.policy_id == mailbox_policy)
        .map(|a| hex::decode(&a.asset_name).unwrap_or_default())
        .unwrap_or_default();

    let mailbox_output = Output::new(mailbox_addr, mailbox_utxo.lovelace)
        .set_inline_datum(new_mailbox_datum)
        .add_asset(
            Hash::new(mailbox_policy_bytes),
            mailbox_asset_name.clone(),
            1,
        )
        .map_err(|e| anyhow!("Failed to add mailbox state NFT: {e:?}"))?;

    let fee_estimate = 2_500_000u64;
    let igp_pay_total = igp_data.as_ref().map(|d| d.2).unwrap_or(0);
    let change = sender_utxo
        .lovelace
        .saturating_sub(fee_estimate)
        .saturating_sub(igp_pay_total);
    if change < 1_000_000 {
        return Err(anyhow!(
            "Sender UTXO of {} lovelace does not cover the fee and gas payment",
            sender_utxo.lovelace
        ));
    }

    let mut staging = StagingTransaction::new()
        .input(Input::new(
            Hash::new(sender_tx_hash),
            sender_utxo.output_index as u64,
        ))
        .input(Input::new(
            Hash::new(mailbox_tx_hash),
            mailbox_utxo.output_index as u64,
        ))
        .collateral_input(Input::new(
            Hash::new(collateral_tx_hash),
            collateral_utxo.output_index as u64,
        ))
        .disclosed_signer(Hash::new(payer_pkh_bytes))
        .output(mailbox_output)
        .add_spend_redeemer(
            Input::new(Hash::new(mailbox_tx_hash), mailbox_utxo.output_index as u64),
            mailbox_redeemer.clone(),
            PLACEHOLDER_EX_UNITS,
        )
        .language_view(ScriptKind::PlutusV3, cost_model)
        .fee(fee_estimate)
        .invalid_from_slot(validity_end)
        .network_id(ctx.network_id());

    if let Some((ref igp_utxo, ref igp_policy_id, igp_pay, ref igp_redeemer, ref new_igp_datum)) =
        igp_data
    {
        let igp_tx_hash: [u8; 32] = hex::decode(&igp_utxo.tx_hash)?
            .try_into()
            .map_err(|_| anyhow!("Invalid IGP tx hash"))?;
        let igp_policy_bytes: [u8; 28] = hex::decode(igp_policy_id)?
            .try_into()
            .map_err(|_| anyhow!("Invalid IGP policy"))?;
        let igp_asset_name = igp_utxo
            .assets
            .iter()
            .find(|a| a.policy_id == *igp_policy_id)
            .map(|a| hex::decode(&a.asset_name).unwrap_or_default())
            .unwrap_or_default();
        let igp_addr = pallas_addresses::Address::from_bech32(&igp_utxo.address)?;

        let igp_output = Output::new(igp_addr, igp_utxo.lovelace + igp_pay)
            .set_inline_datum(new_igp_datum.clone())
            .add_asset(Hash::new(igp_policy_bytes), igp_asset_name, 1)
            .map_err(|e| anyhow!("Failed to add IGP state NFT: {e:?}"))?;

        staging = staging
            .input(Input::new(
                Hash::new(igp_tx_hash),
                igp_utxo.output_index as u64,
            ))
            .output(igp_output)
            .add_spend_redeemer(
                Input::new(Hash::new(igp_tx_hash), igp_utxo.output_index as u64),
                igp_redeemer.clone(),
                PLACEHOLDER_EX_UNITS,
            );

        if let Some(rs) = deployment
            .igp
            .as_ref()
            .and_then(|i| i.reference_script_utxo.as_ref())
        {
            let rs_hash: [u8; 32] = hex::decode(&rs.tx_hash)?
                .try_into()
                .map_err(|_| anyhow!("Invalid IGP reference script tx hash"))?;
            staging =
                staging.reference_input(Input::new(Hash::new(rs_hash), rs.output_index as u64));
        }
    }

    if let Some(rs) = mailbox_info.reference_script_utxo.as_ref() {
        let rs_hash: [u8; 32] = hex::decode(&rs.tx_hash)?
            .try_into()
            .map_err(|_| anyhow!("Invalid mailbox reference script tx hash"))?;
        staging = staging.reference_input(Input::new(Hash::new(rs_hash), rs.output_index as u64));
    } else {
        let script_raw = ctx.load_script_from_blueprint("mailbox", "mailbox.spend")?;
        staging = staging.script(ScriptKind::PlutusV3, hex::decode(&script_raw)?);
    }

    staging = staging.output(Output::new(payer_addr, change));

    let mut declared = vec![(
        RedeemerRef::Spend(Input::new(
            Hash::new(mailbox_tx_hash),
            mailbox_utxo.output_index as u64,
        )),
        mailbox_redeemer.clone(),
    )];
    if let Some((ref igp_utxo, _, _, ref igp_redeemer, _)) = igp_data {
        let hash: [u8; 32] = hex::decode(&igp_utxo.tx_hash)?
            .try_into()
            .map_err(|_| anyhow!("Invalid IGP tx hash"))?;
        declared.push((
            RedeemerRef::Spend(Input::new(Hash::new(hash), igp_utxo.output_index as u64)),
            igp_redeemer.clone(),
        ));
    }
    let staging = calibrate_ex_units(&client, staging, declared).await?;
    let tx = staging
        .build_conway_raw()
        .map_err(|e| anyhow!("Failed to build transaction: {e:?}"))?;

    println!("  TX Hash: {}", hex::encode(&tx.tx_hash.0));

    let signature = keypair.sign(&tx.tx_hash.0);
    let signed_tx = tx
        .add_signature(keypair.pallas_public_key().clone(), signature)
        .map_err(|e| anyhow!("Failed to sign transaction: {e:?}"))?;

    println!("\n{}", "Step 6: Submitting...".cyan());
    let submitted = client
        .submit_and_confirm(&signed_tx.tx_bytes.0, ctx.no_wait)
        .await?;

    println!("\n{}", "Dispatched".green().bold());
    println!("  TX: {submitted}");
    println!("  Message ID: 0x{message_id}");
    Ok(())
}

//! Query command - Query contract state and UTXOs

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand, ValueEnum};
use colored::Colorize;

use crate::utils::blockfrost::BlockfrostClient;
use crate::utils::context::CliContext;
use crate::utils::plutus::script_hash_to_address;

#[derive(Args)]
pub struct QueryArgs {
    #[command(subcommand)]
    command: QueryCommands,
}

#[derive(Subcommand)]
enum QueryCommands {
    /// Query mailbox state
    Mailbox {
        /// Mailbox policy ID (for finding UTXO)
        #[arg(long)]
        mailbox_policy: Option<String>,
    },

    /// Query ISM configuration
    Ism {
        /// ISM policy ID
        #[arg(long)]
        ism_policy: Option<String>,
    },

    /// Query UTXOs at an address
    Utxos {
        /// Address to query
        address: String,

        /// Output format
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },

    /// Query a specific UTXO
    Utxo {
        /// UTXO reference (tx_hash#index)
        utxo: String,
    },

    /// Query protocol parameters
    Params,

    /// Query latest block/slot
    Tip,

    /// Query transaction details
    Tx {
        /// Transaction hash
        tx_hash: String,
    },

    /// Query asset information
    Asset {
        /// Asset unit (policy_id + asset_name hex)
        unit: String,
    },

    /// Query message processing status
    Message {
        /// Message ID (32 bytes hex)
        #[arg(long)]
        message_id: String,

        /// Mailbox address
        #[arg(long)]
        mailbox_address: Option<String>,
    },

    /// Query generic recipient state and received messages
    Recipient {
        /// Recipient script hash (28 bytes hex, as shown in registry)
        #[arg(long)]
        script_hash: Option<String>,

        /// Recipient state NFT policy ID (alternative to script-hash)
        #[arg(long)]
        policy: Option<String>,

        /// Show message history from spent UTXOs
        #[arg(long)]
        history: bool,

        /// Number of transactions to scan for history (default 100)
        #[arg(long, default_value = "100")]
        history_limit: u32,

        /// Show raw datum JSON
        #[arg(long)]
        raw: bool,
    },
}

#[derive(Clone, ValueEnum)]
enum OutputFormat {
    Table,
    Json,
}

pub async fn execute(ctx: &CliContext, args: QueryArgs) -> Result<()> {
    match args.command {
        QueryCommands::Mailbox { mailbox_policy } => query_mailbox(ctx, mailbox_policy).await,
        QueryCommands::Ism { ism_policy } => query_ism(ctx, ism_policy).await,
        QueryCommands::Utxos { address, format } => query_utxos(ctx, &address, format).await,
        QueryCommands::Utxo { utxo } => query_utxo(ctx, &utxo).await,
        QueryCommands::Params => query_params(ctx).await,
        QueryCommands::Tip => query_tip(ctx).await,
        QueryCommands::Tx { tx_hash } => query_tx(ctx, &tx_hash).await,
        QueryCommands::Asset { unit } => query_asset(ctx, &unit).await,
        QueryCommands::Message {
            message_id,
            mailbox_address,
        } => query_message(ctx, &message_id, mailbox_address).await,
        QueryCommands::Recipient { script_hash, policy, history, history_limit, raw } => {
            query_recipient(ctx, script_hash, policy, history, history_limit, raw).await
        }
    }
}

async fn query_mailbox(ctx: &CliContext, mailbox_policy: Option<String>) -> Result<()> {
    println!("{}", "Querying Mailbox state...".cyan());

    let policy_id = match mailbox_policy {
        Some(p) => p,
        None => {
            let deployment = ctx.load_deployment_info()?;
            deployment
                .mailbox
                .and_then(|m| m.state_nft_policy)
                .ok_or_else(|| anyhow!("Mailbox policy not found"))?
        }
    };

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let mailbox_utxo = client
        .find_utxo_by_asset(&policy_id, "")
        .await?
        .ok_or_else(|| anyhow!("Mailbox UTXO not found with policy {}", policy_id))?;

    println!("\n{}", "Mailbox UTXO:".green());
    println!("  TX: {}#{}", mailbox_utxo.tx_hash, mailbox_utxo.output_index);
    println!("  Address: {}", mailbox_utxo.address);
    println!("  Lovelace: {}", mailbox_utxo.lovelace);

    if let Some(datum) = &mailbox_utxo.inline_datum {
        println!("\n{}", "Mailbox State:".green());

        if let Some(fields) = datum.get("fields").and_then(|f| f.as_array()) {
            let local_domain = fields.get(0)
                .and_then(|d| d.get("int"))
                .and_then(|i| i.as_u64());
            let default_ism = fields.get(1)
                .and_then(|i| i.get("bytes"))
                .and_then(|b| b.as_str());
            let owner = fields.get(2)
                .and_then(|o| o.get("bytes"))
                .and_then(|b| b.as_str());
            let outbound_nonce = fields.get(3)
                .and_then(|n| n.get("int"))
                .and_then(|i| i.as_u64());
            let merkle_root = fields.get(4)
                .and_then(|r| r.get("bytes"))
                .and_then(|b| b.as_str());
            let merkle_count = fields.get(5)
                .and_then(|c| c.get("int"))
                .and_then(|i| i.as_u64());

            println!("  Local Domain: {:?}", local_domain);
            println!("  Default ISM: {:?}", default_ism);
            println!("  Owner: {:?}", owner);
            println!("  Outbound Nonce: {:?}", outbound_nonce);
            println!("  Merkle Root: {:?}", merkle_root);
            println!("  Merkle Count: {:?}", merkle_count);
        }

        println!("\n{}", "Raw Datum:".yellow());
        println!("{}", serde_json::to_string_pretty(datum)?);
    }

    Ok(())
}

async fn query_ism(ctx: &CliContext, ism_policy: Option<String>) -> Result<()> {
    println!("{}", "Querying ISM configuration...".cyan());

    let policy_id = match ism_policy {
        Some(p) => p,
        None => {
            let deployment = ctx.load_deployment_info()?;
            deployment
                .ism
                .and_then(|i| i.state_nft_policy)
                .ok_or_else(|| anyhow!("ISM policy not found"))?
        }
    };

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let ism_utxo = client
        .find_utxo_by_asset(&policy_id, "")
        .await?
        .ok_or_else(|| anyhow!("ISM UTXO not found with policy {}", policy_id))?;

    println!("\n{}", "ISM UTXO:".green());
    println!("  TX: {}#{}", ism_utxo.tx_hash, ism_utxo.output_index);
    println!("  Address: {}", ism_utxo.address);
    println!("  Lovelace: {}", ism_utxo.lovelace);

    if let Some(datum) = &ism_utxo.inline_datum {
        println!("\n{}", "ISM Configuration:".green());
        println!("{}", serde_json::to_string_pretty(datum)?);
    }

    Ok(())
}

async fn query_utxos(ctx: &CliContext, address: &str, format: OutputFormat) -> Result<()> {
    println!("{}", format!("Querying UTXOs at {}...", address).cyan());

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let utxos = client.get_utxos(address).await?;

    match format {
        OutputFormat::Table => {
            println!("\n{} UTXOs found:", utxos.len());
            println!("{}", "-".repeat(100));

            for utxo in &utxos {
                println!(
                    "{}#{} - {} lovelace",
                    &utxo.tx_hash[..16],
                    utxo.output_index,
                    utxo.lovelace
                );
                for asset in &utxo.assets {
                    println!(
                        "  + {} {} ({}...)",
                        asset.quantity,
                        if asset.asset_name.is_empty() {
                            "(no name)"
                        } else {
                            &asset.asset_name
                        },
                        &asset.policy_id[..16]
                    );
                }
                if utxo.inline_datum.is_some() {
                    println!("  [has inline datum]");
                }
            }

            let total: u64 = utxos.iter().map(|u| u.lovelace).sum();
            println!("\nTotal: {} lovelace ({:.2} ADA)", total, total as f64 / 1_000_000.0);
        }
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&utxos)?);
        }
    }

    Ok(())
}

async fn query_utxo(ctx: &CliContext, utxo_ref: &str) -> Result<()> {
    let parts: Vec<&str> = utxo_ref.split('#').collect();
    if parts.len() != 2 {
        return Err(anyhow!("Invalid UTXO format. Use tx_hash#index"));
    }

    let tx_hash = parts[0];
    let _output_index: u32 = parts[1].parse()?;

    println!("{}", format!("Querying UTXO {}...", utxo_ref).cyan());

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    // Get transaction UTXOs
    let tx = client.get_tx(tx_hash).await?;

    println!("\n{}", "Transaction:".green());
    println!("{}", serde_json::to_string_pretty(&tx)?);

    Ok(())
}

async fn query_params(ctx: &CliContext) -> Result<()> {
    println!("{}", "Querying protocol parameters...".cyan());

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let params = client.get_protocol_params().await?;

    println!("\n{}", "Protocol Parameters:".green());
    println!("{}", serde_json::to_string_pretty(&params)?);

    Ok(())
}

async fn query_tip(ctx: &CliContext) -> Result<()> {
    println!("{}", "Querying chain tip...".cyan());

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let slot = client.get_latest_slot().await?;

    println!("\n{}", "Chain Tip:".green());
    println!("  Latest Slot: {}", slot);

    Ok(())
}

async fn query_tx(ctx: &CliContext, tx_hash: &str) -> Result<()> {
    println!("{}", format!("Querying transaction {}...", tx_hash).cyan());

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let tx = client.get_tx(tx_hash).await?;

    println!("\n{}", "Transaction:".green());
    println!("{}", serde_json::to_string_pretty(&tx)?);

    println!("\n{}", "Explorer:".yellow());
    println!("  {}", ctx.explorer_tx_url(tx_hash));

    Ok(())
}

async fn query_asset(ctx: &CliContext, unit: &str) -> Result<()> {
    println!("{}", format!("Querying asset {}...", unit).cyan());

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    // Parse unit into policy_id and asset_name
    let (policy_id, asset_name) = if unit.len() > 56 {
        (&unit[..56], &unit[56..])
    } else {
        (unit, "")
    };

    let utxo = client.find_utxo_by_asset(policy_id, asset_name).await?;

    if let Some(u) = utxo {
        println!("\n{}", "Asset Found:".green());
        println!("  Policy ID: {}", policy_id);
        println!("  Asset Name: {}", if asset_name.is_empty() { "(empty)" } else { asset_name });
        println!("  Location: {}#{}", u.tx_hash, u.output_index);
        println!("  Address: {}", u.address);
    } else {
        println!("\n{}", "Asset not found".yellow());
    }

    Ok(())
}

async fn query_message(
    ctx: &CliContext,
    message_id: &str,
    mailbox_address: Option<String>,
) -> Result<()> {
    println!("{}", format!("Querying message {}...", message_id).cyan());

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    // Get mailbox address
    let address = match mailbox_address {
        Some(a) => a,
        None => {
            let deployment = ctx.load_deployment_info()?;
            deployment
                .mailbox
                .map(|m| m.address)
                .ok_or_else(|| anyhow!("Mailbox address not found"))?
        }
    };

    // Look for processed message marker UTXO
    // The message ID would be stored in a datum at the mailbox address
    let utxos = client.get_utxos(&address).await?;

    println!("\n{}", "Searching for processed message...".cyan());

    for utxo in &utxos {
        if let Some(datum) = &utxo.inline_datum {
            // Check if this is a ProcessedMessageDatum with matching message_id
            if let Some(fields) = datum.get("fields").and_then(|f| f.as_array()) {
                if let Some(id) = fields.get(0).and_then(|i| i.get("bytes")).and_then(|b| b.as_str()) {
                    if id.to_lowercase() == message_id.to_lowercase().strip_prefix("0x").unwrap_or(message_id) {
                        println!("\n{}", "Message PROCESSED:".green().bold());
                        println!("  UTXO: {}#{}", utxo.tx_hash, utxo.output_index);
                        return Ok(());
                    }
                }
            }
        }
    }

    println!("\n{}", "Message NOT YET PROCESSED".yellow());
    println!("The message may be pending or not yet relayed to Cardano.");

    Ok(())
}

async fn query_recipient(
    ctx: &CliContext,
    script_hash: Option<String>,
    policy: Option<String>,
    history: bool,
    history_limit: u32,
    raw: bool,
) -> Result<()> {
    println!("{}", "Querying Generic Recipient state...".cyan());

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    // Find recipient UTXO either by script hash or by NFT policy
    let (recipient_utxo, recipient_address) = if let Some(policy_id) = policy.clone() {
        // Find by NFT policy (more reliable)
        let utxo = client
            .find_utxo_by_asset(&policy_id, "")
            .await?
            .ok_or_else(|| anyhow!("Recipient UTXO not found with policy {}", policy_id))?;
        let addr = utxo.address.clone();
        (utxo, addr)
    } else if let Some(hash) = script_hash.clone() {
        // Convert script hash to bech32 address
        let addr = script_hash_to_address(&hash, ctx.pallas_network())?;
        // Find by address - get UTXOs and find one with inline datum
        let utxos = client.get_utxos(&addr).await?;
        let utxo = utxos
            .into_iter()
            .find(|u| u.inline_datum.is_some())
            .ok_or_else(|| anyhow!("No UTXO with inline datum found at {}", addr))?;
        (utxo, addr)
    } else {
        return Err(anyhow!(
            "Please provide either --script-hash or --policy to identify the recipient"
        ));
    };

    println!("\n{}", "Recipient UTXO:".green());
    println!("  TX: {}#{}", recipient_utxo.tx_hash, recipient_utxo.output_index);
    println!("  Address: {}", recipient_utxo.address);
    println!("  Lovelace: {} ({:.2} ADA)",
        recipient_utxo.lovelace,
        recipient_utxo.lovelace as f64 / 1_000_000.0
    );

    // Show assets
    if !recipient_utxo.assets.is_empty() {
        println!("\n{}", "Assets:".green());
        for asset in &recipient_utxo.assets {
            let name_display = if asset.asset_name.is_empty() {
                "(empty name)".to_string()
            } else {
                // Try to decode as UTF-8
                match hex::decode(&asset.asset_name) {
                    Ok(bytes) => String::from_utf8(bytes)
                        .unwrap_or_else(|_| format!("0x{}", asset.asset_name)),
                    Err(_) => asset.asset_name.clone(),
                }
            };
            println!("  {} x {} (policy: {}...)",
                asset.quantity,
                name_display,
                &asset.policy_id[..16]
            );
        }
    }

    // Parse datum
    if let Some(datum) = &recipient_utxo.inline_datum {
        println!("\n{}", "Recipient State:".green());

        // Blockfrost may return datum as either:
        // 1. JSON object with "constructor" and "fields" (decoded)
        // 2. Hex string (raw CBOR)
        let parsed = if let Some(hex_str) = datum.as_str() {
            // Raw CBOR hex - decode it
            parse_cbor_recipient_datum(hex_str)
        } else if datum.get("fields").is_some() {
            // Already decoded JSON
            parse_json_recipient_datum(datum)
        } else {
            None
        };

        if let Some((ism, nonce, messages_received, last_message)) = parsed {
            println!("  ISM Override: {}", ism.as_deref().unwrap_or("None (using default)"));
            println!("  Last Processed Nonce: {}",
                nonce.map(|n| n.to_string()).unwrap_or_else(|| "None".to_string())
            );

            println!("\n{}", "Message Statistics:".cyan());
            println!("  Messages Received: {}", messages_received);

            if let Some(msg_bytes) = last_message {
                println!("\n{}", "Last Message:".cyan());
                println!("  Hex: {}", hex::encode(&msg_bytes));

                // Try to decode as UTF-8
                if let Ok(text) = String::from_utf8(msg_bytes.clone()) {
                    if text.chars().all(|c| !c.is_control() || c == '\n' || c == '\t') {
                        println!("  UTF-8: {}", text);
                    }
                }
                println!("  Length: {} bytes", msg_bytes.len());
            } else {
                println!("\n{}", "Last Message: None (no messages received yet)".yellow());
            }
        } else {
            println!("  (Could not parse datum structure)");
        }

        if raw {
            println!("\n{}", "Raw Datum:".yellow());
            println!("{}", serde_json::to_string_pretty(datum)?);
        }
    } else {
        println!("\n{}", "No inline datum found".yellow());
    }

    // Show message history if requested
    if history {
        println!("\n{}", "=".repeat(60));
        println!("{}", "Message History (from transaction history):".cyan().bold());
        println!("{}", "=".repeat(60));

        // Get transaction history for this address
        let txs = client.get_address_transactions(&recipient_address, history_limit).await?;
        println!("Scanning {} transactions...\n", txs.len());

        let mut messages: Vec<(String, u64, Option<i64>, Option<Vec<u8>>)> = Vec::new();

        for tx in &txs {
            // Get transaction UTXOs to see the spent inputs
            if let Ok(tx_utxos) = client.get_tx_utxos(&tx.tx_hash).await {
                // Look at outputs to this address (these contain the state after each message)
                for output in &tx_utxos.outputs {
                    if output.address == recipient_address {
                        if let Some(datum) = &output.inline_datum {
                            // Parse the datum to extract message info
                            let parsed = if let Some(hex_str) = datum.as_str() {
                                parse_cbor_recipient_datum(hex_str)
                            } else if datum.get("fields").is_some() {
                                parse_json_recipient_datum(datum)
                            } else {
                                None
                            };

                            if let Some((_ism, nonce, _count, last_msg)) = parsed {
                                // Store: tx_hash, block_time, nonce, message
                                messages.push((tx.tx_hash.clone(), tx.block_time, nonce, last_msg));
                            }
                        }
                    }
                }
            }
        }

        // Sort by nonce (ascending) to show messages in order
        messages.sort_by(|a, b| {
            let nonce_a = a.2.unwrap_or(0);
            let nonce_b = b.2.unwrap_or(0);
            nonce_a.cmp(&nonce_b)
        });

        // Deduplicate by nonce (keep first occurrence)
        messages.dedup_by(|a, b| a.2 == b.2);

        if messages.is_empty() {
            println!("{}", "No message history found.".yellow());
        } else {
            println!("Found {} messages:\n", messages.len());

            for (i, (tx_hash, block_time, nonce, msg)) in messages.iter().enumerate() {
                let nonce_str = nonce.map(|n| n.to_string()).unwrap_or_else(|| "?".to_string());

                // Format timestamp
                let datetime = chrono::DateTime::from_timestamp(*block_time as i64, 0)
                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                    .unwrap_or_else(|| block_time.to_string());

                println!("{}. [Nonce {}] - {}", i + 1, nonce_str, datetime);
                println!("   TX: {}...{}", &tx_hash[..8], &tx_hash[tx_hash.len()-8..]);

                if let Some(msg_bytes) = msg {
                    let hex_str = hex::encode(msg_bytes);
                    if hex_str.len() <= 64 {
                        println!("   Hex: {}", hex_str);
                    } else {
                        println!("   Hex: {}...", &hex_str[..64]);
                    }

                    // Try UTF-8 decode
                    if let Ok(text) = String::from_utf8(msg_bytes.clone()) {
                        if text.chars().all(|c| !c.is_control() || c == '\n' || c == '\t') {
                            if text.len() <= 80 {
                                println!("   UTF-8: {}", text);
                            } else {
                                println!("   UTF-8: {}...", &text[..80]);
                            }
                        }
                    }
                    println!("   Length: {} bytes", msg_bytes.len());
                } else {
                    println!("   Message: (empty or not captured)");
                }
                println!();
            }
        }
    }

    Ok(())
}

/// Parse an Option type from Plutus datum JSON
/// Option is represented as:
/// - None: { "constructor": 1, "fields": [] }
/// - Some(x): { "constructor": 0, "fields": [x] }
fn parse_option_bytes(value: Option<&serde_json::Value>) -> Option<String> {
    let v = value?;
    let constructor = v.get("constructor")?.as_u64()?;

    if constructor == 1 {
        // None
        None
    } else if constructor == 0 {
        // Some
        let fields = v.get("fields")?.as_array()?;
        let inner = fields.get(0)?;
        inner.get("bytes").and_then(|b| b.as_str()).map(String::from)
    } else {
        None
    }
}

fn parse_option_int(value: Option<&serde_json::Value>) -> Option<i64> {
    let v = value?;
    let constructor = v.get("constructor")?.as_u64()?;

    if constructor == 1 {
        // None
        None
    } else if constructor == 0 {
        // Some
        let fields = v.get("fields")?.as_array()?;
        let inner = fields.get(0)?;
        inner.get("int").and_then(|i| i.as_i64())
    } else {
        None
    }
}

/// Parse HyperlaneRecipientDatum from CBOR hex string
/// Returns: (ism: Option<String>, nonce: Option<i64>, messages_received: u64, last_message: Option<Vec<u8>>)
fn parse_cbor_recipient_datum(hex_str: &str) -> Option<(Option<String>, Option<i64>, u64, Option<Vec<u8>>)> {
    use ciborium::Value;

    let bytes = hex::decode(hex_str).ok()?;
    let value: Value = ciborium::from_reader(&bytes[..]).ok()?;

    // HyperlaneRecipientDatum is Constructor 0 (tag 121) with fields: [ism, nonce, inner]
    if let Value::Tag(121, box_fields) = value {
        if let Value::Array(fields) = *box_fields {
            if fields.len() >= 3 {
                // Field 0: ism (Option<ScriptHash>)
                // Constructor 0 (tag 121) = Some, Constructor 1 (tag 122) = None
                let ism = match &fields[0] {
                    Value::Tag(121, inner) => {
                        if let Value::Array(arr) = inner.as_ref() {
                            arr.first().and_then(|v| {
                                if let Value::Bytes(b) = v {
                                    Some(hex::encode(b))
                                } else {
                                    None
                                }
                            })
                        } else {
                            None
                        }
                    }
                    _ => None, // Tag 122 = None
                };

                // Field 1: nonce (Option<Int>)
                let nonce = match &fields[1] {
                    Value::Tag(121, inner) => {
                        if let Value::Array(arr) = inner.as_ref() {
                            arr.first().and_then(|v| {
                                if let Value::Integer(i) = v {
                                    i64::try_from(*i).ok()
                                } else {
                                    None
                                }
                            })
                        } else {
                            None
                        }
                    }
                    _ => None, // Tag 122 = None
                };

                // Field 2: inner (GenericRecipientInner)
                // Constructor 0 with fields: [messages_received, last_message]
                if let Value::Tag(121, inner_box) = &fields[2] {
                    if let Value::Array(inner_fields) = inner_box.as_ref() {
                        let messages_received = inner_fields.first().and_then(|v| {
                            if let Value::Integer(i) = v {
                                u64::try_from(*i).ok()
                            } else {
                                None
                            }
                        }).unwrap_or(0);

                        let last_message = inner_fields.get(1).and_then(|v| {
                            // Option<ByteArray>
                            if let Value::Tag(121, msg_inner) = v {
                                if let Value::Array(arr) = msg_inner.as_ref() {
                                    arr.first().and_then(|b| {
                                        if let Value::Bytes(bytes) = b {
                                            Some(bytes.clone())
                                        } else {
                                            None
                                        }
                                    })
                                } else {
                                    None
                                }
                            } else {
                                None // Tag 122 = None
                            }
                        });

                        return Some((ism, nonce, messages_received, last_message));
                    }
                }
            }
        }
    }

    None
}

/// Parse HyperlaneRecipientDatum from JSON (when Blockfrost decodes it)
fn parse_json_recipient_datum(datum: &serde_json::Value) -> Option<(Option<String>, Option<i64>, u64, Option<Vec<u8>>)> {
    let fields = datum.get("fields")?.as_array()?;

    // Field 0: ism (Option<ScriptHash>)
    let ism = parse_option_bytes(fields.get(0));

    // Field 1: nonce (Option<Int>)
    let nonce = parse_option_int(fields.get(1));

    // Field 2: inner (GenericRecipientInner)
    let inner = fields.get(2)?;
    let inner_fields = inner.get("fields")?.as_array()?;

    let messages_received = inner_fields.get(0)
        .and_then(|m| m.get("int"))
        .and_then(|i| i.as_u64())
        .unwrap_or(0);

    let last_message = parse_option_bytes(inner_fields.get(1))
        .and_then(|hex_str| hex::decode(&hex_str).ok());

    Some((ism, nonce, messages_received, last_message))
}

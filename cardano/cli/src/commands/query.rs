//! Query command - Query contract state and UTXOs

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand, ValueEnum};
use colored::Colorize;

use crate::utils::blockfrost::BlockfrostClient;
use crate::utils::context::CliContext;

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

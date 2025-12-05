//! Tx command - Transaction building and submission

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};
use colored::Colorize;

use crate::utils::blockfrost::BlockfrostClient;
use crate::utils::context::CliContext;

#[derive(Args)]
pub struct TxArgs {
    #[command(subcommand)]
    command: TxCommands,
}

#[derive(Subcommand)]
enum TxCommands {
    /// Submit a signed transaction
    Submit {
        /// Path to signed transaction file (.signed)
        tx_file: String,

        /// Wait for confirmation
        #[arg(long)]
        wait: bool,

        /// Timeout in seconds for waiting
        #[arg(long, default_value = "120")]
        timeout: u64,
    },

    /// Get transaction status
    Status {
        /// Transaction hash
        tx_hash: String,
    },

    /// Wait for transaction confirmation
    Wait {
        /// Transaction hash
        tx_hash: String,

        /// Timeout in seconds
        #[arg(long, default_value = "300")]
        timeout: u64,
    },

    /// Sign a transaction
    Sign {
        /// Path to raw transaction file
        tx_file: String,

        /// Output file for signed transaction
        #[arg(short, long)]
        output: Option<String>,
    },

    /// Decode and display transaction contents
    Decode {
        /// Path to transaction file or CBOR hex
        input: String,
    },

    /// Build a simple payment transaction
    BuildPayment {
        /// Recipient address
        #[arg(long)]
        to: String,

        /// Amount in lovelace
        #[arg(long)]
        amount: u64,

        /// Output file
        #[arg(short, long)]
        output: Option<String>,
    },
}

pub async fn execute(ctx: &CliContext, args: TxArgs) -> Result<()> {
    match args.command {
        TxCommands::Submit {
            tx_file,
            wait,
            timeout,
        } => submit(ctx, &tx_file, wait, timeout).await,
        TxCommands::Status { tx_hash } => status(ctx, &tx_hash).await,
        TxCommands::Wait { tx_hash, timeout } => wait_for_tx(ctx, &tx_hash, timeout).await,
        TxCommands::Sign { tx_file, output } => sign(ctx, &tx_file, output).await,
        TxCommands::Decode { input } => decode(&input).await,
        TxCommands::BuildPayment { to, amount, output } => {
            build_payment(ctx, &to, amount, output).await
        }
    }
}

async fn submit(ctx: &CliContext, tx_file: &str, wait: bool, timeout: u64) -> Result<()> {
    println!("{}", format!("Submitting transaction from {}...", tx_file).cyan());

    // Read transaction file
    let content = std::fs::read_to_string(tx_file)
        .with_context(|| format!("Failed to read {}", tx_file))?;

    // Parse as JSON envelope
    let json: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| "Failed to parse transaction file as JSON")?;

    let cbor_hex = json
        .get("cborHex")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing 'cborHex' field in transaction file"))?;

    let cbor = hex::decode(cbor_hex)
        .with_context(|| "Failed to decode CBOR hex")?;

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    // Submit
    println!("Submitting {} bytes...", cbor.len());
    let tx_hash = client.submit_tx(&cbor).await?;

    println!("\n{}", "Transaction submitted!".green().bold());
    println!("  TX Hash: {}", tx_hash);
    println!("  Explorer: {}", ctx.explorer_tx_url(&tx_hash));

    if wait {
        println!("\nWaiting for confirmation...");
        let info = client.wait_for_tx(&tx_hash, timeout).await?;
        println!("\n{}", "Transaction confirmed!".green().bold());
        println!("  Block: {}", info.block);
        println!("  Slot: {}", info.slot);
        println!("  Fee: {} lovelace", info.fees);
    }

    Ok(())
}

async fn status(ctx: &CliContext, tx_hash: &str) -> Result<()> {
    println!("{}", format!("Checking transaction status: {}", tx_hash).cyan());

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    match client.get_tx(tx_hash).await {
        Ok(info) => {
            println!("\n{}", "Transaction CONFIRMED:".green().bold());
            println!("  Block: {}", info.block);
            println!("  Block Height: {}", info.block_height);
            println!("  Slot: {}", info.slot);
            println!("  Index: {}", info.index);
            println!("  Fee: {} lovelace", info.fees);
            println!("  Size: {} bytes", info.size);
            println!("\n  Explorer: {}", ctx.explorer_tx_url(tx_hash));
        }
        Err(e) => {
            if e.to_string().contains("404") {
                println!("\n{}", "Transaction PENDING or NOT FOUND".yellow());
                println!("The transaction may still be propagating or has not been submitted.");
            } else {
                return Err(e);
            }
        }
    }

    Ok(())
}

async fn wait_for_tx(ctx: &CliContext, tx_hash: &str, timeout: u64) -> Result<()> {
    println!(
        "{}",
        format!("Waiting for transaction {}...", tx_hash).cyan()
    );

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let progress = indicatif::ProgressBar::new_spinner();
    progress.set_message("Waiting for confirmation...");
    progress.enable_steady_tick(std::time::Duration::from_millis(100));

    let info = client.wait_for_tx(tx_hash, timeout).await?;

    progress.finish_with_message("Confirmed!");

    println!("\n{}", "Transaction confirmed!".green().bold());
    println!("  Block: {}", info.block);
    println!("  Slot: {}", info.slot);
    println!("  Fee: {} lovelace", info.fees);

    Ok(())
}

async fn sign(ctx: &CliContext, tx_file: &str, output: Option<String>) -> Result<()> {
    println!("{}", format!("Signing transaction from {}...", tx_file).cyan());

    let content = std::fs::read_to_string(tx_file)?;
    let json: serde_json::Value = serde_json::from_str(&content)?;

    let cbor_hex = json
        .get("cborHex")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing 'cborHex' field"))?;

    let _keypair = ctx.load_signing_key()?;

    // For now, provide instructions for cardano-cli signing
    // Full Rust signing requires implementing the transaction format
    println!("\n{}", "Manual Signing Required:".yellow().bold());
    println!("Use cardano-cli to sign the transaction:");
    println!("\ncardano-cli conway transaction sign \\");
    println!("  --testnet-magic {} \\", ctx.network_magic());
    println!("  --tx-body-file {} \\", tx_file);
    println!(
        "  --signing-key-file <signing-key> \\",
    );
    println!(
        "  --out-file {}",
        output.unwrap_or_else(|| tx_file.replace(".raw", ".signed"))
    );

    println!("\nTransaction CBOR (first 100 chars): {}...", &cbor_hex[..100.min(cbor_hex.len())]);

    Ok(())
}

async fn decode(input: &str) -> Result<()> {
    println!("{}", "Decoding transaction...".cyan());

    let cbor_hex = if std::path::Path::new(input).exists() {
        // Read from file
        let content = std::fs::read_to_string(input)?;
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
            json.get("cborHex")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .ok_or_else(|| anyhow!("Missing 'cborHex' field"))?
        } else {
            content.trim().to_string()
        }
    } else {
        // Assume it's CBOR hex directly
        input.to_string()
    };

    let cbor = hex::decode(&cbor_hex)?;

    println!("\n{}", "Transaction CBOR:".green());
    println!("  Length: {} bytes", cbor.len());
    println!("  Hex (first 200 chars): {}...", &cbor_hex[..200.min(cbor_hex.len())]);

    // Basic CBOR structure analysis
    println!("\n{}", "Structure:".green());
    if cbor.len() > 2 {
        let first_byte = cbor[0];
        let major_type = first_byte >> 5;
        let info = first_byte & 0x1f;

        let type_name = match major_type {
            0 => "unsigned integer",
            1 => "negative integer",
            2 => "byte string",
            3 => "text string",
            4 => "array",
            5 => "map",
            6 => "tag",
            7 => "simple/float",
            _ => "unknown",
        };

        println!("  First byte: 0x{:02x}", first_byte);
        println!("  Major type: {} ({})", major_type, type_name);
        println!("  Additional info: {}", info);
    }

    println!("\n{}", "Note:".yellow());
    println!("For full transaction decoding, use cardano-cli transaction view");

    Ok(())
}

async fn build_payment(
    ctx: &CliContext,
    to: &str,
    amount: u64,
    output: Option<String>,
) -> Result<()> {
    println!("{}", "Building payment transaction...".cyan());
    println!("  To: {}", to);
    println!("  Amount: {} lovelace ({:.6} ADA)", amount, amount as f64 / 1_000_000.0);

    let keypair = ctx.load_signing_key()?;
    let from = keypair.address_bech32(ctx.pallas_network());

    println!("  From: {}", from);

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    // Find suitable UTXO
    let utxos = client.get_utxos(&from).await?;
    let suitable = utxos
        .iter()
        .find(|u| u.lovelace >= amount + 2_000_000 && u.assets.is_empty())
        .ok_or_else(|| anyhow!("No suitable UTXO found (need >= {} lovelace)", amount + 2_000_000))?;

    println!("  Input UTXO: {}#{}", suitable.tx_hash, suitable.output_index);

    let output_file = output.unwrap_or_else(|| "payment.raw".to_string());

    println!("\n{}", "Manual Transaction Build Required:".yellow().bold());
    println!("Use cardano-cli to build the transaction:");
    println!("\ncardano-cli conway transaction build \\");
    println!("  --testnet-magic {} \\", ctx.network_magic());
    println!("  --tx-in {}#{} \\", suitable.tx_hash, suitable.output_index);
    println!("  --tx-out {}+{} \\", to, amount);
    println!("  --change-address {} \\", from);
    println!("  --out-file {}", output_file);

    Ok(())
}

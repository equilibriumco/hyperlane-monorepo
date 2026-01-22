//! Token command - Manage test tokens for development
//!
//! This module provides commands for deploying and minting test tokens
//! for testing warp routes and other token-related functionality.

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};
use colored::Colorize;

use crate::utils::blockfrost::BlockfrostClient;
use crate::utils::context::CliContext;
use crate::utils::plutus::{apply_validator_param, encode_output_reference};
use crate::utils::tx_builder::HyperlaneTxBuilder;

#[derive(Args)]
pub struct TokenArgs {
    #[command(subcommand)]
    command: TokenCommands,
}

#[derive(Subcommand)]
enum TokenCommands {
    /// Deploy a test token minting policy and mint tokens
    ///
    /// This creates a one-shot minting policy tied to a UTXO, ensuring
    /// only the deployer can mint tokens and only once.
    Deploy {
        /// Asset name for the token (e.g., "TEST", "WARP")
        #[arg(long, default_value = "TEST")]
        name: String,

        /// Amount of tokens to mint
        #[arg(long, default_value = "1000000")]
        amount: u64,

        /// UTXO to use for minting (tx_hash#index)
        /// If not specified, will find a suitable UTXO automatically
        #[arg(long)]
        utxo: Option<String>,

        /// Dry run - show what would be done without submitting
        #[arg(long)]
        dry_run: bool,
    },

    /// Show test token deployment info
    Info {
        /// Policy ID to look up
        #[arg(long)]
        policy_id: Option<String>,
    },
}

pub async fn execute(ctx: &CliContext, args: TokenArgs) -> Result<()> {
    match args.command {
        TokenCommands::Deploy {
            name,
            amount,
            utxo,
            dry_run,
        } => deploy_token(ctx, &name, amount, utxo, dry_run).await,
        TokenCommands::Info { policy_id } => show_info(ctx, policy_id).await,
    }
}

/// Deploy a test token minting policy and mint tokens
async fn deploy_token(
    ctx: &CliContext,
    name: &str,
    amount: u64,
    utxo: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Deploying test token...".cyan());
    println!("  Token Name: {}", name);
    println!("  Amount: {}", amount);

    let api_key = ctx.require_api_key()?;
    let keypair = ctx.load_signing_key()?;
    let payer_address = keypair.address_bech32(ctx.pallas_network());

    println!("  Deployer: {}", payer_address);

    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    // Get UTXOs
    let utxos = client.get_utxos(&payer_address).await?;
    println!("  Found {} UTXOs at wallet", utxos.len());

    // Calculate minimum required lovelace (token output + fees)
    let output_lovelace = 2_000_000u64; // 2 ADA for token output
    let min_required = output_lovelace + 5_000_000; // +5 ADA for fees

    // Find input UTXO
    let input_utxo = match &utxo {
        Some(u) => {
            let utxo_ref = crate::utils::types::UtxoRef::parse(u)
                .ok_or_else(|| anyhow!("Invalid UTXO format. Use tx_hash#index"))?;
            utxos
                .iter()
                .find(|u| u.tx_hash == utxo_ref.tx_hash && u.output_index == utxo_ref.output_index)
                .cloned()
                .ok_or_else(|| anyhow!("UTXO not found"))?
        }
        None => {
            utxos
                .iter()
                .find(|u| u.lovelace >= min_required && u.assets.is_empty())
                .cloned()
                .ok_or_else(|| anyhow!("No suitable UTXO found (need >= {} ADA without assets)", min_required / 1_000_000))?
        }
    };

    // Find collateral UTXO
    let collateral_utxo = utxos
        .iter()
        .find(|u| {
            u.lovelace >= 5_000_000
                && u.assets.is_empty()
                && !(u.tx_hash == input_utxo.tx_hash && u.output_index == input_utxo.output_index)
        })
        .cloned()
        .ok_or_else(|| anyhow!("No suitable collateral UTXO found (need a second UTXO with >= 5 ADA)"))?;

    println!("  Input UTXO: {}#{} ({} ADA)", input_utxo.tx_hash, input_utxo.output_index, input_utxo.lovelace / 1_000_000);
    println!("  Collateral: {}#{}", collateral_utxo.tx_hash, collateral_utxo.output_index);

    // Encode output reference for test_token parameter
    let output_ref_cbor = encode_output_reference(&input_utxo.tx_hash, input_utxo.output_index)?;
    let output_ref_hex = hex::encode(&output_ref_cbor);
    println!("  OutputRef CBOR: {}", output_ref_hex.yellow());

    // Apply parameter to test_token minting policy
    println!("\n{}", "Applying test_token parameter...".cyan());
    let applied = apply_validator_param(&ctx.contracts_dir, "test_token", "test_token", &output_ref_hex)?;
    println!("  Test Token Policy ID: {}", applied.policy_id.green());

    // Asset name bytes
    let asset_name_hex = hex::encode(name.as_bytes());
    println!("  Asset Name (hex): {}", asset_name_hex);

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        println!("\nTransaction would:");
        println!("  - Spend UTXO {}#{}", input_utxo.tx_hash, input_utxo.output_index);
        println!("  - Mint {} tokens with policy {}", amount, applied.policy_id);
        println!("  - Send tokens to {}", payer_address);
        println!("\nTo bridge these tokens, use:");
        println!("  hyperlane-cardano warp deploy \\");
        println!("    --policy-id {} \\", applied.policy_id);
        println!("    --asset-name {} \\", name);
        println!("    --type collateral");
        return Ok(());
    }

    // Build and submit transaction
    println!("\n{}", "Building transaction...".cyan());
    let mint_script_cbor = hex::decode(&applied.compiled_code)
        .with_context(|| "Invalid script CBOR")?;

    let tx_builder = HyperlaneTxBuilder::new(&client, ctx.pallas_network());
    let built_tx = tx_builder
        .build_mint_token_tx(
            &keypair,
            &input_utxo,
            &collateral_utxo,
            &mint_script_cbor,
            name,
            amount,
            output_lovelace,
        )
        .await?;

    println!("  TX Hash: {}", hex::encode(&built_tx.tx_hash.0));

    // Sign transaction
    println!("{}", "Signing transaction...".cyan());
    let signed_tx = tx_builder.sign_tx(built_tx, &keypair)?;
    println!("  Signed TX size: {} bytes", signed_tx.len());

    // Submit transaction
    println!("{}", "Submitting transaction...".cyan());
    let tx_hash = client.submit_tx(&signed_tx).await?;
    println!("\n{}", "✓ Transaction submitted!".green().bold());
    println!("  TX Hash: {}", tx_hash);
    println!("  Explorer: {}", ctx.explorer_tx_url(&tx_hash));

    // Save token info to deployment file
    let token_info_path = ctx.network_deployments_dir().join("test_token.json");
    let token_info = serde_json::json!({
        "policy_id": applied.policy_id,
        "asset_name": name,
        "asset_name_hex": asset_name_hex,
        "amount": amount,
        "mint_tx_hash": tx_hash,
        "seed_utxo": format!("{}#{}", input_utxo.tx_hash, input_utxo.output_index),
    });
    std::fs::write(&token_info_path, serde_json::to_string_pretty(&token_info)?)?;

    println!("\n{}", "═══════════════════════════════════════════════════════════════".green());
    println!("{}", "Test Token Deployment Summary".green().bold());
    println!("{}", "═══════════════════════════════════════════════════════════════".green());
    println!();
    println!("{}", "Token Info:".cyan());
    println!("  Policy ID: {}", applied.policy_id.green());
    println!("  Asset Name: {} ({})", name, asset_name_hex);
    println!("  Amount Minted: {}", amount);
    println!("  Recipient: {}", payer_address);
    println!();
    println!("{}", "Saved to:".cyan());
    println!("  {:?}", token_info_path);
    println!();
    println!("{}", "═══════════════════════════════════════════════════════════════".green());
    println!("{}", "To deploy a warp route for this token, run:".yellow());
    println!("{}", "═══════════════════════════════════════════════════════════════".green());
    println!();
    println!("  hyperlane-cardano warp deploy \\");
    println!("    --policy-id {} \\", applied.policy_id);
    println!("    --asset-name {} \\", name);
    println!("    --type collateral");
    println!();

    Ok(())
}

async fn show_info(ctx: &CliContext, policy_id: Option<String>) -> Result<()> {
    // Try to load test token info from deployment file
    let token_info_path = ctx.network_deployments_dir().join("test_token.json");

    if token_info_path.exists() {
        let content = std::fs::read_to_string(&token_info_path)?;
        let info: serde_json::Value = serde_json::from_str(&content)?;

        println!("{}", "Test Token Info:".cyan());
        if let Some(policy) = info.get("policy_id").and_then(|v| v.as_str()) {
            println!("  Policy ID: {}", policy.green());
        }
        if let Some(name) = info.get("asset_name").and_then(|v| v.as_str()) {
            println!("  Asset Name: {}", name);
        }
        if let Some(amount) = info.get("amount").and_then(|v| v.as_u64()) {
            println!("  Amount Minted: {}", amount);
        }
        if let Some(tx_hash) = info.get("mint_tx_hash").and_then(|v| v.as_str()) {
            println!("  Mint TX: {}", tx_hash);
            println!("  Explorer: {}", ctx.explorer_tx_url(tx_hash));
        }
    } else {
        println!("{}", "No test token deployed yet.".yellow());
        println!("Run 'hyperlane-cardano token deploy' to create a test token.");
    }

    // If a policy ID was provided, try to look it up
    if let Some(policy) = policy_id {
        let api_key = ctx.require_api_key()?;
        let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);
        let keypair = ctx.load_signing_key()?;
        let address = keypair.address_bech32(ctx.pallas_network());

        println!("\n{}", "Looking up token balance...".cyan());
        let utxos = client.get_utxos(&address).await?;

        let mut found = false;
        for utxo in &utxos {
            for asset in &utxo.assets {
                if asset.policy_id == policy {
                    println!("  UTXO: {}#{}", utxo.tx_hash, utxo.output_index);
                    println!("  Asset: {}", asset.asset_name);
                    println!("  Quantity: {}", asset.quantity);
                    found = true;
                }
            }
        }

        if !found {
            println!("  No tokens found with policy ID {}", policy);
        }
    }

    Ok(())
}

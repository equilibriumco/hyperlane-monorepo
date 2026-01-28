//! Warp command - Manage warp routes (token bridges)

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use colored::Colorize;

use crate::utils::blockfrost::BlockfrostClient;
use crate::utils::cbor::{build_warp_route_collateral_datum, build_warp_route_native_datum};
use crate::utils::context::CliContext;
use crate::utils::crypto::Keypair;
use crate::utils::plutus::{
    apply_validator_param, apply_validator_params, encode_output_reference,
    encode_script_hash_param, AppliedValidator,
};
use crate::utils::tx_builder::HyperlaneTxBuilder;
use crate::utils::types::{ReferenceScriptUtxo, Utxo, WarpRouteDeployment};

#[derive(Args)]
pub struct WarpArgs {
    #[command(subcommand)]
    command: WarpCommands,
}

#[derive(Subcommand)]
enum WarpCommands {
    /// Deploy a new warp route
    Deploy {
        /// Token type
        #[arg(long, value_enum)]
        token_type: TokenType,

        /// Token policy ID (for collateral type)
        #[arg(long)]
        token_policy: Option<String>,

        /// Token asset name (for collateral type)
        #[arg(long)]
        token_asset: Option<String>,

        /// Local token decimals (Cardano side, e.g., 6 for ADA)
        #[arg(long)]
        decimals: u8,

        /// Remote token decimals (wire format, e.g., 18 for EVM chains)
        #[arg(long)]
        remote_decimals: u8,

        /// Dry run
        #[arg(long)]
        dry_run: bool,
    },

    /// Show warp route configuration
    Show {
        /// Warp route policy ID
        #[arg(long)]
        warp_policy: Option<String>,
    },
}

#[derive(Clone, ValueEnum)]
enum TokenType {
    /// Native ADA
    Native,
    /// Collateral (lock existing tokens)
    Collateral,
}

pub async fn execute(ctx: &CliContext, args: WarpArgs) -> Result<()> {
    match args.command {
        WarpCommands::Deploy {
            token_type,
            token_policy,
            token_asset,
            decimals,
            remote_decimals,
            dry_run,
        } => {
            deploy(
                ctx,
                token_type,
                token_policy,
                token_asset,
                decimals,
                remote_decimals,
                dry_run,
            )
            .await
        }
        WarpCommands::Show { warp_policy } => show(ctx, warp_policy).await,
    }
}
async fn deploy(
    ctx: &CliContext,
    token_type: TokenType,
    token_policy: Option<String>,
    token_asset: Option<String>,
    decimals: u8,
    remote_decimals: u8,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Deploying warp route...".cyan());

    let type_str = match token_type {
        TokenType::Native => "Native (ADA)",
        TokenType::Collateral => "Collateral (Lock existing tokens)",
    };

    println!("  Token Type: {}", type_str);
    println!("  Local Decimals: {}", decimals);
    println!("  Remote Decimals: {}", remote_decimals);

    match token_type {
        TokenType::Collateral => {
            let policy = token_policy
                .ok_or_else(|| anyhow!("--token-policy required for collateral type"))?;
            let asset = token_asset.unwrap_or_default();
            deploy_collateral_route(ctx, &policy, &asset, decimals, remote_decimals, dry_run).await
        }
        TokenType::Native => deploy_native_route(ctx, decimals, remote_decimals, dry_run).await,
    }
}

/// Context holding shared state prepared for warp route deployment
struct WarpDeploymentContext {
    client: BlockfrostClient,
    keypair: Keypair,
    owner_pkh: String,
    warp_input: Utxo,
    warp_collateral: Utxo,
    warp_nft_applied: AppliedValidator,
    warp_route_applied: AppliedValidator,
    warp_address: String,
}

/// Prepare shared warp route deployment context
///
/// This handles all the common setup steps:
/// 1. Load deployment info and get mailbox_policy_id
/// 2. Load API and signing key
/// 3. Find suitable UTXOs
/// 4. Compute state_nft policy
/// 5. Compute warp_route script hash
/// 6. Calculate warp route address
async fn prepare_warp_deployment(ctx: &CliContext) -> Result<WarpDeploymentContext> {
    // Load deployment info to get mailbox_policy_id
    let deployment = ctx.load_deployment_info()?;
    let mailbox_policy_id = deployment
        .mailbox
        .as_ref()
        .and_then(|m| m.state_nft_policy.as_ref())
        .ok_or_else(|| anyhow!("Mailbox not deployed. Run 'hyperlane-cardano init' first"))?;

    // Load API and signing key
    let api_key = ctx.require_api_key()?;
    let keypair = ctx.load_signing_key()?;
    let owner_pkh = keypair.verification_key_hash_hex();
    let payer_address = keypair.address_bech32(ctx.pallas_network());

    println!("\n{}", "Step 1: Finding UTXOs...".cyan());
    println!("  Owner: {}", payer_address);

    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);
    let utxos = client.get_utxos(&payer_address).await?;

    println!("  Found {} UTXOs", utxos.len());

    // We need 2 UTXOs for warp route deployment (state + collateral)
    let min_ada = 25_000_000u64;
    let suitable_utxos: Vec<_> = utxos
        .into_iter()
        .filter(|u| u.lovelace >= min_ada && u.assets.is_empty())
        .collect();

    if suitable_utxos.len() < 2 {
        let large_utxos: Vec<_> = suitable_utxos
            .iter()
            .filter(|u| u.lovelace >= 100_000_000 && u.assets.is_empty())
            .collect();

        if !large_utxos.is_empty() {
            let large = large_utxos[0];
            return Err(anyhow!(
                "Need at least 2 UTXOs with >= 25 ADA each. Found {}.\n\
                You have a large UTXO ({}#{}) with {} ADA that can be split.\n\
                Run: hyperlane-cardano utxo split --utxo '{}#{}' --count 2 --amount 50000000",
                suitable_utxos.len(),
                large.tx_hash,
                large.output_index,
                large.lovelace / 1_000_000,
                large.tx_hash,
                large.output_index
            ));
        }

        return Err(anyhow!(
            "Need at least 2 UTXOs with >= 25 ADA each. Found {}. \
            Please fund the wallet with more ADA.",
            suitable_utxos.len()
        ));
    }

    let mut suitable_utxos_iter = suitable_utxos.into_iter();

    // SAFETY: Already checked length above.
    let warp_input = suitable_utxos_iter.next().unwrap();
    let warp_collateral = suitable_utxos_iter.next().unwrap();

    println!(
        "  Warp Route Input: {}#{}",
        warp_input.tx_hash, warp_input.output_index
    );

    // Step 2: Compute state_nft policy
    println!("\n{}", "Step 2: Computing script hashes...".cyan());
    println!("  Mailbox Policy ID: {}", mailbox_policy_id);

    let warp_nft_output_ref =
        encode_output_reference(&warp_input.tx_hash, warp_input.output_index)?;
    let warp_nft_applied = apply_validator_param(
        &ctx.contracts_dir,
        "state_nft",
        "state_nft",
        &hex::encode(&warp_nft_output_ref),
    )?;
    println!("  State NFT Policy: {}", warp_nft_applied.policy_id.green());

    // Compute warp_route script hash
    let mailbox_param_cbor = encode_script_hash_param(mailbox_policy_id)?;
    let state_nft_param_cbor = encode_script_hash_param(&warp_nft_applied.policy_id)?;
    let warp_route_applied = apply_validator_params(
        &ctx.contracts_dir,
        "warp_route",
        "warp_route",
        &[
            &hex::encode(&mailbox_param_cbor),
            &hex::encode(&state_nft_param_cbor),
        ],
    )?;
    println!(
        "  Warp Route Script Hash: {}",
        warp_route_applied.policy_id.green()
    );

    let warp_route_script = hex::decode(&warp_route_applied.compiled_code)
        .with_context(|| "Invalid warp route script CBOR")?;
    println!(
        "  Warp Route Script Size: {} bytes",
        warp_route_script.len()
    );

    // Calculate warp route script address
    let warp_address = ctx.script_address(&warp_route_applied.policy_id)?;
    println!("  Warp Route Address: {}", warp_address);

    Ok(WarpDeploymentContext {
        client,
        keypair,
        owner_pkh,
        warp_input,
        warp_collateral,
        warp_nft_applied,
        warp_route_applied,
        warp_address,
    })
}

/// Finalize warp route deployment by building and submitting the transaction
///
/// This handles the common deployment steps:
/// 1. Build and submit the deployment transaction
/// 2. Save deployment info to JSON file
/// 3. Update deployment_info.json
async fn finalize_warp_deployment(
    ctx: &CliContext,
    deploy_ctx: &WarpDeploymentContext,
    warp_datum: &[u8],
    warp_type: &str,
    extra_info: serde_json::Value,
) -> Result<String> {
    println!(
        "\n{}",
        "Step 4: Deploying warp route with reference script...".cyan()
    );

    let warp_nft_script = hex::decode(&deploy_ctx.warp_nft_applied.compiled_code)
        .with_context(|| "Invalid warp NFT script CBOR")?;
    let warp_route_script = hex::decode(&deploy_ctx.warp_route_applied.compiled_code)
        .with_context(|| "Invalid warp route script CBOR")?;

    let tx_builder = HyperlaneTxBuilder::new(&deploy_ctx.client, ctx.pallas_network());
    let warp_tx = tx_builder
        .build_init_recipient_two_utxo_tx(
            &deploy_ctx.keypair,
            &deploy_ctx.warp_input,
            &deploy_ctx.warp_collateral,
            &warp_nft_script,
            &warp_route_script,
            &deploy_ctx.warp_address,
            warp_datum,
            5_000_000,
            18_000_000,
        )
        .await?;

    println!("  TX Hash: {}", hex::encode(&warp_tx.tx_hash.0));

    let warp_signed = tx_builder.sign_tx(warp_tx, &deploy_ctx.keypair)?;
    println!("  Submitting warp route transaction...");
    let warp_tx_hash = deploy_ctx.client.submit_tx(&warp_signed).await?;
    println!("  ✓ Warp route deployed: {}", warp_tx_hash.green());

    // Save deployment info
    let warp_ref_script_utxo = format!("{}#1", warp_tx_hash);
    let info_filename = format!("{}_warp_route.json", warp_type);
    let warp_info_path = ctx.network_deployments_dir().join(&info_filename);

    let mut warp_info = serde_json::json!({
        "type": warp_type,
        "warp_route": {
            "script_hash": deploy_ctx.warp_route_applied.policy_id,
            "nft_policy": deploy_ctx.warp_nft_applied.policy_id,
            "address": deploy_ctx.warp_address,
            "tx_hash": warp_tx_hash,
            "reference_script_utxo": warp_ref_script_utxo,
        },
        "owner": deploy_ctx.owner_pkh,
    });

    // Merge extra info
    if let (Some(base), Some(extra)) = (warp_info.as_object_mut(), extra_info.as_object()) {
        for (k, v) in extra {
            base.insert(k.clone(), v.clone());
        }
    }

    std::fs::write(&warp_info_path, serde_json::to_string_pretty(&warp_info)?)?;

    // Update deployment_info.json
    if let Ok(mut deployment) = ctx.load_deployment_info() {
        let warp_deployment = WarpRouteDeployment {
            warp_type: warp_type.to_string(),
            decimals: extra_info
                .get("decimals")
                .and_then(|v| v.as_u64())
                .unwrap_or(6) as u32,
            owner: deploy_ctx.owner_pkh.clone(),
            script_hash: deploy_ctx.warp_route_applied.policy_id.clone(),
            address: deploy_ctx.warp_address.clone(),
            nft_policy: deploy_ctx.warp_nft_applied.policy_id.clone(),
            init_tx_hash: Some(warp_tx_hash.clone()),
            reference_script_utxo: Some(ReferenceScriptUtxo {
                tx_hash: warp_tx_hash.clone(),
                output_index: 1,
                lovelace: 18_000_000,
            }),
            token_policy: extra_info
                .get("token_policy")
                .and_then(|v| v.as_str())
                .map(String::from),
            token_asset: extra_info
                .get("token_asset")
                .and_then(|v| v.as_str())
                .map(String::from),
            minting_policy: None,
            minting_ref_script_utxo: None,
        };
        deployment.warp_routes.push(warp_deployment);

        if let Err(e) = ctx.save_deployment_info(&deployment) {
            println!("  Warning: Failed to update deployment_info.json: {}", e);
        }
    }

    Ok(warp_tx_hash)
}

/// Deploy a Collateral warp route
///
/// Collateral warp routes hold tokens directly in the warp route UTXO.
async fn deploy_collateral_route(
    ctx: &CliContext,
    token_policy: &str,
    token_asset: &str,
    decimals: u8,
    remote_decimals: u8,
    dry_run: bool,
) -> Result<()> {
    println!(
        "\n{}",
        "═══════════════════════════════════════════════════════════════".cyan()
    );
    println!("{}", "Deploying Collateral Warp Route".cyan().bold());
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════".cyan()
    );

    println!("\n{}", "Token Configuration:".green());
    println!("  Policy ID: {}", token_policy);
    println!(
        "  Asset Name: {}",
        if token_asset.is_empty() {
            "(empty)"
        } else {
            token_asset
        }
    );
    println!("  Local Decimals: {}", decimals);
    println!("  Remote Decimals: {}", remote_decimals);

    // Prepare deployment context
    let deploy_ctx = prepare_warp_deployment(ctx).await?;

    // Build warp route datum
    println!("\n{}", "Step 3: Preparing warp route deployment...".cyan());
    let token_asset_hex = hex::encode(token_asset.as_bytes());
    let warp_datum = build_warp_route_collateral_datum(
        token_policy,
        &token_asset_hex,
        decimals as u32,
        remote_decimals as u32,
        &deploy_ctx.owner_pkh,
    )?;
    println!("  Warp Route Datum CBOR: {} bytes", warp_datum.len());

    if dry_run {
        println!(
            "\n{}",
            "═══════════════════════════════════════════════════════════════".yellow()
        );
        println!("{}", "[Dry run - not submitting transactions]".yellow());
        println!(
            "{}",
            "═══════════════════════════════════════════════════════════════".yellow()
        );
        println!("\nDeployment would create:");
        println!(
            "  1. Warp Route State UTXO at {} with NFT {}",
            deploy_ctx.warp_address, deploy_ctx.warp_nft_applied.policy_id
        );
        println!("  2. Warp Route Reference Script UTXO");
        return Ok(());
    }

    // Finalize deployment
    let extra_info = serde_json::json!({
        "token_policy": token_policy,
        "token_asset": token_asset,
        "decimals": decimals,
    });
    let warp_tx_hash =
        finalize_warp_deployment(ctx, &deploy_ctx, &warp_datum, "collateral", extra_info).await?;

    // Print summary
    print_deployment_summary(
        &deploy_ctx.warp_route_applied.policy_id,
        &deploy_ctx.warp_nft_applied.policy_id,
        &deploy_ctx.warp_address,
        &warp_tx_hash,
        Some(token_policy),
        Some(token_asset),
    );

    Ok(())
}

/// Deploy a Native (ADA) warp route
///
/// Native warp routes hold ADA directly in the warp route UTXO.
async fn deploy_native_route(
    ctx: &CliContext,
    decimals: u8,
    remote_decimals: u8,
    dry_run: bool,
) -> Result<()> {
    println!(
        "\n{}",
        "═══════════════════════════════════════════════════════════════".cyan()
    );
    println!("{}", "Deploying Native (ADA) Warp Route".cyan().bold());
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════".cyan()
    );

    println!("\n{}", "Configuration:".green());
    println!("  Token: ADA (Native)");
    println!("  Local Decimals: {}", decimals);
    println!("  Remote Decimals: {}", remote_decimals);

    // Prepare deployment context
    let deploy_ctx = prepare_warp_deployment(ctx).await?;

    // Build warp route datum
    println!("\n{}", "Step 3: Preparing warp route deployment...".cyan());
    let warp_datum = build_warp_route_native_datum(
        decimals as u32,
        remote_decimals as u32,
        &deploy_ctx.owner_pkh,
    )?;
    println!("  Warp Route Datum CBOR: {} bytes", warp_datum.len());

    if dry_run {
        println!(
            "\n{}",
            "═══════════════════════════════════════════════════════════════".yellow()
        );
        println!("{}", "[Dry run - not submitting transactions]".yellow());
        println!(
            "{}",
            "═══════════════════════════════════════════════════════════════".yellow()
        );
        println!("\nDeployment would create:");
        println!(
            "  1. Warp Route State UTXO at {} with NFT {}",
            deploy_ctx.warp_address, deploy_ctx.warp_nft_applied.policy_id
        );
        println!("  2. Warp Route Reference Script UTXO");
        return Ok(());
    }

    // Finalize deployment
    let extra_info = serde_json::json!({
        "decimals": decimals,
    });
    let warp_tx_hash =
        finalize_warp_deployment(ctx, &deploy_ctx, &warp_datum, "native", extra_info).await?;

    // Print summary
    print_deployment_summary(
        &deploy_ctx.warp_route_applied.policy_id,
        &deploy_ctx.warp_nft_applied.policy_id,
        &deploy_ctx.warp_address,
        &warp_tx_hash,
        None,
        None,
    );

    Ok(())
}

/// Print deployment summary with next steps
fn print_deployment_summary(
    script_hash: &str,
    nft_policy: &str,
    address: &str,
    tx_hash: &str,
    token_policy: Option<&str>,
    token_asset: Option<&str>,
) {
    let ref_script_utxo = format!("{}#1", tx_hash);

    println!(
        "\n{}",
        "═══════════════════════════════════════════════════════════════".green()
    );
    println!("{}", "Warp Route Deployment Complete!".green().bold());
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════".green()
    );
    println!();
    println!("{}", "Warp Route:".cyan());
    println!("  Script Hash: {}", script_hash);
    println!("  NFT Policy: {}", nft_policy);
    println!("  Address: {}", address);
    println!("  TX: {}", tx_hash);
    println!("  State UTXO: {}#0", tx_hash);
    println!("  Reference Script UTXO: {}", ref_script_utxo);

    if let (Some(policy), Some(asset)) = (token_policy, token_asset) {
        println!();
        println!("{}", "Token:".cyan());
        println!("  Policy ID: {}", policy);
        println!("  Asset Name: {}", asset);
    }

    println!();
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════".green()
    );
    println!("{}", "Next steps:".yellow());
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════".green()
    );
    println!();
    println!("1. Enroll remote routers:");
    println!("   hyperlane-cardano warp enroll-router --domain <DOMAIN> --router <ADDRESS>");
    println!();
    println!("2. Transfer tokens:");
    println!("   hyperlane-cardano warp transfer --domain <DOMAIN> --recipient <ADDRESS> --amount <AMOUNT>");
    println!();
}

async fn show(ctx: &CliContext, warp_policy: Option<String>) -> Result<()> {
    println!("{}", "Warp Route Configuration".cyan());

    let policy_id = get_warp_policy(ctx, warp_policy)?;
    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let warp_utxo = client
        .find_utxo_by_asset(&policy_id, "")
        .await?
        .ok_or_else(|| anyhow!("Warp route UTXO not found with policy {}", policy_id))?;

    println!("\n{}", "Warp Route UTXO:".green());
    println!("  TX: {}#{}", warp_utxo.tx_hash, warp_utxo.output_index);
    println!("  Address: {}", warp_utxo.address);
    println!("  Lovelace: {}", warp_utxo.lovelace);

    if let Some(datum) = &warp_utxo.inline_datum {
        println!("\n{}", "Configuration:".green());
        println!("{}", serde_json::to_string_pretty(datum)?);
    }

    Ok(())
}

// Helper functions

fn get_warp_policy(ctx: &CliContext, warp_policy: Option<String>) -> Result<String> {
    if let Some(p) = warp_policy {
        return Ok(p);
    }

    let deployment = ctx.load_deployment_info()?;
    deployment
        .warp_route
        .and_then(|w| w.state_nft_policy)
        .ok_or_else(|| {
            anyhow!("Warp policy not found. Use --warp-policy or update deployment_info.json")
        })
}

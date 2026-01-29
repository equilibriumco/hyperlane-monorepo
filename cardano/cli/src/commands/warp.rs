//! Warp command - Manage warp routes (token bridges)

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use colored::Colorize;
use pallas_primitives::conway::{PlutusData, BigInt};
use sha3::{Digest, Keccak256};

use crate::utils::blockfrost::BlockfrostClient;
use crate::utils::cbor::{
    build_enroll_remote_route_redeemer, build_mailbox_datum, build_mailbox_dispatch_redeemer,
    build_mint_redeemer, build_transfer_remote_redeemer, build_warp_route_collateral_datum,
    build_warp_route_collateral_datum_with_routes, build_warp_route_native_datum,
    build_warp_route_native_datum_with_routes, build_warp_route_synthetic_datum,
    build_warp_route_synthetic_datum_with_routes, decode_plutus_datum, RemoteRoute,
};
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

        /// Local token decimals (Cardano side). Required for collateral/synthetic.
        /// Ignored for native type (ADA is always 6 decimals).
        #[arg(long)]
        decimals: Option<u8>,

        /// Remote token decimals (wire format, e.g., 18 for EVM chains)
        #[arg(long)]
        remote_decimals: u8,

        /// Dry run
        #[arg(long)]
        dry_run: bool,
    },

    /// Enroll a remote router
    EnrollRouter {
        /// Destination domain ID
        #[arg(long)]
        domain: u32,

        /// Remote router address (32 bytes hex)
        #[arg(long)]
        router: String,

        /// Warp route policy ID
        #[arg(long)]
        warp_policy: Option<String>,

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

    /// List enrolled remote routers
    Routers {
        /// Warp route policy ID
        #[arg(long)]
        warp_policy: Option<String>,
    },

    /// Transfer tokens to remote chain
    Transfer {
        /// Destination domain ID
        #[arg(long)]
        domain: u32,

        /// Recipient address on destination (32 bytes hex)
        #[arg(long)]
        recipient: String,

        /// Amount to transfer
        #[arg(long)]
        amount: u64,

        /// Warp route policy ID
        #[arg(long)]
        warp_policy: Option<String>,

        /// Dry run
        #[arg(long)]
        dry_run: bool,
    },

    /// Deploy synthetic minting policy as a reference script
    /// This is required for the relayer to mint synthetic tokens when processing inbound transfers
    DeployMintingRef {
        /// Warp route NFT policy ID (identifies the synthetic warp route)
        #[arg(long)]
        warp_policy: String,

        /// Dry run
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Clone, ValueEnum)]
enum TokenType {
    /// Native ADA
    Native,
    /// Collateral (lock existing tokens)
    Collateral,
    /// Synthetic (mint new tokens)
    Synthetic,
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
        WarpCommands::EnrollRouter {
            domain,
            router,
            warp_policy,
            dry_run,
        } => enroll_router(ctx, domain, &router, warp_policy, dry_run).await,
        WarpCommands::Show { warp_policy } => show(ctx, warp_policy).await,
        WarpCommands::Routers { warp_policy } => list_routers(ctx, warp_policy).await,
        WarpCommands::Transfer {
            domain,
            recipient,
            amount,
            warp_policy,
            dry_run,
        } => transfer(ctx, domain, &recipient, amount, warp_policy, dry_run).await,
        WarpCommands::DeployMintingRef {
            warp_policy,
            dry_run,
        } => deploy_minting_ref(ctx, &warp_policy, dry_run).await,
    }
}

async fn deploy(
    ctx: &CliContext,
    token_type: TokenType,
    token_policy: Option<String>,
    token_asset: Option<String>,
    decimals: Option<u8>,
    remote_decimals: u8,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Deploying warp route...".cyan());

    // For Native (ADA), decimals is always 6. For others, it's required.
    let decimals = match token_type {
        TokenType::Native => 6,
        _ => decimals.ok_or_else(|| anyhow!("--decimals required for collateral/synthetic type"))?,
    };

    // Cardano native token amounts are limited to i64 (~9.2×10^18).
    // With decimals > 6, large transfer amounts can overflow when scaled.
    // For example, with 18 decimals, transferring just 10 tokens = 10×10^18 > i64::MAX.
    // We enforce a maximum of 6 decimals to prevent overflow issues.
    const MAX_CARDANO_DECIMALS: u8 = 6;
    if decimals > MAX_CARDANO_DECIMALS {
        return Err(anyhow!(
            "Cardano warp routes support a maximum of {} decimals (got {}). \
             Cardano native token amounts are limited to i64, and higher decimal \
             values can cause overflow during token transfers.",
            MAX_CARDANO_DECIMALS,
            decimals
        ));
    }

    let type_str = match token_type {
        TokenType::Native => "Native (ADA)",
        TokenType::Collateral => "Collateral (Lock existing tokens)",
        TokenType::Synthetic => "Synthetic (Mint new tokens)",
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
        TokenType::Synthetic => {
            deploy_synthetic_route(ctx, decimals, remote_decimals, dry_run).await
        }
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

    println!("\n{}", "Waiting for confirmation...".cyan());
    deploy_ctx.client.wait_for_tx(&warp_tx_hash, 120).await?;
    println!("  ✓ Warp route confirmed");

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
            minting_policy: extra_info
                .get("minting_policy")
                .and_then(|v| v.as_str())
                .map(String::from),
            minting_ref_script_utxo: None, // Will be set by deploy-minting-ref command
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

/// Deploy a Synthetic warp route (mints synthetic tokens)
async fn deploy_synthetic_route(
    ctx: &CliContext,
    decimals: u8,
    remote_decimals: u8,
    dry_run: bool,
) -> Result<()> {
    println!(
        "\n{}",
        "═══════════════════════════════════════════════════════════════".cyan()
    );
    println!("{}", "Deploying Synthetic Warp Route".cyan().bold());
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════".cyan()
    );

    println!("\n{}", "Configuration:".green());
    println!("  Local Decimals: {}", decimals);
    println!("  Remote Decimals: {}", remote_decimals);

    // Prepare deployment context (shared with collateral/native routes)
    let deploy_ctx = prepare_warp_deployment(ctx).await?;

    // Compute synthetic_token policy (parameterized by warp_route_hash)
    let warp_route_param_cbor = encode_script_hash_param(&deploy_ctx.warp_route_applied.policy_id)?;
    let synthetic_token_applied = apply_validator_param(
        &ctx.contracts_dir,
        "synthetic_token",
        "synthetic_token",
        &hex::encode(&warp_route_param_cbor),
    )?;
    let synthetic_policy_id = &synthetic_token_applied.policy_id;
    println!("  Synthetic Token Policy: {}", synthetic_policy_id.green());

    // Build warp route datum for synthetic type
    println!("\n{}", "Step 3: Preparing warp route deployment...".cyan());
    let warp_datum = build_warp_route_synthetic_datum(
        synthetic_policy_id,
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
        println!("  Synthetic Token Policy: {}", synthetic_policy_id);
        return Ok(());
    }

    // Finalize deployment (shared with collateral/native routes)
    let extra_info = serde_json::json!({
        "decimals": decimals,
        "synthetic_policy": synthetic_policy_id,
        "minting_policy": synthetic_policy_id,
    });
    let warp_tx_hash =
        finalize_warp_deployment(ctx, &deploy_ctx, &warp_datum, "synthetic", extra_info).await?;

    // Print summary
    print_synthetic_deployment_summary(
        &deploy_ctx.warp_route_applied.policy_id,
        &deploy_ctx.warp_nft_applied.policy_id,
        &deploy_ctx.warp_address,
        &warp_tx_hash,
        synthetic_policy_id,
    );

    Ok(())
}

/// Print deployment summary for synthetic warp routes
fn print_synthetic_deployment_summary(
    script_hash: &str,
    nft_policy: &str,
    address: &str,
    tx_hash: &str,
    synthetic_policy: &str,
) {
    let ref_script_utxo = format!("{}#1", tx_hash);

    println!(
        "\n{}",
        "═══════════════════════════════════════════════════════════════".green()
    );
    println!(
        "{}",
        "Synthetic Warp Route Deployment Complete!".green().bold()
    );
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════".green()
    );
    println!();
    println!("{}", "Synthetic Token:".cyan());
    println!("  Policy ID: {}", synthetic_policy);
    println!("  Note: Tokens are minted when receiving transfers from other chains");
    println!();
    println!("{}", "Warp Route:".cyan());
    println!("  Script Hash: {}", script_hash);
    println!("  NFT Policy: {}", nft_policy);
    println!("  Address: {}", address);
    println!("  TX: {}", tx_hash);
    println!("  State UTXO: {}#0", tx_hash);
    println!("  Reference Script UTXO: {}", ref_script_utxo);
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
    println!("2. Receive tokens from other chains:");
    println!("   Transfers from remote chains will mint synthetic tokens to recipients");
    println!();
}

async fn enroll_router(
    ctx: &CliContext,
    domain: u32,
    router: &str,
    warp_policy: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!(
        "\n{}",
        "═══════════════════════════════════════════════════════════════".cyan()
    );
    println!("{}", "Enrolling Remote Router".cyan().bold());
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════".cyan()
    );

    let router = router.strip_prefix("0x").unwrap_or(router);
    if router.len() != 64 {
        return Err(anyhow!("Router address must be 32 bytes (64 hex chars)"));
    }

    println!("\n{}", "Configuration:".green());
    println!("  Domain: {}", domain);
    println!("  Router: 0x{}", router);

    let policy_id = get_warp_policy(ctx, warp_policy)?;
    println!("  Warp Policy: {}", policy_id);

    // Load API key and signing key
    let api_key = ctx.require_api_key()?;
    let keypair = ctx.load_signing_key()?;
    let owner_pkh = keypair.verification_key_hash_hex();
    let payer_address = keypair.address_bech32(ctx.pallas_network());

    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    // Find warp route UTXO
    println!("\n{}", "Step 1: Finding warp route UTXO...".cyan());
    let warp_utxo = client
        .find_utxo_by_asset(&policy_id, "")
        .await?
        .ok_or_else(|| anyhow!("Warp route UTXO not found with policy {}", policy_id))?;

    println!("  TX: {}#{}", warp_utxo.tx_hash, warp_utxo.output_index);
    println!("  Address: {}", warp_utxo.address);
    println!("  Lovelace: {}", warp_utxo.lovelace);

    // Parse existing datum to extract current state
    println!("\n{}", "Step 2: Parsing current datum...".cyan());
    let datum = warp_utxo
        .inline_datum
        .as_ref()
        .ok_or_else(|| anyhow!("Warp route UTXO has no inline datum"))?;

    // Extract current configuration from datum
    let (token_type, decimals, remote_decimals, current_routes, current_owner, total_bridged) =
        parse_warp_datum(datum)?;
    println!("  Token Type: {:?}", token_type);
    println!("  Decimals: {}", decimals);
    println!("  Remote Decimals: {}", remote_decimals);
    println!("  Current Routes: {}", current_routes.len());
    println!("  Total Bridged: {}", total_bridged);

    // Verify owner matches signer
    if current_owner != owner_pkh {
        return Err(anyhow!(
            "Signing key does not match owner. Expected: {}, Got: {}",
            current_owner,
            owner_pkh
        ));
    }

    // Check if route already enrolled
    if current_routes.iter().any(|r| r.domain == domain) {
        println!(
            "\n{}",
            "Warning: Domain already has an enrolled route. This will update it.".yellow()
        );
    }

    // Build updated routes list
    let mut new_routes: Vec<RemoteRoute> = current_routes
        .into_iter()
        .filter(|r| r.domain != domain) // Remove existing route for this domain
        .collect();
    new_routes.push(RemoteRoute {
        domain,
        router: router.to_string(),
    });

    // Build updated datum based on token type
    println!("\n{}", "Step 3: Building updated datum...".cyan());
    let new_datum = match &token_type {
        WarpTokenTypeInfo::Collateral {
            policy_id: tp,
            asset_name,
        } => build_warp_route_collateral_datum_with_routes(
            tp,
            asset_name,
            decimals,
            remote_decimals,
            &new_routes,
            &owner_pkh,
            total_bridged,
        )?,
        WarpTokenTypeInfo::Synthetic { minting_policy } => {
            build_warp_route_synthetic_datum_with_routes(
                minting_policy,
                decimals,
                remote_decimals,
                &new_routes,
                &owner_pkh,
                total_bridged,
            )?
        }
        WarpTokenTypeInfo::Native => build_warp_route_native_datum_with_routes(
            decimals,
            remote_decimals,
            &new_routes,
            &owner_pkh,
            total_bridged,
        )?,
    };
    println!("  New Datum CBOR: {} bytes", new_datum.len());

    // Build redeemer
    let redeemer = build_enroll_remote_route_redeemer(domain, router)?;
    println!("  Redeemer CBOR: {} bytes", redeemer.len());

    if dry_run {
        println!(
            "\n{}",
            "═══════════════════════════════════════════════════════════════".yellow()
        );
        println!("{}", "[Dry run - not submitting transaction]".yellow());
        println!(
            "{}",
            "═══════════════════════════════════════════════════════════════".yellow()
        );
        println!("\nWould enroll route:");
        println!("  Domain {} -> 0x{}", domain, router);
        return Ok(());
    }

    // Find fee/collateral UTXOs
    println!("\n{}", "Step 4: Finding fee UTXOs...".cyan());
    let utxos = client.get_utxos(&payer_address).await?;
    let suitable_utxos: Vec<_> = utxos
        .iter()
        .filter(|u| u.lovelace >= 5_000_000 && u.assets.is_empty())
        .filter(|u| !(u.tx_hash == warp_utxo.tx_hash && u.output_index == warp_utxo.output_index))
        .collect();

    if suitable_utxos.len() < 2 {
        return Err(anyhow!(
            "Need at least 2 UTXOs with >= 5 ADA each (excluding warp UTXO). Found {}.",
            suitable_utxos.len()
        ));
    }

    let fee_input = suitable_utxos[0];
    let collateral = suitable_utxos[1];
    println!(
        "  Fee Input: {}#{}",
        fee_input.tx_hash, fee_input.output_index
    );
    println!(
        "  Collateral: {}#{}",
        collateral.tx_hash, collateral.output_index
    );

    // Build and submit transaction
    println!("\n{}", "Step 5: Building transaction...".cyan());

    // Use the reference script UTXO for the warp route
    // The reference script is at output index 1 of the same tx that created the warp route
    let warp_ref_script = format!("{}#1", warp_utxo.tx_hash);
    println!("  Reference Script: {}", warp_ref_script);

    let tx_builder = HyperlaneTxBuilder::new(&client, ctx.pallas_network());
    let tx = tx_builder
        .build_enroll_route_tx(
            &keypair,
            fee_input,
            collateral,
            &warp_utxo,
            Some(&warp_ref_script),
            &new_datum,
            &redeemer,
        )
        .await?;

    println!("  TX Hash: {}", hex::encode(&tx.tx_hash.0));

    let signed_tx = tx_builder.sign_tx(tx, &keypair)?;
    println!("  Submitting transaction...");
    let tx_hash = client.submit_tx(&signed_tx).await?;

    println!(
        "\n{}",
        "═══════════════════════════════════════════════════════════════".green()
    );
    println!("{}", "Remote Router Enrolled Successfully!".green().bold());
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════".green()
    );
    println!();
    println!("  Domain: {}", domain);
    println!("  Router: 0x{}", router);
    println!("  TX: {}", tx_hash);
    println!();

    Ok(())
}

/// Parsed warp token type info
#[derive(Debug)]
enum WarpTokenTypeInfo {
    Collateral {
        policy_id: String,
        asset_name: String,
    },
    Synthetic {
        minting_policy: String,
    },
    Native,
}

/// Parse warp route datum from Blockfrost JSON format
/// The datum may be either a CBOR hex string or a pre-parsed JSON object
/// Returns (token_type, decimals, remote_decimals, remote_routes, owner, total_bridged)
fn parse_warp_datum(
    datum: &serde_json::Value,
) -> Result<(WarpTokenTypeInfo, u32, u32, Vec<RemoteRoute>, String, i64)> {
    // If the datum is a string, it's CBOR hex - decode it first
    let parsed_datum = if let Some(cbor_hex) = datum.as_str() {
        decode_plutus_datum(cbor_hex)?
    } else {
        datum.clone()
    };

    // WarpRouteDatum { config, owner, total_bridged }
    let fields = parsed_datum
        .get("fields")
        .and_then(|f| f.as_array())
        .ok_or_else(|| {
            anyhow!(
                "Invalid datum: missing fields. Datum: {}",
                serde_json::to_string_pretty(&parsed_datum).unwrap_or_default()
            )
        })?;

    if fields.len() < 3 {
        return Err(anyhow!(
            "Invalid datum: expected 3 fields, got {}",
            fields.len()
        ));
    }

    // Parse config (first field)
    let config = &fields[0];
    let config_fields = config
        .get("fields")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow!("Invalid config: missing fields"))?;

    if config_fields.len() < 4 {
        return Err(anyhow!("Invalid config: expected 4 fields (token_type, decimals, remote_decimals, remote_routes), got {}", config_fields.len()));
    }

    // Parse token_type (first field of config)
    let token_type_data = &config_fields[0];
    let token_type_constructor = token_type_data
        .get("constructor")
        .and_then(|c| c.as_u64())
        .ok_or_else(|| anyhow!("Invalid token_type: missing constructor"))?;

    let token_type = match token_type_constructor {
        0 => {
            // Collateral { policy_id, asset_name }
            let tt_fields = token_type_data
                .get("fields")
                .and_then(|f| f.as_array())
                .ok_or_else(|| anyhow!("Invalid Collateral: missing fields"))?;

            let policy_id = tt_fields
                .get(0)
                .and_then(|f| f.get("bytes"))
                .and_then(|b| b.as_str())
                .ok_or_else(|| anyhow!("Invalid Collateral: missing policy_id"))?
                .to_string();

            let asset_name = tt_fields
                .get(1)
                .and_then(|f| f.get("bytes"))
                .and_then(|b| b.as_str())
                .unwrap_or("")
                .to_string();

            WarpTokenTypeInfo::Collateral {
                policy_id,
                asset_name,
            }
        }
        1 => {
            // Synthetic { minting_policy }
            let tt_fields = token_type_data
                .get("fields")
                .and_then(|f| f.as_array())
                .ok_or_else(|| anyhow!("Invalid Synthetic: missing fields"))?;

            let minting_policy = tt_fields
                .get(0)
                .and_then(|f| f.get("bytes"))
                .and_then(|b| b.as_str())
                .ok_or_else(|| anyhow!("Invalid Synthetic: missing minting_policy"))?
                .to_string();

            WarpTokenTypeInfo::Synthetic { minting_policy }
        }
        2 => {
            // Native (no fields - ADA is held directly in warp route UTXO)
            WarpTokenTypeInfo::Native
        }
        _ => {
            return Err(anyhow!(
                "Unknown token type constructor: {}",
                token_type_constructor
            ))
        }
    };

    // Parse decimals (second field of config)
    let decimals = config_fields
        .get(1)
        .and_then(|f| f.get("int"))
        .and_then(|i| i.as_u64())
        .ok_or_else(|| anyhow!("Invalid decimals"))? as u32;

    // Parse remote_decimals (third field of config)
    let remote_decimals = config_fields
        .get(2)
        .and_then(|f| f.get("int"))
        .and_then(|i| i.as_u64())
        .ok_or_else(|| anyhow!("Invalid remote_decimals"))? as u32;

    // Parse remote_routes (fourth field of config)
    let empty_routes: Vec<serde_json::Value> = vec![];
    let routes_list = config_fields
        .get(3)
        .and_then(|f| f.get("list"))
        .and_then(|l| l.as_array())
        .unwrap_or(&empty_routes);

    let mut remote_routes = Vec::new();
    for route in routes_list {
        let route_fields = route
            .get("list")
            .or_else(|| route.get("fields"))
            .and_then(|f| f.as_array());

        if let Some(rf) = route_fields {
            let domain = rf
                .get(0)
                .and_then(|d| d.get("int"))
                .and_then(|i| i.as_u64())
                .unwrap_or(0) as u32;

            let router = rf
                .get(1)
                .and_then(|r| r.get("bytes"))
                .and_then(|b| b.as_str())
                .unwrap_or("")
                .to_string();

            if !router.is_empty() {
                remote_routes.push(RemoteRoute { domain, router });
            }
        }
    }

    // Parse owner (second field of datum)
    let owner = fields
        .get(1)
        .and_then(|f| f.get("bytes"))
        .and_then(|b| b.as_str())
        .ok_or_else(|| anyhow!("Invalid owner"))?
        .to_string();

    // Parse total_bridged (third field of datum)
    let total_bridged = fields
        .get(2)
        .and_then(|f| f.get("int"))
        .and_then(|i| i.as_i64())
        .unwrap_or(0);

    Ok((
        token_type,
        decimals,
        remote_decimals,
        remote_routes,
        owner,
        total_bridged,
    ))
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
        // Decode CBOR if datum is a hex string (Blockfrost returns CBOR hex for inline datums)
        let parsed_datum = if let Some(cbor_hex) = datum.as_str() {
            decode_plutus_datum(cbor_hex)?
        } else {
            datum.clone()
        };
        println!("\n{}", "Configuration:".green());
        println!("{}", serde_json::to_string_pretty(&parsed_datum)?);
    }

    Ok(())
}

async fn list_routers(ctx: &CliContext, warp_policy: Option<String>) -> Result<()> {
    println!("{}", "Enrolled Remote Routers".cyan());

    let policy_id = get_warp_policy(ctx, warp_policy)?;
    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let warp_utxo = client
        .find_utxo_by_asset(&policy_id, "")
        .await?
        .ok_or_else(|| anyhow!("Warp route UTXO not found"))?;

    if let Some(datum) = &warp_utxo.inline_datum {
        // Decode CBOR if datum is a hex string (Blockfrost returns CBOR hex for inline datums)
        let parsed_datum = if let Some(cbor_hex) = datum.as_str() {
            decode_plutus_datum(cbor_hex)?
        } else {
            datum.clone()
        };

        // Parse remote_routes from datum
        if let Some(fields) = parsed_datum.get("fields").and_then(|f| f.as_array()) {
            // config is first field, remote_routes is inside config at index 3
            // Config: [token_type, decimals, remote_decimals, remote_routes]
            if let Some(config_fields) = fields
                .get(0)
                .and_then(|c| c.get("fields"))
                .and_then(|f| f.as_array())
            {
                if let Some(routes) = config_fields
                    .get(3)
                    .and_then(|r| r.get("list"))
                    .and_then(|l| l.as_array())
                {
                    if routes.is_empty() {
                        println!("\n{}", "No remote routers enrolled".yellow());
                        return Ok(());
                    }

                    println!("\n{}", "Remote Routers:".green());
                    println!("{}", "-".repeat(80));

                    for route in routes {
                        // Routes can be encoded as either:
                        // 1. {"list": [{"int": domain}, {"bytes": router}]} - tuple encoding
                        // 2. {"fields": [{"int": domain}, {"bytes": router}]} - constructor encoding
                        let route_items = route
                            .get("list")
                            .or_else(|| route.get("fields"))
                            .and_then(|f| f.as_array());

                        if let Some(items) = route_items {
                            let domain = items
                                .get(0)
                                .and_then(|d| d.get("int"))
                                .and_then(|i| i.as_u64());
                            let router = items
                                .get(1)
                                .and_then(|r| r.get("bytes"))
                                .and_then(|b| b.as_str());

                            if let (Some(d), Some(r)) = (domain, router) {
                                println!("  Domain {}: 0x{}", d, r);
                            }
                        }
                    }

                    return Ok(());
                }
            }
        }
    }

    println!("\n{}", "No remote routers enrolled".yellow());

    Ok(())
}

async fn transfer(
    ctx: &CliContext,
    domain: u32,
    recipient: &str,
    amount: u64,
    warp_policy: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("\n{}", "═══════════════════════════════════════════════════════════════".cyan());
    println!("{}", "Initiating Cross-Chain Token Transfer".cyan().bold());
    println!("{}", "═══════════════════════════════════════════════════════════════".cyan());

    // Automatically pad shorter addresses (e.g., 20-byte ETH, 28-byte Cardano) to 32 bytes
    let recipient_hex = recipient.strip_prefix("0x").unwrap_or(recipient);
    if recipient_hex.len() > 64 {
        return Err(anyhow!("Recipient too long: {} chars (max 64)", recipient_hex.len()));
    }
    // Left-pad with zeros to 64 hex chars (32 bytes)
    let recipient_padded = format!("{:0>64}", recipient_hex);

    println!("\n{}", "Transfer Configuration:".green());
    println!("  Destination Domain: {}", domain);
    println!("  Recipient: 0x{}", recipient_padded);
    println!("  Amount: {}", amount);

    // Get warp policy ID
    let warp_policy_id = get_warp_policy(ctx, warp_policy)?;
    println!("  Warp Policy: {}", warp_policy_id);

    // Load API key and signing key
    let api_key = ctx.require_api_key()?;
    let keypair = ctx.load_signing_key()?;
    let payer_address = keypair.address_bech32(ctx.pallas_network());
    let payer_pkh = keypair.pub_key_hash();
    println!("  Sender: {}", payer_address);

    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    // Step 1: Find warp route UTXO and parse its datum
    println!("\n{}", "Step 1: Finding Warp Route UTXO...".cyan());
    let warp_utxo = client
        .find_utxo_by_asset(&warp_policy_id, "")
        .await?
        .ok_or_else(|| anyhow!("Warp route UTXO not found with policy {}", warp_policy_id))?;

    println!("  TX: {}#{}", warp_utxo.tx_hash, warp_utxo.output_index);

    let warp_datum = warp_utxo.inline_datum.as_ref()
        .ok_or_else(|| anyhow!("Warp route UTXO has no inline datum"))?;
    let (token_type, decimals, remote_decimals, remote_routes, warp_owner, total_bridged) = parse_warp_datum(warp_datum)?;

    println!("  Token Type: {:?}", token_type);
    println!("  Decimals: {}", decimals);
    println!("  Remote Decimals: {}", remote_decimals);
    println!("  Remote Routes: {}", remote_routes.len());
    println!("  Total Bridged: {}", total_bridged);

    // Verify destination has an enrolled route
    let remote_router = remote_routes.iter()
        .find(|r| r.domain == domain)
        .ok_or_else(|| anyhow!(
            "No remote router enrolled for domain {}. Use 'warp enroll-router' first.",
            domain
        ))?;
    println!("  Remote Router ({}): 0x{}", domain, remote_router.router);

    // The Hyperlane message recipient is the remote warp route contract, not the user's recipient.
    // The user's recipient is encoded in the message body and will be processed by the remote warp route.
    let message_recipient = &remote_router.router;

    // Step 2: Find mailbox UTXO and parse its datum
    println!("\n{}", "Step 2: Finding Mailbox UTXO...".cyan());
    let deployment = ctx.load_deployment_info()?;
    let mailbox_policy_id = deployment
        .mailbox
        .as_ref()
        .and_then(|m| m.state_nft_policy.as_ref())
        .ok_or_else(|| anyhow!("Mailbox not deployed"))?;

    let mailbox_asset_name = deployment
        .mailbox
        .as_ref()
        .and_then(|m| m.state_nft.as_ref())
        .map(|nft| nft.asset_name_hex.clone())
        .unwrap_or_else(|| hex::encode("Mailbox State"));

    let mailbox_utxo = client
        .find_utxo_by_asset(mailbox_policy_id, &mailbox_asset_name)
        .await?
        .ok_or_else(|| anyhow!("Mailbox UTXO not found"))?;

    println!("  TX: {}#{}", mailbox_utxo.tx_hash, mailbox_utxo.output_index);

    let mailbox_datum_value = mailbox_utxo.inline_datum.as_ref()
        .ok_or_else(|| anyhow!("Mailbox UTXO has no inline datum"))?;
    let mailbox_data = parse_mailbox_datum_for_transfer(mailbox_datum_value)?;

    println!("  Local Domain: {}", mailbox_data.local_domain);
    println!("  Outbound Nonce: {}", mailbox_data.outbound_nonce);
    println!("  Merkle Count: {}", mailbox_data.merkle_count);

    // Step 3: For Native/Collateral types, ADA/tokens are locked in warp route UTXO
    // No separate vault needed
    match &token_type {
        WarpTokenTypeInfo::Collateral { .. } | WarpTokenTypeInfo::Native => {
            println!("\n{}", "Step 3: Native/Collateral - tokens locked in warp route UTXO".cyan());
        }
        WarpTokenTypeInfo::Synthetic { .. } => {
            println!("\n{}", "Step 3: Synthetic route - tokens will be burned".cyan());
        }
    };

    // Step 4: Build message body and compute message ID
    println!("\n{}", "Step 4: Building Hyperlane Message...".cyan());

    // Message body for warp transfers: recipient (32 bytes) || amount (uint256, 32 bytes big-endian)
    // The amount must be scaled from Cardano decimals to remote decimals (wire format)
    // Scale factor = 10^(remote_decimals - local_decimals)
    // For Cardano tokens with 6 decimals to EVM 18 decimals: scale = 10^12
    let scaled_amount: u128 = if remote_decimals >= decimals {
        let scale_exponent = remote_decimals - decimals;
        let scale_factor: u128 = 10u128.pow(scale_exponent);
        let result = (amount as u128) * scale_factor;
        println!("  Scaling: {} (local, {} decimals) * 10^{} = {} (wire, {} decimals)",
            amount, decimals, scale_exponent, result, remote_decimals);
        result
    } else {
        let scale_exponent = decimals - remote_decimals;
        let scale_factor: u128 = 10u128.pow(scale_exponent);
        let result = (amount as u128) / scale_factor;
        println!("  Scaling: {} (local, {} decimals) / 10^{} = {} (wire, {} decimals)",
            amount, decimals, scale_exponent, result, remote_decimals);
        result
    };

    let body_hex = build_warp_message_body(&recipient_padded, scaled_amount)?;
    println!("  Body: 0x{} ({} bytes)", &body_hex[..std::cmp::min(40, body_hex.len())], body_hex.len() / 2);

    // The sender is the warp_route script - the mailbox is skipped when computing sender
    // (matching the Aiken contract's get_sender_address logic which skips the mailbox input)
    let warp_route_info = deployment.warp_routes.iter()
        .find(|w| w.nft_policy == warp_policy_id)
        .ok_or_else(|| anyhow!("Warp route not found in deployment info"))?;
    let warp_script_hash = &warp_route_info.script_hash;

    // The sender is always the warp_route - the mailbox is explicitly skipped
    println!("  Sender (warp route): {}#{} -> {}", warp_utxo.tx_hash, warp_utxo.output_index, warp_script_hash);

    // Build sender address (32 bytes: 0x02000000 + script_hash for script credential)
    let sender_hex = format!("02000000{}", warp_script_hash);

    // Compute message ID
    // NOTE: The message recipient is the remote warp route contract, not the user's final recipient
    let message_id = compute_message_id_for_transfer(
        3, // version
        mailbox_data.outbound_nonce,
        mailbox_data.local_domain,
        &sender_hex,
        domain,
        message_recipient,
        &body_hex,
    )?;
    println!("  Message ID: 0x{}", message_id);
    println!("  Nonce: {}", mailbox_data.outbound_nonce);

    // Step 5: Update merkle tree
    println!("\n{}", "Step 5: Computing Merkle Tree Update...".cyan());
    let new_merkle = update_merkle_tree_for_transfer(
        &mailbox_data.merkle_branches,
        mailbox_data.merkle_count,
        &message_id,
    )?;
    println!("  New Count: {}", new_merkle.count);

    // Step 6: Build updated datums
    println!("\n{}", "Step 6: Building Updated Datums...".cyan());

    // Updated warp route datum (increment total_bridged)
    let new_total_bridged = total_bridged + amount as i64;
    let new_warp_datum = match &token_type {
        WarpTokenTypeInfo::Collateral { policy_id, asset_name } => {
            build_warp_route_collateral_datum_with_routes(
                policy_id,
                asset_name,
                decimals,
                remote_decimals,
                &remote_routes,
                &warp_owner,
                new_total_bridged,
            )?
        }
        WarpTokenTypeInfo::Synthetic { minting_policy } => {
            build_warp_route_synthetic_datum_with_routes(
                minting_policy,
                decimals,
                remote_decimals,
                &remote_routes,
                &warp_owner,
                new_total_bridged,
            )?
        }
        WarpTokenTypeInfo::Native => {
            build_warp_route_native_datum_with_routes(
                decimals,
                remote_decimals,
                &remote_routes,
                &warp_owner,
                new_total_bridged,
            )?
        }
    };
    println!("  Warp Datum: {} bytes", new_warp_datum.len());

    // Updated mailbox datum (increment nonce, update merkle tree)
    let branches_refs: Vec<&str> = new_merkle.branches.iter().map(|s| s.as_str()).collect();
    let new_mailbox_datum = build_mailbox_datum(
        mailbox_data.local_domain,
        &mailbox_data.default_ism,
        &mailbox_data.owner,
        mailbox_data.outbound_nonce + 1,
        &branches_refs,
        new_merkle.count,
    )?;
    println!("  Mailbox Datum: {} bytes", new_mailbox_datum.len());

    // Step 7: Build redeemers
    println!("\n{}", "Step 7: Building Redeemers...".cyan());

    let warp_redeemer = build_transfer_remote_redeemer(domain, &recipient_padded, amount as i64)?;
    println!("  TransferRemote Redeemer: {} bytes", warp_redeemer.len());

    // NOTE: Mailbox dispatch uses the remote warp route as recipient, not the user's final recipient
    let mailbox_redeemer = build_mailbox_dispatch_redeemer(domain, message_recipient, &body_hex)?;
    println!("  Dispatch Redeemer: {} bytes", mailbox_redeemer.len());

    if dry_run {
        println!("\n{}", "═══════════════════════════════════════════════════════════════".yellow());
        println!("{}", "[Dry run - not submitting transaction]".yellow());
        println!("{}", "═══════════════════════════════════════════════════════════════".yellow());
        println!("\n{}", "Transaction Summary:".cyan());
        println!("  - Spend Warp Route UTXO: {}#{}", warp_utxo.tx_hash, warp_utxo.output_index);
        println!("  - Spend Mailbox UTXO: {}#{}", mailbox_utxo.tx_hash, mailbox_utxo.output_index);
        println!("\n{}", "Message Summary:".cyan());
        println!("  Message ID: 0x{}", message_id);
        println!("  From: {} (domain {})", payer_address, mailbox_data.local_domain);
        println!("  To Warp Route: 0x{} (domain {})", message_recipient, domain);
        println!("  Final Recipient: 0x{}", recipient_padded);
        println!("  Amount: {}", amount);
        return Ok(());
    }

    // Step 8: Build and submit transaction
    println!("\n{}", "Step 8: Building Transaction...".cyan());

    // Find payer UTXOs for fees and collateral
    let payer_utxos = client.get_utxos(&payer_address).await?;

    // Find tokens to transfer (for collateral type) or burn (for synthetic)
    let token_utxo = match &token_type {
        WarpTokenTypeInfo::Collateral { policy_id: token_pol, asset_name: token_asset, .. } => {
            // Find UTXO with the tokens to transfer
            let asset_name_ascii = String::from_utf8(hex::decode(token_asset).unwrap_or_default())
                .unwrap_or_default();
            payer_utxos.iter()
                .find(|u| u.assets.iter().any(|a|
                    a.policy_id == *token_pol &&
                    (a.asset_name == *token_asset || a.asset_name == asset_name_ascii) &&
                    a.quantity >= amount
                ))
                .cloned()
        }
        WarpTokenTypeInfo::Synthetic { minting_policy } => {
            // Find UTXO with synthetic tokens to burn
            // Synthetic tokens use empty asset name
            payer_utxos.iter()
                .find(|u| u.assets.iter().any(|a|
                    a.policy_id == *minting_policy &&
                    a.quantity >= amount
                ))
                .cloned()
        }
        _ => None,
    };

    // Find collateral UTXO (pure ADA, no tokens)
    let collateral_utxo = payer_utxos
        .iter()
        .find(|u| u.lovelace >= 10_000_000 && u.assets.is_empty())
        .ok_or_else(|| anyhow!("No suitable collateral UTXO (need 10+ ADA without tokens)"))?;

    // Calculate minimum lovelace needed for fee UTXO
    // For Native transfers, need: amount + fee_estimate + min_change_buffer
    // For other types, just need enough for fees
    let fee_estimate = 3_000_000u64;
    let min_change = 2_000_000u64;
    let min_fee_utxo_lovelace = match &token_type {
        WarpTokenTypeInfo::Native => amount + fee_estimate + min_change,
        _ => 5_000_000,
    };

    // Find fee UTXO that sorts AFTER the warp route UTXO
    // This ensures the warp route script is the first input and thus the sender
    // Inputs are sorted lexicographically by (tx_hash, output_index)
    let fee_utxo = payer_utxos
        .iter()
        .filter(|u| {
            u.lovelace >= min_fee_utxo_lovelace &&
            u.assets.is_empty() &&
            (u.tx_hash != collateral_utxo.tx_hash || u.output_index != collateral_utxo.output_index)
        })
        // Prefer UTXOs that sort after the warp route UTXO
        .min_by(|a, b| {
            // Sort by: (sorts_after_warp DESC, tx_hash ASC, output_index ASC)
            let a_after_warp = (&a.tx_hash, a.output_index) > (&warp_utxo.tx_hash, warp_utxo.output_index);
            let b_after_warp = (&b.tx_hash, b.output_index) > (&warp_utxo.tx_hash, warp_utxo.output_index);
            match (a_after_warp, b_after_warp) {
                (true, false) => std::cmp::Ordering::Less,    // a sorts after warp, prefer a
                (false, true) => std::cmp::Ordering::Greater, // b sorts after warp, prefer b
                _ => (&a.tx_hash, a.output_index).cmp(&(&b.tx_hash, b.output_index)),
            }
        })
        .ok_or_else(|| {
            let needed_ada = min_fee_utxo_lovelace / 1_000_000;
            anyhow!(
                "No suitable fee UTXO found. Need at least {} ADA for this {} transfer.\n\
                Hint: You may need to combine smaller UTXOs or use a smaller transfer amount.",
                needed_ada,
                match &token_type {
                    WarpTokenTypeInfo::Native => "native",
                    WarpTokenTypeInfo::Collateral { .. } => "collateral",
                    WarpTokenTypeInfo::Synthetic { .. } => "synthetic",
                }
            )
        })?;

    println!("  Fee UTXO: {}#{}", fee_utxo.tx_hash, fee_utxo.output_index);
    println!("  Collateral: {}#{}", collateral_utxo.tx_hash, collateral_utxo.output_index);

    // Get cost model and current slot
    let cost_model = client.get_plutusv3_cost_model().await?;
    let current_slot = client.get_latest_slot().await?;
    let validity_end = current_slot + 7200; // ~2 hours

    // Find warp route reference script UTXO from deployment info
    let warp_ref_utxo = deployment.warp_routes.iter()
        .find(|w| w.nft_policy == warp_policy_id)
        .and_then(|w| w.reference_script_utxo.as_ref());

    // Load warp script only if no reference UTXO available
    // The warp_route validator takes two params: mailbox_policy_id + state_nft_policy_id
    let warp_script = if warp_ref_utxo.is_none() {
        let mailbox_param_cbor = encode_script_hash_param(mailbox_policy_id)?;
        let state_nft_param_cbor = encode_script_hash_param(&warp_policy_id)?;
        let warp_route_applied = apply_validator_params(
            &ctx.contracts_dir,
            "warp_route",
            "warp_route",
            &[
                &hex::encode(&mailbox_param_cbor),
                &hex::encode(&state_nft_param_cbor),
            ],
        )?;
        Some(hex::decode(&warp_route_applied.compiled_code)?)
    } else {
        None
    };

    // Build the transaction using pallas_txbuilder
    use pallas_txbuilder::{BuildConway, Input, Output, StagingTransaction, ScriptKind, ExUnits};
    use pallas_crypto::hash::Hash;

    // Parse addresses
    let warp_addr = pallas_addresses::Address::from_bech32(&warp_utxo.address)?;
    let mailbox_addr = pallas_addresses::Address::from_bech32(&mailbox_utxo.address)?;
    let payer_addr = pallas_addresses::Address::from_bech32(&payer_address)?;

    // Parse tx hashes
    let warp_tx_hash: [u8; 32] = hex::decode(&warp_utxo.tx_hash)?.try_into()
        .map_err(|_| anyhow!("Invalid warp tx hash"))?;
    let mailbox_tx_hash: [u8; 32] = hex::decode(&mailbox_utxo.tx_hash)?.try_into()
        .map_err(|_| anyhow!("Invalid mailbox tx hash"))?;
    let fee_tx_hash: [u8; 32] = hex::decode(&fee_utxo.tx_hash)?.try_into()
        .map_err(|_| anyhow!("Invalid fee tx hash"))?;
    let collateral_tx_hash: [u8; 32] = hex::decode(&collateral_utxo.tx_hash)?.try_into()
        .map_err(|_| anyhow!("Invalid collateral tx hash"))?;

    // Parse policy IDs
    let warp_policy_bytes: [u8; 28] = hex::decode(&warp_policy_id)?.try_into()
        .map_err(|_| anyhow!("Invalid warp policy"))?;
    let mailbox_policy_bytes: [u8; 28] = hex::decode(mailbox_policy_id)?.try_into()
        .map_err(|_| anyhow!("Invalid mailbox policy"))?;
    let mailbox_asset_bytes = hex::decode(&mailbox_asset_name)?;

    // Build warp route continuation output
    // For Native transfers, the locked ADA goes into the warp_route UTXO
    let warp_output_lovelace = match &token_type {
        WarpTokenTypeInfo::Native => warp_utxo.lovelace + amount,
        _ => warp_utxo.lovelace,
    };
    let mut warp_output = Output::new(warp_addr.clone(), warp_output_lovelace)
        .set_inline_datum(new_warp_datum.clone())
        .add_asset(Hash::new(warp_policy_bytes), vec![], 1)
        .map_err(|e| anyhow!("Failed to add warp NFT: {:?}", e))?;

    // For Collateral transfers, add the locked tokens to the warp_route UTXO
    if let WarpTokenTypeInfo::Collateral { policy_id: token_pol, asset_name: token_asset } = &token_type {
        println!("  Adding collateral tokens to warp output...");
        println!("    Token Policy: {}", token_pol);
        println!("    Token Asset (hex): {}", token_asset);

        let token_pol_bytes: [u8; 28] = hex::decode(token_pol)?.try_into()
            .map_err(|_| anyhow!("Invalid token policy"))?;
        let token_asset_bytes = hex::decode(token_asset)?;
        println!("    Token Asset (decoded): {} bytes", token_asset_bytes.len());

        // Get current token amount in warp route (if any)
        let current_warp_tokens = warp_utxo.assets.iter()
            .find(|a| a.policy_id == *token_pol)
            .map(|a| a.quantity)
            .unwrap_or(0);
        println!("    Current tokens in warp route: {}", current_warp_tokens);
        println!("    Adding: {} tokens", current_warp_tokens + amount);

        warp_output = warp_output
            .add_asset(Hash::new(token_pol_bytes), token_asset_bytes.clone(), current_warp_tokens + amount)
            .map_err(|e| anyhow!("Failed to add tokens to warp route: {:?}", e))?;
        println!("    Collateral tokens added successfully");
    }

    // Build mailbox continuation output
    let mailbox_output = Output::new(mailbox_addr.clone(), mailbox_utxo.lovelace)
        .set_inline_datum(new_mailbox_datum.clone())
        .add_asset(Hash::new(mailbox_policy_bytes), mailbox_asset_bytes, 1)
        .map_err(|e| anyhow!("Failed to add mailbox NFT: {:?}", e))?;

    // Build staging transaction
    let payer_pkh_bytes: [u8; 28] = payer_pkh.try_into()
        .map_err(|_| anyhow!("Invalid payer pkh length"))?;

    // For native transfers, the amount being locked in warp_route comes from the fee UTXO
    let native_transfer_amount = match &token_type {
        WarpTokenTypeInfo::Native => amount,
        _ => 0,
    };
    let change = fee_utxo.lovelace.saturating_sub(fee_estimate).saturating_sub(native_transfer_amount);

    // If change is too small to create an output, add it to the fee to balance the transaction
    let actual_fee = if change > 1_500_000 {
        fee_estimate
    } else {
        fee_estimate + change
    };

    let mut staging = StagingTransaction::new()
        // Fee input
        .input(Input::new(Hash::new(fee_tx_hash), fee_utxo.output_index as u64))
        // Warp route input
        .input(Input::new(Hash::new(warp_tx_hash), warp_utxo.output_index as u64))
        // Mailbox input
        .input(Input::new(Hash::new(mailbox_tx_hash), mailbox_utxo.output_index as u64))
        // Collateral
        .collateral_input(Input::new(Hash::new(collateral_tx_hash), collateral_utxo.output_index as u64))
        // Required signer
        .disclosed_signer(Hash::new(payer_pkh_bytes))
        // Outputs
        .output(warp_output)
        .output(mailbox_output)
        // Warp route spend redeemer
        .add_spend_redeemer(
            Input::new(Hash::new(warp_tx_hash), warp_utxo.output_index as u64),
            warp_redeemer.clone(),
            Some(ExUnits { mem: 2_000_000, steps: 1_000_000_000 }),
        )
        // Mailbox spend redeemer
        .add_spend_redeemer(
            Input::new(Hash::new(mailbox_tx_hash), mailbox_utxo.output_index as u64),
            mailbox_redeemer.clone(),
            Some(ExUnits { mem: 5_000_000, steps: 2_000_000_000 }),
        )
        // Cost model
        .language_view(ScriptKind::PlutusV3, cost_model)
        // Fee and validity (use actual_fee which includes dust if change not created)
        .fee(actual_fee)
        .invalid_from_slot(validity_end)
        .network_id(0); // Testnet

    // For collateral type, add token input and change output
    if let (Some(ref tu), WarpTokenTypeInfo::Collateral { policy_id: token_pol, asset_name: token_asset }) =
           (&token_utxo, &token_type) {
        let token_tx_hash: [u8; 32] = hex::decode(&tu.tx_hash)?.try_into()
            .map_err(|_| anyhow!("Invalid token tx hash"))?;

        staging = staging.input(Input::new(Hash::new(token_tx_hash), tu.output_index as u64));

        // Build change output for remaining tokens
        let token_asset_obj = tu.assets.iter()
            .find(|a| a.policy_id == *token_pol)
            .ok_or_else(|| anyhow!("Token not found in UTXO"))?;

        if token_asset_obj.quantity > amount {
            let remaining = token_asset_obj.quantity - amount;
            let token_pol_bytes: [u8; 28] = hex::decode(token_pol)?.try_into()
                .map_err(|_| anyhow!("Invalid token policy"))?;
            let token_asset_bytes = hex::decode(token_asset)?;

            let token_change = Output::new(payer_addr.clone(), 2_000_000)
                .add_asset(Hash::new(token_pol_bytes), token_asset_bytes, remaining)
                .map_err(|e| anyhow!("Failed to add token change: {:?}", e))?;
            staging = staging.output(token_change);
        }
    }

    // Add synthetic token burning if needed
    if let (WarpTokenTypeInfo::Synthetic { minting_policy }, Some(ref tu)) = (&token_type, &token_utxo) {
        println!("  Adding synthetic token burn...");

        let token_tx_hash: [u8; 32] = hex::decode(&tu.tx_hash)?.try_into()
            .map_err(|_| anyhow!("Invalid synthetic token tx hash"))?;

        let mint_policy_bytes: [u8; 28] = hex::decode(minting_policy)?.try_into()
            .map_err(|_| anyhow!("Invalid minting policy"))?;

        // Find the synthetic token asset in the UTXO
        let synthetic_asset = tu.assets.iter()
            .find(|a| a.policy_id == *minting_policy)
            .ok_or_else(|| anyhow!("Synthetic token not found in UTXO"))?;

        let asset_name_bytes = hex::decode(&synthetic_asset.asset_name).unwrap_or_default();

        // Add the input with synthetic tokens
        staging = staging.input(Input::new(Hash::new(token_tx_hash), tu.output_index as u64));

        // Burn the tokens (negative mint)
        staging = staging
            .mint_asset(Hash::new(mint_policy_bytes), asset_name_bytes.clone(), -(amount as i64))
            .map_err(|e| anyhow!("Failed to add burn: {:?}", e))?;

        // Add burn redeemer (Burn is constructor 1 in synthetic minting policy)
        let burn_redeemer = crate::utils::cbor::build_synthetic_burn_redeemer(amount as i64);
        staging = staging.add_mint_redeemer(
            Hash::new(mint_policy_bytes),
            burn_redeemer,
            Some(ExUnits { mem: 500_000, steps: 200_000_000 }),
        );

        // Load and add the minting policy script
        // The synthetic minting policy is parameterized by the warp route hash
        let warp_hash_param = encode_script_hash_param(warp_script_hash)?;
        let mint_policy_applied = apply_validator_param(
            &ctx.contracts_dir,
            "synthetic_token",
            "synthetic_token",
            &hex::encode(&warp_hash_param),
        )?;
        let mint_script = hex::decode(&mint_policy_applied.compiled_code)?;
        staging = staging.script(ScriptKind::PlutusV3, mint_script);

        // If there are remaining synthetic tokens, create a change output
        if synthetic_asset.quantity > amount {
            let remaining = synthetic_asset.quantity - amount;
            let token_change = Output::new(payer_addr.clone(), tu.lovelace)
                .add_asset(Hash::new(mint_policy_bytes), asset_name_bytes, remaining)
                .map_err(|e| anyhow!("Failed to add synthetic token change: {:?}", e))?;
            staging = staging.output(token_change);
        }
    }

    // Add warp route script via reference if available, otherwise inline
    if let Some(ref_utxo) = warp_ref_utxo {
        println!("  Using warp route reference script: {}#{}", ref_utxo.tx_hash, ref_utxo.output_index);
        let ref_tx_hash: [u8; 32] = hex::decode(&ref_utxo.tx_hash)?.try_into()
            .map_err(|_| anyhow!("Invalid warp reference script tx hash"))?;
        staging = staging.reference_input(Input::new(Hash::new(ref_tx_hash), ref_utxo.output_index as u64));
    } else if let Some(script) = warp_script {
        println!("  Using inline warp route script ({} bytes)", script.len());
        staging = staging.script(ScriptKind::PlutusV3, script);
    }

    // Add mailbox script via reference if available, otherwise inline
    if let Some(ref_utxo) = deployment.mailbox.as_ref().and_then(|m| m.reference_script_utxo.as_ref()) {
        println!("  Using mailbox reference script: {}#{}", ref_utxo.tx_hash, ref_utxo.output_index);
        let ref_tx_hash: [u8; 32] = hex::decode(&ref_utxo.tx_hash)?.try_into()
            .map_err(|_| anyhow!("Invalid mailbox reference script tx hash"))?;
        staging = staging.reference_input(Input::new(Hash::new(ref_tx_hash), ref_utxo.output_index as u64));
    } else {
        let mailbox_script_raw = ctx.load_script_from_blueprint("mailbox", "mailbox.spend")?;
        let mailbox_script = hex::decode(&mailbox_script_raw)?;
        println!("  Using inline mailbox script ({} bytes)", mailbox_script.len());
        staging = staging.script(ScriptKind::PlutusV3, mailbox_script);
    }

    // Add change output
    if change > 1_500_000 {
        staging = staging.output(Output::new(payer_addr.clone(), change));
    }

    // Build the transaction
    let tx = staging.build_conway_raw()
        .map_err(|e| anyhow!("Failed to build transaction: {:?}", e))?;

    println!("  TX Hash: {}", hex::encode(&tx.tx_hash.0));

    // Sign the transaction
    println!("\n{}", "Signing transaction...".cyan());
    let tx_hash_bytes: &[u8] = &tx.tx_hash.0;
    let signature = keypair.sign(tx_hash_bytes);
    let signed_tx = tx.add_signature(keypair.pallas_public_key().clone(), signature)
        .map_err(|e| anyhow!("Failed to sign transaction: {:?}", e))?;

    // Submit the transaction
    println!("{}", "Submitting transaction...".cyan());
    let tx_hash = client.submit_tx(&signed_tx.tx_bytes.0).await?;

    println!("\n{}", "═══════════════════════════════════════════════════════════════".green());
    println!("{}", "Transfer Initiated Successfully!".green().bold());
    println!("{}", "═══════════════════════════════════════════════════════════════".green());
    println!();
    println!("  Transaction Hash: {}", tx_hash);
    println!("  Explorer: {}", ctx.explorer_tx_url(&tx_hash));
    println!();
    println!("{}", "Message Details:".cyan());
    println!("  Message ID: 0x{}", message_id);
    println!("  From: {} (domain {})", payer_address, mailbox_data.local_domain);
    println!("  To Warp Route: 0x{} (domain {})", message_recipient, domain);
    println!("  Final Recipient: 0x{}", recipient_padded);
    println!("  Amount: {}", amount);
    println!("  Nonce: {}", mailbox_data.outbound_nonce);
    println!();
    println!("{}", "Note: The relayer will pick up this message and deliver it".yellow());
    println!("{}", "to the destination chain. Track progress on Hyperlane Explorer.".yellow());

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

// ============================================================================
// Helper functions for warp transfer
// ============================================================================

/// Mailbox data needed for transfer
struct MailboxDataForTransfer {
    local_domain: u32,
    default_ism: String,
    owner: String,
    outbound_nonce: u32,
    merkle_branches: Vec<String>,
    merkle_count: u32,
}

/// Merkle tree state after update
struct MerkleTreeUpdate {
    branches: Vec<String>,
    count: u32,
}

/// Build warp message body: recipient (32 bytes) || amount (32 bytes, uint256 big-endian)
///
/// The EVM TokenMessage library expects:
/// - recipient: bytes32 (32 bytes)
/// - amount: uint256 (32 bytes, big-endian)
fn build_warp_message_body(recipient_hex: &str, amount: u128) -> Result<String> {
    // Validate recipient is 32 bytes (64 hex chars)
    if recipient_hex.len() != 64 {
        return Err(anyhow!("Recipient must be 32 bytes (64 hex chars)"));
    }

    // Amount as 32 bytes (uint256), big-endian, padded with leading zeros
    // EVM expects uint256 which is 32 bytes
    let mut amount_bytes = [0u8; 32];
    amount_bytes[16..32].copy_from_slice(&amount.to_be_bytes());

    // Concatenate recipient + amount (total 64 bytes)
    Ok(format!("{}{}", recipient_hex, hex::encode(amount_bytes)))
}

/// Compute message ID (keccak256 of encoded message)
fn compute_message_id_for_transfer(
    version: u8,
    nonce: u32,
    origin: u32,
    sender_hex: &str,
    destination: u32,
    recipient_hex: &str,
    body_hex: &str,
) -> Result<String> {
    let mut message = Vec::new();

    // Version (1 byte)
    message.push(version);

    // Nonce (4 bytes, big-endian)
    message.extend_from_slice(&nonce.to_be_bytes());

    // Origin (4 bytes, big-endian)
    message.extend_from_slice(&origin.to_be_bytes());

    // Sender (32 bytes)
    let sender_bytes = hex::decode(sender_hex)?;
    if sender_bytes.len() != 32 {
        return Err(anyhow!("Sender must be 32 bytes"));
    }
    message.extend_from_slice(&sender_bytes);

    // Destination (4 bytes, big-endian)
    message.extend_from_slice(&destination.to_be_bytes());

    // Recipient (32 bytes)
    let recipient_bytes = hex::decode(recipient_hex)?;
    if recipient_bytes.len() != 32 {
        return Err(anyhow!("Recipient must be 32 bytes"));
    }
    message.extend_from_slice(&recipient_bytes);

    // Body (variable)
    let body_bytes = hex::decode(body_hex)?;
    message.extend_from_slice(&body_bytes);

    // Compute keccak256
    let mut hasher = Keccak256::new();
    hasher.update(&message);
    let result = hasher.finalize();

    Ok(hex::encode(result))
}

/// Update merkle tree with a new leaf (message hash)
fn update_merkle_tree_for_transfer(
    current_branches: &[String],
    current_count: u32,
    message_id: &str,
) -> Result<MerkleTreeUpdate> {
    let message_hash = hex::decode(message_id)?;
    if message_hash.len() != 32 {
        return Err(anyhow!("Message hash must be 32 bytes"));
    }

    // Zero hash for empty branches
    let zero_hash = "0000000000000000000000000000000000000000000000000000000000000000";

    // Ensure we have 32 branches (pad with zeros if needed)
    let mut branches: Vec<String> = current_branches.to_vec();
    while branches.len() < 32 {
        branches.push(zero_hash.to_string());
    }

    // Insert the new leaf
    let new_count = current_count + 1;
    let mut node = message_id.to_string();
    let mut size = new_count;
    let mut depth = 0usize;

    // Standard Hyperlane merkle tree algorithm
    while size > 0 {
        if size % 2 == 1 {
            // Odd: store node at this level and stop
            branches[depth] = node.clone();
            break;
        } else {
            // Even: hash with sibling and continue up
            let sibling = &branches[depth];
            node = hash_pair(sibling, &node)?;
        }
        size /= 2;
        depth += 1;
    }

    Ok(MerkleTreeUpdate {
        branches,
        count: new_count,
    })
}

/// Hash two nodes together: keccak256(left || right)
fn hash_pair(left: &str, right: &str) -> Result<String> {
    let left_bytes = hex::decode(left)?;
    let right_bytes = hex::decode(right)?;

    let mut combined = Vec::new();
    combined.extend_from_slice(&left_bytes);
    combined.extend_from_slice(&right_bytes);

    let mut hasher = Keccak256::new();
    hasher.update(&combined);
    let result = hasher.finalize();

    Ok(hex::encode(result))
}

/// Parse mailbox datum for transfer
fn parse_mailbox_datum_for_transfer(datum: &serde_json::Value) -> Result<MailboxDataForTransfer> {
    // Check if datum is a hex string (raw CBOR)
    if let Some(hex_str) = datum.as_str() {
        return parse_mailbox_datum_from_cbor_for_transfer(hex_str);
    }

    // Otherwise try the JSON format
    let fields = datum
        .get("fields")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow!("Invalid datum structure - missing fields"))?;

    if fields.len() < 5 {
        return Err(anyhow!("Mailbox datum must have 5 fields, got {}", fields.len()));
    }

    let local_domain = fields
        .get(0)
        .and_then(|f| f.get("int"))
        .and_then(|i| i.as_u64())
        .ok_or_else(|| anyhow!("Invalid local_domain"))? as u32;

    let default_ism = fields
        .get(1)
        .and_then(|f| f.get("bytes"))
        .and_then(|b| b.as_str())
        .ok_or_else(|| anyhow!("Invalid default_ism"))?
        .to_string();

    let owner = fields
        .get(2)
        .and_then(|f| f.get("bytes"))
        .and_then(|b| b.as_str())
        .ok_or_else(|| anyhow!("Invalid owner"))?
        .to_string();

    let outbound_nonce = fields
        .get(3)
        .and_then(|f| f.get("int"))
        .and_then(|i| i.as_u64())
        .ok_or_else(|| anyhow!("Invalid outbound_nonce"))? as u32;

    // Parse nested MerkleTreeState
    let merkle_tree = fields
        .get(4)
        .ok_or_else(|| anyhow!("Missing merkle_tree field"))?;

    let merkle_tree_fields = merkle_tree
        .get("fields")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow!("Invalid merkle_tree structure"))?;

    if merkle_tree_fields.len() < 2 {
        return Err(anyhow!("MerkleTreeState must have 2 fields"));
    }

    let branches_list = merkle_tree_fields
        .get(0)
        .and_then(|f| f.get("list"))
        .and_then(|l| l.as_array())
        .ok_or_else(|| anyhow!("Invalid branches list"))?;

    let merkle_branches: Vec<String> = branches_list
        .iter()
        .filter_map(|b| b.get("bytes").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .collect();

    let merkle_count = merkle_tree_fields
        .get(1)
        .and_then(|f| f.get("int"))
        .and_then(|i| i.as_u64())
        .ok_or_else(|| anyhow!("Invalid merkle_count"))? as u32;

    Ok(MailboxDataForTransfer {
        local_domain,
        default_ism,
        owner,
        outbound_nonce,
        merkle_branches,
        merkle_count,
    })
}

/// Parse mailbox datum from raw CBOR hex
fn parse_mailbox_datum_from_cbor_for_transfer(hex_str: &str) -> Result<MailboxDataForTransfer> {
    let cbor_bytes = hex::decode(hex_str)?;
    let datum: PlutusData = pallas_codec::minicbor::decode(&cbor_bytes)
        .map_err(|e| anyhow!("Failed to decode CBOR datum: {:?}", e))?;

    // Mailbox Datum is Constr 0 [local_domain, default_ism, owner, outbound_nonce, merkle_tree]
    let fields = match &datum {
        PlutusData::Constr(c) if c.tag == 121 => &c.fields,
        _ => return Err(anyhow!("Expected Constr 0 datum")),
    };

    let fields_vec: Vec<&PlutusData> = fields.iter().collect();
    if fields_vec.len() < 5 {
        return Err(anyhow!("Mailbox datum must have 5 fields, got {}", fields_vec.len()));
    }

    let local_domain = extract_u32_for_transfer(fields_vec[0])?;
    let default_ism = extract_bytes_hex_for_transfer(fields_vec[1])?;
    let owner = extract_bytes_hex_for_transfer(fields_vec[2])?;
    let outbound_nonce = extract_u32_for_transfer(fields_vec[3])?;

    // Parse nested MerkleTreeState
    let merkle_tree_fields = match fields_vec[4] {
        PlutusData::Constr(c) if c.tag == 121 => {
            let f: Vec<&PlutusData> = c.fields.iter().collect();
            if f.len() < 2 {
                return Err(anyhow!("MerkleTreeState must have 2 fields"));
            }
            f
        }
        _ => return Err(anyhow!("Expected Constr 0 for MerkleTreeState")),
    };

    let merkle_branches = match merkle_tree_fields[0] {
        PlutusData::Array(arr) => {
            arr.iter()
                .map(|item| extract_bytes_hex_for_transfer(item))
                .collect::<Result<Vec<String>>>()?
        }
        _ => return Err(anyhow!("Expected array for merkle branches")),
    };

    let merkle_count = extract_u32_for_transfer(merkle_tree_fields[1])?;

    Ok(MailboxDataForTransfer {
        local_domain,
        default_ism,
        owner,
        outbound_nonce,
        merkle_branches,
        merkle_count,
    })
}

/// Extract u32 from PlutusData
fn extract_u32_for_transfer(data: &PlutusData) -> Result<u32> {
    match data {
        PlutusData::BigInt(BigInt::Int(i)) => {
            let inner = &i.0;
            match i64::try_from(*inner) {
                Ok(val) => Ok(val as u32),
                Err(_) => Err(anyhow!("Integer too large for u32")),
            }
        }
        _ => Err(anyhow!("Expected integer")),
    }
}

/// Extract bytes as hex string from PlutusData
fn extract_bytes_hex_for_transfer(data: &PlutusData) -> Result<String> {
    match data {
        PlutusData::BoundedBytes(b) => {
            let bytes: &[u8] = b.as_ref();
            Ok(hex::encode(bytes))
        }
        _ => Err(anyhow!("Expected bytes")),
    }
}

/// Deploy the synthetic minting policy as a reference script UTXO
///
/// This is required for the relayer to mint synthetic tokens when processing inbound transfers.
/// The minting policy is parameterized by the warp route script hash, so we need to:
/// 1. Find the synthetic warp route by its NFT policy
/// 2. Extract the warp route script hash from its address
/// 3. Apply the parameter to the synthetic_token minting policy
/// 4. Deploy the resulting script as a reference script UTXO with a marker NFT
async fn deploy_minting_ref(
    ctx: &CliContext,
    warp_policy: &str,
    dry_run: bool,
) -> Result<()> {
    println!("\n{}", "═══════════════════════════════════════════════════════════════".cyan());
    println!("{}", "Deploying Synthetic Minting Policy Reference Script".cyan().bold());
    println!("{}", "═══════════════════════════════════════════════════════════════".cyan());

    println!("\n{}", "Configuration:".green());
    println!("  Warp Route NFT Policy: {}", warp_policy);

    // Load deployment info
    let deployment = ctx.load_deployment_info()?;
    let mailbox = deployment.mailbox.as_ref()
        .ok_or_else(|| anyhow!("Mailbox not deployed. Run 'hyperlane-cardano init' first"))?;
    // Verify mailbox has state NFT policy (for validation, not needed by this command)
    let _mailbox_policy_id = mailbox.state_nft_policy.as_ref()
        .ok_or_else(|| anyhow!("Mailbox state NFT policy not found in deployment"))?;

    // Load API and signing key
    let api_key = ctx.require_api_key()?;
    let keypair = ctx.load_signing_key()?;
    let payer_address = keypair.address_bech32(ctx.pallas_network());

    println!("  Payer: {}", payer_address);

    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    // Step 1: Find the warp route UTXO
    println!("\n{}", "Step 1: Finding warp route UTXO...".cyan());
    let warp_utxo = client.find_utxo_by_asset(warp_policy, "").await?
        .ok_or_else(|| anyhow!("Could not find warp route UTXO with policy {}", warp_policy))?;
    println!("  Found: {}#{}", warp_utxo.tx_hash, warp_utxo.output_index);

    // Step 2: Extract warp route script hash from address
    println!("\n{}", "Step 2: Extracting warp route info...".cyan());
    let warp_address = pallas_addresses::Address::from_bech32(&warp_utxo.address)
        .map_err(|e| anyhow!("Invalid warp route address: {:?}", e))?;

    // Extract script hash from Shelley address
    // Shelley script address format: header(1 byte) + script_hash(28 bytes) + optional_staking
    // Header 0x70/0xF0 (testnet) or 0x71/0xF1 (mainnet) indicates script payment credential
    let warp_route_hash = match &warp_address {
        pallas_addresses::Address::Shelley(_) => {
            // Get the payment credential from the address
            // We need to check if it's a script hash
            let addr_bytes = warp_address.to_vec();
            if addr_bytes.len() < 29 {
                return Err(anyhow!("Address too short to contain script hash"));
            }
            // Check header byte - 0x70, 0x71, 0xF0, 0xF1 indicate script payment credential
            let header = addr_bytes[0];
            let is_script = (header & 0x10) == 0x10 || (header & 0xF0) == 0x70 || (header & 0xF0) == 0xF0;
            if !is_script {
                return Err(anyhow!("Warp route address is not a script address (header: 0x{:02x})", header));
            }
            // Script hash is bytes 1-29
            hex::encode(&addr_bytes[1..29])
        }
        _ => return Err(anyhow!("Warp route address is not a Shelley address")),
    };
    println!("  Warp Route Script Hash: {}", warp_route_hash);

    // Step 3: Parse datum to verify it's a synthetic warp route and get minting policy
    println!("\n{}", "Step 3: Verifying synthetic warp route...".cyan());
    let datum_json = warp_utxo.inline_datum.as_ref()
        .ok_or_else(|| anyhow!("Warp route UTXO has no inline datum"))?;

    // Blockfrost returns datum as hex-encoded CBOR string
    let datum_hex = datum_json.as_str()
        .ok_or_else(|| anyhow!("Datum is not a hex string"))?;
    let datum_cbor = hex::decode(datum_hex)
        .map_err(|e| anyhow!("Failed to decode datum hex: {}", e))?;

    use pallas_codec::minicbor;
    let datum: PlutusData = minicbor::decode(&datum_cbor)
        .map_err(|e| anyhow!("Failed to decode warp route datum: {}", e))?;

    // Extract token_type from datum.config.token_type
    // Datum structure: Constr 0 [config, owner, total_bridged]
    // Config structure: Constr (tag) [token_type, decimals, remote_decimals, remote_routes]
    // Token type: Constr 121 (Collateral), Constr 122 (Synthetic), Constr 123 (Native)
    let (token_type_tag, minting_policy) = if let PlutusData::Constr(datum_constr) = &datum {
        let fields = datum_constr.fields.clone().to_vec();
        if fields.is_empty() {
            return Err(anyhow!("Invalid warp route datum: no fields"));
        }
        if let PlutusData::Constr(config_constr) = &fields[0] {
            let config_fields = config_constr.fields.clone().to_vec();
            if config_fields.is_empty() {
                return Err(anyhow!("Invalid warp route config: no fields"));
            }
            if let PlutusData::Constr(tt_constr) = &config_fields[0] {
                match tt_constr.tag {
                    122 => {
                        // Synthetic { minting_policy }
                        let tt_fields = tt_constr.fields.clone().to_vec();
                        if tt_fields.is_empty() {
                            return Err(anyhow!("Synthetic token type has no minting_policy"));
                        }
                        let policy = extract_bytes_hex_for_transfer(&tt_fields[0])?;
                        (122u64, policy)
                    }
                    tag => return Err(anyhow!(
                        "Warp route is not a Synthetic type (tag={}, expected 122). \
                         Only Synthetic warp routes need minting policy reference scripts.",
                        tag
                    )),
                }
            } else {
                return Err(anyhow!("Invalid token_type in config"));
            }
        } else {
            return Err(anyhow!("Invalid config field in datum"));
        }
    } else {
        return Err(anyhow!("Invalid warp route datum structure"));
    };

    println!("  Token Type: Synthetic (tag={})", token_type_tag);
    println!("  Minting Policy ID: {}", minting_policy);

    // Step 4: Compute the minting policy script
    println!("\n{}", "Step 4: Computing minting policy script...".cyan());
    let warp_hash_param = encode_script_hash_param(&warp_route_hash)?;
    let mint_policy_applied = apply_validator_param(
        &ctx.contracts_dir,
        "synthetic_token",
        "synthetic_token",
        &hex::encode(&warp_hash_param),
    )?;

    // Verify the computed policy ID matches the one in the datum
    if mint_policy_applied.policy_id != minting_policy {
        return Err(anyhow!(
            "Computed minting policy ({}) does not match datum minting policy ({}). \
             This indicates a parameter mismatch.",
            mint_policy_applied.policy_id,
            minting_policy
        ));
    }
    println!("  ✓ Computed minting policy matches datum");

    let mint_script = hex::decode(&mint_policy_applied.compiled_code)
        .with_context(|| "Invalid minting policy script CBOR")?;
    println!("  Minting Policy Script Size: {} bytes", mint_script.len());

    // Step 5: Find UTXOs for deployment
    println!("\n{}", "Step 5: Finding UTXOs for deployment...".cyan());
    let utxos = client.get_utxos(&payer_address).await?;
    println!("  Found {} UTXOs", utxos.len());

    // Need one UTXO for the reference script deployment
    // Exclude UTXOs with assets, reference scripts, or inline datums
    let min_ada = 15_000_000u64; // 15 ADA to cover reference script UTXO
    let suitable_utxos: Vec<_> = utxos.iter()
        .filter(|u| {
            u.lovelace >= min_ada
                && u.assets.is_empty()
                && u.reference_script.is_none()
                && u.inline_datum.is_none()
                && u.datum_hash.is_none()
        })
        .collect();

    if suitable_utxos.is_empty() {
        return Err(anyhow!(
            "Need at least 1 UTXO with >= 15 ADA. Found none. \
             Please fund the wallet with more ADA."
        ));
    }

    let deploy_input = suitable_utxos[0];
    println!("  Deploy Input: {}#{}", deploy_input.tx_hash, deploy_input.output_index);

    // Step 6: Compute one-shot NFT policy for marker
    println!("\n{}", "Step 6: Computing marker NFT policy...".cyan());
    let nft_output_ref = encode_output_reference(&deploy_input.tx_hash, deploy_input.output_index)?;
    let nft_applied = apply_validator_param(
        &ctx.contracts_dir,
        "state_nft",
        "state_nft",
        &hex::encode(&nft_output_ref),
    )?;
    let nft_policy_id = &nft_applied.policy_id;
    let nft_script = hex::decode(&nft_applied.compiled_code)
        .with_context(|| "Invalid NFT script CBOR")?;
    println!("  Marker NFT Policy: {}", nft_policy_id);

    if dry_run {
        println!("\n{}", "═══════════════════════════════════════════════════════════════".yellow());
        println!("{}", "[Dry run - not submitting transaction]".yellow());
        println!("{}", "═══════════════════════════════════════════════════════════════".yellow());
        println!("\nDeployment would create:");
        println!("  - Minting Policy Reference Script UTXO");
        println!("  - NFT Policy: {}", nft_policy_id);
        println!("  - NFT Asset Name: mint_ref (6d696e745f726566)");
        println!("  - Script Hash: {}", minting_policy);
        return Ok(());
    }

    // Step 7: Build and submit transaction
    println!("\n{}", "Step 7: Deploying minting policy reference script...".cyan());

    use pallas_addresses::Network;
    use pallas_crypto::hash::Hash;
    use pallas_txbuilder::{BuildConway, ExUnits, Input, Output, ScriptKind, StagingTransaction};

    let current_slot = client.get_latest_slot().await?;
    let validity_end = current_slot + 7200; // ~2 hours

    let cost_model = client.get_plutusv3_cost_model().await?;

    // Parse input
    let input_tx_hash: [u8; 32] = hex::decode(&deploy_input.tx_hash)?
        .try_into()
        .map_err(|_| anyhow!("Invalid input tx hash"))?;

    let payer_addr = pallas_addresses::Address::from_bech32(&payer_address)
        .map_err(|e| anyhow!("Invalid payer address: {}", e))?;

    let input = Input::new(Hash::new(input_tx_hash), deploy_input.output_index as u64);
    let collateral = input.clone(); // Use same UTXO for collateral

    // NFT asset name: "mint_ref" = 0x6d696e745f726566
    let nft_asset_name: Vec<u8> = b"mint_ref".to_vec();

    // Calculate reference script UTXO ADA (needs to cover script size)
    let base_ref_script_lovelace = 12_000_000u64; // 12 ADA for ~4KB script
    let fee_estimate = 2_500_000u64;
    let change = deploy_input.lovelace.saturating_sub(base_ref_script_lovelace).saturating_sub(fee_estimate);

    // If change is below min UTXO, add it to the ref script output to conserve value
    let min_utxo = 1_500_000u64;
    let (ref_script_lovelace, actual_change) = if change >= min_utxo {
        (base_ref_script_lovelace, change)
    } else {
        // Add the dust change to the ref script output
        (base_ref_script_lovelace + change, 0)
    };

    // Build reference script output at payer address with NFT and script attached
    let nft_policy_hash: [u8; 28] = hex::decode(nft_policy_id)?
        .try_into()
        .map_err(|_| anyhow!("Invalid NFT policy ID"))?;

    let ref_script_output = Output::new(payer_addr.clone(), ref_script_lovelace)
        .add_asset(Hash::new(nft_policy_hash), nft_asset_name.clone(), 1)
        .map_err(|e| anyhow!("Failed to add NFT: {:?}", e))?
        .set_inline_script(ScriptKind::PlutusV3, mint_script);

    // Build mint redeemer (MintOnce = Constr 0 [])
    let mint_redeemer_cbor = build_mint_redeemer();

    let mut staging = StagingTransaction::new()
        .input(input)
        .collateral_input(collateral)
        .output(ref_script_output)
        .mint_asset(Hash::new(nft_policy_hash), nft_asset_name, 1)
        .map_err(|e| anyhow!("Failed to add NFT mint: {:?}", e))?
        .script(ScriptKind::PlutusV3, nft_script)
        .add_mint_redeemer(
            Hash::new(nft_policy_hash),
            mint_redeemer_cbor,
            Some(ExUnits { mem: 1_000_000, steps: 500_000_000 }),
        )
        .language_view(ScriptKind::PlutusV3, cost_model)
        .fee(fee_estimate)
        .invalid_from_slot(validity_end)
        .network_id(if matches!(ctx.pallas_network(), Network::Testnet) { 0 } else { 1 });

    // Add change output if there's enough
    if actual_change >= min_utxo {
        staging = staging.output(Output::new(payer_addr.clone(), actual_change));
    }

    // Add required signer
    let payer_hash: [u8; 28] = keypair.verification_key_hash();
    staging = staging.disclosed_signer(Hash::new(payer_hash));

    // Build the transaction
    let tx = staging.build_conway_raw()
        .map_err(|e| anyhow!("Failed to build transaction: {:?}", e))?;

    println!("  TX Hash: {}", hex::encode(&tx.tx_hash.0));

    // Sign the transaction
    let tx_hash_bytes: &[u8] = &tx.tx_hash.0;
    let signature = keypair.sign(tx_hash_bytes);
    let signed_tx = tx.add_signature(keypair.pallas_public_key().clone(), signature)
        .map_err(|e| anyhow!("Failed to sign transaction: {:?}", e))?;

    // Submit the transaction
    println!("  Submitting transaction...");
    let tx_hash = client.submit_tx(&signed_tx.tx_bytes.0).await?;
    println!("  ✓ Transaction submitted: {}", tx_hash.green());

    // Wait for confirmation
    println!("\n{}", "Waiting for confirmation...".cyan());
    client.wait_for_tx(&tx_hash, 120).await?;
    println!("  ✓ Transaction confirmed");

    // Build reference script UTXO info
    let mint_ref_utxo = format!("{}#0", tx_hash);

    // Update deployment info
    if let Ok(mut updated_deployment) = ctx.load_deployment_info() {
        // Find the matching warp route and add the minting ref script info
        for warp_route in updated_deployment.warp_routes.iter_mut() {
            if warp_route.nft_policy == warp_policy {
                warp_route.minting_ref_script_utxo = Some(crate::utils::types::ReferenceScriptUtxo {
                    tx_hash: tx_hash.clone(),
                    output_index: 0,
                    lovelace: ref_script_lovelace,
                });
                break;
            }
        }
        if let Err(e) = ctx.save_deployment_info(&updated_deployment) {
            println!("  Warning: Failed to update deployment_info.json: {}", e);
        }
    }

    println!("\n{}", "═══════════════════════════════════════════════════════════════".green());
    println!("{}", "Minting Policy Reference Script Deployed!".green().bold());
    println!("{}", "═══════════════════════════════════════════════════════════════".green());
    println!();
    println!("{}", "Reference Script UTXO:".cyan());
    println!("  TX Hash: {}", tx_hash);
    println!("  Output Index: 0");
    println!("  UTXO: {}", mint_ref_utxo);
    println!("  NFT Policy: {}", nft_policy_id);
    println!("  NFT Asset: mint_ref");
    println!();
    println!("{}", "Minting Policy:".cyan());
    println!("  Policy ID: {}", minting_policy);
    println!("  Script Size: {} bytes", hex::decode(&mint_policy_applied.compiled_code)?.len());
    println!();
    println!("{}", "═══════════════════════════════════════════════════════════════".green());
    println!("{}", "Next steps:".yellow());
    println!("{}", "═══════════════════════════════════════════════════════════════".green());
    println!();
    println!("1. Update the registry to include this as an additional_input:");
    println!("   hyperlane-cardano registry update \\");
    println!("     --script-hash {} \\", warp_route_hash);
    println!("     --additional-input mint_ref:{}:6d696e745f726566:false", nft_policy_id);
    println!();
    println!("   The 'false' means must_spend=false (use as reference input).");
    println!();

    Ok(())
}


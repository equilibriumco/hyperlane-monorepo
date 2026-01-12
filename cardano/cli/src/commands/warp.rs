//! Warp command - Manage warp routes (token bridges)

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand, ValueEnum};
use colored::Colorize;

use crate::utils::blockfrost::BlockfrostClient;
use crate::utils::context::CliContext;

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

        /// Token decimals
        #[arg(long, default_value = "6")]
        decimals: u8,

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

    /// Initialize vault for collateral warp route
    InitVault {
        /// Token policy ID
        #[arg(long)]
        token_policy: String,

        /// Token asset name
        #[arg(long, default_value = "")]
        token_asset: String,

        /// Warp route script hash
        #[arg(long)]
        warp_hash: String,

        /// Dry run
        #[arg(long)]
        dry_run: bool,
    },

    /// Query vault balance
    VaultBalance {
        /// Vault policy ID
        #[arg(long)]
        vault_policy: Option<String>,
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
            dry_run,
        } => deploy(ctx, token_type, token_policy, token_asset, decimals, dry_run).await,
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
        WarpCommands::InitVault {
            token_policy,
            token_asset,
            warp_hash,
            dry_run,
        } => init_vault(ctx, &token_policy, &token_asset, &warp_hash, dry_run).await,
        WarpCommands::VaultBalance { vault_policy } => vault_balance(ctx, vault_policy).await,
    }
}

async fn deploy(
    _ctx: &CliContext,
    token_type: TokenType,
    token_policy: Option<String>,
    token_asset: Option<String>,
    decimals: u8,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Deploying warp route...".cyan());

    let type_str = match token_type {
        TokenType::Native => "Native (ADA)",
        TokenType::Collateral => "Collateral (Lock existing tokens)",
        TokenType::Synthetic => "Synthetic (Mint new tokens)",
    };

    println!("  Token Type: {}", type_str);
    println!("  Decimals: {}", decimals);

    match token_type {
        TokenType::Native => {
            println!("  Note: Native ADA warp route will lock ADA in vault");
        }
        TokenType::Collateral => {
            let policy = token_policy.ok_or_else(|| anyhow!("--token-policy required for collateral type"))?;
            let asset = token_asset.unwrap_or_default();
            println!("  Token Policy: {}", policy);
            println!("  Token Asset: {}", if asset.is_empty() { "(empty)" } else { &asset });
        }
        TokenType::Synthetic => {
            println!("  Note: Synthetic warp route will mint/burn synthetic tokens");
        }
    }

    if dry_run {
        println!("\n{}", "[Dry run - not deploying]".yellow());
        return Ok(());
    }

    println!("\n{}", "Manual Deployment Required:".yellow().bold());
    println!("Warp route deployment requires:");
    println!("1. Create state NFT for warp route");
    println!("2. Initialize warp route with config datum");
    println!("3. For collateral type: also deploy vault contract");
    println!("4. For synthetic type: also deploy minting policy");
    println!("\nSee cardano/scripts/ for reference implementations");

    Ok(())
}

async fn enroll_router(
    ctx: &CliContext,
    domain: u32,
    router: &str,
    warp_policy: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Enrolling remote router...".cyan());

    let router = router.strip_prefix("0x").unwrap_or(router);
    if router.len() != 64 {
        return Err(anyhow!("Router address must be 32 bytes (64 hex chars)"));
    }

    println!("  Domain: {}", domain);
    println!("  Router: 0x{}", router);

    let policy_id = get_warp_policy(ctx, warp_policy)?;
    println!("  Warp Policy: {}", policy_id);

    // Build EnrollRemoteRoute redeemer
    let redeemer_json = serde_json::json!({
        "constructor": 2, // EnrollRemoteRoute
        "fields": [
            {"int": domain},
            {"bytes": router}
        ]
    });

    println!("\n{}", "EnrollRemoteRoute Redeemer:".green());
    println!("{}", serde_json::to_string_pretty(&redeemer_json)?);

    if dry_run {
        println!("\n{}", "[Dry run - not submitting]".yellow());
        return Ok(());
    }

    println!("\n{}", "Manual Transaction Required:".yellow().bold());
    println!("Build a transaction that spends the warp route UTXO");
    println!("with EnrollRemoteRoute redeemer and updated datum");

    Ok(())
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
        // Parse remote_routes from datum
        if let Some(fields) = datum.get("fields").and_then(|f| f.as_array()) {
            // config is first field, remote_routes is inside config
            if let Some(config_fields) = fields.get(0)
                .and_then(|c| c.get("fields"))
                .and_then(|f| f.as_array())
            {
                if let Some(routes) = config_fields.get(2)
                    .and_then(|r| r.get("list"))
                    .and_then(|l| l.as_array())
                {
                    println!("\n{}", "Remote Routers:".green());
                    println!("{}", "-".repeat(80));

                    for route in routes {
                        if let Some(route_fields) = route.get("fields").and_then(|f| f.as_array()) {
                            let domain = route_fields.get(0)
                                .and_then(|d| d.get("int"))
                                .and_then(|i| i.as_u64());
                            let router = route_fields.get(1)
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
    println!("{}", "Initiating token transfer...".cyan());

    // Automatically pad shorter addresses (e.g., 20-byte ETH, 28-byte Cardano) to 32 bytes
    let recipient_hex = recipient.strip_prefix("0x").unwrap_or(recipient);
    if recipient_hex.len() > 64 {
        return Err(anyhow!("Recipient too long: {} chars (max 64)", recipient_hex.len()));
    }
    // Left-pad with zeros to 64 hex chars (32 bytes)
    let recipient = format!("{:0>64}", recipient_hex);

    println!("  Destination Domain: {}", domain);
    println!("  Recipient: 0x{}", recipient);
    println!("  Amount: {}", amount);

    let policy_id = get_warp_policy(ctx, warp_policy)?;
    println!("  Warp Policy: {}", policy_id);

    // Build TransferRemote redeemer
    let redeemer_json = serde_json::json!({
        "constructor": 0, // TransferRemote
        "fields": [
            {"int": domain},
            {"bytes": recipient},
            {"int": amount}
        ]
    });

    println!("\n{}", "TransferRemote Redeemer:".green());
    println!("{}", serde_json::to_string_pretty(&redeemer_json)?);

    if dry_run {
        println!("\n{}", "[Dry run - not submitting]".yellow());
        return Ok(());
    }

    println!("\n{}", "Manual Transaction Required:".yellow().bold());
    println!("Build a transaction that:");
    println!("1. Spends the warp route UTXO with TransferRemote redeemer");
    println!("2. Spends the mailbox UTXO with Dispatch redeemer");
    println!("3. For collateral: deposits tokens to vault");
    println!("4. For synthetic: burns synthetic tokens");
    println!("5. Creates updated warp route and mailbox UTXOs");

    Ok(())
}

async fn init_vault(
    ctx: &CliContext,
    token_policy: &str,
    token_asset: &str,
    warp_hash: &str,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Initializing vault...".cyan());

    println!("  Token Policy: {}", token_policy);
    println!("  Token Asset: {}", if token_asset.is_empty() { "(empty)" } else { token_asset });
    println!("  Warp Route Hash: {}", warp_hash);

    let keypair = ctx.load_signing_key()?;
    let owner_pkh = keypair.verification_key_hash_hex();

    // Build vault datum
    let datum_json = serde_json::json!({
        "constructor": 0,
        "fields": [
            {"bytes": warp_hash},
            {"bytes": owner_pkh},
            {"constructor": 0, "fields": [
                {"bytes": token_policy},
                {"bytes": token_asset}
            ]},
            {"int": 0}
        ]
    });

    println!("\n{}", "Vault Datum:".green());
    println!("{}", serde_json::to_string_pretty(&datum_json)?);

    if dry_run {
        println!("\n{}", "[Dry run - not submitting]".yellow());
        return Ok(());
    }

    println!("\n{}", "Manual Deployment Required:".yellow().bold());
    println!("Deploy the vault similarly to other contracts:");
    println!("1. Create state NFT for vault");
    println!("2. Initialize vault with datum at vault script address");

    Ok(())
}

async fn vault_balance(ctx: &CliContext, vault_policy: Option<String>) -> Result<()> {
    println!("{}", "Querying vault balance...".cyan());

    let policy_id = match vault_policy {
        Some(p) => p,
        None => {
            let deployment = ctx.load_deployment_info()?;
            deployment
                .vault
                .and_then(|v| v.state_nft_policy)
                .ok_or_else(|| anyhow!("Vault policy not found"))?
        }
    };

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let vault_utxo = client
        .find_utxo_by_asset(&policy_id, "")
        .await?
        .ok_or_else(|| anyhow!("Vault UTXO not found with policy {}", policy_id))?;

    println!("\n{}", "Vault UTXO:".green());
    println!("  TX: {}#{}", vault_utxo.tx_hash, vault_utxo.output_index);
    println!("  Address: {}", vault_utxo.address);
    println!("  Lovelace: {} ({:.6} ADA)", vault_utxo.lovelace, vault_utxo.lovelace as f64 / 1_000_000.0);

    if !vault_utxo.assets.is_empty() {
        println!("\n  Assets:");
        for asset in &vault_utxo.assets {
            println!(
                "    {} {} ({})",
                asset.quantity,
                if asset.asset_name.is_empty() { "(no name)" } else { &asset.asset_name },
                &asset.policy_id[..16]
            );
        }
    }

    if let Some(datum) = &vault_utxo.inline_datum {
        if let Some(fields) = datum.get("fields").and_then(|f| f.as_array()) {
            if let Some(total_locked) = fields.get(3).and_then(|t| t.get("int")).and_then(|i| i.as_i64()) {
                println!("\n  Total Locked (from datum): {}", total_locked);
            }
        }
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

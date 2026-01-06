//! Config command - Update relayer configuration from deployment info

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};
use colored::Colorize;
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::utils::context::CliContext;
use crate::utils::plutus::{apply_validator_param_with_purpose, encode_script_hash_param};

#[derive(Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommands,
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Update relayer config with deployment info
    UpdateRelayer {
        /// Path to relayer-config.json (default: ../config/relayer-config.json)
        #[arg(long)]
        config_path: Option<String>,

        /// Cardano chain name in the config (default: cardano<network>, e.g., cardanopreview)
        #[arg(long)]
        chain_name: Option<String>,

        /// Dry run - show changes without writing
        #[arg(long)]
        dry_run: bool,
    },

    /// Generate validator config from deployment info
    UpdateValidator {
        /// Path to validator-config.json (default: ../config/validator-config.json)
        #[arg(long)]
        config_path: Option<String>,

        /// Cardano chain name in the config (default: cardano<network>, e.g., cardanopreview)
        #[arg(long)]
        chain_name: Option<String>,

        /// Validator signing key (hex format, or use VALIDATOR_HEX_KEY env var)
        #[arg(long, env = "VALIDATOR_HEX_KEY")]
        validator_key: Option<String>,

        /// Path to checkpoint storage directory (default: ./signatures)
        #[arg(long, default_value = "./signatures")]
        checkpoint_path: String,

        /// Database path for validator state (default: /tmp/hyperlane-validator-db)
        #[arg(long, default_value = "/tmp/hyperlane-validator-db")]
        db_path: String,

        /// Metrics port (default: 9091)
        #[arg(long, default_value = "9091")]
        metrics_port: u16,

        /// Block to start indexing from (default: uses deployment init block)
        #[arg(long)]
        index_from: Option<u64>,

        /// Dry run - show changes without writing
        #[arg(long)]
        dry_run: bool,
    },

    /// Show current Cardano configuration from relayer config
    Show {
        /// Path to relayer-config.json
        #[arg(long)]
        config_path: Option<String>,

        /// Cardano chain name in the config
        #[arg(long)]
        chain_name: Option<String>,
    },
}

pub async fn execute(ctx: &CliContext, args: ConfigArgs) -> Result<()> {
    match args.command {
        ConfigCommands::UpdateRelayer {
            config_path,
            chain_name,
            dry_run,
        } => update_relayer(ctx, config_path, chain_name, dry_run).await,
        ConfigCommands::UpdateValidator {
            config_path,
            chain_name,
            validator_key,
            checkpoint_path,
            db_path,
            metrics_port,
            index_from,
            dry_run,
        } => update_validator(ctx, config_path, chain_name, validator_key, checkpoint_path, db_path, metrics_port, index_from, dry_run).await,
        ConfigCommands::Show {
            config_path,
            chain_name,
        } => show_config(ctx, config_path, chain_name).await,
    }
}

fn get_config_path(ctx: &CliContext, config_path: Option<String>, filename: &str) -> PathBuf {
    config_path
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // Default: go up from deployments dir and into config
            ctx.deployments_dir
                .parent()
                .unwrap_or(&ctx.deployments_dir)
                .join("config")
                .join(filename)
        })
}

fn get_relayer_config_path(ctx: &CliContext, config_path: Option<String>) -> PathBuf {
    get_config_path(ctx, config_path, "relayer-config.json")
}

fn get_validator_config_path(ctx: &CliContext, config_path: Option<String>) -> PathBuf {
    get_config_path(ctx, config_path, "validator-config.json")
}

fn get_chain_name(ctx: &CliContext, chain_name: Option<String>) -> String {
    chain_name.unwrap_or_else(|| format!("cardano{}", ctx.network()))
}

async fn update_relayer(
    ctx: &CliContext,
    config_path: Option<String>,
    chain_name: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Updating relayer configuration...".cyan());

    // Load deployment info
    let deployment = ctx.load_deployment_info()?;
    println!("  Loaded deployment info for network: {}", deployment.network);

    let config_file = get_relayer_config_path(ctx, config_path);
    let chain = get_chain_name(ctx, chain_name);

    println!("  Config file: {:?}", config_file);
    println!("  Chain name: {}", chain);

    // Load or create config
    let mut config: Value = if config_file.exists() {
        let content = std::fs::read_to_string(&config_file)
            .with_context(|| format!("Failed to read config file: {:?}", config_file))?;
        serde_json::from_str(&content)
            .with_context(|| "Failed to parse relayer config")?
    } else {
        println!("  {} Config file does not exist, creating new one", "[NEW]".yellow());
        create_default_config()
    };

    // Ensure chains object exists
    if config.get("chains").is_none() {
        config["chains"] = json!({});
    }

    // Get or create the chain config
    let chains = config["chains"].as_object_mut()
        .ok_or_else(|| anyhow!("chains must be an object"))?;

    if !chains.contains_key(&chain) {
        println!("  {} Chain '{}' does not exist, creating", "[NEW]".yellow(), chain);
        chains.insert(chain.clone(), create_default_cardano_chain(&chain, ctx.network()));
    }

    let chain_config = chains.get_mut(&chain)
        .ok_or_else(|| anyhow!("Chain config not found"))?;

    // Update the connection object
    if chain_config.get("connection").is_none() {
        chain_config["connection"] = json!({});
    }

    let connection = chain_config["connection"].as_object_mut()
        .ok_or_else(|| anyhow!("connection must be an object"))?;

    println!("\n{}", "Updating Cardano configuration:".green());

    // Helper to get old value before updating
    fn get_old_value(connection: &serde_json::Map<String, Value>, key: &str) -> String {
        connection.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("(not set)")
            .to_string()
    }

    // Collect updates to apply
    let mut connection_updates: Vec<(String, Value, String, String)> = Vec::new(); // (key, value, old_display, new_display)
    let mut chain_updates: Vec<(String, Value, String)> = Vec::new(); // (key, value, display)

    // Update mailbox info
    if let Some(ref mailbox) = deployment.mailbox {
        let old = get_old_value(connection, "mailboxScriptHash");
        connection_updates.push((
            "mailboxScriptHash".to_string(),
            json!(mailbox.hash.clone()),
            old,
            mailbox.hash.clone(),
        ));

        if let Some(ref nft) = mailbox.state_nft {
            let old = get_old_value(connection, "mailboxPolicyId");
            connection_updates.push((
                "mailboxPolicyId".to_string(),
                json!(nft.policy_id.clone()),
                old,
                nft.policy_id.clone(),
            ));

            // Add mailbox asset name hex for NFT lookup
            let old = get_old_value(connection, "mailboxAssetNameHex");
            connection_updates.push((
                "mailboxAssetNameHex".to_string(),
                json!(nft.asset_name_hex.clone()),
                old,
                nft.asset_name_hex.clone(),
            ));
        }

        // Update mailbox reference script UTXO
        if let Some(ref ref_utxo) = mailbox.reference_script_utxo {
            let ref_utxo_str = format!("{}#{}", ref_utxo.tx_hash, ref_utxo.output_index);
            let old = get_old_value(connection, "mailboxReferenceScriptUtxo");
            connection_updates.push((
                "mailboxReferenceScriptUtxo".to_string(),
                json!(ref_utxo_str.clone()),
                old,
                ref_utxo_str,
            ));
        }

        // Update Hyperlane-format mailbox address (0x02000000 prefix for Cardano)
        let hyperlane_mailbox = format!("0x02000000{}", mailbox.hash);
        chain_updates.push(("mailbox".to_string(), json!(hyperlane_mailbox.clone()), hyperlane_mailbox));
    }

    // Update ISM info
    if let Some(ref ism) = deployment.ism {
        let old = get_old_value(connection, "ismScriptHash");
        connection_updates.push((
            "ismScriptHash".to_string(),
            json!(ism.hash.clone()),
            old,
            ism.hash.clone(),
        ));

        if let Some(ref nft) = ism.state_nft {
            let old = get_old_value(connection, "ismPolicyId");
            connection_updates.push((
                "ismPolicyId".to_string(),
                json!(nft.policy_id.clone()),
                old,
                nft.policy_id.clone(),
            ));

            // Add ISM asset name hex for NFT lookup
            let old = get_old_value(connection, "ismAssetNameHex");
            connection_updates.push((
                "ismAssetNameHex".to_string(),
                json!(nft.asset_name_hex.clone()),
                old,
                nft.asset_name_hex.clone(),
            ));
        }

        // Update ISM reference script UTXO
        if let Some(ref ref_utxo) = ism.reference_script_utxo {
            let ref_utxo_str = format!("{}#{}", ref_utxo.tx_hash, ref_utxo.output_index);
            let old = get_old_value(connection, "ismReferenceScriptUtxo");
            connection_updates.push((
                "ismReferenceScriptUtxo".to_string(),
                json!(ref_utxo_str.clone()),
                old,
                ref_utxo_str,
            ));
        }

        // Update Hyperlane-format ISM address
        let hyperlane_ism = format!("0x02000000{}", ism.hash);
        chain_updates.push(("interchainSecurityModule".to_string(), json!(hyperlane_ism.clone()), hyperlane_ism));
    }

    // Update registry info
    if let Some(ref registry) = deployment.registry {
        if let Some(ref nft) = registry.state_nft {
            let old = get_old_value(connection, "registryPolicyId");
            connection_updates.push((
                "registryPolicyId".to_string(),
                json!(nft.policy_id.clone()),
                old,
                nft.policy_id.clone(),
            ));

            // Add registry asset name hex for NFT lookup
            let old = get_old_value(connection, "registryAssetNameHex");
            connection_updates.push((
                "registryAssetNameHex".to_string(),
                json!(nft.asset_name_hex.clone()),
                old,
                nft.asset_name_hex.clone(),
            ));
        }

        // The registry script hash is also used as processedMessagesScriptHash
        let old = get_old_value(connection, "processedMessagesScriptHash");
        connection_updates.push((
            "processedMessagesScriptHash".to_string(),
            json!(registry.hash.clone()),
            old,
            registry.hash.clone(),
        ));
    }

    // Generate processed_message_nft policy (parameterized with mailbox_policy_id)
    // This is used for efficient O(1) processed message lookups
    // IMPORTANT: We use mailbox_policy_id (state NFT policy, stable) NOT mailbox.hash (script hash, changes with code)
    if let Some(ref mailbox) = deployment.mailbox {
        println!("\n{}", "Generating processed_message_nft policy...".cyan());

        // Get the mailbox_policy_id (state NFT policy) - this is stable across upgrades
        let mailbox_policy_id = mailbox.state_nft.as_ref()
            .map(|nft| nft.policy_id.clone())
            .or_else(|| mailbox.state_nft_policy.clone())
            .ok_or_else(|| anyhow!("Mailbox state NFT policy not found. Ensure mailbox is initialized."))?;

        // Apply mailbox_policy_id parameter to processed_message_nft validator
        let mailbox_policy_param = encode_script_hash_param(&mailbox_policy_id)
            .with_context(|| "Failed to encode mailbox_policy_id as CBOR")?;
        let mailbox_policy_param_hex = hex::encode(&mailbox_policy_param);

        match apply_validator_param_with_purpose(
            &ctx.contracts_dir,
            "processed_message_nft",
            "processed_message_nft",
            Some("mint"),
            &mailbox_policy_param_hex,
        ) {
            Ok(applied) => {
                println!("  Applied mailbox_policy_id parameter: {}", mailbox_policy_id);
                println!("  Resulting policy ID: {}", applied.policy_id.green());

                let old_policy = get_old_value(connection, "processedMessagesNftPolicyId");
                connection_updates.push((
                    "processedMessagesNftPolicyId".to_string(),
                    json!(applied.policy_id.clone()),
                    old_policy,
                    applied.policy_id.clone(),
                ));

                let old_cbor = get_old_value(connection, "processedMessagesNftScriptCbor");
                let cbor_display = if applied.compiled_code.len() > 40 {
                    format!("{}...", &applied.compiled_code[..40])
                } else {
                    applied.compiled_code.clone()
                };
                connection_updates.push((
                    "processedMessagesNftScriptCbor".to_string(),
                    json!(applied.compiled_code.clone()),
                    if old_cbor.len() > 40 { format!("{}...", &old_cbor[..40]) } else { old_cbor },
                    cbor_display,
                ));
            }
            Err(e) => {
                println!("  {} Failed to apply processed_message_nft parameter: {}", "[WARN]".yellow(), e);
                println!("  {} NFT-based message lookup will not be available", "[WARN]".yellow());
            }
        }
    }

    // Update IGP info
    if let Some(ref igp) = deployment.igp {
        if let Some(ref nft) = igp.state_nft {
            let old = get_old_value(connection, "igpPolicyId");
            connection_updates.push((
                "igpPolicyId".to_string(),
                json!(nft.policy_id.clone()),
                old,
                nft.policy_id.clone(),
            ));
        }
    }

    // Update validator announce info
    if let Some(ref va) = deployment.validator_announce {
        if let Some(ref nft) = va.state_nft {
            let old = get_old_value(connection, "validatorAnnouncePolicyId");
            connection_updates.push((
                "validatorAnnouncePolicyId".to_string(),
                json!(nft.policy_id.clone()),
                old,
                nft.policy_id.clone(),
            ));
        }
    }

    // Update network
    connection_updates.push((
        "network".to_string(),
        json!(ctx.network()),
        get_old_value(connection, "network"),
        ctx.network().to_string(),
    ));

    // Apply connection updates and print
    for (key, value, old, new) in connection_updates {
        connection.insert(key.clone(), value);
        if old != new {
            println!("  {}: {} -> {}", key, old.dimmed(), new.green());
        } else {
            println!("  {}: {} (unchanged)", key, new.dimmed());
        }
    }

    // Apply chain-level updates
    for (key, value, display) in chain_updates {
        chain_config[&key] = value;
        println!("  {} (Hyperlane): {}", key, display.green());
    }

    if dry_run {
        println!("\n{}", "[Dry run - not writing changes]".yellow());
        println!("\nResulting config for chain '{}':", chain);
        println!("{}", serde_json::to_string_pretty(chain_config)?);
    } else {
        // Ensure parent directory exists
        if let Some(parent) = config_file.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config directory: {:?}", parent))?;
        }

        // Write config
        let content = serde_json::to_string_pretty(&config)?;
        std::fs::write(&config_file, content)
            .with_context(|| format!("Failed to write config file: {:?}", config_file))?;

        println!("\n{}", "Configuration updated successfully!".green().bold());
        println!("  File: {:?}", config_file);
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn update_validator(
    ctx: &CliContext,
    config_path: Option<String>,
    chain_name: Option<String>,
    validator_key: Option<String>,
    checkpoint_path: String,
    db_path: String,
    metrics_port: u16,
    index_from: Option<u64>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Generating validator configuration...".cyan());

    // Load deployment info
    let deployment = ctx.load_deployment_info()?;
    println!("  Loaded deployment info for network: {}", deployment.network);

    let config_file = get_validator_config_path(ctx, config_path);
    let chain = get_chain_name(ctx, chain_name.clone());

    println!("  Config file: {:?}", config_file);
    println!("  Chain name: {}", chain);

    // Validate required fields
    let mailbox = deployment.mailbox.as_ref()
        .ok_or_else(|| anyhow!("Mailbox not deployed. Run 'deploy mailbox' first."))?;
    let ism = deployment.ism.as_ref()
        .ok_or_else(|| anyhow!("ISM not deployed. Run 'deploy ism' first."))?;

    // Get or prompt for validator key
    let validator_key = match validator_key {
        Some(key) => {
            // Ensure it has 0x prefix
            if key.starts_with("0x") {
                key
            } else {
                format!("0x{}", key)
            }
        }
        None => {
            println!("\n{}", "No validator key provided!".yellow());
            println!("  Use --validator-key or set VALIDATOR_HEX_KEY environment variable");
            println!("  Example: --validator-key 0x1234...abcd");
            return Err(anyhow!("Validator key is required. Use --validator-key or VALIDATOR_HEX_KEY env var."));
        }
    };

    // Validate key format (should be 64 hex chars + 0x prefix = 66 chars)
    if validator_key.len() != 66 || !validator_key[2..].chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(anyhow!("Invalid validator key format. Expected 32 bytes (64 hex chars) with 0x prefix."));
    }

    println!("\n{}", "Building validator configuration:".green());

    // Build the chain-specific connection config
    let blockfrost_url = match ctx.network() {
        "mainnet" => "https://cardano-mainnet.blockfrost.io/api/v0",
        "preprod" => "https://cardano-preprod.blockfrost.io/api/v0",
        _ => "https://cardano-preview.blockfrost.io/api/v0",
    };

    let domain_id = match ctx.network() {
        "mainnet" => 2001,
        "preprod" => 2002,
        _ => 2003,
    };

    // Determine index_from block
    let index_from_block = index_from.unwrap_or_else(|| {
        // Try to get from mailbox init tx block (would need to query, use 0 as default)
        // For now, use a reasonable default for preview testnet
        match ctx.network() {
            "preview" => 3821995, // Approximate block where contracts were deployed
            "preprod" => 0,
            "mainnet" => 0,
            _ => 0,
        }
    });

    // Build connection object
    let mut connection = json!({
        "url": blockfrost_url,
        "apiKey": "", // Will be set via BLOCKFROST_API_KEY env var
        "network": ctx.network(),
        "mailboxScriptHash": mailbox.hash,
        "ismScriptHash": ism.hash,
    });

    // Add mailbox NFT info
    if let Some(ref nft) = mailbox.state_nft {
        connection["mailboxPolicyId"] = json!(nft.policy_id);
        connection["mailboxAssetNameHex"] = json!(nft.asset_name_hex);
    }

    // Add mailbox reference script UTXO
    if let Some(ref ref_utxo) = mailbox.reference_script_utxo {
        connection["mailboxReferenceScriptUtxo"] = json!(format!("{}#{}", ref_utxo.tx_hash, ref_utxo.output_index));
    }

    // Add ISM NFT info
    if let Some(ref nft) = ism.state_nft {
        connection["ismPolicyId"] = json!(nft.policy_id);
        connection["ismAssetNameHex"] = json!(nft.asset_name_hex);
    }

    // Add ISM reference script UTXO
    if let Some(ref ref_utxo) = ism.reference_script_utxo {
        connection["ismReferenceScriptUtxo"] = json!(format!("{}#{}", ref_utxo.tx_hash, ref_utxo.output_index));
    }

    // Add registry info
    if let Some(ref registry) = deployment.registry {
        connection["processedMessagesScriptHash"] = json!(registry.hash);
        if let Some(ref nft) = registry.state_nft {
            connection["registryPolicyId"] = json!(nft.policy_id);
            connection["registryAssetNameHex"] = json!(nft.asset_name_hex);
        }
    }

    // Add processed messages NFT policy (for O(1) lookups)
    if let Some(ref mailbox_nft) = mailbox.state_nft {
        let mailbox_policy_id = &mailbox_nft.policy_id;
        let mailbox_policy_param = encode_script_hash_param(mailbox_policy_id)
            .with_context(|| "Failed to encode mailbox_policy_id as CBOR")?;
        let mailbox_policy_param_hex = hex::encode(&mailbox_policy_param);

        match apply_validator_param_with_purpose(
            &ctx.contracts_dir,
            "processed_message_nft",
            "processed_message_nft",
            Some("mint"),
            &mailbox_policy_param_hex,
        ) {
            Ok(applied) => {
                connection["processedMessagesNftPolicyId"] = json!(applied.policy_id);
                connection["processedMessagesNftScriptCbor"] = json!(applied.compiled_code);
            }
            Err(e) => {
                println!("  {} Failed to generate processed_message_nft policy: {}", "[WARN]".yellow(), e);
            }
        }
    }

    // Add IGP info
    if let Some(ref igp) = deployment.igp {
        if let Some(ref nft) = igp.state_nft {
            connection["igpPolicyId"] = json!(nft.policy_id);
        }
    }

    // Add validator announce info
    if let Some(ref va) = deployment.validator_announce {
        if let Some(ref nft) = va.state_nft {
            connection["validatorAnnouncePolicyId"] = json!(nft.policy_id);
        }
    }

    // Build complete validator config
    let config = json!({
        "originChainName": chain,
        "db": db_path,
        "allowPublicRpcs": false,
        "interval": 5,
        "maxSignConcurrency": 50,
        "validator": {
            "type": "hexKey",
            "key": validator_key
        },
        "checkpointSyncer": {
            "type": "localStorage",
            "path": checkpoint_path
        },
        "chains": {
            chain.clone(): {
                "name": chain,
                "domainId": domain_id,
                "chainId": domain_id,
                "protocol": "cardano",
                "rpcUrls": [{"http": blockfrost_url}],
                "connection": connection,
                "mailbox": format!("0x02000000{}", mailbox.hash),
                "interchainSecurityModule": format!("0x02000000{}", ism.hash),
                "interchainGasPaymaster": "0x0200000000000000000000000000000000000000000000000000000000000001",
                "validatorAnnounce": "0x0200000000000000000000000000000000000000000000000000000000000002",
                "merkleTreeHook": "0x0200000000000000000000000000000000000000000000000000000000000003",
                "blocks": {
                    "confirmations": 1,
                    "reorgPeriod": 5,
                    "estimateBlockTime": 20
                },
                "index": {
                    "from": index_from_block
                },
                "signer": {
                    "type": "hexKey",
                    "key": validator_key
                }
            }
        },
        "metricsPort": metrics_port
    });

    println!("  Origin chain: {}", chain);
    println!("  Domain ID: {}", domain_id);
    println!("  Checkpoint path: {}", checkpoint_path);
    println!("  Database path: {}", db_path);
    println!("  Index from block: {}", index_from_block);
    println!("  Metrics port: {}", metrics_port);

    if dry_run {
        println!("\n{}", "[Dry run - not writing changes]".yellow());
        println!("\nGenerated validator config:");
        println!("{}", serde_json::to_string_pretty(&config)?);
    } else {
        // Ensure parent directory exists
        if let Some(parent) = config_file.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config directory: {:?}", parent))?;
        }

        // Write config
        let content = serde_json::to_string_pretty(&config)?;
        std::fs::write(&config_file, content)
            .with_context(|| format!("Failed to write config file: {:?}", config_file))?;

        println!("\n{}", "Validator configuration generated successfully!".green().bold());
        println!("  File: {:?}", config_file);
        println!("\n{}", "Next steps:".cyan());
        println!("  1. Set BLOCKFROST_API_KEY environment variable");
        println!("  2. Create checkpoint directory: mkdir -p {}", checkpoint_path);
        println!("  3. Run validator: CONFIG_FILES={:?} ./validator", config_file);
    }

    Ok(())
}

async fn show_config(
    ctx: &CliContext,
    config_path: Option<String>,
    chain_name: Option<String>,
) -> Result<()> {
    let config_file = get_relayer_config_path(ctx, config_path);
    let chain = get_chain_name(ctx, chain_name);

    println!("{}", "Cardano Relayer Configuration".cyan());
    println!("  Config file: {:?}", config_file);
    println!("  Chain name: {}", chain);

    if !config_file.exists() {
        println!("\n{}", "Config file does not exist".yellow());
        return Ok(());
    }

    let content = std::fs::read_to_string(&config_file)
        .with_context(|| format!("Failed to read config file: {:?}", config_file))?;
    let config: Value = serde_json::from_str(&content)
        .with_context(|| "Failed to parse relayer config")?;

    let chain_config = config
        .get("chains")
        .and_then(|c| c.get(&chain));

    match chain_config {
        Some(cc) => {
            println!("\n{}", "Chain Configuration:".green());
            println!("{}", serde_json::to_string_pretty(cc)?);
        }
        None => {
            println!("\n{}", format!("Chain '{}' not found in config", chain).yellow());

            if let Some(chains) = config.get("chains").and_then(|c| c.as_object()) {
                println!("Available chains: {}", chains.keys().cloned().collect::<Vec<_>>().join(", "));
            }
        }
    }

    Ok(())
}

fn create_default_config() -> Value {
    json!({
        "chains": {},
        "relayChains": "",
        "originChainNames": "",
        "destinationChainNames": "",
        "gasPaymentEnforcement": [{"type": "none"}],
        "db": "/tmp/hyperlane-relayer-db",
        "allowLocalCheckpointSyncers": true,
        "allowContractCallCaching": false,
        "metrics": {"port": 9090}
    })
}

fn create_default_cardano_chain(chain_name: &str, network: &str) -> Value {
    let blockfrost_url = match network {
        "mainnet" => "https://cardano-mainnet.blockfrost.io/api/v0",
        "preprod" => "https://cardano-preprod.blockfrost.io/api/v0",
        _ => "https://cardano-preview.blockfrost.io/api/v0",
    };

    // Domain ID convention: 2001 = mainnet, 2002 = preprod, 2003 = preview
    let domain_id = match network {
        "mainnet" => 2001,
        "preprod" => 2002,
        _ => 2003,
    };

    json!({
        "name": chain_name,
        "domainId": domain_id,
        "chainId": domain_id,
        "protocol": "cardano",
        "submitter": "Lander",
        "rpcUrls": [{"http": blockfrost_url}],
        "connection": {
            "url": blockfrost_url,
            "apiKey": "",
            "network": network
        },
        "mailbox": "0x0200000000000000000000000000000000000000000000000000000000000000",
        "interchainGasPaymaster": "0x0200000000000000000000000000000000000000000000000000000000000001",
        "validatorAnnounce": "0x0200000000000000000000000000000000000000000000000000000000000002",
        "merkleTreeHook": "0x0200000000000000000000000000000000000000000000000000000000000003",
        "interchainSecurityModule": "0x0200000000000000000000000000000000000000000000000000000000000000",
        "blocks": {
            "confirmations": 1,
            "reorgPeriod": 5,
            "estimateBlockTime": 20
        },
        "index": {
            "from": 0
        }
    })
}

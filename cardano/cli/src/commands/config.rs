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
        ConfigCommands::Show {
            config_path,
            chain_name,
        } => show_config(ctx, config_path, chain_name).await,
    }
}

fn get_config_path(ctx: &CliContext, config_path: Option<String>) -> PathBuf {
    config_path
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // Default: go up from deployments dir and into config
            ctx.deployments_dir
                .parent()
                .unwrap_or(&ctx.deployments_dir)
                .join("config")
                .join("relayer-config.json")
        })
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

    let config_file = get_config_path(ctx, config_path);
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

    // Generate processed_message_nft policy (parameterized with mailbox script hash)
    // This is used for efficient O(1) processed message lookups
    if let Some(ref mailbox) = deployment.mailbox {
        println!("\n{}", "Generating processed_message_nft policy...".cyan());

        // Apply mailbox script hash parameter to processed_message_nft validator
        let mailbox_hash_param = encode_script_hash_param(&mailbox.hash)
            .with_context(|| "Failed to encode mailbox hash as CBOR")?;
        let mailbox_hash_param_hex = hex::encode(&mailbox_hash_param);

        match apply_validator_param_with_purpose(
            &ctx.contracts_dir,
            "processed_message_nft",
            "processed_message_nft",
            Some("mint"),
            &mailbox_hash_param_hex,
        ) {
            Ok(applied) => {
                println!("  Applied mailbox_script_hash parameter: {}", mailbox.hash);
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

async fn show_config(
    ctx: &CliContext,
    config_path: Option<String>,
    chain_name: Option<String>,
) -> Result<()> {
    let config_file = get_config_path(ctx, config_path);
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

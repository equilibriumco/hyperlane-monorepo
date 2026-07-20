//! Deploy command - Extract validators, compute hashes, generate addresses

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};
use colored::Colorize;

use crate::utils::blockfrost::BlockfrostClient;
use crate::utils::context::CliContext;
use crate::utils::plutus::{ExtractedValidator, HyperlaneValidators, PlutusBlueprint};
use crate::utils::tx_builder::HyperlaneTxBuilder;
use crate::utils::types::{DeploymentInfo, ReferenceScriptUtxo, ScriptInfo};

#[derive(Args)]
pub struct DeployArgs {
    #[command(subcommand)]
    command: DeployCommands,
}

#[derive(Subcommand)]
enum DeployCommands {
    /// Extract all validators from plutus.json
    Extract {
        /// Output directory for .plutus files (defaults to deployments/<network>)
        #[arg(short, long)]
        output: Option<String>,

        /// Only extract specific validators (comma-separated)
        #[arg(long)]
        only: Option<String>,

        /// Default ISM flavour to deploy: "messageid" (default) or "merkleroot".
        /// Selects which validator fills the `ism` slot and records its module_type.
        #[arg(long, default_value = "messageid")]
        ism_module_type: String,
    },

    /// Show validator information without extracting
    Info,

    /// Generate deployment info JSON from extracted validators
    GenerateConfig {
        /// Output file path
        #[arg(short, long)]
        output: Option<String>,
    },

    /// Deploy a script as a reference script UTXO on-chain
    /// This allows the script to be referenced by other transactions without including it in the witness set
    ReferenceScript {
        /// Name of the script to deploy (mailbox, message_id_multisig_ism,
        /// merkle_root_multisig_ism, ism, igp, validator_announce, warp_route,
        /// or path to .plutus file)
        #[arg(long)]
        script: String,

        /// Output lovelace amount (if not specified, calculates minimum based on script size + 10% buffer)
        #[arg(long)]
        lovelace: Option<u64>,

        /// Dry run - show what would be done without submitting
        #[arg(long)]
        dry_run: bool,
    },

    /// Deploy all core reference scripts (mailbox, ism, igp)
    ReferenceScriptsAll {
        /// Output lovelace per script (if not specified, calculates minimum for each script)
        #[arg(long)]
        lovelace: Option<u64>,

        /// Dry run
        #[arg(long)]
        dry_run: bool,
    },
}

pub async fn execute(ctx: &CliContext, args: DeployArgs) -> Result<()> {
    match args.command {
        DeployCommands::Extract {
            output,
            only,
            ism_module_type,
        } => extract(ctx, output, only, ism_module_type).await,
        DeployCommands::Info => info(ctx).await,
        DeployCommands::GenerateConfig { output } => generate_config(ctx, output).await,
        DeployCommands::ReferenceScript {
            script,
            lovelace,
            dry_run,
        } => deploy_reference_script(ctx, &script, lovelace, dry_run).await,
        DeployCommands::ReferenceScriptsAll { lovelace, dry_run } => {
            deploy_all_reference_scripts(ctx, lovelace, dry_run).await
        }
    }
}

/// Calculate minimum lovelace for a reference script UTXO
/// Formula: (160 + addressSize + scriptSize) * coinsPerUTxOByte
/// We add a 10% buffer for safety
pub(crate) fn calculate_min_lovelace_for_ref_script(
    script_size: usize,
    coins_per_utxo_byte: u64,
) -> u64 {
    // Overhead: 160 (base) + 57 (typical script address) + 10 (value) + 30 (other)
    const OVERHEAD: u64 = 160 + 57 + 10 + 30;
    let base = (OVERHEAD + script_size as u64) * coins_per_utxo_byte;
    // Add 10% buffer and round up to nearest ADA
    let with_buffer = (base * 110) / 100;
    // Round up to nearest 1 ADA (1_000_000 lovelace)
    ((with_buffer + 999_999) / 1_000_000) * 1_000_000
}

async fn extract(
    ctx: &CliContext,
    output: Option<String>,
    only: Option<String>,
    ism_module_type: String,
) -> Result<()> {
    println!("{}", "Extracting Hyperlane validators...".cyan());

    // Load blueprint
    let blueprint_path = ctx.plutus_json_path();
    println!("Loading blueprint from {:?}", blueprint_path);

    let blueprint = PlutusBlueprint::from_file(&blueprint_path)?;
    println!(
        "  Blueprint: {} v{} ({})",
        blueprint.preamble.title, blueprint.preamble.version, blueprint.preamble.plutus_version
    );

    // Determine output directory
    let output_dir = match output {
        Some(p) => {
            let path = std::path::PathBuf::from(p);
            std::fs::create_dir_all(&path)
                .with_context(|| format!("Failed to create output directory: {:?}", path))?;
            path
        }
        None => ctx.ensure_deployments_dir()?,
    };

    // Filter validators if --only specified
    let filter: Option<Vec<&str>> = only.as_ref().map(|s| s.split(',').collect());

    // Extract validators
    let network = ctx.pallas_network();
    let validators = HyperlaneValidators::extract(&blueprint, network)?;

    // Select which validator fills the default `ism` slot.
    let ism_mt = ism_module_type.to_lowercase();
    if ism_mt != "messageid" && ism_mt != "merkleroot" {
        return Err(anyhow!(
            "--ism-module-type must be 'messageid' or 'merkleroot', got '{ism_module_type}'"
        ));
    }
    let default_ism = if ism_mt == "merkleroot" {
        &validators.merkle_root_ism
    } else {
        &validators.message_id_ism
    };
    println!(
        "  Default ISM: {} ({})",
        if ism_mt == "merkleroot" {
            "MerkleRoot"
        } else {
            "MessageId"
        },
        default_ism.hash
    );

    // Both ISM flavours are always written out (as separate files), so either
    // one can be initialized later as an alt ISM via `init ism --module-type`.
    let all_validators = vec![
        ("mailbox", Some(&validators.mailbox)),
        ("message_id_multisig_ism", Some(&validators.message_id_ism)),
        (
            "merkle_root_multisig_ism",
            Some(&validators.merkle_root_ism),
        ),
        ("igp", validators.igp.as_ref()),
        ("validator_announce", validators.validator_announce.as_ref()),
        ("warp_route", validators.warp_route.as_ref()),
    ];

    println!("\n{}", "Extracted validators:".green());
    println!("{}", "-".repeat(80));

    for (name, validator_opt) in &all_validators {
        // Skip if filter specified and name not in filter
        if let Some(ref f) = filter {
            if !f.contains(name) {
                continue;
            }
        }

        if let Some(validator) = validator_opt {
            // Save .plutus file
            let plutus_path = output_dir.join(format!("{}.plutus", name));
            validator.save_plutus_file(&plutus_path)?;

            // Save hash file
            let hash_path = output_dir.join(format!("{}.hash", name));
            std::fs::write(&hash_path, &validator.hash)?;

            // Save address file
            let addr_path = output_dir.join(format!("{}.addr", name));
            std::fs::write(&addr_path, &validator.address)?;

            println!("{} {}", "✓".green(), format!("{:<20}", name).bold());
            println!("    Hash:    {}", validator.hash);
            println!("    Address: {}", validator.address);
            println!("    File:    {:?}", plutus_path);
        }
    }

    // Also extract minting policies
    if let Some(state_nft) = &validators.state_nft {
        if filter.is_none() || filter.as_ref().unwrap().contains(&"state_nft") {
            let plutus_path = output_dir.join("state_nft.plutus");
            state_nft.save_plutus_file(&plutus_path)?;

            println!(
                "\n{} {} {}",
                "✓".green(),
                "state_nft".bold(),
                "(minting policy - requires parameter application)".yellow()
            );
            println!("    Note: Use 'aiken blueprint apply' to create parameterized policy");
        }
    }

    // Update deployment_info.json with new addresses
    // Note: For parameterized scripts, hash_before_parametrization contains the pre-param hash
    // The `hash` field will be updated later when parameters are applied during init
    let to_script_info = |v: &ExtractedValidator| {
        if v.requires_parameters {
            // Parameterized script - record hash before parameters as placeholder
            // The final hash will be set during initialization
            ScriptInfo {
                hash_before_parametrization: Some(v.hash.clone()),
                hash: v.hash.clone(), // Placeholder, will be updated during init
                address: v.address.clone(), // Placeholder, will be updated during init
                applied_parameters: Vec::new(), // Will be filled during init
                module_type: None,
                state_nft: None,
                state_utxo: None,
                reference_script_utxo: None,
                init_tx_hash: None,
                initialized: false,
                utxo: None,
                state_nft_policy: None,
            }
        } else {
            // Non-parameterized script
            ScriptInfo::new(v.hash.clone(), v.address.clone())
        }
    };

    // Load existing deployment_info to preserve initialized state, warp routes, etc.
    let info_path = output_dir.join("deployment_info.json");
    let mut deployment_info = if info_path.exists() {
        let content = std::fs::read_to_string(&info_path)?;
        serde_json::from_str(&content).unwrap_or_else(|_| DeploymentInfo::new(ctx.network.as_str()))
    } else {
        DeploymentInfo::new(ctx.network.as_str())
    };

    let should_update =
        |name: &str| -> bool { filter.as_ref().map_or(true, |f| f.contains(&name)) };

    if should_update("mailbox") {
        deployment_info.mailbox = Some(to_script_info(&validators.mailbox));
    }
    if should_update("message_id_multisig_ism")
        || should_update("merkle_root_multisig_ism")
        || should_update("ism")
    {
        let mut ism_info = to_script_info(default_ism);
        ism_info.module_type = Some(ism_mt.clone());
        deployment_info.ism = Some(ism_info);
    }
    if should_update("igp") {
        if let Some(igp) = &validators.igp {
            deployment_info.igp = Some(to_script_info(igp));
        }
    }
    if should_update("validator_announce") {
        if let Some(va) = &validators.validator_announce {
            deployment_info.validator_announce = Some(to_script_info(va));
        }
    }

    let info_json = serde_json::to_string_pretty(&deployment_info)?;
    std::fs::write(&info_path, info_json)?;
    println!("\n{}", "✓ Deployment info updated".green());

    println!("\n{}", "✓ Extraction complete!".green().bold());
    println!("Output directory: {:?}", output_dir);

    Ok(())
}

async fn info(ctx: &CliContext) -> Result<()> {
    println!("{}", "Hyperlane validator information".cyan());

    let blueprint_path = ctx.plutus_json_path();
    let blueprint = PlutusBlueprint::from_file(&blueprint_path)?;

    println!(
        "\n{}: {} v{}",
        "Blueprint".bold(),
        blueprint.preamble.title,
        blueprint.preamble.version
    );
    println!("Plutus version: {}", blueprint.preamble.plutus_version);

    println!("\n{}", "Spend validators:".green());
    for v in blueprint.spend_validators() {
        let has_params = !v.parameters.is_empty();
        let params_info = if has_params {
            format!(" (parameterized: {})", v.parameters.len())
        } else {
            String::new()
        };
        println!("  - {}{}", v.title, params_info.yellow());
    }

    println!("\n{}", "Mint validators (minting policies):".green());
    for v in blueprint.mint_validators() {
        let has_params = !v.parameters.is_empty();
        let params_info = if has_params {
            format!(" (parameterized: {})", v.parameters.len())
        } else {
            String::new()
        };
        println!("  - {}{}", v.title, params_info.yellow());
    }

    // Show computed hashes for non-parameterized validators
    let network = ctx.pallas_network();
    println!("\n{}", "Script hashes (non-parameterized):".green());

    for v in &blueprint.validators {
        if v.parameters.is_empty() {
            if let Ok(extracted) = ExtractedValidator::from_def(v, network) {
                if v.title.ends_with(".spend") {
                    println!("  {}", v.title.bold());
                    println!("    Hash:    {}", extracted.hash);
                    println!("    Address: {}", extracted.address);
                }
            }
        }
    }

    Ok(())
}

async fn generate_config(ctx: &CliContext, output: Option<String>) -> Result<()> {
    println!("{}", "Generating deployment configuration...".cyan());

    let blueprint_path = ctx.plutus_json_path();
    let blueprint = PlutusBlueprint::from_file(&blueprint_path)?;

    let network = ctx.pallas_network();
    let validators = HyperlaneValidators::extract(&blueprint, network)?;

    let mut info = DeploymentInfo::new(ctx.network.as_str());

    // Helper to create ScriptInfo for validators
    let to_script_info = |v: &ExtractedValidator| {
        if v.requires_parameters {
            ScriptInfo {
                hash_before_parametrization: Some(v.hash.clone()),
                hash: v.hash.clone(),
                address: v.address.clone(),
                applied_parameters: Vec::new(),
                module_type: None,
                state_nft: None,
                state_utxo: None,
                reference_script_utxo: None,
                init_tx_hash: None,
                initialized: false,
                utxo: None,
                state_nft_policy: None,
            }
        } else {
            ScriptInfo::new(v.hash.clone(), v.address.clone())
        }
    };

    info.mailbox = Some(to_script_info(&validators.mailbox));
    info.ism = Some(to_script_info(&validators.ism));

    if let Some(igp) = &validators.igp {
        info.igp = Some(to_script_info(igp));
    }

    if let Some(va) = &validators.validator_announce {
        info.validator_announce = Some(to_script_info(va));
    }

    // Save to file
    let output_path = match output {
        Some(p) => std::path::PathBuf::from(p),
        None => ctx.network_deployments_dir().join("deployment_info.json"),
    };

    let content = serde_json::to_string_pretty(&info)?;
    std::fs::create_dir_all(output_path.parent().unwrap())?;
    std::fs::write(&output_path, &content)
        .with_context(|| format!("Failed to write {:?}", output_path))?;

    println!(
        "{} Deployment config saved to {:?}",
        "✓".green(),
        output_path
    );
    println!("\n{}", "Note:".yellow().bold());
    println!("  This config contains script hashes and addresses.");
    println!("  After initialization, update with state NFT policy IDs and UTXOs.");

    Ok(())
}

/// Deploy a single script as a reference script UTXO
async fn deploy_reference_script(
    ctx: &CliContext,
    script_name: &str,
    lovelace: Option<u64>,
    dry_run: bool,
) -> Result<()> {
    deploy_reference_script_internal(ctx, script_name, lovelace, dry_run, &[]).await?;
    Ok(())
}

/// Internal reference script deployment that tracks spent UTXOs
/// Returns (spent_utxo_ref, submitted_tx_hash) if successful
async fn deploy_reference_script_internal(
    ctx: &CliContext,
    script_name: &str,
    lovelace_override: Option<u64>,
    dry_run: bool,
    exclude_utxos: &[String],
) -> Result<Option<(String, String)>> {
    println!(
        "{}",
        format!("Deploying reference script: {}", script_name).cyan()
    );

    let api_key = ctx.require_api_key()?;
    let keypair = ctx.load_signing_key()?;

    // Load script CBOR
    let (script_cbor, script_hash, script_title) = load_script(ctx, script_name)?;

    // Calculate or use provided lovelace amount
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);
    let lovelace = match lovelace_override {
        Some(l) => l,
        None => {
            // Calculate minimum based on script size and protocol params
            let params = client.get_protocol_params().await?;
            let min_lovelace = calculate_min_lovelace_for_ref_script(
                script_cbor.len(),
                params.coins_per_utxo_byte,
            );
            println!(
                "  Calculated min: {} ADA (script size: {} bytes, {} lovelace/byte)",
                (min_lovelace as f64) / 1_000_000.0,
                script_cbor.len(),
                params.coins_per_utxo_byte
            );
            min_lovelace
        }
    };

    println!("  Script Hash: {}", script_hash.green());
    println!("  Script Size: {} bytes", script_cbor.len());
    println!("  Output:      {} ADA", lovelace / 1_000_000);

    let payer_address = keypair.address_bech32(ctx.pallas_network());

    // Get UTXOs for fee payment and filter out already-spent ones
    let all_utxos = client.get_utxos(&payer_address).await?;
    let utxos: Vec<_> = all_utxos
        .into_iter()
        .filter(|u| {
            let utxo_ref = format!("{}#{}", u.tx_hash, u.output_index);
            !exclude_utxos.contains(&utxo_ref)
        })
        .collect();
    println!(
        "  Found {} UTXOs at wallet (excluding {} spent)",
        utxos.len(),
        exclude_utxos.len()
    );

    // Find suitable UTXOs (must not have reference scripts)
    let input_utxo = utxos
        .iter()
        .find(|u| {
            u.lovelace >= lovelace + 5_000_000
                && u.assets.is_empty()
                && u.reference_script.is_none()
        })
        .ok_or_else(|| {
            anyhow!(
                "No suitable UTXO found (need >= {} ADA without assets or reference scripts)",
                (lovelace + 5_000_000) / 1_000_000
            )
        })?;

    println!(
        "  Input UTXO: {}#{}",
        input_utxo.tx_hash, input_utxo.output_index
    );

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        println!("\nTransaction would:");
        println!(
            "  - Spend UTXO {}#{}",
            input_utxo.tx_hash, input_utxo.output_index
        );
        println!(
            "  - Create reference script output with {} ADA",
            lovelace / 1_000_000
        );
        println!("  - Script hash: {}", script_hash);
        println!("\nAfter deployment, update the relayer config to use this reference script:");
        println!("  Reference Script UTXO: <tx_hash>#0");
        return Ok(None);
    }

    // Build the reference script transaction
    let tx_builder = HyperlaneTxBuilder::new(&client, ctx.pallas_network());
    let built_tx = tx_builder
        .build_reference_script_tx(&keypair, input_utxo, &script_cbor, lovelace)
        .await?;

    println!("  TX Hash: {}", hex::encode(&built_tx.tx_hash.0));

    // Sign transaction
    println!("{}", "Signing transaction...".cyan());
    let signed_tx = tx_builder.sign_tx(built_tx, &keypair)?;
    println!("  Signed TX size: {} bytes", signed_tx.len());

    // Submit transaction
    println!("{}", "Submitting transaction...".cyan());
    let tx_hash = client.submit_and_confirm(&signed_tx, ctx.no_wait).await?;
    println!("\n{}", "✓ Transaction submitted!".green().bold());
    println!("  Explorer: {}", ctx.explorer_tx_url(&tx_hash));

    // Save reference script info
    let ref_script_info = ReferenceScriptInfo {
        script_name: script_title,
        script_hash: script_hash.clone(),
        tx_hash: tx_hash.clone(),
        output_index: 0,
        lovelace,
    };

    let ref_scripts_file = ctx.network_deployments_dir().join("reference_scripts.json");
    let mut ref_scripts: Vec<ReferenceScriptInfo> = if ref_scripts_file.exists() {
        let content = std::fs::read_to_string(&ref_scripts_file)?;
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        Vec::new()
    };

    // Update or add the reference script info
    if let Some(existing) = ref_scripts
        .iter_mut()
        .find(|r| r.script_hash == script_hash)
    {
        *existing = ref_script_info;
    } else {
        ref_scripts.push(ref_script_info);
    }

    std::fs::write(
        &ref_scripts_file,
        serde_json::to_string_pretty(&ref_scripts)?,
    )?;
    println!("\n{}", "✓ Reference script info saved".green());
    println!("  File: {:?}", ref_scripts_file);

    // Also update deployment_info.json with the reference script UTXO
    if let Ok(mut deployment) = ctx.load_deployment_info() {
        let ref_utxo = ReferenceScriptUtxo {
            tx_hash: tx_hash.clone(),
            output_index: 0,
            lovelace,
        };

        // Match script name to the appropriate field in deployment info
        match script_name {
            "mailbox" => {
                if let Some(ref mut mailbox) = deployment.mailbox {
                    mailbox.reference_script_utxo = Some(ref_utxo);
                }
            }
            "ism" => {
                // Generic alias: always targets the default ISM, regardless of flavour.
                if let Some(ref mut ism) = deployment.ism {
                    ism.reference_script_utxo = Some(ref_utxo);
                }
            }
            "message_id_multisig_ism" | "merkle_root_multisig_ism" => {
                // A single reference script UTXO can be shared by every ISM instance
                // (default or alt) of the same flavour.
                let flavour = if script_name == "merkle_root_multisig_ism" {
                    "merkleroot"
                } else {
                    "messageid"
                };
                if let Some(ref mut ism) = deployment.ism {
                    if ism.module_type.as_deref() == Some(flavour) {
                        ism.reference_script_utxo = Some(ref_utxo.clone());
                    }
                }
                for alt in deployment.alt_isms.iter_mut() {
                    if alt.module_type.as_deref() == Some(flavour) {
                        alt.reference_script_utxo = Some(ref_utxo.clone());
                    }
                }
            }
            "igp" => {
                if let Some(ref mut igp) = deployment.igp {
                    igp.reference_script_utxo = Some(ref_utxo);
                }
            }
            "validator_announce" => {
                if let Some(ref mut va) = deployment.validator_announce {
                    va.reference_script_utxo = Some(ref_utxo);
                }
            }
            "warp_route" => {
                // Warp route ref scripts are managed per-route in warp_routes array
            }
            _ => {
                // Unknown script, skip updating deployment_info.json
            }
        }

        ctx.save_deployment_info(&deployment)?;
        println!("{}", "✓ Deployment info updated".green());
    }

    println!("\n{}", "Reference UTXO for relayer config:".yellow());
    println!("  {}#0", tx_hash);

    // Return the spent UTXO reference and the new tx_hash
    Ok(Some((
        format!("{}#{}", input_utxo.tx_hash, input_utxo.output_index),
        tx_hash,
    )))
}

/// Deploy all core reference scripts (mailbox, ism, igp)
async fn deploy_all_reference_scripts(
    ctx: &CliContext,
    lovelace: Option<u64>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Deploying all core reference scripts...".cyan());

    // Deploy a reference script for every ISM flavour actually deployed (the
    // default ISM plus any alt ISMs), so the relayer can reference the correct
    // script regardless of which ISM instance a message is verified against.
    let ism_slot = |module_type: &str| -> &'static str {
        if module_type == "merkleroot" {
            "merkle_root_multisig_ism"
        } else {
            "message_id_multisig_ism"
        }
    };

    // The IGP reference script matters for third parties: without it, every gas
    // payer must ship a local copy of the compiled IGP script (and carry it in
    // each payment's witness set) just to call `pay-for-gas`.
    let mut scripts: Vec<&'static str> = vec!["mailbox", "igp"];
    if let Ok(deployment) = ctx.load_deployment_info() {
        let mut flavours: Vec<String> = Vec::new();
        let default_mt = deployment
            .ism
            .as_ref()
            .and_then(|m| m.module_type.clone())
            .unwrap_or_else(|| "messageid".to_string());
        flavours.push(default_mt);
        for alt in &deployment.alt_isms {
            if let Some(mt) = &alt.module_type {
                if !flavours.contains(mt) {
                    flavours.push(mt.clone());
                }
            }
        }
        for mt in &flavours {
            scripts.push(ism_slot(mt));
        }
    } else {
        // No deployment info yet - fall back to the default MessageId ISM.
        scripts.push("message_id_multisig_ism");
    }

    // Track spent UTXOs to avoid reusing them
    let mut spent_utxos: Vec<String> = Vec::new();

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    // Get wallet address for UTXO polling
    let keypair = ctx.load_signing_key()?;
    let wallet_address = keypair.address_bech32(ctx.pallas_network());

    for script in &scripts {
        println!("\n{}", format!("=== {} ===", script).cyan().bold());
        // Pass lovelace override - if None, each script calculates its own minimum
        let result =
            deploy_reference_script_internal(ctx, script, lovelace, dry_run, &spent_utxos).await?;

        if let Some((spent_utxo, tx_hash)) = result {
            spent_utxos.push(spent_utxo);

            // Wait for the change UTXO to appear at the wallet address
            // This is more reliable than wait_for_tx because Blockfrost can index
            // the transaction before the address UTXOs are updated
            println!("Waiting for change UTXO to be indexed (timeout: 120s)...");
            // Change output is at index #1 (reference script output is at #0)
            match client
                .wait_for_utxo(&wallet_address, &tx_hash, 1, 120)
                .await
            {
                Ok(utxo) => println!(
                    "{}",
                    format!(
                        "✓ Change UTXO available: {}#{} ({} ADA)",
                        tx_hash,
                        1,
                        utxo.lovelace / 1_000_000
                    )
                    .green()
                ),
                Err(e) => {
                    println!("{}", format!("Warning: {}", e).yellow());
                    println!(
                        "Continuing anyway, but next deployment may fail if UTXO not yet available"
                    );
                }
            }
        }
    }

    println!("\n{}", "✓ All reference scripts deployed!".green().bold());

    Ok(())
}

/// Load a script by name or path
fn load_script(ctx: &CliContext, script_name: &str) -> Result<(Vec<u8>, String, String)> {
    // Check if it's a file path
    let path = std::path::Path::new(script_name);
    if path.exists() && path.extension().map_or(false, |e| e == "plutus") {
        // Load from file
        let content = std::fs::read_to_string(path)?;
        let json: serde_json::Value = serde_json::from_str(&content)?;
        let cbor_hex = json["cborHex"]
            .as_str()
            .ok_or_else(|| anyhow!("No cborHex in plutus file"))?;
        let cbor = hex::decode(cbor_hex)?;
        let hash = hex::encode(crate::utils::crypto::script_hash(&cbor));
        let title = path.file_stem().unwrap().to_string_lossy().to_string();
        return Ok((cbor, hash, title));
    }

    // For parameterized scripts, check if we have an applied version from initialization
    let applied_script_path = match script_name {
        "mailbox" => Some(ctx.network_deployments_dir().join("mailbox_applied.plutus")),
        _ => None,
    };

    if let Some(applied_path) = applied_script_path {
        if applied_path.exists() {
            println!("  Using applied script: {:?}", applied_path);
            let content = std::fs::read_to_string(&applied_path)?;
            let json: serde_json::Value = serde_json::from_str(&content)?;
            let cbor_hex = json["cborHex"]
                .as_str()
                .ok_or_else(|| anyhow!("No cborHex in applied plutus file"))?;
            let cbor = hex::decode(cbor_hex)?;
            let hash = hex::encode(crate::utils::crypto::script_hash(&cbor));
            return Ok((cbor, hash, script_name.to_string()));
        } else {
            return Err(anyhow!(
                "Validator '{}' is parameterized but no applied script found at {:?}. Run 'init {}' first.",
                script_name, applied_path, script_name
            ));
        }
    }

    // Load from blueprint
    let blueprint = PlutusBlueprint::from_file(&ctx.plutus_json_path())?;

    // Map common names to validator titles. "ism" is a generic alias that
    // resolves to whichever flavour is recorded as the default in deployment info.
    let title: String = match script_name {
        "mailbox" => "mailbox.mailbox.spend".to_string(),
        "message_id_multisig_ism" => {
            "message_id_multisig_ism.message_id_multisig_ism.spend".to_string()
        }
        "merkle_root_multisig_ism" => {
            "merkle_root_multisig_ism.merkle_root_multisig_ism.spend".to_string()
        }
        "ism" => {
            let default_mt = ctx
                .load_deployment_info()
                .ok()
                .and_then(|d| d.ism.and_then(|m| m.module_type))
                .unwrap_or_else(|| "messageid".to_string());
            if default_mt == "merkleroot" {
                "merkle_root_multisig_ism.merkle_root_multisig_ism.spend".to_string()
            } else {
                "message_id_multisig_ism.message_id_multisig_ism.spend".to_string()
            }
        }
        "igp" => "igp.igp.spend".to_string(),
        "validator_announce" => "validator_announce.validator_announce.spend".to_string(),
        "warp_route" => "warp_route.warp_route.spend".to_string(),
        "generic_recipient" => "generic_recipient.generic_recipient.spend".to_string(),
        _ => script_name.to_string(), // Try as exact title
    };

    let validator = blueprint
        .find_validator(&title)
        .ok_or_else(|| anyhow!("Validator '{}' not found in blueprint", title))?;

    if !validator.parameters.is_empty() {
        return Err(anyhow!(
            "Validator '{}' is parameterized. Run 'init {}' first to apply parameters.",
            title,
            script_name
        ));
    }

    let cbor = hex::decode(&validator.compiled_code)?;
    let hash = validator.hash.clone();
    let short_name = title.split('.').next().unwrap_or(&title).to_string();

    Ok((cbor, hash, short_name))
}

/// Reference script deployment info
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ReferenceScriptInfo {
    script_name: String,
    script_hash: String,
    tx_hash: String,
    output_index: u32,
    lovelace: u64,
}

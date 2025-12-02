//! Init command - Initialize contracts with state NFTs and initial datums

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};
use colored::Colorize;

use crate::utils::blockfrost::BlockfrostClient;
use crate::utils::cbor::{build_generic_recipient_datum, build_ism_datum, build_mailbox_datum};
use crate::utils::context::CliContext;
use crate::utils::plutus::{apply_validator_param, encode_output_reference};
use crate::utils::tx_builder::HyperlaneTxBuilder;

#[derive(Args)]
pub struct InitArgs {
    #[command(subcommand)]
    command: InitCommands,
}

#[derive(Subcommand)]
enum InitCommands {
    /// Initialize the Mailbox contract
    Mailbox {
        /// Local domain ID (e.g., 2003 for Cardano Preview)
        #[arg(long)]
        domain: u32,

        /// ISM script hash (28 bytes hex)
        #[arg(long)]
        ism_hash: String,

        /// UTXO to use for minting state NFT (tx_hash#index)
        #[arg(long)]
        utxo: Option<String>,

        /// Dry run - show what would be done without submitting
        #[arg(long)]
        dry_run: bool,
    },

    /// Initialize the MultisigISM contract
    Ism {
        /// Origin domain IDs (comma-separated, e.g., "43113,421614")
        #[arg(long)]
        domains: String,

        /// Initial validators per domain (format: "domain:addr1,addr2;domain2:addr3")
        #[arg(long)]
        validators: Option<String>,

        /// Initial threshold per domain (format: "domain:threshold;domain2:threshold")
        #[arg(long)]
        thresholds: Option<String>,

        /// UTXO to use for minting state NFT
        #[arg(long)]
        utxo: Option<String>,

        /// Dry run
        #[arg(long)]
        dry_run: bool,
    },

    /// Initialize the Registry contract
    Registry {
        /// UTXO to use for minting state NFT
        #[arg(long)]
        utxo: Option<String>,

        /// Dry run
        #[arg(long)]
        dry_run: bool,
    },

    /// Initialize a Generic Recipient contract for testing
    ///
    /// Creates the two-UTXO pattern required for reference scripts:
    /// - State UTXO: at script address with state NFT + datum
    /// - Reference Script UTXO: at deployer address with ref NFT + script attached
    Recipient {
        /// Mailbox script hash (required to parameterize the recipient)
        #[arg(long)]
        mailbox_hash: Option<String>,

        /// Custom ISM script hash (optional, uses default if not specified)
        #[arg(long)]
        custom_ism: Option<String>,

        /// UTXO to use for minting state NFT
        #[arg(long)]
        utxo: Option<String>,

        /// Output lovelace amount for state UTXO (default 5 ADA)
        #[arg(long, default_value = "5000000")]
        output_lovelace: u64,

        /// Output lovelace amount for reference script UTXO (default 20 ADA)
        /// Reference script UTXOs need more ADA due to storing the script
        #[arg(long, default_value = "20000000")]
        ref_script_lovelace: u64,

        /// Pre-applied state NFT script file (bypasses aiken)
        #[arg(long)]
        nft_script: Option<String>,

        /// Pre-applied recipient script file (bypasses aiken)
        #[arg(long)]
        recipient_script: Option<String>,

        /// Dry run
        #[arg(long)]
        dry_run: bool,
    },

    /// Initialize all core contracts at once
    All {
        /// Local domain ID for Cardano
        #[arg(long)]
        domain: u32,

        /// Origin domains for ISM (comma-separated)
        #[arg(long)]
        origin_domains: String,

        /// Dry run
        #[arg(long)]
        dry_run: bool,
    },

    /// Show initialization status of contracts
    Status,

    /// Generate initial datums without initializing
    GenerateDatums {
        /// Local domain ID
        #[arg(long)]
        domain: u32,

        /// ISM script hash
        #[arg(long)]
        ism_hash: String,

        /// Owner public key hash (defaults to signing key)
        #[arg(long)]
        owner: Option<String>,

        /// Output directory
        #[arg(short, long)]
        output: Option<String>,
    },
}

pub async fn execute(ctx: &CliContext, args: InitArgs) -> Result<()> {
    match args.command {
        InitCommands::Mailbox {
            domain,
            ism_hash,
            utxo,
            dry_run,
        } => init_mailbox(ctx, domain, &ism_hash, utxo, dry_run).await,
        InitCommands::Ism {
            domains,
            validators,
            thresholds,
            utxo,
            dry_run,
        } => init_ism(ctx, &domains, validators, thresholds, utxo, dry_run).await,
        InitCommands::Registry { utxo, dry_run } => init_registry(ctx, utxo, dry_run).await,
        InitCommands::Recipient {
            mailbox_hash,
            custom_ism,
            utxo,
            output_lovelace,
            ref_script_lovelace,
            nft_script,
            recipient_script,
            dry_run,
        } => init_recipient(ctx, mailbox_hash, custom_ism, utxo, output_lovelace, ref_script_lovelace, nft_script, recipient_script, dry_run).await,
        InitCommands::All {
            domain,
            origin_domains,
            dry_run,
        } => init_all(ctx, domain, &origin_domains, dry_run).await,
        InitCommands::Status => show_status(ctx).await,
        InitCommands::GenerateDatums {
            domain,
            ism_hash,
            owner,
            output,
        } => generate_datums(ctx, domain, &ism_hash, owner, output).await,
    }
}

async fn init_mailbox(
    ctx: &CliContext,
    domain: u32,
    ism_hash: &str,
    utxo: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Initializing Mailbox contract...".cyan());
    println!("  Domain: {}", domain);
    println!("  Default ISM: {}", ism_hash);

    let api_key = ctx.require_api_key()?;
    let keypair = ctx.load_signing_key()?;
    let owner_pkh = keypair.verification_key_hash_hex();

    println!("  Owner: {}", owner_pkh);

    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);
    let address = keypair.address_bech32(ctx.pallas_network());

    // Get UTXOs
    let utxos = client.get_utxos(&address).await?;
    println!("  Found {} UTXOs at wallet", utxos.len());

    // Find input UTXO for spending
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
                .find(|u| u.lovelace >= 10_000_000 && u.assets.is_empty())
                .cloned()
                .ok_or_else(|| anyhow!("No suitable UTXO found (need >= 10 ADA without assets)"))?
        }
    };

    // Find collateral UTXO (must be different from input)
    let collateral_utxo = utxos
        .iter()
        .find(|u| {
            u.lovelace >= 5_000_000
                && u.assets.is_empty()
                && !(u.tx_hash == input_utxo.tx_hash && u.output_index == input_utxo.output_index)
        })
        .cloned()
        .ok_or_else(|| anyhow!("No suitable collateral UTXO found (need a second UTXO with >= 5 ADA)"))?;

    println!("  Input UTXO: {}#{}", input_utxo.tx_hash, input_utxo.output_index);
    println!("  Collateral: {}#{}", collateral_utxo.tx_hash, collateral_utxo.output_index);

    // Encode output reference for state NFT parameter
    let output_ref_cbor = encode_output_reference(&input_utxo.tx_hash, input_utxo.output_index)?;
    let output_ref_hex = hex::encode(&output_ref_cbor);
    println!("  OutputRef CBOR: {}", output_ref_hex.yellow());

    // Apply parameter to state_nft minting policy using aiken CLI
    println!("\n{}", "Applying state_nft parameter...".cyan());
    let applied = apply_validator_param(&ctx.contracts_dir, "state_nft", "state_nft", &output_ref_hex)?;
    println!("  State NFT Policy ID: {}", applied.policy_id.green());

    // Get mailbox script address
    let deployment = ctx.load_deployment_info()
        .with_context(|| "Run 'deploy extract' first")?;
    let mailbox_addr = deployment
        .mailbox
        .as_ref()
        .map(|m| m.address.clone())
        .ok_or_else(|| anyhow!("Mailbox address not found in deployment info"))?;
    println!("  Mailbox Address: {}", mailbox_addr);

    // Build mailbox datum
    let merkle_root = "0".repeat(64); // 32 bytes of zeros
    let datum_cbor = build_mailbox_datum(domain, ism_hash, &owner_pkh, 0, &merkle_root, 0)?;
    println!("  Datum CBOR: {}...", hex::encode(&datum_cbor[..32.min(datum_cbor.len())]));

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        println!("\nTransaction would:");
        println!("  - Spend UTXO {}#{}", input_utxo.tx_hash, input_utxo.output_index);
        println!("  - Mint state NFT with policy {}", applied.policy_id);
        println!("  - Create output at {} with {} ADA + NFT + datum", mailbox_addr, 5);
        return Ok(());
    }

    // Build and submit transaction
    println!("\n{}", "Building transaction...".cyan());
    let mint_script_cbor = hex::decode(&applied.compiled_code)
        .with_context(|| "Invalid script CBOR")?;

    let tx_builder = HyperlaneTxBuilder::new(&client, ctx.pallas_network());
    let built_tx = tx_builder
        .build_init_tx(
            &keypair,
            &input_utxo,
            &collateral_utxo,
            &mint_script_cbor,
            &mailbox_addr,
            &datum_cbor,
            5_000_000, // 5 ADA output
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
    println!("  Explorer: https://preview.cardanoscan.io/transaction/{}", tx_hash);

    // Update deployment info
    let mut deployment = deployment;
    if let Some(ref mut mailbox) = deployment.mailbox {
        mailbox.state_nft_policy = Some(applied.policy_id.clone());
    }
    ctx.save_deployment_info(&deployment)?;
    println!("\n{}", "✓ Deployment info updated".green());

    Ok(())
}

async fn init_ism(
    ctx: &CliContext,
    domains: &str,
    validators: Option<String>,
    thresholds: Option<String>,
    utxo: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Initializing MultisigISM contract...".cyan());

    let api_key = ctx.require_api_key()?;
    let keypair = ctx.load_signing_key()?;
    let owner_pkh = keypair.verification_key_hash_hex();

    // Parse domains
    let domain_list: Vec<u32> = domains
        .split(',')
        .map(|s| s.trim().parse::<u32>())
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| "Invalid domain format")?;

    println!("  Domains: {:?}", domain_list);
    println!("  Owner: {}", owner_pkh);

    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);
    let address = keypair.address_bech32(ctx.pallas_network());

    // Get UTXOs
    let utxos = client.get_utxos(&address).await?;
    println!("  Found {} UTXOs at wallet", utxos.len());

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
                .find(|u| u.lovelace >= 10_000_000 && u.assets.is_empty())
                .cloned()
                .ok_or_else(|| anyhow!("No suitable UTXO found (need >= 10 ADA without assets)"))?
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
        .ok_or_else(|| anyhow!("No suitable collateral UTXO found"))?;

    println!("  Input UTXO: {}#{}", input_utxo.tx_hash, input_utxo.output_index);
    println!("  Collateral: {}#{}", collateral_utxo.tx_hash, collateral_utxo.output_index);

    // Encode output reference for state NFT parameter
    let output_ref_cbor = encode_output_reference(&input_utxo.tx_hash, input_utxo.output_index)?;
    let output_ref_hex = hex::encode(&output_ref_cbor);
    println!("  OutputRef CBOR: {}", output_ref_hex.yellow());

    // Apply parameter to state_nft minting policy
    println!("\n{}", "Applying state_nft parameter...".cyan());
    let applied = apply_validator_param(&ctx.contracts_dir, "state_nft", "state_nft", &output_ref_hex)?;
    println!("  State NFT Policy ID: {}", applied.policy_id.green());

    // Get ISM script address
    let deployment = ctx.load_deployment_info()
        .with_context(|| "Run 'deploy extract' first")?;
    let ism_addr = deployment
        .ism
        .as_ref()
        .map(|m| m.address.clone())
        .ok_or_else(|| anyhow!("ISM address not found in deployment info"))?;
    println!("  ISM Address: {}", ism_addr);

    // Parse validators if provided, otherwise use empty lists
    let validator_map: Vec<(u32, Vec<String>)> = if let Some(v) = validators {
        parse_domain_map(&v)?
    } else {
        domain_list.iter().map(|d| (*d, vec![])).collect()
    };

    // Parse thresholds
    let threshold_map: Vec<(u32, u32)> = if let Some(t) = thresholds {
        parse_threshold_map(&t)?
    } else {
        domain_list.iter().map(|d| (*d, 1)).collect()
    };

    // Build ISM datum
    let datum_cbor = build_ism_datum(&validator_map, &threshold_map, &owner_pkh)?;
    println!("  Datum CBOR: {}...", hex::encode(&datum_cbor[..32.min(datum_cbor.len())]));

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        println!("\nTransaction would:");
        println!("  - Spend UTXO {}#{}", input_utxo.tx_hash, input_utxo.output_index);
        println!("  - Mint state NFT with policy {}", applied.policy_id);
        println!("  - Create output at {} with 5 ADA + NFT + datum", ism_addr);
        return Ok(());
    }

    // Build and submit transaction
    println!("\n{}", "Building transaction...".cyan());
    let mint_script_cbor = hex::decode(&applied.compiled_code)
        .with_context(|| "Invalid script CBOR")?;

    let tx_builder = HyperlaneTxBuilder::new(&client, ctx.pallas_network());
    let built_tx = tx_builder
        .build_init_tx(
            &keypair,
            &input_utxo,
            &collateral_utxo,
            &mint_script_cbor,
            &ism_addr,
            &datum_cbor,
            5_000_000, // 5 ADA output
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

    // Update deployment info
    let mut deployment = deployment;
    if let Some(ref mut ism) = deployment.ism {
        ism.state_nft_policy = Some(applied.policy_id.clone());
    }
    ctx.save_deployment_info(&deployment)?;
    println!("\n{}", "✓ Deployment info updated".green());

    Ok(())
}

async fn init_registry(ctx: &CliContext, utxo: Option<String>, dry_run: bool) -> Result<()> {
    println!("{}", "Initializing Registry contract...".cyan());

    let api_key = ctx.require_api_key()?;
    let keypair = ctx.load_signing_key()?;
    let owner_pkh = keypair.verification_key_hash_hex();

    println!("  Owner: {}", owner_pkh);

    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);
    let address = keypair.address_bech32(ctx.pallas_network());

    // Get UTXOs
    let utxos = client.get_utxos(&address).await?;
    println!("  Found {} UTXOs at wallet", utxos.len());

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
                .find(|u| u.lovelace >= 10_000_000 && u.assets.is_empty())
                .cloned()
                .ok_or_else(|| anyhow!("No suitable UTXO found (need >= 10 ADA without assets)"))?
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
        .ok_or_else(|| anyhow!("No suitable collateral UTXO found"))?;

    println!("  Input UTXO: {}#{}", input_utxo.tx_hash, input_utxo.output_index);
    println!("  Collateral: {}#{}", collateral_utxo.tx_hash, collateral_utxo.output_index);

    // Encode output reference for state NFT parameter
    let output_ref_cbor = encode_output_reference(&input_utxo.tx_hash, input_utxo.output_index)?;
    let output_ref_hex = hex::encode(&output_ref_cbor);
    println!("  OutputRef CBOR: {}", output_ref_hex.yellow());

    // Apply parameter to state_nft minting policy
    println!("\n{}", "Applying state_nft parameter...".cyan());
    let applied = apply_validator_param(&ctx.contracts_dir, "state_nft", "state_nft", &output_ref_hex)?;
    println!("  State NFT Policy ID: {}", applied.policy_id.green());

    // Get Registry script address
    let deployment = ctx.load_deployment_info()
        .with_context(|| "Run 'deploy extract' first")?;
    let registry_addr = deployment
        .registry
        .as_ref()
        .map(|m| m.address.clone())
        .ok_or_else(|| anyhow!("Registry address not found in deployment info"))?;
    println!("  Registry Address: {}", registry_addr);

    // Empty registry datum
    let datum_cbor = crate::utils::cbor::build_registry_datum(&[], &owner_pkh)?;
    println!("  Datum CBOR: {}...", hex::encode(&datum_cbor[..32.min(datum_cbor.len())]));

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        println!("\nTransaction would:");
        println!("  - Spend UTXO {}#{}", input_utxo.tx_hash, input_utxo.output_index);
        println!("  - Mint state NFT with policy {}", applied.policy_id);
        println!("  - Create output at {} with 5 ADA + NFT + datum", registry_addr);
        return Ok(());
    }

    // Build and submit transaction
    println!("\n{}", "Building transaction...".cyan());
    let mint_script_cbor = hex::decode(&applied.compiled_code)
        .with_context(|| "Invalid script CBOR")?;

    let tx_builder = HyperlaneTxBuilder::new(&client, ctx.pallas_network());
    let built_tx = tx_builder
        .build_init_tx(
            &keypair,
            &input_utxo,
            &collateral_utxo,
            &mint_script_cbor,
            &registry_addr,
            &datum_cbor,
            5_000_000, // 5 ADA output
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

    // Update deployment info
    let mut deployment = deployment;
    if let Some(ref mut registry) = deployment.registry {
        registry.state_nft_policy = Some(applied.policy_id.clone());
    }
    ctx.save_deployment_info(&deployment)?;
    println!("\n{}", "✓ Deployment info updated".green());

    Ok(())
}

async fn init_recipient(
    ctx: &CliContext,
    mailbox_hash: Option<String>,
    custom_ism: Option<String>,
    utxo: Option<String>,
    output_lovelace: u64,
    ref_script_lovelace: u64,
    nft_script: Option<String>,
    recipient_script: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Initializing Generic Recipient contract (two-UTXO pattern)...".cyan());
    println!("{}", "This will create:".cyan());
    println!("  - State UTXO: script address + state NFT + datum");
    println!("  - Reference Script UTXO: deployer address + ref NFT + script");

    let api_key = ctx.require_api_key()?;
    let keypair = ctx.load_signing_key()?;

    // Load deployment info to get mailbox hash if not provided
    let deployment = ctx.load_deployment_info()
        .with_context(|| "Run 'deploy extract' first")?;

    let mailbox_hash = match mailbox_hash {
        Some(h) => h,
        None => deployment
            .mailbox
            .as_ref()
            .map(|m| m.hash.clone())
            .ok_or_else(|| anyhow!("Mailbox hash not found. Use --mailbox-hash or run 'deploy extract' first"))?,
    };

    println!("\n{}", "Configuration:".cyan());
    println!("  Mailbox Hash: {}", mailbox_hash);
    if let Some(ref ism) = custom_ism {
        println!("  Custom ISM: {}", ism);
    }
    println!("  State UTXO lovelace: {} ADA", output_lovelace / 1_000_000);
    println!("  Ref Script UTXO lovelace: {} ADA", ref_script_lovelace / 1_000_000);

    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);
    let address = keypair.address_bech32(ctx.pallas_network());

    // Get UTXOs
    let utxos = client.get_utxos(&address).await?;
    println!("  Found {} UTXOs at wallet", utxos.len());

    // Calculate minimum required lovelace
    let min_required = output_lovelace + ref_script_lovelace + 5_000_000; // +5 ADA for fees

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
        .ok_or_else(|| anyhow!("No suitable collateral UTXO found"))?;

    println!("  Input UTXO: {}#{} ({} ADA)", input_utxo.tx_hash, input_utxo.output_index, input_utxo.lovelace / 1_000_000);
    println!("  Collateral: {}#{}", collateral_utxo.tx_hash, collateral_utxo.output_index);

    // Get applied scripts - either from files or by running aiken
    let (nft_policy_id, nft_compiled_code, recipient_hash, recipient_compiled_code) =
        if let (Some(nft_file), Some(recipient_file)) = (&nft_script, &recipient_script) {
            // Load pre-applied scripts from files
            println!("\n{}", "Loading pre-applied scripts...".cyan());

            let nft_content = std::fs::read_to_string(nft_file)
                .with_context(|| format!("Failed to read NFT script file: {}", nft_file))?;
            let nft_json: serde_json::Value = serde_json::from_str(&nft_content)?;
            let nft_cbor = nft_json["cborHex"].as_str()
                .ok_or_else(|| anyhow!("Missing cborHex in NFT script file"))?;
            let nft_policy = crate::utils::crypto::script_hash_from_hex(nft_cbor)?;
            let nft_policy_hex = hex::encode(nft_policy);
            println!("  NFT Script: {}", nft_file);
            println!("  NFT Policy ID: {}", nft_policy_hex.green());

            let recipient_content = std::fs::read_to_string(recipient_file)
                .with_context(|| format!("Failed to read recipient script file: {}", recipient_file))?;
            let recipient_json: serde_json::Value = serde_json::from_str(&recipient_content)?;
            let recipient_cbor = recipient_json["cborHex"].as_str()
                .ok_or_else(|| anyhow!("Missing cborHex in recipient script file"))?;
            let recipient_hash_bytes = crate::utils::crypto::script_hash_from_hex(recipient_cbor)?;
            let recipient_hash_hex = hex::encode(recipient_hash_bytes);
            println!("  Recipient Script: {}", recipient_file);
            println!("  Recipient Script Hash: {}", recipient_hash_hex.green());

            (nft_policy_hex, nft_cbor.to_string(), recipient_hash_hex, recipient_cbor.to_string())
        } else {
            // Apply parameters using aiken
            let output_ref_cbor = encode_output_reference(&input_utxo.tx_hash, input_utxo.output_index)?;
            let output_ref_hex = hex::encode(&output_ref_cbor);
            println!("  OutputRef CBOR: {}", output_ref_hex.yellow());

            println!("\n{}", "Applying state_nft parameter...".cyan());
            let nft_applied = apply_validator_param(&ctx.contracts_dir, "state_nft", "state_nft", &output_ref_hex)?;
            println!("  State NFT Policy ID: {}", nft_applied.policy_id.green());

            println!("\n{}", "Applying generic_recipient parameter...".cyan());
            let mailbox_hash_cbor = encode_script_hash_param(&mailbox_hash)?;
            let recipient_applied = apply_validator_param(
                &ctx.contracts_dir,
                "generic_recipient",
                "generic_recipient",
                &mailbox_hash_cbor,
            )?;
            println!("  Recipient Script Hash: {}", recipient_applied.policy_id.green());

            (nft_applied.policy_id, nft_applied.compiled_code, recipient_applied.policy_id, recipient_applied.compiled_code)
        };

    // Compute recipient address
    let recipient_addr = crate::utils::plutus::script_hash_to_address(
        &recipient_hash,
        ctx.pallas_network(),
    )?;
    println!("  Recipient Address: {}", recipient_addr);

    // Build recipient datum
    let datum_cbor = build_generic_recipient_datum(custom_ism.as_deref(), 0)?;
    println!("  Datum CBOR: {}...", hex::encode(&datum_cbor[..32.min(datum_cbor.len())]));

    // Reference script NFT asset name is "ref" (726566 in hex)
    let ref_asset_name = "726566";

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        println!("\nTransaction would:");
        println!("  - Spend UTXO {}#{}", input_utxo.tx_hash, input_utxo.output_index);
        println!("  - Mint TWO NFTs with policy {}:", nft_policy_id);
        println!("    - State NFT (empty asset name) -> script address");
        println!("    - Ref NFT (asset name 'ref') -> deployer address");
        println!("  - Create state UTXO at {} with {} ADA + state NFT + datum", recipient_addr, output_lovelace / 1_000_000);
        println!("  - Create ref script UTXO at {} with {} ADA + ref NFT + script", address, ref_script_lovelace / 1_000_000);
        println!("\nTo register this recipient, run:");
        println!("  hyperlane-cardano registry register \\");
        println!("    --script-hash {} \\", recipient_hash);
        println!("    --state-policy {} \\", nft_policy_id);
        println!("    --state-asset \"\" \\");
        println!("    --ref-script-policy {} \\", nft_policy_id);
        println!("    --ref-script-asset {}", ref_asset_name);
        return Ok(());
    }

    // Build and submit transaction using the two-UTXO pattern
    println!("\n{}", "Building two-UTXO transaction...".cyan());
    let mint_script_cbor = hex::decode(&nft_compiled_code)
        .with_context(|| "Invalid NFT script CBOR")?;
    let recipient_script_cbor = hex::decode(&recipient_compiled_code)
        .with_context(|| "Invalid recipient script CBOR")?;

    let tx_builder = HyperlaneTxBuilder::new(&client, ctx.pallas_network());
    let built_tx = tx_builder
        .build_init_recipient_two_utxo_tx(
            &keypair,
            &input_utxo,
            &collateral_utxo,
            &mint_script_cbor,
            &recipient_script_cbor,
            &recipient_addr,
            &datum_cbor,
            output_lovelace,
            ref_script_lovelace,
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

    // Output deployment info
    println!("\n{}", "═══════════════════════════════════════════════════════════════".green());
    println!("{}", "Recipient Deployment Summary (Two-UTXO Pattern)".green().bold());
    println!("{}", "═══════════════════════════════════════════════════════════════".green());
    println!();
    println!("{}", "Script Info:".cyan());
    println!("  Script Hash: {}", recipient_hash.green());
    println!("  Address: {}", recipient_addr);
    println!();
    println!("{}", "State UTXO (output #0):".cyan());
    println!("  NFT Policy: {}", nft_policy_id.green());
    println!("  NFT Asset Name: (empty)");
    println!("  Location: {}", recipient_addr);
    println!();
    println!("{}", "Reference Script UTXO (output #1):".cyan());
    println!("  NFT Policy: {}", nft_policy_id.green());
    println!("  NFT Asset Name: {} (\"ref\")", ref_asset_name);
    println!("  Location: {}", address);
    println!();
    println!("{}", "═══════════════════════════════════════════════════════════════".green());
    println!("{}", "To register this recipient with the Hyperlane registry, run:".yellow());
    println!("{}", "═══════════════════════════════════════════════════════════════".green());
    println!();
    println!("  hyperlane-cardano registry register \\");
    println!("    --script-hash {} \\", recipient_hash);
    println!("    --state-policy {} \\", nft_policy_id);
    println!("    --state-asset \"\" \\");
    println!("    --ref-script-policy {} \\", nft_policy_id);
    println!("    --ref-script-asset {}", ref_asset_name);
    println!();

    Ok(())
}

/// Encode a script hash (28 bytes) as a CBOR parameter for aiken blueprint apply
fn encode_script_hash_param(hash_hex: &str) -> Result<String> {
    // Script hash is 28 bytes, encoded as a CBOR bytestring
    let hash_bytes = hex::decode(hash_hex)
        .map_err(|e| anyhow!("Invalid script hash hex: {}", e))?;

    if hash_bytes.len() != 28 {
        return Err(anyhow!("Script hash must be 28 bytes, got {}", hash_bytes.len()));
    }

    // CBOR encode as bytestring: 0x58 1c <28 bytes>
    let mut cbor = vec![0x58, 0x1c]; // bytestring with 1-byte length = 28
    cbor.extend_from_slice(&hash_bytes);

    Ok(hex::encode(cbor))
}

async fn init_all(
    ctx: &CliContext,
    domain: u32,
    origin_domains: &str,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Initializing all core contracts...".cyan());
    println!("  Cardano Domain: {}", domain);
    println!("  Origin Domains: {}", origin_domains);

    // Load deployment info to get script hashes
    let deployment = ctx.load_deployment_info()
        .with_context(|| "Run 'deploy extract' first to generate deployment info")?;

    let ism_hash = deployment
        .ism
        .as_ref()
        .map(|i| i.hash.clone())
        .ok_or_else(|| anyhow!("ISM hash not found in deployment info"))?;

    println!("\n{}", "1. Initializing ISM...".cyan());
    init_ism(ctx, origin_domains, None, None, None, dry_run).await?;

    println!("\n{}", "2. Initializing Mailbox...".cyan());
    init_mailbox(ctx, domain, &ism_hash, None, dry_run).await?;

    println!("\n{}", "3. Initializing Registry...".cyan());
    init_registry(ctx, None, dry_run).await?;

    println!("\n{}", "✓ All contracts prepared for initialization".green().bold());

    Ok(())
}

async fn show_status(ctx: &CliContext) -> Result<()> {
    println!("{}", "Checking contract initialization status...".cyan());

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    // Load deployment info
    let deployment = match ctx.load_deployment_info() {
        Ok(d) => d,
        Err(_) => {
            println!("\n{}", "No deployment info found.".yellow());
            println!("Run 'deploy extract' first.");
            return Ok(());
        }
    };

    println!("\n{}", "Contract Status:".green());
    println!("{}", "-".repeat(60));

    // Check each contract
    for (name, script_opt) in [
        ("Mailbox", &deployment.mailbox),
        ("ISM", &deployment.ism),
        ("Registry", &deployment.registry),
    ] {
        if let Some(script) = script_opt {
            let utxos = client.get_utxos(&script.address).await?;
            let initialized = !utxos.is_empty();

            let status = if initialized {
                format!("{} ({})", "Initialized".green(), utxos.len())
            } else {
                "Not initialized".red().to_string()
            };

            println!("{:<12} {}", format!("{}:", name).bold(), status);
            println!("             Address: {}", script.address);

            if let Some(policy) = &script.state_nft_policy {
                println!("             NFT Policy: {}", policy);
            }
        } else {
            println!("{:<12} {}", format!("{}:", name).bold(), "Not deployed".red());
        }
    }

    Ok(())
}

async fn generate_datums(
    ctx: &CliContext,
    domain: u32,
    ism_hash: &str,
    owner: Option<String>,
    output: Option<String>,
) -> Result<()> {
    println!("{}", "Generating initial datums...".cyan());

    let owner_pkh = match owner {
        Some(o) => o,
        None => {
            let keypair = ctx.load_signing_key()?;
            keypair.verification_key_hash_hex()
        }
    };

    let output_dir = match output {
        Some(p) => std::path::PathBuf::from(p),
        None => ctx.ensure_deployments_dir()?,
    };

    // Mailbox datum
    let merkle_root = "0".repeat(64);
    let mailbox_datum = build_mailbox_datum(domain, ism_hash, &owner_pkh, 0, &merkle_root, 0)?;

    let mailbox_json = serde_json::json!({
        "constructor": 0,
        "fields": [
            {"int": domain},
            {"bytes": ism_hash},
            {"bytes": owner_pkh},
            {"int": 0},
            {"bytes": merkle_root},
            {"int": 0}
        ]
    });

    std::fs::write(
        output_dir.join("mailbox_datum.json"),
        serde_json::to_string_pretty(&mailbox_json)?,
    )?;
    std::fs::write(
        output_dir.join("mailbox_datum.cbor"),
        hex::encode(&mailbox_datum),
    )?;

    // ISM datum (empty validators)
    let ism_json = serde_json::json!({
        "constructor": 0,
        "fields": [
            {"list": []},
            {"list": []},
            {"bytes": owner_pkh}
        ]
    });

    std::fs::write(
        output_dir.join("ism_datum.json"),
        serde_json::to_string_pretty(&ism_json)?,
    )?;

    // Registry datum (empty)
    let registry_json = serde_json::json!({
        "constructor": 0,
        "fields": [
            {"list": []},
            {"bytes": owner_pkh}
        ]
    });

    std::fs::write(
        output_dir.join("registry_datum.json"),
        serde_json::to_string_pretty(&registry_json)?,
    )?;

    // Mint redeemer
    let mint_redeemer = serde_json::json!({
        "constructor": 0,
        "fields": []
    });

    std::fs::write(
        output_dir.join("mint_redeemer.json"),
        serde_json::to_string_pretty(&mint_redeemer)?,
    )?;

    println!("{}", "✓ Datums generated:".green());
    println!("  {:?}/mailbox_datum.json", output_dir);
    println!("  {:?}/ism_datum.json", output_dir);
    println!("  {:?}/registry_datum.json", output_dir);
    println!("  {:?}/mint_redeemer.json", output_dir);

    Ok(())
}

// Helper functions

fn parse_domain_map(s: &str) -> Result<Vec<(u32, Vec<String>)>> {
    // Format: "domain:addr1,addr2;domain2:addr3,addr4"
    let mut result = Vec::new();

    for part in s.split(';') {
        let mut iter = part.split(':');
        let domain: u32 = iter
            .next()
            .ok_or_else(|| anyhow!("Missing domain"))?
            .trim()
            .parse()?;
        let addrs: Vec<String> = iter
            .next()
            .map(|s| s.split(',').map(|a| a.trim().to_string()).collect())
            .unwrap_or_default();
        result.push((domain, addrs));
    }

    Ok(result)
}

fn parse_threshold_map(s: &str) -> Result<Vec<(u32, u32)>> {
    // Format: "domain:threshold;domain2:threshold2"
    let mut result = Vec::new();

    for part in s.split(';') {
        let mut iter = part.split(':');
        let domain: u32 = iter
            .next()
            .ok_or_else(|| anyhow!("Missing domain"))?
            .trim()
            .parse()?;
        let threshold: u32 = iter
            .next()
            .ok_or_else(|| anyhow!("Missing threshold"))?
            .trim()
            .parse()?;
        result.push((domain, threshold));
    }

    Ok(result)
}

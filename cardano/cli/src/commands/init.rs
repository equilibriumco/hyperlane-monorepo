//! Init command - Initialize contracts with state NFTs and initial datums

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};
use colored::Colorize;

use crate::utils::blockfrost::BlockfrostClient;
use crate::utils::cbor::{build_deferred_recipient_datum, build_generic_recipient_datum, build_igp_datum, build_ism_datum, build_mailbox_datum};
use crate::utils::context::CliContext;
use crate::utils::plutus::{apply_validator_param, apply_validator_params, encode_output_reference, encode_script_hash_param, script_hash_to_address};
use crate::utils::tx_builder::HyperlaneTxBuilder;
use crate::utils::types::{AppliedParameter, StateNftInfo};

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

        /// Processed messages script hash (28 bytes hex)
        /// This is the address where processed message markers are stored.
        /// Defaults to the registry script hash if not provided.
        #[arg(long)]
        processed_messages_hash: Option<String>,

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

    /// Initialize a recipient contract
    ///
    /// Creates the two-UTXO pattern required for reference scripts:
    /// - State UTXO: at script address with state NFT + datum
    /// - Reference Script UTXO: at deployer address with ref NFT + script attached
    ///
    /// By default, deploys the built-in example_generic_recipient.
    /// For custom recipients, use --custom-contracts, --custom-module, and --custom-validator.
    Recipient {
        /// Mailbox policy ID (required to parameterize the recipient)
        #[arg(long)]
        mailbox_hash: Option<String>,

        /// Custom ISM script hash (optional, uses default if not specified)
        #[arg(long)]
        custom_ism: Option<String>,

        /// Deploy example_deferred_recipient instead of example_generic_recipient
        /// This also deploys the stored_message_nft policy for deferred processing
        #[arg(long)]
        deferred: bool,

        /// Path to custom Aiken contracts directory (containing plutus.json)
        /// If not specified, uses the built-in example_generic_recipient (or example_deferred_recipient with --deferred)
        #[arg(long = "custom-contracts")]
        custom_contracts: Option<String>,

        /// Module name in the blueprint (required with --custom-contracts)
        #[arg(long = "custom-module")]
        custom_module: Option<String>,

        /// Validator name in the blueprint (required with --custom-contracts)
        #[arg(long = "custom-validator")]
        custom_validator: Option<String>,

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

    /// Initialize the IGP (Interchain Gas Paymaster) contract
    Igp {
        /// Beneficiary address for claimed fees (defaults to signer's pkh)
        #[arg(long)]
        beneficiary: Option<String>,

        /// Default gas limit for messages
        #[arg(long, default_value = "200000")]
        default_gas_limit: u64,

        /// Gas oracle config: "domain:gas_price:exchange_rate" (repeatable)
        #[arg(long = "oracle")]
        oracles: Vec<String>,

        /// UTXO to use for minting state NFT (tx_hash#index)
        #[arg(long)]
        utxo: Option<String>,

        /// Dry run - show what would be done without submitting
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
            processed_messages_hash,
            utxo,
            dry_run,
        } => init_mailbox(ctx, domain, &ism_hash, processed_messages_hash, utxo, dry_run).await,
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
            deferred,
            custom_contracts,
            custom_module,
            custom_validator,
            utxo,
            output_lovelace,
            ref_script_lovelace,
            nft_script,
            recipient_script,
            dry_run,
        } => init_recipient(ctx, mailbox_hash, custom_ism, deferred, custom_contracts, custom_module, custom_validator, utxo, output_lovelace, ref_script_lovelace, nft_script, recipient_script, dry_run).await,
        InitCommands::Igp {
            beneficiary,
            default_gas_limit,
            oracles,
            utxo,
            dry_run,
        } => init_igp(ctx, beneficiary, default_gas_limit, oracles, utxo, dry_run).await,
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
    _processed_messages_hash: Option<String>,
    utxo: Option<String>,
    dry_run: bool,
) -> Result<()> {
    // Note: processed_messages_hash is now ignored - we derive it from processed_message_nft
    init_mailbox_internal(ctx, domain, ism_hash, utxo, dry_run, &[]).await?;
    Ok(())
}

/// Internal mailbox init that excludes already-spent UTXOs and returns the spent UTXO reference
///
/// The mailbox initialization follows a specific parameterization chain:
/// 1. Create state_nft policy (one-shot) -> mailbox_policy_id
/// 2. Apply mailbox_policy_id to processed_message_nft -> processed_messages_nft_policy
/// 3. Apply processed_messages_nft_policy to mailbox -> final mailbox script
///
/// This ensures replay protection is stable across mailbox upgrades.
async fn init_mailbox_internal(
    ctx: &CliContext,
    domain: u32,
    ism_hash: &str,
    utxo: Option<String>,
    dry_run: bool,
    exclude_utxos: &[String],
) -> Result<Option<String>> {
    println!("{}", "Initializing Mailbox contract...".cyan());
    println!("  Domain: {}", domain);
    println!("  Default ISM: {}", ism_hash);

    let api_key = ctx.require_api_key()?;
    let keypair = ctx.load_signing_key()?;
    let owner_pkh = keypair.verification_key_hash_hex();

    println!("  Owner: {}", owner_pkh);

    // Get deployment info
    let deployment = ctx.load_deployment_info()
        .with_context(|| "Run 'deploy extract' first")?;

    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);
    let address = keypair.address_bech32(ctx.pallas_network());

    // Get UTXOs and filter out already-spent ones
    let all_utxos = client.get_utxos(&address).await?;
    let utxos: Vec<_> = all_utxos
        .into_iter()
        .filter(|u| {
            let utxo_ref = format!("{}#{}", u.tx_hash, u.output_index);
            !exclude_utxos.contains(&utxo_ref)
        })
        .collect();
    println!("  Found {} UTXOs at wallet (excluding {} spent)", utxos.len(), exclude_utxos.len());

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
                .find(|u| u.lovelace >= 10_000_000 && u.assets.is_empty() && u.reference_script.is_none())
                .cloned()
                .ok_or_else(|| anyhow!("No suitable UTXO found (need >= 10 ADA without assets or reference scripts)"))?
        }
    };

    // Find collateral UTXO (must be different from input, must not have reference script)
    let collateral_utxo = utxos
        .iter()
        .find(|u| {
            u.lovelace >= 5_000_000
                && u.assets.is_empty()
                && u.reference_script.is_none()
                && !(u.tx_hash == input_utxo.tx_hash && u.output_index == input_utxo.output_index)
        })
        .cloned()
        .ok_or_else(|| anyhow!("No suitable collateral UTXO found (need a second UTXO with >= 5 ADA without reference scripts)"))?;

    println!("  Input UTXO: {}#{}", input_utxo.tx_hash, input_utxo.output_index);
    println!("  Collateral: {}#{}", collateral_utxo.tx_hash, collateral_utxo.output_index);

    // Encode output reference for state NFT parameter
    let output_ref_cbor = encode_output_reference(&input_utxo.tx_hash, input_utxo.output_index)?;
    let output_ref_hex = hex::encode(&output_ref_cbor);
    println!("  OutputRef CBOR: {}", output_ref_hex.yellow());

    // Step 1: Apply parameter to state_nft minting policy to get mailbox_policy_id
    println!("\n{}", "Step 1: Creating state_nft policy (mailbox_policy_id)...".cyan());
    let applied_nft = apply_validator_param(&ctx.contracts_dir, "state_nft", "state_nft", &output_ref_hex)?;
    let mailbox_policy_id = applied_nft.policy_id.clone();
    println!("  Mailbox Policy ID: {}", mailbox_policy_id.green());

    // Step 2: Apply mailbox_policy_id to processed_message_nft to get the NFT policy
    println!("\n{}", "Step 2: Creating processed_message_nft policy...".cyan());
    let mailbox_policy_cbor = encode_script_hash_param(&mailbox_policy_id)?;
    let mailbox_policy_hex = hex::encode(&mailbox_policy_cbor);
    let applied_processed_nft = apply_validator_param(&ctx.contracts_dir, "processed_message_nft", "processed_message_nft", &mailbox_policy_hex)?;
    let processed_messages_nft_policy = applied_processed_nft.policy_id.clone();
    println!("  Processed Messages NFT Policy: {}", processed_messages_nft_policy.green());

    // Step 3: Apply processed_messages_nft_policy to mailbox validator
    println!("\n{}", "Step 3: Creating mailbox validator...".cyan());
    let pm_policy_cbor = encode_script_hash_param(&processed_messages_nft_policy)?;
    let pm_policy_hex = hex::encode(&pm_policy_cbor);
    let applied_mailbox = apply_validator_param(&ctx.contracts_dir, "mailbox", "mailbox", &pm_policy_hex)?;
    let mailbox_addr = script_hash_to_address(&applied_mailbox.policy_id, ctx.pallas_network())?;
    println!("  Mailbox Script Hash: {}", applied_mailbox.policy_id.green());
    println!("  Mailbox Address: {}", mailbox_addr);

    // Build mailbox datum with empty merkle tree (32 zero branches)
    let zero_branch = "0".repeat(64); // 32 bytes of zeros
    let empty_branches: Vec<&str> = vec![zero_branch.as_str(); 32];
    let datum_cbor = build_mailbox_datum(domain, ism_hash, &owner_pkh, 0, &empty_branches, 0)?;
    println!("  Datum CBOR: {}...", hex::encode(&datum_cbor[..32.min(datum_cbor.len())]));

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        println!("\nTransaction would:");
        println!("  - Spend UTXO {}#{}", input_utxo.tx_hash, input_utxo.output_index);
        println!("  - Mint state NFT with policy {}", applied_nft.policy_id);
        println!("  - Create output at {} with {} ADA + NFT + datum", mailbox_addr, 7);
        println!("\n{}", "Parameterization chain:".green());
        println!("  1. mailbox_policy_id (state NFT): {}", mailbox_policy_id);
        println!("  2. processed_messages_nft_policy: {}", processed_messages_nft_policy);
        println!("  3. Resulting mailbox hash: {}", applied_mailbox.policy_id);
        return Ok(None);
    }

    // Build and submit transaction
    println!("\n{}", "Building transaction...".cyan());
    let mint_script_cbor = hex::decode(&applied_nft.compiled_code)
        .with_context(|| "Invalid script CBOR")?;

    // State NFT asset name for mailbox
    let mailbox_asset_name = "Mailbox State";

    let tx_builder = HyperlaneTxBuilder::new(&client, ctx.pallas_network());
    let built_tx = tx_builder
        .build_init_tx(
            &keypair,
            &input_utxo,
            &collateral_utxo,
            &mint_script_cbor,
            &mailbox_addr,
            &datum_cbor,
            7_000_000, // 7 ADA output (increased for larger merkle tree datum)
            Some(mailbox_asset_name),
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

    // State UTXO reference (first output is the state UTXO)
    let state_utxo_ref = format!("{}#0", tx_hash);

    // Update deployment info with complete initialization details
    let mut deployment = deployment;
    if let Some(ref mut mailbox) = deployment.mailbox {
        // Record the parameter that was applied (now using processed_messages_nft_policy)
        mailbox.applied_parameters = vec![
            AppliedParameter {
                name: "processed_messages_nft_policy".to_string(),
                param_type: "PolicyId".to_string(),
                value: processed_messages_nft_policy.clone(),
                description: Some("Policy ID for processed message NFTs (parameterized by mailbox_policy_id)".to_string()),
            }
        ];

        // Update hash and address to post-parameterization values
        mailbox.hash = applied_mailbox.policy_id.clone();
        mailbox.address = mailbox_addr.clone();

        // Record state NFT info
        mailbox.state_nft = Some(StateNftInfo {
            policy_id: applied_nft.policy_id.clone(),
            asset_name_hex: hex::encode(mailbox_asset_name.as_bytes()),
            asset_name: mailbox_asset_name.to_string(),
            seed_utxo: format!("{}#{}", input_utxo.tx_hash, input_utxo.output_index),
        });

        // Record initialization details
        mailbox.init_tx_hash = Some(tx_hash.clone());
        mailbox.state_utxo = Some(state_utxo_ref.clone());
        mailbox.initialized = true;

        // Legacy fields
        mailbox.utxo = Some(state_utxo_ref);
        mailbox.state_nft_policy = Some(applied_nft.policy_id.clone());
    }
    ctx.save_deployment_info(&deployment)?;
    println!("\n{}", "✓ Deployment info updated".green());
    println!("  Mailbox hash updated to: {}", applied_mailbox.policy_id);

    // Save the applied scripts for reference
    let mailbox_script_path = ctx.network_deployments_dir().join("mailbox_applied.plutus");
    applied_mailbox.save_plutus_file(&mailbox_script_path, "Applied mailbox validator")?;
    println!("  Mailbox script saved to: {:?}", mailbox_script_path);

    let processed_nft_path = ctx.network_deployments_dir().join("processed_message_nft_applied.plutus");
    applied_processed_nft.save_plutus_file(&processed_nft_path, "Applied processed_message_nft minting policy")?;
    println!("  Processed message NFT script saved to: {:?}", processed_nft_path);

    // Return the spent UTXO reference
    Ok(Some(format!("{}#{}", input_utxo.tx_hash, input_utxo.output_index)))
}

async fn init_ism(
    ctx: &CliContext,
    domains: &str,
    validators: Option<String>,
    thresholds: Option<String>,
    utxo: Option<String>,
    dry_run: bool,
) -> Result<()> {
    init_ism_internal(ctx, domains, validators, thresholds, utxo, dry_run, &[]).await?;
    Ok(())
}

/// Internal ISM init that tracks spent UTXOs and returns the spent UTXO reference
async fn init_ism_internal(
    ctx: &CliContext,
    domains: &str,
    validators: Option<String>,
    thresholds: Option<String>,
    utxo: Option<String>,
    dry_run: bool,
    exclude_utxos: &[String],
) -> Result<Option<String>> {
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

    // Get UTXOs and filter out already-spent ones
    let all_utxos = client.get_utxos(&address).await?;
    let utxos: Vec<_> = all_utxos
        .into_iter()
        .filter(|u| {
            let utxo_ref = format!("{}#{}", u.tx_hash, u.output_index);
            !exclude_utxos.contains(&utxo_ref)
        })
        .collect();
    println!("  Found {} UTXOs at wallet (excluding {} spent)", utxos.len(), exclude_utxos.len());

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
                .find(|u| u.lovelace >= 10_000_000 && u.assets.is_empty() && u.reference_script.is_none())
                .cloned()
                .ok_or_else(|| anyhow!("No suitable UTXO found (need >= 10 ADA without assets or reference scripts)"))?
        }
    };

    // Find collateral UTXO (must not have reference script)
    let collateral_utxo = utxos
        .iter()
        .find(|u| {
            u.lovelace >= 5_000_000
                && u.assets.is_empty()
                && u.reference_script.is_none()
                && !(u.tx_hash == input_utxo.tx_hash && u.output_index == input_utxo.output_index)
        })
        .cloned()
        .ok_or_else(|| anyhow!("No suitable collateral UTXO found (without reference scripts)"))?;

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
        return Ok(None);
    }

    // Build and submit transaction
    println!("\n{}", "Building transaction...".cyan());
    let mint_script_cbor = hex::decode(&applied.compiled_code)
        .with_context(|| "Invalid script CBOR")?;

    // State NFT asset name for ISM
    let ism_asset_name = "ISM State";

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
            Some(ism_asset_name),
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

    // State UTXO reference (first output is the state UTXO)
    let state_utxo_ref = format!("{}#0", tx_hash);

    // Update deployment info with complete initialization details
    let mut deployment = deployment;
    if let Some(ref mut ism) = deployment.ism {
        // ISM is not parameterized, so no applied_parameters

        // Record state NFT info
        ism.state_nft = Some(StateNftInfo {
            policy_id: applied.policy_id.clone(),
            asset_name_hex: hex::encode(ism_asset_name.as_bytes()),
            asset_name: ism_asset_name.to_string(),
            seed_utxo: format!("{}#{}", input_utxo.tx_hash, input_utxo.output_index),
        });

        // Record initialization details
        ism.init_tx_hash = Some(tx_hash.clone());
        ism.state_utxo = Some(state_utxo_ref.clone());
        ism.initialized = true;

        // Legacy fields
        ism.utxo = Some(state_utxo_ref);
        ism.state_nft_policy = Some(applied.policy_id.clone());
    }
    ctx.save_deployment_info(&deployment)?;
    println!("\n{}", "✓ Deployment info updated".green());

    // Return the spent UTXO reference
    Ok(Some(format!("{}#{}", input_utxo.tx_hash, input_utxo.output_index)))
}

async fn init_registry(ctx: &CliContext, utxo: Option<String>, dry_run: bool) -> Result<()> {
    init_registry_internal(ctx, utxo, dry_run, &[]).await?;
    Ok(())
}

/// Internal registry init that excludes already-spent UTXOs
async fn init_registry_internal(
    ctx: &CliContext,
    utxo: Option<String>,
    dry_run: bool,
    exclude_utxos: &[String],
) -> Result<Option<String>> {
    println!("{}", "Initializing Registry contract...".cyan());

    let api_key = ctx.require_api_key()?;
    let keypair = ctx.load_signing_key()?;
    let owner_pkh = keypair.verification_key_hash_hex();

    println!("  Owner: {}", owner_pkh);

    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);
    let address = keypair.address_bech32(ctx.pallas_network());

    // Get UTXOs and filter out already-spent ones
    let all_utxos = client.get_utxos(&address).await?;
    let utxos: Vec<_> = all_utxos
        .into_iter()
        .filter(|u| {
            let utxo_ref = format!("{}#{}", u.tx_hash, u.output_index);
            !exclude_utxos.contains(&utxo_ref)
        })
        .collect();
    println!("  Found {} UTXOs at wallet (excluding {} spent)", utxos.len(), exclude_utxos.len());

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
                .find(|u| u.lovelace >= 10_000_000 && u.assets.is_empty() && u.reference_script.is_none())
                .cloned()
                .ok_or_else(|| anyhow!("No suitable UTXO found (need >= 10 ADA without assets or reference scripts)"))?
        }
    };

    // Find collateral UTXO (must not have reference script)
    let collateral_utxo = utxos
        .iter()
        .find(|u| {
            u.lovelace >= 5_000_000
                && u.assets.is_empty()
                && u.reference_script.is_none()
                && !(u.tx_hash == input_utxo.tx_hash && u.output_index == input_utxo.output_index)
        })
        .cloned()
        .ok_or_else(|| anyhow!("No suitable collateral UTXO found (without reference scripts)"))?;

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
        return Ok(None);
    }

    // Build and submit transaction
    println!("\n{}", "Building transaction...".cyan());
    let mint_script_cbor = hex::decode(&applied.compiled_code)
        .with_context(|| "Invalid script CBOR")?;

    // State NFT asset name for Registry
    let registry_asset_name = "Registry State";

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
            Some(registry_asset_name),
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

    // State UTXO reference (first output is the state UTXO)
    let state_utxo_ref = format!("{}#0", tx_hash);

    // Update deployment info with complete initialization details
    let mut deployment = deployment;
    if let Some(ref mut registry) = deployment.registry {
        // Registry is not parameterized, so no applied_parameters

        // Record state NFT info
        registry.state_nft = Some(StateNftInfo {
            policy_id: applied.policy_id.clone(),
            asset_name_hex: hex::encode(registry_asset_name.as_bytes()),
            asset_name: registry_asset_name.to_string(),
            seed_utxo: format!("{}#{}", input_utxo.tx_hash, input_utxo.output_index),
        });

        // Record initialization details
        registry.init_tx_hash = Some(tx_hash.clone());
        registry.state_utxo = Some(state_utxo_ref.clone());
        registry.initialized = true;

        // Legacy fields
        registry.utxo = Some(state_utxo_ref);
        registry.state_nft_policy = Some(applied.policy_id.clone());
    }
    ctx.save_deployment_info(&deployment)?;
    println!("\n{}", "✓ Deployment info updated".green());

    // Return the spent UTXO reference
    Ok(Some(format!("{}#{}", input_utxo.tx_hash, input_utxo.output_index)))
}

async fn init_igp(
    ctx: &CliContext,
    beneficiary: Option<String>,
    default_gas_limit: u64,
    oracles: Vec<String>,
    utxo: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Initializing IGP contract...".cyan());

    let api_key = ctx.require_api_key()?;
    let keypair = ctx.load_signing_key()?;
    let owner_pkh = keypair.verification_key_hash_hex();

    // Determine beneficiary - use provided or default to owner
    let beneficiary_pkh = match beneficiary {
        Some(ref b) => {
            // If it's a bech32 address, extract the pkh; otherwise assume it's already a pkh hex
            if b.starts_with("addr") {
                // For simplicity, we'll just require the pkh directly for now
                return Err(anyhow!("Please provide beneficiary as a 28-byte hex public key hash, not a bech32 address"));
            }
            if b.len() != 56 {
                return Err(anyhow!("Beneficiary must be a 28-byte hex public key hash (56 hex chars)"));
            }
            b.clone()
        }
        None => owner_pkh.clone(),
    };

    // Parse oracle configurations
    let gas_oracles: Vec<(u32, u64, u64)> = oracles
        .iter()
        .map(|s| parse_oracle_config(s))
        .collect::<Result<Vec<_>>>()?;

    println!("  Owner: {}", owner_pkh);
    println!("  Beneficiary: {}", beneficiary_pkh);
    println!("  Default Gas Limit: {}", default_gas_limit);
    println!("  Gas Oracles: {} configured", gas_oracles.len());
    for (domain, gas_price, exchange_rate) in &gas_oracles {
        println!("    - Domain {}: gas_price={}, exchange_rate={}", domain, gas_price, exchange_rate);
    }

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
                .find(|u| u.lovelace >= 10_000_000 && u.assets.is_empty() && u.reference_script.is_none())
                .cloned()
                .ok_or_else(|| anyhow!("No suitable UTXO found (need >= 10 ADA without assets or reference scripts)"))?
        }
    };

    // Find collateral UTXO (must not have reference script)
    let collateral_utxo = utxos
        .iter()
        .find(|u| {
            u.lovelace >= 5_000_000
                && u.assets.is_empty()
                && u.reference_script.is_none()
                && !(u.tx_hash == input_utxo.tx_hash && u.output_index == input_utxo.output_index)
        })
        .cloned()
        .ok_or_else(|| anyhow!("No suitable collateral UTXO found (need a second UTXO with >= 5 ADA without reference scripts)"))?;

    println!("  Input UTXO: {}#{}", input_utxo.tx_hash, input_utxo.output_index);
    println!("  Collateral: {}#{}", collateral_utxo.tx_hash, collateral_utxo.output_index);

    // Encode output reference for state NFT parameter
    let output_ref_cbor = encode_output_reference(&input_utxo.tx_hash, input_utxo.output_index)?;
    let output_ref_hex = hex::encode(&output_ref_cbor);

    // Apply parameter to state_nft minting policy
    println!("\n{}", "Applying state_nft parameter...".cyan());
    let applied = apply_validator_param(&ctx.contracts_dir, "state_nft", "state_nft", &output_ref_hex)?;
    println!("  State NFT Policy ID: {}", applied.policy_id.green());

    // Get IGP script address from deployment_info.json
    let deployment = ctx.load_deployment_info()
        .with_context(|| "Run 'deploy extract' first")?;
    let igp_addr = deployment
        .igp
        .as_ref()
        .map(|i| i.address.clone())
        .ok_or_else(|| anyhow!("IGP address not found in deployment info"))?;
    println!("  IGP Address: {}", igp_addr);

    // Build IGP datum
    let datum_cbor = build_igp_datum(&owner_pkh, &beneficiary_pkh, &gas_oracles, default_gas_limit)?;
    println!("  Datum CBOR: {}...", hex::encode(&datum_cbor[..32.min(datum_cbor.len())]));

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        println!("\nTransaction would:");
        println!("  - Spend UTXO {}#{}", input_utxo.tx_hash, input_utxo.output_index);
        println!("  - Mint state NFT with policy {}", applied.policy_id);
        println!("  - Create output at {} with 5 ADA + NFT + datum", igp_addr);
        return Ok(());
    }

    // Build and submit transaction
    println!("\n{}", "Building transaction...".cyan());
    let mint_script_cbor = hex::decode(&applied.compiled_code)
        .with_context(|| "Invalid script CBOR")?;

    // State NFT asset name for IGP
    let igp_asset_name = "IGP State";

    let tx_builder = HyperlaneTxBuilder::new(&client, ctx.pallas_network());
    let built_tx = tx_builder
        .build_init_tx(
            &keypair,
            &input_utxo,
            &collateral_utxo,
            &mint_script_cbor,
            &igp_addr,
            &datum_cbor,
            5_000_000, // 5 ADA output
            Some(igp_asset_name),
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

    // State UTXO reference (first output is the state UTXO)
    let state_utxo_ref = format!("{}#0", tx_hash);

    // Update deployment info with complete initialization details
    let mut deployment = deployment;
    if let Some(ref mut igp) = deployment.igp {
        // IGP is not parameterized, so no applied_parameters

        // Record state NFT info
        igp.state_nft = Some(StateNftInfo {
            policy_id: applied.policy_id.clone(),
            asset_name_hex: hex::encode(igp_asset_name.as_bytes()),
            asset_name: igp_asset_name.to_string(),
            seed_utxo: format!("{}#{}", input_utxo.tx_hash, input_utxo.output_index),
        });

        // Record initialization details
        igp.init_tx_hash = Some(tx_hash.clone());
        igp.state_utxo = Some(state_utxo_ref.clone());
        igp.initialized = true;

        // Legacy fields
        igp.utxo = Some(state_utxo_ref);
        igp.state_nft_policy = Some(applied.policy_id.clone());
    }
    ctx.save_deployment_info(&deployment)?;
    println!("\n{}", "✓ Deployment info updated".green());
    println!("  IGP State NFT Policy: {}", applied.policy_id);
    println!("  IGP State UTXO: {}#0", tx_hash);
    println!("  IGP Initialized: true");

    Ok(())
}

/// Parse oracle config string "domain:gas_price:exchange_rate"
fn parse_oracle_config(s: &str) -> Result<(u32, u64, u64)> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return Err(anyhow!(
            "Invalid oracle format: '{}'. Expected 'domain:gas_price:exchange_rate'",
            s
        ));
    }

    let domain: u32 = parts[0]
        .trim()
        .parse()
        .with_context(|| format!("Invalid domain in oracle config: '{}'", parts[0]))?;
    let gas_price: u64 = parts[1]
        .trim()
        .parse()
        .with_context(|| format!("Invalid gas_price in oracle config: '{}'", parts[1]))?;
    let exchange_rate: u64 = parts[2]
        .trim()
        .parse()
        .with_context(|| format!("Invalid exchange_rate in oracle config: '{}'", parts[2]))?;

    Ok((domain, gas_price, exchange_rate))
}

async fn init_recipient(
    ctx: &CliContext,
    mailbox_hash: Option<String>,
    custom_ism: Option<String>,
    deferred: bool,
    custom_contracts: Option<String>,
    custom_module: Option<String>,
    custom_validator: Option<String>,
    utxo: Option<String>,
    output_lovelace: u64,
    ref_script_lovelace: u64,
    nft_script: Option<String>,
    recipient_script: Option<String>,
    dry_run: bool,
) -> Result<()> {
    // Determine if using custom contracts or built-in
    let (use_custom, custom_contracts_path, module_name, validator_name) = match (&custom_contracts, &custom_module, &custom_validator) {
        (Some(dir), Some(m), Some(v)) => (true, dir.clone(), m.clone(), v.clone()),
        (None, None, None) => (false, String::new(), String::new(), String::new()),
        _ => {
            return Err(anyhow!(
                "--custom-contracts, --custom-module, and --custom-validator must all be specified together for custom recipients"
            ));
        }
    };

    if use_custom {
        println!("{}", format!("Initializing custom recipient '{}' (two-UTXO pattern)...", validator_name).cyan());
    } else if deferred {
        println!("{}", "Initializing Deferred Recipient contract (two-UTXO pattern)...".cyan());
    } else {
        println!("{}", "Initializing Generic Recipient contract (two-UTXO pattern)...".cyan());
    }
    println!("{}", "This will create:".cyan());
    println!("  - State UTXO: script address + state NFT + datum");
    println!("  - Reference Script UTXO: deployer address + ref NFT + script");

    let api_key = ctx.require_api_key()?;
    let keypair = ctx.load_signing_key()?;

    // Load deployment info to get mailbox policy ID if not provided
    let deployment = ctx.load_deployment_info()
        .with_context(|| "Run 'deploy extract' first")?;

    // The recipient needs the mailbox NFT policy ID (not the script hash)
    // to verify the mailbox is calling it
    let mailbox_policy_id = match mailbox_hash {
        Some(h) => h,
        None => deployment
            .mailbox
            .as_ref()
            .and_then(|m| m.state_nft.as_ref().map(|nft| nft.policy_id.clone()))
            .or_else(|| deployment.mailbox.as_ref().and_then(|m| m.state_nft_policy.clone()))
            .ok_or_else(|| anyhow!("Mailbox NFT policy not found. Use --mailbox-hash or ensure mailbox is initialized"))?,
    };

    println!("\n{}", "Configuration:".cyan());
    println!("  Mailbox Policy ID: {}", mailbox_policy_id);
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
                .find(|u| u.lovelace >= min_required && u.assets.is_empty() && u.reference_script.is_none())
                .cloned()
                .ok_or_else(|| anyhow!("No suitable UTXO found (need >= {} ADA without assets or reference scripts)", min_required / 1_000_000))?
        }
    };

    // Find collateral UTXO (must not have reference script)
    let collateral_utxo = utxos
        .iter()
        .find(|u| {
            u.lovelace >= 5_000_000
                && u.assets.is_empty()
                && u.reference_script.is_none()
                && !(u.tx_hash == input_utxo.tx_hash && u.output_index == input_utxo.output_index)
        })
        .cloned()
        .ok_or_else(|| anyhow!("No suitable collateral UTXO found (without reference scripts)"))?;

    println!("  Input UTXO: {}#{} ({} ADA)", input_utxo.tx_hash, input_utxo.output_index, input_utxo.lovelace / 1_000_000);
    println!("  Collateral: {}#{}", collateral_utxo.tx_hash, collateral_utxo.output_index);

    // Get applied scripts - either from files or by running aiken
    // For deferred recipients, we also get the stored_message_nft compiled code for reference script deployment
    let (nft_policy_id, nft_compiled_code, recipient_hash, recipient_compiled_code, msg_nft_policy, msg_nft_compiled_code) =
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

            // Pre-applied scripts don't have msg_nft_policy tracking
            (nft_policy_hex, nft_cbor.to_string(), recipient_hash_hex, recipient_cbor.to_string(), None, None)
        } else {
            // Apply parameters using aiken
            let output_ref_cbor = encode_output_reference(&input_utxo.tx_hash, input_utxo.output_index)?;
            let output_ref_hex = hex::encode(&output_ref_cbor);
            println!("  OutputRef CBOR: {}", output_ref_hex.yellow());

            // State NFT is always from the Hyperlane contracts
            println!("\n{}", "Applying state_nft parameter...".cyan());
            let nft_applied = apply_validator_param(&ctx.contracts_dir, "state_nft", "state_nft", &output_ref_hex)?;
            println!("  State NFT Policy ID: {}", nft_applied.policy_id.green());

            // Recipient can be from custom contracts or built-in
            let mailbox_policy_cbor = encode_script_hash_param(&mailbox_policy_id)?;
            let mailbox_policy_cbor_hex = hex::encode(&mailbox_policy_cbor);

            // Returns (recipient_applied, msg_nft_policy, msg_nft_compiled_code)
            let (recipient_applied, msg_nft_policy, msg_nft_compiled_code) = if use_custom {
                println!("\n{}", format!("Applying {}.{} parameter from custom contracts...", module_name, validator_name).cyan());
                let custom_path = std::path::Path::new(&custom_contracts_path);
                let applied = apply_validator_param(
                    custom_path,
                    &module_name,
                    &validator_name,
                    &mailbox_policy_cbor_hex,
                )?;
                (applied, None, None)
            } else if deferred {
                // Deferred recipient requires two steps:
                // 1. Apply stored_message_nft with mailbox_policy_id to get message NFT policy
                // 2. Apply example_deferred_recipient with both mailbox_policy_id and message_nft_policy
                println!("\n{}", "Applying stored_message_nft parameter...".cyan());
                let stored_msg_nft = apply_validator_param(
                    &ctx.contracts_dir,
                    "stored_message_nft",
                    "stored_message_nft",
                    &mailbox_policy_cbor_hex,
                )?;
                println!("  Stored Message NFT Policy: {}", stored_msg_nft.policy_id.green());

                // Encode the message NFT policy as CBOR for second parameter
                let msg_nft_cbor = encode_script_hash_param(&stored_msg_nft.policy_id)?;
                let msg_nft_cbor_hex = hex::encode(&msg_nft_cbor);

                println!("\n{}", "Applying example_deferred_recipient parameters...".cyan());
                println!("  Parameter 1: mailbox_policy_id = {}", mailbox_policy_id);
                println!("  Parameter 2: message_nft_policy = {}", stored_msg_nft.policy_id);

                let applied = apply_validator_params(
                    &ctx.contracts_dir,
                    "example_deferred_recipient",
                    "example_deferred_recipient",
                    &[&mailbox_policy_cbor_hex, &msg_nft_cbor_hex],
                )?;
                // Return the stored_message_nft compiled code for reference script deployment
                (applied, Some(stored_msg_nft.policy_id), Some(stored_msg_nft.compiled_code))
            } else {
                println!("\n{}", "Applying example_generic_recipient parameter...".cyan());
                let applied = apply_validator_param(
                    &ctx.contracts_dir,
                    "example_generic_recipient",
                    "example_generic_recipient",
                    &mailbox_policy_cbor_hex,
                )?;
                (applied, None, None)
            };
            println!("  Recipient Script Hash: {}", recipient_applied.policy_id.green());

            (nft_applied.policy_id, nft_applied.compiled_code, recipient_applied.policy_id, recipient_applied.compiled_code, msg_nft_policy, msg_nft_compiled_code)
        };

    // Compute recipient address
    let recipient_addr = crate::utils::plutus::script_hash_to_address(
        &recipient_hash,
        ctx.pallas_network(),
    )?;
    println!("  Recipient Address: {}", recipient_addr);

    // Build recipient datum (different structure for deferred vs generic)
    let datum_cbor = if deferred {
        build_deferred_recipient_datum(custom_ism.as_deref(), 0, 0)?
    } else {
        build_generic_recipient_datum(custom_ism.as_deref(), 0)?
    };
    println!("  Datum CBOR: {}...", hex::encode(&datum_cbor[..32.min(datum_cbor.len())]));

    // Reference script NFT asset name is "ref" (726566 in hex)
    let ref_asset_name = "726566";

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        println!("\nTransaction would:");
        println!("  - Spend UTXO {}#{}", input_utxo.tx_hash, input_utxo.output_index);
        if msg_nft_compiled_code.is_some() {
            println!("  - Mint THREE NFTs with policy {}:", nft_policy_id);
            println!("    - State NFT (empty asset name) -> script address");
            println!("    - Ref NFT (asset name 'ref') -> deployer address");
            println!("    - Msg Ref NFT (asset name 'msg_ref') -> deployer address");
            println!("  - Create state UTXO at {} with {} ADA + state NFT + datum", recipient_addr, output_lovelace / 1_000_000);
            println!("  - Create recipient ref script UTXO at {} with {} ADA + ref NFT + script", address, ref_script_lovelace / 1_000_000);
            println!("  - Create message NFT ref script UTXO at {} with 20 ADA + msg_ref NFT + stored_message_nft script", address);
        } else {
            println!("  - Mint TWO NFTs with policy {}:", nft_policy_id);
            println!("    - State NFT (empty asset name) -> script address");
            println!("    - Ref NFT (asset name 'ref') -> deployer address");
            println!("  - Create state UTXO at {} with {} ADA + state NFT + datum", recipient_addr, output_lovelace / 1_000_000);
            println!("  - Create ref script UTXO at {} with {} ADA + ref NFT + script", address, ref_script_lovelace / 1_000_000);
        }
        println!("\nTo register this recipient, run:");
        println!("  hyperlane-cardano registry register \\");
        println!("    --script-hash {} \\", recipient_hash);
        if deferred {
            println!("    --recipient-type deferred \\");
            if let Some(ref policy) = msg_nft_policy {
                println!("    --message-policy {} \\", policy);
            }
        } else {
            println!("    --recipient-type <RECIPIENT_TYPE> \\");
        }
        println!("    --state-policy {} \\", nft_policy_id);
        println!("    --state-asset \"\" \\");
        println!("    --ref-script-policy {} \\", nft_policy_id);
        println!("    --ref-script-asset {} \\", ref_asset_name);
        println!("    --signing-key <path-to-owner-key>");
        println!("\nNote: The signing key's public key hash becomes the registration owner.");
        println!("Only the owner can update or remove this registration.");
        if !deferred {
            println!("RECIPIENT_TYPE can be: generic, token-receiver, deferred");
        }
        return Ok(());
    }

    // Build and submit transaction
    let mint_script_cbor = hex::decode(&nft_compiled_code)
        .with_context(|| "Invalid NFT script CBOR")?;
    let recipient_script_cbor = hex::decode(&recipient_compiled_code)
        .with_context(|| "Invalid recipient script CBOR")?;

    let tx_builder = HyperlaneTxBuilder::new(&client, ctx.pallas_network());

    // Use three-UTXO pattern for deferred recipients (includes stored_message_nft reference script)
    // Use two-UTXO pattern for generic/token recipients
    let built_tx = if let Some(ref msg_nft_code) = msg_nft_compiled_code {
        println!("\n{}", "Building three-UTXO transaction (deferred recipient)...".cyan());
        let msg_nft_script_cbor = hex::decode(msg_nft_code)
            .with_context(|| "Invalid message NFT script CBOR")?;
        let msg_ref_lovelace = 20_000_000u64; // 20 ADA for stored_message_nft reference script
        tx_builder
            .build_init_recipient_three_utxo_tx(
                &keypair,
                &input_utxo,
                &collateral_utxo,
                &mint_script_cbor,
                &recipient_script_cbor,
                &msg_nft_script_cbor,
                &recipient_addr,
                &datum_cbor,
                output_lovelace,
                ref_script_lovelace,
                msg_ref_lovelace,
            )
            .await?
    } else {
        println!("\n{}", "Building two-UTXO transaction...".cyan());
        tx_builder
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
            .await?
    };

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
    let pattern_name = if msg_nft_compiled_code.is_some() { "Three-UTXO" } else { "Two-UTXO" };
    println!("\n{}", "═══════════════════════════════════════════════════════════════".green());
    println!("{}", format!("Recipient Deployment Summary ({} Pattern)", pattern_name).green().bold());
    println!("{}", "═══════════════════════════════════════════════════════════════".green());
    println!();
    println!("{}", "Script Info:".cyan());
    println!("  Script Hash: {}", recipient_hash.green());
    println!("  Address: {}", recipient_addr);
    if let Some(ref policy) = msg_nft_policy {
        println!("  Message NFT Policy: {}", policy.green());
    }
    println!();
    println!("{}", "State UTXO (output #0):".cyan());
    println!("  NFT Policy: {}", nft_policy_id.green());
    println!("  NFT Asset Name: (empty)");
    println!("  Location: {}", recipient_addr);
    println!();
    println!("{}", "Recipient Reference Script UTXO (output #1):".cyan());
    println!("  NFT Policy: {}", nft_policy_id.green());
    println!("  NFT Asset Name: {} (\"ref\")", ref_asset_name);
    println!("  Location: {}", address);
    if msg_nft_compiled_code.is_some() {
        println!();
        let msg_ref_asset_name = "6d73675f726566"; // "msg_ref" in hex
        println!("{}", "Message NFT Reference Script UTXO (output #2):".cyan());
        println!("  NFT Policy: {}", nft_policy_id.green());
        println!("  NFT Asset Name: {} (\"msg_ref\")", msg_ref_asset_name);
        println!("  Location: {}", address);
        println!("  Contains: stored_message_nft minting policy script");
    }
    println!();
    println!("{}", "═══════════════════════════════════════════════════════════════".green());
    println!("{}", "To register this recipient with the Hyperlane registry, run:".yellow());
    println!("{}", "═══════════════════════════════════════════════════════════════".green());
    println!();
    println!("  hyperlane-cardano registry register \\");
    println!("    --script-hash {} \\", recipient_hash);
    if deferred {
        println!("    --recipient-type deferred \\");
        if let Some(ref policy) = msg_nft_policy {
            println!("    --message-policy {} \\", policy);
        }
    } else {
        println!("    --recipient-type <RECIPIENT_TYPE> \\");
    }
    println!("    --state-policy {} \\", nft_policy_id);
    println!("    --state-asset \"\" \\");
    println!("    --ref-script-policy {} \\", nft_policy_id);
    println!("    --ref-script-asset {} \\", ref_asset_name);
    println!("    --signing-key <path-to-owner-key>");
    println!();
    println!("{}", "Note: The signing key's public key hash becomes the registration owner.".cyan());
    println!("{}", "Only the owner can update or remove this registration.".cyan());
    if !deferred {
        println!("{}", "RECIPIENT_TYPE can be: generic, token-receiver, deferred".cyan());
    }
    println!();

    Ok(())
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

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    // Track spent UTXOs to avoid reusing them
    let mut spent_utxos: Vec<String> = Vec::new();

    println!("\n{}", "1. Initializing ISM...".cyan());
    let ism_spent = init_ism_internal(ctx, origin_domains, None, None, None, dry_run, &spent_utxos).await?;
    if let Some(utxo) = ism_spent {
        spent_utxos.push(utxo);
    }

    if !dry_run {
        // Wait for ISM transaction to be confirmed before proceeding
        // Re-load deployment info to get the tx hash
        let deployment = ctx.load_deployment_info()?;
        if let Some(ref ism) = deployment.ism {
            if let Some(ref tx_hash) = ism.init_tx_hash {
                println!("\n{}", "Waiting for ISM transaction confirmation...".yellow());
                wait_for_tx_confirmation(&client, tx_hash).await?;
            }
        }
    }

    println!("\n{}", "2. Initializing Mailbox...".cyan());
    let mailbox_spent = init_mailbox_internal(ctx, domain, &ism_hash, None, dry_run, &spent_utxos).await?;
    if let Some(utxo) = mailbox_spent {
        spent_utxos.push(utxo);
    }

    if !dry_run {
        // Wait for Mailbox transaction to be confirmed before proceeding
        let deployment = ctx.load_deployment_info()?;
        if let Some(ref mailbox) = deployment.mailbox {
            if let Some(ref tx_hash) = mailbox.init_tx_hash {
                println!("\n{}", "Waiting for Mailbox transaction confirmation...".yellow());
                wait_for_tx_confirmation(&client, tx_hash).await?;
            }
        }
    }

    println!("\n{}", "3. Initializing Registry...".cyan());
    init_registry_internal(ctx, None, dry_run, &spent_utxos).await?;

    println!("\n{}", "✓ All contracts initialized successfully!".green().bold());

    Ok(())
}

/// Wait for a specific transaction to be confirmed on-chain.
async fn wait_for_tx_confirmation(client: &BlockfrostClient, tx_hash: &str) -> Result<()> {
    use std::time::Duration;
    use tokio::time::sleep;

    // Cardano block time is ~20 seconds, but transactions often appear within a few seconds
    // We'll poll every 5 seconds for up to 120 seconds
    let max_attempts = 24;
    let delay = Duration::from_secs(5);

    println!("  Waiting for tx: {}...", &tx_hash[..16]);

    for attempt in 1..=max_attempts {
        // Try to fetch the transaction
        match client.get_tx(tx_hash).await {
            Ok(_tx) => {
                println!("  {} (attempt {})", "✓ Transaction confirmed!".green(), attempt);
                return Ok(());
            }
            Err(_) => {
                // Transaction not yet confirmed, wait and retry
                print!("  Checking... (attempt {}/{})   \r", attempt, max_attempts);
                std::io::Write::flush(&mut std::io::stdout())?;
                sleep(delay).await;
            }
        }
    }

    // If we get here, the transaction wasn't confirmed in time
    Err(anyhow!("Transaction {} was not confirmed within 120 seconds. Please check the explorer and try again.", tx_hash))
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
        ("IGP", &deployment.igp),
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

    // Mailbox datum with empty merkle tree (32 zero branches)
    let zero_branch = "0".repeat(64); // 32 bytes of zeros
    let empty_branches: Vec<&str> = vec![zero_branch.as_str(); 32];
    let mailbox_datum = build_mailbox_datum(domain, ism_hash, &owner_pkh, 0, &empty_branches, 0)?;

    // Build branches JSON array
    let branches_json: Vec<serde_json::Value> = empty_branches
        .iter()
        .map(|b| serde_json::json!({"bytes": b}))
        .collect();

    let mailbox_json = serde_json::json!({
        "constructor": 0,
        "fields": [
            {"int": domain},
            {"bytes": ism_hash},
            {"bytes": owner_pkh},
            {"int": 0},
            {
                "constructor": 0,
                "fields": [
                    {"list": branches_json},
                    {"int": 0}
                ]
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_oracle_config_valid() {
        let result = parse_oracle_config("43113:25000000000:1000000").unwrap();
        assert_eq!(result, (43113, 25000000000, 1000000));
    }

    #[test]
    fn test_parse_oracle_config_with_whitespace() {
        let result = parse_oracle_config(" 43113 : 25000000000 : 1000000 ").unwrap();
        assert_eq!(result, (43113, 25000000000, 1000000));
    }

    #[test]
    fn test_parse_oracle_config_large_values() {
        // Test with large u64 values
        let result = parse_oracle_config("11155111:30000000000:1200000").unwrap();
        assert_eq!(result, (11155111, 30000000000, 1200000));
    }

    #[test]
    fn test_parse_oracle_config_too_few_parts() {
        let result = parse_oracle_config("43113:25000000000");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Expected 'domain:gas_price:exchange_rate'"));
    }

    #[test]
    fn test_parse_oracle_config_too_many_parts() {
        let result = parse_oracle_config("43113:25000000000:1000000:extra");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_oracle_config_invalid_domain() {
        let result = parse_oracle_config("fuji:25000000000:1000000");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid domain"));
    }

    #[test]
    fn test_parse_oracle_config_invalid_gas_price() {
        let result = parse_oracle_config("43113:not_a_number:1000000");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid gas_price"));
    }

    #[test]
    fn test_parse_oracle_config_negative_value() {
        // Negative values can't parse as u64
        let result = parse_oracle_config("43113:-100:1000000");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_oracle_config_empty_string() {
        let result = parse_oracle_config("");
        assert!(result.is_err());
    }
}

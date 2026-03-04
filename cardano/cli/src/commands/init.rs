//! Init command - Initialize contracts with state NFTs and initial datums

use anyhow::{anyhow, Context, Result};
use clap::{Args, Subcommand};
use colored::Colorize;

use crate::utils::blockfrost::BlockfrostClient;
use crate::utils::cbor::{build_igp_datum, build_ism_config_datum, build_ism_datum, build_mailbox_datum};
use crate::utils::context::CliContext;
use crate::utils::plutus::{
    apply_validator_param, apply_validator_params, encode_output_reference,
    encode_script_hash_param, script_hash_to_address, AppliedValidator, PlutusBlueprint,
};
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

    /// Initialize a custom recipient contract
    ///
    /// Creates the three-UTXO pattern required for protocol recipients:
    /// - Config UTXO:        script address with canonical config NFT + ISM config datum
    /// - State UTXO:         script address with state NFT + initial state datum
    /// - Reference Script UTXO: deployer address with ref NFT + script CBOR attached
    ///
    /// Uses a two-transaction flow:
    /// TX1: Fund an ADA-only "init-signal" UTXO at the script address.
    /// TX2: Spend the init-signal (Init redeemer), mint canonical config NFT + state NFTs,
    ///      create all three output UTXOs.
    Recipient {
        /// Mailbox policy ID (required to parameterize the recipient)
        #[arg(long)]
        mailbox_hash: Option<String>,

        /// Custom ISM script hash (optional, uses default if not specified)
        #[arg(long)]
        custom_ism: Option<String>,

        /// Cardano domain ID (defaults to network: mainnet=2001, preprod=2002, preview=2003)
        #[arg(long)]
        domain: Option<u32>,

        /// Owner verification key hash (28 bytes hex).
        /// Defaults to the signing key's public key hash.
        #[arg(long)]
        owner: Option<String>,

        /// Path to custom Aiken contracts directory (containing plutus.json)
        #[arg(long = "custom-contracts")]
        custom_contracts: String,

        /// Module name in the blueprint
        #[arg(long = "custom-module")]
        custom_module: String,

        /// Validator name in the blueprint
        #[arg(long = "custom-validator")]
        custom_validator: String,

        /// Output lovelace amount for state UTXO (default 5 ADA)
        #[arg(long, default_value = "5000000")]
        output_lovelace: u64,

        /// Output lovelace amount for reference script UTXO (default 20 ADA)
        #[arg(long, default_value = "20000000")]
        ref_script_lovelace: u64,

        /// Initial state datum CBOR (hex-encoded).
        /// Defaults to empty Constr 0 [].
        #[arg(long)]
        datum_cbor: Option<String>,

        /// Dry run
        #[arg(long)]
        dry_run: bool,
    },

    /// Initialize the IGP (Interchain Gas Paymaster) contract
    Igp {
        /// Beneficiary address for claimed fees (defaults to signer's pkh)
        #[arg(long)]
        beneficiary: Option<String>,

        /// Gas oracle config: "domain:gas_price:exchange_rate:gas_overhead" (repeatable)
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

        /// ISM validators per domain: "domain:addr1,addr2;domain2:addr3"
        #[arg(long)]
        validators: Option<String>,

        /// ISM threshold per domain: "domain:threshold;domain2:threshold"
        #[arg(long)]
        thresholds: Option<String>,

        /// Validator announce S3/storage URL
        #[arg(long)]
        storage_location: Option<String>,

        /// Validator ECDSA secp256k1 private key (hex) for announce
        #[arg(long)]
        validator_key: Option<String>,

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
        InitCommands::Recipient {
            mailbox_hash,
            custom_ism,
            domain,
            owner,
            custom_contracts,
            custom_module,
            custom_validator,
            output_lovelace,
            ref_script_lovelace,
            datum_cbor,
            dry_run,
        } => {
            init_recipient(
                ctx,
                mailbox_hash,
                custom_ism,
                domain,
                owner,
                custom_contracts,
                custom_module,
                custom_validator,
                output_lovelace,
                ref_script_lovelace,
                datum_cbor,
                dry_run,
            )
            .await
        }
        InitCommands::Igp {
            beneficiary,
            oracles,
            utxo,
            dry_run,
        } => init_igp(ctx, beneficiary, oracles, utxo, dry_run).await,
        InitCommands::All {
            domain,
            origin_domains,
            validators,
            thresholds,
            storage_location,
            validator_key,
            dry_run,
        } => {
            init_all(
                ctx,
                domain,
                &origin_domains,
                validators,
                thresholds,
                storage_location,
                validator_key,
                dry_run,
            )
            .await
        }
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
    init_mailbox_internal(ctx, domain, ism_hash, utxo, dry_run, &[]).await?;
    Ok(())
}

/// Internal mailbox init that excludes already-spent UTXOs and returns the spent UTXO reference
///
/// The mailbox initialization follows a specific parameterization chain:
/// 1. Create state_nft policy (one-shot) -> mailbox_policy_id
/// 2. Apply mailbox_policy_id to verified_message_nft -> verified_message_nft_policy
/// 3. Apply [verified_message_nft_policy, ism_nft_policy] to mailbox -> final mailbox script
///
/// This ensures message verification and storage are stable across mailbox upgrades.
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
    let deployment = ctx
        .load_deployment_info()
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
    println!(
        "  Found {} UTXOs at wallet (excluding {} spent)",
        utxos.len(),
        exclude_utxos.len()
    );

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
        None => utxos
            .iter()
            .find(|u| {
                u.lovelace >= 10_000_000 && u.assets.is_empty() && u.reference_script.is_none()
            })
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "No suitable UTXO found (need >= 10 ADA without assets or reference scripts)"
                )
            })?,
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

    println!(
        "  Input UTXO: {}#{}",
        input_utxo.tx_hash, input_utxo.output_index
    );
    println!(
        "  Collateral: {}#{}",
        collateral_utxo.tx_hash, collateral_utxo.output_index
    );

    // Encode output reference for state NFT parameter
    let output_ref_cbor = encode_output_reference(&input_utxo.tx_hash, input_utxo.output_index)?;
    let output_ref_hex = hex::encode(&output_ref_cbor);
    println!("  OutputRef CBOR: {}", output_ref_hex.yellow());

    // Step 1: Apply parameter to state_nft minting policy to get mailbox_policy_id
    println!(
        "\n{}",
        "Step 1: Creating state_nft policy (mailbox_policy_id)...".cyan()
    );
    let applied_nft = apply_validator_param(
        &ctx.contracts_dir,
        "state_nft",
        "state_nft",
        &output_ref_hex,
    )?;
    let mailbox_policy_id = applied_nft.policy_id.clone();
    println!("  Mailbox Policy ID: {}", mailbox_policy_id.green());

    // Step 2: Apply mailbox_policy_id to verified_message_nft to get the NFT policy
    let mailbox_policy_cbor = encode_script_hash_param(&mailbox_policy_id)?;
    let mailbox_policy_hex = hex::encode(&mailbox_policy_cbor);
    println!(
        "\n{}",
        "Step 2: Creating verified_message_nft policy...".cyan()
    );
    let applied_verified_nft = apply_validator_param(
        &ctx.contracts_dir,
        "verified_message_nft",
        "verified_message_nft",
        &mailbox_policy_hex,
    )?;
    let verified_message_nft_policy = applied_verified_nft.policy_id.clone();
    println!(
        "  Verified Message NFT Policy: {}",
        verified_message_nft_policy.green()
    );

    // Step 3: Apply verified_message_nft_policy and ism_nft_policy to mailbox
    println!("\n{}", "Step 3: Creating mailbox validator...".cyan());
    let vm_policy_cbor = encode_script_hash_param(&verified_message_nft_policy)?;
    let vm_policy_hex = hex::encode(&vm_policy_cbor);

    // Get ISM state NFT policy for authenticity verification
    let ism_nft_policy = deployment
        .ism
        .as_ref()
        .and_then(|i| i.state_nft_policy.clone())
        .ok_or_else(|| {
            anyhow!("ISM state NFT policy not found in deployment info. Initialize ISM first.")
        })?;
    println!("  ISM NFT Policy: {}", ism_nft_policy);
    let ism_nft_cbor = encode_script_hash_param(&ism_nft_policy)?;
    let ism_nft_hex = hex::encode(&ism_nft_cbor);

    let applied_mailbox = apply_validator_params(
        &ctx.contracts_dir,
        "mailbox",
        "mailbox",
        &[&vm_policy_hex, &ism_nft_hex],
    )?;
    let mailbox_addr = script_hash_to_address(&applied_mailbox.policy_id, ctx.pallas_network())?;
    println!(
        "  Mailbox Script Hash: {}",
        applied_mailbox.policy_id.green()
    );
    println!("  Mailbox Address: {}", mailbox_addr);

    // Build mailbox datum with empty merkle tree (32 zero branches)
    let zero_branch = "0".repeat(64); // 32 bytes of zeros
    let empty_branches: Vec<&str> = vec![zero_branch.as_str(); 32];
    // SMT EMPTY_ROOT: 128 levels of hash(zero, zero) starting from keccak256(0x00)
    let empty_root = "5c3cc358c060877ced35947091c44c900594ece1e0a4ade23143ef57c3f7600f";
    let datum_cbor = build_mailbox_datum(
        domain,
        ism_hash,
        &owner_pkh,
        0,
        &empty_branches,
        0,
        empty_root,
    )?;
    println!(
        "  Datum CBOR: {}...",
        hex::encode(&datum_cbor[..32.min(datum_cbor.len())])
    );

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        println!("\nTransaction would:");
        println!(
            "  - Spend UTXO {}#{}",
            input_utxo.tx_hash, input_utxo.output_index
        );
        println!("  - Mint state NFT with policy {}", applied_nft.policy_id);
        println!(
            "  - Create output at {} with {} ADA + NFT + datum",
            mailbox_addr, 7
        );
        println!("\n{}", "Parameterization chain:".green());
        println!("  1. mailbox_policy_id (state NFT): {}", mailbox_policy_id);
        println!(
            "  2. verified_message_nft_policy: {}",
            verified_message_nft_policy
        );
        println!("  3. Resulting mailbox hash: {}", applied_mailbox.policy_id);
        return Ok(None);
    }

    // Build and submit transaction
    println!("\n{}", "Building transaction...".cyan());
    let mint_script_cbor =
        hex::decode(&applied_nft.compiled_code).with_context(|| "Invalid script CBOR")?;

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
    let tx_hash = client.submit_and_confirm(&signed_tx, ctx.no_wait).await?;
    println!("\n{}", "✓ Transaction submitted!".green().bold());
    println!("  Explorer: {}", ctx.explorer_tx_url(&tx_hash));

    // State UTXO reference (first output is the state UTXO)
    let state_utxo_ref = format!("{}#0", tx_hash);

    // Update deployment info with complete initialization details
    let mut deployment = deployment;
    if let Some(ref mut mailbox) = deployment.mailbox {
        mailbox.applied_parameters = vec![
            AppliedParameter {
                name: "verified_message_nft_policy".to_string(),
                param_type: "PolicyId".to_string(),
                value: verified_message_nft_policy.clone(),
                description: Some(
                    "Policy ID for verified message NFTs (parameterized by mailbox_policy_id)"
                        .to_string(),
                ),
            },
            AppliedParameter {
                name: "ism_nft_policy".to_string(),
                param_type: "PolicyId".to_string(),
                value: ism_nft_policy.to_string(),
                description: Some(
                    "Policy ID of ISM state NFT for authenticity verification".to_string(),
                ),
            },
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

    let verified_nft_path = ctx
        .network_deployments_dir()
        .join("verified_message_nft_applied.plutus");
    applied_verified_nft.save_plutus_file(
        &verified_nft_path,
        "Applied verified_message_nft minting policy",
    )?;
    println!(
        "  Verified message NFT script saved to: {:?}",
        verified_nft_path
    );

    println!("\n{}", "Relayer config values:".cyan());
    println!(
        "  verifiedMessageNftPolicyId: {}",
        verified_message_nft_policy
    );
    println!(
        "  verifiedMessageNftScriptCbor: {}",
        applied_verified_nft.compiled_code
    );

    // Return the spent UTXO reference
    Ok(Some(format!(
        "{}#{}",
        input_utxo.tx_hash, input_utxo.output_index
    )))
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
    println!(
        "  Found {} UTXOs at wallet (excluding {} spent)",
        utxos.len(),
        exclude_utxos.len()
    );

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
        None => utxos
            .iter()
            .find(|u| {
                u.lovelace >= 10_000_000 && u.assets.is_empty() && u.reference_script.is_none()
            })
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "No suitable UTXO found (need >= 10 ADA without assets or reference scripts)"
                )
            })?,
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

    println!(
        "  Input UTXO: {}#{}",
        input_utxo.tx_hash, input_utxo.output_index
    );
    println!(
        "  Collateral: {}#{}",
        collateral_utxo.tx_hash, collateral_utxo.output_index
    );

    // Encode output reference for state NFT parameter
    let output_ref_cbor = encode_output_reference(&input_utxo.tx_hash, input_utxo.output_index)?;
    let output_ref_hex = hex::encode(&output_ref_cbor);
    println!("  OutputRef CBOR: {}", output_ref_hex.yellow());

    // Apply parameter to state_nft minting policy
    println!("\n{}", "Applying state_nft parameter...".cyan());
    let applied = apply_validator_param(
        &ctx.contracts_dir,
        "state_nft",
        "state_nft",
        &output_ref_hex,
    )?;
    println!("  State NFT Policy ID: {}", applied.policy_id.green());

    // Get ISM script address
    let deployment = ctx
        .load_deployment_info()
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
    println!(
        "  Datum CBOR: {}...",
        hex::encode(&datum_cbor[..32.min(datum_cbor.len())])
    );

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        println!("\nTransaction would:");
        println!(
            "  - Spend UTXO {}#{}",
            input_utxo.tx_hash, input_utxo.output_index
        );
        println!("  - Mint state NFT with policy {}", applied.policy_id);
        println!("  - Create output at {} with 5 ADA + NFT + datum", ism_addr);
        return Ok(None);
    }

    // Build and submit transaction
    println!("\n{}", "Building transaction...".cyan());
    let mint_script_cbor =
        hex::decode(&applied.compiled_code).with_context(|| "Invalid script CBOR")?;

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
    let tx_hash = client.submit_and_confirm(&signed_tx, ctx.no_wait).await?;
    println!("\n{}", "✓ Transaction submitted!".green().bold());
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
    Ok(Some(format!(
        "{}#{}",
        input_utxo.tx_hash, input_utxo.output_index
    )))
}

async fn init_igp(
    ctx: &CliContext,
    beneficiary: Option<String>,
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
                return Err(anyhow!(
                    "Beneficiary must be a 28-byte hex public key hash (56 hex chars)"
                ));
            }
            b.clone()
        }
        None => owner_pkh.clone(),
    };

    // Parse oracle configurations
    let gas_oracles: Vec<(u32, u64, u64, u64)> = oracles
        .iter()
        .map(|s| parse_oracle_config(s))
        .collect::<Result<Vec<_>>>()?;

    println!("  Owner: {}", owner_pkh);
    println!("  Beneficiary: {}", beneficiary_pkh);
    println!("  Gas Oracles: {} configured", gas_oracles.len());
    for (domain, gas_price, exchange_rate, gas_overhead) in &gas_oracles {
        println!(
            "    - Domain {}: gas_price={}, exchange_rate={}, gas_overhead={}",
            domain, gas_price, exchange_rate, gas_overhead
        );
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
        None => utxos
            .iter()
            .find(|u| {
                u.lovelace >= 10_000_000 && u.assets.is_empty() && u.reference_script.is_none()
            })
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "No suitable UTXO found (need >= 10 ADA without assets or reference scripts)"
                )
            })?,
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

    println!(
        "  Input UTXO: {}#{}",
        input_utxo.tx_hash, input_utxo.output_index
    );
    println!(
        "  Collateral: {}#{}",
        collateral_utxo.tx_hash, collateral_utxo.output_index
    );

    // Encode output reference for state NFT parameter
    let output_ref_cbor = encode_output_reference(&input_utxo.tx_hash, input_utxo.output_index)?;
    let output_ref_hex = hex::encode(&output_ref_cbor);

    // Apply parameter to state_nft minting policy
    println!("\n{}", "Applying state_nft parameter...".cyan());
    let applied = apply_validator_param(
        &ctx.contracts_dir,
        "state_nft",
        "state_nft",
        &output_ref_hex,
    )?;
    println!("  State NFT Policy ID: {}", applied.policy_id.green());

    // Get IGP script address from deployment_info.json
    let deployment = ctx
        .load_deployment_info()
        .with_context(|| "Run 'deploy extract' first")?;
    let igp_addr = deployment
        .igp
        .as_ref()
        .map(|i| i.address.clone())
        .ok_or_else(|| anyhow!("IGP address not found in deployment info"))?;
    println!("  IGP Address: {}", igp_addr);

    // Build IGP datum
    let datum_cbor = build_igp_datum(&owner_pkh, &beneficiary_pkh, &gas_oracles)?;
    println!(
        "  Datum CBOR: {}...",
        hex::encode(&datum_cbor[..32.min(datum_cbor.len())])
    );

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        println!("\nTransaction would:");
        println!(
            "  - Spend UTXO {}#{}",
            input_utxo.tx_hash, input_utxo.output_index
        );
        println!("  - Mint state NFT with policy {}", applied.policy_id);
        println!("  - Create output at {} with 5 ADA + NFT + datum", igp_addr);
        return Ok(());
    }

    // Build and submit transaction
    println!("\n{}", "Building transaction...".cyan());
    let mint_script_cbor =
        hex::decode(&applied.compiled_code).with_context(|| "Invalid script CBOR")?;

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
    let tx_hash = client.submit_and_confirm(&signed_tx, ctx.no_wait).await?;
    println!("\n{}", "✓ Transaction submitted!".green().bold());
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

/// Parse oracle config string "domain:gas_price:exchange_rate:gas_overhead"
fn parse_oracle_config(s: &str) -> Result<(u32, u64, u64, u64)> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 4 {
        return Err(anyhow!(
            "Invalid oracle format: '{}'. Expected 'domain:gas_price:exchange_rate:gas_overhead'",
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
    let gas_overhead: u64 = parts[3]
        .trim()
        .parse()
        .with_context(|| format!("Invalid gas_overhead in oracle config: '{}'", parts[3]))?;

    Ok((domain, gas_price, exchange_rate, gas_overhead))
}

async fn init_recipient(
    ctx: &CliContext,
    mailbox_hash: Option<String>,
    custom_ism: Option<String>,
    domain: Option<u32>,
    owner: Option<String>,
    custom_contracts: String,
    custom_module: String,
    custom_validator: String,
    output_lovelace: u64,
    ref_script_lovelace: u64,
    datum_cbor_hex: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!(
        "{}",
        format!(
            "Initializing recipient '{}' (canonical config NFT pattern)...",
            custom_validator
        )
        .cyan()
    );

    let api_key = ctx.require_api_key()?;
    let keypair = ctx.load_signing_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);
    let wallet_address = keypair.address_bech32(ctx.pallas_network());

    let deployment = ctx
        .load_deployment_info()
        .with_context(|| "Run 'deploy extract' first")?;

    let mailbox_info = deployment
        .mailbox
        .as_ref()
        .ok_or_else(|| anyhow!("Mailbox not found in deployment info"))?;

    let verified_msg_nft_policy = match mailbox_hash {
        Some(h) => h,
        None => mailbox_info
            .applied_parameters
            .iter()
            .find(|p| p.name == "verified_message_nft_policy")
            .map(|p| p.value.clone())
            .ok_or_else(|| {
                anyhow!(
                    "verified_message_nft_policy not found in mailbox params. Use --mailbox-hash"
                )
            })?,
    };

    let owner_pkh_bytes: [u8; 28] = match owner {
        Some(ref hex_str) => {
            let bytes = hex::decode(hex_str).with_context(|| "Invalid owner hex")?;
            if bytes.len() != 28 {
                return Err(anyhow!("Owner key hash must be 28 bytes"));
            }
            bytes.try_into().unwrap()
        }
        None => keypair.verification_key_hash(),
    };
    let owner_pkh_hex = hex::encode(owner_pkh_bytes);

    let domain_id = domain.unwrap_or_else(|| ctx.domain());

    println!("\n{}", "Configuration:".cyan());
    println!("  verified_message_nft_policy: {}", verified_msg_nft_policy);
    println!("  owner pkh: {}", owner_pkh_hex);
    println!("  domain: {}", domain_id);
    if let Some(ref ism) = custom_ism {
        println!("  custom ISM: {}", ism);
    }

    // Step 1: Apply (verified_message_nft_policy, owner) to recipient validator
    let verified_msg_cbor = encode_script_hash_param(&verified_msg_nft_policy)?;
    let verified_msg_cbor_hex = hex::encode(&verified_msg_cbor);
    let owner_cbor = encode_script_hash_param(&owner_pkh_hex)?;
    let owner_cbor_hex = hex::encode(&owner_cbor);

    let custom_path = std::path::Path::new(&custom_contracts);
    println!(
        "\n{}",
        format!(
            "Applying params to {}.{}...",
            custom_module, custom_validator
        )
        .cyan()
    );
    let recipient_applied = apply_validator_params(
        custom_path,
        &custom_module,
        &custom_validator,
        &[&verified_msg_cbor_hex, &owner_cbor_hex],
    )?;
    let script_hash = recipient_applied.policy_id.clone();
    let script_addr = script_hash_to_address(&script_hash, ctx.pallas_network())?;
    println!("  Script hash: {}", script_hash.green());
    println!("  Script address: {}", script_addr);

    // Step 2: Load fixed canonical_config_nft policy from blueprint (no parameters).
    // Asset name = recipient's script hash bytes (28 bytes).
    let blueprint = PlutusBlueprint::from_file(&ctx.contracts_dir.join("plutus.json"))?;
    let canonical_def = blueprint
        .find_validator("canonical_config_nft.canonical_config_nft.mint")
        .ok_or_else(|| anyhow!("canonical_config_nft validator not found in plutus.json"))?;
    let canonical_applied = AppliedValidator {
        compiled_code: canonical_def.compiled_code.clone(),
        policy_id: canonical_def.hash.clone(),
    };
    println!(
        "\n  Canonical config NFT policy: {}",
        canonical_applied.policy_id.green()
    );
    let script_hash_bytes =
        hex::decode(&script_hash).with_context(|| "Invalid script hash hex")?;

    // Step 3: Select wallet UTXOs — only 2 needed (TX1 spends fee_utxo, TX2 spends change)
    let utxos = client.get_utxos(&wallet_address).await?;
    println!("\n  Wallet UTXOs: {}", utxos.len());

    // fee_utxo: large pure-ADA UTXO, used in TX1 (sends 2 ADA + returns change to wallet)
    // The change output from TX1 is used as TX2's fee input.
    // fee_utxo is also the state_nft seed via init_signal_utxo (TX1's output #0).
    let fee_utxo = utxos
        .iter()
        .find(|u| {
            u.lovelace >= output_lovelace + ref_script_lovelace + 10_000_000
                && u.assets.is_empty()
                && u.reference_script.is_none()
        })
        .cloned()
        .ok_or_else(|| {
            anyhow!(
                "No suitable fee UTXO found (need >= {} ADA without assets)",
                (output_lovelace + ref_script_lovelace + 10_000_000) / 1_000_000
            )
        })?;

    // collateral_utxo: pure-ADA UTXO ≥5 ADA, distinct from fee_utxo, used as TX2 collateral
    let collateral_utxo = utxos
        .iter()
        .find(|u| {
            u.lovelace >= 5_000_000
                && u.assets.is_empty()
                && u.reference_script.is_none()
                && !(u.tx_hash == fee_utxo.tx_hash && u.output_index == fee_utxo.output_index)
        })
        .cloned()
        .ok_or_else(|| anyhow!("No suitable collateral UTXO found (need ≥5 ADA without assets)"))?;

    println!(
        "  TX1 fee UTXO: {}#{} ({} ADA)",
        fee_utxo.tx_hash,
        fee_utxo.output_index,
        fee_utxo.lovelace / 1_000_000
    );
    println!(
        "  Collateral:   {}#{}",
        collateral_utxo.tx_hash, collateral_utxo.output_index
    );

    // ISM config datum: Option<ScriptHash>
    let config_datum = {
        let ism_bytes = match &custom_ism {
            Some(hex_str) => {
                let bytes = hex::decode(hex_str).with_context(|| "Invalid ISM hash hex")?;
                if bytes.len() != 28 {
                    return Err(anyhow!("ISM hash must be 28 bytes"));
                }
                let mut arr = [0u8; 28];
                arr.copy_from_slice(&bytes);
                Some(arr)
            }
            None => None,
        };
        build_ism_config_datum(ism_bytes.as_ref())
    };

    // Initial state datum (default: GreetingDatum { last_greeting: #"", greeting_count: 0 })
    let state_datum = match datum_cbor_hex {
        Some(ref hex_str) => hex::decode(hex_str).with_context(|| "Invalid datum CBOR hex")?,
        None => vec![0xd8, 0x79, 0x82, 0x40, 0x00],
    };

    if dry_run {
        println!("\n{}", "[Dry run - not submitting]".yellow());
        println!(
            "TX1: Send 2 ADA from {}#{} to {}",
            fee_utxo.tx_hash, fee_utxo.output_index, script_addr
        );
        println!(
            "TX2: Mint canonical NFT {}/{} + state NFT at {}",
            canonical_applied.policy_id, script_hash, script_addr
        );
        println!("  Output #0: config UTXO  (canonical NFT + ISM config datum)");
        println!("  Output #1: state UTXO   (state NFT + initial datum)");
        println!("  Output #2: ref script   (ref NFT + recipient script, at deployer)");
        return Ok(());
    }

    let tx_builder = HyperlaneTxBuilder::new(&client, ctx.pallas_network());

    // TX1: send 2 ADA init-signal to script address
    let init_signal_lovelace = 2_000_000u64;
    println!("\n{}", "TX1: Funding init-signal UTXO at script address...".cyan());
    let tx1 = tx_builder
        .build_send_ada_tx(&keypair, &fee_utxo, &script_addr, init_signal_lovelace)
        .await?;
    let tx1_signed = tx_builder.sign_tx(tx1, &keypair)?;
    let tx1_hash = client.submit_and_confirm(&tx1_signed, ctx.no_wait).await?;
    println!("  Explorer: {}", ctx.explorer_tx_url(&tx1_hash));

    // Wait for TX1 outputs to be indexed by Blockfrost:
    //   output #0 = init-signal at script address (consumed in TX2 with Init redeemer)
    //   output #1 = fee change back to wallet (used as TX2 fee input)
    println!(
        "{}",
        "  Waiting for TX1 outputs to be indexed (max 60s)...".cyan()
    );
    let init_signal_utxo = client
        .wait_for_utxo(&script_addr, &tx1_hash, 0, 60)
        .await?;
    let fee_change_utxo = client
        .wait_for_utxo(&wallet_address, &tx1_hash, 1, 60)
        .await?;
    println!(
        "  Init-signal: {}#{}",
        init_signal_utxo.tx_hash, init_signal_utxo.output_index
    );
    println!(
        "  Fee change:  {}#{} ({} ADA)",
        fee_change_utxo.tx_hash,
        fee_change_utxo.output_index,
        fee_change_utxo.lovelace / 1_000_000
    );

    // Apply state_nft seed = init_signal_utxo (consumed in TX2)
    println!("\n{}", "Applying seed to state_nft...".cyan());
    let seed_cbor = encode_output_reference(&init_signal_utxo.tx_hash, init_signal_utxo.output_index)?;
    let seed_cbor_hex = hex::encode(&seed_cbor);
    let state_nft_applied =
        apply_validator_param(&ctx.contracts_dir, "state_nft", "state_nft", &seed_cbor_hex)?;
    println!("  State NFT policy: {}", state_nft_applied.policy_id.green());

    // TX2: canonical init (spend init-signal + fee change, mint canonical + state NFTs)
    let canonical_cbor =
        hex::decode(&canonical_applied.compiled_code).with_context(|| "Invalid canonical CBOR")?;
    let state_nft_cbor = hex::decode(&state_nft_applied.compiled_code)
        .with_context(|| "Invalid state NFT CBOR")?;
    let recipient_cbor = hex::decode(&recipient_applied.compiled_code)
        .with_context(|| "Invalid recipient CBOR")?;

    println!("\n{}", "TX2: Building canonical NFT init transaction...".cyan());
    let tx2 = tx_builder
        .build_init_canonical_nft_tx(
            &keypair,
            &init_signal_utxo,
            &fee_change_utxo,
            &collateral_utxo,
            &canonical_cbor,
            &state_nft_cbor,
            &recipient_cbor,
            &script_addr,
            &script_hash_bytes,
            &config_datum,
            &state_datum,
            &owner_pkh_bytes,
            output_lovelace,
            ref_script_lovelace,
        )
        .await?;
    let tx2_signed = tx_builder.sign_tx(tx2, &keypair)?;
    let tx2_hash = client.submit_and_confirm(&tx2_signed, ctx.no_wait).await?;
    println!("\n{}", "✓ Canonical init complete!".green().bold());
    println!("  Explorer: {}", ctx.explorer_tx_url(&tx2_hash));

    println!(
        "\n{}",
        "═══════════════════════════════════════════════════════════════".green()
    );
    println!(
        "{}",
        "Recipient Deployment Summary (Canonical Config NFT Pattern)"
            .green()
            .bold()
    );
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════".green()
    );
    println!();
    println!("{}", "Script Info:".cyan());
    println!("  Script hash:             {}", script_hash.green());
    println!("  Address:                 {}", script_addr);
    println!("  State NFT policy:        {}", state_nft_applied.policy_id.green());
    println!("  Canonical config policy: {}", canonical_applied.policy_id.green());
    println!();
    println!("{}", "Outputs (TX2):".cyan());
    println!("  #0 config UTXO  — canonical NFT + ISM config datum  @ script address");
    println!("  #1 state UTXO   — state NFT + initial datum          @ script address");
    println!("  #2 ref script   — ref NFT + recipient script CBOR    @ deployer address");

    if let Ok(mut dep) = ctx.load_deployment_info() {
        use crate::utils::types::{RecipientDeployment, ReferenceScriptUtxo};
        dep.recipients
            .retain(|r| r.recipient_type != custom_validator);
        dep.recipients.push(RecipientDeployment {
            recipient_type: custom_validator.clone(),
            script_hash: script_hash.clone(),
            address: script_addr.clone(),
            nft_policy: state_nft_applied.policy_id.clone(),
            init_tx_hash: Some(tx2_hash.clone()),
            reference_script_utxo: Some(ReferenceScriptUtxo {
                tx_hash: tx2_hash.clone(),
                output_index: 2,
                lovelace: ref_script_lovelace,
            }),
        });
        ctx.save_deployment_info(&dep)?;
        println!("\n{}", "✓ Saved to deployment_info.json".green());
    }

    Ok(())
}

async fn init_all(
    ctx: &CliContext,
    domain: u32,
    origin_domains: &str,
    validators: Option<String>,
    thresholds: Option<String>,
    storage_location: Option<String>,
    validator_key: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Initializing all core contracts...".cyan());
    println!("  Cardano Domain: {}", domain);
    println!("  Origin Domains: {}", origin_domains);

    // Load deployment info to get script hashes
    let deployment = ctx
        .load_deployment_info()
        .with_context(|| "Run 'deploy extract' first to generate deployment info")?;

    let ism_hash = deployment
        .ism
        .as_ref()
        .map(|i| i.hash.clone())
        .ok_or_else(|| anyhow!("ISM hash not found in deployment info"))?;

    // Track spent UTXOs to avoid reusing them
    let mut spent_utxos: Vec<String> = Vec::new();

    let mut step = 1;

    println!("\n{}", format!("{}. Initializing ISM...", step).cyan());
    let ism_spent =
        init_ism_internal(ctx, origin_domains, None, None, None, dry_run, &spent_utxos).await?;
    if let Some(utxo) = ism_spent {
        spent_utxos.push(utxo);
    }
    step += 1;

    println!("\n{}", format!("{}. Initializing Mailbox...", step).cyan());
    let mailbox_spent =
        init_mailbox_internal(ctx, domain, &ism_hash, None, dry_run, &spent_utxos).await?;
    if let Some(utxo) = mailbox_spent {
        spent_utxos.push(utxo);
    }
    step += 1;

    // Optional: set ISM validators per domain
    if let Some(ref validators_str) = validators {
        println!(
            "\n{}",
            format!("{}. Setting ISM validators...", step).cyan()
        );
        // Parse "domain:addr1,addr2;domain2:addr3" format
        for domain_block in validators_str.split(';') {
            let parts: Vec<&str> = domain_block.splitn(2, ':').collect();
            if parts.len() != 2 {
                return Err(anyhow!(
                    "Invalid validators format: '{}'. Expected 'domain:addr1,addr2'",
                    domain_block
                ));
            }
            let d: u32 = parts[0]
                .parse()
                .with_context(|| format!("Invalid domain in validators: '{}'", parts[0]))?;
            let addrs: Vec<String> = parts[1].split(',').map(|s| s.trim().to_string()).collect();

            // Parse threshold for this domain if provided
            let thresh = thresholds.as_ref().and_then(|t| {
                t.split(';').find_map(|tb| {
                    let tp: Vec<&str> = tb.splitn(2, ':').collect();
                    if tp.len() == 2 && tp[0].parse::<u32>().ok() == Some(d) {
                        tp[1].parse::<u32>().ok()
                    } else {
                        None
                    }
                })
            });

            if let Some(spent) =
                super::ism::set_validators(ctx, d, addrs, thresh, None, None, dry_run, &spent_utxos)
                    .await?
            {
                spent_utxos.push(spent);
            }
        }
        step += 1;
    }

    // Optional: set ISM thresholds for domains not covered by validators
    if let Some(ref thresholds_str) = thresholds {
        if validators.is_none() {
            println!(
                "\n{}",
                format!("{}. Setting ISM thresholds...", step).cyan()
            );
            for threshold_block in thresholds_str.split(';') {
                let parts: Vec<&str> = threshold_block.splitn(2, ':').collect();
                if parts.len() != 2 {
                    return Err(anyhow!(
                        "Invalid thresholds format: '{}'. Expected 'domain:threshold'",
                        threshold_block
                    ));
                }
                let d: u32 = parts[0]
                    .parse()
                    .with_context(|| format!("Invalid domain in thresholds: '{}'", parts[0]))?;
                let t: u32 = parts[1]
                    .parse()
                    .with_context(|| format!("Invalid threshold: '{}'", parts[1]))?;
                if let Some(spent) =
                    super::ism::set_threshold(ctx, d, t, None, None, dry_run, &spent_utxos).await?
                {
                    spent_utxos.push(spent);
                }
            }
            step += 1;
        }
    }

    // Optional: validator announce
    if let (Some(ref location), Some(ref key)) = (&storage_location, &validator_key) {
        println!("\n{}", format!("{}. Announcing validator...", step).cyan());
        super::validator::announce_validator(ctx, location, key, None, dry_run, &spent_utxos)
            .await?;
    }

    println!(
        "\n{}",
        "✓ All contracts initialized successfully!".green().bold()
    );
    println!(
        "{}",
        "Note: IGP not initialized by 'init all'. Run 'init igp' separately."
            .yellow()
    );

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
            println!(
                "{:<12} {}",
                format!("{}:", name).bold(),
                "Not deployed".red()
            );
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
    let empty_root = "5c3cc358c060877ced35947091c44c900594ece1e0a4ade23143ef57c3f7600f";
    let mailbox_datum = build_mailbox_datum(
        domain,
        ism_hash,
        &owner_pkh,
        0,
        &empty_branches,
        0,
        empty_root,
    )?;

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
            },
            {"bytes": empty_root}
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
        let result = parse_oracle_config("43113:25000000000:1000000:500000").unwrap();
        assert_eq!(result, (43113, 25000000000, 1000000, 500000));
    }

    #[test]
    fn test_parse_oracle_config_with_whitespace() {
        let result = parse_oracle_config(" 43113 : 25000000000 : 1000000 : 0 ").unwrap();
        assert_eq!(result, (43113, 25000000000, 1000000, 0));
    }

    #[test]
    fn test_parse_oracle_config_large_values() {
        let result = parse_oracle_config("11155111:30000000000:1200000:1000000").unwrap();
        assert_eq!(result, (11155111, 30000000000, 1200000, 1000000));
    }

    #[test]
    fn test_parse_oracle_config_too_few_parts() {
        let result = parse_oracle_config("43113:25000000000:1000000");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Expected 'domain:gas_price:exchange_rate:gas_overhead'"));
    }

    #[test]
    fn test_parse_oracle_config_too_many_parts() {
        let result = parse_oracle_config("43113:25000000000:1000000:500000:extra");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_oracle_config_invalid_domain() {
        let result = parse_oracle_config("fuji:25000000000:1000000:0");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid domain"));
    }

    #[test]
    fn test_parse_oracle_config_invalid_gas_price() {
        let result = parse_oracle_config("43113:not_a_number:1000000:0");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid gas_price"));
    }

    #[test]
    fn test_parse_oracle_config_negative_value() {
        // Negative values can't parse as u64
        let result = parse_oracle_config("43113:-100:1000000:0");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_oracle_config_empty_string() {
        let result = parse_oracle_config("");
        assert!(result.is_err());
    }
}

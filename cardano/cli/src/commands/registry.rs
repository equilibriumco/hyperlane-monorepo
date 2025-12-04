//! Registry command - Manage recipient registry

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand, ValueEnum};
use colored::Colorize;
use pallas_addresses::Network;

use crate::utils::blockfrost::BlockfrostClient;
use crate::utils::cbor::RegistrationData;
use crate::utils::context::CliContext;
use crate::utils::crypto::Keypair;
use crate::utils::tx_builder::HyperlaneTxBuilder;
use crate::utils::types::RecipientInfo;

#[derive(Args)]
pub struct RegistryArgs {
    #[command(subcommand)]
    command: RegistryCommands,
}

#[derive(Subcommand)]
enum RegistryCommands {
    /// Register a new recipient
    Register {
        /// Recipient script hash (28 bytes hex)
        #[arg(long)]
        script_hash: String,

        /// State NFT policy ID
        #[arg(long)]
        state_policy: String,

        /// State NFT asset name (hex, empty for unit)
        #[arg(long, default_value = "")]
        state_asset: String,

        /// Reference script NFT policy ID (optional, for reference script UTXO lookup)
        #[arg(long)]
        ref_script_policy: Option<String>,

        /// Reference script NFT asset name (hex, empty for unit)
        #[arg(long)]
        ref_script_asset: Option<String>,

        /// Recipient type
        #[arg(long, value_enum, default_value = "generic")]
        recipient_type: RecipientTypeArg,

        /// Custom ISM script hash (optional)
        #[arg(long)]
        custom_ism: Option<String>,

        /// Registry policy ID
        #[arg(long)]
        registry_policy: Option<String>,

        /// Dry run
        #[arg(long)]
        dry_run: bool,
    },

    /// List registered recipients
    List {
        /// Registry policy ID
        #[arg(long)]
        registry_policy: Option<String>,

        /// Output format
        #[arg(long, value_enum, default_value = "table")]
        format: OutputFormat,
    },

    /// Show details for a specific recipient
    Show {
        /// Recipient script hash
        #[arg(long)]
        script_hash: String,

        /// Registry policy ID
        #[arg(long)]
        registry_policy: Option<String>,
    },

    /// Remove a recipient registration (owner only)
    Remove {
        /// Recipient script hash
        #[arg(long)]
        script_hash: String,

        /// Registry policy ID
        #[arg(long)]
        registry_policy: Option<String>,

        /// Dry run
        #[arg(long)]
        dry_run: bool,
    },

    /// Generate registration JSON for manual submission
    GenerateJson {
        /// Recipient script hash
        #[arg(long)]
        script_hash: String,

        /// Owner verification key hash (28 bytes hex)
        #[arg(long)]
        owner: String,

        /// State NFT policy ID
        #[arg(long)]
        state_policy: String,

        /// State NFT asset name
        #[arg(long, default_value = "")]
        state_asset: String,

        /// Recipient type
        #[arg(long, value_enum, default_value = "generic")]
        recipient_type: RecipientTypeArg,

        /// Output file
        #[arg(short, long)]
        output: Option<String>,
    },
}

#[derive(Clone, ValueEnum)]
enum RecipientTypeArg {
    Generic,
    TokenReceiver,
    ContractCaller,
}

#[derive(Clone, ValueEnum)]
enum OutputFormat {
    Table,
    Json,
}

pub async fn execute(ctx: &CliContext, args: RegistryArgs) -> Result<()> {
    match args.command {
        RegistryCommands::Register {
            script_hash,
            state_policy,
            state_asset,
            ref_script_policy,
            ref_script_asset,
            recipient_type,
            custom_ism,
            registry_policy,
            dry_run,
        } => {
            register(
                ctx,
                &script_hash,
                &state_policy,
                &state_asset,
                ref_script_policy,
                ref_script_asset,
                recipient_type,
                custom_ism,
                registry_policy,
                dry_run,
            )
            .await
        }
        RegistryCommands::List {
            registry_policy,
            format,
        } => list(ctx, registry_policy, format).await,
        RegistryCommands::Show {
            script_hash,
            registry_policy,
        } => show(ctx, &script_hash, registry_policy).await,
        RegistryCommands::Remove {
            script_hash,
            registry_policy,
            dry_run,
        } => remove(ctx, &script_hash, registry_policy, dry_run).await,
        RegistryCommands::GenerateJson {
            script_hash,
            owner,
            state_policy,
            state_asset,
            recipient_type,
            output,
        } => generate_json(&script_hash, &owner, &state_policy, &state_asset, recipient_type, output).await,
    }
}

async fn register(
    ctx: &CliContext,
    script_hash: &str,
    state_policy: &str,
    state_asset: &str,
    ref_script_policy: Option<String>,
    ref_script_asset: Option<String>,
    recipient_type: RecipientTypeArg,
    custom_ism: Option<String>,
    registry_policy: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Registering recipient...".cyan());

    // Validate script hash
    let script_hash = validate_script_hash(script_hash)?;
    println!("  Script Hash: {}", script_hash);
    println!("  State Policy: {}", state_policy);
    println!("  State Asset: {}", if state_asset.is_empty() { "(empty)" } else { state_asset });

    // Handle reference script locator
    if let (Some(ref_policy), Some(ref_asset)) = (&ref_script_policy, &ref_script_asset) {
        println!("  Ref Script Policy: {}", ref_policy);
        println!("  Ref Script Asset: {}", if ref_asset.is_empty() { "(empty)" } else { ref_asset });
    } else if ref_script_policy.is_some() || ref_script_asset.is_some() {
        return Err(anyhow!("Both --ref-script-policy and --ref-script-asset must be provided together"));
    }

    let type_str = match recipient_type {
        RecipientTypeArg::Generic => "GenericHandler",
        RecipientTypeArg::TokenReceiver => "TokenReceiver",
        RecipientTypeArg::ContractCaller => "ContractCaller",
    };
    println!("  Type: {}", type_str);

    if let Some(ism) = &custom_ism {
        println!("  Custom ISM: {}", ism);
    }

    // Get signing key early to determine owner
    let signing_key_path = ctx.signing_key_path()
        .ok_or_else(|| anyhow!("Signing key required for registration. Use --signing-key"))?;
    let payer = Keypair::from_file(signing_key_path)?;
    let owner_pkh = hex::encode(payer.verification_key_hash());

    let registration = RecipientInfo {
        script_hash: script_hash.clone(),
        owner: owner_pkh.clone(),
        state_policy_id: state_policy.to_string(),
        state_asset_name: state_asset.to_string(),
        recipient_type: type_str.to_string(),
        custom_ism: custom_ism.clone(),
        ref_script_policy_id: ref_script_policy.clone(),
        ref_script_asset_name: ref_script_asset.clone(),
    };

    println!("\n{}", "Registration Data:".green());
    println!("{}", serde_json::to_string_pretty(&registration)?);

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        return Ok(());
    }

    // Get required context
    let api_key = ctx.require_api_key()?;

    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);
    let network = if ctx.network() == "mainnet" { Network::Mainnet } else { Network::Testnet };
    let tx_builder = HyperlaneTxBuilder::new(&client, network);

    // Get registry policy
    let policy_id = get_registry_policy(ctx, registry_policy)?;
    println!("\n{}", "Looking up registry UTXO...".cyan());

    // Find registry UTXO
    let registry_utxo = client
        .find_utxo_by_asset(&policy_id, "")
        .await?
        .ok_or_else(|| anyhow!("Registry UTXO not found with policy {}", policy_id))?;
    println!("  Found: {}#{}", registry_utxo.tx_hash, registry_utxo.output_index);

    // Parse existing registrations
    let existing_registrations = if let Some(datum) = &registry_utxo.inline_datum {
        parse_registrations_from_datum(datum)?
    } else {
        vec![]
    };
    println!("  Existing registrations: {}", existing_registrations.len());

    // Check if already registered
    if existing_registrations.iter().any(|r| r.script_hash == script_hash) {
        return Err(anyhow!("Recipient {} is already registered", script_hash));
    }

    // Convert to RegistrationData (includes owner field now)
    let existing: Vec<RegistrationData> = existing_registrations
        .iter()
        .map(|r| RegistrationData {
            script_hash: r.script_hash.clone(),
            owner: r.owner.clone(),
            state_policy_id: r.state_policy_id.clone(),
            state_asset_name: r.state_asset_name.clone(),
            ref_script_policy_id: r.ref_script_policy_id.clone(),
            ref_script_asset_name: r.ref_script_asset_name.clone(),
        })
        .collect();

    let new_registration = RegistrationData {
        script_hash: script_hash.clone(),
        owner: owner_pkh.clone(),
        state_policy_id: state_policy.to_string(),
        state_asset_name: state_asset.to_string(),
        ref_script_policy_id: ref_script_policy,
        ref_script_asset_name: ref_script_asset,
    };

    // Get payer UTXOs
    println!("\n{}", "Finding payer UTXOs...".cyan());
    let payer_address = payer.address_bech32(network);
    let payer_utxos = client.get_utxos(&payer_address).await?;

    if payer_utxos.is_empty() {
        return Err(anyhow!("No UTXOs found at payer address {}", payer_address));
    }

    // Find suitable input and collateral
    let input_utxo = payer_utxos.iter()
        .find(|u| u.lovelace >= 5_000_000 && u.assets.is_empty())
        .ok_or_else(|| anyhow!("Need a UTXO with at least 5 ADA for fees"))?;

    let collateral_utxo = payer_utxos.iter()
        .find(|u| u.lovelace >= 5_000_000 && u.assets.is_empty() && u.tx_hash != input_utxo.tx_hash)
        .unwrap_or(input_utxo);

    println!("  Input UTXO: {}#{}", input_utxo.tx_hash, input_utxo.output_index);
    println!("  Collateral: {}#{}", collateral_utxo.tx_hash, collateral_utxo.output_index);

    // Load registry script from deployment directory
    println!("\n{}", "Loading registry script...".cyan());
    let registry_script_path = ctx.network_deployments_dir().join("registry.plutus");
    if !registry_script_path.exists() {
        return Err(anyhow!(
            "Registry script not found at {:?}. Make sure to deploy first.",
            registry_script_path
        ));
    }
    let registry_script_json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&registry_script_path)?
    )?;
    let registry_script_hex = registry_script_json
        .get("cborHex")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Registry script missing cborHex field"))?;
    let registry_script_cbor = hex::decode(registry_script_hex)?;

    // Build transaction (owner_pkh already defined earlier)
    println!("\n{}", "Building transaction...".cyan());
    let tx = tx_builder
        .build_registry_register_tx(
            &payer,
            input_utxo,
            collateral_utxo,
            &registry_utxo,
            &registry_script_cbor,
            &existing,
            &new_registration,
            &owner_pkh,
        )
        .await?;

    // Sign transaction
    println!("{}", "Signing transaction...".cyan());
    let signed_tx = tx_builder.sign_tx(tx, &payer)?;

    // Submit transaction
    println!("{}", "Submitting transaction...".cyan());
    let tx_hash = client.submit_tx(&signed_tx).await?;

    println!("\n{} Transaction submitted!", "✓".green());
    println!("  TX Hash: {}", tx_hash);
    println!("\n{}", "Recipient registered successfully!".green().bold());

    Ok(())
}

async fn list(
    ctx: &CliContext,
    registry_policy: Option<String>,
    format: OutputFormat,
) -> Result<()> {
    println!("{}", "Listing registered recipients...".cyan());

    let policy_id = get_registry_policy(ctx, registry_policy)?;
    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let registry_utxo = client
        .find_utxo_by_asset(&policy_id, "")
        .await?
        .ok_or_else(|| anyhow!("Registry UTXO not found with policy {}", policy_id))?;

    println!("\n{}", "Registry UTXO:".green());
    println!("  {}#{}", registry_utxo.tx_hash, registry_utxo.output_index);

    if let Some(datum) = &registry_utxo.inline_datum {
        let registrations = parse_registrations_from_datum(datum)?;

        match format {
            OutputFormat::Table => {
                println!("\n{}", "Registered Recipients:".green());
                println!("{}", "-".repeat(120));
                println!(
                    "{:<58} {:<58} {:<10}",
                    "Script Hash", "Owner", "Type"
                );
                println!("{}", "-".repeat(120));

                for reg in &registrations {
                    // Truncate hashes for display
                    let script_display = if reg.script_hash.len() > 56 {
                        format!("{}...", &reg.script_hash[..53])
                    } else {
                        reg.script_hash.clone()
                    };
                    let owner_display = if reg.owner.len() > 56 {
                        format!("{}...", &reg.owner[..53])
                    } else {
                        reg.owner.clone()
                    };
                    println!(
                        "{:<58} {:<58} {:<10}",
                        script_display, owner_display, reg.recipient_type
                    );
                }

                println!("\n{} recipients registered", registrations.len());
            }
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(&registrations)?);
            }
        }
    } else {
        println!("\n{}", "No inline datum found".yellow());
    }

    Ok(())
}

async fn show(
    ctx: &CliContext,
    script_hash: &str,
    registry_policy: Option<String>,
) -> Result<()> {
    println!("{}", "Looking up recipient...".cyan());

    let script_hash = validate_script_hash(script_hash)?;
    let policy_id = get_registry_policy(ctx, registry_policy)?;
    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let registry_utxo = client
        .find_utxo_by_asset(&policy_id, "")
        .await?
        .ok_or_else(|| anyhow!("Registry UTXO not found"))?;

    if let Some(datum) = &registry_utxo.inline_datum {
        let registrations = parse_registrations_from_datum(datum)?;

        if let Some(reg) = registrations.iter().find(|r| r.script_hash == script_hash) {
            println!("\n{}", "Recipient Found:".green());
            println!("{}", serde_json::to_string_pretty(reg)?);
        } else {
            println!("\n{}", "Recipient not found in registry".yellow());
        }
    } else {
        println!("\n{}", "No inline datum found".yellow());
    }

    Ok(())
}

async fn remove(
    ctx: &CliContext,
    script_hash: &str,
    _registry_policy: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Removing recipient registration...".cyan());

    let script_hash = validate_script_hash(script_hash)?;
    println!("  Script Hash: {}", script_hash);

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        return Ok(());
    }

    println!("\n{}", "Manual Steps Required:".yellow().bold());
    println!("Build a transaction that spends the registry UTXO with 'Remove' redeemer");

    Ok(())
}

async fn generate_json(
    script_hash: &str,
    owner: &str,
    state_policy: &str,
    state_asset: &str,
    recipient_type: RecipientTypeArg,
    output: Option<String>,
) -> Result<()> {
    let script_hash = validate_script_hash(script_hash)?;
    let owner = validate_script_hash(owner)?; // Same validation - 28 bytes hex

    let type_str = match recipient_type {
        RecipientTypeArg::Generic => "GenericHandler",
        RecipientTypeArg::TokenReceiver => "TokenReceiver",
        RecipientTypeArg::ContractCaller => "ContractCaller",
    };

    let registration = RecipientInfo {
        script_hash,
        owner,
        state_policy_id: state_policy.to_string(),
        state_asset_name: state_asset.to_string(),
        recipient_type: type_str.to_string(),
        custom_ism: None,
        ref_script_policy_id: None,
        ref_script_asset_name: None,
    };

    let json = serde_json::to_string_pretty(&registration)?;

    if let Some(path) = output {
        std::fs::write(&path, &json)?;
        println!("{} Registration JSON written to {}", "✓".green(), path);
    } else {
        println!("{}", json);
    }

    Ok(())
}

// Helper functions

fn get_registry_policy(ctx: &CliContext, registry_policy: Option<String>) -> Result<String> {
    if let Some(p) = registry_policy {
        return Ok(p);
    }

    let deployment = ctx.load_deployment_info()?;
    deployment
        .registry
        .and_then(|r| r.state_nft_policy)
        .ok_or_else(|| {
            anyhow!("Registry policy not found. Use --registry-policy or update deployment_info.json")
        })
}

fn validate_script_hash(hash: &str) -> Result<String> {
    let hash = hash.strip_prefix("0x").unwrap_or(hash).to_lowercase();
    if hash.len() != 56 {
        return Err(anyhow!(
            "Script hash must be 28 bytes (56 hex chars), got {}",
            hash.len()
        ));
    }
    hex::decode(&hash)?;
    Ok(hash)
}

fn parse_registrations_from_datum(datum: &serde_json::Value) -> Result<Vec<RecipientInfo>> {
    // Check if datum is a JSON object with "fields" key (Blockfrost JSON format)
    if let Some(fields) = datum.get("fields").and_then(|f| f.as_array()) {
        return parse_registrations_from_json_fields(fields);
    }

    // Otherwise, it's likely a CBOR hex string
    if let Some(hex_str) = datum.as_str() {
        return parse_registrations_from_cbor(hex_str);
    }

    Err(anyhow!("Invalid datum structure: expected JSON object with 'fields' or CBOR hex string"))
}

fn parse_registrations_from_cbor(hex_str: &str) -> Result<Vec<RecipientInfo>> {
    use pallas_codec::minicbor;
    use pallas_primitives::conway::PlutusData;

    let hex_str = hex_str.trim_matches('"');
    let cbor_bytes = hex::decode(hex_str)?;
    let plutus_data: PlutusData = minicbor::decode(&cbor_bytes)
        .map_err(|e| anyhow!("Failed to decode CBOR: {}", e))?;

    // Registry datum: Constr 0 [registrations_list, owner]
    let (tag, fields) = match &plutus_data {
        PlutusData::Constr(c) => (c.tag, c.fields.iter().collect::<Vec<_>>()),
        _ => return Err(anyhow!("Registry datum is not a Constr")),
    };

    if tag != 121 {
        return Err(anyhow!("Invalid registry datum constructor: {}", tag));
    }

    if fields.is_empty() {
        return Ok(vec![]);
    }

    // Parse registrations list (field 0)
    let registrations_list = match &fields[0] {
        PlutusData::Array(arr) => arr.iter().collect::<Vec<_>>(),
        _ => return Err(anyhow!("Registrations field is not a list")),
    };

    let mut registrations = Vec::new();
    for entry in registrations_list {
        if let Ok(reg) = parse_registration_from_plutus(entry) {
            registrations.push(reg);
        }
    }

    Ok(registrations)
}

fn parse_registration_from_plutus(data: &pallas_primitives::conway::PlutusData) -> Result<RecipientInfo> {
    use pallas_primitives::conway::PlutusData;

    let (tag, fields) = match data {
        PlutusData::Constr(c) => (c.tag, c.fields.iter().collect::<Vec<_>>()),
        _ => return Err(anyhow!("Registration is not a Constr")),
    };

    // RecipientRegistration has 7 fields now:
    // script_hash, owner, state_locator, reference_script_locator, additional_inputs, recipient_type, custom_ism
    if tag != 121 || fields.len() < 7 {
        return Err(anyhow!("Invalid registration structure, expected 7 fields, got {}", fields.len()));
    }

    // Script hash (field 0)
    let script_hash = match &fields[0] {
        PlutusData::BoundedBytes(bytes) => hex::encode(bytes.as_ref() as &[u8]),
        _ => return Err(anyhow!("Invalid script_hash")),
    };

    // Owner (field 1) - VerificationKeyHash
    let owner = match &fields[1] {
        PlutusData::BoundedBytes(bytes) => hex::encode(bytes.as_ref() as &[u8]),
        _ => return Err(anyhow!("Invalid owner")),
    };

    // State locator (field 2)
    let (state_policy, state_asset) = parse_utxo_locator_from_plutus(&fields[2])?;

    // Reference script locator (field 3) - Option<UtxoLocator>
    let (ref_script_policy, ref_script_asset) = match &fields[3] {
        PlutusData::Constr(c) => {
            if c.tag == 121 {
                // Some(locator)
                if let Some(locator) = c.fields.first() {
                    let (policy, asset) = parse_utxo_locator_from_plutus(locator)?;
                    (Some(policy), Some(asset))
                } else {
                    (None, None)
                }
            } else {
                // None
                (None, None)
            }
        }
        _ => (None, None),
    };

    // Recipient type (field 5)
    let recipient_type = match &fields[5] {
        PlutusData::Constr(c) => match c.tag {
            121 => "GenericHandler",
            122 => "TokenReceiver",
            123 => "ContractCaller",
            _ => "Unknown",
        },
        _ => "Unknown",
    }.to_string();

    // Custom ISM (field 6)
    let custom_ism = match &fields[6] {
        PlutusData::Constr(c) => {
            if c.tag == 121 {
                if let Some(PlutusData::BoundedBytes(bytes)) = c.fields.first() {
                    Some(hex::encode(bytes.as_ref() as &[u8]))
                } else {
                    None
                }
            } else {
                None
            }
        }
        _ => None,
    };

    Ok(RecipientInfo {
        script_hash,
        owner,
        state_policy_id: state_policy,
        state_asset_name: state_asset,
        recipient_type,
        custom_ism,
        ref_script_policy_id: ref_script_policy,
        ref_script_asset_name: ref_script_asset,
    })
}

fn parse_utxo_locator_from_plutus(data: &pallas_primitives::conway::PlutusData) -> Result<(String, String)> {
    use pallas_primitives::conway::PlutusData;

    let (tag, fields) = match data {
        PlutusData::Constr(c) => (c.tag, c.fields.iter().collect::<Vec<_>>()),
        _ => return Err(anyhow!("Invalid UtxoLocator")),
    };

    if tag != 121 || fields.len() < 2 {
        return Err(anyhow!("Invalid UtxoLocator structure"));
    }

    let policy_id = match &fields[0] {
        PlutusData::BoundedBytes(bytes) => hex::encode(bytes.as_ref() as &[u8]),
        _ => return Err(anyhow!("Invalid policy_id")),
    };

    let asset_name = match &fields[1] {
        PlutusData::BoundedBytes(bytes) => hex::encode(bytes.as_ref() as &[u8]),
        _ => return Err(anyhow!("Invalid asset_name")),
    };

    Ok((policy_id, asset_name))
}

fn parse_registrations_from_json_fields(fields: &[serde_json::Value]) -> Result<Vec<RecipientInfo>> {
    let registrations_list = fields
        .get(0)
        .and_then(|v| v.get("list"))
        .and_then(|l| l.as_array())
        .ok_or_else(|| anyhow!("Missing registrations list"))?;

    let mut registrations = Vec::new();

    for entry in registrations_list {
        let entry_fields = entry
            .get("fields")
            .and_then(|f| f.as_array())
            .ok_or_else(|| anyhow!("Invalid registration entry"))?;

        // Field 0: script_hash
        let script_hash = entry_fields
            .get(0)
            .and_then(|h| h.get("bytes"))
            .and_then(|b| b.as_str())
            .ok_or_else(|| anyhow!("Invalid script hash"))?
            .to_string();

        // Field 1: owner (VerificationKeyHash)
        let owner = entry_fields
            .get(1)
            .and_then(|h| h.get("bytes"))
            .and_then(|b| b.as_str())
            .ok_or_else(|| anyhow!("Invalid owner"))?
            .to_string();

        // Field 2: state_locator
        let state_fields = entry_fields
            .get(2)
            .and_then(|s| s.get("fields"))
            .and_then(|f| f.as_array())
            .ok_or_else(|| anyhow!("Invalid state locator"))?;

        let state_policy = state_fields
            .get(0)
            .and_then(|p| p.get("bytes"))
            .and_then(|b| b.as_str())
            .unwrap_or("")
            .to_string();

        let state_asset = state_fields
            .get(1)
            .and_then(|a| a.get("bytes"))
            .and_then(|b| b.as_str())
            .unwrap_or("")
            .to_string();

        // Field 3: reference_script_locator - Option<UtxoLocator>
        let (ref_script_policy, ref_script_asset) = entry_fields
            .get(3)
            .and_then(|opt| {
                if opt.get("constructor") == Some(&serde_json::json!(0)) {
                    // Some(locator)
                    opt.get("fields")
                        .and_then(|f| f.as_array())
                        .and_then(|a| a.get(0))
                        .and_then(|locator| locator.get("fields"))
                        .and_then(|f| f.as_array())
                        .map(|locator_fields| {
                            let policy = locator_fields
                                .get(0)
                                .and_then(|p| p.get("bytes"))
                                .and_then(|b| b.as_str())
                                .unwrap_or("")
                                .to_string();
                            let asset = locator_fields
                                .get(1)
                                .and_then(|a| a.get("bytes"))
                                .and_then(|b| b.as_str())
                                .unwrap_or("")
                                .to_string();
                            (Some(policy), Some(asset))
                        })
                } else {
                    None
                }
            })
            .unwrap_or((None, None));

        // Field 5: recipient_type
        let recipient_type = entry_fields
            .get(5)
            .and_then(|t| t.get("constructor"))
            .and_then(|c| c.as_u64())
            .map(|c| match c {
                0 => "GenericHandler",
                1 => "TokenReceiver",
                2 => "ContractCaller",
                _ => "Unknown",
            })
            .unwrap_or("Unknown")
            .to_string();

        // Field 6: custom_ism
        let custom_ism = entry_fields
            .get(6)
            .and_then(|i| {
                if i.get("constructor") == Some(&serde_json::json!(0)) {
                    // Some(ism)
                    i.get("fields")
                        .and_then(|f| f.as_array())
                        .and_then(|a| a.get(0))
                        .and_then(|h| h.get("bytes"))
                        .and_then(|b| b.as_str())
                        .map(|s| s.to_string())
                } else {
                    None
                }
            });

        registrations.push(RecipientInfo {
            script_hash,
            owner,
            state_policy_id: state_policy,
            state_asset_name: state_asset,
            recipient_type,
            custom_ism,
            ref_script_policy_id: ref_script_policy,
            ref_script_asset_name: ref_script_asset,
        });
    }

    Ok(registrations)
}

//! ISM command - Manage Interchain Security Module validators

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};
use colored::Colorize;
use pallas_primitives::conway::{PlutusData, Constr, BigInt, BoundedBytes};
use pallas_primitives::MaybeIndefArray;

use crate::utils::blockfrost::BlockfrostClient;
use crate::utils::context::CliContext;

#[derive(Args)]
pub struct IsmArgs {
    #[command(subcommand)]
    command: IsmCommands,
}

#[derive(Subcommand)]
enum IsmCommands {
    /// Set validators for a domain
    SetValidators {
        /// Origin domain ID (e.g., 43113 for Fuji)
        #[arg(long)]
        domain: u32,

        /// Validator Ethereum addresses (20-byte hex, comma-separated)
        /// Example: ab8cc5ae0dcce3d0dff1925a70cda0250f06ba21,...
        #[arg(long, value_delimiter = ',')]
        validators: Vec<String>,

        /// Also set the threshold for this domain
        #[arg(long)]
        threshold: Option<u32>,

        /// ISM policy ID (for finding the ISM UTXO)
        #[arg(long)]
        ism_policy: Option<String>,

        /// Path to signing key
        #[arg(long)]
        signing_key: Option<String>,

        /// Dry run
        #[arg(long)]
        dry_run: bool,
    },

    /// Set threshold for a domain
    SetThreshold {
        /// Origin domain ID
        #[arg(long)]
        domain: u32,

        /// Required number of signatures
        #[arg(long)]
        threshold: u32,

        /// ISM policy ID
        #[arg(long)]
        ism_policy: Option<String>,

        /// Path to signing key
        #[arg(long)]
        signing_key: Option<String>,

        /// Dry run
        #[arg(long)]
        dry_run: bool,
    },

    /// Show current ISM configuration
    Show {
        /// ISM policy ID
        #[arg(long)]
        ism_policy: Option<String>,

        /// Filter by domain
        #[arg(long)]
        domain: Option<u32>,
    },

    /// Add a single validator to an existing set
    AddValidator {
        /// Origin domain ID
        #[arg(long)]
        domain: u32,

        /// Validator address to add (20-byte hex)
        #[arg(long)]
        validator: String,

        /// ISM policy ID
        #[arg(long)]
        ism_policy: Option<String>,

        /// Dry run
        #[arg(long)]
        dry_run: bool,
    },

    /// Remove a validator from an existing set
    RemoveValidator {
        /// Origin domain ID
        #[arg(long)]
        domain: u32,

        /// Validator address to remove (20-byte hex)
        #[arg(long)]
        validator: String,

        /// ISM policy ID
        #[arg(long)]
        ism_policy: Option<String>,

        /// Dry run
        #[arg(long)]
        dry_run: bool,
    },
}

pub async fn execute(ctx: &CliContext, args: IsmArgs) -> Result<()> {
    match args.command {
        IsmCommands::SetValidators {
            domain,
            validators,
            threshold,
            ism_policy,
            signing_key,
            dry_run,
        } => set_validators(ctx, domain, validators, threshold, ism_policy, signing_key, dry_run).await,
        IsmCommands::SetThreshold {
            domain,
            threshold,
            ism_policy,
            signing_key,
            dry_run,
        } => set_threshold(ctx, domain, threshold, ism_policy, signing_key, dry_run).await,
        IsmCommands::Show { ism_policy, domain } => show_config(ctx, ism_policy, domain).await,
        IsmCommands::AddValidator {
            domain,
            validator,
            ism_policy,
            dry_run,
        } => add_validator(ctx, domain, &validator, ism_policy, dry_run).await,
        IsmCommands::RemoveValidator {
            domain,
            validator,
            ism_policy,
            dry_run,
        } => remove_validator(ctx, domain, &validator, ism_policy, dry_run).await,
    }
}

async fn set_validators(
    ctx: &CliContext,
    domain: u32,
    validators: Vec<String>,
    threshold: Option<u32>,
    ism_policy: Option<String>,
    signing_key: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Setting ISM validators...".cyan());
    println!("  Domain: {}", domain);
    println!("  Validators: {}", validators.len());

    // Validate and normalize validator Ethereum addresses (20 bytes)
    let normalized: Vec<Vec<u8>> = validators
        .iter()
        .map(|v| {
            let v = v.strip_prefix("0x").unwrap_or(v);
            let bytes = hex::decode(v)?;
            if bytes.len() != 20 {
                Err(anyhow!(
                    "Validator address must be 20 bytes (40 hex chars), got {}: {}",
                    bytes.len(),
                    v
                ))
            } else {
                Ok(bytes)
            }
        })
        .collect::<Result<Vec<_>>>()?;

    for (i, v) in validators.iter().enumerate() {
        let v = v.strip_prefix("0x").unwrap_or(v);
        println!("  Validator {}: {}", i + 1, v);
    }

    // Get ISM policy ID
    let policy_id = get_ism_policy(ctx, ism_policy)?;
    println!("  ISM Policy: {}", policy_id);

    // Load signing key
    let keypair = if let Some(path) = signing_key {
        ctx.load_signing_key_from(std::path::Path::new(&path))?
    } else {
        ctx.load_signing_key()?
    };

    let payer_address = keypair.address_bech32(ctx.pallas_network());
    let payer_pkh = keypair.pub_key_hash();
    println!("  Payer: {}", payer_address);

    // Find ISM UTXO
    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let ism_utxo = client
        .find_utxo_by_asset(&policy_id, "")
        .await?
        .ok_or_else(|| anyhow!("ISM UTXO not found with policy {}", policy_id))?;

    println!("\n{}", "Found ISM UTXO:".green());
    println!("  TX: {}#{}", ism_utxo.tx_hash, ism_utxo.output_index);
    println!("  Address: {}", ism_utxo.address);
    println!("  Lovelace: {}", ism_utxo.lovelace);

    // Parse current datum to get owner and other fields
    let current_datum = ism_utxo
        .inline_datum
        .as_ref()
        .ok_or_else(|| anyhow!("ISM UTXO has no inline datum"))?;

    // Get current validators/thresholds from datum
    let (current_validators_map, current_thresholds_map, owner) =
        parse_ism_datum_full(current_datum)?;

    println!("  Owner: {}", hex::encode(&owner));

    // Verify we are the owner
    if owner != payer_pkh {
        return Err(anyhow!(
            "Signing key does not match ISM owner. Expected: {}, Got: {}",
            hex::encode(&owner),
            hex::encode(&payer_pkh)
        ));
    }

    // Build new datum with updated validators for this domain
    // NOTE: SetValidators redeemer ONLY updates validators, not thresholds.
    // The contract compares the output datum against expected, and thresholds must match.
    // Use update_assoc to maintain order and match contract behavior exactly.
    let new_validators_list = update_assoc(&current_validators_map, domain, normalized.clone());

    // Keep thresholds unchanged for SetValidators (use SetThreshold to change them)
    let new_thresholds_list = current_thresholds_map.clone();

    // Warn user if they provided --threshold
    if let Some(thresh) = threshold {
        if thresh == 0 || thresh as usize > validators.len() {
            return Err(anyhow!(
                "Threshold must be between 1 and {} (number of validators)",
                validators.len()
            ));
        }
        println!("\n{}", "NOTE: --threshold provided but SetValidators only updates validators.".yellow());
        println!("After this transaction, run a separate SetThreshold command to set threshold to {}.", thresh);
    }

    // Build the new datum
    let new_datum = build_ism_datum(&new_validators_list, &new_thresholds_list, &owner)?;
    let new_datum_cbor = pallas_codec::minicbor::to_vec(&new_datum)
        .map_err(|e| anyhow!("Failed to encode datum: {:?}", e))?;

    println!("\n{}", "New ISM Datum:".green());
    println!("  Validators for domain {}: {}", domain, validators.len());
    // Find threshold for this domain in the ordered list
    if let Some((_, thresh)) = new_thresholds_list.iter().find(|(d, _)| *d == domain) {
        println!("  Current threshold for domain {}: {}", domain, thresh);
    } else {
        println!("  No threshold set for domain {} (will need SetThreshold)", domain);
    }
    println!("  Datum CBOR: {}", hex::encode(&new_datum_cbor));

    // Build SetValidators redeemer
    let redeemer = build_set_validators_redeemer_plutus(domain, &normalized)?;
    let redeemer_cbor = pallas_codec::minicbor::to_vec(&redeemer)
        .map_err(|e| anyhow!("Failed to encode redeemer: {:?}", e))?;
    println!("\n{}", "SetValidators Redeemer:".green());
    println!("  CBOR: {}", hex::encode(&redeemer_cbor));

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        println!("\nTo update ISM, build a transaction that:");
        println!("1. Spends ISM UTXO: {}#{}", ism_utxo.tx_hash, ism_utxo.output_index);
        println!("2. Uses SetValidators redeemer: {}", hex::encode(&redeemer_cbor));
        println!("3. Creates new ISM UTXO with updated datum");
        println!("4. Requires owner signature: {}", hex::encode(&owner));
        return Ok(());
    }

    // Build and submit the transaction
    println!("\n{}", "Building transaction...".cyan());

    // Get payer UTXOs for fees and collateral
    let payer_utxos = client.get_utxos(&payer_address).await?;
    if payer_utxos.is_empty() {
        return Err(anyhow!("No UTXOs found for payer address"));
    }

    // Find collateral UTXO (pure ADA, no tokens, no reference script)
    let collateral_utxo = payer_utxos
        .iter()
        .find(|u| u.lovelace >= 5_000_000 && u.assets.is_empty() && u.reference_script.is_none())
        .ok_or_else(|| anyhow!("No suitable collateral UTXO (need 5+ ADA without tokens or reference scripts)"))?;

    // Find fee UTXO (pure ADA, no tokens, no reference script, different from collateral if possible)
    let fee_utxo = payer_utxos
        .iter()
        .find(|u| {
            u.lovelace >= 10_000_000 &&
            u.assets.is_empty() &&
            u.reference_script.is_none() &&
            (u.tx_hash != collateral_utxo.tx_hash || u.output_index != collateral_utxo.output_index)
        })
        .or_else(|| {
            // If no pure ADA UTXO with 10M, try with at least 5M
            payer_utxos.iter().find(|u| {
                u.lovelace >= 5_000_000 &&
                u.assets.is_empty() &&
                u.reference_script.is_none() &&
                (u.tx_hash != collateral_utxo.tx_hash || u.output_index != collateral_utxo.output_index)
            })
        })
        .unwrap_or(collateral_utxo);

    println!("  Collateral: {}#{}", collateral_utxo.tx_hash, collateral_utxo.output_index);
    println!("  Fee input: {}#{}", fee_utxo.tx_hash, fee_utxo.output_index);

    // Load ISM script from blueprint
    let blueprint = ctx.load_blueprint()?;
    let ism_validator = blueprint
        .find_validator("multisig_ism.multisig_ism.spend")
        .ok_or_else(|| anyhow!("ISM validator not found in blueprint"))?;
    let ism_script_bytes = hex::decode(&ism_validator.compiled_code)?;

    // Get PlutusV3 cost model
    let cost_model = client.get_plutusv3_cost_model().await?;

    // Get current slot for validity
    let current_slot = client.get_latest_slot().await?;
    let validity_end = current_slot + 7200; // ~2 hours

    // Build the transaction using pallas_txbuilder
    use pallas_txbuilder::{BuildConway, Input, Output, StagingTransaction, ScriptKind, ExUnits};
    use pallas_crypto::hash::Hash;

    // Parse addresses and hashes
    let ism_address = pallas_addresses::Address::from_bech32(&ism_utxo.address)
        .map_err(|e| anyhow!("Invalid ISM address: {:?}", e))?;
    let payer_addr = pallas_addresses::Address::from_bech32(&payer_address)
        .map_err(|e| anyhow!("Invalid payer address: {:?}", e))?;

    let ism_tx_hash: [u8; 32] = hex::decode(&ism_utxo.tx_hash)?
        .try_into().map_err(|_| anyhow!("Invalid ISM tx hash"))?;
    let collateral_tx_hash: [u8; 32] = hex::decode(&collateral_utxo.tx_hash)?
        .try_into().map_err(|_| anyhow!("Invalid collateral tx hash"))?;
    let fee_tx_hash: [u8; 32] = hex::decode(&fee_utxo.tx_hash)?
        .try_into().map_err(|_| anyhow!("Invalid fee tx hash"))?;
    let policy_id_bytes: [u8; 28] = hex::decode(&policy_id)?
        .try_into().map_err(|_| anyhow!("Invalid policy ID"))?;
    let owner_hash: [u8; 28] = owner.clone()
        .try_into().map_err(|_| anyhow!("Invalid owner hash"))?;

    // Build ISM continuation output with new datum and state NFT
    // Get the asset name from the input UTXO (should be "ISM State" hex-encoded)
    let state_nft_asset = ism_utxo
        .assets
        .iter()
        .find(|a| a.policy_id == policy_id)
        .ok_or_else(|| anyhow!("State NFT not found in ISM UTXO"))?;
    let asset_name_bytes = hex::decode(&state_nft_asset.asset_name)
        .unwrap_or_default();

    let ism_output = Output::new(ism_address, ism_utxo.lovelace)
        .set_inline_datum(new_datum_cbor.clone())
        .add_asset(Hash::new(policy_id_bytes), asset_name_bytes, 1)
        .map_err(|e| anyhow!("Failed to add state NFT: {:?}", e))?;

    // Calculate change
    let fee_estimate = 2_000_000u64;
    let change = fee_utxo.lovelace.saturating_sub(fee_estimate);

    // Build staging transaction
    let mut staging = StagingTransaction::new()
        // ISM script input
        .input(Input::new(Hash::new(ism_tx_hash), ism_utxo.output_index as u64))
        // Fee input
        .input(Input::new(Hash::new(fee_tx_hash), fee_utxo.output_index as u64))
        // Collateral
        .collateral_input(Input::new(Hash::new(collateral_tx_hash), collateral_utxo.output_index as u64))
        // ISM continuation output
        .output(ism_output)
        // Spend redeemer for ISM input (use redeemer_cbor bytes)
        .add_spend_redeemer(
            Input::new(Hash::new(ism_tx_hash), ism_utxo.output_index as u64),
            redeemer_cbor.clone(),
            Some(ExUnits { mem: 5_000_000, steps: 2_000_000_000 }),
        )
        // ISM script
        .script(ScriptKind::PlutusV3, ism_script_bytes)
        // Cost model for script data hash
        .language_view(ScriptKind::PlutusV3, cost_model)
        // Required signer (owner)
        .disclosed_signer(Hash::new(owner_hash))
        // Fee and validity
        .fee(fee_estimate)
        .invalid_from_slot(validity_end)
        .network_id(0); // Testnet

    // Add change output if significant
    if change > 1_500_000 {
        staging = staging.output(Output::new(payer_addr.clone(), change));
    }

    // Build the transaction
    let tx = staging.build_conway_raw()
        .map_err(|e| anyhow!("Failed to build transaction: {:?}", e))?;

    println!("  TX Hash: {}", hex::encode(&tx.tx_hash.0));

    // Sign the transaction
    println!("{}", "Signing transaction...".cyan());
    let tx_hash_bytes: &[u8] = &tx.tx_hash.0;
    let signature = keypair.sign(tx_hash_bytes);
    let signed_tx = tx.add_signature(keypair.pallas_public_key().clone(), signature)
        .map_err(|e| anyhow!("Failed to sign transaction: {:?}", e))?;

    // Submit the transaction
    println!("{}", "Submitting transaction...".cyan());
    let tx_hash = client.submit_tx(&signed_tx.tx_bytes.0).await?;

    println!("\n{}", "SUCCESS!".green().bold());
    println!("  Transaction Hash: {}", tx_hash);
    println!("  Explorer: {}", ctx.explorer_tx_url(&tx_hash));
    println!("\n  Domain: {}", domain);
    println!("  Validators: {} (threshold {})", validators.len(), threshold.unwrap_or(0));

    Ok(())
}

async fn set_threshold(
    ctx: &CliContext,
    domain: u32,
    threshold: u32,
    ism_policy: Option<String>,
    signing_key: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Setting ISM threshold...".cyan());
    println!("  Domain: {}", domain);
    println!("  Threshold: {}", threshold);

    if threshold == 0 {
        return Err(anyhow!("Threshold must be at least 1"));
    }

    let policy_id = get_ism_policy(ctx, ism_policy)?;
    println!("  ISM Policy: {}", policy_id);

    // Load signing key
    let keypair = if let Some(path) = signing_key {
        ctx.load_signing_key_from(std::path::Path::new(&path))?
    } else {
        ctx.load_signing_key()?
    };

    let payer_address = keypair.address_bech32(ctx.pallas_network());
    let payer_pkh = keypair.pub_key_hash();
    println!("  Payer: {}", payer_address);

    // Find ISM UTXO
    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let ism_utxo = client
        .find_utxo_by_asset(&policy_id, "")
        .await?
        .ok_or_else(|| anyhow!("ISM UTXO not found with policy {}", policy_id))?;

    println!("\n{}", "Found ISM UTXO:".green());
    println!("  TX: {}#{}", ism_utxo.tx_hash, ism_utxo.output_index);
    println!("  Address: {}", ism_utxo.address);
    println!("  Lovelace: {}", ism_utxo.lovelace);

    // Parse current datum to get owner and other fields
    let current_datum = ism_utxo
        .inline_datum
        .as_ref()
        .ok_or_else(|| anyhow!("ISM UTXO has no inline datum"))?;

    // Get current validators/thresholds from datum
    let (current_validators_map, current_thresholds_map, owner) =
        parse_ism_datum_full(current_datum)?;

    println!("  Owner: {}", hex::encode(&owner));

    // Verify we are the owner
    if owner != payer_pkh {
        return Err(anyhow!(
            "Signing key does not match ISM owner. Expected: {}, Got: {}",
            hex::encode(&owner),
            hex::encode(&payer_pkh)
        ));
    }

    // Check that validators exist for this domain
    let domain_validators = current_validators_map
        .iter()
        .find(|(d, _)| *d == domain)
        .map(|(_, v)| v.clone());

    if let Some(ref validators) = domain_validators {
        if threshold as usize > validators.len() {
            return Err(anyhow!(
                "Threshold {} exceeds number of validators {} for domain {}",
                threshold,
                validators.len(),
                domain
            ));
        }
    } else {
        println!("{}", format!("Warning: No validators set for domain {}. Set validators first.", domain).yellow());
    }

    // Build new datum with updated threshold for this domain
    // Keep validators unchanged, only update thresholds
    let new_validators_list = current_validators_map.clone();
    let new_thresholds_list = update_assoc(&current_thresholds_map, domain, threshold);

    // Build the new datum
    let new_datum = build_ism_datum(&new_validators_list, &new_thresholds_list, &owner)?;
    let new_datum_cbor = pallas_codec::minicbor::to_vec(&new_datum)
        .map_err(|e| anyhow!("Failed to encode datum: {:?}", e))?;

    println!("\n{}", "New ISM Datum:".green());
    println!("  Threshold for domain {}: {}", domain, threshold);
    println!("  Datum CBOR: {}", hex::encode(&new_datum_cbor));

    // Build SetThreshold redeemer
    let redeemer = build_set_threshold_redeemer_plutus(domain, threshold);
    let redeemer_cbor = pallas_codec::minicbor::to_vec(&redeemer)
        .map_err(|e| anyhow!("Failed to encode redeemer: {:?}", e))?;
    println!("\n{}", "SetThreshold Redeemer:".green());
    println!("  CBOR: {}", hex::encode(&redeemer_cbor));

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        println!("\nTo update ISM threshold, build a transaction that:");
        println!("1. Spends ISM UTXO: {}#{}", ism_utxo.tx_hash, ism_utxo.output_index);
        println!("2. Uses SetThreshold redeemer: {}", hex::encode(&redeemer_cbor));
        println!("3. Creates new ISM UTXO with updated datum");
        println!("4. Requires owner signature: {}", hex::encode(&owner));
        return Ok(());
    }

    // Build and submit the transaction
    println!("\n{}", "Building transaction...".cyan());

    // Get payer UTXOs for fees and collateral
    let payer_utxos = client.get_utxos(&payer_address).await?;
    if payer_utxos.is_empty() {
        return Err(anyhow!("No UTXOs found for payer address"));
    }

    // Find collateral UTXO (pure ADA, no tokens, no reference script)
    let collateral_utxo = payer_utxos
        .iter()
        .find(|u| u.lovelace >= 5_000_000 && u.assets.is_empty() && u.reference_script.is_none())
        .ok_or_else(|| anyhow!("No suitable collateral UTXO (need 5+ ADA without tokens or reference scripts)"))?;

    // Find fee UTXO (pure ADA, no tokens, no reference script, different from collateral if possible)
    let fee_utxo = payer_utxos
        .iter()
        .find(|u| {
            u.lovelace >= 10_000_000 &&
            u.assets.is_empty() &&
            u.reference_script.is_none() &&
            (u.tx_hash != collateral_utxo.tx_hash || u.output_index != collateral_utxo.output_index)
        })
        .or_else(|| {
            payer_utxos.iter().find(|u| {
                u.lovelace >= 5_000_000 &&
                u.assets.is_empty() &&
                u.reference_script.is_none() &&
                (u.tx_hash != collateral_utxo.tx_hash || u.output_index != collateral_utxo.output_index)
            })
        })
        .unwrap_or(collateral_utxo);

    println!("  Collateral: {}#{}", collateral_utxo.tx_hash, collateral_utxo.output_index);
    println!("  Fee input: {}#{}", fee_utxo.tx_hash, fee_utxo.output_index);

    // Load ISM script from blueprint
    let blueprint = ctx.load_blueprint()?;
    let ism_validator = blueprint
        .find_validator("multisig_ism.multisig_ism.spend")
        .ok_or_else(|| anyhow!("ISM validator not found in blueprint"))?;
    let ism_script_bytes = hex::decode(&ism_validator.compiled_code)?;

    // Get PlutusV3 cost model
    let cost_model = client.get_plutusv3_cost_model().await?;

    // Get current slot for validity
    let current_slot = client.get_latest_slot().await?;
    let validity_end = current_slot + 7200; // ~2 hours

    // Build the transaction using pallas_txbuilder
    use pallas_txbuilder::{BuildConway, Input, Output, StagingTransaction, ScriptKind, ExUnits};
    use pallas_crypto::hash::Hash;

    // Parse addresses and hashes
    let ism_address = pallas_addresses::Address::from_bech32(&ism_utxo.address)
        .map_err(|e| anyhow!("Invalid ISM address: {:?}", e))?;
    let payer_addr = pallas_addresses::Address::from_bech32(&payer_address)
        .map_err(|e| anyhow!("Invalid payer address: {:?}", e))?;

    let ism_tx_hash: [u8; 32] = hex::decode(&ism_utxo.tx_hash)?
        .try_into().map_err(|_| anyhow!("Invalid ISM tx hash"))?;
    let collateral_tx_hash: [u8; 32] = hex::decode(&collateral_utxo.tx_hash)?
        .try_into().map_err(|_| anyhow!("Invalid collateral tx hash"))?;
    let fee_tx_hash: [u8; 32] = hex::decode(&fee_utxo.tx_hash)?
        .try_into().map_err(|_| anyhow!("Invalid fee tx hash"))?;
    let policy_id_bytes: [u8; 28] = hex::decode(&policy_id)?
        .try_into().map_err(|_| anyhow!("Invalid policy ID"))?;
    let owner_hash: [u8; 28] = owner.clone()
        .try_into().map_err(|_| anyhow!("Invalid owner hash"))?;

    // Get asset name from the ISM UTXO
    let state_nft_asset = ism_utxo
        .assets
        .iter()
        .find(|a| a.policy_id == policy_id)
        .ok_or_else(|| anyhow!("State NFT not found in ISM UTXO"))?;
    let asset_name_bytes = hex::decode(&state_nft_asset.asset_name)
        .unwrap_or_default();

    // Build ISM continuation output with new datum and state NFT
    let ism_output = Output::new(ism_address, ism_utxo.lovelace)
        .set_inline_datum(new_datum_cbor.clone())
        .add_asset(Hash::new(policy_id_bytes), asset_name_bytes, 1)
        .map_err(|e| anyhow!("Failed to add state NFT: {:?}", e))?;

    // Calculate change
    let fee_estimate = 2_000_000u64;
    let change = fee_utxo.lovelace.saturating_sub(fee_estimate);

    // Check for reference script UTXO in deployment info
    let ref_script_utxo = ctx.load_deployment_info()
        .ok()
        .and_then(|d| d.ism)
        .and_then(|ism| ism.reference_script_utxo)
        .map(|r| (r.tx_hash, r.output_index));

    // Build staging transaction
    let mut staging = StagingTransaction::new()
        // ISM script input
        .input(Input::new(Hash::new(ism_tx_hash), ism_utxo.output_index as u64))
        // Fee input
        .input(Input::new(Hash::new(fee_tx_hash), fee_utxo.output_index as u64))
        // Collateral
        .collateral_input(Input::new(Hash::new(collateral_tx_hash), collateral_utxo.output_index as u64))
        // ISM continuation output
        .output(ism_output)
        // Spend redeemer for ISM input
        .add_spend_redeemer(
            Input::new(Hash::new(ism_tx_hash), ism_utxo.output_index as u64),
            redeemer_cbor.clone(),
            Some(ExUnits { mem: 5_000_000, steps: 2_000_000_000 }),
        )
        // Cost model for script data hash
        .language_view(ScriptKind::PlutusV3, cost_model)
        // Required signer (owner)
        .disclosed_signer(Hash::new(owner_hash))
        // Fee and validity
        .fee(fee_estimate)
        .invalid_from_slot(validity_end)
        .network_id(0); // Testnet

    // Add reference script OR embedded script
    if let Some((ref_tx_hash, ref_output_idx)) = ref_script_utxo {
        println!("  Using reference script: {}#{}", ref_tx_hash, ref_output_idx);
        let ref_tx_hash_bytes: [u8; 32] = hex::decode(&ref_tx_hash)?
            .try_into().map_err(|_| anyhow!("Invalid reference script tx hash"))?;
        staging = staging.reference_input(Input::new(Hash::new(ref_tx_hash_bytes), ref_output_idx as u64));
    } else {
        println!("  Using embedded script (no reference script found)");
        staging = staging.script(ScriptKind::PlutusV3, ism_script_bytes);
    }

    // Add change output if significant
    if change > 1_500_000 {
        staging = staging.output(Output::new(payer_addr.clone(), change));
    }

    // Build the transaction
    let tx = staging.build_conway_raw()
        .map_err(|e| anyhow!("Failed to build transaction: {:?}", e))?;

    println!("  TX Hash: {}", hex::encode(&tx.tx_hash.0));

    // Sign the transaction
    println!("{}", "Signing transaction...".cyan());
    let tx_hash_bytes: &[u8] = &tx.tx_hash.0;
    let signature = keypair.sign(tx_hash_bytes);
    let signed_tx = tx.add_signature(keypair.pallas_public_key().clone(), signature)
        .map_err(|e| anyhow!("Failed to sign transaction: {:?}", e))?;

    // Submit the transaction
    println!("{}", "Submitting transaction...".cyan());
    let tx_hash = client.submit_tx(&signed_tx.tx_bytes.0).await?;

    println!("\n{}", "SUCCESS!".green().bold());
    println!("  Transaction Hash: {}", tx_hash);
    println!("  Explorer: {}", ctx.explorer_tx_url(&tx_hash));
    println!("\n  Domain: {}", domain);
    println!("  New threshold: {}", threshold);

    Ok(())
}

async fn show_config(
    ctx: &CliContext,
    ism_policy: Option<String>,
    domain_filter: Option<u32>,
) -> Result<()> {
    println!("{}", "ISM Configuration".cyan());

    let policy_id = get_ism_policy(ctx, ism_policy)?;
    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let ism_utxo = client
        .find_utxo_by_asset(&policy_id, "")
        .await?
        .ok_or_else(|| anyhow!("ISM UTXO not found with policy {}", policy_id))?;

    println!("\n{}", "ISM UTXO:".green());
    println!("  TX: {}#{}", ism_utxo.tx_hash, ism_utxo.output_index);
    println!("  Address: {}", ism_utxo.address);
    println!("  Lovelace: {}", ism_utxo.lovelace);

    if let Some(datum) = &ism_utxo.inline_datum {
        // Parse datum using the existing function that handles both CBOR hex and JSON
        match parse_ism_datum_full(datum) {
            Ok((validators_list, thresholds_list, owner)) => {
                println!("\n{}", "Parsed Configuration:".green());

                // Display validators by domain
                println!("\n  {}:", "Validators".cyan());
                for (domain, validators) in &validators_list {
                    if let Some(filter) = domain_filter {
                        if *domain != filter {
                            continue;
                        }
                    }
                    println!("    Domain {}:", domain);
                    for validator in validators {
                        println!("      - 0x{}", hex::encode(validator));
                    }
                }

                // Display thresholds by domain
                println!("\n  {}:", "Thresholds".cyan());
                for (domain, threshold) in &thresholds_list {
                    if let Some(filter) = domain_filter {
                        if *domain != filter {
                            continue;
                        }
                    }
                    println!("    Domain {}: {}", domain, threshold);
                }

                // Display owner
                println!("\n  {}: {}", "Owner".cyan(), hex::encode(&owner));
            }
            Err(e) => {
                // Fallback: show raw datum if parsing fails
                println!("\n{}", "Inline Datum (raw):".yellow());
                println!("{}", serde_json::to_string_pretty(datum)?);
                println!("\n{}", format!("Note: Could not parse datum: {}", e).yellow());
            }
        }
    } else {
        println!("\n{}", "No inline datum found".yellow());
    }

    Ok(())
}

async fn add_validator(
    ctx: &CliContext,
    domain: u32,
    validator: &str,
    ism_policy: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Adding validator to ISM...".cyan());
    println!("  Domain: {}", domain);
    println!("  Validator: {}", validator);

    // First get current validators
    let policy_id = get_ism_policy(ctx, ism_policy.clone())?;
    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let ism_utxo = client
        .find_utxo_by_asset(&policy_id, "")
        .await?
        .ok_or_else(|| anyhow!("ISM UTXO not found"))?;

    // Parse current validators from datum
    let current_validators = parse_validators_from_datum(&ism_utxo.inline_datum, domain)?;
    println!("  Current validators: {:?}", current_validators);

    // Add new validator
    let normalized = validator.strip_prefix("0x").unwrap_or(validator).to_lowercase();
    if current_validators.contains(&normalized) {
        println!("{}", "Validator already exists!".yellow());
        return Ok(());
    }

    let mut new_validators = current_validators;
    new_validators.push(normalized);

    println!("  New validators: {:?}", new_validators);

    // Call set_validators with updated list
    set_validators(ctx, domain, new_validators, None, ism_policy, None, dry_run).await
}

async fn remove_validator(
    ctx: &CliContext,
    domain: u32,
    validator: &str,
    ism_policy: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Removing validator from ISM...".cyan());
    println!("  Domain: {}", domain);
    println!("  Validator: {}", validator);

    let policy_id = get_ism_policy(ctx, ism_policy.clone())?;
    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let ism_utxo = client
        .find_utxo_by_asset(&policy_id, "")
        .await?
        .ok_or_else(|| anyhow!("ISM UTXO not found"))?;

    let current_validators = parse_validators_from_datum(&ism_utxo.inline_datum, domain)?;
    let normalized = validator.strip_prefix("0x").unwrap_or(validator).to_lowercase();

    if !current_validators.contains(&normalized) {
        println!("{}", "Validator not found in current set!".yellow());
        return Ok(());
    }

    let new_validators: Vec<String> = current_validators
        .into_iter()
        .filter(|v| v != &normalized)
        .collect();

    if new_validators.is_empty() {
        return Err(anyhow!("Cannot remove last validator"));
    }

    println!("  New validators: {:?}", new_validators);

    set_validators(ctx, domain, new_validators, None, ism_policy, None, dry_run).await
}

// Helper functions

fn get_ism_policy(ctx: &CliContext, ism_policy: Option<String>) -> Result<String> {
    if let Some(p) = ism_policy {
        return Ok(p);
    }

    // Try to load from deployment info
    let deployment = ctx.load_deployment_info()?;
    deployment
        .ism
        .and_then(|i| i.state_nft_policy)
        .ok_or_else(|| anyhow!("ISM policy not found. Use --ism-policy or update deployment_info.json"))
}

fn parse_validators_from_datum(
    datum: &Option<serde_json::Value>,
    domain: u32,
) -> Result<Vec<String>> {
    let datum = datum.as_ref().ok_or_else(|| anyhow!("No inline datum"))?;

    let fields = datum
        .get("fields")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow!("Invalid datum structure"))?;

    let validators_list = fields
        .get(0)
        .and_then(|v| v.get("list"))
        .and_then(|l| l.as_array())
        .ok_or_else(|| anyhow!("Missing validators list"))?;

    for entry in validators_list {
        let entry_fields = entry
            .get("fields")
            .and_then(|f| f.as_array())
            .ok_or_else(|| anyhow!("Invalid validator entry"))?;

        let entry_domain = entry_fields
            .get(0)
            .and_then(|d| d.get("int"))
            .and_then(|i| i.as_u64())
            .ok_or_else(|| anyhow!("Invalid domain"))? as u32;

        if entry_domain == domain {
            let addrs = entry_fields
                .get(1)
                .and_then(|a| a.get("list"))
                .and_then(|l| l.as_array())
                .ok_or_else(|| anyhow!("Invalid validators"))?;

            return addrs
                .iter()
                .map(|a| {
                    a.get("bytes")
                        .and_then(|b| b.as_str())
                        .map(|s| s.to_string())
                        .ok_or_else(|| anyhow!("Invalid validator bytes"))
                })
                .collect();
        }
    }

    Ok(vec![])
}

/// Parse full ISM datum to get validators, thresholds, and owner
/// Parse ISM datum, returning ordered lists to preserve entry order (critical for Plutus comparison)
fn parse_ism_datum_full(
    datum: &serde_json::Value,
) -> Result<(Vec<(u32, Vec<Vec<u8>>)>, Vec<(u32, u32)>, Vec<u8>)> {
    // Check if datum is a hex string (raw CBOR from Blockfrost)
    if let Some(hex_str) = datum.as_str() {
        return parse_ism_datum_from_cbor(hex_str);
    }

    // Otherwise try the JSON format
    let fields = datum
        .get("fields")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow!("Invalid datum structure - missing fields"))?;

    // Parse validators (field 0) - maintain order!
    let mut validators_list: Vec<(u32, Vec<Vec<u8>>)> = Vec::new();
    if let Some(validators_arr) = fields.get(0).and_then(|v| v.get("list")).and_then(|l| l.as_array()) {
        for entry in validators_arr {
            if let Some(entry_fields) = entry.get("fields").and_then(|f| f.as_array()) {
                let domain = entry_fields
                    .get(0)
                    .and_then(|d| d.get("int"))
                    .and_then(|i| i.as_u64())
                    .ok_or_else(|| anyhow!("Invalid domain in validators"))? as u32;

                let mut validator_keys = Vec::new();
                if let Some(keys) = entry_fields.get(1).and_then(|a| a.get("list")).and_then(|l| l.as_array()) {
                    for key in keys {
                        if let Some(bytes_str) = key.get("bytes").and_then(|b| b.as_str()) {
                            let bytes = hex::decode(bytes_str)?;
                            validator_keys.push(bytes);
                        }
                    }
                }
                validators_list.push((domain, validator_keys));
            }
        }
    }

    // Parse thresholds (field 1) - maintain order!
    let mut thresholds_list: Vec<(u32, u32)> = Vec::new();
    if let Some(thresholds_arr) = fields.get(1).and_then(|v| v.get("list")).and_then(|l| l.as_array()) {
        for entry in thresholds_arr {
            if let Some(entry_fields) = entry.get("fields").and_then(|f| f.as_array()) {
                let domain = entry_fields
                    .get(0)
                    .and_then(|d| d.get("int"))
                    .and_then(|i| i.as_u64())
                    .ok_or_else(|| anyhow!("Invalid domain in thresholds"))? as u32;

                let threshold = entry_fields
                    .get(1)
                    .and_then(|t| t.get("int"))
                    .and_then(|i| i.as_u64())
                    .ok_or_else(|| anyhow!("Invalid threshold"))? as u32;

                thresholds_list.push((domain, threshold));
            }
        }
    }

    // Parse owner (field 2)
    let owner = fields
        .get(2)
        .and_then(|o| o.get("bytes"))
        .and_then(|b| b.as_str())
        .ok_or_else(|| anyhow!("Missing owner field"))?;
    let owner_bytes = hex::decode(owner)?;

    Ok((validators_list, thresholds_list, owner_bytes))
}

/// Parse ISM datum from raw CBOR hex
/// Returns ordered lists (Vec) to preserve entry order - critical for Plutus comparison!
fn parse_ism_datum_from_cbor(
    hex_str: &str,
) -> Result<(Vec<(u32, Vec<Vec<u8>>)>, Vec<(u32, u32)>, Vec<u8>)> {
    let cbor_bytes = hex::decode(hex_str)?;
    let datum: PlutusData = pallas_codec::minicbor::decode(&cbor_bytes)
        .map_err(|e| anyhow!("Failed to decode CBOR datum: {:?}", e))?;

    // ISM Datum is Constr 0 [validators_list, thresholds_list, owner]
    let (tag, fields) = match &datum {
        PlutusData::Constr(c) => (c.tag, &c.fields),
        _ => return Err(anyhow!("Expected Constr datum")),
    };

    if tag != 121 {
        return Err(anyhow!("Expected Constr 0 (tag 121), got tag {}", tag));
    }

    let fields_vec: Vec<&PlutusData> = fields.iter().collect();
    if fields_vec.len() < 3 {
        return Err(anyhow!("ISM datum must have 3 fields, got {}", fields_vec.len()));
    }

    // Parse validators (field 0) - MAINTAIN ORDER
    // Tuples in Aiken can be encoded as Constr 0 (tag 121)
    let mut validators_vec: Vec<(u32, Vec<Vec<u8>>)> = Vec::new();
    if let PlutusData::Array(validators_list) = fields_vec[0] {
        for entry in validators_list.iter() {
            // Tuple (domain, keys) can be Constr 0 or Array
            let entry_fields: Vec<&PlutusData> = match entry {
                PlutusData::Constr(c) => c.fields.iter().collect(),
                PlutusData::Array(arr) => arr.iter().collect(),
                _ => continue,
            };
            if entry_fields.len() >= 2 {
                let domain = extract_u32(entry_fields[0])?;
                let mut keys = Vec::new();
                if let PlutusData::Array(keys_list) = entry_fields[1] {
                    for k in keys_list.iter() {
                        if let PlutusData::BoundedBytes(b) = k {
                            keys.push(b.to_vec());
                        }
                    }
                }
                validators_vec.push((domain, keys));
            }
        }
    }

    // Parse thresholds (field 1) - MAINTAIN ORDER
    // Tuples in Aiken can be encoded as Constr 0 (tag 121)
    let mut thresholds_vec: Vec<(u32, u32)> = Vec::new();
    if let PlutusData::Array(thresholds_list) = fields_vec[1] {
        for entry in thresholds_list.iter() {
            // Tuple (domain, threshold) can be Constr 0 or Array
            let entry_fields: Vec<&PlutusData> = match entry {
                PlutusData::Constr(c) => c.fields.iter().collect(),
                PlutusData::Array(arr) => arr.iter().collect(),
                _ => continue,
            };
            if entry_fields.len() >= 2 {
                let domain = extract_u32(entry_fields[0])?;
                let threshold = extract_u32(entry_fields[1])?;
                thresholds_vec.push((domain, threshold));
            }
        }
    }

    // Parse owner (field 2)
    let owner = match fields_vec[2] {
        PlutusData::BoundedBytes(b) => b.to_vec(),
        _ => return Err(anyhow!("Expected owner as bytes")),
    };

    Ok((validators_vec, thresholds_vec, owner))
}

/// Extract u32 from PlutusData
fn extract_u32(data: &PlutusData) -> Result<u32> {
    match data {
        PlutusData::BigInt(BigInt::Int(i)) => {
            // pallas Int wraps minicbor::data::Int
            // Access the inner value and convert to i64
            let inner = &i.0;
            match i64::try_from(*inner) {
                Ok(val) => Ok(val as u32),
                Err(_) => Err(anyhow!("Integer too large for u32")),
            }
        }
        _ => Err(anyhow!("Expected integer")),
    }
}

/// Build ISM datum as PlutusData from ORDERED lists (Vec preserves order)
/// Uses indefinite-length encoding to match Aiken's native encoding style
fn build_ism_datum(
    validators: &[(u32, Vec<Vec<u8>>)],
    thresholds: &[(u32, u32)],
    owner: &[u8],
) -> Result<PlutusData> {
    // ISM Datum structure (Aiken type MultisigIsmDatum):
    // Constr 0 [
    //   List<(Domain, List<ByteArray>)>,  // validators
    //   List<(Domain, Int)>,              // thresholds
    //   ByteArray                         // owner
    // ]
    // NOTE: In Plutus/Aiken, 2-tuples are encoded as plain CBOR arrays [a, b], NOT as Constr 0
    // CRITICAL: Order must be preserved for Plutus datum comparison!
    // Using Indef encoding to match Aiken's native encoding style

    // Build validators list - each (domain, keys) tuple is a plain array [domain, keys_list]
    let validators_list: Vec<PlutusData> = validators
        .iter()
        .map(|(domain, keys)| {
            let keys_data: Vec<PlutusData> = keys
                .iter()
                .map(|k| PlutusData::BoundedBytes(BoundedBytes::from(k.clone())))
                .collect();

            // Tuple (Domain, List<ByteArray>) is a plain array [domain, keys], NOT Constr 0
            PlutusData::Array(MaybeIndefArray::Indef(vec![
                PlutusData::BigInt(BigInt::Int((*domain as i64).into())),
                PlutusData::Array(MaybeIndefArray::Indef(keys_data)),
            ]))
        })
        .collect();

    // Build thresholds list - each (domain, threshold) tuple is a plain array [domain, threshold]
    let thresholds_list: Vec<PlutusData> = thresholds
        .iter()
        .map(|(domain, threshold)| {
            // Tuple (Domain, Int) is a plain array [domain, threshold], NOT Constr 0
            PlutusData::Array(MaybeIndefArray::Indef(vec![
                PlutusData::BigInt(BigInt::Int((*domain as i64).into())),
                PlutusData::BigInt(BigInt::Int((*threshold as i64).into())),
            ]))
        })
        .collect();

    // Build the full datum using Indef encoding
    Ok(PlutusData::Constr(Constr {
        tag: 121, // Constr 0
        any_constructor: None,
        fields: MaybeIndefArray::Indef(vec![
            PlutusData::Array(MaybeIndefArray::Indef(validators_list)),
            PlutusData::Array(MaybeIndefArray::Indef(thresholds_list)),
            PlutusData::BoundedBytes(BoundedBytes::from(owner.to_vec())),
        ]),
    }))
}

/// Update or add entry in ordered list (mirrors Aiken's update_assoc)
fn update_assoc<V: Clone>(list: &[(u32, V)], key: u32, value: V) -> Vec<(u32, V)> {
    let mut result = Vec::new();
    let mut found = false;
    for (k, v) in list {
        if *k == key {
            result.push((key, value.clone()));
            found = true;
        } else {
            result.push((*k, v.clone()));
        }
    }
    if !found {
        result.push((key, value));
    }
    result
}

/// Build SetValidators redeemer as PlutusData
/// SetValidators { domain: Domain, validators: List<ByteArray> }
fn build_set_validators_redeemer_plutus(
    domain: u32,
    validators: &[Vec<u8>],
) -> Result<PlutusData> {
    // SetValidators is Constr 1 (Verify=0, SetValidators=1, SetThreshold=2)
    let validators_data: Vec<PlutusData> = validators
        .iter()
        .map(|v| PlutusData::BoundedBytes(BoundedBytes::from(v.clone())))
        .collect();

    Ok(PlutusData::Constr(Constr {
        tag: 122, // Constr 1
        any_constructor: None,
        fields: MaybeIndefArray::Def(vec![
            PlutusData::BigInt(BigInt::Int((domain as i64).into())),
            PlutusData::Array(MaybeIndefArray::Def(validators_data)),
        ]),
    }))
}

/// Build SetThreshold redeemer as PlutusData
/// SetThreshold { domain: Domain, threshold: Int }
fn build_set_threshold_redeemer_plutus(
    domain: u32,
    threshold: u32,
) -> PlutusData {
    // SetThreshold is Constr 2 (Verify=0, SetValidators=1, SetThreshold=2)
    PlutusData::Constr(Constr {
        tag: 123, // Constr 2
        any_constructor: None,
        fields: MaybeIndefArray::Def(vec![
            PlutusData::BigInt(BigInt::Int((domain as i64).into())),
            PlutusData::BigInt(BigInt::Int((threshold as i64).into())),
        ]),
    })
}

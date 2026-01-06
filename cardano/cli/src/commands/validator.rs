//! Validator command - Manage validator announcements for Hyperlane

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};
use colored::Colorize;
use pallas_crypto::hash::Hash;
use pallas_txbuilder::{BuildConway, Input, Output, ScriptKind, StagingTransaction};

use crate::utils::blockfrost::BlockfrostClient;
use crate::utils::cbor::CborBuilder;
use crate::utils::context::CliContext;
use crate::utils::crypto::Keypair;
use crate::utils::plutus::{apply_validator_params, script_hash_to_address};
use crate::utils::types::Utxo;

#[derive(Args)]
pub struct ValidatorArgs {
    #[command(subcommand)]
    command: ValidatorCommands,
}

#[derive(Subcommand)]
enum ValidatorCommands {
    /// Announce validator storage location on-chain
    ///
    /// Validators must announce their checkpoint storage location so relayers
    /// can discover where to fetch signed checkpoints.
    Announce {
        /// Storage location URL (e.g., "s3://bucket-name/cardano-validator")
        #[arg(long)]
        storage_location: String,

        /// Path to signing key (if not using CARDANO_SIGNING_KEY env)
        #[arg(long)]
        signing_key: Option<String>,

        /// Dry run - show what would be done without submitting
        #[arg(long)]
        dry_run: bool,
    },

    /// Show validator announcements
    Show {
        /// Filter by validator pubkey (32 bytes hex)
        #[arg(long)]
        validator: Option<String>,
    },
}

pub async fn execute(ctx: &CliContext, args: ValidatorArgs) -> Result<()> {
    match args.command {
        ValidatorCommands::Announce {
            storage_location,
            signing_key,
            dry_run,
        } => announce_validator(ctx, &storage_location, signing_key, dry_run).await,
        ValidatorCommands::Show { validator } => show_announcements(ctx, validator).await,
    }
}

/// Announce validator storage location
async fn announce_validator(
    ctx: &CliContext,
    storage_location: &str,
    signing_key: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Announcing validator storage location...".cyan());
    println!("  Storage: {}", storage_location);

    if storage_location.is_empty() {
        return Err(anyhow!("Storage location cannot be empty"));
    }

    // Load signing key
    let keypair = if let Some(path) = signing_key {
        ctx.load_signing_key_from(std::path::Path::new(&path))?
    } else {
        ctx.load_signing_key()?
    };

    let payer_address = keypair.address_bech32(ctx.pallas_network());
    let payer_pkh = keypair.pub_key_hash();
    println!("  Validator: {}", hex::encode(&payer_pkh));
    println!("  Address: {}", payer_address);

    // Get mailbox info from deployment
    let deployment = ctx.load_deployment_info()?;
    let mailbox = deployment
        .mailbox
        .as_ref()
        .ok_or_else(|| anyhow!("Mailbox not deployed. Run 'init mailbox' first."))?;

    let mailbox_policy_id = mailbox
        .state_nft_policy
        .as_ref()
        .ok_or_else(|| anyhow!("Mailbox state NFT policy not found"))?;

    // Get local domain from mailbox UTXO datum
    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let mailbox_utxo = client
        .find_utxo_by_asset(mailbox_policy_id, "")
        .await?
        .ok_or_else(|| anyhow!("Mailbox UTXO not found"))?;

    let local_domain = parse_mailbox_domain(&mailbox_utxo.inline_datum)?;
    println!("  Mailbox Policy: {}", mailbox_policy_id);
    println!("  Local Domain: {}", local_domain);

    // Parametrize the validator_announce script
    let contracts_dir = ctx.contracts_dir.clone();

    // Encode parameters for validator_announce:
    // 1. mailbox_policy_id (28 bytes as ByteArray)
    // 2. mailbox_domain (Int)
    let policy_id_cbor = encode_policy_id_param(mailbox_policy_id)?;
    let domain_cbor = encode_int_param(local_domain);

    println!("\n{}", "Applying validator parameters...".cyan());
    let applied = apply_validator_params(
        &contracts_dir,
        "validator_announce",
        "validator_announce",
        &[&hex::encode(&policy_id_cbor), &hex::encode(&domain_cbor)],
    )?;

    let va_script_hash = &applied.policy_id;
    let va_address = script_hash_to_address(va_script_hash, ctx.pallas_network())?;
    println!("  Script Hash: {}", va_script_hash);
    println!("  Script Address: {}", va_address);

    // Build validator pubkey (32 bytes): 4 zero bytes + 28 byte verification key hash
    let mut validator_pubkey = vec![0u8; 4];
    validator_pubkey.extend_from_slice(&payer_pkh);
    println!("  Validator Pubkey: {}", hex::encode(&validator_pubkey));

    // Check for existing announcement from this validator
    let existing_announcement: Option<Utxo> = find_existing_announcement(
        &client,
        &va_address,
        &validator_pubkey,
    ).await?;

    // Check for bare UTXO at script address (for new announcements)
    let bare_utxo: Option<Utxo> = if existing_announcement.is_none() {
        find_bare_utxo(&client, &va_address).await?
    } else {
        None
    };

    // Build ValidatorAnnounceDatum
    // { validator_pubkey, mailbox_policy_id, mailbox_domain, storage_location }
    let datum_cbor = build_validator_announce_datum(
        &validator_pubkey,
        mailbox_policy_id,
        local_domain,
        storage_location,
    )?;
    println!("\n{}", "ValidatorAnnounceDatum:".green());
    println!("  CBOR: {}", hex::encode(&datum_cbor));

    // Build Announce redeemer
    // Announce { storage_location: ByteArray } = Constr 0 [storage_location]
    let redeemer_cbor = build_announce_redeemer(storage_location)?;
    println!("\n{}", "Announce Redeemer:".green());
    println!("  CBOR: {}", hex::encode(&redeemer_cbor));

    // Determine which UTXO to spend
    let (script_utxo, is_update) = match (&existing_announcement, &bare_utxo) {
        (Some(existing), _) => {
            println!("\n{}", "Updating existing announcement...".yellow());
            (existing.clone(), true)
        }
        (None, Some(bare)) => {
            println!("\n{}", "Creating new announcement...".green());
            (bare.clone(), false)
        }
        (None, None) => {
            // Need to create a seed UTXO first
            println!("\n{}", "No spendable UTXO found at validator announce address.".yellow());
            println!("Creating seed UTXO for new announcement...");

            if dry_run {
                println!("\n{}", "[Dry run - would create seed UTXO, then announce]".yellow());
                return Ok(());
            }

            // Create seed UTXO transaction
            let seed_tx_hash = create_seed_utxo(ctx, &client, &keypair, &va_address).await?;
            println!("  Seed TX: {}", seed_tx_hash);
            println!("  Waiting for confirmation...");

            // Wait for confirmation
            tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;

            // Find the newly created UTXO
            let new_bare = find_bare_utxo(&client, &va_address).await?
                .ok_or_else(|| anyhow!("Seed UTXO not found after creation"))?;

            (new_bare, false)
        }
    };

    println!("\n{}", "Script UTXO to spend:".green());
    println!("  TX: {}#{}", script_utxo.tx_hash, script_utxo.output_index);
    println!("  Lovelace: {}", script_utxo.lovelace);

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        println!("\nTo announce validator, build a transaction that:");
        println!("1. Spends: {}#{}", script_utxo.tx_hash, script_utxo.output_index);
        println!("2. Uses Announce redeemer: {}", hex::encode(&redeemer_cbor));
        println!("3. Creates UTXO at {} with datum", va_address);
        println!("4. Requires validator signature: {}", hex::encode(&payer_pkh));
        return Ok(());
    }

    // Build and submit the transaction
    println!("\n{}", "Building transaction...".cyan());

    // Get payer UTXOs for fees and collateral
    let payer_utxos = client.get_utxos(&payer_address).await?;
    if payer_utxos.is_empty() {
        return Err(anyhow!("No UTXOs found for payer address"));
    }

    // Find collateral UTXO (pure ADA, no tokens)
    let collateral_utxo = payer_utxos
        .iter()
        .find(|u| u.lovelace >= 5_000_000 && u.assets.is_empty())
        .ok_or_else(|| anyhow!("No suitable collateral UTXO (need 5+ ADA without tokens)"))?;

    // Find fee UTXO
    let fee_utxo = payer_utxos
        .iter()
        .find(|u| {
            u.lovelace >= 5_000_000 &&
            u.assets.is_empty() &&
            (u.tx_hash != collateral_utxo.tx_hash || u.output_index != collateral_utxo.output_index)
        })
        .unwrap_or(collateral_utxo);

    println!("  Collateral: {}#{}", collateral_utxo.tx_hash, collateral_utxo.output_index);
    println!("  Fee input: {}#{}", fee_utxo.tx_hash, fee_utxo.output_index);

    // Get PlutusV3 cost model
    let cost_model = client.get_plutusv3_cost_model().await?;

    // Get current slot for validity
    let current_slot = client.get_latest_slot().await?;
    let validity_end = current_slot + 7200; // ~2 hours

    // Parse addresses and hashes
    let va_addr = pallas_addresses::Address::from_bech32(&va_address)
        .map_err(|e| anyhow!("Invalid VA address: {:?}", e))?;
    let payer_addr = pallas_addresses::Address::from_bech32(&payer_address)
        .map_err(|e| anyhow!("Invalid payer address: {:?}", e))?;

    let script_tx_hash: [u8; 32] = hex::decode(&script_utxo.tx_hash)?
        .try_into().map_err(|_| anyhow!("Invalid script tx hash"))?;
    let collateral_tx_hash: [u8; 32] = hex::decode(&collateral_utxo.tx_hash)?
        .try_into().map_err(|_| anyhow!("Invalid collateral tx hash"))?;
    let fee_tx_hash: [u8; 32] = hex::decode(&fee_utxo.tx_hash)?
        .try_into().map_err(|_| anyhow!("Invalid fee tx hash"))?;
    let signer_hash: [u8; 28] = payer_pkh.clone()
        .try_into().map_err(|_| anyhow!("Invalid signer hash"))?;

    // Output lovelace (minimum for datum UTXO)
    let output_lovelace = std::cmp::max(script_utxo.lovelace, 2_000_000);

    // Build continuation output with datum
    let va_output = Output::new(va_addr, output_lovelace)
        .set_inline_datum(datum_cbor.clone());

    // Calculate change (script execution requires higher fee)
    let fee_estimate = 1_000_000u64;
    let change = fee_utxo.lovelace.saturating_sub(fee_estimate);

    // Get the script bytes
    let script_bytes = hex::decode(&applied.compiled_code)?;

    // Build staging transaction
    let mut staging = StagingTransaction::new()
        // Script input
        .input(Input::new(Hash::new(script_tx_hash), script_utxo.output_index as u64))
        // Fee input
        .input(Input::new(Hash::new(fee_tx_hash), fee_utxo.output_index as u64))
        // Collateral
        .collateral_input(Input::new(Hash::new(collateral_tx_hash), collateral_utxo.output_index as u64))
        // Continuation output
        .output(va_output)
        // Spend redeemer
        .add_spend_redeemer(
            Input::new(Hash::new(script_tx_hash), script_utxo.output_index as u64),
            redeemer_cbor.clone(),
            Some(pallas_txbuilder::ExUnits { mem: 5_000_000, steps: 2_000_000_000 }),
        )
        // Script (embedded since not deployed as reference script)
        .script(ScriptKind::PlutusV3, script_bytes)
        // Cost model for script data hash
        .language_view(ScriptKind::PlutusV3, cost_model)
        // Required signer
        .disclosed_signer(Hash::new(signer_hash))
        // Fee and validity
        .fee(fee_estimate)
        .invalid_from_slot(validity_end)
        .network_id(0); // Testnet

    // Add change output
    if change >= 1_000_000 {
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
    println!("\n  Validator: {}", hex::encode(&validator_pubkey));
    println!("  Storage Location: {}", storage_location);
    println!("  Action: {}", if is_update { "Updated" } else { "Created" });

    Ok(())
}

/// Show validator announcements
async fn show_announcements(ctx: &CliContext, validator_filter: Option<String>) -> Result<()> {
    println!("{}", "Validator Announcements".cyan());

    // Get mailbox info from deployment
    let deployment = ctx.load_deployment_info()?;
    let mailbox = deployment
        .mailbox
        .as_ref()
        .ok_or_else(|| anyhow!("Mailbox not deployed"))?;

    let mailbox_policy_id = mailbox
        .state_nft_policy
        .as_ref()
        .ok_or_else(|| anyhow!("Mailbox state NFT policy not found"))?;

    // Get local domain
    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let mailbox_utxo = client
        .find_utxo_by_asset(mailbox_policy_id, "")
        .await?
        .ok_or_else(|| anyhow!("Mailbox UTXO not found"))?;

    let local_domain = parse_mailbox_domain(&mailbox_utxo.inline_datum)?;

    // Parametrize validator_announce to get the address
    let contracts_dir = ctx.contracts_dir.clone();
    let policy_id_cbor = encode_policy_id_param(mailbox_policy_id)?;
    let domain_cbor = encode_int_param(local_domain);

    let applied = apply_validator_params(
        &contracts_dir,
        "validator_announce",
        "validator_announce",
        &[&hex::encode(&policy_id_cbor), &hex::encode(&domain_cbor)],
    )?;

    let va_address = script_hash_to_address(&applied.policy_id, ctx.pallas_network())?;
    println!("  Script Address: {}", va_address);
    println!("  Mailbox Domain: {}", local_domain);

    // Get all UTXOs at the validator announce address
    let utxos = client.get_utxos(&va_address).await?;

    if utxos.is_empty() {
        println!("\n{}", "No announcements found.".yellow());
        return Ok(());
    }

    println!("\n{}", "Announcements:".green());
    println!("{}", "-".repeat(80));

    let mut count = 0;
    for utxo in utxos {
        if let Some(ref datum_val) = utxo.inline_datum {
            if let Some((validator, storage_location)) = parse_announcement_datum(datum_val)? {
                // Apply filter if specified
                if let Some(ref filter) = validator_filter {
                    let filter_normalized = filter.strip_prefix("0x").unwrap_or(filter).to_lowercase();
                    if !hex::encode(&validator).contains(&filter_normalized) {
                        continue;
                    }
                }

                count += 1;
                println!("\n  Validator: 0x{}", hex::encode(&validator));
                println!("  Storage: {}", storage_location);
                println!("  UTXO: {}#{}", utxo.tx_hash, utxo.output_index);
                println!("  Lovelace: {}", utxo.lovelace);
            }
        }
    }

    if count == 0 {
        println!("\n{}", "No matching announcements found.".yellow());
    } else {
        println!("\n  Total: {} announcement(s)", count);
    }

    Ok(())
}

// ============================================================================
// Helper functions
// ============================================================================

/// Parse local domain from mailbox datum
fn parse_mailbox_domain(datum: &Option<serde_json::Value>) -> Result<u32> {
    let datum = datum.as_ref().ok_or_else(|| anyhow!("Mailbox has no inline datum"))?;

    // Handle CBOR hex string format
    if let Some(hex_str) = datum.as_str() {
        let cbor_bytes = hex::decode(hex_str)?;
        // Parse CBOR to get first field (local_domain)
        use pallas_primitives::conway::{PlutusData, BigInt};
        let parsed: PlutusData = pallas_codec::minicbor::decode(&cbor_bytes)
            .map_err(|e| anyhow!("Failed to decode mailbox datum: {:?}", e))?;

        if let PlutusData::Constr(c) = parsed {
            let fields: Vec<&PlutusData> = c.fields.iter().collect();
            if let Some(PlutusData::BigInt(BigInt::Int(i))) = fields.first() {
                return Ok(i64::try_from(i.0)? as u32);
            }
        }
        return Err(anyhow!("Could not parse domain from CBOR datum"));
    }

    // Handle JSON format
    let fields = datum
        .get("fields")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow!("Invalid mailbox datum"))?;

    fields
        .first()
        .and_then(|d| d.get("int"))
        .and_then(|i| i.as_u64())
        .map(|d| d as u32)
        .ok_or_else(|| anyhow!("Could not parse domain from mailbox datum"))
}

/// Encode policy ID (28 bytes) as CBOR parameter
fn encode_policy_id_param(policy_id_hex: &str) -> Result<Vec<u8>> {
    let hash_bytes = hex::decode(policy_id_hex)
        .map_err(|e| anyhow!("Invalid policy ID hex: {}", e))?;

    if hash_bytes.len() != 28 {
        return Err(anyhow!("Policy ID must be 28 bytes, got {}", hash_bytes.len()));
    }

    // CBOR encoding: 581c (28-byte bytestring prefix) + bytes
    let mut cbor = vec![0x58, 0x1c];
    cbor.extend_from_slice(&hash_bytes);
    Ok(cbor)
}

/// Encode integer as CBOR parameter
fn encode_int_param(n: u32) -> Vec<u8> {
    let mut builder = CborBuilder::new();
    builder.uint(n as u64);
    builder.build()
}

/// Build ValidatorAnnounceDatum CBOR
/// Structure: Constr 0 [validator_pubkey, mailbox_policy_id, mailbox_domain, storage_location]
fn build_validator_announce_datum(
    validator_pubkey: &[u8],
    mailbox_policy_id: &str,
    mailbox_domain: u32,
    storage_location: &str,
) -> Result<Vec<u8>> {
    let mut builder = CborBuilder::new();
    builder.start_constr(0);

    // validator_pubkey (32 bytes)
    builder.bytes_hex(&hex::encode(validator_pubkey))?;

    // mailbox_policy_id (28 bytes)
    builder.bytes_hex(mailbox_policy_id)?;

    // mailbox_domain (Int)
    builder.uint(mailbox_domain as u64);

    // storage_location (bytes)
    builder.bytes_hex(&hex::encode(storage_location.as_bytes()))?;

    builder.end_constr();
    Ok(builder.build())
}

/// Build Announce redeemer CBOR
/// Structure: Constr 0 [storage_location]
fn build_announce_redeemer(storage_location: &str) -> Result<Vec<u8>> {
    let mut builder = CborBuilder::new();
    builder.start_constr(0); // Announce = Constr 0
    builder.bytes_hex(&hex::encode(storage_location.as_bytes()))?;
    builder.end_constr();
    Ok(builder.build())
}

/// Find existing announcement from a specific validator
async fn find_existing_announcement(
    client: &BlockfrostClient,
    address: &str,
    validator_pubkey: &[u8],
) -> Result<Option<Utxo>> {
    let utxos = client.get_utxos(address).await?;

    for utxo in utxos {
        if let Some(ref datum_val) = utxo.inline_datum {
            if let Some((validator, _)) = parse_announcement_datum(datum_val)? {
                if validator == validator_pubkey {
                    return Ok(Some(utxo));
                }
            }
        }
    }

    Ok(None)
}

/// Find bare UTXO (no datum) at script address
async fn find_bare_utxo(
    client: &BlockfrostClient,
    address: &str,
) -> Result<Option<Utxo>> {
    let utxos = client.get_utxos(address).await?;

    for utxo in utxos {
        if utxo.inline_datum.is_none() && utxo.lovelace >= 2_000_000 {
            return Ok(Some(utxo));
        }
    }

    Ok(None)
}

/// Parse announcement datum to extract validator and storage_location
fn parse_announcement_datum(datum_val: &serde_json::Value) -> Result<Option<(Vec<u8>, String)>> {
    // Try CBOR hex format first (Blockfrost returns datum as hex string in Value)
    if let Some(hex_str) = datum_val.as_str() {
        if let Ok(cbor_bytes) = hex::decode(hex_str) {
            use pallas_primitives::conway::PlutusData;
            if let Ok(parsed) = pallas_codec::minicbor::decode::<PlutusData>(&cbor_bytes) {
                if let PlutusData::Constr(c) = parsed {
                    let fields: Vec<&PlutusData> = c.fields.iter().collect();
                    if fields.len() >= 4 {
                        if let (PlutusData::BoundedBytes(validator), PlutusData::BoundedBytes(storage)) =
                            (fields[0], fields[3])
                        {
                            let storage_bytes: &[u8] = storage.as_ref();
                            let storage_str = String::from_utf8(storage_bytes.to_vec())
                                .unwrap_or_else(|_| hex::encode(storage_bytes));
                            return Ok(Some((validator.to_vec(), storage_str)));
                        }
                    }
                }
            }
        }
    }

    // Try JSON object format
    if let Some(fields) = datum_val.get("fields").and_then(|f| f.as_array()) {
        if fields.len() >= 4 {
            let validator_hex = fields[0].get("bytes").and_then(|b| b.as_str());
            let storage_hex = fields[3].get("bytes").and_then(|b| b.as_str());

            if let (Some(v_hex), Some(s_hex)) = (validator_hex, storage_hex) {
                let validator = hex::decode(v_hex)?;
                let storage_bytes = hex::decode(s_hex)?;
                let storage = String::from_utf8(storage_bytes)
                    .unwrap_or_else(|_| s_hex.to_string());
                return Ok(Some((validator, storage)));
            }
        }
    }

    Ok(None)
}

/// Create a seed UTXO at the script address for new announcements
async fn create_seed_utxo(
    ctx: &CliContext,
    client: &BlockfrostClient,
    keypair: &Keypair,
    script_address: &str,
) -> Result<String> {
    let payer_address = keypair.address_bech32(ctx.pallas_network());

    // Get payer UTXOs
    let payer_utxos = client.get_utxos(&payer_address).await?;
    let fee_utxo = payer_utxos
        .iter()
        .find(|u| u.lovelace >= 5_000_000 && u.assets.is_empty())
        .ok_or_else(|| anyhow!("No suitable UTXO for seed transaction"))?;

    // Parse addresses
    let script_addr = pallas_addresses::Address::from_bech32(script_address)
        .map_err(|e| anyhow!("Invalid script address: {:?}", e))?;
    let payer_addr = pallas_addresses::Address::from_bech32(&payer_address)
        .map_err(|e| anyhow!("Invalid payer address: {:?}", e))?;

    let fee_tx_hash: [u8; 32] = hex::decode(&fee_utxo.tx_hash)?
        .try_into().map_err(|_| anyhow!("Invalid fee tx hash"))?;

    // Build simple transaction: send 2 ADA to script address
    let seed_amount = 2_000_000u64;
    let fee_estimate = 200_000u64;
    let change = fee_utxo.lovelace.saturating_sub(seed_amount).saturating_sub(fee_estimate);

    let current_slot = client.get_latest_slot().await?;
    let validity_end = current_slot + 7200;

    let mut staging = StagingTransaction::new()
        .input(Input::new(Hash::new(fee_tx_hash), fee_utxo.output_index as u64))
        .output(Output::new(script_addr, seed_amount))
        .fee(fee_estimate)
        .invalid_from_slot(validity_end)
        .network_id(0);

    if change >= 1_000_000 {
        staging = staging.output(Output::new(payer_addr, change));
    }

    let tx = staging.build_conway_raw()
        .map_err(|e| anyhow!("Failed to build seed transaction: {:?}", e))?;

    // Sign and submit
    let signature = keypair.sign(&tx.tx_hash.0);
    let signed_tx = tx.add_signature(keypair.pallas_public_key().clone(), signature)
        .map_err(|e| anyhow!("Failed to sign seed transaction: {:?}", e))?;

    let tx_hash = client.submit_tx(&signed_tx.tx_bytes.0).await?;
    Ok(tx_hash)
}

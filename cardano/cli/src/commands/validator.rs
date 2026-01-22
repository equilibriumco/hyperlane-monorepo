//! Validator command - Manage validator announcements for Hyperlane
//!
//! Implements Hyperlane-compatible validator announcements using ECDSA secp256k1 signatures.
//! This ensures cross-chain interoperability - the validator's Ethereum address is used as
//! their identity across all chains.

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};
use colored::Colorize;
use k256::ecdsa::{SigningKey, Signature, signature::hazmat::PrehashSigner};
use pallas_crypto::hash::Hash;
use pallas_txbuilder::{BuildConway, Input, Output, ScriptKind, StagingTransaction};
use tiny_keccak::{Hasher, Keccak};

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
    ///
    /// This command requires a secp256k1 private key (the same key used for
    /// signing Hyperlane checkpoints). The Ethereum address derived from this
    /// key will be stored as the validator's identity.
    Announce {
        /// Storage location URL (e.g., "s3://bucket-name/cardano-validator")
        #[arg(long)]
        storage_location: String,

        /// Validator's secp256k1 private key (hex, 32 bytes)
        /// This is the key used for signing Hyperlane checkpoints.
        /// Can also be set via HYPERLANE_VALIDATOR_KEY env var.
        #[arg(long, env = "HYPERLANE_VALIDATOR_KEY")]
        validator_key: String,

        /// Path to Cardano signing key for paying transaction fees
        /// (if not using CARDANO_SIGNING_KEY env)
        #[arg(long)]
        signing_key: Option<String>,

        /// Dry run - show what would be done without submitting
        #[arg(long)]
        dry_run: bool,
    },

    /// Show validator announcements
    Show {
        /// Filter by validator address (20 bytes hex, Ethereum address)
        #[arg(long)]
        validator: Option<String>,
    },
}

pub async fn execute(ctx: &CliContext, args: ValidatorArgs) -> Result<()> {
    match args.command {
        ValidatorCommands::Announce {
            storage_location,
            validator_key,
            signing_key,
            dry_run,
        } => announce_validator(ctx, &storage_location, &validator_key, signing_key, dry_run).await,
        ValidatorCommands::Show { validator } => show_announcements(ctx, validator).await,
    }
}

/// Announce validator storage location using ECDSA secp256k1 signature
async fn announce_validator(
    ctx: &CliContext,
    storage_location: &str,
    validator_key_hex: &str,
    signing_key: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Announcing validator storage location...".cyan());
    println!("  Storage: {}", storage_location);

    if storage_location.is_empty() {
        return Err(anyhow!("Storage location cannot be empty"));
    }

    // Parse the secp256k1 validator key
    let validator_key_hex = validator_key_hex.strip_prefix("0x").unwrap_or(validator_key_hex);
    let validator_key_bytes = hex::decode(validator_key_hex)
        .map_err(|e| anyhow!("Invalid validator key hex: {}", e))?;

    if validator_key_bytes.len() != 32 {
        return Err(anyhow!("Validator key must be 32 bytes, got {}", validator_key_bytes.len()));
    }

    let signing_key_secp = SigningKey::from_slice(&validator_key_bytes)
        .map_err(|e| anyhow!("Invalid secp256k1 private key: {}", e))?;

    // Derive public keys and Ethereum address
    let verifying_key = signing_key_secp.verifying_key();
    let pubkey_uncompressed = verifying_key.to_encoded_point(false);
    let pubkey_compressed = verifying_key.to_encoded_point(true);

    // Uncompressed without prefix (64 bytes: x || y)
    let uncompressed_bytes: Vec<u8> = pubkey_uncompressed.as_bytes()[1..].to_vec();
    // Compressed with prefix (33 bytes: 0x02/0x03 || x)
    let compressed_bytes: Vec<u8> = pubkey_compressed.as_bytes().to_vec();

    // Derive Ethereum address: keccak256(uncompressed_pubkey)[12:32]
    let eth_address = pubkey_to_eth_address(&uncompressed_bytes);
    println!("  Validator Address: 0x{}", hex::encode(&eth_address));

    // Load Cardano signing key for paying transaction fees
    let cardano_keypair = if let Some(path) = signing_key {
        ctx.load_signing_key_from(std::path::Path::new(&path))?
    } else {
        ctx.load_signing_key()?
    };

    let payer_address = cardano_keypair.address_bech32(ctx.pallas_network());
    println!("  Payer Address: {}", payer_address);

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

    // Compute the announcement digest (matching contract's formula)
    let announcement_digest = compute_announcement_digest(
        mailbox_policy_id,
        local_domain,
        storage_location,
    )?;
    println!("\n{}", "Announcement Digest:".green());
    println!("  Hash: 0x{}", hex::encode(&announcement_digest));

    // Sign the digest with secp256k1 (EIP-191 style already included in digest)
    // Use sign_prehash since the digest is already a hash - don't hash again!
    let signature: Signature = signing_key_secp.sign_prehash(&announcement_digest)
        .map_err(|e| anyhow!("Failed to sign announcement: {:?}", e))?;
    let signature_bytes = signature.to_bytes().to_vec();
    println!("  Signature: 0x{}", hex::encode(&signature_bytes));

    // Check for existing announcement from this validator
    let existing_announcement: Option<Utxo> = find_existing_announcement(
        &client,
        &va_address,
        &eth_address,
    ).await?;

    // Check for bare UTXO at script address (for new announcements)
    let bare_utxo: Option<Utxo> = if existing_announcement.is_none() {
        find_bare_utxo(&client, &va_address).await?
    } else {
        None
    };

    // Build ValidatorAnnounceDatum
    // { validator_address (20 bytes), mailbox_policy_id, mailbox_domain, storage_location }
    let datum_cbor = build_validator_announce_datum(
        &eth_address,
        mailbox_policy_id,
        local_domain,
        storage_location,
    )?;
    println!("\n{}", "ValidatorAnnounceDatum:".green());
    println!("  CBOR: {}", hex::encode(&datum_cbor));

    // Build Announce redeemer with signature
    // { storage_location, compressed_pubkey, uncompressed_pubkey, signature }
    let redeemer_cbor = build_announce_redeemer(
        storage_location,
        &compressed_bytes,
        &uncompressed_bytes,
        &signature_bytes,
    )?;
    println!("\n{}", "Announce Redeemer:".green());
    println!("  CBOR: {}", hex::encode(&redeemer_cbor));

    // Track spent UTXOs to exclude from later queries (Blockfrost may have stale data)
    let mut spent_utxos: Vec<(String, u64)> = Vec::new();

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
            let seed_result = create_seed_utxo(ctx, &client, &cardano_keypair, &va_address).await?;
            println!("  Seed TX: {}", seed_result.tx_hash);
            println!("  Waiting for confirmation...");

            // Track the spent UTXO so we don't try to reuse it
            spent_utxos.push((seed_result.spent_utxo_hash.clone(), seed_result.spent_utxo_index));

            // Wait for confirmation with retry
            let mut retries = 0;
            let max_retries = 12; // 12 * 5s = 60s max wait
            let new_bare = loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                if let Some(utxo) = find_bare_utxo(&client, &va_address).await? {
                    break utxo;
                }
                retries += 1;
                if retries >= max_retries {
                    return Err(anyhow!("Seed UTXO not found after {}s. TX: {}. Please retry.",
                        retries * 5, seed_result.tx_hash));
                }
                print!(".");
                std::io::Write::flush(&mut std::io::stdout())?;
            };
            println!(); // newline after dots

            (new_bare, false)
        }
    };

    println!("\n{}", "Script UTXO to spend:".green());
    println!("  TX: {}#{}", script_utxo.tx_hash, script_utxo.output_index);
    println!("  Lovelace: {}", script_utxo.lovelace);

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        return Ok(());
    }

    // Build and submit the transaction
    println!("\n{}", "Building transaction...".cyan());

    // Get payer UTXOs for fees and collateral, excluding recently spent ones
    let all_payer_utxos = client.get_utxos(&payer_address).await?;
    let payer_utxos: Vec<_> = all_payer_utxos
        .into_iter()
        .filter(|u| !spent_utxos.iter().any(|(hash, idx)| &u.tx_hash == hash && u.output_index == *idx as u32))
        .collect();
    if payer_utxos.is_empty() {
        return Err(anyhow!("No UTXOs found for payer address"));
    }

    // Find collateral UTXO (pure ADA, no tokens, no reference script)
    let collateral_utxo = payer_utxos
        .iter()
        .find(|u| u.lovelace >= 5_000_000 && u.assets.is_empty() && u.reference_script.is_none())
        .ok_or_else(|| anyhow!("No suitable collateral UTXO (need 5+ ADA without tokens or reference scripts)"))?;

    // Find fee UTXO (must not have reference script)
    let fee_utxo = payer_utxos
        .iter()
        .find(|u| {
            u.lovelace >= 5_000_000 &&
            u.assets.is_empty() &&
            u.reference_script.is_none() &&
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

    // Output lovelace (minimum for datum UTXO)
    let output_lovelace = std::cmp::max(script_utxo.lovelace, 2_000_000);

    // Build continuation output with datum
    let va_output = Output::new(va_addr, output_lovelace)
        .set_inline_datum(datum_cbor.clone());

    // Calculate change (script execution requires higher fee for ECDSA verification)
    let fee_estimate = 2_000_000u64;
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
        // Spend redeemer (with higher execution units for ECDSA)
        .add_spend_redeemer(
            Input::new(Hash::new(script_tx_hash), script_utxo.output_index as u64),
            redeemer_cbor.clone(),
            Some(pallas_txbuilder::ExUnits { mem: 14_000_000, steps: 10_000_000_000 }),
        )
        // Script (embedded since not deployed as reference script)
        .script(ScriptKind::PlutusV3, script_bytes)
        // Cost model for script data hash
        .language_view(ScriptKind::PlutusV3, cost_model)
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

    // Sign the transaction with Cardano key
    println!("{}", "Signing transaction...".cyan());
    let tx_hash_bytes: &[u8] = &tx.tx_hash.0;
    let signature = cardano_keypair.sign(tx_hash_bytes);
    let signed_tx = tx.add_signature(cardano_keypair.pallas_public_key().clone(), signature)
        .map_err(|e| anyhow!("Failed to sign transaction: {:?}", e))?;

    // Submit the transaction
    println!("{}", "Submitting transaction...".cyan());
    let tx_hash = client.submit_tx(&signed_tx.tx_bytes.0).await?;

    println!("\n{}", "SUCCESS!".green().bold());
    println!("  Transaction Hash: {}", tx_hash);
    println!("  Explorer: {}", ctx.explorer_tx_url(&tx_hash));
    println!("\n  Validator Address: 0x{}", hex::encode(&eth_address));
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
            if let Some((validator_addr, storage_location)) = parse_announcement_datum(datum_val)? {
                // Apply filter if specified
                if let Some(ref filter) = validator_filter {
                    let filter_normalized = filter.strip_prefix("0x").unwrap_or(filter).to_lowercase();
                    if !hex::encode(&validator_addr).contains(&filter_normalized) {
                        continue;
                    }
                }

                count += 1;
                println!("\n  Validator: 0x{}", hex::encode(&validator_addr));
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
// Crypto Helper Functions
// ============================================================================

/// Compute keccak256 hash
fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak::v256();
    let mut output = [0u8; 32];
    hasher.update(data);
    hasher.finalize(&mut output);
    output
}

/// Convert uncompressed secp256k1 public key (64 bytes) to Ethereum address (20 bytes)
fn pubkey_to_eth_address(uncompressed_pubkey: &[u8]) -> Vec<u8> {
    assert_eq!(uncompressed_pubkey.len(), 64, "Expected 64-byte uncompressed pubkey");
    let hash = keccak256(uncompressed_pubkey);
    hash[12..32].to_vec()
}

/// Compute the announcement digest that validators sign
/// Matches the Hyperlane EVM ValidatorAnnounce format:
/// 1. domain_hash = keccak256(localDomain || mailboxAddress || "HYPERLANE_ANNOUNCEMENT")
/// 2. inner_hash = keccak256(domain_hash || storage_location)
/// 3. announcement_digest = keccak256("\x19Ethereum Signed Message:\n32" || inner_hash)
fn compute_announcement_digest(
    mailbox_policy_id: &str,
    mailbox_domain: u32,
    storage_location: &str,
) -> Result<[u8; 32]> {
    // Step 1: domain_hash
    // Domain as 4-byte big-endian
    let domain_bytes = mailbox_domain.to_be_bytes();

    // Mailbox address as 32 bytes (policy ID padded with zeros on right)
    let policy_bytes = hex::decode(mailbox_policy_id)?;
    let mut mailbox_address = [0u8; 32];
    mailbox_address[..policy_bytes.len()].copy_from_slice(&policy_bytes);

    // "HYPERLANE_ANNOUNCEMENT" as bytes
    let announcement_string = b"HYPERLANE_ANNOUNCEMENT";

    let mut domain_hash_input = Vec::new();
    domain_hash_input.extend_from_slice(&domain_bytes);
    domain_hash_input.extend_from_slice(&mailbox_address);
    domain_hash_input.extend_from_slice(announcement_string);

    let domain_hash = keccak256(&domain_hash_input);

    // Step 2: inner_hash = keccak256(domain_hash || storage_location)
    let mut inner_hash_input = Vec::new();
    inner_hash_input.extend_from_slice(&domain_hash);
    inner_hash_input.extend_from_slice(storage_location.as_bytes());

    let inner_hash = keccak256(&inner_hash_input);

    // Step 3: EIP-191 signed message hash
    // Prefix: "\x19Ethereum Signed Message:\n32"
    let eip191_prefix = b"\x19Ethereum Signed Message:\n32";

    let mut prefixed = Vec::new();
    prefixed.extend_from_slice(eip191_prefix);
    prefixed.extend_from_slice(&inner_hash);

    Ok(keccak256(&prefixed))
}

// ============================================================================
// CBOR Building Helper Functions
// ============================================================================

/// Parse local domain from mailbox datum
fn parse_mailbox_domain(datum: &Option<serde_json::Value>) -> Result<u32> {
    let datum = datum.as_ref().ok_or_else(|| anyhow!("Mailbox has no inline datum"))?;

    // Handle CBOR hex string format
    if let Some(hex_str) = datum.as_str() {
        let cbor_bytes = hex::decode(hex_str)?;
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
/// Structure: Constr 0 [validator_address (20 bytes), mailbox_policy_id, mailbox_domain, storage_location]
/// Note: Ethereum addresses are stored as 20 bytes (matching the Aiken contract expectation)
fn build_validator_announce_datum(
    validator_address: &[u8],
    mailbox_policy_id: &str,
    mailbox_domain: u32,
    storage_location: &str,
) -> Result<Vec<u8>> {
    let mut builder = CborBuilder::new();
    builder.start_constr(0);

    // validator_address (20 bytes - Ethereum address)
    // The Aiken contract expects exactly 20 bytes
    let eth_address = if validator_address.len() == 20 {
        validator_address.to_vec()
    } else if validator_address.len() == 32 {
        // Extract last 20 bytes if given a 32-byte H256
        validator_address[12..32].to_vec()
    } else {
        return Err(anyhow!("Invalid validator address length: {}", validator_address.len()));
    };
    builder.bytes_hex(&hex::encode(&eth_address))?;

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
/// Structure: Constr 0 [storage_location, compressed_pubkey, uncompressed_pubkey, signature]
fn build_announce_redeemer(
    storage_location: &str,
    compressed_pubkey: &[u8],
    uncompressed_pubkey: &[u8],
    signature: &[u8],
) -> Result<Vec<u8>> {
    let mut builder = CborBuilder::new();
    builder.start_constr(0); // Announce = Constr 0

    // storage_location
    builder.bytes_hex(&hex::encode(storage_location.as_bytes()))?;

    // compressed_pubkey (33 bytes)
    builder.bytes_hex(&hex::encode(compressed_pubkey))?;

    // uncompressed_pubkey (64 bytes)
    builder.bytes_hex(&hex::encode(uncompressed_pubkey))?;

    // signature (64 bytes)
    builder.bytes_hex(&hex::encode(signature))?;

    builder.end_constr();
    Ok(builder.build())
}

// ============================================================================
// UTXO Query Helper Functions
// ============================================================================

/// Find existing announcement from a specific validator (by Ethereum address)
async fn find_existing_announcement(
    client: &BlockfrostClient,
    address: &str,
    validator_address: &[u8],
) -> Result<Option<Utxo>> {
    let utxos = client.get_utxos(address).await?;

    for utxo in utxos {
        if let Some(ref datum_val) = utxo.inline_datum {
            if let Some((addr, _)) = parse_announcement_datum(datum_val)? {
                // The datum stores a 32-byte H256 (12 zero bytes + 20-byte Ethereum address)
                // Extract the last 20 bytes to compare with the Ethereum address
                let stored_eth_addr = if addr.len() == 32 {
                    &addr[12..32]
                } else {
                    &addr[..]
                };
                if stored_eth_addr == validator_address {
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

/// Parse announcement datum to extract validator address and storage_location
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

/// Info about a spent UTXO during seed creation
struct SeedCreationResult {
    tx_hash: String,
    spent_utxo_hash: String,
    spent_utxo_index: u64,
}

/// Create a seed UTXO at the script address for new announcements
async fn create_seed_utxo(
    ctx: &CliContext,
    client: &BlockfrostClient,
    keypair: &Keypair,
    script_address: &str,
) -> Result<SeedCreationResult> {
    let payer_address = keypair.address_bech32(ctx.pallas_network());

    // Get payer UTXOs (must not have reference script)
    let payer_utxos = client.get_utxos(&payer_address).await?;
    let fee_utxo = payer_utxos
        .iter()
        .find(|u| u.lovelace >= 5_000_000 && u.assets.is_empty() && u.reference_script.is_none())
        .ok_or_else(|| anyhow!("No suitable UTXO for seed transaction (need 5+ ADA without tokens or reference scripts)"))?;

    // Parse addresses
    let script_addr = pallas_addresses::Address::from_bech32(script_address)
        .map_err(|e| anyhow!("Invalid script address: {:?}", e))?;
    let payer_addr = pallas_addresses::Address::from_bech32(&payer_address)
        .map_err(|e| anyhow!("Invalid payer address: {:?}", e))?;

    let fee_tx_hash: [u8; 32] = hex::decode(&fee_utxo.tx_hash)?
        .try_into().map_err(|_| anyhow!("Invalid fee tx hash"))?;

    // Build simple transaction: send 2 ADA to script address
    let seed_amount = 2_000_000u64;
    let fee_estimate = 250_000u64; // Increased to cover varying tx sizes
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
    Ok(SeedCreationResult {
        tx_hash,
        spent_utxo_hash: fee_utxo.tx_hash.clone(),
        spent_utxo_index: fee_utxo.output_index as u64,
    })
}

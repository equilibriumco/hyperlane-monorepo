//! Mailbox command - Manage Hyperlane Mailbox contract

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};
use colored::Colorize;
use pallas_primitives::conway::{PlutusData, BigInt};
use sha3::{Digest, Keccak256};

use crate::utils::blockfrost::BlockfrostClient;
use crate::utils::cbor::{build_mailbox_datum, build_mailbox_set_default_ism_redeemer, build_mailbox_dispatch_redeemer};
use crate::utils::context::CliContext;

#[derive(Args)]
pub struct MailboxArgs {
    #[command(subcommand)]
    command: MailboxCommands,
}

#[derive(Subcommand)]
enum MailboxCommands {
    /// Dispatch a message to a remote chain
    Dispatch {
        /// Destination domain ID (e.g., 43113 for Avalanche Fuji)
        #[arg(long)]
        destination: u32,

        /// Recipient address on destination chain (32 bytes hex, with or without 0x prefix)
        #[arg(long)]
        recipient: String,

        /// Message body (string or hex with 0x prefix)
        #[arg(long)]
        body: String,

        /// Mailbox policy ID (for finding the mailbox UTXO)
        #[arg(long)]
        mailbox_policy: Option<String>,

        /// Reference script UTXO (format: txhash#index)
        #[arg(long)]
        reference_script: Option<String>,

        /// Path to signing key
        #[arg(long)]
        signing_key: Option<String>,

        /// Dry run - show message details without submitting
        #[arg(long)]
        dry_run: bool,
    },

    /// Set the default ISM for the mailbox
    SetDefaultIsm {
        /// New ISM script hash (28 bytes, hex)
        #[arg(long)]
        ism_hash: String,

        /// Mailbox policy ID (for finding the mailbox UTXO)
        #[arg(long)]
        mailbox_policy: Option<String>,

        /// Reference script UTXO (format: txhash#index) - script deployed on-chain
        #[arg(long)]
        reference_script: Option<String>,

        /// Path to signing key
        #[arg(long)]
        signing_key: Option<String>,

        /// Dry run
        #[arg(long)]
        dry_run: bool,
    },

    /// Show current mailbox configuration
    Show {
        /// Mailbox policy ID
        #[arg(long)]
        mailbox_policy: Option<String>,
    },
}

pub async fn execute(ctx: &CliContext, args: MailboxArgs) -> Result<()> {
    match args.command {
        MailboxCommands::Dispatch {
            destination,
            recipient,
            body,
            mailbox_policy,
            reference_script,
            signing_key,
            dry_run,
        } => dispatch(ctx, destination, &recipient, &body, mailbox_policy, reference_script, signing_key, dry_run).await,
        MailboxCommands::SetDefaultIsm {
            ism_hash,
            mailbox_policy,
            reference_script,
            signing_key,
            dry_run,
        } => set_default_ism(ctx, &ism_hash, mailbox_policy, reference_script, signing_key, dry_run).await,
        MailboxCommands::Show { mailbox_policy } => show_config(ctx, mailbox_policy).await,
    }
}

#[allow(clippy::too_many_arguments)]
async fn dispatch(
    ctx: &CliContext,
    destination: u32,
    recipient: &str,
    body: &str,
    mailbox_policy: Option<String>,
    reference_script: Option<String>,
    signing_key: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Dispatching Hyperlane message...".cyan());

    // Parse recipient address (32 bytes hex)
    // Automatically pad shorter addresses (e.g., 20-byte ETH, 28-byte Cardano) to 32 bytes
    let recipient_raw = recipient.strip_prefix("0x").unwrap_or(recipient);
    // Validate hex
    hex::decode(recipient_raw).map_err(|e| anyhow!("Invalid recipient hex: {}", e))?;
    if recipient_raw.len() > 64 {
        return Err(anyhow!(
            "Recipient too long: {} hex chars (max 64)",
            recipient_raw.len()
        ));
    }
    // Left-pad with zeros to 64 hex chars (32 bytes, standard Hyperlane H256 format)
    let recipient_hex = format!("{:0>64}", recipient_raw);

    // Parse body (string or hex with 0x prefix)
    let body_bytes = if body.starts_with("0x") {
        hex::decode(&body[2..]).map_err(|e| anyhow!("Invalid body hex: {}", e))?
    } else {
        body.as_bytes().to_vec()
    };
    let body_hex = hex::encode(&body_bytes);

    println!("  Destination: {}", destination);
    println!("  Recipient: 0x{}", recipient_hex);
    println!("  Body: {} ({} bytes)", if body.starts_with("0x") { body.to_string() } else { format!("\"{}\"", body) }, body_bytes.len());

    // Get mailbox policy ID
    let policy_id = get_mailbox_policy(ctx, mailbox_policy)?;
    println!("  Mailbox Policy: {}", policy_id);

    // Load signing key (optional for dry_run)
    let keypair = if dry_run {
        // Try to load signing key, but don't fail in dry_run mode
        if let Some(path) = signing_key {
            Some(ctx.load_signing_key_from(std::path::Path::new(&path))?)
        } else {
            ctx.load_signing_key().ok()
        }
    } else {
        // Require signing key for actual transactions
        let kp = if let Some(path) = signing_key {
            ctx.load_signing_key_from(std::path::Path::new(&path))?
        } else {
            ctx.load_signing_key()?
        };
        Some(kp)
    };

    // Use actual address or placeholder for dry_run
    let (payer_address, payer_pkh) = if let Some(ref kp) = keypair {
        (kp.address_bech32(ctx.pallas_network()), kp.pub_key_hash())
    } else {
        // Use placeholder for dry_run when no key available
        let placeholder_pkh = vec![0u8; 28]; // 28-byte placeholder
        ("(no signing key - dry run)".to_string(), placeholder_pkh)
    };
    println!("  Sender: {}", payer_address);

    // Find mailbox UTXO
    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    // Get asset name from deployment info
    let asset_name_hex = ctx
        .load_deployment_info()
        .ok()
        .and_then(|d| d.mailbox)
        .and_then(|m| m.state_nft)
        .map(|nft| nft.asset_name_hex)
        .unwrap_or_else(|| hex::encode("Mailbox State"));

    let mailbox_utxo = client
        .find_utxo_by_asset(&policy_id, &asset_name_hex)
        .await?
        .ok_or_else(|| anyhow!("Mailbox UTXO not found with policy {}", policy_id))?;

    println!("\n{}", "Found Mailbox UTXO:".green());
    println!("  TX: {}#{}", mailbox_utxo.tx_hash, mailbox_utxo.output_index);

    // Parse current datum
    let current_datum = mailbox_utxo
        .inline_datum
        .as_ref()
        .ok_or_else(|| anyhow!("Mailbox UTXO has no inline datum"))?;

    let mailbox_data = parse_mailbox_datum(current_datum)?;
    println!("\n{}", "Current Mailbox State:".green());
    println!("  Local Domain: {}", mailbox_data.local_domain);
    println!("  Outbound Nonce: {}", mailbox_data.outbound_nonce);
    println!("  Merkle Count: {}", mailbox_data.merkle_count);

    // Build sender address (32 bytes: 0x00000000 + pkh for verification key)
    let sender_hex = format!("00000000{}", hex::encode(&payer_pkh));
    println!("  Sender (Hyperlane): 0x{}", sender_hex);

    // Calculate message ID
    let message_id = compute_message_id(
        3, // version
        mailbox_data.outbound_nonce,
        mailbox_data.local_domain,
        &sender_hex,
        destination,
        &recipient_hex,
        &body_hex,
    )?;
    println!("\n{}", "Message Details:".green());
    println!("  Message ID: 0x{}", message_id);
    println!("  Nonce: {}", mailbox_data.outbound_nonce);

    // Calculate new merkle tree state
    let new_merkle = update_merkle_tree(&mailbox_data.merkle_branches, mailbox_data.merkle_count, &message_id)?;
    println!("  New Merkle Count: {}", new_merkle.count);

    // Build new datum with updated nonce and merkle tree
    let branches_refs: Vec<&str> = new_merkle.branches.iter().map(|s| s.as_str()).collect();
    let new_datum_cbor = build_mailbox_datum(
        mailbox_data.local_domain,
        &mailbox_data.default_ism,
        &mailbox_data.owner,
        mailbox_data.outbound_nonce + 1,
        &branches_refs,
        new_merkle.count,
    )?;

    // Build Dispatch redeemer
    let redeemer_cbor = build_mailbox_dispatch_redeemer(destination, &recipient_hex, &body_hex)?;

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        println!("\n{}", "Transaction Details:".cyan());
        println!("  Mailbox UTXO: {}#{}", mailbox_utxo.tx_hash, mailbox_utxo.output_index);
        println!("  Redeemer: {}", hex::encode(&redeemer_cbor));
        println!("  New Datum: {}", hex::encode(&new_datum_cbor));
        println!("\n{}", "Message Summary:".cyan());
        println!("  Message ID: 0x{}", message_id);
        println!("  From: {} (domain {})", payer_address, mailbox_data.local_domain);
        println!("  To: 0x{} (domain {})", recipient_hex, destination);
        println!("  Body: {} bytes", body_bytes.len());
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

    // Find fee UTXO that sorts BEFORE the mailbox UTXO lexicographically.
    // This is critical because Cardano sorts inputs lexicographically, and the
    // mailbox script uses the first input's address as the "sender". We need
    // the fee input (payer's wallet) to come first, not the mailbox script.
    // Must also not have a reference script attached.
    let fee_utxo = payer_utxos
        .iter()
        .find(|u| {
            u.lovelace >= 5_000_000 &&
            u.assets.is_empty() &&
            u.reference_script.is_none() &&
            u.tx_hash < mailbox_utxo.tx_hash  // Must sort before mailbox
        })
        .ok_or_else(|| anyhow!(
            "No suitable fee UTXO found that sorts before mailbox UTXO {}. \
             The fee input must have a tx_hash that is lexicographically smaller \
             than the mailbox tx_hash so that the payer becomes the message sender.",
            mailbox_utxo.tx_hash
        ))?;

    println!("  Collateral: {}#{}", collateral_utxo.tx_hash, collateral_utxo.output_index);
    println!("  Fee input: {}#{}", fee_utxo.tx_hash, fee_utxo.output_index);

    // Parse reference script UTXO if provided
    let ref_script_utxo = if let Some(ref ref_script) = reference_script {
        let parts: Vec<&str> = ref_script.split('#').collect();
        if parts.len() != 2 {
            return Err(anyhow!("Invalid reference script format. Use: txhash#index"));
        }
        Some((parts[0].to_string(), parts[1].parse::<u64>()?))
    } else {
        // Try to get from deployment info
        ctx.load_deployment_info()
            .ok()
            .and_then(|d| d.mailbox)
            .and_then(|m| m.reference_script_utxo)
            .map(|r| (r.tx_hash, r.output_index as u64))
    };

    // Load mailbox script only if not using reference script
    let mailbox_script_bytes = if ref_script_utxo.is_none() {
        let blueprint = ctx.load_blueprint()?;
        let mailbox_validator = blueprint
            .find_validator("mailbox.mailbox.spend")
            .ok_or_else(|| anyhow!("Mailbox validator not found in blueprint"))?;
        Some(hex::decode(&mailbox_validator.compiled_code)?)
    } else {
        println!("  Using reference script: {}#{}", ref_script_utxo.as_ref().unwrap().0, ref_script_utxo.as_ref().unwrap().1);
        None
    };

    // Get PlutusV3 cost model
    let cost_model = client.get_plutusv3_cost_model().await?;

    // Get current slot for validity
    let current_slot = client.get_latest_slot().await?;
    let validity_end = current_slot + 7200; // ~2 hours

    // Build the transaction using pallas_txbuilder
    use pallas_txbuilder::{BuildConway, Input, Output, StagingTransaction, ScriptKind, ExUnits};
    use pallas_crypto::hash::Hash;

    // Parse addresses and hashes
    let mailbox_address = pallas_addresses::Address::from_bech32(&mailbox_utxo.address)
        .map_err(|e| anyhow!("Invalid mailbox address: {:?}", e))?;
    let payer_addr = pallas_addresses::Address::from_bech32(&payer_address)
        .map_err(|e| anyhow!("Invalid payer address: {:?}", e))?;

    let mailbox_tx_hash: [u8; 32] = hex::decode(&mailbox_utxo.tx_hash)?
        .try_into().map_err(|_| anyhow!("Invalid mailbox tx hash"))?;
    let collateral_tx_hash: [u8; 32] = hex::decode(&collateral_utxo.tx_hash)?
        .try_into().map_err(|_| anyhow!("Invalid collateral tx hash"))?;
    let fee_tx_hash: [u8; 32] = hex::decode(&fee_utxo.tx_hash)?
        .try_into().map_err(|_| anyhow!("Invalid fee tx hash"))?;
    let policy_id_bytes: [u8; 28] = hex::decode(&policy_id)?
        .try_into().map_err(|_| anyhow!("Invalid policy ID"))?;

    // Build mailbox continuation output with new datum and state NFT
    // Decode asset name from hex (it's stored as hex string like "4d61696c626f78205374617465")
    let asset_name_bytes = hex::decode(&asset_name_hex)
        .map_err(|e| anyhow!("Invalid asset name hex: {}", e))?;
    let mailbox_output = Output::new(mailbox_address, mailbox_utxo.lovelace)
        .set_inline_datum(new_datum_cbor.clone())
        .add_asset(Hash::new(policy_id_bytes), asset_name_bytes, 1)
        .map_err(|e| anyhow!("Failed to add state NFT: {:?}", e))?;

    // Calculate change
    let fee_estimate = 2_000_000u64;
    let change = fee_utxo.lovelace.saturating_sub(fee_estimate);

    // Build staging transaction
    // Convert payer_pkh to [u8; 28] for disclosed_signer
    let payer_pkh_bytes: [u8; 28] = payer_pkh.clone().try_into()
        .map_err(|_| anyhow!("Invalid payer pkh length"))?;

    let mut staging = StagingTransaction::new()
        // Fee input first (so it becomes the sender)
        .input(Input::new(Hash::new(fee_tx_hash), fee_utxo.output_index as u64))
        // Mailbox script input
        .input(Input::new(Hash::new(mailbox_tx_hash), mailbox_utxo.output_index as u64))
        // Collateral
        .collateral_input(Input::new(Hash::new(collateral_tx_hash), collateral_utxo.output_index as u64))
        // Required signer (so Plutus script can verify sender authorization)
        .disclosed_signer(Hash::new(payer_pkh_bytes))
        // Mailbox continuation output
        .output(mailbox_output)
        // Spend redeemer for mailbox input
        .add_spend_redeemer(
            Input::new(Hash::new(mailbox_tx_hash), mailbox_utxo.output_index as u64),
            redeemer_cbor.clone(),
            Some(ExUnits { mem: 5_000_000, steps: 2_000_000_000 }),
        )
        // Cost model for script data hash
        .language_view(ScriptKind::PlutusV3, cost_model)
        // Fee and validity
        .fee(fee_estimate)
        .invalid_from_slot(validity_end)
        .network_id(0); // Testnet

    // Add reference input OR embedded script
    if let Some((ref_tx_hash, ref_output_idx)) = ref_script_utxo {
        let ref_tx_hash_bytes: [u8; 32] = hex::decode(&ref_tx_hash)?
            .try_into().map_err(|_| anyhow!("Invalid reference script tx hash"))?;
        staging = staging.reference_input(Input::new(Hash::new(ref_tx_hash_bytes), ref_output_idx));
    } else if let Some(script_bytes) = mailbox_script_bytes {
        staging = staging.script(ScriptKind::PlutusV3, script_bytes);
    } else {
        return Err(anyhow!("No script provided and no reference script specified"));
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
    // Unwrap is safe here - we already verified keypair exists (not dry_run)
    let kp = keypair.as_ref().expect("Keypair required for transaction signing");
    let signature = kp.sign(tx_hash_bytes);
    let signed_tx = tx.add_signature(kp.pallas_public_key().clone(), signature)
        .map_err(|e| anyhow!("Failed to sign transaction: {:?}", e))?;

    // Submit the transaction
    println!("{}", "Submitting transaction...".cyan());
    let tx_hash = client.submit_tx(&signed_tx.tx_bytes.0).await?;

    println!("\n{}", "SUCCESS!".green().bold());
    println!("  Transaction Hash: {}", tx_hash);
    println!("  Explorer: {}", ctx.explorer_tx_url(&tx_hash));
    println!("\n{}", "Message Dispatched:".cyan());
    println!("  Message ID: 0x{}", message_id);
    println!("  From: {} (domain {})", payer_address, mailbox_data.local_domain);
    println!("  To: 0x{} (domain {})", recipient_hex, destination);
    println!("  Nonce: {}", mailbox_data.outbound_nonce);

    Ok(())
}

/// Compute message ID (keccak256 of encoded message)
/// Format: version (1 byte) || nonce (4 bytes) || origin (4 bytes) || sender (32 bytes)
///         || destination (4 bytes) || recipient (32 bytes) || body (variable)
fn compute_message_id(
    version: u8,
    nonce: u32,
    origin: u32,
    sender_hex: &str,
    destination: u32,
    recipient_hex: &str,
    body_hex: &str,
) -> Result<String> {
    let mut message = Vec::new();

    // Version (1 byte)
    message.push(version);

    // Nonce (4 bytes, big-endian)
    message.extend_from_slice(&nonce.to_be_bytes());

    // Origin (4 bytes, big-endian)
    message.extend_from_slice(&origin.to_be_bytes());

    // Sender (32 bytes)
    let sender_bytes = hex::decode(sender_hex)?;
    if sender_bytes.len() != 32 {
        return Err(anyhow!("Sender must be 32 bytes"));
    }
    message.extend_from_slice(&sender_bytes);

    // Destination (4 bytes, big-endian)
    message.extend_from_slice(&destination.to_be_bytes());

    // Recipient (32 bytes)
    let recipient_bytes = hex::decode(recipient_hex)?;
    if recipient_bytes.len() != 32 {
        return Err(anyhow!("Recipient must be 32 bytes"));
    }
    message.extend_from_slice(&recipient_bytes);

    // Body (variable)
    let body_bytes = hex::decode(body_hex)?;
    message.extend_from_slice(&body_bytes);

    // Compute keccak256
    let mut hasher = Keccak256::new();
    hasher.update(&message);
    let result = hasher.finalize();

    Ok(hex::encode(result))
}

/// Merkle tree state after update
struct MerkleTreeUpdate {
    branches: Vec<String>,
    count: u32,
}

/// Update merkle tree with a new leaf (message hash)
/// Implements the incremental merkle tree algorithm from hyperlane-core
fn update_merkle_tree(
    current_branches: &[String],
    current_count: u32,
    message_id: &str,
) -> Result<MerkleTreeUpdate> {
    let message_hash = hex::decode(message_id)?;
    if message_hash.len() != 32 {
        return Err(anyhow!("Message hash must be 32 bytes"));
    }

    // Zero hash for empty branches
    let zero_hash = "0000000000000000000000000000000000000000000000000000000000000000";

    // Ensure we have 32 branches (pad with zeros if needed)
    let mut branches: Vec<String> = current_branches.to_vec();
    while branches.len() < 32 {
        branches.push(zero_hash.to_string());
    }

    // Insert the new leaf
    let new_count = current_count + 1;
    let mut node = message_id.to_string();
    let mut size = new_count;
    let mut depth = 0usize;

    // Standard Hyperlane merkle tree algorithm (matches Solidity/Rust implementations)
    while size > 0 {
        if size % 2 == 1 {
            // Odd: store node at this level and stop
            branches[depth] = node.clone();
            break;
        } else {
            // Even: hash with sibling and continue up
            let sibling = &branches[depth];
            node = hash_pair(sibling, &node)?;
        }
        size /= 2;
        depth += 1;
    }

    Ok(MerkleTreeUpdate {
        branches,
        count: new_count,
    })
}

/// Hash two nodes together: keccak256(left || right)
fn hash_pair(left: &str, right: &str) -> Result<String> {
    let left_bytes = hex::decode(left)?;
    let right_bytes = hex::decode(right)?;

    let mut combined = Vec::new();
    combined.extend_from_slice(&left_bytes);
    combined.extend_from_slice(&right_bytes);

    let mut hasher = Keccak256::new();
    hasher.update(&combined);
    let result = hasher.finalize();

    Ok(hex::encode(result))
}

async fn set_default_ism(
    ctx: &CliContext,
    new_ism_hash: &str,
    mailbox_policy: Option<String>,
    reference_script: Option<String>,
    signing_key: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Setting Mailbox default ISM...".cyan());

    // Validate ISM hash
    let new_ism_hash = new_ism_hash.strip_prefix("0x").unwrap_or(new_ism_hash);
    let ism_bytes = hex::decode(new_ism_hash)?;
    if ism_bytes.len() != 28 {
        return Err(anyhow!(
            "ISM script hash must be 28 bytes (56 hex chars), got {}",
            ism_bytes.len()
        ));
    }
    println!("  New ISM: {}", new_ism_hash);

    // Get mailbox policy ID
    let policy_id = get_mailbox_policy(ctx, mailbox_policy)?;
    println!("  Mailbox Policy: {}", policy_id);

    // Load signing key
    let keypair = if let Some(path) = signing_key {
        ctx.load_signing_key_from(std::path::Path::new(&path))?
    } else {
        ctx.load_signing_key()?
    };

    let payer_address = keypair.address_bech32(ctx.pallas_network());
    let payer_pkh = keypair.pub_key_hash();
    println!("  Payer: {}", payer_address);

    // Find mailbox UTXO
    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    // Get asset name from deployment info (hex-encoded)
    let asset_name_hex = ctx
        .load_deployment_info()
        .ok()
        .and_then(|d| d.mailbox)
        .and_then(|m| m.state_nft)
        .map(|nft| nft.asset_name_hex)
        .unwrap_or_else(|| hex::encode("Mailbox State"));

    let mailbox_utxo = client
        .find_utxo_by_asset(&policy_id, &asset_name_hex)
        .await?
        .ok_or_else(|| anyhow!("Mailbox UTXO not found with policy {}", policy_id))?;

    println!("\n{}", "Found Mailbox UTXO:".green());
    println!("  TX: {}#{}", mailbox_utxo.tx_hash, mailbox_utxo.output_index);
    println!("  Address: {}", mailbox_utxo.address);
    println!("  Lovelace: {}", mailbox_utxo.lovelace);

    // Parse current datum
    let current_datum = mailbox_utxo
        .inline_datum
        .as_ref()
        .ok_or_else(|| anyhow!("Mailbox UTXO has no inline datum"))?;

    let mailbox_data = parse_mailbox_datum(current_datum)?;
    println!("\n{}", "Current Mailbox State:".green());
    println!("  Local Domain: {}", mailbox_data.local_domain);
    println!("  Default ISM: {}", mailbox_data.default_ism);
    println!("  Owner: {}", mailbox_data.owner);
    println!("  Outbound Nonce: {}", mailbox_data.outbound_nonce);
    println!("  Merkle Count: {}", mailbox_data.merkle_count);

    // Verify we are the owner
    if mailbox_data.owner != hex::encode(&payer_pkh) {
        return Err(anyhow!(
            "Signing key does not match mailbox owner. Expected: {}, Got: {}",
            mailbox_data.owner,
            hex::encode(&payer_pkh)
        ));
    }

    // Check if ISM is already set to this value
    if mailbox_data.default_ism.to_lowercase() == new_ism_hash.to_lowercase() {
        println!("\n{}", "Mailbox default ISM is already set to this value!".yellow());
        return Ok(());
    }

    // Build new datum with updated default_ism
    // Convert branches from Vec<String> to Vec<&str> for build_mailbox_datum
    let branches_refs: Vec<&str> = mailbox_data.merkle_branches.iter().map(|s| s.as_str()).collect();
    let new_datum_cbor = build_mailbox_datum(
        mailbox_data.local_domain,
        new_ism_hash,
        &mailbox_data.owner,
        mailbox_data.outbound_nonce,
        &branches_refs,
        mailbox_data.merkle_count,
    )?;

    println!("\n{}", "New Mailbox Datum:".green());
    println!("  Default ISM: {}", new_ism_hash);
    println!("  Datum CBOR: {}", hex::encode(&new_datum_cbor));

    // Build SetDefaultIsm redeemer
    let redeemer_cbor = build_mailbox_set_default_ism_redeemer(new_ism_hash)?;
    println!("\n{}", "SetDefaultIsm Redeemer:".green());
    println!("  CBOR: {}", hex::encode(&redeemer_cbor));

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        println!("\nTo update mailbox, build a transaction that:");
        println!("1. Spends Mailbox UTXO: {}#{}", mailbox_utxo.tx_hash, mailbox_utxo.output_index);
        println!("2. Uses SetDefaultIsm redeemer: {}", hex::encode(&redeemer_cbor));
        println!("3. Creates new Mailbox UTXO with updated datum");
        println!("4. Requires owner signature: {}", mailbox_data.owner);
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

    // Find fee UTXO (must not have reference script)
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

    // Parse reference script UTXO if provided
    let ref_script_utxo = if let Some(ref ref_script) = reference_script {
        let parts: Vec<&str> = ref_script.split('#').collect();
        if parts.len() != 2 {
            return Err(anyhow!("Invalid reference script format. Use: txhash#index"));
        }
        Some((parts[0].to_string(), parts[1].parse::<u64>()?))
    } else {
        None
    };

    // Load mailbox script only if not using reference script
    let mailbox_script_bytes = if ref_script_utxo.is_none() {
        let mailbox_script_path = ctx.network_deployments_dir().join("mailbox.plutus");
        if mailbox_script_path.exists() {
            println!("  Loading script from: {}", mailbox_script_path.display());
            let script_json: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(&mailbox_script_path)?
            )?;
            let cbor_hex = script_json
                .get("cborHex")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("Missing cborHex in mailbox.plutus"))?;
            Some(hex::decode(cbor_hex)?)
        } else {
            // Fall back to blueprint
            println!("  Deployment file not found, falling back to blueprint");
            let blueprint = ctx.load_blueprint()?;
            let mailbox_validator = blueprint
                .find_validator("mailbox.mailbox.spend")
                .ok_or_else(|| anyhow!("Mailbox validator not found in blueprint"))?;
            Some(hex::decode(&mailbox_validator.compiled_code)?)
        }
    } else {
        println!("  Using reference script: {}#{}", ref_script_utxo.as_ref().unwrap().0, ref_script_utxo.as_ref().unwrap().1);
        None
    };

    // Get PlutusV3 cost model
    let cost_model = client.get_plutusv3_cost_model().await?;

    // Get current slot for validity
    let current_slot = client.get_latest_slot().await?;
    let validity_end = current_slot + 7200; // ~2 hours

    // Build the transaction using pallas_txbuilder
    use pallas_txbuilder::{BuildConway, Input, Output, StagingTransaction, ScriptKind, ExUnits};
    use pallas_crypto::hash::Hash;

    // Parse addresses and hashes
    let mailbox_address = pallas_addresses::Address::from_bech32(&mailbox_utxo.address)
        .map_err(|e| anyhow!("Invalid mailbox address: {:?}", e))?;
    let payer_addr = pallas_addresses::Address::from_bech32(&payer_address)
        .map_err(|e| anyhow!("Invalid payer address: {:?}", e))?;

    let mailbox_tx_hash: [u8; 32] = hex::decode(&mailbox_utxo.tx_hash)?
        .try_into().map_err(|_| anyhow!("Invalid mailbox tx hash"))?;
    let collateral_tx_hash: [u8; 32] = hex::decode(&collateral_utxo.tx_hash)?
        .try_into().map_err(|_| anyhow!("Invalid collateral tx hash"))?;
    let fee_tx_hash: [u8; 32] = hex::decode(&fee_utxo.tx_hash)?
        .try_into().map_err(|_| anyhow!("Invalid fee tx hash"))?;
    let policy_id_bytes: [u8; 28] = hex::decode(&policy_id)?
        .try_into().map_err(|_| anyhow!("Invalid policy ID"))?;
    let owner_hash: [u8; 28] = hex::decode(&mailbox_data.owner)?
        .try_into().map_err(|_| anyhow!("Invalid owner hash"))?;

    // Build mailbox continuation output with new datum and state NFT
    let mailbox_output = Output::new(mailbox_address, mailbox_utxo.lovelace)
        .set_inline_datum(new_datum_cbor.clone())
        .add_asset(Hash::new(policy_id_bytes), vec![], 1)
        .map_err(|e| anyhow!("Failed to add state NFT: {:?}", e))?;

    // Calculate change
    let fee_estimate = 2_000_000u64;
    let change = fee_utxo.lovelace.saturating_sub(fee_estimate);

    // Build staging transaction
    let mut staging = StagingTransaction::new()
        // Mailbox script input
        .input(Input::new(Hash::new(mailbox_tx_hash), mailbox_utxo.output_index as u64))
        // Fee input
        .input(Input::new(Hash::new(fee_tx_hash), fee_utxo.output_index as u64))
        // Collateral
        .collateral_input(Input::new(Hash::new(collateral_tx_hash), collateral_utxo.output_index as u64))
        // Mailbox continuation output
        .output(mailbox_output)
        // Spend redeemer for mailbox input
        .add_spend_redeemer(
            Input::new(Hash::new(mailbox_tx_hash), mailbox_utxo.output_index as u64),
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

    // Add reference input OR embedded script
    if let Some((ref_tx_hash, ref_output_idx)) = ref_script_utxo {
        let ref_tx_hash_bytes: [u8; 32] = hex::decode(&ref_tx_hash)?
            .try_into().map_err(|_| anyhow!("Invalid reference script tx hash"))?;
        staging = staging.reference_input(Input::new(Hash::new(ref_tx_hash_bytes), ref_output_idx));
    } else if let Some(script_bytes) = mailbox_script_bytes {
        staging = staging.script(ScriptKind::PlutusV3, script_bytes);
    } else {
        return Err(anyhow!("No script provided and no reference script specified"));
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
    println!("\n  Old ISM: {}", mailbox_data.default_ism);
    println!("  New ISM: {}", new_ism_hash);

    Ok(())
}

async fn show_config(
    ctx: &CliContext,
    mailbox_policy: Option<String>,
) -> Result<()> {
    println!("{}", "Mailbox Configuration".cyan());

    let policy_id = get_mailbox_policy(ctx, mailbox_policy)?;
    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let mailbox_utxo = client
        .find_utxo_by_asset(&policy_id, "")
        .await?
        .ok_or_else(|| anyhow!("Mailbox UTXO not found with policy {}", policy_id))?;

    println!("\n{}", "Mailbox UTXO:".green());
    println!("  TX: {}#{}", mailbox_utxo.tx_hash, mailbox_utxo.output_index);
    println!("  Address: {}", mailbox_utxo.address);
    println!("  Lovelace: {}", mailbox_utxo.lovelace);

    if let Some(datum) = &mailbox_utxo.inline_datum {
        println!("\n{}", "Inline Datum:".green());
        println!("{}", serde_json::to_string_pretty(datum)?);

        // Parse and display datum
        match parse_mailbox_datum(datum) {
            Ok(data) => {
                println!("\n{}", "Parsed Configuration:".green());
                println!("  Local Domain: {}", data.local_domain);
                println!("  Default ISM: {}", data.default_ism);
                println!("  Owner: {}", data.owner);
                println!("  Outbound Nonce: {}", data.outbound_nonce);
                println!("  Merkle Branches: {} branches stored", data.merkle_branches.len());
                println!("  Merkle Count: {}", data.merkle_count);
            }
            Err(e) => {
                println!("\n{}", format!("Failed to parse datum: {:?}", e).yellow());
            }
        }
    } else {
        println!("\n{}", "No inline datum found".yellow());
    }

    Ok(())
}

// Helper functions

fn get_mailbox_policy(ctx: &CliContext, mailbox_policy: Option<String>) -> Result<String> {
    if let Some(p) = mailbox_policy {
        return Ok(p);
    }

    // Try to load from deployment info
    let deployment = ctx.load_deployment_info()?;
    deployment
        .mailbox
        .and_then(|m| m.state_nft_policy)
        .ok_or_else(|| anyhow!("Mailbox policy not found. Use --mailbox-policy or update deployment_info.json"))
}

/// Parsed mailbox datum with nested MerkleTreeState
struct MailboxData {
    local_domain: u32,
    default_ism: String,
    owner: String,
    outbound_nonce: u32,
    /// Full branch state from MerkleTreeState (32 branches, each 32 bytes hex)
    merkle_branches: Vec<String>,
    merkle_count: u32,
}

/// Parse mailbox datum from Blockfrost JSON
/// New structure with nested MerkleTreeState:
/// MailboxDatum { local_domain, default_ism, owner, outbound_nonce, merkle_tree: { branches, count } }
fn parse_mailbox_datum(datum: &serde_json::Value) -> Result<MailboxData> {
    // Check if datum is a hex string (raw CBOR)
    if let Some(hex_str) = datum.as_str() {
        return parse_mailbox_datum_from_cbor(hex_str);
    }

    // Otherwise try the JSON format
    let fields = datum
        .get("fields")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow!("Invalid datum structure - missing fields"))?;

    if fields.len() < 5 {
        return Err(anyhow!("Mailbox datum must have 5 fields, got {}", fields.len()));
    }

    let local_domain = fields
        .get(0)
        .and_then(|f| f.get("int"))
        .and_then(|i| i.as_u64())
        .ok_or_else(|| anyhow!("Invalid local_domain"))? as u32;

    let default_ism = fields
        .get(1)
        .and_then(|f| f.get("bytes"))
        .and_then(|b| b.as_str())
        .ok_or_else(|| anyhow!("Invalid default_ism"))?
        .to_string();

    let owner = fields
        .get(2)
        .and_then(|f| f.get("bytes"))
        .and_then(|b| b.as_str())
        .ok_or_else(|| anyhow!("Invalid owner"))?
        .to_string();

    let outbound_nonce = fields
        .get(3)
        .and_then(|f| f.get("int"))
        .and_then(|i| i.as_u64())
        .ok_or_else(|| anyhow!("Invalid outbound_nonce"))? as u32;

    // Parse nested MerkleTreeState { branches: List<ByteArray>, count: Int }
    let merkle_tree = fields
        .get(4)
        .ok_or_else(|| anyhow!("Missing merkle_tree field"))?;

    let merkle_tree_fields = merkle_tree
        .get("fields")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow!("Invalid merkle_tree structure - missing fields"))?;

    if merkle_tree_fields.len() < 2 {
        return Err(anyhow!("MerkleTreeState must have 2 fields, got {}", merkle_tree_fields.len()));
    }

    // Parse branches list
    let branches_list = merkle_tree_fields
        .get(0)
        .and_then(|f| f.get("list"))
        .and_then(|l| l.as_array())
        .ok_or_else(|| anyhow!("Invalid branches list"))?;

    let merkle_branches: Vec<String> = branches_list
        .iter()
        .filter_map(|b| b.get("bytes").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .collect();

    let merkle_count = merkle_tree_fields
        .get(1)
        .and_then(|f| f.get("int"))
        .and_then(|i| i.as_u64())
        .ok_or_else(|| anyhow!("Invalid merkle_count"))? as u32;

    Ok(MailboxData {
        local_domain,
        default_ism,
        owner,
        outbound_nonce,
        merkle_branches,
        merkle_count,
    })
}

/// Parse mailbox datum from raw CBOR hex
/// New structure with nested MerkleTreeState:
/// MailboxDatum { local_domain, default_ism, owner, outbound_nonce, merkle_tree: { branches, count } }
fn parse_mailbox_datum_from_cbor(hex_str: &str) -> Result<MailboxData> {
    let cbor_bytes = hex::decode(hex_str)?;
    let datum: PlutusData = pallas_codec::minicbor::decode(&cbor_bytes)
        .map_err(|e| anyhow!("Failed to decode CBOR datum: {:?}", e))?;

    // Mailbox Datum is Constr 0 [local_domain, default_ism, owner, outbound_nonce, merkle_tree]
    let fields = match &datum {
        PlutusData::Constr(c) if c.tag == 121 => &c.fields,
        _ => return Err(anyhow!("Expected Constr 0 datum")),
    };

    let fields_vec: Vec<&PlutusData> = fields.iter().collect();
    if fields_vec.len() < 5 {
        return Err(anyhow!("Mailbox datum must have 5 fields, got {}", fields_vec.len()));
    }

    let local_domain = extract_u32(fields_vec[0])?;
    let default_ism = extract_bytes_hex(fields_vec[1])?;
    let owner = extract_bytes_hex(fields_vec[2])?;
    let outbound_nonce = extract_u32(fields_vec[3])?;

    // Parse nested MerkleTreeState { branches: List<ByteArray>, count: Int }
    let merkle_tree_fields = match fields_vec[4] {
        PlutusData::Constr(c) if c.tag == 121 => {
            let f: Vec<&PlutusData> = c.fields.iter().collect();
            if f.len() < 2 {
                return Err(anyhow!("MerkleTreeState must have 2 fields"));
            }
            f
        }
        _ => return Err(anyhow!("Expected Constr 0 for MerkleTreeState")),
    };

    // Parse branches list
    let merkle_branches = match merkle_tree_fields[0] {
        PlutusData::Array(arr) => {
            arr.iter()
                .map(|item| extract_bytes_hex(item))
                .collect::<Result<Vec<String>>>()?
        }
        _ => return Err(anyhow!("Expected array for merkle branches")),
    };

    let merkle_count = extract_u32(merkle_tree_fields[1])?;

    Ok(MailboxData {
        local_domain,
        default_ism,
        owner,
        outbound_nonce,
        merkle_branches,
        merkle_count,
    })
}

/// Extract u32 from PlutusData
fn extract_u32(data: &PlutusData) -> Result<u32> {
    match data {
        PlutusData::BigInt(BigInt::Int(i)) => {
            let inner = &i.0;
            match i64::try_from(*inner) {
                Ok(val) => Ok(val as u32),
                Err(_) => Err(anyhow!("Integer too large for u32")),
            }
        }
        _ => Err(anyhow!("Expected integer")),
    }
}

/// Extract bytes as hex string from PlutusData
fn extract_bytes_hex(data: &PlutusData) -> Result<String> {
    match data {
        PlutusData::BoundedBytes(b) => {
            let bytes: &[u8] = b.as_ref();
            Ok(hex::encode(bytes))
        }
        _ => Err(anyhow!("Expected bytes")),
    }
}

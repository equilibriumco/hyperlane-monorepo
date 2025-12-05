//! Cardano Recipient Registration CLI Tool
//!
//! This tool helps developers register their recipient contracts in the
//! Hyperlane registry on Cardano.
//!
//! Usage:
//!   cardano_register --script-hash <HASH> --state-policy <POLICY_ID> --state-asset <ASSET_NAME>
//!
//! Options:
//!   --script-hash       The recipient script hash (28 bytes, hex)
//!   --state-policy      Policy ID of the NFT that marks the recipient state UTXO
//!   --state-asset       Asset name of the state NFT (hex)
//!   --recipient-type    Type of recipient: generic, token-receiver, deferred
//!   --custom-ism        Optional custom ISM script hash
//!   --additional-input  Additional inputs in format "name:policy:asset:spend" (can repeat)

use clap::{Parser, ValueEnum};
use hyperlane_cardano::{
    types::{AdditionalInput, RecipientRegistration, RecipientType, ScriptHash, UtxoLocator},
    CardanoNetwork,
};
use std::env;

#[derive(Parser, Debug)]
#[command(name = "cardano_register")]
#[command(about = "Register a recipient contract in the Hyperlane registry on Cardano")]
struct Args {
    /// Recipient script hash (28 bytes, hex-encoded)
    #[arg(long)]
    script_hash: String,

    /// Owner verification key hash (28 bytes, hex-encoded)
    /// This is who can update/remove the registration
    #[arg(long)]
    owner: String,

    /// Policy ID of the NFT that marks the recipient state UTXO
    #[arg(long)]
    state_policy: String,

    /// Asset name of the state NFT (hex-encoded, can be empty for unit name)
    #[arg(long, default_value = "")]
    state_asset: String,

    /// Type of recipient
    #[arg(long, value_enum, default_value = "generic")]
    recipient_type: RecipientTypeArg,

    /// Custom ISM script hash (optional, 28 bytes, hex-encoded)
    #[arg(long)]
    custom_ism: Option<String>,

    /// Additional inputs in format "name:policy_id:asset_name:must_spend"
    /// Example: "oracle:abc123:feed:true"
    #[arg(long, action = clap::ArgAction::Append)]
    additional_input: Vec<String>,

    /// Policy ID of the NFT that marks the reference script UTXO (optional)
    /// If not provided, the script is assumed to be embedded in the state UTXO
    #[arg(long)]
    ref_script_policy: Option<String>,

    /// Asset name of the reference script NFT (hex-encoded, can be empty for unit name)
    #[arg(long)]
    ref_script_asset: Option<String>,

    /// For TokenReceiver: vault policy ID
    #[arg(long)]
    vault_policy: Option<String>,

    /// For TokenReceiver: vault asset name
    #[arg(long)]
    vault_asset: Option<String>,

    /// For TokenReceiver: minting policy script hash
    #[arg(long)]
    minting_policy: Option<String>,

    /// For Deferred: message NFT policy ID
    #[arg(long)]
    message_policy: Option<String>,

    /// Cardano network (mainnet, preprod, preview)
    #[arg(long, default_value = "preprod")]
    network: NetworkArg,

    /// Print the registration data without submitting
    #[arg(long)]
    dry_run: bool,
}

#[derive(ValueEnum, Clone, Debug)]
enum RecipientTypeArg {
    Generic,
    TokenReceiver,
    Deferred,
}

#[derive(ValueEnum, Clone, Debug)]
enum NetworkArg {
    Mainnet,
    Preprod,
    Preview,
}

impl From<NetworkArg> for CardanoNetwork {
    fn from(n: NetworkArg) -> Self {
        match n {
            NetworkArg::Mainnet => CardanoNetwork::Mainnet,
            NetworkArg::Preprod => CardanoNetwork::Preprod,
            NetworkArg::Preview => CardanoNetwork::Preview,
        }
    }
}

fn parse_script_hash(hex: &str) -> Result<ScriptHash, String> {
    let bytes = hex::decode(hex).map_err(|e| format!("Invalid hex: {}", e))?;
    if bytes.len() != 28 {
        return Err(format!("Script hash must be 28 bytes, got {}", bytes.len()));
    }
    let mut hash = [0u8; 28];
    hash.copy_from_slice(&bytes);
    Ok(hash)
}

fn parse_additional_input(s: &str) -> Result<AdditionalInput, String> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 4 {
        return Err("Additional input must be in format 'name:policy_id:asset_name:must_spend'".to_string());
    }

    let name = parts[0].to_string();
    let policy_id = parts[1].to_string();
    let asset_name = parts[2].to_string();
    let must_be_spent = parts[3].parse::<bool>().map_err(|_| "must_spend must be 'true' or 'false'")?;

    Ok(AdditionalInput {
        name,
        locator: UtxoLocator { policy_id, asset_name },
        must_be_spent,
    })
}

fn build_recipient_type(args: &Args) -> Result<RecipientType, String> {
    match args.recipient_type {
        RecipientTypeArg::Generic => Ok(RecipientType::Generic),
        RecipientTypeArg::TokenReceiver => {
            let vault_locator = match (&args.vault_policy, &args.vault_asset) {
                (Some(policy), Some(asset)) => Some(UtxoLocator {
                    policy_id: policy.clone(),
                    asset_name: asset.clone(),
                }),
                (None, None) => None,
                _ => return Err("Both --vault-policy and --vault-asset must be provided together".to_string()),
            };

            let minting_policy = args.minting_policy
                .as_ref()
                .map(|h| parse_script_hash(h))
                .transpose()?;

            Ok(RecipientType::TokenReceiver {
                vault_locator,
                minting_policy,
            })
        }
        RecipientTypeArg::Deferred => {
            let message_policy_hex = args.message_policy
                .as_ref()
                .ok_or("--message-policy is required for Deferred")?;

            Ok(RecipientType::Deferred {
                message_policy: parse_script_hash(message_policy_hex)?,
            })
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Get API key from environment (will be used for actual submission)
    let _api_key = env::var("BLOCKFROST_API_KEY")
        .expect("Set BLOCKFROST_API_KEY environment variable");

    // Parse script hash
    let script_hash = parse_script_hash(&args.script_hash)?;

    // Parse owner
    let owner = parse_script_hash(&args.owner)?;

    // Build recipient type
    let recipient_type = build_recipient_type(&args)?;

    // Parse custom ISM
    let custom_ism = args.custom_ism
        .as_ref()
        .map(|h| parse_script_hash(h))
        .transpose()?;

    // Parse additional inputs
    let additional_inputs: Vec<AdditionalInput> = args.additional_input
        .iter()
        .map(|s| parse_additional_input(s))
        .collect::<Result<Vec<_>, _>>()?;

    // Build reference script locator if provided
    let reference_script_locator = match (&args.ref_script_policy, &args.ref_script_asset) {
        (Some(policy), asset) => Some(UtxoLocator {
            policy_id: policy.clone(),
            asset_name: asset.clone().unwrap_or_default(),
        }),
        _ => None,
    };

    // Build registration
    let registration = RecipientRegistration {
        script_hash,
        owner,
        state_locator: UtxoLocator {
            policy_id: args.state_policy.clone(),
            asset_name: args.state_asset.clone(),
        },
        reference_script_locator,
        additional_inputs,
        recipient_type,
        custom_ism,
    };

    // Print registration data
    println!("=== Recipient Registration ===");
    println!("Script Hash: {}", hex::encode(registration.script_hash));
    println!("State UTXO:");
    println!("  Policy ID: {}", registration.state_locator.policy_id);
    println!("  Asset Name: {}", registration.state_locator.asset_name);
    if let Some(ref ref_locator) = registration.reference_script_locator {
        println!("Reference Script UTXO:");
        println!("  Policy ID: {}", ref_locator.policy_id);
        println!("  Asset Name: {}", ref_locator.asset_name);
    } else {
        println!("Reference Script: Embedded in state UTXO");
    }
    println!("Recipient Type: {:?}", registration.recipient_type);
    if let Some(ism) = &registration.custom_ism {
        println!("Custom ISM: {}", hex::encode(ism));
    }
    if !registration.additional_inputs.is_empty() {
        println!("Additional Inputs:");
        for input in &registration.additional_inputs {
            println!("  - {} ({}/{}), must_spend={}",
                input.name,
                input.locator.policy_id,
                input.locator.asset_name,
                input.must_be_spent
            );
        }
    }

    if args.dry_run {
        println!("\n[Dry run - registration not submitted]");
        return Ok(());
    }

    // Build CBOR datum for registration
    println!("\n=== Building Registration Datum ===");

    // Note: Actually submitting the registration requires:
    // 1. Building a transaction that spends the registry UTXO
    // 2. Including the new registration in the updated datum
    // 3. Signing with the registry owner key
    // 4. Submitting to Blockfrost
    //
    // This would typically be done by the registry owner, not individual recipients.
    // Recipients would submit a registration request that the registry owner approves.

    println!("To complete registration:");
    println!("1. The registry owner must create a transaction that:");
    println!("   - Spends the registry UTXO with 'Register' redeemer");
    println!("   - Includes this registration in the new datum");
    println!("2. Sign and submit the transaction");
    println!();
    println!("Registration data (JSON for manual submission):");
    println!("{}", serde_json::to_string_pretty(&registration)?);

    Ok(())
}

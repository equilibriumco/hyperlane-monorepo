//! Greeting contract commands
//!
//! Higher-level commands for interacting with the greeting recipient contract.
//! These commands auto-derive the redeemer and new datum from the message body,
//! removing the need to manually construct CBOR.

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};
use colored::Colorize;

use crate::utils::blockfrost::BlockfrostClient;
use crate::utils::cbor::{normalize_datum, CborBuilder};
use crate::utils::context::CliContext;
use crate::utils::plutus::{
    apply_validator_param, encode_script_hash_param, script_hash_to_address,
};
use crate::utils::tx_builder::HyperlaneTxBuilder;

use super::message::{
    decode_body_utf8, parse_message_redemption_datum, parse_utxo_ref, resolve_message_infra,
};

#[derive(Args)]
pub struct GreetingArgs {
    #[command(subcommand)]
    command: GreetingCommands,
}

#[derive(Subcommand)]
enum GreetingCommands {
    /// List pending greeting messages
    List {
        /// Greeting contract NFT policy ID (auto-loaded from deployment_info.json)
        #[arg(long)]
        greeting_policy: Option<String>,

        /// Output format (table or json)
        #[arg(long, default_value = "table")]
        format: String,
    },

    /// Receive a greeting message (auto-builds redeemer and new datum)
    Receive {
        /// Message UTXO to receive (auto-discovered if omitted)
        #[arg(long)]
        message_utxo: Option<String>,

        /// Greeting contract NFT policy ID (auto-loaded from deployment_info.json)
        #[arg(long)]
        greeting_policy: Option<String>,

        /// Dry run (don't submit transaction)
        #[arg(long)]
        dry_run: bool,
    },

    /// Show current greeting contract state
    Show {
        /// Greeting contract NFT policy ID (auto-loaded from deployment_info.json)
        #[arg(long)]
        greeting_policy: Option<String>,
    },
}

fn resolve_greeting_policy(ctx: &CliContext, override_policy: Option<String>) -> Result<String> {
    if let Some(policy) = override_policy {
        return Ok(policy);
    }
    let deployment = ctx.load_deployment_info()
        .map_err(|_| anyhow!("No --greeting-policy provided and deployment_info.json not found"))?;
    deployment
        .recipients
        .iter()
        .find(|r| r.recipient_type == "greeting")
        .map(|r| r.nft_policy.clone())
        .ok_or_else(|| anyhow!(
            "No greeting recipient in deployment_info.json. Use --greeting-policy or deploy with 'init recipient'"
        ))
}

pub async fn execute(ctx: &CliContext, args: GreetingArgs) -> Result<()> {
    match args.command {
        GreetingCommands::List {
            greeting_policy,
            format,
        } => {
            let policy = resolve_greeting_policy(ctx, greeting_policy)?;
            list_greetings(ctx, &policy, &format).await
        }

        GreetingCommands::Receive {
            message_utxo,
            greeting_policy,
            dry_run,
        } => {
            let policy = resolve_greeting_policy(ctx, greeting_policy)?;
            receive_greeting(ctx, message_utxo.as_deref(), &policy, dry_run).await
        }

        GreetingCommands::Show { greeting_policy } => {
            let policy = resolve_greeting_policy(ctx, greeting_policy)?;
            show_greeting(ctx, &policy).await
        }
    }
}

async fn list_greetings(ctx: &CliContext, greeting_policy: &str, format: &str) -> Result<()> {
    println!("{}", "Listing pending greeting messages...".cyan());

    let infra = resolve_message_infra(ctx)?;

    let redemption_address =
        script_hash_to_address(&infra.redemption_hash, ctx.pallas_network())?;

    println!("  Redemption address: {}", redemption_address);
    println!("  NFT Policy: {}", infra.message_nft_policy);
    println!("  Filter by greeting: {}", greeting_policy);

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let utxos = client.get_utxos(&redemption_address).await?;

    let message_utxos: Vec<_> = utxos
        .iter()
        .filter(|utxo| {
            utxo.assets
                .iter()
                .any(|asset| asset.policy_id == infra.message_nft_policy)
        })
        .filter(|utxo| {
            if let Some(datum_json) = &utxo.inline_datum {
                let datum_str = serde_json::to_string(datum_json).unwrap_or_default();
                if let Ok(parsed) = parse_message_redemption_datum(&datum_str) {
                    return parsed.recipient_policy == greeting_policy;
                }
            }
            false
        })
        .collect();

    if message_utxos.is_empty() {
        println!("\n{}", "No pending greeting messages found.".yellow());
        return Ok(());
    }

    println!(
        "\n{} {} pending greeting message(s):",
        "Found".green(),
        message_utxos.len()
    );

    if format == "json" {
        let json_output: Vec<serde_json::Value> = message_utxos
            .iter()
            .filter_map(|utxo| {
                let datum_str =
                    serde_json::to_string(utxo.inline_datum.as_ref()?).unwrap_or_default();
                let parsed = parse_message_redemption_datum(&datum_str).ok()?;
                let body_decoded = decode_body_utf8(&parsed.body);
                Some(serde_json::json!({
                    "utxo": format!("{}#{}", utxo.tx_hash, utxo.output_index),
                    "origin": parsed.origin,
                    "nonce": parsed.nonce,
                    "body_hex": parsed.body,
                    "body": body_decoded,
                    "expiry_slot": parsed.expiry_slot,
                }))
            })
            .collect();

        println!("{}", serde_json::to_string_pretty(&json_output)?);
    } else {
        for (i, utxo) in message_utxos.iter().enumerate() {
            if i > 0 {
                println!();
            }
            println!("{}", format!("Greeting {}:", i + 1).green());
            println!("  UTXO:   {}#{}", utxo.tx_hash, utxo.output_index);

            if let Some(datum_json) = &utxo.inline_datum {
                let datum_str = serde_json::to_string(datum_json).unwrap_or_default();
                if let Ok(parsed) = parse_message_redemption_datum(&datum_str) {
                    println!("  Origin: {}", parsed.origin);
                    println!("  Nonce:  {}", parsed.nonce);
                    println!("  Expiry: slot {}", parsed.expiry_slot);
                    if let Some(decoded) = decode_body_utf8(&parsed.body) {
                        println!("  Body:   {}", decoded.cyan());
                        println!(
                            "  Result: {}",
                            format!("Hello, {}", decoded).green()
                        );
                    } else {
                        println!("  Body (hex): {}", parsed.body);
                    }
                }
            }
        }
    }

    Ok(())
}

async fn receive_greeting(
    ctx: &CliContext,
    message_utxo_ref: Option<&str>,
    greeting_policy: &str,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Receiving greeting message...".cyan());

    let infra = resolve_message_infra(ctx)?;
    let message_nft_policy = &infra.message_nft_policy;

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);
    let tx_builder = HyperlaneTxBuilder::new(&client, ctx.pallas_network());

    let keypair = ctx.load_signing_key()?;
    let payer_address = keypair.address_bech32(ctx.pallas_network());
    println!("  Receiver: {}", payer_address);

    // Auto-discover message UTXO if not provided
    let resolved_utxo_ref = match message_utxo_ref {
        Some(r) => r.to_string(),
        None => {
            println!("  Auto-discovering pending greeting message...");
            let redemption_address =
                script_hash_to_address(&infra.redemption_hash, ctx.pallas_network())?;
            let utxos = client.get_utxos(&redemption_address).await?;
            let pending: Vec<_> = utxos
                .iter()
                .filter(|u| {
                    u.assets.iter().any(|a| a.policy_id == *message_nft_policy)
                })
                .filter(|u| {
                    u.inline_datum.as_ref().map_or(false, |datum_json| {
                        let datum_str = serde_json::to_string(datum_json).unwrap_or_default();
                        parse_message_redemption_datum(&datum_str)
                            .map_or(false, |p| p.recipient_policy == greeting_policy)
                    })
                })
                .collect();
            match pending.len() {
                0 => return Err(anyhow!("No pending greeting messages found")),
                1 => {
                    let u = pending[0];
                    let r = format!("{}#{}", u.tx_hash, u.output_index);
                    println!("  Found 1 pending message: {}", r);
                    r
                }
                n => {
                    let u = pending[0];
                    let r = format!("{}#{}", u.tx_hash, u.output_index);
                    println!("  Found {} pending messages, using oldest: {}", n, r);
                    r
                }
            }
        }
    };

    // 1. Fetch message UTXO and parse datum
    let (msg_tx_hash, msg_output_index) = parse_utxo_ref(&resolved_utxo_ref)?;
    let tx_utxos = client.get_tx_utxos(&msg_tx_hash).await?;
    let msg_utxo_entry = tx_utxos
        .outputs
        .iter()
        .find(|o| o.output_index == msg_output_index)
        .ok_or_else(|| anyhow!("Message UTXO not found: {}", resolved_utxo_ref))?;

    let msg_address = &msg_utxo_entry.address;
    let msg_utxos = client.get_utxos(msg_address).await?;
    let message_utxo = msg_utxos
        .iter()
        .find(|u| u.tx_hash == msg_tx_hash && u.output_index == msg_output_index)
        .ok_or_else(|| anyhow!("Message UTXO already spent or not found"))?;

    println!("\n{}", "Message UTXO:".green());
    println!("  {}#{}", message_utxo.tx_hash, message_utxo.output_index);

    let message_id = message_utxo
        .assets
        .iter()
        .find(|asset| asset.policy_id == *message_nft_policy)
        .map(|asset| asset.asset_name.clone())
        .ok_or_else(|| anyhow!("Stored message NFT not found in UTXO"))?;

    println!("  Message ID: {}", message_id);

    let datum_json = message_utxo
        .inline_datum
        .as_ref()
        .ok_or_else(|| anyhow!("Message UTXO has no inline datum"))?;
    let datum_str = serde_json::to_string(datum_json)?;
    let parsed_datum = parse_message_redemption_datum(&datum_str)?;

    println!("  Origin: {}", parsed_datum.origin);
    println!("  Return Address (relayer): {}", parsed_datum.return_address);

    let body_hex = &parsed_datum.body;
    let body_bytes =
        hex::decode(body_hex).map_err(|e| anyhow!("Invalid body hex in datum: {}", e))?;

    if let Some(decoded) = decode_body_utf8(body_hex) {
        println!("  Body: {}", decoded.cyan());
        println!("  Greeting: {}", format!("Hello, {}", decoded).green());
    } else {
        println!("  Body (hex): {}", body_hex);
    }

    // 2. Fetch greeting recipient state UTXO and parse current datum
    let recipient_state_utxo = client
        .find_utxo_by_asset(greeting_policy, "")
        .await?
        .ok_or_else(|| {
            anyhow!(
                "Greeting state UTXO not found with policy {}",
                greeting_policy
            )
        })?;

    println!("\n{}", "Greeting State UTXO:".green());
    println!(
        "  {}#{}",
        recipient_state_utxo.tx_hash, recipient_state_utxo.output_index
    );

    let state_datum_json = recipient_state_utxo
        .inline_datum
        .as_ref()
        .ok_or_else(|| anyhow!("Greeting state UTXO has no inline datum"))?;
    let state_datum_str = serde_json::to_string(state_datum_json)?;
    let (old_greeting, old_count) = parse_greeting_datum(&state_datum_str)?;

    if let Some(decoded) = decode_body_utf8(&old_greeting) {
        println!("  Current greeting: {}", decoded);
    }
    println!("  Current count: {}", old_count);

    // 3. Build greeting redeemer: HandleMessage { body } = Constr 0 [Bytes(body)]
    let recipient_redeemer = build_greeting_redeemer(body_hex)?;
    println!(
        "\n  Built greeting redeemer (HandleMessage, {} bytes)",
        body_bytes.len()
    );

    // 4. Build new greeting datum
    let mut greeting_bytes = b"Hello, ".to_vec();
    greeting_bytes.extend_from_slice(&body_bytes);
    let greeting_hex = hex::encode(&greeting_bytes);
    let new_count = old_count + 1;

    let new_datum = build_greeting_datum(&greeting_hex, new_count)?;
    println!(
        "  Built new greeting datum (count: {})",
        new_count
    );

    // 5. Auto-discover recipient reference script
    let recipient_ref_script =
        match client.find_utxo_by_asset(greeting_policy, "726566").await? {
            Some(ref_utxo) => {
                let ref_str = format!("{}#{}", ref_utxo.tx_hash, ref_utxo.output_index);
                println!("  Auto-discovered greeting ref script: {}", ref_str);
                Some(ref_str)
            }
            None => None,
        };

    // 6. Find fee UTXO
    let fee_utxos = client.get_utxos(&payer_address).await?;
    let fee_utxo = fee_utxos
        .iter()
        .find(|u| u.assets.is_empty() && u.lovelace >= 5_000_000 && u.reference_script.is_none())
        .ok_or_else(|| {
            anyhow!("No suitable fee UTXO found (need >= 5 ADA without tokens or reference scripts)")
        })?;

    println!("\n{}", "Fee UTXO:".green());
    println!("  {}#{}", fee_utxo.tx_hash, fee_utxo.output_index);

    // 7. Build transaction
    println!("\n{}", "Building receive transaction...".cyan());

    let receive_redeemer = build_claim_message_redeemer();
    let nft_redeemer = build_nft_burn_redeemer();

    // Load inline scripts
    let deployment_info = ctx.load_deployment_info()?;
    let mailbox_info = deployment_info
        .mailbox
        .as_ref()
        .ok_or_else(|| anyhow!("Mailbox not found in deployment_info.json"))?;
    let mailbox_policy = mailbox_info
        .state_nft_policy
        .as_ref()
        .ok_or_else(|| anyhow!("Missing mailbox.stateNftPolicy in deployment_info.json"))?;

    let nft_policy_param = encode_script_hash_param(message_nft_policy)?;
    let nft_policy_hex = hex::encode(&nft_policy_param);
    let redemption_applied = apply_validator_param(
        &ctx.contracts_dir,
        "message_redemption",
        "message_redemption",
        &nft_policy_hex,
    )?;
    let redemption_inline_script = hex::decode(&redemption_applied.compiled_code)?;

    let mailbox_policy_param = encode_script_hash_param(mailbox_policy)?;
    let mailbox_policy_hex = hex::encode(&mailbox_policy_param);
    let nft_applied = apply_validator_param(
        &ctx.contracts_dir,
        "stored_message_nft",
        "stored_message_nft",
        &mailbox_policy_hex,
    )?;
    let nft_inline_script = hex::decode(&nft_applied.compiled_code)?;

    let built_tx = tx_builder
        .build_message_receive_tx(
            &keypair,
            fee_utxo,
            message_utxo,
            &recipient_state_utxo,
            message_nft_policy,
            &message_id,
            &parsed_datum.return_address,
            &receive_redeemer,
            &nft_redeemer,
            None,
            None,
            Some(&redemption_inline_script),
            Some(&nft_inline_script),
            Some(&recipient_redeemer),
            Some(&new_datum),
            recipient_ref_script.as_deref(),
            None,
        )
        .await?;

    println!("  Transaction built");

    if dry_run {
        println!("\n{}", "[Dry run - transaction not submitted]".yellow());
        println!("\nTransaction hash: {}", hex::encode(built_tx.tx_hash.0));
        return Ok(());
    }

    let signed_tx = tx_builder.sign_tx(built_tx, &keypair)?;
    println!("  Transaction signed ({} bytes)", signed_tx.len());

    println!("\n{}", "Submitting transaction...".cyan());
    let tx_hash = client.submit_and_confirm(&signed_tx, ctx.no_wait).await?;

    println!("\n{}", "Greeting received successfully!".green());
    if let Some(decoded) = decode_body_utf8(body_hex) {
        println!("  Greeting: Hello, {}", decoded);
    }
    println!("  Count: {}", new_count);
    println!("  View on explorer: {}", ctx.explorer_tx_url(&tx_hash));

    Ok(())
}

async fn show_greeting(ctx: &CliContext, greeting_policy: &str) -> Result<()> {
    println!("{}", "Fetching greeting contract state...".cyan());

    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    let utxo = client
        .find_utxo_by_asset(greeting_policy, "")
        .await?
        .ok_or_else(|| {
            anyhow!(
                "Greeting state UTXO not found with policy {}",
                greeting_policy
            )
        })?;

    println!("\n{}", "Greeting Contract State:".green());
    println!("  UTXO:   {}#{}", utxo.tx_hash, utxo.output_index);
    println!("  Policy: {}", greeting_policy);

    let datum_json = utxo
        .inline_datum
        .as_ref()
        .ok_or_else(|| anyhow!("Greeting state UTXO has no inline datum"))?;
    let datum_str = serde_json::to_string(datum_json)?;
    let (greeting_hex, count) = parse_greeting_datum(&datum_str)?;

    if let Some(decoded) = decode_body_utf8(&greeting_hex) {
        println!("  Last greeting: {}", decoded.cyan());
    } else if greeting_hex.is_empty() {
        println!("  Last greeting: (none)");
    } else {
        println!("  Last greeting (hex): {}", greeting_hex);
    }
    println!("  Greeting count: {}", count);

    Ok(())
}

/// Parse GreetingDatum: Constr 0 [Bytes(last_greeting), Int(greeting_count)]
fn parse_greeting_datum(json_str: &str) -> Result<(String, u64)> {
    let raw_json: serde_json::Value = serde_json::from_str(json_str)?;
    let json = normalize_datum(&raw_json)?;

    let fields = json
        .get("fields")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow!("Invalid GreetingDatum: missing fields"))?;

    if fields.len() < 2 {
        return Err(anyhow!(
            "Invalid GreetingDatum: expected 2 fields, got {}",
            fields.len()
        ));
    }

    let last_greeting = fields[0]
        .get("bytes")
        .and_then(|b| b.as_str())
        .ok_or_else(|| anyhow!("Invalid GreetingDatum: bad last_greeting"))?
        .to_string();

    let count = fields[1]
        .get("int")
        .and_then(|i| i.as_u64())
        .ok_or_else(|| anyhow!("Invalid GreetingDatum: bad greeting_count"))?;

    Ok((last_greeting, count))
}

/// Build GreetingRedeemer: HandleMessage { body } = Constr 0 [Bytes(body)]
fn build_greeting_redeemer(body_hex: &str) -> Result<Vec<u8>> {
    let mut builder = CborBuilder::new();
    builder.start_constr(0);
    builder.bytes_hex(body_hex)?;
    builder.end_constr();
    Ok(builder.build())
}

/// Build GreetingDatum: Constr 0 [Bytes(last_greeting), Int(greeting_count)]
fn build_greeting_datum(greeting_hex: &str, count: u64) -> Result<Vec<u8>> {
    let mut builder = CborBuilder::new();
    builder.start_constr(0);
    builder.bytes_hex(greeting_hex)?;
    builder.uint(count);
    builder.end_constr();
    Ok(builder.build())
}

fn build_claim_message_redeemer() -> Vec<u8> {
    let mut builder = CborBuilder::new();
    builder.start_constr(0).end_constr();
    builder.build()
}

fn build_nft_burn_redeemer() -> Vec<u8> {
    let mut builder = CborBuilder::new();
    builder.start_constr(1).end_constr();
    builder.build()
}

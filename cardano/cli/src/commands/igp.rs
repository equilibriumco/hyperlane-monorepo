//! IGP command - Manage Interchain Gas Paymaster

use anyhow::{anyhow, Result};
use clap::{Args, Subcommand};
use colored::Colorize;
use pallas_crypto::hash::Hash;
use pallas_primitives::conway::{BigInt, Constr, PlutusData};
use pallas_primitives::MaybeIndefArray;
use pallas_txbuilder::{BuildConway, ExUnits, Input, Output, ScriptKind, StagingTransaction};

use crate::utils::blockfrost::BlockfrostClient;
use crate::utils::cbor::build_igp_datum;
use crate::utils::context::CliContext;
use crate::utils::crypto::Keypair;
use crate::utils::types::Utxo;

/// Shared context for IGP transactions that require signing
struct IgpTxContext {
    policy_id: String,
    keypair: Keypair,
    payer_address: String,
    payer_pkh: Vec<u8>,
    client: BlockfrostClient,
    igp_utxo: Utxo,
    // Parsed datum fields
    owner: Vec<u8>,
    beneficiary: Vec<u8>,
    gas_oracles: Vec<(u32, u64, u64)>,
    default_gas_limit: u64,
}

impl IgpTxContext {
    /// Create a new IGP transaction context
    async fn new(ctx: &CliContext, igp_policy: Option<String>) -> Result<Self> {
        let policy_id = get_igp_policy(ctx, igp_policy)?;
        let keypair = ctx.load_signing_key()?;
        let payer_address = keypair.address_bech32(ctx.pallas_network());
        let payer_pkh = keypair.pub_key_hash();

        let api_key = ctx.require_api_key()?;
        let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

        let igp_utxo = client
            .find_utxo_by_asset(&policy_id, "")
            .await?
            .ok_or_else(|| anyhow!("IGP UTXO not found with policy {}", policy_id))?;

        let current_datum = igp_utxo
            .inline_datum
            .as_ref()
            .ok_or_else(|| anyhow!("IGP UTXO has no inline datum"))?;

        let (owner, beneficiary, gas_oracles, default_gas_limit) = parse_igp_datum(current_datum)?;

        Ok(Self {
            policy_id,
            keypair,
            payer_address,
            payer_pkh,
            client,
            igp_utxo,
            owner,
            beneficiary,
            gas_oracles,
            default_gas_limit,
        })
    }

    /// Print common info about IGP UTXO
    fn print_igp_utxo_info(&self) {
        println!("\n{}", "Found IGP UTXO:".green());
        println!("  TX: {}#{}", self.igp_utxo.tx_hash, self.igp_utxo.output_index);
        println!("  Address: {}", self.igp_utxo.address);
        println!("  Lovelace: {}", self.igp_utxo.lovelace);
    }

    /// Build the new IGP datum CBOR
    fn build_new_datum(&self, updated_oracles: Option<&Vec<(u32, u64, u64)>>) -> Result<Vec<u8>> {
        let oracles = updated_oracles.unwrap_or(&self.gas_oracles);
        build_igp_datum(
            &hex::encode(&self.owner),
            &hex::encode(&self.beneficiary),
            oracles,
            self.default_gas_limit,
        )
    }

    /// Get state NFT asset name from IGP UTXO
    fn get_state_nft_asset_name(&self) -> Result<Vec<u8>> {
        let state_nft_asset = self
            .igp_utxo
            .assets
            .iter()
            .find(|a| a.policy_id == self.policy_id)
            .ok_or_else(|| anyhow!("State NFT not found in IGP UTXO"))?;
        Ok(hex::decode(&state_nft_asset.asset_name).unwrap_or_default())
    }

    /// Find collateral UTXO from payer's UTXOs
    fn find_collateral_utxo<'a>(&self, utxos: &'a [Utxo]) -> Result<&'a Utxo> {
        utxos
            .iter()
            .find(|u| u.lovelace >= 5_000_000 && u.assets.is_empty())
            .ok_or_else(|| anyhow!("No suitable collateral UTXO (need 5+ ADA without tokens)"))
    }

    /// Find fee/payment UTXO distinct from collateral
    fn find_fee_utxo<'a>(
        &self,
        utxos: &'a [Utxo],
        collateral: &'a Utxo,
        min_lovelace: u64,
    ) -> &'a Utxo {
        utxos
            .iter()
            .find(|u| {
                u.lovelace >= min_lovelace
                    && u.assets.is_empty()
                    && (u.tx_hash != collateral.tx_hash || u.output_index != collateral.output_index)
            })
            .unwrap_or(collateral)
    }

    /// Build, sign, and submit an IGP transaction
    async fn build_sign_submit(
        &self,
        ctx: &CliContext,
        new_datum_cbor: Vec<u8>,
        redeemer_cbor: Vec<u8>,
        new_igp_lovelace: u64,
        additional_output: Option<Output>,
        fee_utxo: &Utxo,
        collateral_utxo: &Utxo,
        signer_pkh: &[u8],
    ) -> Result<String> {
        // Load IGP script from blueprint
        let blueprint = ctx.load_blueprint()?;
        let igp_validator = blueprint
            .find_validator("igp.igp.spend")
            .ok_or_else(|| anyhow!("IGP validator not found in blueprint"))?;
        let igp_script_bytes = hex::decode(&igp_validator.compiled_code)?;

        // Get PlutusV3 cost model
        let cost_model = self.client.get_plutusv3_cost_model().await?;

        // Get current slot for validity
        let current_slot = self.client.get_latest_slot().await?;
        let validity_end = current_slot + 7200; // ~2 hours

        // Parse addresses and hashes
        let igp_address = pallas_addresses::Address::from_bech32(&self.igp_utxo.address)
            .map_err(|e| anyhow!("Invalid IGP address: {:?}", e))?;
        let payer_addr = pallas_addresses::Address::from_bech32(&self.payer_address)
            .map_err(|e| anyhow!("Invalid payer address: {:?}", e))?;

        let igp_tx_hash: [u8; 32] = hex::decode(&self.igp_utxo.tx_hash)?
            .try_into()
            .map_err(|_| anyhow!("Invalid IGP tx hash"))?;
        let collateral_tx_hash: [u8; 32] = hex::decode(&collateral_utxo.tx_hash)?
            .try_into()
            .map_err(|_| anyhow!("Invalid collateral tx hash"))?;
        let fee_tx_hash: [u8; 32] = hex::decode(&fee_utxo.tx_hash)?
            .try_into()
            .map_err(|_| anyhow!("Invalid fee tx hash"))?;
        let policy_id_bytes: [u8; 28] = hex::decode(&self.policy_id)?
            .try_into()
            .map_err(|_| anyhow!("Invalid policy ID"))?;
        let signer_hash: [u8; 28] = signer_pkh
            .to_vec()
            .try_into()
            .map_err(|_| anyhow!("Invalid signer hash"))?;

        let asset_name_bytes = self.get_state_nft_asset_name()?;

        // Build IGP continuation output with new datum and state NFT
        let igp_output = Output::new(igp_address, new_igp_lovelace)
            .set_inline_datum(new_datum_cbor)
            .add_asset(Hash::new(policy_id_bytes), asset_name_bytes, 1)
            .map_err(|e| anyhow!("Failed to add state NFT: {:?}", e))?;

        // Calculate change
        let fee_estimate = 2_000_000u64;
        let fee_input_value = if fee_utxo.tx_hash == self.igp_utxo.tx_hash
            && fee_utxo.output_index == self.igp_utxo.output_index
        {
            // Fee comes from IGP UTXO adjustment, no separate change
            0
        } else {
            fee_utxo.lovelace
        };

        // Build staging transaction
        let mut staging = StagingTransaction::new()
            // IGP script input
            .input(Input::new(
                Hash::new(igp_tx_hash),
                self.igp_utxo.output_index as u64,
            ))
            // Collateral
            .collateral_input(Input::new(
                Hash::new(collateral_tx_hash),
                collateral_utxo.output_index as u64,
            ))
            // IGP continuation output
            .output(igp_output)
            // Spend redeemer for IGP input
            .add_spend_redeemer(
                Input::new(Hash::new(igp_tx_hash), self.igp_utxo.output_index as u64),
                redeemer_cbor,
                Some(ExUnits {
                    mem: 5_000_000,
                    steps: 2_000_000_000,
                }),
            )
            // IGP script
            .script(ScriptKind::PlutusV3, igp_script_bytes)
            // Cost model for script data hash
            .language_view(ScriptKind::PlutusV3, cost_model)
            // Required signer
            .disclosed_signer(Hash::new(signer_hash))
            // Fee and validity
            .fee(fee_estimate)
            .invalid_from_slot(validity_end)
            .network_id(ctx.network_id());

        // Add fee input if separate from IGP
        if fee_utxo.tx_hash != self.igp_utxo.tx_hash
            || fee_utxo.output_index != self.igp_utxo.output_index
        {
            staging = staging.input(Input::new(
                Hash::new(fee_tx_hash),
                fee_utxo.output_index as u64,
            ));
        }

        // Add additional output (e.g., beneficiary payment for claim)
        if let Some(output) = additional_output {
            staging = staging.output(output);
        }

        // Add change output if significant
        let change = fee_input_value.saturating_sub(fee_estimate);
        if change > 1_500_000 {
            staging = staging.output(Output::new(payer_addr, change));
        }

        // Build the transaction
        let tx = staging
            .build_conway_raw()
            .map_err(|e| anyhow!("Failed to build transaction: {:?}", e))?;

        println!("  TX Hash: {}", hex::encode(&tx.tx_hash.0));

        // Sign the transaction
        println!("{}", "Signing transaction...".cyan());
        let tx_hash_bytes: &[u8] = &tx.tx_hash.0;
        let signature = self.keypair.sign(tx_hash_bytes);
        let signed_tx = tx
            .add_signature(self.keypair.pallas_public_key().clone(), signature)
            .map_err(|e| anyhow!("Failed to sign transaction: {:?}", e))?;

        // Submit the transaction
        println!("{}", "Submitting transaction...".cyan());
        let tx_hash = self.client.submit_tx(&signed_tx.tx_bytes.0).await?;

        Ok(tx_hash)
    }
}

#[derive(Args)]
pub struct IgpArgs {
    #[command(subcommand)]
    command: IgpCommands,
}

#[derive(Subcommand)]
enum IgpCommands {
    /// Show IGP state and configuration
    Show {
        /// IGP state NFT policy ID (defaults to deployment info)
        #[arg(long)]
        igp_policy: Option<String>,
    },

    /// Quote gas payment for a destination
    Quote {
        /// Destination domain ID
        #[arg(long)]
        destination: u32,

        /// Gas amount to quote (uses default_gas_limit if not provided)
        #[arg(long)]
        gas_amount: Option<u64>,

        /// IGP state NFT policy ID (defaults to deployment info)
        #[arg(long)]
        igp_policy: Option<String>,
    },

    /// Set gas oracle configuration for a domain (owner only)
    SetOracle {
        /// Destination domain ID
        #[arg(long)]
        domain: u32,

        /// Gas price in destination native units (e.g., wei for EVM)
        #[arg(long)]
        gas_price: u64,

        /// Token exchange rate (ADA to destination token, scaled)
        #[arg(long)]
        exchange_rate: u64,

        /// IGP state NFT policy ID (defaults to deployment info)
        #[arg(long)]
        igp_policy: Option<String>,

        /// Dry run - show what would be done without submitting
        #[arg(long)]
        dry_run: bool,
    },

    /// Pay for gas for a message
    PayForGas {
        /// Message ID (32 bytes hex, with or without 0x prefix)
        #[arg(long)]
        message_id: String,

        /// Destination domain ID
        #[arg(long)]
        destination: u32,

        /// Gas amount to pay for
        #[arg(long)]
        gas_amount: u64,

        /// IGP state NFT policy ID (defaults to deployment info)
        #[arg(long)]
        igp_policy: Option<String>,

        /// Dry run - show what would be done without submitting
        #[arg(long)]
        dry_run: bool,
    },

    /// Claim accumulated fees (beneficiary only)
    Claim {
        /// Amount to claim in lovelace
        #[arg(long)]
        amount: u64,

        /// IGP state NFT policy ID (defaults to deployment info)
        #[arg(long)]
        igp_policy: Option<String>,

        /// Dry run - show what would be done without submitting
        #[arg(long)]
        dry_run: bool,
    },
}

pub async fn execute(ctx: &CliContext, args: IgpArgs) -> Result<()> {
    match args.command {
        IgpCommands::Show { igp_policy } => show_igp(ctx, igp_policy).await,
        IgpCommands::Quote {
            destination,
            gas_amount,
            igp_policy,
        } => quote_gas(ctx, destination, gas_amount, igp_policy).await,
        IgpCommands::SetOracle {
            domain,
            gas_price,
            exchange_rate,
            igp_policy,
            dry_run,
        } => set_oracle(ctx, domain, gas_price, exchange_rate, igp_policy, dry_run).await,
        IgpCommands::PayForGas {
            message_id,
            destination,
            gas_amount,
            igp_policy,
            dry_run,
        } => pay_for_gas(ctx, &message_id, destination, gas_amount, igp_policy, dry_run).await,
        IgpCommands::Claim {
            amount,
            igp_policy,
            dry_run,
        } => claim_fees(ctx, amount, igp_policy, dry_run).await,
    }
}

async fn show_igp(ctx: &CliContext, igp_policy: Option<String>) -> Result<()> {
    println!("{}", "IGP State".cyan());

    let policy_id = get_igp_policy(ctx, igp_policy)?;
    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    // Find IGP UTXO by state NFT
    let igp_utxo = client
        .find_utxo_by_asset(&policy_id, "")
        .await?
        .ok_or_else(|| anyhow!("IGP UTXO not found with policy {}", policy_id))?;

    println!("\n{}", "IGP UTXO:".green());
    println!("  TX: {}#{}", igp_utxo.tx_hash, igp_utxo.output_index);
    println!("  Address: {}", igp_utxo.address);
    println!(
        "  Balance: {} ADA ({} lovelace)",
        igp_utxo.lovelace as f64 / 1_000_000.0,
        igp_utxo.lovelace
    );

    // Parse the datum
    let datum = igp_utxo
        .inline_datum
        .as_ref()
        .ok_or_else(|| anyhow!("IGP UTXO has no inline datum"))?;

    match parse_igp_datum(datum) {
        Ok((owner, beneficiary, gas_oracles, default_gas_limit)) => {
            println!("\n{}", "Configuration:".green());
            println!("  Owner: {}", hex::encode(&owner));
            println!("  Beneficiary: {}", hex::encode(&beneficiary));
            println!("  Default Gas Limit: {}", default_gas_limit);

            println!("\n{}", "Gas Oracles:".green());
            if gas_oracles.is_empty() {
                println!("  (none configured)");
            } else {
                for (domain, gas_price, exchange_rate) in &gas_oracles {
                    println!("  Domain {}:", domain);
                    println!("    Gas Price: {}", gas_price);
                    println!("    Exchange Rate: {}", exchange_rate);
                }
            }

            // Show claimable balance
            let min_utxo = 5_000_000u64;
            let claimable = igp_utxo.lovelace.saturating_sub(min_utxo);
            println!("\n{}", "Claimable Fees:".green());
            println!(
                "  {} ADA ({} lovelace)",
                claimable as f64 / 1_000_000.0,
                claimable
            );
        }
        Err(e) => {
            println!("\n{}", "Raw Datum:".yellow());
            println!("{}", serde_json::to_string_pretty(datum)?);
            println!("\n{}", format!("Could not parse datum: {}", e).yellow());
        }
    }

    Ok(())
}

async fn quote_gas(
    ctx: &CliContext,
    destination: u32,
    gas_amount: Option<u64>,
    igp_policy: Option<String>,
) -> Result<()> {
    println!("{}", "IGP Gas Quote".cyan());

    let policy_id = get_igp_policy(ctx, igp_policy)?;
    let api_key = ctx.require_api_key()?;
    let client = BlockfrostClient::new(ctx.blockfrost_url(), api_key);

    // Find IGP UTXO by state NFT
    let igp_utxo = client
        .find_utxo_by_asset(&policy_id, "")
        .await?
        .ok_or_else(|| anyhow!("IGP UTXO not found with policy {}", policy_id))?;

    // Parse the datum
    let datum = igp_utxo
        .inline_datum
        .as_ref()
        .ok_or_else(|| anyhow!("IGP UTXO has no inline datum"))?;

    let (_, _, gas_oracles, default_gas_limit) = parse_igp_datum(datum)?;

    // Determine effective gas amount
    let effective_gas = gas_amount.unwrap_or(default_gas_limit);

    // Find oracle for destination
    let oracle = gas_oracles.iter().find(|(d, _, _)| *d == destination);

    println!("\n{}", format!("Quote for destination {}:", destination).green());
    println!("  Gas amount: {}", format_number(effective_gas));

    let (gas_price, exchange_rate, required_lovelace) = match oracle {
        Some((_, gp, er)) => {
            let lovelace = calculate_gas_payment(effective_gas, *gp, *er);
            (*gp, *er, lovelace)
        }
        None => {
            // Use default values (same as contract)
            let default_gas_price = 1u64;
            let default_exchange_rate = 1_000_000u64;
            let lovelace = calculate_gas_payment(effective_gas, default_gas_price, default_exchange_rate);
            println!(
                "  {}",
                "Warning: No oracle configured for this destination, using defaults".yellow()
            );
            (default_gas_price, default_exchange_rate, lovelace)
        }
    };

    println!("  Gas price: {}", format_number(gas_price));
    println!("  Exchange rate: {}", format_number(exchange_rate));
    println!(
        "\n{} {} ADA ({} lovelace)",
        "Required payment:".green().bold(),
        required_lovelace as f64 / 1_000_000.0,
        format_number(required_lovelace)
    );

    Ok(())
}

async fn set_oracle(
    ctx: &CliContext,
    domain: u32,
    gas_price: u64,
    exchange_rate: u64,
    igp_policy: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Setting IGP Gas Oracle...".cyan());
    println!("  Domain: {}", domain);
    println!("  Gas Price: {}", format_number(gas_price));
    println!("  Exchange Rate: {}", format_number(exchange_rate));

    // Validate inputs
    if gas_price == 0 {
        return Err(anyhow!("Gas price must be greater than 0"));
    }
    if exchange_rate == 0 {
        return Err(anyhow!("Exchange rate must be greater than 0"));
    }

    // Initialize shared context
    let igp_ctx = IgpTxContext::new(ctx, igp_policy).await?;
    println!("  IGP Policy: {}", igp_ctx.policy_id);
    println!("  Payer: {}", igp_ctx.payer_address);

    igp_ctx.print_igp_utxo_info();
    println!("  Owner: {}", hex::encode(&igp_ctx.owner));

    // Verify we are the owner
    if igp_ctx.owner != igp_ctx.payer_pkh {
        return Err(anyhow!(
            "Signing key does not match IGP owner. Expected: {}, Got: {}",
            hex::encode(&igp_ctx.owner),
            hex::encode(&igp_ctx.payer_pkh)
        ));
    }

    // Update gas oracles (upsert)
    let mut gas_oracles = igp_ctx.gas_oracles.clone();
    let mut found = false;
    for oracle in &mut gas_oracles {
        if oracle.0 == domain {
            oracle.1 = gas_price;
            oracle.2 = exchange_rate;
            found = true;
            break;
        }
    }
    if !found {
        gas_oracles.push((domain, gas_price, exchange_rate));
    }

    // Build new datum
    let new_datum_cbor = igp_ctx.build_new_datum(Some(&gas_oracles))?;

    println!("\n{}", "New IGP Datum:".green());
    println!("  Gas oracles: {} configured", gas_oracles.len());
    for (d, gp, er) in &gas_oracles {
        println!("    Domain {}: gas_price={}, exchange_rate={}", d, gp, er);
    }
    println!(
        "  Datum CBOR: {}...",
        &hex::encode(&new_datum_cbor)[..64.min(new_datum_cbor.len() * 2)]
    );

    // Build SetGasOracle redeemer
    let redeemer = build_set_gas_oracle_redeemer(domain, gas_price, exchange_rate);
    let redeemer_cbor = pallas_codec::minicbor::to_vec(&redeemer)
        .map_err(|e| anyhow!("Failed to encode redeemer: {:?}", e))?;
    println!("\n{}", "SetGasOracle Redeemer:".green());
    println!("  CBOR: {}", hex::encode(&redeemer_cbor));

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        println!("\nTo update IGP, build a transaction that:");
        println!(
            "1. Spends IGP UTXO: {}#{}",
            igp_ctx.igp_utxo.tx_hash, igp_ctx.igp_utxo.output_index
        );
        println!("2. Uses SetGasOracle redeemer: {}", hex::encode(&redeemer_cbor));
        println!("3. Creates new IGP UTXO with updated datum");
        println!("4. Requires owner signature: {}", hex::encode(&igp_ctx.owner));
        return Ok(());
    }

    // Build and submit the transaction
    println!("\n{}", "Building transaction...".cyan());

    // Get payer UTXOs for fees and collateral
    let payer_utxos = igp_ctx.client.get_utxos(&igp_ctx.payer_address).await?;
    if payer_utxos.is_empty() {
        return Err(anyhow!("No UTXOs found for payer address"));
    }

    let collateral_utxo = igp_ctx.find_collateral_utxo(&payer_utxos)?;
    let fee_utxo = igp_ctx.find_fee_utxo(&payer_utxos, collateral_utxo, 5_000_000);

    println!(
        "  Collateral: {}#{}",
        collateral_utxo.tx_hash, collateral_utxo.output_index
    );
    println!("  Fee input: {}#{}", fee_utxo.tx_hash, fee_utxo.output_index);

    let tx_hash = igp_ctx
        .build_sign_submit(
            ctx,
            new_datum_cbor,
            redeemer_cbor,
            igp_ctx.igp_utxo.lovelace, // Same lovelace - no payment change
            None,                       // No additional output
            fee_utxo,
            collateral_utxo,
            &igp_ctx.owner,
        )
        .await?;

    println!("\n{}", "SUCCESS!".green().bold());
    println!("  Transaction Hash: {}", tx_hash);
    println!("  Explorer: {}", ctx.explorer_tx_url(&tx_hash));
    println!("\n  Domain: {}", domain);
    println!("  Gas Price: {}", format_number(gas_price));
    println!("  Exchange Rate: {}", format_number(exchange_rate));

    Ok(())
}

async fn pay_for_gas(
    ctx: &CliContext,
    message_id: &str,
    destination: u32,
    gas_amount: u64,
    igp_policy: Option<String>,
    dry_run: bool,
) -> Result<()> {
    println!("{}", "Paying for Gas...".cyan());

    // Parse and validate message ID (32 bytes)
    let message_id_clean = message_id.strip_prefix("0x").unwrap_or(message_id);
    let message_id_bytes =
        hex::decode(message_id_clean).map_err(|_| anyhow!("Invalid message ID hex"))?;
    if message_id_bytes.len() != 32 {
        return Err(anyhow!(
            "Message ID must be 32 bytes, got {}",
            message_id_bytes.len()
        ));
    }

    println!("  Message ID: 0x{}", message_id_clean);
    println!("  Destination: {}", destination);
    println!("  Gas Amount: {}", format_number(gas_amount));

    // Initialize shared context
    let igp_ctx = IgpTxContext::new(ctx, igp_policy).await?;
    println!("  IGP Policy: {}", igp_ctx.policy_id);
    println!("  Payer: {}", igp_ctx.payer_address);

    igp_ctx.print_igp_utxo_info();

    // Calculate required payment
    let effective_gas = if gas_amount > 0 {
        gas_amount
    } else {
        igp_ctx.default_gas_limit
    };

    let oracle = igp_ctx
        .gas_oracles
        .iter()
        .find(|(d, _, _)| *d == destination);
    let (gas_price, exchange_rate, required_lovelace) = match oracle {
        Some((_, gp, er)) => {
            let lovelace = calculate_gas_payment(effective_gas, *gp, *er);
            (*gp, *er, lovelace)
        }
        None => {
            let default_gas_price = 1u64;
            let default_exchange_rate = 1_000_000u64;
            let lovelace =
                calculate_gas_payment(effective_gas, default_gas_price, default_exchange_rate);
            println!(
                "  {}",
                "Warning: No oracle configured for this destination, using defaults".yellow()
            );
            (default_gas_price, default_exchange_rate, lovelace)
        }
    };

    println!("\n{}", "Payment Calculation:".green());
    println!("  Gas Amount: {}", format_number(effective_gas));
    println!("  Gas Price: {}", format_number(gas_price));
    println!("  Exchange Rate: {}", format_number(exchange_rate));
    println!(
        "  Required Payment: {} ADA ({} lovelace)",
        required_lovelace as f64 / 1_000_000.0,
        format_number(required_lovelace)
    );

    // Build new datum (unchanged - PayForGas doesn't modify datum)
    let new_datum_cbor = igp_ctx.build_new_datum(None)?;

    // Build PayForGas redeemer
    let redeemer = build_pay_for_gas_redeemer(&message_id_bytes, destination, effective_gas);
    let redeemer_cbor = pallas_codec::minicbor::to_vec(&redeemer)
        .map_err(|e| anyhow!("Failed to encode redeemer: {:?}", e))?;
    println!("\n{}", "PayForGas Redeemer:".green());
    println!("  CBOR: {}", hex::encode(&redeemer_cbor));

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        println!("\nTo pay for gas, build a transaction that:");
        println!(
            "1. Spends IGP UTXO: {}#{}",
            igp_ctx.igp_utxo.tx_hash, igp_ctx.igp_utxo.output_index
        );
        println!("2. Uses PayForGas redeemer");
        println!(
            "3. Creates new IGP UTXO with {} additional lovelace",
            required_lovelace
        );
        return Ok(());
    }

    // Build and submit the transaction
    println!("\n{}", "Building transaction...".cyan());

    // Get payer UTXOs for fees, collateral, and payment
    let payer_utxos = igp_ctx.client.get_utxos(&igp_ctx.payer_address).await?;
    if payer_utxos.is_empty() {
        return Err(anyhow!("No UTXOs found for payer address"));
    }

    let collateral_utxo = igp_ctx.find_collateral_utxo(&payer_utxos)?;

    // Find payment UTXO (need enough for payment + fees)
    let required_input = required_lovelace + 3_000_000;
    let payment_utxo = payer_utxos
        .iter()
        .find(|u| {
            u.lovelace >= required_input
                && u.assets.is_empty()
                && (u.tx_hash != collateral_utxo.tx_hash
                    || u.output_index != collateral_utxo.output_index)
        })
        .ok_or_else(|| {
            anyhow!(
                "No suitable payment UTXO (need {} lovelace + fees)",
                required_lovelace
            )
        })?;

    println!(
        "  Collateral: {}#{}",
        collateral_utxo.tx_hash, collateral_utxo.output_index
    );
    println!(
        "  Payment input: {}#{}",
        payment_utxo.tx_hash, payment_utxo.output_index
    );

    // New IGP output value = old value + payment
    let new_igp_lovelace = igp_ctx.igp_utxo.lovelace + required_lovelace;

    let tx_hash = igp_ctx
        .build_sign_submit(
            ctx,
            new_datum_cbor,
            redeemer_cbor,
            new_igp_lovelace,
            None,
            payment_utxo,
            collateral_utxo,
            &igp_ctx.payer_pkh,
        )
        .await?;

    println!("\n{}", "SUCCESS!".green().bold());
    println!("  Transaction Hash: {}", tx_hash);
    println!("  Explorer: {}", ctx.explorer_tx_url(&tx_hash));
    println!("\n  Message ID: 0x{}", message_id_clean);
    println!("  Destination: {}", destination);
    println!(
        "  Payment: {} ADA ({} lovelace)",
        required_lovelace as f64 / 1_000_000.0,
        format_number(required_lovelace)
    );

    Ok(())
}

async fn claim_fees(
    ctx: &CliContext,
    amount: u64,
    igp_policy: Option<String>,
    dry_run: bool,
) -> Result<()> {
    // Validate amount
    if amount == 0 {
        return Err(anyhow!("Claim amount must be greater than 0"));
    }

    println!("{}", "Claiming IGP Fees...".cyan());
    println!(
        "  Amount: {} ADA ({} lovelace)",
        amount as f64 / 1_000_000.0,
        format_number(amount)
    );

    // Initialize shared context
    let igp_ctx = IgpTxContext::new(ctx, igp_policy).await?;
    println!("  IGP Policy: {}", igp_ctx.policy_id);
    println!("  Claimer: {}", igp_ctx.payer_address);

    igp_ctx.print_igp_utxo_info();
    println!("  Beneficiary: {}", hex::encode(&igp_ctx.beneficiary));

    // Verify we are the beneficiary
    if igp_ctx.beneficiary != igp_ctx.payer_pkh {
        return Err(anyhow!(
            "Signing key does not match IGP beneficiary. Expected: {}, Got: {}",
            hex::encode(&igp_ctx.beneficiary),
            hex::encode(&igp_ctx.payer_pkh)
        ));
    }

    // Calculate claimable amount (current balance - min UTXO)
    let min_utxo = 5_000_000u64;
    let claimable = igp_ctx.igp_utxo.lovelace.saturating_sub(min_utxo);
    println!(
        "  Claimable: {} ADA ({} lovelace)",
        claimable as f64 / 1_000_000.0,
        format_number(claimable)
    );

    if amount > claimable {
        return Err(anyhow!(
            "Cannot claim {} lovelace, only {} available",
            amount,
            claimable
        ));
    }

    // Build new datum (unchanged - Claim doesn't modify datum)
    let new_datum_cbor = igp_ctx.build_new_datum(None)?;

    // Build Claim redeemer
    let redeemer = build_claim_redeemer(amount);
    let redeemer_cbor = pallas_codec::minicbor::to_vec(&redeemer)
        .map_err(|e| anyhow!("Failed to encode redeemer: {:?}", e))?;
    println!("\n{}", "Claim Redeemer:".green());
    println!("  CBOR: {}", hex::encode(&redeemer_cbor));

    if dry_run {
        println!("\n{}", "[Dry run - not submitting transaction]".yellow());
        println!("\nTo claim fees, build a transaction that:");
        println!(
            "1. Spends IGP UTXO: {}#{}",
            igp_ctx.igp_utxo.tx_hash, igp_ctx.igp_utxo.output_index
        );
        println!("2. Uses Claim redeemer");
        println!("3. Creates new IGP UTXO with {} less lovelace", amount);
        println!("4. Sends {} lovelace to beneficiary", amount);
        return Ok(());
    }

    // Build and submit the transaction
    println!("\n{}", "Building transaction...".cyan());

    // Get payer UTXOs for fees and collateral
    let payer_utxos = igp_ctx.client.get_utxos(&igp_ctx.payer_address).await?;
    if payer_utxos.is_empty() {
        return Err(anyhow!("No UTXOs found for payer address"));
    }

    let collateral_utxo = igp_ctx.find_collateral_utxo(&payer_utxos)?;
    let fee_utxo = igp_ctx.find_fee_utxo(&payer_utxos, collateral_utxo, 3_000_000);

    println!(
        "  Collateral: {}#{}",
        collateral_utxo.tx_hash, collateral_utxo.output_index
    );
    println!("  Fee input: {}#{}", fee_utxo.tx_hash, fee_utxo.output_index);

    // New IGP output value = old value - claimed amount
    let new_igp_lovelace = igp_ctx.igp_utxo.lovelace - amount;

    // Build beneficiary output (receives the claimed amount)
    let payer_addr = pallas_addresses::Address::from_bech32(&igp_ctx.payer_address)
        .map_err(|e| anyhow!("Invalid payer address: {:?}", e))?;
    let beneficiary_output = Output::new(payer_addr, amount);

    let tx_hash = igp_ctx
        .build_sign_submit(
            ctx,
            new_datum_cbor,
            redeemer_cbor,
            new_igp_lovelace,
            Some(beneficiary_output),
            fee_utxo,
            collateral_utxo,
            &igp_ctx.beneficiary,
        )
        .await?;

    println!("\n{}", "SUCCESS!".green().bold());
    println!("  Transaction Hash: {}", tx_hash);
    println!("  Explorer: {}", ctx.explorer_tx_url(&tx_hash));
    println!(
        "\n  Claimed: {} ADA ({} lovelace)",
        amount as f64 / 1_000_000.0,
        format_number(amount)
    );
    println!(
        "  New IGP Balance: {} ADA ({} lovelace)",
        new_igp_lovelace as f64 / 1_000_000.0,
        format_number(new_igp_lovelace)
    );

    Ok(())
}

/// Build PayForGas redeemer
/// Structure: Constr 0 [message_id: ByteArray, destination: Int, gas_amount: Int]
fn build_pay_for_gas_redeemer(message_id: &[u8], destination: u32, gas_amount: u64) -> PlutusData {
    use pallas_primitives::conway::BoundedBytes;
    PlutusData::Constr(Constr {
        tag: 121, // Constr 0 (PayForGas)
        any_constructor: None,
        fields: MaybeIndefArray::Def(vec![
            PlutusData::BoundedBytes(BoundedBytes::from(message_id.to_vec())),
            PlutusData::BigInt(BigInt::Int((destination as i64).into())),
            PlutusData::BigInt(BigInt::Int((gas_amount as i64).into())),
        ]),
    })
}

/// Build Claim redeemer
/// Structure: Constr 1 [amount: Int]
fn build_claim_redeemer(amount: u64) -> PlutusData {
    PlutusData::Constr(Constr {
        tag: 122, // Constr 1 (Claim)
        any_constructor: None,
        fields: MaybeIndefArray::Def(vec![PlutusData::BigInt(BigInt::Int(
            (amount as i64).into(),
        ))]),
    })
}

/// Build SetGasOracle redeemer
/// Structure: Constr 2 [domain: Int, config: Constr 0 [gas_price: Int, exchange_rate: Int]]
/// IGP redeemers: PayForGas=0, Claim=1, SetGasOracle=2
fn build_set_gas_oracle_redeemer(domain: u32, gas_price: u64, exchange_rate: u64) -> PlutusData {
    PlutusData::Constr(Constr {
        tag: 123, // Constr 2 (SetGasOracle)
        any_constructor: None,
        fields: MaybeIndefArray::Def(vec![
            PlutusData::BigInt(BigInt::Int((domain as i64).into())),
            PlutusData::Constr(Constr {
                tag: 121, // Constr 0 (GasOracleConfig)
                any_constructor: None,
                fields: MaybeIndefArray::Def(vec![
                    PlutusData::BigInt(BigInt::Int((gas_price as i64).into())),
                    PlutusData::BigInt(BigInt::Int((exchange_rate as i64).into())),
                ]),
            }),
        ]),
    })
}

/// Calculate gas payment in lovelace
/// Formula: gas_amount * gas_price * token_exchange_rate / 1_000_000_000_000
fn calculate_gas_payment(gas_amount: u64, gas_price: u64, exchange_rate: u64) -> u64 {
    // Use u128 to avoid overflow during multiplication
    let numerator = gas_amount as u128 * gas_price as u128 * exchange_rate as u128;
    (numerator / 1_000_000_000_000u128) as u64
}

/// Format a number with thousand separators
fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

fn get_igp_policy(ctx: &CliContext, igp_policy: Option<String>) -> Result<String> {
    if let Some(p) = igp_policy {
        return Ok(p);
    }

    let deployment = ctx.load_deployment_info()?;
    deployment
        .igp
        .and_then(|i| i.state_nft_policy)
        .ok_or_else(|| anyhow!("IGP policy not found. Use --igp-policy or initialize IGP first"))
}

/// Parse IGP datum, returns (owner, beneficiary, gas_oracles, default_gas_limit)
fn parse_igp_datum(
    datum: &serde_json::Value,
) -> Result<(Vec<u8>, Vec<u8>, Vec<(u32, u64, u64)>, u64)> {
    // Check if datum is raw CBOR hex
    if let Some(hex_str) = datum.as_str() {
        let decoded = crate::utils::cbor::decode_plutus_datum(hex_str)?;
        return parse_igp_datum(&decoded);
    }

    let fields = datum
        .get("fields")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow!("Invalid IGP datum structure"))?;

    if fields.len() < 4 {
        return Err(anyhow!("IGP datum must have 4 fields"));
    }

    // owner (field 0)
    let owner = hex::decode(
        fields[0]
            .get("bytes")
            .and_then(|b| b.as_str())
            .ok_or_else(|| anyhow!("Invalid owner"))?,
    )?;

    // beneficiary (field 1)
    let beneficiary = hex::decode(
        fields[1]
            .get("bytes")
            .and_then(|b| b.as_str())
            .ok_or_else(|| anyhow!("Invalid beneficiary"))?,
    )?;

    // gas_oracles (field 2)
    let mut gas_oracles = Vec::new();
    if let Some(list) = fields[2].get("list").and_then(|l| l.as_array()) {
        for entry in list {
            let items: Vec<&serde_json::Value> = entry
                .get("list")
                .and_then(|l| l.as_array())
                .map(|a| a.iter().collect())
                .unwrap_or_default();

            if items.len() >= 2 {
                let domain = items[0].get("int").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
                let oracle_fields = items[1].get("fields").and_then(|f| f.as_array());
                if let Some(of) = oracle_fields {
                    if of.len() >= 2 {
                        let gas_price = of[0].get("int").and_then(|i| i.as_u64()).unwrap_or(0);
                        let exchange_rate = of[1].get("int").and_then(|i| i.as_u64()).unwrap_or(0);
                        gas_oracles.push((domain, gas_price, exchange_rate));
                    }
                }
            }
        }
    }

    // default_gas_limit (field 3)
    let default_gas_limit = fields[3].get("int").and_then(|i| i.as_u64()).unwrap_or(0);

    Ok((owner, beneficiary, gas_oracles, default_gas_limit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_igp_datum_basic() {
        let datum = json!({
            "constructor": 0,
            "fields": [
                {"bytes": "1212a023380020f8c7b94b831e457b9ee65f009df9d1d588430dcc89"},
                {"bytes": "1212a023380020f8c7b94b831e457b9ee65f009df9d1d588430dcc89"},
                {"list": []},
                {"int": 200000}
            ]
        });

        let (owner, beneficiary, gas_oracles, default_gas_limit) =
            parse_igp_datum(&datum).unwrap();

        assert_eq!(
            hex::encode(&owner),
            "1212a023380020f8c7b94b831e457b9ee65f009df9d1d588430dcc89"
        );
        assert_eq!(
            hex::encode(&beneficiary),
            "1212a023380020f8c7b94b831e457b9ee65f009df9d1d588430dcc89"
        );
        assert!(gas_oracles.is_empty());
        assert_eq!(default_gas_limit, 200000);
    }

    #[test]
    fn test_parse_igp_datum_with_oracles() {
        let datum = json!({
            "constructor": 0,
            "fields": [
                {"bytes": "aabbccdd"},
                {"bytes": "11223344"},
                {
                    "list": [
                        {
                            "list": [
                                {"int": 43113},
                                {
                                    "constructor": 0,
                                    "fields": [
                                        {"int": 25000000000_u64},
                                        {"int": 1000000}
                                    ]
                                }
                            ]
                        }
                    ]
                },
                {"int": 150000}
            ]
        });

        let (owner, beneficiary, gas_oracles, default_gas_limit) =
            parse_igp_datum(&datum).unwrap();

        assert_eq!(hex::encode(&owner), "aabbccdd");
        assert_eq!(hex::encode(&beneficiary), "11223344");
        assert_eq!(gas_oracles.len(), 1);
        assert_eq!(gas_oracles[0], (43113, 25000000000, 1000000));
        assert_eq!(default_gas_limit, 150000);
    }

    #[test]
    fn test_parse_igp_datum_multiple_oracles() {
        let datum = json!({
            "constructor": 0,
            "fields": [
                {"bytes": "aabbccdd"},
                {"bytes": "11223344"},
                {
                    "list": [
                        {
                            "list": [
                                {"int": 43113},
                                {
                                    "constructor": 0,
                                    "fields": [
                                        {"int": 25000000000_u64},
                                        {"int": 1000000}
                                    ]
                                }
                            ]
                        },
                        {
                            "list": [
                                {"int": 11155111},
                                {
                                    "constructor": 0,
                                    "fields": [
                                        {"int": 30000000000_u64},
                                        {"int": 1200000}
                                    ]
                                }
                            ]
                        }
                    ]
                },
                {"int": 200000}
            ]
        });

        let (_, _, gas_oracles, _) = parse_igp_datum(&datum).unwrap();

        assert_eq!(gas_oracles.len(), 2);
        assert_eq!(gas_oracles[0], (43113, 25000000000, 1000000));
        assert_eq!(gas_oracles[1], (11155111, 30000000000, 1200000));
    }

    #[test]
    fn test_parse_igp_datum_missing_fields() {
        let datum = json!({
            "constructor": 0,
            "fields": [
                {"bytes": "aabbccdd"},
                {"bytes": "11223344"}
            ]
        });

        let result = parse_igp_datum(&datum);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must have 4 fields"));
    }

    #[test]
    fn test_parse_igp_datum_invalid_structure() {
        let datum = json!({
            "invalid": "structure"
        });

        let result = parse_igp_datum(&datum);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid IGP datum structure"));
    }

    #[test]
    fn test_parse_igp_datum_invalid_owner() {
        let datum = json!({
            "constructor": 0,
            "fields": [
                {"int": 123},
                {"bytes": "11223344"},
                {"list": []},
                {"int": 200000}
            ]
        });

        let result = parse_igp_datum(&datum);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid owner"));
    }

    #[test]
    fn test_parse_igp_datum_invalid_beneficiary() {
        let datum = json!({
            "constructor": 0,
            "fields": [
                {"bytes": "aabbccdd"},
                {"int": 456},
                {"list": []},
                {"int": 200000}
            ]
        });

        let result = parse_igp_datum(&datum);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid beneficiary"));
    }

    #[test]
    fn test_parse_igp_datum_empty_oracle_list_items() {
        // Test with malformed oracle entries that should be skipped
        let datum = json!({
            "constructor": 0,
            "fields": [
                {"bytes": "aabbccdd"},
                {"bytes": "11223344"},
                {
                    "list": [
                        {"list": []},
                        {"list": [{"int": 1}]}
                    ]
                },
                {"int": 200000}
            ]
        });

        let (_, _, gas_oracles, _) = parse_igp_datum(&datum).unwrap();
        // Malformed entries should be skipped
        assert!(gas_oracles.is_empty());
    }

    // Tests for calculate_gas_payment
    #[test]
    fn test_calculate_gas_payment_basic() {
        // 200,000 gas * 25,000,000,000 gas_price * 1,000,000 exchange_rate / 1e12
        // = 200,000 * 25,000,000,000 * 1,000,000 / 1,000,000,000,000
        // = 5,000,000,000,000,000,000,000 / 1,000,000,000,000
        // = 5,000,000,000 lovelace = 5000 ADA
        let result = calculate_gas_payment(200_000, 25_000_000_000, 1_000_000);
        assert_eq!(result, 5_000_000_000);
    }

    #[test]
    fn test_calculate_gas_payment_with_defaults() {
        // Using default oracle values from contract: gas_price=1, exchange_rate=1,000,000
        // 200,000 * 1 * 1,000,000 / 1e12 = 200,000,000,000 / 1e12 = 0.2 (rounds to 0)
        let result = calculate_gas_payment(200_000, 1, 1_000_000);
        assert_eq!(result, 0);
    }

    #[test]
    fn test_calculate_gas_payment_large_values() {
        // Test with large values to ensure no overflow
        let result = calculate_gas_payment(1_000_000, 100_000_000_000, 2_000_000);
        // 1,000,000 * 100,000,000,000 * 2,000,000 / 1e12
        // = 200,000,000,000,000,000,000,000 / 1e12
        // = 200,000,000,000 lovelace
        assert_eq!(result, 200_000_000_000);
    }

    #[test]
    fn test_calculate_gas_payment_zero_gas() {
        let result = calculate_gas_payment(0, 25_000_000_000, 1_000_000);
        assert_eq!(result, 0);
    }

    #[test]
    fn test_calculate_gas_payment_different_rates() {
        // Sepolia example: 30 gwei gas price, 1.2x exchange rate
        // 200,000 * 30,000,000,000 * 1,200,000 / 1e12
        // = 7,200,000,000,000,000,000,000 / 1e12
        // = 7,200,000,000 lovelace = 7200 ADA
        let result = calculate_gas_payment(200_000, 30_000_000_000, 1_200_000);
        assert_eq!(result, 7_200_000_000);
    }

    // Tests for format_number
    #[test]
    fn test_format_number_small() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(1), "1");
        assert_eq!(format_number(12), "12");
        assert_eq!(format_number(123), "123");
    }

    #[test]
    fn test_format_number_thousands() {
        assert_eq!(format_number(1_000), "1,000");
        assert_eq!(format_number(12_345), "12,345");
        assert_eq!(format_number(123_456), "123,456");
    }

    #[test]
    fn test_format_number_millions() {
        assert_eq!(format_number(1_000_000), "1,000,000");
        assert_eq!(format_number(5_000_000), "5,000,000");
        assert_eq!(format_number(123_456_789), "123,456,789");
    }

    #[test]
    fn test_format_number_large() {
        assert_eq!(format_number(25_000_000_000), "25,000,000,000");
        assert_eq!(format_number(1_000_000_000_000), "1,000,000,000,000");
    }

    // Tests for build_set_gas_oracle_redeemer
    #[test]
    fn test_build_set_gas_oracle_redeemer() {
        let redeemer = build_set_gas_oracle_redeemer(43113, 25_000_000_000, 1_000_000);

        // Verify it's a Constr 2 (tag 123)
        match &redeemer {
            PlutusData::Constr(c) => {
                assert_eq!(c.tag, 123); // Constr 2

                // Should have 2 fields: domain and config
                match &c.fields {
                    MaybeIndefArray::Def(fields) => {
                        assert_eq!(fields.len(), 2);

                        // Second field: GasOracleConfig (Constr 0)
                        match &fields[1] {
                            PlutusData::Constr(config) => {
                                assert_eq!(config.tag, 121); // Constr 0
                            }
                            _ => panic!("Expected Constr for config"),
                        }
                    }
                    _ => panic!("Expected Def fields"),
                }
            }
            _ => panic!("Expected Constr"),
        }
    }

    #[test]
    fn test_build_set_gas_oracle_redeemer_encodes_correctly() {
        let redeemer = build_set_gas_oracle_redeemer(43113, 25_000_000_000, 1_000_000);
        let cbor = pallas_codec::minicbor::to_vec(&redeemer).unwrap();

        // Just verify it encodes without error and produces some bytes
        assert!(!cbor.is_empty());
        // The CBOR should start with d8 7b (tag 123 = Constr 2)
        assert_eq!(cbor[0], 0xd8);
        assert_eq!(cbor[1], 0x7b);
    }

    // Tests for build_pay_for_gas_redeemer
    #[test]
    fn test_build_pay_for_gas_redeemer() {
        let message_id = [0u8; 32];
        let redeemer = build_pay_for_gas_redeemer(&message_id, 43113, 200_000);

        // Verify it's a Constr 0 (tag 121)
        match &redeemer {
            PlutusData::Constr(c) => {
                assert_eq!(c.tag, 121); // Constr 0 (PayForGas)

                match &c.fields {
                    MaybeIndefArray::Def(fields) => {
                        assert_eq!(fields.len(), 3); // message_id, destination, gas_amount
                    }
                    _ => panic!("Expected Def fields"),
                }
            }
            _ => panic!("Expected Constr"),
        }
    }

    #[test]
    fn test_build_pay_for_gas_redeemer_encodes_correctly() {
        let message_id = hex::decode(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        let redeemer = build_pay_for_gas_redeemer(&message_id, 43113, 200_000);
        let cbor = pallas_codec::minicbor::to_vec(&redeemer).unwrap();

        assert!(!cbor.is_empty());
        // The CBOR should start with d8 79 (tag 121 = Constr 0)
        assert_eq!(cbor[0], 0xd8);
        assert_eq!(cbor[1], 0x79);
    }

    // Tests for build_claim_redeemer
    #[test]
    fn test_build_claim_redeemer() {
        let redeemer = build_claim_redeemer(5_000_000);

        // Verify it's a Constr 1 (tag 122)
        match &redeemer {
            PlutusData::Constr(c) => {
                assert_eq!(c.tag, 122); // Constr 1 (Claim)

                match &c.fields {
                    MaybeIndefArray::Def(fields) => {
                        assert_eq!(fields.len(), 1); // amount
                    }
                    _ => panic!("Expected Def fields"),
                }
            }
            _ => panic!("Expected Constr"),
        }
    }

    #[test]
    fn test_build_claim_redeemer_encodes_correctly() {
        let redeemer = build_claim_redeemer(10_000_000);
        let cbor = pallas_codec::minicbor::to_vec(&redeemer).unwrap();

        assert!(!cbor.is_empty());
        // The CBOR should start with d8 7a (tag 122 = Constr 1)
        assert_eq!(cbor[0], 0xd8);
        assert_eq!(cbor[1], 0x7a);
    }
}

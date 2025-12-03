//! Hyperlane Cardano CLI
//!
//! A comprehensive CLI for deploying, initializing, and managing
//! Hyperlane smart contracts on Cardano.

mod commands;
mod utils;

use clap::{Parser, Subcommand};
use colored::Colorize;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use commands::{deploy, init, ism, mailbox, query, registry, tx, utxo, warp};

/// Hyperlane Cardano CLI - Deploy and manage Hyperlane on Cardano
#[derive(Parser)]
#[command(name = "hyperlane-cardano")]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Cardano network (mainnet, preprod, preview)
    #[arg(long, global = true, default_value = "preview", env = "CARDANO_NETWORK")]
    network: String,

    /// Blockfrost API key
    #[arg(long, global = true, env = "BLOCKFROST_API_KEY")]
    api_key: Option<String>,

    /// Path to signing key file
    #[arg(long, global = true, env = "CARDANO_SIGNING_KEY")]
    signing_key: Option<String>,

    /// Path to deployment directory
    #[arg(long, global = true, default_value = "./deployments")]
    deployments_dir: String,

    /// Path to contracts directory (with plutus.json)
    #[arg(long, global = true, default_value = "./contracts")]
    contracts_dir: String,

    /// Verbose output
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Deploy Hyperlane contracts (extract validators, compute hashes)
    Deploy(deploy::DeployArgs),

    /// Initialize contracts with state NFTs and initial datums
    Init(init::InitArgs),

    /// Manage Interchain Security Module (ISM) validators
    Ism(ism::IsmArgs),

    /// Manage Hyperlane Mailbox contract
    Mailbox(mailbox::MailboxArgs),

    /// Manage recipient registry
    Registry(registry::RegistryArgs),

    /// Manage warp routes (token bridges)
    Warp(warp::WarpArgs),

    /// Query contract state and UTXOs
    Query(query::QueryArgs),

    /// UTXO management utilities
    Utxo(utxo::UtxoArgs),

    /// Transaction building and submission
    Tx(tx::TxArgs),

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("info")
    };

    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(filter)
        .init();

    // Print banner
    println!(
        "{}",
        r#"
    __  __                      __
   / / / /_  ______  ___  _____/ /___ _____  ___
  / /_/ / / / / __ \/ _ \/ ___/ / __ `/ __ \/ _ \
 / __  / /_/ / /_/ /  __/ /  / / /_/ / / / /  __/
/_/ /_/\__, / .___/\___/_/  /_/\__,_/_/ /_/\___/
      /____/_/              Cardano CLI
"#
        .cyan()
    );

    // Create context with global options
    let ctx = utils::context::CliContext::new(
        &cli.network,
        cli.api_key.as_deref(),
        cli.signing_key.as_deref(),
        &cli.deployments_dir,
        &cli.contracts_dir,
    )?;

    // Execute command
    match cli.command {
        Commands::Deploy(args) => deploy::execute(&ctx, args).await,
        Commands::Init(args) => init::execute(&ctx, args).await,
        Commands::Ism(args) => ism::execute(&ctx, args).await,
        Commands::Mailbox(args) => mailbox::execute(&ctx, args).await,
        Commands::Registry(args) => registry::execute(&ctx, args).await,
        Commands::Warp(args) => warp::execute(&ctx, args).await,
        Commands::Query(args) => query::execute(&ctx, args).await,
        Commands::Utxo(args) => utxo::execute(&ctx, args).await,
        Commands::Tx(args) => tx::execute(&ctx, args).await,
        Commands::Completions { shell } => {
            use clap::CommandFactory;
            use clap_complete::generate;
            use std::io;

            let mut cmd = Cli::command();
            let name = cmd.get_name().to_string();
            generate(shell, &mut cmd, name, &mut io::stdout());
            Ok(())
        }
    }
}

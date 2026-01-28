use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use tt_cli::{Cli, Commands, Config};

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize tracing with verbose flag support
    let filter = if cli.verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::from_default_env()
    };
    // Use try_init to avoid panic if tracing is already initialized (e.g., in tests)
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();

    // Load configuration
    let config =
        Config::load_from(cli.config.as_deref()).context("failed to load configuration")?;

    tracing::debug!(?config, "loaded configuration");

    match cli.command {
        Some(Commands::Status) => {
            println!("Time tracker status: idle");
            println!("Database: {}", config.database_path.display());
        }
        None => {
            // No subcommand, show help
            use clap::CommandFactory;
            Cli::command().print_help()?;
            println!();
        }
    }

    Ok(())
}

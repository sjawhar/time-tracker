use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use tt_cli::{Cli, Commands, Config, commands};

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Fast path for ingest/export - skip config and tracing for performance
    // This is critical because these commands are called frequently:
    // - ingest: on every tmux focus change
    // - export: via SSH during sync
    if matches!(&cli.command, Some(Commands::Ingest(_) | Commands::Export)) {
        return match cli.command {
            Some(Commands::Ingest(args)) => commands::ingest::run(args),
            Some(Commands::Export) => commands::export::run(),
            _ => unreachable!(),
        };
    }

    // Normal path with config and tracing
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
        Some(Commands::Ingest(_) | Commands::Export) => {
            // Already handled above
            unreachable!();
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

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

    match cli.command {
        Some(Commands::Status) => {
            let config =
                Config::load_from(cli.config.as_deref()).context("failed to load configuration")?;
            tracing::debug!(?config, "loaded configuration");
            let mut stdout = std::io::stdout();
            tt_cli::commands::status::run(&mut stdout, &config)?;
        }
        Some(Commands::Ingest(args)) => {
            tt_cli::commands::ingest::run(args)?;
        }
        Some(Commands::Export(args)) => {
            tt_cli::commands::export::run(args)?;
        }
        Some(Commands::Events(_args)) => {
            let config =
                Config::load_from(cli.config.as_deref()).context("failed to load configuration")?;
            let mut stdout = std::io::stdout();
            tt_cli::commands::events::run(&mut stdout, &config)?;
        }
        Some(Commands::Import(args)) => {
            let config =
                Config::load_from(cli.config.as_deref()).context("failed to load configuration")?;
            let imported = tt_cli::commands::import::run(&args, &config)?;
            println!("Imported {imported} events");
        }
        Some(Commands::Sync(args)) => {
            let config =
                Config::load_from(cli.config.as_deref()).context("failed to load configuration")?;
            let report = tt_cli::commands::sync::run(&args, &config)?;
            println!("Synced {} events from {}", report.imported, report.remote);
        }
        Some(Commands::Report(args)) => {
            let config =
                Config::load_from(cli.config.as_deref()).context("failed to load configuration")?;
            let mut stdout = std::io::stdout();
            tt_cli::commands::report::run(&mut stdout, &args, &config)?;
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

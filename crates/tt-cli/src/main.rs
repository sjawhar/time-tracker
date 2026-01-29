use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use tt_cli::commands::ingest;
use tt_cli::{Cli, Commands, Config, IngestEvent};

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

    match &cli.command {
        Some(Commands::Ingest { event }) => {
            // Ingest command doesn't need config - optimize for startup time
            match event {
                IngestEvent::PaneFocus {
                    pane,
                    cwd,
                    session,
                    window,
                } => {
                    let written = ingest::ingest_pane_focus(pane, session, *window, cwd)?;
                    if written {
                        tracing::debug!("event ingested");
                    } else {
                        tracing::debug!("event debounced");
                    }
                }
            }
        }
        Some(Commands::Status) => {
            // Load configuration only when needed
            let config =
                Config::load_from(cli.config.as_deref()).context("failed to load configuration")?;
            tracing::debug!(?config, "loaded configuration");

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

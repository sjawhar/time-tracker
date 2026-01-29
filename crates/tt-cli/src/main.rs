use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use tt_cli::commands::{events, export, import, infer, ingest, recompute, report, status, sync};
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
        Some(Commands::Export) => {
            // Export doesn't need config - just reads files and outputs to stdout
            export::run()?;
        }
        Some(Commands::Import) => {
            // Import needs config for database path
            let config =
                Config::load_from(cli.config.as_deref()).context("failed to load configuration")?;
            tracing::debug!(?config, "loaded configuration");

            // Ensure parent directory exists
            if let Some(parent) = config.database_path.parent() {
                std::fs::create_dir_all(parent).context("failed to create database directory")?;
            }

            let db =
                tt_db::Database::open(&config.database_path).context("failed to open database")?;
            import::run(&db)?;
        }
        Some(Commands::Sync { remote }) => {
            // Sync needs config for database path
            let config =
                Config::load_from(cli.config.as_deref()).context("failed to load configuration")?;
            tracing::debug!(?config, "loaded configuration");

            // Ensure parent directory exists
            if let Some(parent) = config.database_path.parent() {
                std::fs::create_dir_all(parent).context("failed to create database directory")?;
            }

            let db =
                tt_db::Database::open(&config.database_path).context("failed to open database")?;
            sync::run(&db, remote)?;
        }
        Some(Commands::Events { after, before }) => {
            // Events command needs config for database path
            let config =
                Config::load_from(cli.config.as_deref()).context("failed to load configuration")?;
            tracing::debug!(?config, "loaded configuration");

            // Ensure parent directory exists
            if let Some(parent) = config.database_path.parent() {
                std::fs::create_dir_all(parent).context("failed to create database directory")?;
            }

            let db =
                tt_db::Database::open(&config.database_path).context("failed to open database")?;
            events::run(&db, after.as_deref(), before.as_deref())?;
        }
        Some(Commands::Status) => {
            // Status needs config for database path
            let config =
                Config::load_from(cli.config.as_deref()).context("failed to load configuration")?;
            tracing::debug!(?config, "loaded configuration");

            // Ensure parent directory exists
            if let Some(parent) = config.database_path.parent() {
                std::fs::create_dir_all(parent).context("failed to create database directory")?;
            }

            let db =
                tt_db::Database::open(&config.database_path).context("failed to open database")?;
            status::run(&db, &config.database_path)?;
        }
        Some(Commands::Infer { force }) => {
            // Infer needs config for database path
            let config =
                Config::load_from(cli.config.as_deref()).context("failed to load configuration")?;
            tracing::debug!(?config, "loaded configuration");

            // Ensure parent directory exists
            if let Some(parent) = config.database_path.parent() {
                std::fs::create_dir_all(parent).context("failed to create database directory")?;
            }

            let db =
                tt_db::Database::open(&config.database_path).context("failed to open database")?;
            infer::run(&db, *force)?;
        }
        Some(Commands::Recompute { force }) => {
            // Recompute needs config for database path
            let config =
                Config::load_from(cli.config.as_deref()).context("failed to load configuration")?;
            tracing::debug!(?config, "loaded configuration");

            // Ensure parent directory exists
            if let Some(parent) = config.database_path.parent() {
                std::fs::create_dir_all(parent).context("failed to create database directory")?;
            }

            let db =
                tt_db::Database::open(&config.database_path).context("failed to open database")?;
            recompute::run(&db, *force)?;
        }
        Some(Commands::Report {
            week: _,
            last_week,
            day,
            last_day,
            json,
        }) => {
            let config =
                Config::load_from(cli.config.as_deref()).context("failed to load configuration")?;
            tracing::debug!(?config, "loaded configuration");

            if let Some(parent) = config.database_path.parent() {
                std::fs::create_dir_all(parent).context("failed to create database directory")?;
            }

            let db =
                tt_db::Database::open(&config.database_path).context("failed to open database")?;

            // Determine period - default to Week
            let period = if *last_week {
                report::Period::LastWeek
            } else if *day {
                report::Period::Day
            } else if *last_day {
                report::Period::LastDay
            } else {
                // Default or explicit --week
                report::Period::Week
            };

            report::run(&db, period, *json)?;
        }
        Some(Commands::Week { json }) => {
            let config =
                Config::load_from(cli.config.as_deref()).context("failed to load configuration")?;
            tracing::debug!(?config, "loaded configuration");

            if let Some(parent) = config.database_path.parent() {
                std::fs::create_dir_all(parent).context("failed to create database directory")?;
            }

            let db =
                tt_db::Database::open(&config.database_path).context("failed to open database")?;
            report::run(&db, report::Period::Week, *json)?;
        }
        Some(Commands::Today { json }) => {
            let config =
                Config::load_from(cli.config.as_deref()).context("failed to load configuration")?;
            tracing::debug!(?config, "loaded configuration");

            if let Some(parent) = config.database_path.parent() {
                std::fs::create_dir_all(parent).context("failed to create database directory")?;
            }

            let db =
                tt_db::Database::open(&config.database_path).context("failed to open database")?;
            report::run(&db, report::Period::Day, *json)?;
        }
        Some(Commands::Yesterday { json }) => {
            let config =
                Config::load_from(cli.config.as_deref()).context("failed to load configuration")?;
            tracing::debug!(?config, "loaded configuration");

            if let Some(parent) = config.database_path.parent() {
                std::fs::create_dir_all(parent).context("failed to create database directory")?;
            }

            let db =
                tt_db::Database::open(&config.database_path).context("failed to open database")?;
            report::run(&db, report::Period::LastDay, *json)?;
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

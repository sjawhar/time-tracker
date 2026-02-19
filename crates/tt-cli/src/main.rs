use std::path::Path;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use tt_cli::commands::{context, export, import, ingest, init, recompute, report, status, streams, tag};
use tt_cli::{Cli, Commands, Config, IngestEvent, StreamsAction};

/// Load config and open database, ensuring the parent directory exists.
fn open_database(config_path: Option<&Path>) -> Result<(tt_db::Database, Config)> {
    let config = Config::load_from(config_path).context("failed to load configuration")?;
    tracing::debug!(?config, "loaded configuration");

    if let Some(parent) = config.database_path.parent() {
        std::fs::create_dir_all(parent).context("failed to create database directory")?;
    }

    let db = tt_db::Database::open(&config.database_path).context("failed to open database")?;
    Ok((db, config))
}

#[expect(
    clippy::too_many_lines,
    reason = "CLI command dispatch is inherently verbose"
)]
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
        Some(Commands::Ingest { event }) => match event {
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
            IngestEvent::Sessions => {
                let (db, _config) = open_database(cli.config.as_deref())?;
                ingest::index_sessions(&db)?;
            }
        },
        Some(Commands::Export) => {
            // Export doesn't need config - just reads files and outputs to stdout
            export::run()?;
        }
        Some(Commands::Import) => {
            let (db, _config) = open_database(cli.config.as_deref())?;
            import::run(&db)?;
        }
        Some(Commands::Status) => {
            let (db, config) = open_database(cli.config.as_deref())?;
            status::run(&db, &config.database_path)?;
        }
        Some(Commands::Recompute { force }) => {
            let (db, _config) = open_database(cli.config.as_deref())?;
            recompute::run(&db, *force)?;
        }
        Some(Commands::Report {
            week: _,
            last_week,
            day,
            last_day,
            weeks,
            json,
        }) => {
            let (db, _config) = open_database(cli.config.as_deref())?;
            let period = if *last_week {
                report::Period::LastWeek
            } else if *day {
                report::Period::Day
            } else if *last_day {
                report::Period::LastDay
            } else {
                report::Period::Week
            };
            report::run(&db, period, *json, *weeks)?;
        }
        Some(Commands::Tag {
            stream,
            tag: tag_name,
        }) => {
            let (db, _config) = open_database(cli.config.as_deref())?;
            tag::run(&db, stream, tag_name)?;
        }
        Some(Commands::Streams(action)) => {
            let (db, _config) = open_database(cli.config.as_deref())?;
            match action {
                StreamsAction::List { json } => streams::run(&db, *json)?,
                StreamsAction::Create { name } => streams::create(&db, name.clone())?,
            }
        }
        Some(Commands::Init { label }) => {
            init::run(label.as_deref())?;
        }
        Some(Commands::Context {
            events,
            agents,
            streams,
            gaps,
            gap_threshold,
            start,
            end,
        }) => {
            let (db, _config) = open_database(cli.config.as_deref())?;
            context::run(
                &db,
                *events,
                *agents,
                *streams,
                *gaps,
                *gap_threshold,
                start.clone(),
                end.clone(),
            )?;
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

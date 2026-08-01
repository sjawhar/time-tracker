use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::Parser;
use tt_cli::logging;

mod todo_dispatch;

use todo_dispatch::{run_priority_action, run_todo_action};
use tt_cli::commands::{
    classify_auto, export, import, ingest, init, machines, proposals, recompute, report, status,
    streams, sync, tag,
};
use tt_cli::{Cli, Commands, Config, IngestEvent, ProposalsAction, StreamsAction, TodoAction};

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

fn load_config(config_path: Option<&Path>) -> Result<Config> {
    let config = Config::load_from(config_path).context("failed to load configuration")?;
    tracing::debug!(?config, "loaded configuration");
    Ok(config)
}

#[expect(
    clippy::too_many_lines,
    reason = "CLI command dispatch is inherently verbose"
)]
fn main() -> Result<()> {
    let cli = Cli::parse();

    // Warnings are on by default; `RUST_LOG` and `-v` widen from there.
    // Use try_init to avoid panic if tracing is already initialized (e.g., in tests)
    let _ = tracing_subscriber::fmt()
        .with_env_filter(logging::filter(cli.verbose))
        .try_init();

    match &cli.command {
        Some(Commands::Ingest { event }) => match event {
            IngestEvent::PaneFocus {
                pane,
                cwd,
                session,
                window,
                pane_pid,
            } => {
                let written =
                    ingest::ingest_pane_focus(pane, session, *window, cwd, pane_pid.as_deref())?;
                if written {
                    tracing::debug!("event ingested");
                } else {
                    tracing::debug!("event debounced");
                }
            }
            IngestEvent::Scroll {
                pane,
                cwd,
                session,
                window,
            } => {
                let written = ingest::ingest_scroll(pane, session, *window, cwd)?;
                if written {
                    tracing::debug!("scroll event ingested");
                } else {
                    tracing::debug!("scroll event debounced");
                }
            }
            IngestEvent::Sessions { full } => {
                let (db, _config) = open_database(cli.config.as_deref())?;
                let mode = if *full {
                    ingest::ScanMode::Full
                } else {
                    ingest::ScanMode::Incremental
                };
                ingest::index_sessions(&db, mode)?;
            }
        },
        Some(Commands::Export { after, since }) => {
            // Export doesn't need config - just reads files and outputs to stdout
            export::run(after.as_deref(), since.as_deref())?;
        }
        Some(Commands::Import) => {
            let (db, _config) = open_database(cli.config.as_deref())?;
            import::run(&db)?;
        }
        Some(Commands::Status) => {
            let (db, config) = open_database(cli.config.as_deref())?;
            status::run(&db, &config)?;
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
            start,
            end,
            json,
        }) => {
            let (db, _config) = open_database(cli.config.as_deref())?;
            let period = if let Some(start_str) = start {
                let start_date = chrono::NaiveDate::parse_from_str(start_str, "%Y-%m-%d")
                    .with_context(|| {
                        format!("invalid --start date '{start_str}', expected YYYY-MM-DD")
                    })?;
                let end_date = match end {
                    Some(end_str) => chrono::NaiveDate::parse_from_str(end_str, "%Y-%m-%d")
                        .with_context(|| {
                            format!("invalid --end date '{end_str}', expected YYYY-MM-DD")
                        })?,
                    None => chrono::Local::now().date_naive() + chrono::Duration::days(1),
                };
                report::Period::Custom(
                    report::local_midnight_to_utc(start_date),
                    report::local_midnight_to_utc(end_date),
                )
            } else if *last_week {
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
            let (db, config) = open_database(cli.config.as_deref())?;
            match action {
                StreamsAction::List { json, misnamed } => {
                    if *misnamed {
                        streams::misnamed_report(&db, *json)?;
                    } else {
                        streams::run(&db, *json)?;
                    }
                }
                StreamsAction::Create { name } => streams::create(&db, name.clone())?,
                StreamsAction::Link { stream, priority } => {
                    streams::link(
                        &db,
                        &config,
                        &streams::LinkOptions {
                            stream: stream.clone(),
                            priority: priority.clone(),
                        },
                    )?;
                }
                StreamsAction::Slug { stream, slug } => streams::set_slug(&db, stream, slug)?,
                StreamsAction::Describe {
                    stream,
                    description,
                    backfill,
                    apply,
                } => match (*backfill, stream.as_deref(), description.as_deref()) {
                    (true, None, None) => {
                        let classifier = tt_llm::RigClassifier::from_config(
                            &config.classifier.model,
                            &config.classifier.api_key_env,
                        )
                        .context("initialize stream description classifier")?;
                        streams::backfill(&db, &classifier, *apply)?;
                    }
                    (true, _, _) => bail!(
                        "--backfill cannot be combined with a stream reference or description"
                    ),
                    (false, Some(stream), Some(description)) => {
                        streams::describe(&db, stream, description)?;
                    }
                    (false, _, _) => {
                        bail!("provide a stream reference and description, or use --backfill");
                    }
                },
                StreamsAction::Dissolve {
                    streams: stream_refs,
                    dry_run,
                } => {
                    let mode = if *dry_run {
                        tt_db::DissolveMode::DryRun
                    } else {
                        tt_db::DissolveMode::Apply
                    };
                    streams::dissolve(&db, &config, stream_refs, mode)?;
                }
                StreamsAction::ReleasePaneFocus { dry_run } => {
                    let mode = if *dry_run {
                        tt_db::ReleaseMode::DryRun
                    } else {
                        tt_db::ReleaseMode::Apply
                    };
                    streams::release_pane_focus(&db, mode)?;
                }
                StreamsAction::Merge {
                    streams: from_refs,
                    into,
                    dry_run,
                } => {
                    let mode = if *dry_run {
                        tt_db::MergeMode::DryRun
                    } else {
                        tt_db::MergeMode::Apply
                    };
                    streams::merge(&db, from_refs, into, mode)?;
                }
                StreamsAction::Rename { stream, name } => streams::rename(&db, stream, name)?,
                StreamsAction::Assign {
                    stream,
                    session,
                    event,
                } => streams::assign(&db, &config, stream, session, event)?,
            }
        }
        Some(Commands::Todo(action)) => {
            // `link` needs the database because creating a todo→session link is what
            // applies that todo's stream to the session's events. Naming a stream needs
            // it because the stream has to exist before it is written to the store.
            if matches!(action, TodoAction::Drift { .. } | TodoAction::Link { .. })
                || matches!(
                    action,
                    TodoAction::Add {
                        stream: Some(_),
                        ..
                    } | TodoAction::Stream {
                        stream: Some(_),
                        ..
                    }
                )
            {
                let (db, config) = open_database(cli.config.as_deref())?;
                run_todo_action(Some(&db), &config, action)?;
            } else {
                let config = load_config(cli.config.as_deref())?;
                run_todo_action(None, &config, action)?;
            }
        }
        Some(Commands::Priority(action)) => {
            let config = load_config(cli.config.as_deref())?;
            run_priority_action(&config, action)?;
        }
        Some(Commands::Init { label }) => {
            init::run(label.as_deref())?;
        }
        Some(Commands::Machines) => {
            let (db, _config) = open_database(cli.config.as_deref())?;
            machines::run(&db)?;
        }
        Some(Commands::Sync {
            remotes,
            reconcile,
            since,
        }) => {
            let (db, _config) = open_database(cli.config.as_deref())?;
            let mode = if *reconcile {
                sync::SyncMode::Reconcile {
                    since: since.clone(),
                }
            } else {
                sync::SyncMode::Incremental
            };
            sync::run(&db, remotes, &mode)?;
        }
        Some(Commands::Classify { auto: _ }) => {
            let (db, config) = open_database(cli.config.as_deref())?;
            let classifier = tt_llm::RigClassifier::from_config(
                &config.classifier.model,
                &config.classifier.api_key_env,
            )
            .context("initialize automatic classifier")?
            .with_session_detail(
                classify_auto::session_detail::session_tools(&config.database_path)
                    .context("open the classifier's session access")?,
            );
            classify_auto::run_auto(&db, &config, &classifier)?;
        }
        Some(Commands::Proposals(action)) => {
            let (db, _config) = open_database(cli.config.as_deref())?;
            match action {
                ProposalsAction::Ls => proposals::list(&db)?,
                ProposalsAction::Accept { id } => proposals::accept(&db, id)?,
                ProposalsAction::Reject { id, stream } => {
                    proposals::reject(&db, id, stream.as_deref())?;
                }
            }
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

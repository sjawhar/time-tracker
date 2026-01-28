//! Command-line argument definitions.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::commands::events::EventsArgs;
use crate::commands::export::ExportArgs;
use crate::commands::import::ImportArgs;
use crate::commands::ingest::IngestArgs;
use crate::commands::report::ReportArgs;
use crate::commands::suggest_tags::SuggestTagsArgs;
use crate::commands::sync::SyncArgs;

/// AI-native time tracker.
///
/// Passively collects activity signals from development tools and uses LLMs
/// to generate accurate timesheets.
#[derive(Debug, Parser)]
#[command(name = "tt", version, about, long_about = None)]
pub struct Cli {
    /// Enable verbose output.
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Path to config file.
    #[arg(short, long, global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

/// Available subcommands.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Show current tracking status.
    Status,
    /// Append events to the remote buffer.
    Ingest(IngestArgs),
    /// Export buffered events plus Claude session events.
    Export(ExportArgs),
    /// List local events.
    Events(EventsArgs),
    /// Import events from stdin into the local database.
    Import(ImportArgs),
    /// Sync events from a remote host.
    Sync(SyncArgs),
    /// Generate time reports.
    Report(ReportArgs),
    /// Suggest tags for a stream.
    SuggestTags(SuggestTagsArgs),
}

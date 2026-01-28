//! Command-line argument definitions.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

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
    /// Ingest an event from tmux hooks.
    ///
    /// Called by tmux hooks on pane focus changes. Appends events to JSONL buffer.
    Ingest(IngestArgs),
    /// Export all events as JSONL to stdout.
    ///
    /// Reads events from the local buffer and parses Claude Code session logs,
    /// outputting a combined event stream sorted by timestamp.
    Export,
}

/// Arguments for the ingest command.
#[derive(Debug, Args)]
pub struct IngestArgs {
    /// Event type (e.g., "pane-focus").
    #[arg(value_name = "TYPE")]
    pub event_type: String,

    /// Pane ID (e.g., "%3").
    #[arg(long)]
    pub pane: String,

    /// Current working directory.
    #[arg(long)]
    pub cwd: String,

    /// tmux session name.
    #[arg(long)]
    pub session: String,

    /// tmux window index.
    #[arg(long, default_value = "0")]
    pub window: u32,
}

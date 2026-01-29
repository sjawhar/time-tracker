//! Command-line argument definitions.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

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

    /// Ingest events from tmux hooks.
    Ingest {
        #[command(subcommand)]
        event: IngestEvent,
    },

    /// Export all events for sync to local machine.
    ///
    /// Reads events from `~/.time-tracker/events.jsonl` and parses Claude Code
    /// session logs, outputting combined events as JSONL to stdout.
    Export,

    /// Import events from stdin into local `SQLite` database.
    ///
    /// Events are expected as JSONL (one JSON object per line).
    /// Duplicate events (same ID) are silently ignored.
    Import,

    /// Sync events from a remote host via SSH.
    ///
    /// Executes `ssh <remote> tt export` and imports the output into the
    /// local `SQLite` database. Requires SSH key authentication (no password prompts).
    Sync {
        /// Remote host in SSH format (user@host or host).
        ///
        /// Uses your SSH config for custom ports/keys. Configure in ~/.ssh/config.
        remote: String,
    },
}

/// Event types that can be ingested.
#[derive(Debug, Subcommand)]
pub enum IngestEvent {
    /// Record a pane focus event.
    PaneFocus {
        /// The tmux pane ID (e.g., %3).
        #[arg(long)]
        pane: String,

        /// The current working directory of the pane.
        #[arg(long)]
        cwd: String,

        /// The tmux session name.
        #[arg(long)]
        session: String,

        /// The tmux window index (optional).
        #[arg(long)]
        window: Option<u32>,
    },
}

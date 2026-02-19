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
    /// Reads events from `~/.local/share/tt/events.jsonl` and parses Claude Code
    /// session logs, outputting combined events as JSONL to stdout.
    Export,

    /// Import events from stdin into local `SQLite` database.
    ///
    /// Events are expected as JSONL (one JSON object per line).
    /// Duplicate events (same ID) are silently ignored.
    Import,

    /// Recompute direct/delegated time for streams.
    ///
    /// Uses the attention allocation algorithm to calculate time based on
    /// focus events (tmux pane focus, AFK, scroll) and agent activity.
    Recompute {
        /// Recompute all streams, not just those marked as needing recomputation.
        #[arg(long)]
        force: bool,
    },

    /// Generate a time report.
    ///
    /// Shows time spent across streams, grouped by tags (when available).
    /// Default period is the current week.
    Report {
        /// Current week (Monday to Sunday). This is the default.
        #[arg(long, group = "period")]
        week: bool,

        /// Previous week.
        #[arg(long, group = "period")]
        last_week: bool,

        /// Today.
        #[arg(long, group = "period")]
        day: bool,

        /// Yesterday.
        #[arg(long, group = "period")]
        last_day: bool,

        /// Number of weekly reports to generate (most recent first).
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..), group = "period")]
        weeks: Option<u32>,

        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Add a tag to a stream.
    ///
    /// Tags are additive—multiple tags per stream are supported.
    /// Use 'tt streams' to see available stream IDs.
    Tag {
        /// Stream ID or name (e.g., 'abc123' or 'time-tracker').
        stream: String,

        /// Tag to add.
        tag: String,
    },

    /// Manage streams.
    #[command(subcommand)]
    Streams(StreamsAction),

    /// Output context for stream inference (JSON).
    ///
    /// Outputs JSON containing events, agents, streams, and gaps for a time range.
    /// Each section is opt-in via flags.
    Context {
        /// Include chronological events.
        #[arg(long)]
        events: bool,

        /// Include Claude session metadata.
        #[arg(long)]
        agents: bool,

        /// Include existing streams.
        #[arg(long)]
        streams: bool,

        /// Include gaps between user input events.
        #[arg(long)]
        gaps: bool,

        /// Minimum gap duration to include (minutes).
        #[arg(long, default_value = "5")]
        gap_threshold: u32,

        /// Start of time range (ISO 8601 or relative like "4 hours ago").
        #[arg(long)]
        start: Option<String>,

        /// End of time range (ISO 8601, defaults to now).
        #[arg(long)]
        end: Option<String>,
    },
}

/// Streams subcommand actions.
#[derive(Debug, Subcommand)]
pub enum StreamsAction {
    /// List streams with time totals and tags.
    ///
    /// Shows streams from the last 7 days, sorted by total time.
    /// Use 'tt tag <id> <tag>' to organize streams into projects.
    List {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Create a new stream (prints ID to stdout).
    Create {
        /// Name for the stream.
        name: String,
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

    /// Index coding assistant sessions.
    ///
    /// Scans Claude Code (~/.claude/projects/) and `OpenCode`
    /// (~/.local/share/opencode/storage/) session files and stores
    /// metadata in the database.
    Sessions,
}

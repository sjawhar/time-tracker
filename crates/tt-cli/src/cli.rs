//! Command-line argument definitions.

use std::path::PathBuf;

use clap::{ArgAction, Parser, Subcommand};

/// AI-native time tracker.
///
/// Passively collects activity signals from development tools and uses LLMs
/// to generate accurate timesheets.
#[derive(Debug, Parser)]
#[command(name = "tt", version, about, long_about = None)]
pub struct Cli {
    /// Enable verbose output.
    #[arg(short, long, global = true, action = ArgAction::Count)]
    pub verbose: u8,

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
    /// Reads events from `~/.local/share/time-tracker/events.jsonl` and parses Claude Code
    /// session logs, outputting combined events as JSONL to stdout.
    Export {
        /// Only export events after this event ID (for incremental sync).
        #[arg(long)]
        after: Option<String>,

        /// Only export events after this timestamp (for incremental `OpenCode` export).
        #[arg(long)]
        since: Option<String>,
    },

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

        /// Start date (YYYY-MM-DD, local time). Use with --end for custom range.
        #[arg(long, group = "period")]
        start: Option<String>,

        /// End date (YYYY-MM-DD, local time, exclusive). Use with --start for custom range.
        #[arg(long, requires = "start")]
        end: Option<String>,

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

    /// Show and inspect markdown-backed todos.
    #[command(subcommand)]
    Todo(TodoAction),

    /// Show and inspect markdown-backed priorities.
    #[command(subcommand)]
    Priority(PriorityAction),

    /// Initialize machine identity for multi-machine sync.
    ///
    /// Generates a persistent UUID for this machine, stored in
    /// `~/.local/share/time-tracker/machine.json`. Idempotent — safe to run again.
    Init {
        /// Human-friendly label for this machine (defaults to hostname).
        #[arg(long)]
        label: Option<String>,
    },

    /// List known remote machines and their sync status.
    Machines,

    /// Sync events from remote machine(s) via SSH.
    ///
    /// Runs `tt export` on each remote via SSH and imports the events
    /// into the local database. Tracks sync position per remote for
    /// incremental pulls.
    Sync {
        /// Remote host(s) to sync from (SSH alias or user@host).
        #[arg(required = true)]
        remotes: Vec<String>,
    },

    /// [DEPRECATED] Output context for stream inference (JSON).
    ///
    /// Use `tt classify` instead, which provides the same data plus
    /// stream proposals and `--apply` for assignments.
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

        /// Only show events/sessions without a `stream_id`.
        #[arg(long)]
        unclassified: bool,

        /// Compact summary output (one line per session/cluster).
        #[arg(long)]
        summary: bool,
    },

    /// Classify events into streams.
    ///
    /// Show unclassified sessions and events, or apply LLM-proposed
    /// stream assignments.
    Classify {
        /// Apply assignments from JSON file or stdin ("-").
        #[arg(long, value_name = "FILE")]
        apply: Option<String>,

        /// Only show unclassified events (no `stream_id`).
        #[arg(long)]
        unclassified: bool,

        /// Compact summary (one line per session/cluster).
        #[arg(long)]
        summary: bool,

        /// Output as JSON.
        #[arg(long)]
        json: bool,

        /// Start of time range (ISO 8601 or relative like "2 days ago").
        #[arg(long)]
        start: Option<String>,

        /// End of time range (ISO 8601, defaults to now).
        #[arg(long)]
        end: Option<String>,

        /// Include gaps between user input events.
        #[arg(long)]
        gaps: bool,

        /// Minimum gap duration to include (minutes).
        #[arg(long, default_value = "5")]
        gap_threshold: u32,
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

    /// Link a stream name to a priority slug.
    Link {
        /// Exact stream display name.
        stream: String,

        /// Priority slug from priorities.md.
        priority: String,
    },
}

/// Todo subcommand actions.
#[derive(Debug, Subcommand)]
pub enum TodoAction {
    /// Show the current actionable todo list.
    Next {
        /// Limit the main list to the first N items.
        #[arg(long, value_name = "N")]
        top: Option<usize>,

        /// Show only quick todos.
        #[arg(long)]
        quick: bool,

        /// Output stable JSON.
        #[arg(long)]
        json: bool,

        /// Group the main list by priority.
        #[arg(long)]
        by_priority: bool,

        /// Include deferred later items.
        #[arg(long)]
        later: bool,
    },

    /// List all todos and parse diagnostics.
    Ls,

    /// Add a todo.
    Add {
        text: String,

        /// Priority slug served by this todo. Repeat for multiple priorities.
        #[arg(long = "priority", value_name = "SLUG")]
        priority: Vec<String>,

        /// Stream name served by this todo.
        #[arg(long, value_name = "NAME")]
        stream: Option<String>,

        /// Due date (YYYY-MM-DD).
        #[arg(long, value_name = "DATE")]
        due: Option<String>,

        /// Defer until date (YYYY-MM-DD).
        #[arg(long, value_name = "DATE")]
        when: Option<String>,

        #[arg(long)]
        quick: bool,

        #[arg(long)]
        pin: bool,
    },

    /// Mark a todo done by id.
    Done { id: String },

    /// Defer a todo until a date (YYYY-MM-DD).
    Defer { id: String, date: String },

    /// Mark a todo blocked with a reason.
    Block {
        id: String,
        /// Why the todo is blocked.
        reason: String,
    },

    /// Clear a todo's blocked state.
    Unblock { id: String },

    /// Move a todo relative to other todo lines.
    Rank {
        id: String,

        #[arg(long, group = "rank_position")]
        top: bool,

        /// Move above another todo id.
        #[arg(long, value_name = "ID", group = "rank_position")]
        above: Option<String>,

        /// Move below another todo id.
        #[arg(long, value_name = "ID", group = "rank_position")]
        below: Option<String>,
    },

    /// Add ids to todos that are missing them.
    NormalizeIds,

    /// Check todo ordering and priority alignment.
    Check {
        /// Output stable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Compare priority importance against tracked stream time.
    Drift {
        /// Current week (Monday to Sunday). This is the default.
        #[arg(long, group = "todo_drift_period")]
        week: bool,

        /// Previous week.
        #[arg(long, group = "todo_drift_period")]
        last_week: bool,

        /// Today.
        #[arg(long, group = "todo_drift_period")]
        day: bool,

        /// Yesterday.
        #[arg(long, group = "todo_drift_period")]
        last_day: bool,

        /// Output stable JSON.
        #[arg(long)]
        json: bool,
    },
}

/// Priority subcommand actions.
#[derive(Debug, Subcommand)]
pub enum PriorityAction {
    /// List priorities and parse diagnostics.
    Ls,

    /// Add a priority.
    Add {
        /// Priority slug — the priority's name. Lowercase ASCII letters, digits, or '-'.
        slug: String,

        /// Priority value.
        #[arg(long)]
        value: i32,

        /// Optional freeform description.
        #[arg(long)]
        description: Option<String>,
    },

    /// Set or clear a priority's description (an empty/whitespace string clears it).
    Describe {
        /// Priority slug.
        slug: String,

        /// Description text; pass "" to clear.
        text: String,
    },

    /// Set a priority value.
    Value {
        slug: String,
        n: i32,
    },

    Rename {
        old_slug: String,
        new_slug: String,
    },

    /// Mark a priority done.
    Done {
        slug: String,
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

    /// Record a tmux scroll (copy-mode) event.
    Scroll {
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

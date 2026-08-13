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

        /// Re-derive the remote's events from scratch and drop local
        /// `user_message` copies the remote no longer produces.
        ///
        /// Ordinary syncs only add events, so a correction to what counts as a
        /// user message never removes the rows an older remote wrote. Use this
        /// after upgrading a remote to reconcile the replica.
        #[arg(long)]
        reconcile: bool,

        /// Limit `--reconcile` to sessions updated at or after this RFC3339 time.
        ///
        /// Reconciling the full history re-scans every session on the remote.
        /// Sessions outside the window are left exactly as they are.
        #[arg(long, requires = "reconcile")]
        since: Option<String>,
    },

    /// Classify unassigned activity into streams using the configured LLM.
    ///
    /// The one machine-inference path. It selects its own candidates newest-first,
    /// refuses to invent a container for work it cannot place, and leaves anything it
    /// cannot resolve unassigned. The `tt-serve` daemon runs it continuously; this
    /// runs one bounded pass on demand.
    ///
    /// To correct a verdict, use `tt streams assign`.
    Classify {
        /// Required, because a pass spends real LLM calls.
        #[arg(long, required = true)]
        auto: bool,
    },

    /// Review classifier stream proposals.
    #[command(subcommand)]
    Proposals(ProposalsAction),
}

/// Proposal review actions.
#[derive(Debug, Subcommand)]
pub enum ProposalsAction {
    /// List pending proposals.
    Ls,

    /// Accept a proposal and confirm its assignments.
    Accept { id: String },

    /// Reject a proposal, optionally assigning its events to another stream.
    Reject {
        id: String,

        /// Stream ID, slug, or exact name to assign instead.
        #[arg(long)]
        stream: Option<String>,
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

        /// Instead list existing streams whose name does not describe work.
        ///
        /// The guard judges names as they are proposed, so containers minted
        /// before it existed are still standing and still receiving
        /// assignments. This reports them across all time with their event
        /// counts, direct time, and what each name describes instead.
        ///
        /// A report, never an action: nothing is renamed, merged, or dissolved.
        #[arg(long)]
        misnamed: bool,
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

    /// Set a stream's slug (short stable identifier used by todo references).
    Slug {
        /// Stream reference: ID, existing slug, or exact display name.
        stream: String,

        /// New slug: lowercase kebab-case, max 32 chars.
        slug: String,
    },

    Describe {
        #[arg(required_unless_present = "backfill")]
        stream: Option<String>,

        description: Option<String>,

        #[arg(long)]
        backfill: bool,

        #[arg(long, requires = "backfill")]
        apply: bool,
    },

    /// Release a stream's events back to unassigned and retire the stream.
    ///
    /// The undo for a container that should never have been minted — an
    /// activity type, a date range, a catch-all. Released events return to the
    /// unassigned pool, where the terminal-focus pass and the classifier can
    /// reach them. No event is ever deleted.
    ///
    /// Events a human assigned are never touched, and a stream still holding
    /// one is left in place. Start with --dry-run.
    Dissolve {
        /// Stream references: ID, slug, or exact display name.
        #[arg(required = true)]
        streams: Vec<String>,

        /// Report what would change without writing anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// Release attribution from pane focus that could never have earned it.
    ///
    /// A tmux pane focus carries no window title, no app id and — unless the
    /// process-tree stamp caught an agent running in it — no session. Nothing in
    /// this tree can attribute one: every session-keyed writer filters on the
    /// session id, and the classifier's window-run phase reads `window_focus` only.
    ///
    /// A stream on such a row was put there by the deleted cwd propagator, which
    /// wrote the classifier's own assignment source, so the cleanup that released
    /// 777,583 of its rows could not find these. They still inflate the direct
    /// time reported for the containers they name.
    ///
    /// Events a human assigned are never touched and no event is ever deleted.
    /// No stream is retired — judging a stream stays 'tt streams dissolve'.
    /// Start with --dry-run.
    ReleasePaneFocus {
        /// Report what would change without writing anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// Release attention-opening events incorrectly routed to the reserved junk stream.
    ReleaseJunkAttention {
        /// Report what would change without writing anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// Release junk attribution from sessions that outgrew the junk rule.
    ///
    /// A session is judged junk once, from its tool-call and message counts, and
    /// nothing revisits that verdict: each session gets a single re-check, and a
    /// junked session's events are no longer unassigned so the classifier never
    /// selects it again. A session opening with 'Hello' that goes on to make 17
    /// tool calls therefore stays filed as no attributable work permanently.
    ///
    /// Selection is the junk rule inverted, so a session that still satisfies it
    /// cannot be reached. Released events return to unassigned for the classifier
    /// to re-reach, and their classification record is forgotten so it will.
    /// Events a human assigned are never touched, no event is deleted, and the
    /// junk stream is not retired. Start with --dry-run.
    ReleaseOutgrownJunk {
        /// Report what would change without writing anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// Apply recent, observed pane session identities to historical tmux focus events.
    ///
    /// This writes only `session_id`, never a stream or assignment source. A binding is usable
    /// only for the same pane on the same machine within the 30-minute freshness window. Start
    /// with --dry-run.
    BackfillPaneBindings {
        /// Report what would change without writing anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// Collapse streams onto one row, moving their events and tags.
    ///
    /// The counterpart to dissolve: dissolving says this was never work, merging
    /// says this was work, in a stream that already exists. It is what repairs a
    /// real initiative that was minted once per week.
    ///
    /// Events a human assigned MOVE TOO, keeping their assignment source: a merge
    /// corrects which row holds the work, not the human's judgement about what the
    /// work was. Tags move without duplicating. Emptied sources are retired. No
    /// event is ever deleted. Start with --dry-run.
    ///
    /// The target's time totals go stale — run 'tt recompute' afterwards.
    Merge {
        /// Source stream references: ID, slug, or exact display name.
        #[arg(required = true)]
        streams: Vec<String>,

        /// Target stream reference: ID, slug, or exact display name.
        #[arg(long, value_name = "STREAM")]
        into: String,

        /// Report what would change without writing anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// Collapse trailing numbered execution instances onto their initiative.
    ///
    /// An instance suffix such as `(Ralph iteration 7)` names one execution of work,
    /// not a distinct initiative. This selects exact suffix families, renames the
    /// deterministic target to the initiative name, and merges the rest into it.
    /// Events assigned by hand move with their assignment source; no event is deleted.
    /// Start with --dry-run.
    CollapseInstances {
        /// Report exact families and counts without writing anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// Set a stream's display name.
    ///
    /// The first half of repairing a real initiative that was minted once per
    /// week: strip the '(Jun14-20)' suffix here, then collapse the rows that now
    /// share a name with 'tt streams merge'. Names are not unique, so that
    /// intermediate state is legal — and reported, because a shared name no longer
    /// identifies one row.
    Rename {
        /// Stream reference: ID, slug, or exact display name.
        stream: String,

        /// New display name.
        name: String,
    },

    /// Record that specific sessions or events belong to a stream.
    ///
    /// The human-correction surface, and the only one that writes
    /// `assignment_source = "user"` outside proposal review. Every machine writer
    /// refuses to overwrite what this records.
    ///
    /// Deliberately narrow: it moves only sessions and events named explicitly on the
    /// command line, and never creates the target stream. There is no pattern, time
    /// range, or file-input form — machine inference belongs to `tt classify --auto`.
    Assign {
        /// Stream reference: ID, slug, or exact display name. Must already exist.
        stream: String,

        /// Agent session ID; all of its events move. Repeatable.
        #[arg(long, value_name = "ID", required_unless_present = "event")]
        session: Vec<String>,

        /// Explicit event ID. Repeatable.
        #[arg(long, value_name = "ID", required_unless_present = "session")]
        event: Vec<String>,
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

        /// Stream slug served by this todo (must exist in the DB).
        #[arg(long, value_name = "SLUG")]
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

    /// Link the current agent session to a todo.
    ///
    /// Session is auto-detected from `CLAUDE_CODE_SESSION_ID` or `OPENCODE_SESSION_ID`.
    Link {
        id: String,

        /// Explicit session ID (overrides env detection).
        #[arg(long, value_name = "ID")]
        session: Option<String>,
    },

    /// Remove an agent session link from a todo.
    Unlink {
        id: String,

        /// Explicit session ID (overrides env detection).
        #[arg(long, value_name = "ID")]
        session: Option<String>,
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

    /// Set or clear the stream a todo serves.
    ///
    /// Alignment reads todo → stream → priority, so a todo with no stream cannot
    /// answer whether you are working on the right thing. `tt todo add --stream`
    /// only covers todos that do not exist yet; this sets the field on one that does.
    ///
    /// The stream must already exist — a reference matching nothing is reported, never
    /// created — and nothing here guesses which stream a todo belongs to.
    Stream {
        id: String,

        /// Stream reference: ID, slug, or exact display name. Must already exist.
        #[arg(required_unless_present = "clear", conflicts_with = "clear")]
        stream: Option<String>,

        /// Remove the todo's stream instead of setting one.
        #[arg(long)]
        clear: bool,
    },

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

        /// The pane's process id, used to identify the agent session running in it.
        ///
        /// Optional, and absent on any install whose `~/.tmux.conf` has not
        /// re-sourced `config/tmux-hook.conf` since this was added — tmux is asked
        /// directly in that case. Taken as text rather than a number on purpose:
        /// the value comes from a shell hook, and a focus event must never be lost
        /// to a value clap would reject.
        #[arg(long)]
        pane_pid: Option<String>,
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

    /// Record the agent session running in every live tmux pane.
    ///
    /// The periodic half of pane identity. Focus-time capture only observes a pane
    /// when it is switched to — which is exactly when no tool call is usually in
    /// flight there — so most focus events carry no identity. The daemon runs this
    /// every ingest tick; run it by hand (or from cron on a capture-only machine)
    /// to the same effect. Writes only pane→session bindings, never a stream.
    PaneSweep,

    /// Index coding assistant sessions.
    ///
    /// Scans Claude Code (~/.claude/projects/) and `OpenCode`
    /// (~/.local/share/opencode/) session stores and stores metadata in the
    /// database.
    ///
    /// Only sessions touched since the last successful scan are re-derived.
    Sessions {
        /// Re-derive every session, ignoring the incremental scan cursor.
        ///
        /// Needed after a change to what the extractor derives (corrected rows only
        /// appear for sessions actually re-read), or if the cursor is suspected of
        /// having drifted. Costs a full pass over both transcript stores.
        #[arg(long)]
        full: bool,
    },
}

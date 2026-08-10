//! Storage layer for the time tracker.
//!
//! Provides persistence for events using `SQLite`.
//!
//! # Thread Safety
//!
//! The [`Database`] type wraps a `rusqlite::Connection`, which is `Send` but not `Sync`.
//! This means a `Database` instance can be moved between threads but cannot be shared
//! across threads without external synchronization.
//!
//! For multi-threaded access, either:
//! - Use a `Mutex<Database>` to serialize access
//! - Create a connection pool (e.g., with `r2d2`)
//! - Use separate `Database` instances per thread
//!
//! # Schema
//!
//! ## Timestamp Format
//!
//! Timestamps are stored as TEXT in ISO 8601 format (e.g., `2024-01-15T10:30:00Z`).
//! This format ensures:
//! - Lexicographic ordering matches chronological ordering
//! - Human-readable values in the database
//! - Timezone-aware (always UTC)
//!
//! ## Schema Versioning
//!
//! The database tracks its schema version in a `schema_info` table. On open,
//! supported older versions are migrated forward additively; unsupported
//! version mismatches fail fast rather than silently corrupting data.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::Path,
    time::Duration,
};

use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
use rusqlite::{
    Connection, OptionalExtension, Transaction, TransactionBehavior, params, params_from_iter,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tt_core::attribution::{ArtifactMention, RemoteActivity};
use tt_core::{AllocationConfig, AllocationResult, EventType, SessionType, allocate_time};

pub mod timeline_gaps;
pub use timeline_gaps::IdleGap;
mod timeline;
pub use timeline::*;
mod dashboard;
pub use dashboard::ActiveAgentSession;
mod session_activity;
pub use session_activity::WindowedAgentSession;

/// Current schema version. Increment when making schema changes.
const SCHEMA_VERSION: i32 = 13;

/// `meta` key recording that `normalize_stream_timestamps` has already run.
///
/// A data repair, not a schema change, so it is tracked here instead of by
/// `SCHEMA_VERSION` — see `normalize_stream_timestamps`.
const STREAM_TIMESTAMPS_NORMALIZED_KEY: &str = "stream_timestamps_normalized";

/// `meta` key holding how far session ingest has scanned.
///
/// The wall-clock instant at which the last *fully successful* scan began. Ingest
/// reads it back (minus a safety overlap) as the `since` bound for both transcript
/// stores, so a steady-state tick re-derives only what changed instead of the whole
/// corpus. Bookkeeping, so writing it must not bump `db_version` — see
/// `set_session_scan_cursor`.
const SESSION_SCAN_CURSOR_KEY: &str = "session_scan_cursor";

const EVENT_COLUMNS: &str = "id, timestamp, type, source, machine_id, schema_version, cwd, git_project, git_workspace, pane_id, tmux_session, window_index, status, idle_duration_ms, action, session_id, stream_id, assignment_source, window_app_id, window_title";
const STREAM_COLUMNS: &str = "id, created_at, updated_at, name, time_direct_ms, time_delegated_ms, first_event_at, last_event_at, needs_recompute, slug, description, color";
const AGENT_SESSION_COLUMNS: &str = "session_id, source, parent_session_id, project_path, project_name, start_time, end_time, message_count, summary, user_prompts, starting_prompt, assistant_message_count, tool_call_count, session_type, machine_id";

/// Asks whether a stream id names a row of `streams`.
///
/// `events.stream_id` is a foreign key onto `streams.id`, so this is exactly the
/// question every write of an externally supplied id has to answer first. Shared so
/// the assignment guard and proposal acceptance cannot drift apart.
const STREAM_EXISTS_SQL: &str = "SELECT EXISTS(SELECT 1 FROM streams WHERE id = ?1)";

/// Whether a proposal is still answerable, for queries that alias `proposals` as `p`.
///
/// `tt streams dissolve` deletes a stream at any time, stranding every pending
/// proposal that named it: accepting one now raises a foreign-key error, so no human
/// can act on it. Queries that hold a candidate back *because a human is reviewing
/// it* must skip a stranded proposal, or the candidate is exiled from every later
/// pass. A proposal that mints a new stream names no id and stays answerable.
const PROPOSAL_IS_ANSWERABLE_SQL: &str = "(p.proposed_stream_id IS NULL
                    OR EXISTS (SELECT 1 FROM streams s2 WHERE s2.id = p.proposed_stream_id))";

/// The event types that carry a human's attention, as a SQL `IN` list.
///
/// These are the three types that open a direct-time interval: a focus event is the
/// capture of a human looking at something, and `user_message` opens the same window
/// because sending a message to an agent is direct work. Every other type records agent
/// activity, which is delegated time and belongs to no attention budget — counting it
/// here would rank a proposal by how busy the machine was, not by how much of the
/// reviewer's own day it explains. Kept in one place so the ranking cannot drift from
/// `tt-core`'s allocation rules.
const ATTENTION_EVENT_TYPES_SQL: &str = "('user_message', 'window_focus', 'tmux_pane_focus')";

/// The focus events no writer in this tree could have attributed.
///
/// A `tmux_pane_focus` carries no window title (0 of 102,748), no `window_app_id` and,
/// unless the process-tree stamp caught an agent running in the pane, no `session_id`.
/// With the session id gone every session-keyed writer filters it out, and the only
/// id-keyed machine writer is fed from a `type = 'window_focus'` query — so a stream on
/// one of these rows was put there by the deleted cwd propagator. Written once because
/// the count, the preview and the update must select the same rows.
const UNATTRIBUTABLE_PANE_FOCUS_SQL: &str = "type = 'tmux_pane_focus' AND session_id IS NULL";

/// Whether a row still carries attribution state a release would clear.
///
/// Both columns, because releasing means both go NULL: a row holding only a stale
/// `assignment_source` is as much a claim of provenance as one holding a stream, and
/// leaving it would make the postcondition — a sessionless pane carries no attribution
/// — untrue of exactly the rows a previous half-cleanup touched. A row carrying neither
/// is already clean, so excluding it is what makes a second run report zero.
const RELEASABLE_ATTRIBUTION_SQL: &str = "(stream_id IS NOT NULL OR assignment_source IS NOT NULL) \
     AND (assignment_source IS NULL OR assignment_source != 'user')";

/// Reserved slug of the stream holding sessions with no attributable work.
///
/// Junk is routed rather than deleted so a filter that starts eating real work is
/// detectable, and reversible with `tt streams dissolve`.
pub const JUNK_STREAM_SLUG: &str = "junk";

/// Display name of the reserved junk stream.
const JUNK_STREAM_NAME: &str = "junk: no attributable work";

/// `assignment_source` recorded when a session is routed to the junk stream.
pub const JUNK_ASSIGNMENT_SOURCE: &str = "junk";

/// `assignment_source` recorded when a subagent takes its parent's stream.
const INHERITED_ASSIGNMENT_SOURCE: &str = "inherited";

/// `assignment_source` recorded when an event takes the stream of the classified
/// session it belongs to.
///
/// Deliberately not `inferred`: nothing here judges content. The classifier's verdict
/// about this exact session already exists, and this records that the event was a
/// member of it. Distinct from `inherited` too — that is a *subagent* taking its
/// parent's stream, and `inherit_stream_for_session` re-points those rows when the
/// parent is reclassified, which must not happen to these.
const SESSION_MEMBERSHIP_ASSIGNMENT_SOURCE: &str = "session_membership";

/// Format a datetime as RFC3339 with second precision and 'Z' suffix.
///
/// This ensures lexicographic ordering matches chronological ordering.
fn format_timestamp(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Format an optional datetime as RFC3339 with second precision.
fn format_timestamp_opt(dt: Option<DateTime<Utc>>) -> Option<String> {
    dt.map(format_timestamp)
}

/// Read a naive `YYYY-MM-DD HH:MM:SS` datetime as UTC.
///
/// That is the shape `SQLite`'s `CURRENT_TIMESTAMP` writes, and it is UTC by
/// definition, so the reading is recovery rather than assumption. Only this one
/// shape is accepted: it is the only one that ever reached the table, and
/// guessing at a second would be inventing a creation time.
fn naive_utc_timestamp(value: &str) -> Option<String> {
    NaiveDateTime::parse_from_str(value.trim(), "%Y-%m-%d %H:%M:%S%.f")
        .ok()
        .map(|naive| format_timestamp(naive.and_utc()))
}

/// Which `streams` timestamp column a repair targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamTimestampColumn {
    CreatedAt,
    UpdatedAt,
}

impl StreamTimestampColumn {
    const fn name(self) -> &'static str {
        match self {
            Self::CreatedAt => "created_at",
            Self::UpdatedAt => "updated_at",
        }
    }

    /// The update statement for this column, spelled out per variant so no column
    /// name is ever interpolated into SQL.
    const fn update_sql(self) -> &'static str {
        match self {
            Self::CreatedAt => "UPDATE streams SET created_at = ?1 WHERE id = ?2",
            Self::UpdatedAt => "UPDATE streams SET updated_at = ?1 WHERE id = ?2",
        }
    }
}

/// A `streams` timestamp column that no parser can read.
///
/// Carries the stream and the offending text because the whole point of failing
/// here is that a person can go look at the row.
#[derive(Debug, Error)]
#[error("stream {stream_id} has an unreadable {column} timestamp {value:?}: {source}")]
struct MalformedStreamTimestamp {
    stream_id: String,
    column: &'static str,
    value: String,
    source: chrono::ParseError,
}

/// The span of time a stream has been active over, from its events.
///
/// Produced by [`Database::stream_activity_windows`] and consumed by the classifier's
/// roster ordering. `first == last` for a stream with a single event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivityWindow {
    pub first: DateTime<Utc>,
    pub last: DateTime<Utc>,
}

/// Reads a stream's activity bounds, or reports that they cannot be read.
///
/// An unreadable bound warns and yields `None`, which drops the stream to the tail of the
/// classifier's roster — the same posture `remote_activity_for_correlation` takes. This key
/// orders a *presentation*, so failing the read would cost a whole pass its classifications
/// to mis-sort one row.
fn read_activity_window(stream_id: &str, first: &str, last: &str) -> Option<ActivityWindow> {
    let parsed = DateTime::parse_from_rfc3339(first)
        .ok()
        .zip(DateTime::parse_from_rfc3339(last).ok());
    if let Some((first_at, last_at)) = parsed {
        return Some(ActivityWindow {
            first: first_at.with_timezone(&Utc),
            last: last_at.with_timezone(&Utc),
        });
    }
    tracing::warn!(
        stream_id,
        first,
        last,
        "stream has a malformed activity timestamp; ordering it last"
    );
    None
}

/// The earliest and latest event timestamps in the database.
pub type EventTimeBounds = (DateTime<Utc>, DateTime<Utc>);

/// Parse a `streams` timestamp column, failing loudly when it cannot be read.
///
/// Substituting `Utc::now()` — which this replaces — turned a data defect into
/// wrong data that reads as correct, and re-dated the stream on every single
/// read. `normalize_stream_timestamps` repairs the one unreadable shape that ever
/// reached the table, so anything still unreadable here is genuinely unknown and
/// must surface rather than be guessed at.
fn parse_stream_timestamp(
    stream_id: &str,
    column: &'static str,
    value: &str,
) -> Result<DateTime<Utc>, rusqlite::Error> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|source| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(MalformedStreamTimestamp {
                    stream_id: stream_id.to_string(),
                    column,
                    value: value.to_string(),
                    source,
                }),
            )
        })
}

/// A coherent unit of work, grouping related events.
///
/// Streams are materialized for performance but can be recomputed from events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Stream {
    /// Unique identifier (UUID).
    pub id: String,

    /// Human-readable name (auto-generated or user-provided).
    pub name: Option<String>,

    /// Short unique identifier (kebab-case), used by todo-store references.
    pub slug: Option<String>,

    /// User-provided detail that helps identify this stream.
    pub description: Option<String>,

    /// User-selected display color.
    pub color: Option<String>,

    /// When the stream was created.
    pub created_at: DateTime<Utc>,

    /// When the stream was last updated.
    pub updated_at: DateTime<Utc>,

    /// Total human attention time in milliseconds.
    pub time_direct_ms: i64,

    /// Total agent execution time in milliseconds.
    pub time_delegated_ms: i64,

    /// Timestamp of the first event in this stream, as the column holds it.
    ///
    /// **Nothing writes this, and nothing may read it to answer a question.** The
    /// `--apply` engine that filled it is gone; `update_stream_times` — the only
    /// writer `tt recompute` calls — sets the two time totals, `updated_at` and
    /// `needs_recompute`, and `mint_stream_in` inserts NULL. On the live table 985
    /// of 1,245 streams have it NULL and the newest value among the other 260 is
    /// 2026-04-30, 99 days behind the newest event. Ask
    /// [`Database::stream_activity_windows`] or [`Database::streams_in_range`], which
    /// read `events`. The field survives only so the row round-trips faithfully into
    /// the daemon's JSON.
    pub first_event_at: Option<DateTime<Utc>>,

    /// Timestamp of the last event in this stream, as the column holds it.
    ///
    /// Dead in exactly the way [`Stream::first_event_at`] is dead, and read that way
    /// for longer: `tt streams list` filtered its seven-day window on this column and
    /// so reported "No streams with activity in the last 7 days." on a database with
    /// 90 of them.
    pub last_event_at: Option<DateTime<Utc>>,

    /// Flag for lazy recomputation.
    pub needs_recompute: bool,
}

/// The review state of a classifier proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Pending,
    Accepted,
    Rejected,
    /// A later classifier verdict answered the same target confidently enough to apply.
    ///
    /// Distinct from `Rejected`, and that distinction is the whole design: rejection is
    /// a human's verdict that `has_rejected_proposal` reads to suppress future answers,
    /// so writing one to retire a question the machine has since answered itself would
    /// falsify the record and silence the classifier on that target for good.
    Superseded,
}

impl ProposalStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Superseded => "superseded",
        }
    }

    fn from_db(value: &str) -> Result<Self, DbError> {
        match value {
            "pending" => Ok(Self::Pending),
            "accepted" => Ok(Self::Accepted),
            "rejected" => Ok(Self::Rejected),
            "superseded" => Ok(Self::Superseded),
            _ => Err(DbError::InvalidProposalStatus(value.to_string())),
        }
    }
}

/// A stream-assignment suggestion produced by the classifier.
#[derive(Debug, Clone)]
pub struct Proposal {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub session_id: Option<String>,
    pub event_ids: Option<Vec<String>>,
    pub proposed_stream_id: Option<String>,
    pub proposed_new_stream: Option<String>,
    pub confidence: f64,
    pub reasoning: String,
    pub status: ProposalStatus,
    /// The newest classifier generation that has answered this question.
    ///
    /// Written when the proposal is filed, and rewritten when a later pass re-answers
    /// the same target and finds the question still waiting. So it names the classifier
    /// whose verdict the queue currently reflects, not the one that first asked — that
    /// is the reading the skip needs, and the reading that makes a re-ask cost one pass
    /// rather than every pass.
    ///
    /// `None` on the rows written before the column existed. Absent must never compare
    /// equal to the current generation: those proposals were authored by a classifier
    /// nobody can identify, so they are re-asked exactly once, like any other stale
    /// generation.
    pub classifier_generation: Option<u32>,
}

/// A pending proposal paired with the attention reviewing it would resolve.
#[derive(Debug, Clone)]
pub struct RankedProposal {
    pub proposal: Proposal,
    /// How many of the proposal's events are attention-bearing.
    pub attention_events: u64,
}

/// A pending proposal naming a stream that is about to stop existing.
///
/// `proposals.proposed_stream_id` carries no foreign key, so retiring a stream leaves
/// every pending proposal that named it unacceptable — [`Self::accept_proposal`] matches
/// on id alone, and the id will match nothing. The reader half of that already exists:
/// [`PROPOSAL_IS_ANSWERABLE_SQL`] keeps such a row from suppressing a fresh answer, and
/// `tt proposals ls` renders it `(gone)`. This is what lets the command creating the
/// dangle name it first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrandedProposal {
    /// The proposal no reviewer will be able to accept.
    pub proposal_id: String,
    /// The stream id it names, which is about to name nothing.
    pub stream_id: String,
    /// How many events the proposal would assign.
    ///
    /// Zero for a session-scoped proposal: its target is the session, not an event set.
    pub event_count: usize,
}

/// The result of accepting a classifier proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptProposalOutcome {
    /// The stream that received the accepted assignment.
    pub stream_id: String,
    /// Whether accepting the proposal created the stream.
    pub created_stream: bool,
    /// The number of events whose assignment changed.
    pub events_assigned: u64,
}

/// The result of rejecting a classifier proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectProposalOutcome {
    /// The stream that received a replacement user assignment, if one was selected.
    pub stream_id: Option<String>,
    /// The number of events whose assignment changed.
    pub events_assigned: u64,
}

/// Whether a dissolution writes or only reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DissolveMode {
    /// Roll the dissolution back after measuring it.
    DryRun,
    /// Commit the dissolution.
    Apply,
}

/// The result of dissolving a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DissolveOutcome {
    /// Events returned to the unassigned pool.
    pub released: u64,
    /// Events left assigned because a human assigned them.
    pub retained: u64,
    /// Whether the stream row was removed.
    pub retired: bool,
}

/// Whether a release writes or only reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseMode {
    /// Roll the release back after measuring it.
    DryRun,
    /// Commit the release.
    Apply,
}

/// The result of releasing unattributable pane focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleaseOutcome {
    /// Events returned to the unassigned pool.
    pub released: u64,
    /// Events left alone because a human assigned them.
    pub retained: u64,
    /// Streams that lost at least one event.
    pub streams_affected: u64,
}

/// What one bulk junk-routing step settled.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct JunkRoutingOutcome {
    /// Sessions routed to the reserved junk stream.
    pub sessions: u64,
    /// Events that gained the junk stream: the routed sessions' own events, plus the
    /// subagent events that inherited it.
    pub events: u64,
}

/// Whether a merge writes or only reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMode {
    /// Roll the merge back after measuring it.
    DryRun,
    /// Commit the merge.
    Apply,
}

/// What one source stream contributed to a merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedSource {
    /// The source stream's id.
    pub stream_id: String,
    /// Events re-pointed at the target.
    pub events_moved: u64,
    /// How many of those carried a human's assignment.
    pub user_events_moved: u64,
    /// Tags the target did not already hold.
    pub tags_moved: u64,
    /// Pending proposals re-pointed from the source at the target.
    pub proposals_repointed: u64,
    /// Whether the source stream row was removed.
    pub retired: bool,
}

impl MergedSource {
    /// Whether this source moved a row, so the daemon's change signal is owed a bump.
    const fn changed_anything(&self) -> bool {
        self.events_moved > 0 || self.tags_moved > 0 || self.proposals_repointed > 0 || self.retired
    }
}

#[derive(Deserialize)]
struct NewStreamProposal {
    name: String,
    /// Absent when the classifier named the work but did not describe it.
    ///
    /// Stored as SQL NULL so `tt streams describe --backfill`, which selects streams
    /// with no description, still reaches a stream minted from such a proposal.
    #[serde(default)]
    description: Option<String>,
    tags: Vec<String>,
}

/// Current availability of the classifier integration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassifierHealthState {
    #[default]
    Ready,
    Unconfigured,
}

impl ClassifierHealthState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Unconfigured => "unconfigured",
        }
    }

    fn from_db(value: &str) -> Result<Self, DbError> {
        match value {
            "ready" => Ok(Self::Ready),
            "unconfigured" => Ok(Self::Unconfigured),
            _ => Err(DbError::InvalidClassifierHealthState(value.to_owned())),
        }
    }
}

/// Persisted classifier health across daemon and CLI processes.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ClassifierHealth {
    pub state: ClassifierHealthState,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_failure_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub consecutive_failures: u32,
}

/// Database errors.
#[derive(Debug, Error)]
pub enum DbError {
    /// An error from the underlying database.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// Schema version mismatch.
    #[error("schema version mismatch: database has version {found}, expected {expected}")]
    SchemaVersionMismatch { found: i32, expected: i32 },

    /// Slug already assigned to another stream.
    #[error("slug '{slug}' is already in use by another stream")]
    SlugTaken { slug: String },

    /// A proposal row has an unrecognized status value.
    #[error("invalid proposal status: {0}")]
    InvalidProposalStatus(String),

    /// A proposal row has malformed event identifiers.
    #[error("invalid proposal event IDs: {0}")]
    InvalidProposalEventIds(#[from] serde_json::Error),

    /// A proposal's new-stream definition is malformed.
    #[error("invalid proposed new stream: {0}")]
    InvalidProposalNewStream(serde_json::Error),

    /// A merge named the same stream as both a source and the target.
    #[error("cannot merge stream '{stream_id}' into itself")]
    MergeIntoSelf { stream_id: String },

    /// A merge named a target that does not exist.
    #[error("merge target '{stream_id}' does not exist")]
    MergeTargetNotFound { stream_id: String },

    /// A proposal cannot be accepted because it is not pending.
    #[error("proposal '{proposal_id}' is not pending")]
    ProposalNotPending { proposal_id: String },

    /// A proposal does not exist.
    #[error("proposal '{proposal_id}' does not exist")]
    ProposalNotFound { proposal_id: String },

    /// A proposal has an invalid stream target configuration.
    #[error("proposal '{proposal_id}' has an invalid stream target")]
    InvalidProposalStreamTarget { proposal_id: String },

    /// A proposal has an invalid event-assignment target configuration.
    #[error("proposal '{proposal_id}' has an invalid assignment target")]
    InvalidProposalAssignmentTarget { proposal_id: String },

    /// A proposal refers to a stream that does not exist.
    #[error("proposed stream '{stream_id}' does not exist")]
    ProposedStreamNotFound { stream_id: String },

    /// A rejection target does not refer to a stream.
    #[error("target stream '{stream_reference}' does not exist")]
    RejectTargetStreamNotFound { stream_reference: String },

    /// A classifier health timestamp is malformed.
    #[error("invalid classifier health timestamp: {0}")]
    InvalidClassifierHealthTimestamp(#[from] chrono::ParseError),

    /// The persisted classifier failure count is malformed.
    #[error("invalid classifier failure count: {0}")]
    InvalidClassifierFailureCount(#[from] std::num::ParseIntError),
    /// The persisted classifier availability is unknown.
    #[error("invalid classifier health state: {0}")]
    InvalidClassifierHealthState(String),
}

/// Status of events from a single source.
///
/// Used by the `tt status` command to show the most recent event per source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceStatus {
    /// The event source (e.g., "remote.tmux", "remote.agent").
    pub source: String,

    /// Timestamp of the most recent event from this source.
    pub last_timestamp: DateTime<Utc>,
}

/// The newest event of one type produced *on this machine*.
///
/// Distinct from [`SourceStatus`], which groups by the `source` string over every machine
/// in the database. Staleness is only answerable per machine: a synced remote whose watcher
/// is healthy keeps `window_focus` looking alive while the local one is dead, which is how
/// nine days of local silence went unnoticed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalEventTypeStatus {
    /// The event type.
    pub event_type: tt_core::EventType,

    /// Timestamp of this machine's most recent event of that type.
    pub last_timestamp: DateTime<Utc>,
}

/// A known remote machine.
#[derive(Debug, Clone)]
pub struct Machine {
    pub machine_id: String,
    pub label: String,
    pub last_sync_at: Option<String>,
    pub last_event_id: Option<String>,
}

/// An event stored in the database.
///
/// This type represents both events being inserted and events being read.
/// All fields match the columns in the `events` table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredEvent {
    /// Unique identifier (deterministic hash of content).
    pub id: String,

    /// When the event occurred (UTC).
    pub timestamp: DateTime<Utc>,

    /// Event type (e.g., `tmux_pane_focus`, `agent_session`).
    #[serde(rename = "type")]
    pub event_type: tt_core::EventType,

    /// Event source (e.g., "remote.tmux", "remote.agent").
    pub source: String,

    /// Machine UUID that generated this event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,

    /// Schema version of the payload (default: 1).
    #[serde(default = "default_schema_version")]
    pub schema_version: i32,

    /// Tmux pane ID (for `tmux_pane_focus` events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,

    /// Tmux session name (for `tmux_pane_focus` events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_session: Option<String>,

    /// Tmux window index (for `tmux_pane_focus` events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_index: Option<u32>,

    /// Git project name (from remote origin).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_project: Option<String>,

    /// Git workspace name (if in a non-default workspace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_workspace: Option<String>,

    /// AFK status (for `afk_change` events): "idle" or "active".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,

    /// Idle duration in milliseconds (for `afk_change` events with retroactive idle).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_duration_ms: Option<i64>,

    /// Active-window application id (for `window_focus` events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_app_id: Option<String>,

    /// Active-window title (for `window_focus` events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_title: Option<String>,

    /// Agent action (for `agent_session` events): "started", "ended", "`tool_use`".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,

    /// Working directory, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// Agent session ID, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    /// Stream ID this event is assigned to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_id: Option<String>,

    /// How this event was assigned to a stream ('inferred' or 'user').
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignment_source: Option<String>,

    /// Raw JSON data for the event payload.
    /// This is populated from the database `data` column and used by `AllocatableEvent::data()`.
    /// Not part of JSON serialization - explicit fields above are used instead.
    #[serde(skip)]
    pub data: serde_json::Value,
}

const fn default_schema_version() -> i32 {
    1
}

impl StoredEvent {
    /// Builds a JSON object from the explicit data fields.
    ///
    /// This is used when inserting events into the database.
    /// Fields are only included if they have values.
    #[must_use]
    pub fn build_data_json(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();

        if let Some(ref v) = self.pane_id {
            map.insert("pane_id".to_string(), serde_json::Value::String(v.clone()));
        }
        if let Some(ref v) = self.tmux_session {
            map.insert(
                "session_name".to_string(),
                serde_json::Value::String(v.clone()),
            );
        }
        if let Some(v) = self.window_index {
            map.insert(
                "window_index".to_string(),
                serde_json::Value::Number(v.into()),
            );
        }
        if let Some(ref v) = self.git_project {
            map.insert(
                "git_project".to_string(),
                serde_json::Value::String(v.clone()),
            );
        }
        if let Some(ref v) = self.git_workspace {
            map.insert(
                "git_workspace".to_string(),
                serde_json::Value::String(v.clone()),
            );
        }
        if let Some(ref v) = self.status {
            map.insert("status".to_string(), serde_json::Value::String(v.clone()));
        }
        if let Some(v) = self.idle_duration_ms {
            map.insert(
                "idle_duration_ms".to_string(),
                serde_json::Value::Number(v.into()),
            );
        }
        if let Some(ref v) = self.window_app_id {
            map.insert("app".to_string(), serde_json::Value::String(v.clone()));
        }
        if let Some(ref v) = self.window_title {
            map.insert("title".to_string(), serde_json::Value::String(v.clone()));
        }
        if let Some(ref v) = self.action {
            map.insert("action".to_string(), serde_json::Value::String(v.clone()));
        }
        // Include cwd and session_id in data for backward compatibility with existing events
        if let Some(ref v) = self.cwd {
            map.insert("cwd".to_string(), serde_json::Value::String(v.clone()));
        }
        if let Some(ref v) = self.session_id {
            map.insert(
                "session_id".to_string(),
                serde_json::Value::String(v.clone()),
            );
        }

        serde_json::Value::Object(map)
    }
}

// Implement AllocatableEvent for StoredEvent so it can be used with the time allocation algorithm
impl tt_core::AllocatableEvent for StoredEvent {
    fn timestamp(&self) -> DateTime<Utc> {
        self.timestamp
    }

    fn event_type(&self) -> tt_core::EventType {
        self.event_type
    }

    fn stream_id(&self) -> Option<&str> {
        self.stream_id.as_deref()
    }

    fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    fn action(&self) -> Option<&str> {
        self.action.as_deref()
    }

    fn data(&self) -> &serde_json::Value {
        &self.data
    }
}

/// Database connection wrapper.
///
/// See the [module documentation](self) for thread safety considerations.
#[derive(Debug)]
pub struct Database {
    conn: Connection,
}

impl Database {
    /// Opens a database at the given path, creating it if necessary.
    ///
    /// The database schema is automatically initialized on first open.
    /// If the database has an incompatible schema version, returns an error.
    pub fn open(path: &Path) -> Result<Self, DbError> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(30))?;
        // NORMAL: bulk ingest commits ~80k small transactions; synchronous=FULL
        // fsyncs twice per commit, while NORMAL only fsyncs at checkpoints.
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let db = Self { conn };
        db.init()?;
        Ok(db)
    }

    /// Opens an in-memory database.
    ///
    /// Useful for testing. The database is destroyed when the connection closes.
    pub fn open_in_memory() -> Result<Self, DbError> {
        let conn = Connection::open_in_memory()?;
        conn.busy_timeout(Duration::from_secs(30))?;
        let db = Self { conn };
        db.init()?;
        Ok(db)
    }

    /// Opens a write transaction, taking the write lock at `BEGIN`.
    ///
    /// **Every transaction in this module writes**, and rusqlite's
    /// `unchecked_transaction` is `DEFERRED`: it takes no lock, acquires a *read*
    /// snapshot on its first `SELECT`, and only tries to become a write transaction at
    /// its first mutation. In WAL mode that promotion cannot be granted if any other
    /// connection has committed since the snapshot was taken — the snapshot the
    /// statement was planned against no longer exists — so `SQLite` fails the statement
    /// with `SQLITE_BUSY_SNAPSHOT` (error 517), "cannot promote read transaction to
    /// write transaction because of writes by another connection".
    ///
    /// **`busy_timeout` does not cover 517, which is why `open`'s 30 seconds never
    /// helped.** A busy timeout waits for a lock that will eventually be free; a stale
    /// snapshot never becomes fresh by waiting. The call fails immediately, every time a
    /// concurrent writer got there first.
    ///
    /// That is measured, not theoretical. `tt-serve` runs ingest, classification and
    /// sync concurrently against separate connections, and
    /// `claim_unassigned_events_for_classified_sessions` reads the sessions to claim and
    /// then updates their events — the exact read-then-write shape. Against a
    /// continuously-writing classifier it lost the race on **every** tick: `session
    /// ingest failed … Error code 517` every 30 seconds. Because
    /// `attribute_unassigned_events` propagates with `?`, that took the `terminal_focus`
    /// and `artifact_reference` passes down with it, so all three attribution passes
    /// were dead for as long as the classifier was busy.
    ///
    /// `IMMEDIATE` takes the write lock at `BEGIN`, so there is no promotion left to
    /// fail. A concurrent writer now *waits*, which is precisely the retryable condition
    /// `busy_timeout` was configured for. Read-only work must not use this — it would
    /// serialise readers against writers for no reason — but no caller here is
    /// read-only.
    fn write_tx(&self) -> Result<Transaction<'_>, DbError> {
        Ok(Transaction::new_unchecked(
            &self.conn,
            TransactionBehavior::Immediate,
        )?)
    }

    pub fn migrate_legacy_event_types(&self) -> Result<(usize, usize), DbError> {
        let started = self.conn.execute(
            "UPDATE events SET type = 'agent_session', action = 'started'
             WHERE type = 'session_start'
             OR (type = 'agent_session' AND action IS NULL AND id LIKE '%session_start')",
            [],
        )?;
        let ended = self.conn.execute(
            "UPDATE events SET type = 'agent_session', action = 'ended'
             WHERE type = 'session_end'
             OR (type = 'agent_session' AND action IS NULL AND id LIKE '%session_end')",
            [],
        )?;
        Ok((started, ended))
    }

    /// Initializes the database schema.
    ///
    /// Checks schema version, applies additive migrations, and creates tables if needed.
    /// Unsupported schema versions fail fast.
    #[expect(clippy::too_many_lines)]
    fn init(&self) -> Result<(), DbError> {
        // Enable foreign key constraints
        self.conn.execute("PRAGMA foreign_keys = ON", [])?;

        // Check if schema_info table exists and get version
        let existing_version: Option<i32> = self
            .conn
            .query_row("SELECT version FROM schema_info LIMIT 1", [], |row| {
                row.get(0)
            })
            .ok();

        match existing_version {
            Some(v) if v == SCHEMA_VERSION => return self.normalize_stream_timestamps(),
            Some(v @ 8..=12) => {
                let tx = self.write_tx()?;
                if v == 8 {
                    tx.execute("ALTER TABLE events ADD COLUMN window_app_id TEXT", [])?;
                    tx.execute("ALTER TABLE events ADD COLUMN window_title TEXT", [])?;
                }
                if v <= 9 {
                    tx.execute("ALTER TABLE streams ADD COLUMN slug TEXT", [])?;
                }
                tx.execute(
                    "CREATE UNIQUE INDEX IF NOT EXISTS idx_streams_slug ON streams(slug)",
                    [],
                )?;
                if v <= 10 {
                    tx.execute("ALTER TABLE streams ADD COLUMN description TEXT", [])?;
                    tx.execute("ALTER TABLE streams ADD COLUMN color TEXT", [])?;
                }
                if v <= 12 {
                    // Additive and nullable, so every row already in the queue reads as
                    // authored by an unidentifiable classifier and is re-asked once.
                    //
                    // The table may not exist. A capture-only machine never runs the
                    // classifier, so nothing ever creates `proposals` there: devbox sat at
                    // schema 10 holding six tables and no proposals at all, and this ALTER
                    // aborted its migration outright, leaving `tt` unable to open the
                    // database. The `CREATE TABLE IF NOT EXISTS` block below runs after this
                    // arm and declares the column, so a missing table needs no ALTER — it is
                    // created complete a few lines later. Do not "fix" a future migration by
                    // assuming a table's presence; assume only what the version guarantees.
                    let has_proposals: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM sqlite_master
                          WHERE type = 'table' AND name = 'proposals')",
                        [],
                        |row| row.get(0),
                    )?;
                    if has_proposals {
                        tx.execute(
                            "ALTER TABLE proposals ADD COLUMN classifier_generation INTEGER",
                            [],
                        )?;
                    }
                }
                tx.execute(
                    "UPDATE schema_info SET version = ?1",
                    params![SCHEMA_VERSION],
                )?;
                tx.commit()?;
            }
            Some(v) => {
                return Err(DbError::SchemaVersionMismatch {
                    found: v,
                    expected: SCHEMA_VERSION,
                });
            }
            None => {
                // No schema_info table, initialize fresh
            }
        }

        self.conn.pragma_update(None, "journal_mode", "WAL")?;

        self.conn.execute_batch(
            "
            -- Schema version tracking
            CREATE TABLE IF NOT EXISTS schema_info (
                version INTEGER NOT NULL
            );

            -- Events table: stores raw activity signals
            CREATE TABLE IF NOT EXISTS events (
                id TEXT PRIMARY KEY,
                timestamp TEXT NOT NULL,
                type TEXT NOT NULL,
                source TEXT NOT NULL,
                machine_id TEXT,
                schema_version INTEGER DEFAULT 1,
                cwd TEXT,
                git_project TEXT,
                git_workspace TEXT,
                pane_id TEXT,
                tmux_session TEXT,
                window_index INTEGER,
                status TEXT,
                idle_duration_ms INTEGER,
                action TEXT,
                session_id TEXT,
                stream_id TEXT,
                assignment_source TEXT DEFAULT 'inferred',
                window_app_id TEXT,
                window_title TEXT,

                FOREIGN KEY (stream_id) REFERENCES streams(id) ON DELETE SET NULL
            );

            -- Streams table: coherent units of work
            CREATE TABLE IF NOT EXISTS streams (
                id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                name TEXT,
                slug TEXT,
                description TEXT,
                color TEXT,
                time_direct_ms INTEGER DEFAULT 0,
                time_delegated_ms INTEGER DEFAULT 0,
                first_event_at TEXT,
                last_event_at TEXT,
                needs_recompute INTEGER DEFAULT 0
            );

            -- Stream tags table: flexible metadata for streams
            CREATE TABLE IF NOT EXISTS stream_tags (
                stream_id TEXT NOT NULL,
                tag TEXT NOT NULL,
                PRIMARY KEY (stream_id, tag),
                FOREIGN KEY (stream_id) REFERENCES streams(id) ON DELETE CASCADE
            );

            -- Indexes for common queries
            CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events(timestamp);
            CREATE INDEX IF NOT EXISTS idx_events_type ON events(type);
            CREATE INDEX IF NOT EXISTS idx_events_stream ON events(stream_id);
            CREATE INDEX IF NOT EXISTS idx_events_cwd ON events(cwd);
            CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id);
            CREATE INDEX IF NOT EXISTS idx_events_git_project ON events(git_project);
            CREATE INDEX IF NOT EXISTS idx_events_machine ON events(machine_id);
            CREATE INDEX IF NOT EXISTS idx_streams_updated ON streams(updated_at);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_streams_slug ON streams(slug);
            CREATE INDEX IF NOT EXISTS idx_stream_tags_tag ON stream_tags(tag);

            -- Mutable metadata observed by classifier pollers
            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            -- Classifier suggestions awaiting user review
            CREATE TABLE IF NOT EXISTS proposals (
                id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                session_id TEXT,
                event_ids TEXT,
                proposed_stream_id TEXT,
                proposed_new_stream TEXT,
                confidence REAL NOT NULL,
                reasoning TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                classifier_generation INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_proposals_status ON proposals(status);

            -- One follow-up classification after a session accumulates more prompts
            CREATE TABLE IF NOT EXISTS classified_sessions (
                session_id TEXT PRIMARY KEY,
                classified_at TEXT NOT NULL,
                prompt_count INTEGER NOT NULL,
                rechecked INTEGER NOT NULL DEFAULT 0
            );

            -- Agent sessions table: indexed coding assistant sessions
            CREATE TABLE IF NOT EXISTS agent_sessions (
                session_id TEXT PRIMARY KEY,
                source TEXT NOT NULL DEFAULT 'claude',
                parent_session_id TEXT,
                session_type TEXT NOT NULL DEFAULT 'user',
                project_path TEXT NOT NULL,
                project_name TEXT NOT NULL,
                start_time TEXT NOT NULL,
                end_time TEXT,
                message_count INTEGER NOT NULL,
                summary TEXT,
                user_prompts TEXT DEFAULT '[]',
                starting_prompt TEXT,
                assistant_message_count INTEGER DEFAULT 0,
                tool_call_count INTEGER DEFAULT 0,
                machine_id TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_agent_sessions_start_time ON agent_sessions(start_time);
            CREATE INDEX IF NOT EXISTS idx_agent_sessions_project_path ON agent_sessions(project_path);
            CREATE INDEX IF NOT EXISTS idx_agent_sessions_parent ON agent_sessions(parent_session_id);

            -- Machines table: tracks known remote machines for sync
            CREATE TABLE IF NOT EXISTS machines (
                machine_id TEXT PRIMARY KEY,
                label TEXT,
                last_sync_at TEXT,
                last_event_id TEXT
            );
            ",
        )?;

        if existing_version.is_none() {
            self.conn.execute(
                "INSERT INTO schema_info (version) VALUES (?1)",
                params![SCHEMA_VERSION],
            )?;
        }

        self.conn.execute(
            "INSERT OR IGNORE INTO meta(key, value) VALUES('db_version', '0')",
            [],
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO meta(key, value) VALUES('classifier_state', 'ready')",
            [],
        )?;

        self.normalize_stream_timestamps()?;

        Ok(())
    }

    /// Every `streams` timestamp that `parse_stream_timestamp` would refuse.
    ///
    /// Discriminates with the real parser rather than a SQL shape test, so this
    /// cannot drift away from what the read path actually accepts.
    fn unreadable_stream_timestamps(
        &self,
    ) -> Result<Vec<(String, StreamTimestampColumn, String)>, DbError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, created_at, updated_at FROM streams")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;

        let mut unreadable = Vec::new();
        for row in rows {
            let (id, created_at, updated_at) = row?;
            for (column, value) in [
                (StreamTimestampColumn::CreatedAt, created_at),
                (StreamTimestampColumn::UpdatedAt, updated_at),
            ] {
                if DateTime::parse_from_rfc3339(&value).is_err() {
                    unreadable.push((id.clone(), column, value));
                }
            }
        }
        Ok(unreadable)
    }

    /// One-shot repair of `streams` timestamps written in `SQLite`'s
    /// `CURRENT_TIMESTAMP` shape instead of RFC 3339.
    ///
    /// Four streams (`office-admin-2026w14`, `oc-voice-ce`,
    /// `startup-credits-2026w14`, `personal-2026w14`) held a `created_at` like
    /// `2026-03-04 14:32:13` — a real creation time missing only its timezone,
    /// which is why `DateTime::parse_from_rfc3339` gave up with "premature end of
    /// input". All four carry hand-written slug ids, and nothing in `tt` can
    /// produce that shape: `insert_stream` formats through `format_timestamp`, and
    /// `tt sync` copies events but never streams. So the repair is genuinely
    /// one-time, and `meta` records that it ran.
    ///
    /// Recorded in `meta` rather than gated on `SCHEMA_VERSION` deliberately. The
    /// table's shape is unchanged, so this is not a schema migration; bumping the
    /// version would hard-fail every machine's `tt` binary with
    /// `SchemaVersionMismatch` until each one was redeployed, which is a large
    /// blast radius for repairing four rows.
    ///
    /// Deliberately does **not** bump `db_version`: a creation time feeds no
    /// attribution and no verdict, so signalling the daemon would buy nothing but a
    /// spurious recompute.
    fn normalize_stream_timestamps(&self) -> Result<(), DbError> {
        let already_run: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![STREAM_TIMESTAMPS_NORMALIZED_KEY],
                |row| row.get(0),
            )
            .optional()?;
        if already_run.is_some() {
            return Ok(());
        }

        let unreadable = self.unreadable_stream_timestamps()?;
        let tx = self.write_tx()?;
        for (id, column, value) in &unreadable {
            if let Some(repaired) = naive_utc_timestamp(value) {
                tx.execute(column.update_sql(), params![repaired, id])?;
                tracing::info!(
                    stream_id = %id,
                    column = column.name(),
                    from = %value,
                    to = %repaired,
                    "repaired stream timestamp"
                );
            } else {
                tracing::warn!(
                    stream_id = %id,
                    column = column.name(),
                    value = %value,
                    "stream timestamp cannot be read or repaired; reads of this stream will fail"
                );
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES(?1, '1')",
            params![STREAM_TIMESTAMPS_NORMALIZED_KEY],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn bump_db_version_in_transaction(tx: &rusqlite::Transaction<'_>) -> Result<i64, DbError> {
        tx.execute(
            "UPDATE meta SET value = CAST(value AS INTEGER) + 1 WHERE key = 'db_version'",
            [],
        )?;
        tx.query_row(
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'db_version'",
            [],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    /// Increments and returns the attribution-visible database version.
    pub fn bump_db_version(&self) -> Result<i64, DbError> {
        let tx = self.write_tx()?;
        let version = Self::bump_db_version_in_transaction(&tx)?;
        tx.commit()?;
        Ok(version)
    }

    /// Returns the attribution-visible database version.
    pub fn get_db_version(&self) -> Result<i64, DbError> {
        self.conn
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'db_version'",
                [],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Inserts a single event into the database.
    ///
    /// Uses `INSERT OR IGNORE` for idempotent upserts. If an event with the
    /// same ID already exists, it is not modified.
    ///
    /// Returns `true` if the event was inserted, `false` if it already existed.
    pub fn insert_event(&self, event: &StoredEvent) -> Result<bool, DbError> {
        Ok(self.insert_events(std::slice::from_ref(event))? > 0)
    }

    /// Inserts multiple events in a single transaction.
    ///
    /// Uses `INSERT OR IGNORE` for each event. Returns the number of events
    /// that were actually inserted (excluding duplicates).
    pub fn insert_events(&self, events: &[StoredEvent]) -> Result<usize, DbError> {
        let tx = self.write_tx()?;
        let mut count = 0;

        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO events (id, timestamp, type, source, machine_id, schema_version, cwd, git_project, git_workspace, pane_id, tmux_session, window_index, status, idle_duration_ms, action, session_id, stream_id, assignment_source, window_app_id, window_title)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            )?;

            for event in events {
                let timestamp_str = format_timestamp(event.timestamp);

                let rows = stmt.execute(params![
                    event.id,
                    timestamp_str,
                    event.event_type.to_string(),
                    event.source,
                    event.machine_id,
                    event.schema_version,
                    event.cwd,
                    event.git_project,
                    event.git_workspace,
                    event.pane_id,
                    event.tmux_session,
                    event.window_index,
                    event.status,
                    event.idle_duration_ms,
                    event.action,
                    event.session_id,
                    event.stream_id,
                    event.assignment_source,
                    event.window_app_id,
                    event.window_title,
                ])?;

                count += rows;
            }
        }

        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(count)
    }

    /// Retrieves events from the database with optional time range filtering.
    ///
    /// Events are returned ordered by timestamp ascending.
    ///
    /// # Arguments
    ///
    /// * `after` - If provided, only events after this timestamp are returned.
    /// * `before` - If provided, only events before this timestamp are returned.
    ///
    /// Events with malformed timestamps are skipped with a warning.
    pub fn get_events(
        &self,
        after: Option<DateTime<Utc>>,
        before: Option<DateTime<Utc>>,
    ) -> Result<Vec<StoredEvent>, DbError> {
        let mut sql = format!("SELECT {EVENT_COLUMNS} FROM events WHERE 1=1");
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(ref after_ts) = after {
            sql.push_str(" AND timestamp > ?");
            params_vec.push(Box::new(format_timestamp(*after_ts)));
        }

        if let Some(ref before_ts) = before {
            sql.push_str(" AND timestamp < ?");
            params_vec.push(Box::new(format_timestamp(*before_ts)));
        }

        sql.push_str(" ORDER BY timestamp ASC");

        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(AsRef::as_ref).collect();
        let mut stmt = self.conn.prepare(&sql)?;

        let mut events = Vec::new();
        let mut rows = stmt.query(params_refs.as_slice())?;
        while let Some(row) = rows.next()? {
            if let Some(event) = Self::row_to_event(row)? {
                events.push(event);
            }
        }

        Ok(events)
    }

    /// Earliest and latest event timestamps, or `None` when there are no events.
    ///
    /// Two indexed scalar reads against `idx_events_timestamp`. A caller that needs
    /// only the span must use this rather than loading the table for its ends:
    /// `classify_window_runs` took `get_events(None, None).first()/.last()` and paid
    /// **1.3 GB of RSS and 242% CPU on every pass** over 2.7M rows to learn two
    /// timestamps, which is the same defect `tt sync`'s recompute treadmill had.
    pub fn event_time_bounds(&self) -> Result<Option<EventTimeBounds>, DbError> {
        let bounds: Option<(Option<String>, Option<String>)> = self
            .conn
            .query_row(
                "SELECT min(timestamp), max(timestamp) FROM events",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        let Some((Some(first), Some(last))) = bounds else {
            return Ok(None);
        };
        let parse = |value: &str| -> Result<DateTime<Utc>, DbError> {
            Ok(DateTime::parse_from_rfc3339(value)
                .map_err(|source| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(source),
                    )
                })?
                .with_timezone(&Utc))
        };
        Ok(Some((parse(&first)?, parse(&last)?)))
    }

    /// Sessions whose events point at more than one stream, with those streams.
    ///
    /// A data-integrity report `recompute` prints, and it never needed the events in
    /// memory to produce it. Ordered by session id, and each stream list ordered, so the
    /// warning block is stable between runs.
    ///
    /// Reads `events` directly rather than through `row_to_event`, so a row that method
    /// would drop (unknown type, unparseable timestamp) is included here. Measured at 0
    /// rows on the live corpus before this replaced the in-memory scan.
    pub fn sessions_spanning_multiple_streams(
        &self,
    ) -> Result<Vec<(String, Vec<String>)>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, stream_id FROM events
             WHERE session_id IS NOT NULL AND stream_id IS NOT NULL
               AND session_id IN (
                   SELECT session_id FROM events
                   WHERE session_id IS NOT NULL AND stream_id IS NOT NULL
                   GROUP BY session_id
                   HAVING COUNT(DISTINCT stream_id) > 1
               )
             GROUP BY session_id, stream_id
             ORDER BY session_id ASC, stream_id ASC",
        )?;
        let mut grouped: Vec<(String, Vec<String>)> = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let session_id: String = row.get(0)?;
            let stream_id: String = row.get(1)?;
            match grouped.last_mut() {
                Some((existing, streams)) if *existing == session_id => streams.push(stream_id),
                _ => grouped.push((session_id, vec![stream_id])),
            }
        }
        Ok(grouped)
    }

    /// Retrieves events within an inclusive time range.
    ///
    /// Events are returned ordered by timestamp ascending.
    /// Events with malformed timestamps are skipped with a warning.
    ///
    /// # Arguments
    ///
    /// * `start` - Start of the time range (inclusive).
    /// * `end` - End of the time range (inclusive).
    pub fn get_events_in_range(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<StoredEvent>, DbError> {
        let sql = format!(
            "SELECT {EVENT_COLUMNS} FROM events
             WHERE timestamp >= ?1 AND timestamp <= ?2
             ORDER BY timestamp ASC"
        );
        let mut stmt = self.conn.prepare(&sql)?;

        let mut events = Vec::new();
        let mut rows = stmt.query(params![format_timestamp(start), format_timestamp(end)])?;

        while let Some(row) = rows.next()? {
            if let Some(event) = Self::row_to_event(row)? {
                events.push(event);
            }
        }

        Ok(events)
    }

    pub fn get_agent_session_start_events(
        &self,
        session_ids: &[String],
    ) -> Result<Vec<StoredEvent>, DbError> {
        if session_ids.is_empty() {
            return Ok(Vec::new());
        }

        let placeholders = session_ids
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT {EVENT_COLUMNS} FROM events
             WHERE type = 'agent_session' AND action = 'started' AND session_id IN ({placeholders})
             ORDER BY timestamp ASC"
        );

        let mut stmt = self.conn.prepare(&sql)?;
        let mut events = Vec::new();
        let mut rows = stmt.query(params_from_iter(session_ids.iter()))?;

        while let Some(row) = rows.next()? {
            if let Some(event) = Self::row_to_event(row)? {
                events.push(event);
            }
        }

        Ok(events)
    }

    // ========== Stream Methods ==========

    /// Inserts a stream into the database.
    ///
    /// The name is stored in its [normalized](tt_core::normalize_stream_name) form, so
    /// no row can be born carrying a name only whitespace tells apart from another's.
    /// That is a column invariant rather than a caller's duty: the live table already
    /// holds `" agent-c: eval-3 prometheus test-stage (round 2)"` beside its unspaced
    /// twin because one writer forgot.
    ///
    /// Normalizing is all this does. Deciding that an existing stream should hold the
    /// work instead belongs to the caller that chose the name — the signature takes an
    /// id the caller already committed to, so it has no way to report a reuse.
    ///
    /// Returns an error if a stream with the same ID already exists.
    pub fn insert_stream(&self, stream: &Stream) -> Result<(), DbError> {
        let name = stream.name.as_deref().map(tt_core::normalize_stream_name);
        let tx = self.write_tx()?;
        let count = tx.execute(
            "INSERT INTO streams (id, created_at, updated_at, name, time_direct_ms, time_delegated_ms, first_event_at, last_event_at, needs_recompute, slug, description, color)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                stream.id,
                format_timestamp(stream.created_at),
                format_timestamp(stream.updated_at),
                name,
                stream.time_direct_ms,
                stream.time_delegated_ms,
                format_timestamp_opt(stream.first_event_at),
                format_timestamp_opt(stream.last_event_at),
                i32::from(stream.needs_recompute),
                stream.slug,
                stream.description,
                stream.color,
            ],
        )?;
        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The stream whose name matches `name` once both sides are
    /// [normalized](tt_core::normalize_stream_name).
    ///
    /// The authoritative reuse check, and the reason the classifier's roster may be
    /// capped at all. A capped roster cannot show the model every stream, so the model
    /// will sometimes propose a name a stream already holds; this turns that into reuse
    /// instead of a second row. The cap can therefore only ever cost a *semantically*
    /// near-duplicate, never an exact one.
    ///
    /// Deliberately reads the table rather than the caller's roster snapshot. The
    /// snapshot is loaded once per pass and a pass runs for hours, so it goes stale
    /// against this process's own writes and against every other writer — which is how
    /// two rows named `agent-c: eval-3 traccar environment (eval-3 integration)` came to
    /// be minted eleven minutes apart.
    ///
    /// Normalizes the stored side too, because rows predating that invariant still carry
    /// whitespace and are exactly the rows most in need of being found.
    ///
    /// Several rows may share a name — three groups do — and the **earliest created** one
    /// wins. An arbitrary winner would let successive passes alternate between them and
    /// keep splitting one initiative across both.
    pub fn find_stream_by_normalized_name(&self, name: &str) -> Result<Option<Stream>, DbError> {
        let wanted = tt_core::normalize_stream_name(name);
        if wanted.is_empty() {
            return Ok(None);
        }
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {STREAM_COLUMNS} FROM streams WHERE name IS NOT NULL
             ORDER BY created_at ASC, id ASC"
        ))?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let stream = Self::row_to_stream(row)?;
            if stream
                .name
                .as_deref()
                .is_some_and(|stored| tt_core::normalize_stream_name(stored) == wanted)
            {
                return Ok(Some(stream));
            }
        }
        Ok(None)
    }

    /// The period each stream has been active over, from its events.
    ///
    /// The classifier roster's ordering key. A *period* rather than a single timestamp,
    /// because the roster is ordered by how far the session under classification falls
    /// from each stream's activity, and one timestamp cannot express "was already underway
    /// when this session ran" — which is the strongest reuse signal there is.
    ///
    /// `streams.first_event_at`/`last_event_at` look like exactly this and are not usable:
    /// only `tt recompute` writes them, and recompute walks the whole event history, so
    /// nobody runs it — **758 of the live table's 1,018 streams have `last_event_at` NULL**.
    /// Ordering a roster by that column would drop three quarters of the streams into an
    /// undifferentiated tail, which is the opposite of what the ordering is for.
    ///
    /// A stream with no events is absent rather than present-and-old: it has no activity
    /// to report, and the caller decides where a stream that has never been seen belongs.
    ///
    /// An unreadable timestamp warns and drops that stream to the caller's tail, the same
    /// posture `remote_activity_for_correlation` takes and for the same reason: this key
    /// orders a *presentation*, so failing the call would cost the whole pass its
    /// classifications to mis-sort one row.
    pub fn stream_activity_windows(&self) -> Result<HashMap<String, ActivityWindow>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT stream_id, MIN(timestamp), MAX(timestamp) FROM events
             WHERE stream_id IS NOT NULL GROUP BY stream_id",
        )?;
        let mut rows = stmt.query([])?;
        let mut windows = HashMap::new();
        while let Some(row) = rows.next()? {
            let stream_id: String = row.get(0)?;
            let earliest: String = row.get(1)?;
            let newest: String = row.get(2)?;
            if let Some(window) = read_activity_window(&stream_id, &earliest, &newest) {
                windows.insert(stream_id, window);
            }
        }
        Ok(windows)
    }

    /// The period one stream has been active over.
    ///
    /// The single-stream form of [`Self::stream_activity_windows`], for the classifier's
    /// off-roster reuse path: a stream found by name rather than read from the roster
    /// snapshot still needs its real period, and paying a whole-table aggregate for one row
    /// would make the rare path the expensive one. Uses `idx_events_stream`.
    ///
    /// `None` when the stream has no events, or when its timestamps cannot be read — the
    /// same degradation, for the same reason, as the bulk form.
    pub fn stream_activity_window(
        &self,
        stream_id: &str,
    ) -> Result<Option<ActivityWindow>, DbError> {
        let bounds: Option<(Option<String>, Option<String>)> = self
            .conn
            .query_row(
                "SELECT MIN(timestamp), MAX(timestamp) FROM events WHERE stream_id = ?1",
                params![stream_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((Some(earliest), Some(newest))) = bounds else {
            return Ok(None);
        };
        Ok(read_activity_window(stream_id, &earliest, &newest))
    }

    /// Retrieves a stream by ID.
    ///
    /// Returns `None` if no stream with the given ID exists.
    pub fn get_stream(&self, id: &str) -> Result<Option<Stream>, DbError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {STREAM_COLUMNS} FROM streams WHERE id = ?1"
        ))?;

        let mut rows = stmt.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::row_to_stream(row)?)),
            None => Ok(None),
        }
    }

    /// Whether `stream_id` names an existing stream.
    ///
    /// A stream id that came from outside the database is untrusted: the roster the
    /// classifier read can be stale by the time its verdict lands, because
    /// `tt streams dissolve` deletes rows. Ask this before writing such an id onto an
    /// event; the foreign key catches it otherwise, and it aborts the whole pass.
    ///
    /// Matches by id alone. `events.stream_id` stores the literal string and its
    /// foreign key points at `streams.id`, so resolving a slug or a name here would
    /// silently rewrite the answer instead of refusing it.
    pub fn stream_exists(&self, stream_id: &str) -> Result<bool, DbError> {
        self.conn
            .query_row(STREAM_EXISTS_SQL, params![stream_id], |row| row.get(0))
            .map_err(Into::into)
    }

    /// Retrieves a stream by slug.
    pub fn get_stream_by_slug(&self, slug: &str) -> Result<Option<Stream>, DbError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {STREAM_COLUMNS} FROM streams WHERE slug = ?1"
        ))?;
        let mut rows = stmt.query(params![slug])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::row_to_stream(row)?)),
            None => Ok(None),
        }
    }

    /// Resolves the reserved junk stream, creating it on first use.
    ///
    /// Junking is recoverable by construction: the events keep their rows and
    /// `tt streams dissolve` releases them back to unassigned, which is how a rule
    /// that starts eating real work gets reversed. Nothing is ever deleted.
    ///
    /// The slug doubles as the id because this row is a singleton the code names
    /// directly, unlike the UUID-keyed streams a classifier mints.
    pub fn junk_stream_id(&self) -> Result<String, DbError> {
        if let Some(stream) = self.get_stream_by_slug(JUNK_STREAM_SLUG)? {
            return Ok(stream.id);
        }
        let now = Utc::now();
        let stream = Stream {
            id: JUNK_STREAM_SLUG.to_string(),
            name: Some(JUNK_STREAM_NAME.to_string()),
            slug: Some(JUNK_STREAM_SLUG.to_string()),
            description: Some(
                "Sessions that ran no tool and held no discussion, plus subagents whose \
                 parent was never indexed. Released with `tt streams dissolve junk`."
                    .to_string(),
            ),
            color: None,
            created_at: now,
            updated_at: now,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: true,
        };
        self.insert_stream(&stream)?;
        Ok(stream.id)
    }

    /// Sets (or replaces) a stream's slug.
    ///
    /// Returns `SlugTaken` if another stream already uses the slug.
    pub fn set_stream_slug(&self, stream_id: &str, slug: &str) -> Result<(), DbError> {
        let tx = self.write_tx()?;
        let result = tx.execute(
            "UPDATE streams SET slug = ?1, updated_at = ?2 WHERE id = ?3",
            params![slug, format_timestamp(Utc::now()), stream_id],
        );
        match result {
            Ok(count) => {
                if count > 0 {
                    Self::bump_db_version_in_transaction(&tx)?;
                }
                tx.commit()?;
                Ok(())
            }
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Err(DbError::SlugTaken {
                    slug: slug.to_string(),
                })
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Sets a stream's optional description.
    pub fn set_stream_description(
        &self,
        stream_id: &str,
        description: &str,
    ) -> Result<(), DbError> {
        let tx = self.write_tx()?;
        let count = tx.execute(
            "UPDATE streams SET description = ?1, updated_at = ?2 WHERE id = ?3",
            params![description, format_timestamp(Utc::now()), stream_id],
        )?;
        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Sets or clears a stream's optional display color.
    pub fn set_stream_color(&self, stream_id: &str, color: Option<&str>) -> Result<(), DbError> {
        let tx = self.write_tx()?;
        let count = tx.execute(
            "UPDATE streams SET color = ?1, updated_at = ?2 WHERE id = ?3",
            params![color, format_timestamp(Utc::now()), stream_id],
        )?;
        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Retrieves all streams.
    ///
    /// Returns streams ordered by `updated_at` descending.
    pub fn get_streams(&self) -> Result<Vec<Stream>, DbError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {STREAM_COLUMNS} FROM streams ORDER BY updated_at DESC"
        ))?;

        let mut streams = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            streams.push(Self::row_to_stream(row)?);
        }
        Ok(streams)
    }

    /// Assigns an event to a stream.
    ///
    /// Updates the event's `stream_id` and `assignment_source` fields.
    pub fn assign_event_to_stream(
        &self,
        event_id: &str,
        stream_id: &str,
        source: &str,
    ) -> Result<(), DbError> {
        let tx = self.write_tx()?;
        let count = tx.execute(
            "UPDATE events SET stream_id = ?1, assignment_source = ?2 WHERE id = ?3",
            params![stream_id, source, event_id],
        )?;
        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Assigns multiple events to streams in a single transaction.
    ///
    /// Returns the number of events updated.
    pub fn assign_events_to_stream(
        &self,
        assignments: &[(String, String)],
        source: &str,
    ) -> Result<u64, DbError> {
        let tx = self.write_tx()?;
        let mut count = 0u64;

        {
            let mut stmt = tx.prepare(
                "UPDATE events SET stream_id = ?1, assignment_source = ?2 WHERE id = ?3",
            )?;

            for (event_id, stream_id) in assignments {
                count += stmt.execute(params![stream_id, source, event_id])? as u64;
            }
        }

        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(count)
    }

    /// Assigns all events for a session to a stream.
    ///
    /// Updates all events where `session_id` matches, setting `stream_id` and
    /// `assignment_source`. Skips events with `assignment_source = 'user'`.
    /// Returns the number of events updated.
    pub fn assign_events_by_session_id(
        &self,
        session_id: &str,
        stream_id: &str,
        source: &str,
    ) -> Result<u64, DbError> {
        let tx = self.write_tx()?;
        let count = tx.execute(
            "UPDATE events SET stream_id = ?1, assignment_source = ?2 \
             WHERE session_id = ?3 AND (assignment_source IS NULL OR assignment_source != 'user')",
            params![stream_id, source, session_id],
        )?;
        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(count as u64)
    }

    /// Assigns events with explicit IDs to a stream.
    ///
    /// Chunks updates to stay below `SQLite`'s variable limit. Skips events with
    /// `assignment_source = 'user'`. Returns the number of events updated.
    pub fn assign_events_by_ids(
        &self,
        ids: &[String],
        stream_id: &str,
        source: &str,
    ) -> Result<u64, DbError> {
        const CHUNK_SIZE: usize = 500;

        let tx = self.write_tx()?;
        let mut total = 0u64;
        for chunk in ids.chunks(CHUNK_SIZE) {
            if chunk.is_empty() {
                continue;
            }

            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "UPDATE events SET stream_id = ?, assignment_source = ? \
                 WHERE id IN ({placeholders}) \
                 AND (assignment_source IS NULL OR assignment_source != 'user')"
            );
            let mut params_vec: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(chunk.len() + 2);
            params_vec.push(&stream_id);
            params_vec.push(&source);
            params_vec.extend(chunk.iter().map(|id| id as &dyn rusqlite::ToSql));
            total += tx.execute(&sql, params_from_iter(params_vec))? as u64;
        }
        if total > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(total)
    }

    /// Records a human's verdict that a session's events belong to a stream.
    ///
    /// The counterpart to [`Self::assign_events_by_session_id`], and deliberately the
    /// opposite on one point: that one skips `assignment_source = 'user'` because a
    /// machine must never overwrite a human, while this one overwrites everything —
    /// including an earlier `'user'` row — because a human must be able to change
    /// their own mind. A guard here would make the correction surface silently refuse
    /// the second correction of the same session.
    ///
    /// `'user'` is hardcoded rather than taken as a parameter so this cannot be
    /// reused as an inference primitive. Returns the number of events updated.
    pub fn reassign_session_as_user(
        &self,
        session_id: &str,
        stream_id: &str,
    ) -> Result<u64, DbError> {
        let tx = self.write_tx()?;
        let count = tx.execute(
            "UPDATE events SET stream_id = ?1, assignment_source = 'user' \
             WHERE session_id = ?2",
            params![stream_id, session_id],
        )?;
        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(count as u64)
    }

    /// Assigns only currently unassigned events belonging to a session.
    pub fn assign_unassigned_events_by_session_id(
        &self,
        session_id: &str,
        stream_id: &str,
        source: &str,
    ) -> Result<u64, DbError> {
        let tx = self.write_tx()?;
        let count = tx.execute(
            "UPDATE events SET stream_id = ?1, assignment_source = ?2 \
             WHERE session_id = ?3 AND stream_id IS NULL",
            params![stream_id, source, session_id],
        )?;
        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(count as u64)
    }

    /// Gives a subagent's events the stream its parent resolved to.
    ///
    /// Claims the events that have no stream and the ones a previous inheritance
    /// wrote, so a subagent follows its parent when the parent is reclassified. Every
    /// other `assignment_source` is a verdict about this session in particular —
    /// human, inferred, todo link, terminal focus — and inheritance never overrides one.
    pub fn inherit_stream_for_session(
        &self,
        session_id: &str,
        stream_id: &str,
    ) -> Result<u64, DbError> {
        let tx = self.write_tx()?;
        let count = tx.execute(
            "UPDATE events SET stream_id = ?1, assignment_source = ?2 \
             WHERE session_id = ?3 \
               AND (stream_id IS NULL OR assignment_source = ?2)",
            params![stream_id, INHERITED_ASSIGNMENT_SOURCE, session_id],
        )?;
        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(count as u64)
    }

    /// Gives an unassigned event the stream of the classified session it belongs to.
    ///
    /// The classifier claims a session's events *at classification time*, through
    /// [`Self::assign_unassigned_events_by_session_id`]. An event that acquires that
    /// `session_id` afterwards is therefore reached by nothing — which is the whole
    /// population of `tmux_pane_focus` rows stamped from the focused pane's process
    /// tree, since a pane is stamped when it is focused and its session may have been
    /// classified hours earlier. This is the other half of that mechanism.
    ///
    /// **This is not a surface→stream rule.** It reads no `cwd`, no window title, no
    /// app id, and no moment in time, and it forms no judgement of its own: the event
    /// carries the session's own id, the classifier already ruled on that exact
    /// session, and this repeats that ruling onto a row that was a member of it all
    /// along. It is the same principle as [`Self::inherit_stream_for_session`] giving a
    /// subagent its parent's stream — an identity established at capture, not inferred
    /// here. A junked session propagates its junk stream for the same reason, and
    /// `tt streams dissolve junk` reverses that exactly as it does for the session.
    ///
    /// The stream is read from the session's **own already-assigned events**, because
    /// `classified_sessions` records only that a session was classified and never which
    /// stream it landed on. **A session whose assigned events name more than one stream
    /// is skipped whole**: propagation may only repeat a verdict it can read
    /// unambiguously, and picking a winner or breaking a tie would make this the
    /// plurality rule it is not. The events it leaves behind stay unassigned, where
    /// they read as classification lag.
    ///
    /// Only `stream_id IS NULL` rows are claimed, which is strictly narrower than
    /// skipping `assignment_source = 'user'`: no assignment of any kind — human,
    /// inferred, todo link, terminal focus, artifact reference — is ever overwritten.
    /// The source is hardcoded so this cannot be reused as an inference primitive.
    /// Returns the number of events claimed.
    pub fn claim_unassigned_events_for_classified_sessions(&self) -> Result<u64, DbError> {
        let tx = self.write_tx()?;
        let mut count = 0u64;

        {
            // Driven from `classified_sessions` (thousands of rows) rather than from the
            // unassigned events (hundreds of thousands), and the `EXISTS` keeps the
            // update loop below to the sessions that actually have something left to
            // claim. `MIN(stream_id)` is the one distinct value the `HAVING` allows.
            let mut resolved = tx.prepare(
                "SELECT e.session_id, MIN(e.stream_id) FROM events e \
                 JOIN classified_sessions c ON c.session_id = e.session_id \
                 WHERE e.stream_id IS NOT NULL \
                 GROUP BY e.session_id \
                 HAVING COUNT(DISTINCT e.stream_id) = 1 \
                    AND EXISTS (SELECT 1 FROM events u \
                                WHERE u.session_id = e.session_id AND u.stream_id IS NULL)",
            )?;
            let sessions = resolved
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;

            let mut claim = tx.prepare(
                "UPDATE events SET stream_id = ?1, assignment_source = ?2 \
                 WHERE session_id = ?3 AND stream_id IS NULL",
            )?;
            for (session_id, stream_id) in sessions {
                count += claim.execute(params![
                    stream_id,
                    SESSION_MEMBERSHIP_ASSIGNMENT_SOURCE,
                    session_id
                ])? as u64;
            }
        }

        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(count)
    }

    /// Assigns only currently unassigned events among explicit IDs.
    pub fn assign_unassigned_events_by_ids(
        &self,
        ids: &[String],
        stream_id: &str,
        source: &str,
    ) -> Result<u64, DbError> {
        if ids.is_empty() {
            return Ok(0);
        }
        let placeholders = std::iter::repeat_n("?", ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "UPDATE events SET stream_id = ?, assignment_source = ? \
             WHERE id IN ({placeholders}) AND stream_id IS NULL"
        );
        let mut params_vec: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(ids.len() + 2);
        params_vec.push(&stream_id);
        params_vec.push(&source);
        params_vec.extend(ids.iter().map(|id| id as &dyn rusqlite::ToSql));
        let tx = self.write_tx()?;
        let count = tx.execute(&sql, params_from_iter(params_vec))?;
        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(count as u64)
    }

    /// Reassigns only classifier-inferred events belonging to a session.
    pub fn reassign_inferred_events_by_session_id(
        &self,
        session_id: &str,
        stream_id: &str,
        source: &str,
    ) -> Result<u64, DbError> {
        let tx = self.write_tx()?;
        let count = tx.execute(
            "UPDATE events SET stream_id = ?1, assignment_source = ?2 \
             WHERE session_id = ?3 AND assignment_source = 'inferred'",
            params![stream_id, source, session_id],
        )?;
        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(count as u64)
    }

    /// Retrieves events assigned to a specific stream.
    ///
    /// Events are returned ordered by timestamp ascending.
    pub fn get_events_by_stream(&self, stream_id: &str) -> Result<Vec<StoredEvent>, DbError> {
        let sql = format!(
            "SELECT {EVENT_COLUMNS} FROM events WHERE stream_id = ?1 ORDER BY timestamp ASC"
        );
        let mut stmt = self.conn.prepare(&sql)?;

        let mut events = Vec::new();
        let mut rows = stmt.query(params![stream_id])?;
        while let Some(row) = rows.next()? {
            if let Some(event) = Self::row_to_event(row)? {
                events.push(event);
            }
        }

        Ok(events)
    }

    /// Retrieves the ids of events that are not assigned to any stream.
    ///
    /// **Ids only, deliberately.** A whole-table scan of unassigned events is
    /// exactly the input a surface→stream rule needs, and the last one — cwd
    /// inference in `tt ingest sessions` — read each row's `cwd` from here and
    /// assigned 695,394 events on the theory that a folder is a project.
    /// Returning ids means the next such pass has to write its own query naming
    /// the surface it wants to key on, rather than finding the scan already
    /// built. See root `AGENTS.md`, "A folder is not a project".
    ///
    /// Ids are returned ordered by timestamp ascending.
    pub fn unassigned_event_ids(&self) -> Result<Vec<String>, DbError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM events WHERE stream_id IS NULL ORDER BY timestamp ASC")?;

        let mut ids = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            ids.push(row.get(0)?);
        }
        Ok(ids)
    }

    /// Retrieves unassigned `window_focus` events — the candidates for
    /// terminal-focus attribution.
    ///
    /// Assigned rows are excluded by the query itself, so this pass can never
    /// overwrite an existing attribution. Events are returned ordered by
    /// timestamp ascending.
    pub fn unattributed_terminal_focus_events(&self) -> Result<Vec<StoredEvent>, DbError> {
        let sql = format!(
            "SELECT {EVENT_COLUMNS} FROM events
             WHERE type = 'window_focus' AND stream_id IS NULL
             ORDER BY timestamp ASC"
        );
        let mut stmt = self.conn.prepare(&sql)?;

        let mut events = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            if let Some(event) = Self::row_to_event(row)? {
                events.push(event);
            }
        }

        Ok(events)
    }

    /// Retrieves classified activity in a time range, for correlating a terminal
    /// focus against the work its host was doing.
    ///
    /// `window_focus` is deliberately not a candidate type: this pass assigns
    /// `window_focus` rows, and feeding its own output back in would let one
    /// resolved event carry the next.
    ///
    /// Both bounds are inclusive, and rows come back ordered by timestamp
    /// ascending so a caller can binary-search the correlation window.
    pub fn remote_activity_for_correlation(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<RemoteActivity>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT timestamp, stream_id FROM events
             WHERE type IN ('tmux_pane_focus', 'agent_tool_use', 'user_message')
               AND stream_id IS NOT NULL
               AND timestamp >= ?1 AND timestamp <= ?2
             ORDER BY timestamp ASC",
        )?;

        let mut activity = Vec::new();
        let mut rows = stmt.query(params![format_timestamp(start), format_timestamp(end)])?;
        while let Some(row) = rows.next()? {
            let timestamp_str: String = row.get(0)?;
            let stream_id: String = row.get(1)?;
            match DateTime::parse_from_rfc3339(&timestamp_str) {
                Ok(timestamp) => activity.push(RemoteActivity {
                    timestamp: timestamp.with_timezone(&Utc),
                    stream_id,
                }),
                Err(e) => tracing::warn!(
                    timestamp = %timestamp_str,
                    error = %e,
                    "skipping correlation candidate with malformed timestamp"
                ),
            }
        }

        Ok(activity)
    }

    /// Collects every artifact reference made by already-classified work.
    ///
    /// This is the candidate set for artifact-reference attribution: when a browser
    /// window is displaying a pull request or issue, the work it belongs to is
    /// whichever stream *did that pull request*, and a session that wrote the
    /// artifact's URL or its `#number` is the record of having done it.
    ///
    /// A session's own stream is the strict plurality of its events' streams; a
    /// session split evenly across two streams contributes nothing, because it
    /// cannot say which one owns the artifact. Session text is read row by row and
    /// dropped immediately — `user_prompts` runs to tens of kilobytes per session
    /// and there are tens of thousands of them, so only the extracted references
    /// are retained.
    pub fn artifact_mentions_for_binding(&self) -> Result<Vec<ArtifactMention>, DbError> {
        let session_streams = self.stream_per_session()?;

        let mut stmt = self.conn.prepare(
            "SELECT session_id, project_name,
                    COALESCE(summary, '') || ' ' || COALESCE(starting_prompt, '')
                    || ' ' || COALESCE(user_prompts, '')
             FROM agent_sessions",
        )?;

        let mut mentions = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let session_id: String = row.get(0)?;
            let Some(stream_id) = session_streams.get(&session_id) else {
                continue;
            };
            let project: String = row.get(1)?;
            let text: String = row.get(2)?;
            let project = (!project.is_empty()).then_some(project.as_str());
            mentions.extend(
                tt_core::attribution::artifact_refs_in_text(&text, project)
                    .into_iter()
                    .map(|artifact| ArtifactMention {
                        artifact,
                        stream_id: stream_id.clone(),
                    }),
            );
        }

        Ok(mentions)
    }

    /// Maps each agent session to the one stream its events overwhelmingly sit in.
    ///
    /// Sessions with no assigned events, or with a tie between two streams, are
    /// absent rather than guessed at.
    fn stream_per_session(&self) -> Result<HashMap<String, String>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, stream_id, COUNT(*) FROM events
             WHERE session_id IS NOT NULL AND stream_id IS NOT NULL
             GROUP BY session_id, stream_id",
        )?;

        let mut votes: HashMap<String, Vec<(String, i64)>> = HashMap::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let session_id: String = row.get(0)?;
            let stream_id: String = row.get(1)?;
            let count: i64 = row.get(2)?;
            votes
                .entry(session_id)
                .or_default()
                .push((stream_id, count));
        }

        Ok(votes
            .into_iter()
            .filter_map(|(session_id, tallies)| {
                let top = tallies.iter().map(|(_, count)| *count).max()?;
                let mut leaders = tallies.iter().filter(|(_, count)| *count == top);
                let (winner, _) = leaders.next()?;
                leaders
                    .next()
                    .is_none()
                    .then(|| (session_id, winner.clone()))
            })
            .collect())
    }

    /// Deletes all events from a specific machine.
    ///
    /// Used to force a clean re-import when the export format changes.
    /// Returns the number of events deleted.
    pub fn delete_events_by_machine(&self, machine_id: &str) -> Result<u64, DbError> {
        let tx = self.write_tx()?;
        let count = tx.execute(
            "DELETE FROM events WHERE machine_id = ?1",
            params![machine_id],
        )?;
        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(count as u64)
    }

    /// Deletes streams that have no events assigned to them.
    ///
    /// Returns the number of streams deleted.
    pub fn delete_orphaned_streams(&self) -> Result<u64, DbError> {
        let tx = self.write_tx()?;
        let count = tx.execute(
            "DELETE FROM streams WHERE id NOT IN (SELECT DISTINCT stream_id FROM events WHERE stream_id IS NOT NULL)",
            [],
        )?;
        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(count as u64)
    }

    /// Releases a stream's events back to unassigned and retires the stream.
    ///
    /// Machine-assigned events (`assignment_source` anything but `'user'`, NULL
    /// included) lose both their `stream_id` and their `assignment_source`, so a
    /// later attribution pass — which only reads `stream_id IS NULL` — can reach
    /// them. Human assignments are never touched, and a stream still holding one
    /// is left in place: it has content only a human can speak for.
    ///
    /// No event row is ever deleted. The stream row is, once nothing points at
    /// it: `stream_tags` cascades away with it and `events.stream_id` is already
    /// NULL by then.
    ///
    /// **Pending proposals are named, never touched.** Retiring a stream strands every
    /// pending proposal that named it, and [`Self::pending_proposals_for_streams`] is
    /// what lets the command report that before the operator decides. Nothing here
    /// rewrites one: dissolution asserts the work never happened, so there is no stream
    /// to re-point a proposal to — the opposite of [`Self::merge_streams`], which always
    /// has one.
    ///
    /// `DissolveMode::DryRun` runs the identical statements and rolls the
    /// transaction back, so the reported counts are the ones a real run would
    /// produce and `db_version` stays where it was.
    pub fn dissolve_stream(
        &self,
        stream_id: &str,
        mode: DissolveMode,
    ) -> Result<DissolveOutcome, DbError> {
        let tx = self.write_tx()?;

        let retained: u64 = tx.query_row(
            "SELECT COUNT(*) FROM events WHERE stream_id = ?1 AND assignment_source = 'user'",
            params![stream_id],
            |row| row.get(0),
        )?;

        let released = tx.execute(
            "UPDATE events SET stream_id = NULL, assignment_source = NULL \
             WHERE stream_id = ?1 AND (assignment_source IS NULL OR assignment_source != 'user')",
            params![stream_id],
        )? as u64;

        let retired = retained == 0
            && tx.execute("DELETE FROM streams WHERE id = ?1", params![stream_id])? > 0;

        if released > 0 || retired {
            Self::bump_db_version_in_transaction(&tx)?;
        }

        match mode {
            DissolveMode::Apply => tx.commit()?,
            DissolveMode::DryRun => tx.rollback()?,
        }

        Ok(DissolveOutcome {
            released,
            retained,
            retired,
        })
    }

    /// Releases the attribution carried by pane focus that could never have earned it.
    ///
    /// **The selection rule is structural, and it is hardcoded here so this cannot
    /// become a bulk-release primitive.** A `tmux_pane_focus` event with a NULL
    /// `session_id` is the one population no writer in this tree can legitimately
    /// have assigned, because it exposes nothing any of them read:
    ///
    /// - every session-keyed writer (`assign_events_by_session_id`,
    ///   `inherit_stream_for_session`, `claim_unassigned_events_for_classified_sessions`,
    ///   `reassign_session_as_user`, …) filters on `session_id = ?`, which a NULL
    ///   never matches;
    /// - the only id-keyed machine writer, `assign_unassigned_events_by_ids`, is fed
    ///   exclusively by `unattributed_terminal_focus_events`, whose query reads
    ///   `type = 'window_focus'` — so the window-run classifier never sees a pane at
    ///   all, and would have nothing to read if it did: 0 of 102,748 pane events carry
    ///   a window title and 0 carry a `window_app_id`;
    /// - `terminal_focus` and `artifact_reference` attribution are likewise
    ///   `window_focus`-only.
    ///
    /// What did assign them was the deleted cwd propagator, and it wrote
    /// `assignment_source = 'inferred'` — the classifier's own value — so the cleanup
    /// that released 777,583 of its rows could not find these by source. Preferring
    /// the structure over that signature is deliberate: on the live corpus the
    /// cwd-matching heuristic identifies 44,228 of the 47,081 structural candidates
    /// (94.0%), and the 2,853 it misses are exactly as unattributable as the rest.
    ///
    /// Release means both columns go NULL, so a released event is indistinguishable
    /// from one never classified — anything less and the passes that read
    /// `stream_id IS NULL` would skip it. An event carrying neither is already clean
    /// and is not counted, which makes a second run report zero.
    ///
    /// A human's assignment is never touched. None exists on this population today,
    /// and the guard stands anyway: only a human can speak for a human's verdict.
    /// `retained` reports what the guard held back.
    ///
    /// No event row is ever deleted and no stream row is retired — the streams these
    /// events were filed under may hold legitimate work too, so judging them stays
    /// `tt streams dissolve`.
    ///
    /// `ReleaseMode::DryRun` runs the identical statements and rolls the transaction
    /// back, so the reported counts are the ones a real run would produce and
    /// `db_version` stays where it was.
    pub fn release_unattributable_pane_focus(
        &self,
        mode: ReleaseMode,
    ) -> Result<ReleaseOutcome, DbError> {
        let tx = self.write_tx()?;

        let retained: u64 = tx.query_row(
            &format!(
                "SELECT COUNT(*) FROM events \
                 WHERE {UNATTRIBUTABLE_PANE_FOCUS_SQL} AND assignment_source = 'user'"
            ),
            [],
            |row| row.get(0),
        )?;

        // Counted before the update, which is about to null the column it reads.
        let streams_affected: u64 = tx.query_row(
            &format!(
                "SELECT COUNT(DISTINCT stream_id) FROM events \
                 WHERE {UNATTRIBUTABLE_PANE_FOCUS_SQL} AND {RELEASABLE_ATTRIBUTION_SQL}"
            ),
            [],
            |row| row.get(0),
        )?;

        let released = tx.execute(
            &format!(
                "UPDATE events SET stream_id = NULL, assignment_source = NULL \
                 WHERE {UNATTRIBUTABLE_PANE_FOCUS_SQL} AND {RELEASABLE_ATTRIBUTION_SQL}"
            ),
            [],
        )? as u64;

        if released > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }

        match mode {
            ReleaseMode::Apply => tx.commit()?,
            ReleaseMode::DryRun => tx.rollback()?,
        }

        Ok(ReleaseOutcome {
            released,
            retained,
            streams_affected,
        })
    }

    /// Re-points every source stream's events at `into_id`, then retires the sources.
    ///
    /// The counterpart to dissolution: dissolving says *this was never work*, merging
    /// says *this was work, in a stream that already exists*. It is what corrects a
    /// real initiative that was minted once per week — rename the weeks to one name,
    /// then collapse them here.
    ///
    /// **Human assignments move too**, keeping their `assignment_source`. A merge
    /// changes which row holds the work, not the human's judgement about what the
    /// work was, so releasing those events (as dissolution does) would discard a
    /// verdict that is still correct.
    ///
    /// **Pending proposals move too.** One naming a source has an obvious correct home —
    /// the target — so re-pointing it preserves the classifier's verdict exactly as
    /// re-pointing an event does, and makes the two orders agree: accepting then merging
    /// and merging then accepting land the same events on the same stream. Stranding it,
    /// the way [`Self::dissolve_stream`] must, would be gratuitous here, because that
    /// command has no target to offer and this one does. Only `pending` rows are touched:
    /// an accepted, rejected or superseded proposal is a historical record, and
    /// [`Self::has_rejected_proposal`] reads the rejected ones to suppress future answers.
    ///
    /// Tags move without duplicating: `INSERT OR IGNORE` means `tags_moved` counts
    /// only the tags the target did not already carry. The source's own tag rows
    /// cascade away with its stream row.
    ///
    /// No event row is ever deleted. Every source is handled in one transaction, so
    /// a failure part-way through leaves no half-merged initiative.
    ///
    /// The target is marked `needs_recompute` because its materialized times no
    /// longer match its events; only `tt recompute` writes those totals. Its
    /// `updated_at` is deliberately left alone — `tt streams` renders that column as
    /// "times last computed", and a merge invalidates those times rather than
    /// refreshing them.
    ///
    /// `MergeMode::DryRun` runs the identical statements and rolls the transaction
    /// back, so the reported counts are the ones a real run would produce and
    /// `db_version` stays where it was.
    ///
    /// # Errors
    ///
    /// `MergeIntoSelf` when `into_id` also appears in `from_ids`, and
    /// `MergeTargetNotFound` when `into_id` names no live row — `events.stream_id` is
    /// a foreign key, so a stale reference would otherwise surface as an opaque
    /// constraint failure part-way through the write.
    pub fn merge_streams(
        &self,
        from_ids: &[String],
        into_id: &str,
        mode: MergeMode,
    ) -> Result<Vec<MergedSource>, DbError> {
        if from_ids.iter().any(|from_id| from_id == into_id) {
            return Err(DbError::MergeIntoSelf {
                stream_id: into_id.to_string(),
            });
        }

        let tx = self.write_tx()?;
        if !tx.query_row(STREAM_EXISTS_SQL, params![into_id], |row| row.get(0))? {
            return Err(DbError::MergeTargetNotFound {
                stream_id: into_id.to_string(),
            });
        }

        let mut merged = Vec::with_capacity(from_ids.len());
        for from_id in from_ids {
            let user_events_moved: u64 = tx.query_row(
                "SELECT COUNT(*) FROM events WHERE stream_id = ?1 AND assignment_source = 'user'",
                params![from_id],
                |row| row.get(0),
            )?;
            let events_moved = tx.execute(
                "UPDATE events SET stream_id = ?1 WHERE stream_id = ?2",
                params![into_id, from_id],
            )? as u64;
            let tags_moved = tx.execute(
                "INSERT OR IGNORE INTO stream_tags (stream_id, tag) \
                 SELECT ?1, tag FROM stream_tags WHERE stream_id = ?2",
                params![into_id, from_id],
            )? as u64;
            let proposals_repointed = tx.execute(
                "UPDATE proposals SET proposed_stream_id = ?1 \
                 WHERE proposed_stream_id = ?2 AND status = 'pending'",
                params![into_id, from_id],
            )? as u64;
            let retired = tx.execute("DELETE FROM streams WHERE id = ?1", params![from_id])? > 0;
            merged.push(MergedSource {
                stream_id: from_id.clone(),
                events_moved,
                user_events_moved,
                tags_moved,
                proposals_repointed,
                retired,
            });
        }

        if merged.iter().any(|source| source.events_moved > 0) {
            tx.execute(
                "UPDATE streams SET needs_recompute = 1 WHERE id = ?1",
                params![into_id],
            )?;
        }
        if merged.iter().any(MergedSource::changed_anything) {
            Self::bump_db_version_in_transaction(&tx)?;
        }

        match mode {
            MergeMode::Apply => tx.commit()?,
            MergeMode::DryRun => tx.rollback()?,
        }

        Ok(merged)
    }

    /// Sets a stream's display name.
    ///
    /// The operator's tool for stripping a week suffix off a real initiative
    /// (`workorder-5: IPI envs + wo-005 (Jun14-20)`), after which the weeks that now
    /// share a name collapse via `merge_streams`. Names carry no uniqueness
    /// constraint, so renaming two streams to one name is allowed and expected.
    ///
    /// The name is stored [normalized](tt_core::normalize_stream_name), the same invariant
    /// `insert_stream` holds — a rename that could reintroduce a whitespace-only variant
    /// would reopen the hole from the other side, and here it would also defeat the very
    /// `merge_streams` step this command exists to set up.
    pub fn rename_stream(&self, stream_id: &str, name: &str) -> Result<(), DbError> {
        let name = tt_core::normalize_stream_name(name);
        let tx = self.write_tx()?;
        let count = tx.execute(
            "UPDATE streams SET name = ?1, updated_at = ?2 WHERE id = ?3",
            params![name, format_timestamp(Utc::now()), stream_id],
        )?;
        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// How many events point at `stream_id`, whoever assigned them.
    pub fn count_events_by_stream(&self, stream_id: &str) -> Result<u64, DbError> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE stream_id = ?1",
                params![stream_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Deletes `user_message` events belonging to non-user agent sessions.
    ///
    /// When sessions are reclassified (e.g., from `user` to `agent`), stale
    /// `user_message` events from previous ingestions create false focus signals
    /// in the allocation algorithm. This cleans them up.
    ///
    /// Returns the number of events deleted.
    pub fn delete_non_user_message_events(&self) -> Result<u64, DbError> {
        let tx = self.write_tx()?;
        let count = tx.execute(
            "DELETE FROM events WHERE type = 'user_message' \
             AND session_id IN (\
               SELECT session_id FROM agent_sessions WHERE session_type != 'user'\
             )",
            [],
        )?;
        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(count as u64)
    }

    /// Deletes `user_message` events that the current extractor no longer derives.
    ///
    /// Event writes are `INSERT OR IGNORE`, so a change to what counts as a user
    /// message adds the corrected rows but leaves the superseded ones behind.
    /// This closes that gap: for every session in `derived`, any stored
    /// `user_message` whose timestamp is absent from that session's freshly
    /// derived set is removed.
    ///
    /// Sessions absent from `derived` are left untouched. That is deliberate —
    /// a caller can only re-derive sessions whose source transcripts it holds,
    /// and an empty keep-set for an unseen session would delete real history.
    /// A session that *is* present with an empty set is different: the extractor
    /// looked and found no human messages, so every stored one goes.
    ///
    /// Returns the number of events deleted.
    pub fn prune_user_message_events(
        &self,
        derived: &HashMap<String, HashSet<DateTime<Utc>>>,
    ) -> Result<u64, DbError> {
        if derived.is_empty() {
            return Ok(0);
        }

        let tx = self.write_tx()?;
        tx.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS derived_sessions (session_id TEXT PRIMARY KEY);\
             CREATE TEMP TABLE IF NOT EXISTS derived_user_messages (\
               session_id TEXT NOT NULL, timestamp TEXT NOT NULL,\
               PRIMARY KEY (session_id, timestamp)) WITHOUT ROWID;\
             DELETE FROM derived_sessions;\
             DELETE FROM derived_user_messages;",
        )?;

        {
            let mut session_stmt =
                tx.prepare("INSERT OR IGNORE INTO derived_sessions (session_id) VALUES (?1)")?;
            let mut message_stmt = tx.prepare(
                "INSERT OR IGNORE INTO derived_user_messages (session_id, timestamp) \
                 VALUES (?1, ?2)",
            )?;
            for (session_id, timestamps) in derived {
                session_stmt.execute(params![session_id])?;
                for timestamp in timestamps {
                    message_stmt.execute(params![session_id, format_timestamp(*timestamp)])?;
                }
            }
        }

        let count = tx.execute(
            "DELETE FROM events \
             WHERE type = 'user_message' \
               AND session_id IN (SELECT session_id FROM derived_sessions) \
               AND NOT EXISTS (\
                 SELECT 1 FROM derived_user_messages d \
                 WHERE d.session_id = events.session_id \
                   AND d.timestamp = events.timestamp\
               )",
            [],
        )?;

        tx.execute_batch("DELETE FROM derived_sessions; DELETE FROM derived_user_messages;")?;

        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(count as u64)
    }

    /// Updates time fields for multiple streams.
    ///
    /// Also clears the `needs_recompute` flag and updates `updated_at`.
    ///
    /// Returns the number of streams updated.
    pub fn update_stream_times(&self, times: &[tt_core::StreamTime]) -> Result<u64, DbError> {
        let tx = self.write_tx()?;
        let mut count = 0u64;

        {
            let now = format_timestamp(Utc::now());
            let mut stmt = tx.prepare(
                "UPDATE streams SET time_direct_ms = ?1, time_delegated_ms = ?2, updated_at = ?3, needs_recompute = 0
                 WHERE id = ?4",
            )?;

            for time in times {
                let rows = stmt.execute(params![
                    time.time_direct_ms,
                    time.time_delegated_ms,
                    now,
                    time.stream_id,
                ])?;
                count += rows as u64;
            }
        }

        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(count)
    }

    /// Marks streams as needing recomputation.
    ///
    /// Returns the number of streams updated.
    pub fn mark_streams_for_recompute(&self, stream_ids: &[&str]) -> Result<u64, DbError> {
        if stream_ids.is_empty() {
            return Ok(0);
        }

        let placeholders = stream_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("UPDATE streams SET needs_recompute = 1 WHERE id IN ({placeholders})");

        let params: Vec<&dyn rusqlite::ToSql> = stream_ids
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();

        let tx = self.write_tx()?;
        let count = tx.execute(&sql, params.as_slice())?;
        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(count as u64)
    }

    /// Gets streams that need recomputation.
    pub fn get_streams_needing_recompute(&self) -> Result<Vec<Stream>, DbError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {STREAM_COLUMNS} FROM streams WHERE needs_recompute = 1"
        ))?;

        let mut streams = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            streams.push(Self::row_to_stream(row)?);
        }
        Ok(streams)
    }

    // ========== Tag Methods ==========

    /// Adds a tag to a stream.
    ///
    /// Idempotent: adding a tag that already exists is a no-op.
    pub fn add_tag(&self, stream_id: &str, tag: &str) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO stream_tags (stream_id, tag) VALUES (?1, ?2)",
            params![stream_id, tag],
        )?;
        Ok(())
    }

    /// Gets all tags for a stream.
    ///
    /// Returns tags sorted alphabetically.
    pub fn get_tags(&self, stream_id: &str) -> Result<Vec<String>, DbError> {
        let mut stmt = self
            .conn
            .prepare("SELECT tag FROM stream_tags WHERE stream_id = ?1 ORDER BY tag ASC")?;

        let rows = stmt.query_map(params![stream_id], |row| row.get(0))?;
        rows.collect::<Result<Vec<String>, _>>().map_err(Into::into)
    }

    /// Removes a tag from a stream.
    pub fn delete_tag(&self, stream_id: &str, tag: &str) -> Result<(), DbError> {
        self.conn.execute(
            "DELETE FROM stream_tags WHERE stream_id = ?1 AND tag = ?2",
            params![stream_id, tag],
        )?;
        Ok(())
    }

    /// Gets all tags grouped by stream ID.
    ///
    /// Returns a vector of (`stream_id`, tags) pairs.
    /// Only streams with at least one tag are included.
    pub fn get_all_tags(&self) -> Result<Vec<(String, Vec<String>)>, DbError> {
        let mut stmt = self
            .conn
            .prepare("SELECT stream_id, tag FROM stream_tags ORDER BY stream_id ASC, tag ASC")?;

        let rows = stmt.query_map([], |row| {
            let stream_id: String = row.get(0)?;
            let tag: String = row.get(1)?;
            Ok((stream_id, tag))
        })?;

        let mut result: Vec<(String, Vec<String>)> = Vec::new();
        for row_result in rows {
            let (stream_id, tag) = row_result?;

            // Since rows are ordered by stream_id, we only need to check the last entry
            if let Some((last_id, tags)) = result.last_mut() {
                if last_id == &stream_id {
                    tags.push(tag);
                    continue;
                }
            }
            result.push((stream_id, vec![tag]));
        }
        Ok(result)
    }

    /// Gets all streams with their tags.
    ///
    /// Returns a vector of (Stream, tags) pairs.
    /// Streams without tags are included with an empty tag vector.
    pub fn get_streams_with_tags(&self) -> Result<Vec<(Stream, Vec<String>)>, DbError> {
        let streams = self.get_streams()?;
        let all_tags = self.get_all_tags()?;

        // Convert to HashMap for O(1) lookup instead of O(n) linear search
        let tags_map: std::collections::HashMap<_, _> = all_tags.into_iter().collect();

        let result = streams
            .into_iter()
            .map(|stream| {
                let tags = tags_map.get(&stream.id).cloned().unwrap_or_default();
                (stream, tags)
            })
            .collect();

        Ok(result)
    }

    /// Stores a classifier proposal for later review.
    pub fn insert_proposal(&self, proposal: &Proposal) -> Result<(), DbError> {
        let event_ids = proposal
            .event_ids
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let tx = self.write_tx()?;
        let count = tx.execute(
            "INSERT INTO proposals (id, created_at, session_id, event_ids, proposed_stream_id, proposed_new_stream, confidence, reasoning, status, classifier_generation)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                proposal.id,
                format_timestamp(proposal.created_at),
                proposal.session_id,
                event_ids,
                proposal.proposed_stream_id,
                proposal.proposed_new_stream,
                proposal.confidence,
                proposal.reasoning,
                proposal.status.as_str(),
                proposal.classifier_generation,
            ],
        )?;
        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Retrieves proposals, optionally limited to one status.
    pub fn get_proposals(&self, status: Option<ProposalStatus>) -> Result<Vec<Proposal>, DbError> {
        let status = status.map(ProposalStatus::as_str);
        let mut stmt = match status {
            Some(_) => self.conn.prepare(
                "SELECT id, created_at, session_id, event_ids, proposed_stream_id, proposed_new_stream, confidence, reasoning, status, classifier_generation FROM proposals WHERE status = ?1 ORDER BY created_at ASC",
            )?,
            None => self.conn.prepare(
                "SELECT id, created_at, session_id, event_ids, proposed_stream_id, proposed_new_stream, confidence, reasoning, status, classifier_generation FROM proposals ORDER BY created_at ASC",
            )?,
        };
        let rows = match status {
            Some(status) => stmt.query_map(params![status], Self::row_to_proposal)?,
            None => stmt.query_map([], Self::row_to_proposal)?,
        };
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// The pending review queue, ordered by the attention reviewing each item resolves.
    ///
    /// A review queue spends the one resource this product exists to conserve, so the
    /// order it presents its items in *is* the product. [`Self::get_proposals`] orders by
    /// `created_at ASC`, which front-loads the stalest question and says nothing about
    /// what answering it is worth. Measured on the live queue of **477 pending
    /// proposals, the 12 that resolve the most attention carry 41.8% of all attention
    /// waiting in it** — so reviewing 12 items rather than 477 attributes nearly half the
    /// unattributed direct time. Age-ordering that queue spends a reviewer's attention at
    /// random; see root `AGENTS.md`, "The point: leverage".
    ///
    /// Attention is counted in events rather than milliseconds because a proposal is
    /// answered before its events are allocated: allocation needs a stream, and supplying
    /// one is exactly what the reviewer has not done yet. The count of attention-bearing
    /// events is the figure available at review time, and it ranks the same way.
    ///
    /// This is a **re-ordering and never a filter**. A proposal resolving no attention is
    /// still a real question and sorts last rather than disappearing, and a proposal
    /// naming a dissolved stream is listed too — [`PROPOSAL_IS_ANSWERABLE_SQL`] is
    /// deliberately not applied, because `tt proposals ls` is where a reviewer learns
    /// such a row exists. Ties fall back to `created_at ASC` and then the id, so equal
    /// attention still presents in one stable order.
    pub fn pending_proposals_by_attention(&self) -> Result<Vec<RankedProposal>, DbError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT p.id, p.created_at, p.session_id, p.event_ids, p.proposed_stream_id,
                    p.proposed_new_stream, p.confidence, p.reasoning, p.status,
                    p.classifier_generation,
                    CASE
                        WHEN p.session_id IS NOT NULL THEN (
                            SELECT COUNT(*) FROM events e
                            WHERE e.session_id = p.session_id
                              AND e.type IN {ATTENTION_EVENT_TYPES_SQL})
                        WHEN p.event_ids IS NOT NULL THEN (
                            SELECT COUNT(*) FROM events e
                            WHERE e.id IN (SELECT value FROM json_each(p.event_ids))
                              AND e.type IN {ATTENTION_EVENT_TYPES_SQL})
                        ELSE 0
                    END AS attention_events
             FROM proposals p
             WHERE p.status = 'pending'
             ORDER BY attention_events DESC, p.created_at ASC, p.id ASC"
        ))?;
        let rows = stmt.query_map([], |row| {
            let proposal = Self::row_to_proposal(row)?;
            let attention: i64 = row.get(10)?;
            let attention_events = u64::try_from(attention).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    10,
                    rusqlite::types::Type::Integer,
                    Box::new(error),
                )
            })?;
            Ok(RankedProposal {
                proposal,
                attention_events,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// The pending proposals naming any of `stream_ids`.
    ///
    /// `proposals.proposed_stream_id` has no foreign key, so retiring one of these
    /// streams leaves every pending proposal that named it unacceptable. The reader half
    /// of that is already handled — [`PROPOSAL_IS_ANSWERABLE_SQL`] stops a stranded row
    /// suppressing a fresh answer, and `tt proposals ls` marks it `(gone)` — and this is
    /// the half the *writer* owes: `tt streams dissolve` names what it strands, in its
    /// preview too, because that is where the operator decides.
    ///
    /// A read, and only a read. Naming a dangle must not create, retire or rewrite one:
    /// dissolution asserts the work never happened, so there is no stream to re-point a
    /// proposal to, and a status the human did not choose is not a command's to write.
    /// Ordered `created_at ASC` then id, matching [`Self::get_proposals`], so a report
    /// built from it is stable.
    pub fn pending_proposals_for_streams(
        &self,
        stream_ids: &[String],
    ) -> Result<Vec<StrandedProposal>, DbError> {
        if stream_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", stream_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, created_at, session_id, event_ids, proposed_stream_id, proposed_new_stream, confidence, reasoning, status, classifier_generation
             FROM proposals
             WHERE status = 'pending' AND proposed_stream_id IN ({placeholders})
             ORDER BY created_at ASC, id ASC"
        ))?;
        let rows = stmt.query_map(params_from_iter(stream_ids), Self::row_to_proposal)?;
        let mut stranded = Vec::new();
        for proposal in rows {
            let proposal = proposal?;
            // `proposed_stream_id IN (…)` cannot match a NULL, so this always binds.
            let Some(stream_id) = proposal.proposed_stream_id else {
                continue;
            };
            stranded.push(StrandedProposal {
                proposal_id: proposal.id,
                stream_id,
                event_count: proposal.event_ids.map_or(0, |event_ids| event_ids.len()),
            });
        }
        Ok(stranded)
    }

    /// Changes the review status of a proposal.
    pub fn set_proposal_status(&self, id: &str, status: ProposalStatus) -> Result<(), DbError> {
        let tx = self.write_tx()?;
        let count = tx.execute(
            "UPDATE proposals SET status = ?1 WHERE id = ?2",
            params![status.as_str(), id],
        )?;
        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Accepts a pending proposal and applies its entire assignment atomically.
    ///
    /// Creates a proposed stream when needed, assigns eligible events, marks the
    /// stream for recomputation, and advances the database version once.
    pub fn accept_proposal(&self, proposal_id: &str) -> Result<AcceptProposalOutcome, DbError> {
        let tx = self.write_tx()?;
        let proposal = Self::pending_proposal_in_transaction(&tx, proposal_id)?;
        let (stream_id, created_stream) = Self::accept_proposal_stream(&tx, &proposal)?;
        let events_assigned = Self::accept_proposal_events(&tx, &proposal, &stream_id)?;

        let status_updates = tx.execute(
            "UPDATE proposals SET status = 'accepted' WHERE id = ?1 AND status = 'pending'",
            params![proposal_id],
        )?;
        if status_updates != 1 {
            return Err(DbError::ProposalNotPending {
                proposal_id: proposal_id.to_string(),
            });
        }
        tx.execute(
            "UPDATE streams SET needs_recompute = 1 WHERE id = ?1",
            params![stream_id],
        )?;
        Self::bump_db_version_in_transaction(&tx)?;
        tx.commit()?;

        Ok(AcceptProposalOutcome {
            stream_id,
            created_stream,
            events_assigned,
        })
    }

    /// Rejects a pending proposal and optionally applies a replacement user assignment atomically.
    pub fn reject_proposal(
        &self,
        proposal_id: &str,
        target_stream_reference: Option<&str>,
    ) -> Result<RejectProposalOutcome, DbError> {
        let tx = self.write_tx()?;
        let proposal = Self::pending_proposal_in_transaction(&tx, proposal_id)?;
        let (stream_id, events_assigned) = match target_stream_reference {
            Some(stream_reference) => {
                let stream_id = Self::resolve_stream_id_in_transaction(&tx, stream_reference)?
                    .ok_or_else(|| DbError::RejectTargetStreamNotFound {
                        stream_reference: stream_reference.to_owned(),
                    })?;
                let events_assigned = Self::accept_proposal_events(&tx, &proposal, &stream_id)?;
                tx.execute(
                    "UPDATE streams SET needs_recompute = 1 WHERE id = ?1",
                    params![stream_id],
                )?;
                (Some(stream_id), events_assigned)
            }
            None => (None, 0),
        };
        let status_updates = tx.execute(
            "UPDATE proposals SET status = 'rejected' WHERE id = ?1 AND status = 'pending'",
            params![proposal_id],
        )?;
        if status_updates != 1 {
            return Err(DbError::ProposalNotPending {
                proposal_id: proposal_id.to_owned(),
            });
        }
        Self::bump_db_version_in_transaction(&tx)?;
        tx.commit()?;

        Ok(RejectProposalOutcome {
            stream_id,
            events_assigned,
        })
    }

    fn pending_proposal_in_transaction(
        tx: &rusqlite::Transaction<'_>,
        proposal_id: &str,
    ) -> Result<Proposal, DbError> {
        let proposal = tx
            .query_row(
                "SELECT id, created_at, session_id, event_ids, proposed_stream_id, proposed_new_stream, confidence, reasoning, status, classifier_generation
                 FROM proposals WHERE id = ?1",
                params![proposal_id],
                Self::row_to_proposal,
            )
            .optional()?
            .ok_or_else(|| DbError::ProposalNotFound {
                proposal_id: proposal_id.to_owned(),
            })?;
        match proposal.status {
            ProposalStatus::Pending => Ok(proposal),
            ProposalStatus::Accepted | ProposalStatus::Rejected | ProposalStatus::Superseded => {
                Err(DbError::ProposalNotPending {
                    proposal_id: proposal_id.to_owned(),
                })
            }
        }
    }

    fn resolve_stream_id_in_transaction(
        tx: &rusqlite::Transaction<'_>,
        stream_reference: &str,
    ) -> Result<Option<String>, DbError> {
        for query in [
            "SELECT id FROM streams WHERE id = ?1",
            "SELECT id FROM streams WHERE slug = ?1",
            "SELECT id FROM streams WHERE name = ?1",
        ] {
            if let Some(stream_id) = tx
                .query_row(query, params![stream_reference], |row| row.get(0))
                .optional()?
            {
                return Ok(Some(stream_id));
            }
        }
        Ok(None)
    }

    fn accept_proposal_stream(
        tx: &rusqlite::Transaction<'_>,
        proposal: &Proposal,
    ) -> Result<(String, bool), DbError> {
        match (&proposal.proposed_stream_id, &proposal.proposed_new_stream) {
            (Some(stream_id), None) => {
                let stream_exists: bool =
                    tx.query_row(STREAM_EXISTS_SQL, params![stream_id], |row| row.get(0))?;
                if !stream_exists {
                    return Err(DbError::ProposedStreamNotFound {
                        stream_id: stream_id.clone(),
                    });
                }
                Ok((stream_id.clone(), false))
            }
            (None, Some(definition)) => {
                let definition: NewStreamProposal =
                    serde_json::from_str(definition).map_err(DbError::InvalidProposalNewStream)?;
                let name = tt_core::normalize_stream_name(&definition.name);
                // The second path that mints a stream from a model-authored name, so it
                // owes the same reuse check `stream_named` does. A proposal sits in the
                // queue for as long as a human takes to review it, and the classifier
                // keeps running meanwhile, so by accept time a stream may already carry
                // this name. Reusing it is what accepting *means* here — the human agreed
                // the work belongs under that name, not that a fresh row must exist.
                let (stream_id, created) =
                    match Self::find_stream_id_by_normalized_name_in(tx, &name)? {
                        Some(existing) => (existing, false),
                        None => (
                            Self::mint_stream_in(tx, &name, definition.description.as_deref())?,
                            true,
                        ),
                    };
                let mut insert_tag = tx.prepare(
                    "INSERT OR IGNORE INTO stream_tags (stream_id, tag) VALUES (?1, ?2)",
                )?;
                for tag in &definition.tags {
                    insert_tag.execute(params![stream_id, tag])?;
                }
                Ok((stream_id, created))
            }
            _ => Err(DbError::InvalidProposalStreamTarget {
                proposal_id: proposal.id.clone(),
            }),
        }
    }

    /// The id of the stream whose stored name normalizes to `wanted`, earliest first.
    ///
    /// The in-transaction twin of [`Self::find_stream_by_normalized_name`]; see there for
    /// why both sides are normalized and why the earliest match wins.
    fn find_stream_id_by_normalized_name_in(
        tx: &rusqlite::Transaction<'_>,
        wanted: &str,
    ) -> Result<Option<String>, DbError> {
        if wanted.is_empty() {
            return Ok(None);
        }
        let mut stmt = tx.prepare(
            "SELECT id, name FROM streams WHERE name IS NOT NULL
             ORDER BY created_at ASC, id ASC",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let stored: String = row.get(1)?;
            if tt_core::normalize_stream_name(&stored) == wanted {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// Inserts a stream under a name already normalized by the caller.
    fn mint_stream_in(
        tx: &rusqlite::Transaction<'_>,
        name: &str,
        description: Option<&str>,
    ) -> Result<String, DbError> {
        let stream_id: String = tx.query_row(
            "WITH random_id(value) AS (SELECT hex(randomblob(16)))
             SELECT lower(
                substr(value, 1, 8) || '-' || substr(value, 9, 4) || '-' ||
                substr(value, 13, 4) || '-' || substr(value, 17, 4) || '-' ||
                substr(value, 21, 12)
             ) FROM random_id",
            [],
            |row| row.get(0),
        )?;
        let now = format_timestamp(Utc::now());
        tx.execute(
            "INSERT INTO streams (id, created_at, updated_at, name, time_direct_ms, time_delegated_ms, first_event_at, last_event_at, needs_recompute, slug, description, color)
             VALUES (?1, ?2, ?3, ?4, 0, 0, NULL, NULL, 1, NULL, ?5, NULL)",
            params![stream_id, now, now, name, description],
        )?;
        Ok(stream_id)
    }

    fn accept_proposal_events(
        tx: &rusqlite::Transaction<'_>,
        proposal: &Proposal,
        stream_id: &str,
    ) -> Result<u64, DbError> {
        const EVENT_ID_CHUNK_SIZE: usize = 500;

        match (&proposal.session_id, &proposal.event_ids) {
            (Some(session_id), None) => Ok(tx.execute(
                "UPDATE events SET stream_id = ?1, assignment_source = 'user'
                 WHERE session_id = ?2 AND (assignment_source IS NULL OR assignment_source != 'user')",
                params![stream_id, session_id],
            )? as u64),
            (None, Some(event_ids)) => {
                let mut events_assigned = 0;
                for event_ids in event_ids.chunks(EVENT_ID_CHUNK_SIZE) {
                    let placeholders = std::iter::repeat_n("?", event_ids.len())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let sql = format!(
                        "UPDATE events SET stream_id = ?, assignment_source = 'user'
                         WHERE id IN ({placeholders})
                         AND (assignment_source IS NULL OR assignment_source != 'user')"
                    );
                    let mut parameters: Vec<&dyn rusqlite::ToSql> =
                        Vec::with_capacity(event_ids.len() + 1);
                    parameters.push(&stream_id);
                    parameters.extend(
                        event_ids
                            .iter()
                            .map(|event_id| event_id as &dyn rusqlite::ToSql),
                    );
                    events_assigned += tx.execute(&sql, params_from_iter(parameters))? as u64;
                }
                Ok(events_assigned)
            }
            _ => Err(DbError::InvalidProposalAssignmentTarget {
                proposal_id: proposal.id.clone(),
            }),
        }
    }

    /// Returns whether a stream proposal for a session was rejected.
    pub fn has_rejected_proposal(
        &self,
        session_id: &str,
        stream_id: &str,
    ) -> Result<bool, DbError> {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM proposals WHERE session_id = ?1 AND proposed_stream_id = ?2 AND status = 'rejected')",
                params![session_id, stream_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Retrieves the oldest pending event-level proposal for the exact event set.
    ///
    /// The duplicate guard for window runs: a run whose answer is already waiting for a
    /// human must not queue a second copy of the same question on every pass. It no
    /// longer holds the run back from being *asked* again — that is the caller's job and
    /// the caller no longer does it.
    ///
    /// Answerable proposals only, and deliberately narrower than
    /// [`Self::supersede_pending_proposals_for_events`]: one naming a dissolved stream
    /// suppresses nothing, because no reviewer can accept it and the run would be left
    /// with no answer anybody could act on.
    pub fn get_pending_proposal_for_events(
        &self,
        event_ids: &[String],
    ) -> Result<Option<Proposal>, DbError> {
        let event_ids = serde_json::to_string(event_ids)?;
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, created_at, session_id, event_ids, proposed_stream_id, proposed_new_stream, confidence, reasoning, status, classifier_generation
             FROM proposals p
             WHERE p.session_id IS NULL AND p.event_ids = ?1 AND p.status = 'pending'
               AND {PROPOSAL_IS_ANSWERABLE_SQL}
             ORDER BY p.created_at ASC LIMIT 1"
        ))?;
        stmt.query_row(params![event_ids], Self::row_to_proposal)
            .optional()
            .map_err(Into::into)
    }

    /// Returns whether a session already carries a proposal a reviewer could act on.
    ///
    /// The session counterpart of [`Self::get_pending_proposal_for_events`], and it
    /// exists because selection stopped doing this job: `unclassified_user_sessions` used
    /// to exclude such sessions in SQL, which answered the duplicate question and froze
    /// the session out of every later pass at the same time. Splitting them keeps the
    /// duplicate guard and drops the freeze.
    ///
    /// Answerable proposals only, for the same reason the event-level lookup is: a
    /// stranded proposal suppresses nothing, because accepting it is impossible.
    pub fn has_pending_proposal_for_session(&self, session_id: &str) -> Result<bool, DbError> {
        self.conn
            .query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM proposals p
                     WHERE p.session_id = ?1 AND p.status = 'pending'
                       AND {PROPOSAL_IS_ANSWERABLE_SQL})"
                ),
                params![session_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Retires the pending proposals covering one session, a later verdict having
    /// answered it.
    ///
    /// A proposal escalates a question to a human; it must not also retire the machine's
    /// ability to answer that question later. So when the classifier does answer — above
    /// the confidence threshold, applied to the events — the queued question is spent and
    /// is marked `superseded`. **Never `rejected`**: that is a human verdict
    /// `has_rejected_proposal` reads to suppress future answers, and manufacturing one
    /// would silence the classifier on this session for good.
    ///
    /// Wider than [`Self::has_pending_proposal_for_session`] on purpose: a proposal naming
    /// a dissolved stream must not *suppress* a fresh answer, but it is still a question
    /// about this session, and a verdict that answers it leaves nothing to strand.
    ///
    /// Bookkeeping, so it does not bump `db_version`. Superseding changes no assignment
    /// by itself; it only ever follows one, and that write has already signalled the
    /// daemon.
    pub fn supersede_pending_proposals_for_session(
        &self,
        session_id: &str,
    ) -> Result<u64, DbError> {
        let superseded = self.conn.execute(
            "UPDATE proposals SET status = 'superseded'
             WHERE session_id = ?1 AND status = 'pending'",
            params![session_id],
        )?;
        Ok(superseded as u64)
    }

    /// Retires the pending proposals covering one exact window-run event set.
    ///
    /// The event-level twin of [`Self::supersede_pending_proposals_for_session`]; see
    /// there for why this is not a rejection and why it does not bump `db_version`. Keyed
    /// on the exact set the proposal was filed against, so answering one run says nothing
    /// about any other.
    pub fn supersede_pending_proposals_for_events(
        &self,
        event_ids: &[String],
    ) -> Result<u64, DbError> {
        let event_ids = serde_json::to_string(event_ids)?;
        let superseded = self.conn.execute(
            "UPDATE proposals SET status = 'superseded'
             WHERE session_id IS NULL AND event_ids = ?1 AND status = 'pending'",
            params![event_ids],
        )?;
        Ok(superseded as u64)
    }

    /// Whether this exact window-run event set already carries an answerable pending
    /// proposal that `generation` produced.
    ///
    /// The gate that stops a bounded pass paying to re-ask a question it has already
    /// answered. Removing the old unconditional skip was right — a queue nobody reviews
    /// must not freeze a run out of ever being re-answered — but it left the opposite
    /// waste: 212 pending window-run proposals were re-classified identically every
    /// pass, out of a budget of 101 calls, while 71,635 focus events across 149 days
    /// stayed unattributed because the budget never reached them.
    ///
    /// Keyed on the generation rather than on mere existence, so bumping
    /// `tt_llm::CLASSIFIER_GENERATION` re-opens every queued question at once. A
    /// `NULL` generation — every row written before this column — matches nothing and is
    /// therefore re-asked, which is the correct treatment for a verdict whose author
    /// cannot be identified.
    ///
    /// Answerable proposals only, like every other suppression read: one naming a
    /// dissolved stream cannot be accepted, so it must not stop the run being asked.
    pub fn has_pending_proposal_for_events_at_generation(
        &self,
        event_ids: &[String],
        generation: u32,
    ) -> Result<bool, DbError> {
        let event_ids = serde_json::to_string(event_ids)?;
        self.conn
            .query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM proposals p
                     WHERE p.session_id IS NULL AND p.event_ids = ?1 AND p.status = 'pending'
                       AND p.classifier_generation = ?2
                       AND {PROPOSAL_IS_ANSWERABLE_SQL})"
                ),
                params![event_ids, generation],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Records that `generation` has now answered the questions waiting on one session.
    ///
    /// The other half of the gate, and without it the gate would be worth nothing: a
    /// re-asked proposal that kept its old stamp would be re-asked again on the next
    /// pass, and every pass after that. Stamping is what makes a generation bump cost
    /// one extra pass rather than an unbounded number.
    ///
    /// It changes nothing a reviewer sees — the proposal keeps its status, its stream,
    /// its confidence and its reasoning — so it is emphatically not a verdict of any
    /// kind, and in particular not a rejection. Bookkeeping, so it does not bump
    /// `db_version`.
    ///
    /// No answerability filter, matching
    /// [`Self::supersede_pending_proposals_for_session`]: a stranded proposal suppresses
    /// nothing on the read side either way, so narrowing here would only leave a row
    /// carrying a stamp that misstates who last looked at it.
    pub fn stamp_pending_proposals_for_session(
        &self,
        session_id: &str,
        generation: u32,
    ) -> Result<u64, DbError> {
        let stamped = self.conn.execute(
            "UPDATE proposals SET classifier_generation = ?2
             WHERE session_id = ?1 AND status = 'pending'",
            params![session_id, generation],
        )?;
        Ok(stamped as u64)
    }

    /// Records that `generation` has now answered the question waiting on one exact
    /// window-run event set.
    ///
    /// The event-level twin of [`Self::stamp_pending_proposals_for_session`]; see there
    /// for why this is not a verdict and why it does not bump `db_version`. This is the
    /// write [`Self::has_pending_proposal_for_events_at_generation`] reads.
    pub fn stamp_pending_proposals_for_events(
        &self,
        event_ids: &[String],
        generation: u32,
    ) -> Result<u64, DbError> {
        let event_ids = serde_json::to_string(event_ids)?;
        let stamped = self.conn.execute(
            "UPDATE proposals SET classifier_generation = ?2
             WHERE session_id IS NULL AND event_ids = ?1 AND status = 'pending'",
            params![event_ids, generation],
        )?;
        Ok(stamped as u64)
    }

    /// Returns whether a new-stream proposal for a session was rejected.
    pub fn has_rejected_new_stream_proposal(&self, session_id: &str) -> Result<bool, DbError> {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM proposals WHERE session_id = ?1 AND proposed_stream_id IS NULL AND status = 'rejected')",
                params![session_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Records the prompt count at classification time without changing `db_version`.
    pub fn record_classification(
        &self,
        session_id: &str,
        prompt_count: u32,
    ) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT INTO classified_sessions (session_id, classified_at, prompt_count)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(session_id) DO UPDATE SET classified_at = excluded.classified_at, prompt_count = excluded.prompt_count",
            params![session_id, format_timestamp(Utc::now()), prompt_count],
        )?;
        Ok(())
    }

    /// Lists sessions eligible for their one re-check.
    pub fn get_recheck_candidates(&self) -> Result<Vec<(String, u32)>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, prompt_count FROM classified_sessions WHERE rechecked = 0 ORDER BY classified_at ASC",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Marks a session as having consumed its re-check without changing `db_version`.
    pub fn mark_rechecked(&self, session_id: &str) -> Result<(), DbError> {
        self.conn.execute(
            "UPDATE classified_sessions SET rechecked = 1 WHERE session_id = ?1",
            params![session_id],
        )?;
        Ok(())
    }

    /// Resolves a stream by ID, slug, or name.
    ///
    /// First checks if the query matches a stream ID, then slug, then name.
    /// Returns None if no matching stream is found.
    pub fn resolve_stream(&self, query: &str) -> Result<Option<Stream>, DbError> {
        // First try by ID
        if let Some(stream) = self.get_stream(query)? {
            return Ok(Some(stream));
        }

        // Then try by slug
        if let Some(stream) = self.get_stream_by_slug(query)? {
            return Ok(Some(stream));
        }

        // Then try by name
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {STREAM_COLUMNS} FROM streams WHERE name = ?1"
        ))?;

        let mut rows = stmt.query(params![query])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::row_to_stream(row)?)),
            None => Ok(None),
        }
    }

    /// Helper to convert a row to a `StoredEvent`.
    ///
    /// Expects the row to have columns in this order:
    /// `id`, `timestamp`, `type`, `source`, `machine_id`, `schema_version`, `cwd`, `git_project`,
    /// `git_workspace`, `pane_id`, `tmux_session`, `window_index`, `status`, `idle_duration_ms`,
    /// `action`, `session_id`, `stream_id`, `assignment_source`, `window_app_id`, `window_title`
    ///
    /// Returns `None` if the row has malformed timestamp (with a warning logged).
    fn row_to_event(row: &rusqlite::Row<'_>) -> Result<Option<StoredEvent>, rusqlite::Error> {
        let id: String = row.get(0)?;
        let timestamp_str: String = row.get(1)?;
        let event_type_str: String = row.get(2)?;
        let source: String = row.get(3)?;
        let machine_id: Option<String> = row.get(4)?;
        let schema_version: i32 = row.get(5)?;
        let cwd: Option<String> = row.get(6)?;
        let git_project: Option<String> = row.get(7)?;
        let git_workspace: Option<String> = row.get(8)?;
        let pane_id: Option<String> = row.get(9)?;
        let tmux_session: Option<String> = row.get(10)?;
        let window_index: Option<u32> = row.get(11)?;
        let status: Option<String> = row.get(12)?;
        let idle_duration_ms: Option<i64> = row.get(13)?;
        let action: Option<String> = row.get(14)?;
        let session_id: Option<String> = row.get(15)?;
        let stream_id: Option<String> = row.get(16)?;
        let assignment_source: Option<String> = row.get(17)?;
        let window_app_id: Option<String> = row.get(18)?;
        let window_title: Option<String> = row.get(19)?;

        let timestamp = match DateTime::parse_from_rfc3339(&timestamp_str) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(e) => {
                tracing::warn!(event_id = %id, error = %e, "skipping event with malformed timestamp");
                return Ok(None);
            }
        };

        let event_type = match event_type_str.parse::<tt_core::EventType>() {
            Ok(event_type) => event_type,
            Err(e) => {
                tracing::warn!(
                    event_id = %id,
                    event_type = %event_type_str,
                    error = %e,
                    "skipping event with unknown type"
                );
                return Ok(None);
            }
        };

        let mut event = StoredEvent {
            id,
            timestamp,
            event_type,
            source,
            machine_id,
            schema_version,
            pane_id,
            tmux_session,
            window_index,
            git_project,
            git_workspace,
            status,
            idle_duration_ms,
            window_app_id,
            window_title,
            action,
            cwd,
            session_id,
            stream_id,
            assignment_source,
            data: serde_json::Value::Null,
        };
        // Populate data field from explicit fields for AllocatableEvent::data()
        event.data = event.build_data_json();
        Ok(Some(event))
    }

    /// Helper to convert a row to a Stream.
    fn row_to_stream(row: &rusqlite::Row<'_>) -> Result<Stream, rusqlite::Error> {
        let id: String = row.get(0)?;
        let created_at_str: String = row.get(1)?;
        let updated_at_str: String = row.get(2)?;
        let name: Option<String> = row.get(3)?;
        let time_direct_ms: i64 = row.get(4)?;
        let time_delegated_ms: i64 = row.get(5)?;
        let first_event_at_str: Option<String> = row.get(6)?;
        let last_event_at_str: Option<String> = row.get(7)?;
        let needs_recompute: i32 = row.get(8)?;
        let slug: Option<String> = row.get(9)?;
        let description: Option<String> = row.get(10)?;
        let color: Option<String> = row.get(11)?;

        let created_at = parse_stream_timestamp(&id, "created_at", &created_at_str)?;
        let updated_at = parse_stream_timestamp(&id, "updated_at", &updated_at_str)?;
        let first_event_at = first_event_at_str
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc));
        let last_event_at = last_event_at_str
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        Ok(Stream {
            id,
            name,
            slug,
            description,
            color,
            created_at,
            updated_at,
            time_direct_ms,
            time_delegated_ms,
            first_event_at,
            last_event_at,
            needs_recompute: needs_recompute != 0,
        })
    }

    fn row_to_proposal(row: &rusqlite::Row<'_>) -> Result<Proposal, rusqlite::Error> {
        let created_at: String = row.get(1)?;
        let event_ids: Option<String> = row.get(3)?;
        let created_at = DateTime::parse_from_rfc3339(&created_at)
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?
            .with_timezone(&Utc);
        let event_ids = event_ids
            .map(|ids| {
                serde_json::from_str(&ids).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })
            })
            .transpose()?;
        let status: String = row.get(8)?;
        let status = ProposalStatus::from_db(&status).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                8,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
        Ok(Proposal {
            id: row.get(0)?,
            created_at,
            session_id: row.get(2)?,
            event_ids,
            proposed_stream_id: row.get(4)?,
            proposed_new_stream: row.get(5)?,
            confidence: row.get(6)?,
            reasoning: row.get(7)?,
            status,
            classifier_generation: row.get(9)?,
        })
    }

    // ========== Agent Session Methods ==========

    /// Insert or update an agent session entry.
    ///
    /// Uses `INSERT ... ON CONFLICT DO UPDATE` for idempotent upserts.
    /// If a session with the same ID already exists, all fields are updated.
    pub fn upsert_agent_session(
        &self,
        entry: &tt_core::session::AgentSession,
        machine_id: Option<&str>,
    ) -> Result<(), DbError> {
        let user_prompts_json =
            serde_json::to_string(&entry.user_prompts).unwrap_or_else(|_| "[]".to_string());
        self.conn.execute(
            "INSERT INTO agent_sessions (session_id, source, parent_session_id, project_path, project_name, start_time, end_time, message_count, summary, user_prompts, starting_prompt, assistant_message_count, tool_call_count, session_type, machine_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT(session_id) DO UPDATE SET
                source = excluded.source,
                parent_session_id = excluded.parent_session_id,
                project_path = excluded.project_path,
                project_name = excluded.project_name,
                start_time = excluded.start_time,
                end_time = excluded.end_time,
                message_count = excluded.message_count,
                summary = excluded.summary,
                user_prompts = excluded.user_prompts,
                starting_prompt = excluded.starting_prompt,
                assistant_message_count = excluded.assistant_message_count,
                tool_call_count = excluded.tool_call_count,
                session_type = excluded.session_type,
                machine_id = excluded.machine_id",
            params![
                entry.session_id,
                entry.source.as_str(),
                entry.parent_session_id,
                entry.project_path,
                entry.project_name,
                format_timestamp(entry.start_time),
                format_timestamp_opt(entry.end_time),
                entry.message_count,
                entry.summary,
                user_prompts_json,
                entry.starting_prompt,
                entry.assistant_message_count,
                entry.tool_call_count,
                entry.session_type.as_str(),
                machine_id,
            ],
        )?;
        Ok(())
    }

    /// Get agent sessions that overlap with a time range.
    ///
    /// A session overlaps if:
    /// - Its `start_time` is at or before the range end, AND
    /// - Its `end_time` is at or after the range start (or is `NULL` for ongoing sessions)
    ///
    /// Sessions are returned ordered by `start_time` ascending.
    /// Sessions with malformed timestamps in the database are skipped with a warning.
    pub fn agent_sessions_in_range(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<tt_core::session::AgentSession>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, source, parent_session_id, project_path, project_name, start_time, end_time, message_count, summary, user_prompts, starting_prompt, assistant_message_count, tool_call_count, session_type
             FROM agent_sessions
             WHERE start_time <= ?2 AND (end_time IS NULL OR end_time >= ?1)
             ORDER BY start_time"
        )?;

        let mut sessions = Vec::new();
        let mut rows = stmt.query(params![format_timestamp(start), format_timestamp(end)])?;

        while let Some(row) = rows.next()? {
            let session_id: String = row.get(0)?;
            let source_str: String = row.get(1)?;
            let start_time_str: String = row.get(5)?;
            let end_time_str: Option<String> = row.get(6)?;
            let user_prompts_str: Option<String> = row.get(9)?;

            let start_time = match DateTime::parse_from_rfc3339(&start_time_str) {
                Ok(dt) => dt.with_timezone(&Utc),
                Err(e) => {
                    tracing::warn!(session_id, error = %e, "skipping session with malformed start_time");
                    continue;
                }
            };

            let end_time = match end_time_str {
                Some(s) => match DateTime::parse_from_rfc3339(&s) {
                    Ok(dt) => Some(dt.with_timezone(&Utc)),
                    Err(e) => {
                        tracing::warn!(session_id, error = %e, "skipping session with malformed end_time");
                        continue;
                    }
                },
                None => None,
            };

            let user_prompts: Vec<String> = user_prompts_str
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();

            sessions.push(tt_core::session::AgentSession {
                session_id,
                source: source_str.parse().unwrap_or_default(),
                parent_session_id: row.get(2)?,
                session_type: row.get::<_, String>(13)?.parse().unwrap_or_default(),
                project_path: row.get(3)?,
                project_name: row.get(4)?,
                start_time,
                end_time,
                message_count: row.get(7)?,
                summary: row.get(8)?,
                user_prompts,
                starting_prompt: row.get(10)?,
                assistant_message_count: row.get(11)?,
                tool_call_count: row.get(12)?,
                // Not stored in database - events are created during indexing
                user_message_timestamps: Vec::new(),
                tool_call_timestamps: Vec::new(),
            });
        }

        Ok(sessions)
    }

    /// Reads one `agent_sessions` row selected with [`AGENT_SESSION_COLUMNS`].
    fn row_to_agent_session(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<(tt_core::session::AgentSession, Option<String>)> {
        let start_time: String = row.get(5)?;
        let end_time: Option<String> = row.get(6)?;
        let prompts: Option<String> = row.get(9)?;
        let start_time = DateTime::parse_from_rfc3339(&start_time)
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?
            .with_timezone(&Utc);
        let end_time = end_time
            .map(|value| {
                DateTime::parse_from_rfc3339(&value)
                    .map(|time| time.with_timezone(&Utc))
                    .map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            6,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })
            })
            .transpose()?;
        let prompts = prompts
            .map(|value| {
                serde_json::from_str(&value).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        9,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })
            })
            .transpose()?
            .unwrap_or_default();
        Ok((
            tt_core::session::AgentSession {
                session_id: row.get(0)?,
                source: row.get::<_, String>(1)?.parse().unwrap_or_default(),
                parent_session_id: row.get(2)?,
                session_type: row.get::<_, String>(13)?.parse().unwrap_or_default(),
                project_path: row.get(3)?,
                project_name: row.get(4)?,
                start_time,
                end_time,
                message_count: row.get(7)?,
                summary: row.get(8)?,
                user_prompts: prompts,
                starting_prompt: row.get(10)?,
                assistant_message_count: row.get(11)?,
                tool_call_count: row.get(12)?,
                user_message_timestamps: Vec::new(),
                tool_call_timestamps: Vec::new(),
            },
            row.get(14)?,
        ))
    }

    /// Retrieves one agent session together with its originating machine ID.
    pub fn get_agent_session(
        &self,
        session_id: &str,
    ) -> Result<Option<(tt_core::session::AgentSession, Option<String>)>, DbError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {AGENT_SESSION_COLUMNS} FROM agent_sessions WHERE session_id = ?1"
        ))?;
        stmt.query_row(params![session_id], Self::row_to_agent_session)
            .optional()
            .map_err(Into::into)
    }

    /// Lists the user sessions a classification pass should spend LLM calls on,
    /// newest first, with the machine each ran on.
    ///
    /// Only `session_type = 'user'` sessions appear: a subagent serves its parent's
    /// task and has no independent work stream, so it inherits instead of being
    /// classified. Ordering is `start_time DESC` and the pass is bounded by `limit`,
    /// so every pass advances the reporting window before touching backfill — the
    /// arbitrary `ORDER BY session_id` this replaces left today's work unreached for
    /// the ~65 hours a full pass takes.
    ///
    /// Proposals are not consulted at all. A pending one used to exclude its session,
    /// on the reasoning that a human owned the answer now — but escalating a question
    /// does not retire the machine's ability to answer it, and every proposal that goes
    /// unreviewed is a session frozen for good. Measured on the live database, 731
    /// pending proposals held back all 37 August candidates and 157 of 185 July ones,
    /// none of them at a confidence a reviewer had reached; replaying the current
    /// classifier over the frozen ones placed roughly three in four confidently.
    ///
    /// What that exclusion was also doing — stopping a duplicate proposal per pass — is
    /// now [`Self::has_pending_proposal_for_session`], applied where a proposal is
    /// filed rather than where a candidate is chosen. An answer that clears the
    /// confidence threshold instead supersedes the queued question; one that does not
    /// leaves it exactly as it stands and files nothing.
    pub fn unclassified_user_sessions(
        &self,
        limit: usize,
    ) -> Result<Vec<(tt_core::session::AgentSession, Option<String>)>, DbError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {AGENT_SESSION_COLUMNS} FROM agent_sessions s
             WHERE s.session_type = 'user'
               AND EXISTS (SELECT 1 FROM events e
                           WHERE e.session_id = s.session_id AND e.stream_id IS NULL)
             ORDER BY s.start_time DESC
             LIMIT ?1"
        ))?;
        let rows = stmt.query_map(
            params![i64::try_from(limit).unwrap_or(i64::MAX)],
            Self::row_to_agent_session,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// The user sessions [`tt_core::is_structurally_junk`] judges worthless, newest first.
    ///
    /// The predicate mirrors that function exactly — `tool_call_count = 0` and
    /// `message_count <= 2` — and mirrors [`Self::unclassified_user_sessions`] on what
    /// *unclassified* means, so the two select from one population rather than two that
    /// merely resemble each other.
    ///
    /// It is a bounded pre-filter and not the rule. The rule is `is_structurally_junk`,
    /// and [`Self::route_structurally_junk_sessions`] re-checks every row this returns
    /// against it, so the two cannot drift into disagreeing about what junk is: a row this
    /// query lets through that the function refuses is left alone.
    fn structurally_junk_sessions(
        conn: &Connection,
        limit: usize,
    ) -> Result<Vec<(tt_core::session::AgentSession, Option<String>)>, DbError> {
        let mut stmt = conn.prepare(&format!(
            "SELECT {AGENT_SESSION_COLUMNS} FROM agent_sessions s
             WHERE s.session_type = 'user'
               AND s.tool_call_count = 0
               AND s.message_count <= 2
               AND EXISTS (SELECT 1 FROM events e
                           WHERE e.session_id = s.session_id AND e.stream_id IS NULL)
             ORDER BY s.start_time DESC
             LIMIT ?1"
        ))?;
        let rows = stmt.query_map(
            params![i64::try_from(limit).unwrap_or(i64::MAX)],
            Self::row_to_agent_session,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Routes the user sessions structure alone can judge worthless to the junk stream.
    ///
    /// The bulk half of the rule [`tt_core::is_structurally_junk`] states, and it exists
    /// because the per-session half was spending a budget meant for model calls.
    /// [`Self::unclassified_user_sessions`] is bounded so a pass reaches today's work
    /// ahead of a backlog measured in thousands, and a junk session costs no model call —
    /// but it was only recognised *after* selection, so it still occupied one of those
    /// slots. Measured on the live database over one hour: **840 structurally-junk
    /// sessions classified against 20 real ones**, with pass summaries reading
    /// `junked=171..177` of 200 session slots. Model-call concurrency could not help,
    /// because only ~29 real sessions per pass ever reached the model.
    ///
    /// Routing them removes them from the candidate set *naturally*, by giving their
    /// events a stream. Filtering them out of the selection query instead is forbidden:
    /// junk excluded from selection is junk that is never routed, and it would accumulate
    /// forever. See root `AGENTS.md`, "Routing is what removes junk from the candidate
    /// set".
    ///
    /// Junk is **routed, never deleted**. The events land on the reserved junk stream, so
    /// `tt streams dissolve junk` reverses this exactly as it reverses the per-session
    /// path; no event row is removed. Only `stream_id IS NULL` rows are claimed, which is
    /// strictly narrower than skipping `assignment_source = 'user'`, so no assignment by
    /// anyone — human, todo link or earlier pass — is overwritten.
    ///
    /// **Subagents inherit the junk stream here too**, exactly as they do when a session
    /// is junked one at a time, because a subagent of work that does not exist is not work
    /// either. A parent's zero tool calls do not make that unreachable:
    /// `agent_sessions.parent_session_id` is written by ingest rather than derived from a
    /// tool count, and `subagents_of_a_junked_session_inherit_the_junk_stream` is a live
    /// test of exactly that shape. Dropping the inheritance would strand those events.
    ///
    /// # Errors
    /// Returns an error when the selection or any of the writes fail.
    pub fn route_structurally_junk_sessions(
        &self,
        limit: usize,
    ) -> Result<JunkRoutingOutcome, DbError> {
        // Probed before the junk stream is resolved, because `junk_stream_id` creates that
        // stream on first use and a pass with no junk to route must not mint one.
        if Self::structurally_junk_sessions(&self.conn, 1)?.is_empty() {
            return Ok(JunkRoutingOutcome::default());
        }
        let stream_id = self.junk_stream_id()?;
        let tx = self.write_tx()?;
        let mut outcome = JunkRoutingOutcome::default();
        {
            // Re-selected inside the transaction, so the rows written are the rows the
            // predicate holds for at write time rather than when the probe ran.
            let candidates = Self::structurally_junk_sessions(&tx, limit)?;
            let mut route = tx.prepare(
                "UPDATE events SET stream_id = ?1, assignment_source = ?2 \
                 WHERE session_id = ?3 AND stream_id IS NULL",
            )?;
            // The same claim `inherit_stream_for_session` makes, and it must stay the
            // same: unassigned events plus ones already `inherited`, so a subagent
            // follows a reclassified parent and every other source is left standing.
            let mut inherit = tx.prepare(
                "UPDATE events SET stream_id = ?1, assignment_source = ?2 \
                 WHERE session_id IN \
                       (SELECT session_id FROM agent_sessions WHERE parent_session_id = ?3) \
                   AND (stream_id IS NULL OR assignment_source = ?2)",
            )?;
            let mut record = tx.prepare(
                "INSERT INTO classified_sessions (session_id, classified_at, prompt_count)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(session_id) DO UPDATE SET classified_at = excluded.classified_at, prompt_count = excluded.prompt_count",
            )?;
            let now = format_timestamp(Utc::now());
            for (session, _) in candidates {
                // `structurally_junk_sessions` selects on the same two columns, but this
                // is what stops the SQL and the rule drifting apart: the function is the
                // rule, and a row the query let through that it refuses is left alone.
                if !tt_core::is_structurally_junk(session.tool_call_count, session.message_count) {
                    continue;
                }
                let routed = route.execute(params![
                    stream_id,
                    JUNK_ASSIGNMENT_SOURCE,
                    session.session_id
                ])? as u64;
                if routed > 0 {
                    let prompt_count =
                        u32::try_from(session.user_prompts.len()).unwrap_or(u32::MAX);
                    record.execute(params![session.session_id, now, prompt_count])?;
                }
                let inherited = inherit.execute(params![
                    stream_id,
                    INHERITED_ASSIGNMENT_SOURCE,
                    session.session_id
                ])?;
                outcome.sessions += 1;
                outcome.events += routed + inherited as u64;
            }
        }
        if outcome.events > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(outcome)
    }

    /// Lists the sessions dispatched by `parent_session_id`, oldest first.
    pub fn subagent_ids_for_parent(&self, parent_session_id: &str) -> Result<Vec<String>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id FROM agent_sessions WHERE parent_session_id = ?1 ORDER BY start_time",
        )?;
        let rows = stmt.query_map(params![parent_session_id], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Lists subagents that still hold unassigned events and name a parent that was
    /// never indexed.
    ///
    /// A subagent whose parent left no trace has nothing to inherit and nothing to
    /// classify against. The known population is a bounded 2026-04 ingest defect —
    /// 362 sessions, all `opencode` on `devbox`, whose parents are absent from
    /// `events` too. Growth here means sessions are being lost at ingest, which is a
    /// bug to fix at the source rather than a classification path to extend.
    pub fn orphan_subagent_ids(&self) -> Result<Vec<String>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT s.session_id FROM agent_sessions s
             WHERE s.parent_session_id IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM agent_sessions p
                               WHERE p.session_id = s.parent_session_id)
               AND EXISTS (SELECT 1 FROM events e
                           WHERE e.session_id = s.session_id AND e.stream_id IS NULL)
             ORDER BY s.start_time",
        )?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn get_meta_value(&self, key: &str) -> Result<Option<String>, DbError> {
        self.conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Retrieves the persisted classifier health without changing `db_version`.
    pub fn get_classifier_health(&self) -> Result<ClassifierHealth, DbError> {
        let state = self
            .get_meta_value("classifier_state")?
            .map(|value| ClassifierHealthState::from_db(&value))
            .transpose()?
            .unwrap_or_default();
        let last_success_at = self
            .get_meta_value("classifier_last_success_at")?
            .map(|value| DateTime::parse_from_rfc3339(&value))
            .transpose()?
            .map(|value| value.with_timezone(&Utc));
        let last_failure_at = self
            .get_meta_value("classifier_last_failure_at")?
            .map(|value| DateTime::parse_from_rfc3339(&value))
            .transpose()?
            .map(|value| value.with_timezone(&Utc));
        let consecutive_failures = self
            .get_meta_value("classifier_consecutive_failures")?
            .map(|value| value.parse::<u32>())
            .transpose()?
            .unwrap_or_default();
        Ok(ClassifierHealth {
            state,
            last_success_at,
            last_failure_at,
            last_error: self.get_meta_value("classifier_last_error")?,
            consecutive_failures,
        })
    }

    /// Records a successful classifier run without changing `db_version`.
    pub fn record_classifier_success(&self, at: DateTime<Utc>) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES('classifier_state', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![ClassifierHealthState::Ready.as_str()],
        )?;
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES('classifier_last_success_at', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![format_timestamp(at)],
        )?;
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES('classifier_consecutive_failures', '0')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [],
        )?;
        Ok(())
    }

    /// Records a failed classifier run without changing `db_version`.
    pub fn record_classifier_failure(&self, at: DateTime<Utc>, error: &str) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES('classifier_state', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![ClassifierHealthState::Ready.as_str()],
        )?;
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES('classifier_last_failure_at', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![format_timestamp(at)],
        )?;
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES('classifier_last_error', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![error],
        )?;
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES('classifier_consecutive_failures', '1')
             ON CONFLICT(key) DO UPDATE SET value = CAST(meta.value AS INTEGER) + 1",
            [],
        )?;
        Ok(())
    }

    /// Records unavailable classifier configuration without changing `db_version`.
    pub fn record_classifier_unconfigured(&self, error: &str) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES('classifier_state', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![ClassifierHealthState::Unconfigured.as_str()],
        )?;
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES('classifier_last_error', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![error],
        )?;
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES('classifier_consecutive_failures', '0')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [],
        )?;
        Ok(())
    }

    /// Marks a constructible classifier ready without changing `db_version`.
    pub fn record_classifier_ready(&self) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES('classifier_state', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![ClassifierHealthState::Ready.as_str()],
        )?;
        Ok(())
    }

    /// Reads how far session ingest has scanned, if a scan has ever completed.
    ///
    /// `None` means no scan has finished yet, which ingest treats as "scan
    /// everything". A value that cannot be parsed is an **error**, not a
    /// substitution: reporting it as `None` would silently restore the full scan
    /// this cursor exists to avoid, and substituting `now()` would skip every
    /// session written before this moment.
    pub fn get_session_scan_cursor(&self) -> Result<Option<DateTime<Utc>>, DbError> {
        let cursor = self
            .get_meta_value(SESSION_SCAN_CURSOR_KEY)?
            .map(|value| DateTime::parse_from_rfc3339(&value))
            .transpose()?
            .map(|value| value.with_timezone(&Utc));
        Ok(cursor)
    }

    /// Records how far session ingest has scanned, without changing `db_version`.
    ///
    /// A cursor changes no event, stream, or assignment, so the daemon has nothing
    /// to recompute; bumping would fire its 2s watcher on every ~30s ingest tick.
    /// Callers must write this **only after** a scan that actually succeeded — see
    /// `tt_cli::commands::ingest`.
    pub fn set_session_scan_cursor(&self, at: DateTime<Utc>) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT INTO meta(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![SESSION_SCAN_CURSOR_KEY, format_timestamp(at)],
        )?;
        Ok(())
    }

    /// Retrieves streams that overlap with a time range.
    ///
    /// A stream overlaps if:
    /// - Its earliest event is at or before the range end, AND
    /// - Its newest event is at or after the range start
    ///
    /// Streams are returned ordered by earliest event ascending. A stream with no
    /// events is excluded: it has no span, so there is nothing to overlap.
    ///
    /// The span comes from `events`, the same source and for the same reason as
    /// [`Self::stream_activity_windows`]. `streams.first_event_at`/`last_event_at`
    /// name exactly this quantity and answer it wrongly — **nothing writes them**,
    /// so on the live table 985 of 1,245 streams have them NULL and the newest value
    /// among the other 260 is 2026-04-30, 99 days behind the newest event.
    pub fn streams_in_range(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<Stream>, DbError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {STREAM_COLUMNS} FROM streams
             JOIN (SELECT stream_id, MIN(timestamp) AS first_at, MAX(timestamp) AS last_at
                     FROM events WHERE stream_id IS NOT NULL GROUP BY stream_id) activity
               ON activity.stream_id = streams.id
             WHERE activity.first_at <= ?2
               AND activity.last_at >= ?1
             ORDER BY activity.first_at ASC"
        ))?;

        let mut streams = Vec::new();
        let mut rows = stmt.query(params![format_timestamp(start), format_timestamp(end)])?;

        while let Some(row) = rows.next()? {
            streams.push(Self::row_to_stream(row)?);
        }

        Ok(streams)
    }

    /// Returns the most recent event timestamp for each source.
    ///
    /// Results are ordered by timestamp descending (most recent first).
    /// Returns an empty vector if the database has no events.
    pub fn get_last_event_per_source(&self) -> Result<Vec<SourceStatus>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT source, MAX(timestamp) as last_timestamp
             FROM events
             GROUP BY source
             ORDER BY last_timestamp DESC",
        )?;

        let rows = stmt.query_map([], |row| {
            let source: String = row.get(0)?;
            let timestamp_str: String = row.get(1)?;
            Ok((source, timestamp_str))
        })?;

        let mut statuses = Vec::new();
        for row_result in rows {
            let (source, timestamp_str) = row_result?;

            let last_timestamp = match DateTime::parse_from_rfc3339(&timestamp_str) {
                Ok(dt) => dt.with_timezone(&Utc),
                Err(e) => {
                    tracing::warn!(source = %source, error = %e, "skipping source with malformed timestamp");
                    continue;
                }
            };

            statuses.push(SourceStatus {
                source,
                last_timestamp,
            });
        }

        Ok(statuses)
    }

    /// Returns this machine's most recent event timestamp for each event type it has ever
    /// produced, ordered by type name.
    ///
    /// "This machine" is derived from the data rather than from `machine.json`: the
    /// `machines` table is populated only by `tt sync` registering a *remote*, so any event
    /// whose `machine_id` is absent from it — including the NULL rows written before
    /// `tt init` — was produced here. Reading the identity file instead would make this
    /// query depend on state outside the database and untestable against an in-memory one.
    ///
    /// A type this machine has never produced is **absent from the result**, not present
    /// with an old timestamp. That is the whole basis of the staleness rule its caller
    /// applies: a server or a fresh install that never ran a watcher has no expectation to
    /// fall behind, so absence must be distinguishable from silence.
    pub fn last_local_event_per_type(&self) -> Result<Vec<LocalEventTypeStatus>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT type, MAX(timestamp) as last_timestamp
             FROM events
             WHERE machine_id IS NULL
                OR machine_id NOT IN (SELECT machine_id FROM machines)
             GROUP BY type
             ORDER BY type ASC",
        )?;

        let rows = stmt.query_map([], |row| {
            let event_type: String = row.get(0)?;
            let timestamp_str: String = row.get(1)?;
            Ok((event_type, timestamp_str))
        })?;

        let mut statuses = Vec::new();
        for row_result in rows {
            let (raw_type, timestamp_str) = row_result?;

            // An unreadable row drops that type from the result, which reads as "never
            // produced" and so cannot manufacture a stale-source warning out of a data
            // defect. Both cases warn; neither fails the verdict.
            let Ok(event_type) = raw_type.parse::<tt_core::EventType>() else {
                tracing::warn!(
                    event_type = %raw_type,
                    "skipping unknown event type while reading local source freshness"
                );
                continue;
            };
            let last_timestamp = match DateTime::parse_from_rfc3339(&timestamp_str) {
                Ok(timestamp) => timestamp.with_timezone(&Utc),
                Err(error) => {
                    tracing::warn!(
                        event_type = %event_type,
                        %error,
                        "skipping local event type with malformed timestamp"
                    );
                    continue;
                }
            };

            statuses.push(LocalEventTypeStatus {
                event_type,
                last_timestamp,
            });
        }

        Ok(statuses)
    }

    /// Inserts or updates a machine entry, including sync position.
    pub fn upsert_machine(
        &self,
        machine_id: &str,
        label: &str,
        last_event_id: Option<&str>,
    ) -> Result<(), DbError> {
        let tx = self.write_tx()?;
        let count = tx.execute(
            "INSERT INTO machines (machine_id, label, last_sync_at, last_event_id)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(machine_id) DO UPDATE SET
                label = excluded.label,
                last_sync_at = excluded.last_sync_at,
                last_event_id = COALESCE(excluded.last_event_id, machines.last_event_id)",
            params![
                machine_id,
                label,
                format_timestamp(Utc::now()),
                last_event_id
            ],
        )?;
        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Inserts or updates a machine entry with explicit sync timestamp.
    pub fn upsert_machine_with_sync_time(
        &self,
        machine_id: &str,
        label: &str,
        last_event_id: Option<&str>,
        last_sync_at: &str,
    ) -> Result<(), DbError> {
        let tx = self.write_tx()?;
        let count = tx.execute(
            "INSERT INTO machines (machine_id, label, last_sync_at, last_event_id)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(machine_id) DO UPDATE SET
                label = excluded.label,
                last_sync_at = excluded.last_sync_at,
                last_event_id = COALESCE(excluded.last_event_id, machines.last_event_id)",
            params![machine_id, label, last_sync_at, last_event_id],
        )?;
        if count > 0 {
            Self::bump_db_version_in_transaction(&tx)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Lists all known machines.
    pub fn list_machines(&self) -> Result<Vec<Machine>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT machine_id, label, last_sync_at, last_event_id FROM machines ORDER BY label",
        )?;
        let machines = stmt
            .query_map([], |row| {
                Ok(Machine {
                    machine_id: row.get(0)?,
                    label: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    last_sync_at: row.get(2)?,
                    last_event_id: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(machines)
    }

    /// Gets the last event ID synced from a machine identified by label.
    pub fn get_machine_last_event_id_by_label(
        &self,
        label: &str,
    ) -> Result<Option<String>, DbError> {
        let mut stmt = self
            .conn
            .prepare("SELECT last_event_id FROM machines WHERE label = ?1")?;
        let result: Option<Option<String>> = stmt
            .query_row(params![label], |row| row.get(0))
            .optional()?;
        Ok(result.flatten())
    }

    /// Gets the last sync timestamp for a machine identified by label.
    pub fn get_machine_last_sync_at_by_label(
        &self,
        label: &str,
    ) -> Result<Option<String>, DbError> {
        let mut stmt = self
            .conn
            .prepare("SELECT last_sync_at FROM machines WHERE label = ?1")?;
        let result: Option<Option<String>> = stmt
            .query_row(params![label], |row| row.get(0))
            .optional()?;
        Ok(result.flatten())
    }

    /// Gets the most recent event ID for a specific machine.
    pub fn get_latest_event_id_for_machine(
        &self,
        machine_id: &str,
    ) -> Result<Option<String>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM events WHERE machine_id = ?1 ORDER BY timestamp DESC LIMIT 1",
        )?;
        let result = stmt
            .query_row(params![machine_id], |row| row.get(0))
            .optional()?;
        Ok(result)
    }

    /// Gets the timestamp of the most recent event attributed to a machine.
    ///
    /// Returns `None` when the machine has never produced an event.
    ///
    /// This is the honest liveness signal for a remote: unlike `last_sync_at`,
    /// a sync that succeeds but returns nothing leaves this timestamp untouched.
    pub fn get_last_event_timestamp_for_machine(
        &self,
        machine_id: &str,
    ) -> Result<Option<String>, DbError> {
        let mut stmt = self
            .conn
            .prepare("SELECT MAX(timestamp) FROM events WHERE machine_id = ?1")?;
        // `MAX` over an empty set still yields one row, holding SQL NULL.
        let latest: Option<String> = stmt.query_row(params![machine_id], |row| row.get(0))?;
        Ok(latest)
    }
}

/// The only time-allocation entry point outside `tt-core`.
///
/// `end` is exclusive: events at exactly `end` belong to the next period.
#[expect(
    clippy::disallowed_methods,
    reason = "single permitted allocation boundary"
)]
pub fn allocate_for_period(
    db: &Database,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    period_end: Option<DateTime<Utc>>,
    config: &AllocationConfig,
) -> Result<AllocationResult, DbError> {
    let inclusive_end = end - chrono::Duration::milliseconds(1);
    let mut events = if inclusive_end < start {
        Vec::new()
    } else {
        db.get_events_in_range(start, inclusive_end)?
    };

    let session_ids_with_starts: BTreeSet<&str> = events
        .iter()
        .filter(|event| {
            event.event_type == EventType::AgentSession
                && event.action.as_deref() == Some("started")
        })
        .filter_map(|event| event.session_id.as_deref())
        .collect();
    let missing_session_ids: Vec<String> = events
        .iter()
        .filter(|event| event.event_type == EventType::AgentToolUse)
        .filter_map(|event| event.session_id.as_deref())
        .filter(|session_id| !session_ids_with_starts.contains(*session_id))
        .map(ToString::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    if !missing_session_ids.is_empty() {
        let mut start_events = db.get_agent_session_start_events(&missing_session_ids)?;
        start_events.append(&mut events);
        events = start_events;
    }

    let agent_sessions = db.agent_sessions_in_range(start, end)?;
    let session_types: HashMap<String, SessionType> = agent_sessions
        .iter()
        .map(|session| (session.session_id.clone(), session.session_type))
        .collect();
    let session_end_times: HashMap<String, DateTime<Utc>> = agent_sessions
        .iter()
        .filter_map(|session| {
            session
                .end_time
                .map(|end| (session.session_id.clone(), end))
        })
        .collect();

    Ok(allocate_time(
        &events,
        config,
        period_end,
        &session_end_times,
        &session_types,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn make_event(
        id: &str,
        timestamp: DateTime<Utc>,
        event_type: tt_core::EventType,
    ) -> StoredEvent {
        StoredEvent {
            id: id.to_string(),
            timestamp,
            event_type,
            source: "remote.tmux".to_string(),
            machine_id: None,
            schema_version: 1,
            pane_id: Some("%3".to_string()),
            tmux_session: Some("dev".to_string()),
            window_index: Some(1),
            git_project: None,
            git_workspace: None,
            status: None,
            idle_duration_ms: None,
            window_app_id: None,
            window_title: None,
            action: None,
            cwd: Some("/home/sami/project-x".to_string()),
            session_id: None,
            stream_id: None,
            assignment_source: None,
            data: json!({}),
        }
    }

    fn ts(hours: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2025, 1, 15, 0, 0, 0).unwrap() + chrono::Duration::hours(hours)
    }

    fn insert_session_with_end(
        db: &Database,
        session_id: &str,
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
    ) {
        let session = tt_core::session::AgentSession {
            session_id: session_id.to_string(),
            source: tt_core::session::SessionSource::Claude,
            parent_session_id: None,
            session_type: tt_core::session::SessionType::User,
            project_path: "/home/sami/project-x".to_string(),
            project_name: "project-x".to_string(),
            start_time,
            end_time: Some(end_time),
            message_count: 0,
            summary: None,
            user_prompts: Vec::new(),
            starting_prompt: None,
            assistant_message_count: 0,
            tool_call_count: 0,
            user_message_timestamps: Vec::new(),
            tool_call_timestamps: Vec::new(),
        };
        db.upsert_agent_session(&session, None).unwrap();
    }

    fn insert_agent_session_event(
        db: &Database,
        id: &str,
        timestamp: DateTime<Utc>,
        session_id: &str,
        action: &str,
    ) {
        let mut event = make_event(id, timestamp, tt_core::EventType::AgentSession);
        event.action = Some(action.to_string());
        event.session_id = Some(session_id.to_string());
        event.stream_id = Some("stream-a".to_string());
        db.insert_event(&event).unwrap();
    }

    fn insert_tool_use_event(db: &Database, id: &str, timestamp: DateTime<Utc>, session_id: &str) {
        let mut event = make_event(id, timestamp, tt_core::EventType::AgentToolUse);
        event.session_id = Some(session_id.to_string());
        event.stream_id = Some("stream-a".to_string());
        db.insert_event(&event).unwrap();
    }

    fn total_delegated(result: &tt_core::AllocationResult) -> i64 {
        result
            .stream_times
            .iter()
            .map(|stream_time| stream_time.time_delegated_ms)
            .sum()
    }

    fn insert_test_stream(db: &Database, timestamp: DateTime<Utc>) {
        db.insert_stream(&Stream {
            id: "stream-a".to_string(),
            name: Some("stream-a".to_string()),
            slug: None,
            description: None,
            color: None,
            created_at: timestamp,
            updated_at: timestamp,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: false,
        })
        .unwrap();
    }

    #[test]
    fn open_in_memory_database() {
        let db = Database::open_in_memory();
        assert!(db.is_ok());
    }

    #[test]
    fn test_insert_event_stores_all_fields() {
        let db = Database::open_in_memory().unwrap();
        let ts = Utc.with_ymd_and_hms(2025, 1, 15, 10, 30, 0).unwrap();

        let event = StoredEvent {
            id: "test-event-123".to_string(),
            timestamp: ts,
            event_type: tt_core::EventType::TmuxPaneFocus,
            source: "remote.tmux".to_string(),
            machine_id: None,
            schema_version: 1,
            pane_id: Some("%42".to_string()),
            tmux_session: Some("main".to_string()),
            window_index: Some(2),
            git_project: Some("my-project".to_string()),
            git_workspace: Some("feature".to_string()),
            status: None,
            idle_duration_ms: None,
            window_app_id: None,
            window_title: None,
            action: None,
            cwd: Some("/home/sami/project".to_string()),
            session_id: Some("abc123".to_string()),
            stream_id: None,
            assignment_source: None,
            data: serde_json::Value::Null,
        };

        let inserted = db.insert_event(&event).unwrap();
        assert!(inserted);

        let events = db.get_events(None, None).unwrap();
        assert_eq!(events.len(), 1);

        let retrieved = &events[0];
        assert_eq!(retrieved.id, "test-event-123");
        assert_eq!(retrieved.timestamp, ts);
        assert_eq!(retrieved.event_type, tt_core::EventType::TmuxPaneFocus);
        assert_eq!(retrieved.source, "remote.tmux");
        assert_eq!(retrieved.schema_version, 1);
        assert_eq!(retrieved.pane_id, Some("%42".to_string()));
        assert_eq!(retrieved.tmux_session, Some("main".to_string()));
        assert_eq!(retrieved.window_index, Some(2));
        assert_eq!(retrieved.git_project, Some("my-project".to_string()));
        assert_eq!(retrieved.git_workspace, Some("feature".to_string()));
        assert_eq!(retrieved.cwd, Some("/home/sami/project".to_string()));
        assert_eq!(retrieved.session_id, Some("abc123".to_string()));
    }

    #[test]
    fn test_insert_event_stores_window_fields() {
        let db = Database::open_in_memory().unwrap();
        let ts = Utc.with_ymd_and_hms(2026, 6, 14, 10, 0, 0).unwrap();
        let mut event = make_event("win-1", ts, tt_core::EventType::WindowFocus);
        event.source = "local.cosmic".to_string();
        event.cwd = None;
        event.pane_id = None;
        event.tmux_session = None;
        event.window_index = None;
        event.window_app_id = Some("firefox".to_string());
        event.window_title = Some("Proposal - Google Docs".to_string());

        assert!(db.insert_event(&event).unwrap());
        let events = db.get_events(None, None).unwrap();
        let got = events.iter().find(|e| e.id == "win-1").unwrap();
        assert_eq!(got.window_app_id.as_deref(), Some("firefox"));
        assert_eq!(got.window_title.as_deref(), Some("Proposal - Google Docs"));
        let data = got.build_data_json();
        assert_eq!(data.get("app").and_then(|v| v.as_str()), Some("firefox"));
        assert_eq!(
            data.get("title").and_then(|v| v.as_str()),
            Some("Proposal - Google Docs")
        );
    }

    #[test]
    fn test_insert_event_idempotent() {
        let db = Database::open_in_memory().unwrap();
        let ts = Utc.with_ymd_and_hms(2025, 1, 15, 10, 30, 0).unwrap();
        let event = make_event("duplicate-id", ts, tt_core::EventType::TmuxPaneFocus);

        let first_insert = db.insert_event(&event).unwrap();
        assert!(first_insert, "first insert should succeed");

        let second_insert = db.insert_event(&event).unwrap();
        assert!(!second_insert, "second insert should be ignored");

        let events = db.get_events(None, None).unwrap();
        assert_eq!(events.len(), 1, "should only have one event");
    }

    #[test]
    fn test_get_events_empty_database() {
        let db = Database::open_in_memory().unwrap();
        let events = db.get_events(None, None).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn event_time_bounds_reports_the_first_and_last_event() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.event_time_bounds().unwrap(), None);

        db.insert_events(&[
            make_event(
                "b",
                Utc.with_ymd_and_hms(2026, 3, 2, 9, 0, 0).unwrap(),
                EventType::UserMessage,
            ),
            make_event(
                "a",
                Utc.with_ymd_and_hms(2026, 3, 1, 9, 0, 0).unwrap(),
                EventType::UserMessage,
            ),
            make_event(
                "c",
                Utc.with_ymd_and_hms(2026, 3, 3, 9, 0, 0).unwrap(),
                EventType::UserMessage,
            ),
        ])
        .unwrap();

        let (first, last) = db.event_time_bounds().unwrap().unwrap();
        assert_eq!(first, Utc.with_ymd_and_hms(2026, 3, 1, 9, 0, 0).unwrap());
        assert_eq!(last, Utc.with_ymd_and_hms(2026, 3, 3, 9, 0, 0).unwrap());
    }

    #[test]
    fn test_get_events_time_range_after() {
        let db = Database::open_in_memory().unwrap();

        let ts1 = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2025, 1, 15, 11, 0, 0).unwrap();
        let ts3 = Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap();

        db.insert_event(&make_event("e1", ts1, tt_core::EventType::TmuxPaneFocus))
            .unwrap();
        db.insert_event(&make_event("e2", ts2, tt_core::EventType::TmuxPaneFocus))
            .unwrap();
        db.insert_event(&make_event("e3", ts3, tt_core::EventType::TmuxPaneFocus))
            .unwrap();

        // Query events after 10:30
        let after = Utc.with_ymd_and_hms(2025, 1, 15, 10, 30, 0).unwrap();
        let events = db.get_events(Some(after), None).unwrap();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id, "e2");
        assert_eq!(events[1].id, "e3");
    }

    #[test]
    fn test_get_events_time_range_before() {
        let db = Database::open_in_memory().unwrap();

        let ts1 = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2025, 1, 15, 11, 0, 0).unwrap();
        let ts3 = Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap();

        db.insert_event(&make_event("e1", ts1, tt_core::EventType::TmuxPaneFocus))
            .unwrap();
        db.insert_event(&make_event("e2", ts2, tt_core::EventType::TmuxPaneFocus))
            .unwrap();
        db.insert_event(&make_event("e3", ts3, tt_core::EventType::TmuxPaneFocus))
            .unwrap();

        // Query events before 11:30
        let before = Utc.with_ymd_and_hms(2025, 1, 15, 11, 30, 0).unwrap();
        let events = db.get_events(None, Some(before)).unwrap();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id, "e1");
        assert_eq!(events[1].id, "e2");
    }

    #[test]
    fn test_get_events_time_range_both() {
        let db = Database::open_in_memory().unwrap();

        let ts1 = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2025, 1, 15, 11, 0, 0).unwrap();
        let ts3 = Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap();

        db.insert_event(&make_event("e1", ts1, tt_core::EventType::TmuxPaneFocus))
            .unwrap();
        db.insert_event(&make_event("e2", ts2, tt_core::EventType::TmuxPaneFocus))
            .unwrap();
        db.insert_event(&make_event("e3", ts3, tt_core::EventType::TmuxPaneFocus))
            .unwrap();

        // Query events between 10:30 and 11:30
        let after = Utc.with_ymd_and_hms(2025, 1, 15, 10, 30, 0).unwrap();
        let before = Utc.with_ymd_and_hms(2025, 1, 15, 11, 30, 0).unwrap();
        let events = db.get_events(Some(after), Some(before)).unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "e2");
    }

    #[test]
    fn test_get_events_ordered_by_timestamp() {
        let db = Database::open_in_memory().unwrap();

        // Insert out of order
        let ts2 = Utc.with_ymd_and_hms(2025, 1, 15, 11, 0, 0).unwrap();
        let ts1 = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        let ts3 = Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap();

        db.insert_event(&make_event("e2", ts2, tt_core::EventType::TmuxPaneFocus))
            .unwrap();
        db.insert_event(&make_event("e1", ts1, tt_core::EventType::TmuxPaneFocus))
            .unwrap();
        db.insert_event(&make_event("e3", ts3, tt_core::EventType::TmuxPaneFocus))
            .unwrap();

        let events = db.get_events(None, None).unwrap();

        assert_eq!(events.len(), 3);
        assert_eq!(events[0].id, "e1");
        assert_eq!(events[1].id, "e2");
        assert_eq!(events[2].id, "e3");
    }

    #[test]
    fn test_get_events_in_range_inclusive() {
        let db = Database::open_in_memory().unwrap();

        let ts1 = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2025, 1, 15, 11, 0, 0).unwrap();
        let ts3 = Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap();

        db.insert_event(&make_event("e1", ts1, tt_core::EventType::TmuxPaneFocus))
            .unwrap();
        db.insert_event(&make_event("e2", ts2, tt_core::EventType::TmuxPaneFocus))
            .unwrap();
        db.insert_event(&make_event("e3", ts3, tt_core::EventType::TmuxPaneFocus))
            .unwrap();

        // Query with inclusive range matching exactly ts1 and ts2
        let events = db.get_events_in_range(ts1, ts2).unwrap();

        // Should include both boundary events (inclusive)
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id, "e1");
        assert_eq!(events[1].id, "e2");
    }

    #[test]
    fn test_get_events_in_range_ordered() {
        let db = Database::open_in_memory().unwrap();

        // Insert out of order
        let ts2 = Utc.with_ymd_and_hms(2025, 1, 15, 11, 0, 0).unwrap();
        let ts1 = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        let ts3 = Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap();

        db.insert_event(&make_event("e2", ts2, tt_core::EventType::TmuxPaneFocus))
            .unwrap();
        db.insert_event(&make_event("e1", ts1, tt_core::EventType::TmuxPaneFocus))
            .unwrap();
        db.insert_event(&make_event("e3", ts3, tt_core::EventType::TmuxPaneFocus))
            .unwrap();

        let start = Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2025, 1, 15, 13, 0, 0).unwrap();
        let events = db.get_events_in_range(start, end).unwrap();

        assert_eq!(events.len(), 3);
        assert_eq!(events[0].id, "e1");
        assert_eq!(events[1].id, "e2");
        assert_eq!(events[2].id, "e3");
    }

    #[test]
    fn test_get_events_in_range_empty() {
        let db = Database::open_in_memory().unwrap();

        let ts = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        db.insert_event(&make_event("e1", ts, tt_core::EventType::TmuxPaneFocus))
            .unwrap();

        // Query a range that doesn't include any events
        let start = Utc.with_ymd_and_hms(2025, 1, 15, 11, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap();
        let events = db.get_events_in_range(start, end).unwrap();

        assert!(events.is_empty());
    }

    #[test]
    fn test_get_agent_session_start_events_filters_and_orders_results() {
        let db = Database::open_in_memory().unwrap();
        let ts1 = Utc.with_ymd_and_hms(2025, 1, 14, 23, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2025, 1, 15, 0, 0, 0).unwrap();
        let ts3 = Utc.with_ymd_and_hms(2025, 1, 15, 1, 0, 0).unwrap();
        let ts4 = Utc.with_ymd_and_hms(2025, 1, 15, 2, 0, 0).unwrap();

        for stream_id in ["stream-a", "stream-b"] {
            db.insert_stream(&Stream {
                id: stream_id.to_string(),
                created_at: ts1,
                updated_at: ts1,
                name: Some(stream_id.to_string()),
                slug: None,
                description: None,
                color: None,
                time_direct_ms: 0,
                time_delegated_ms: 0,
                first_event_at: None,
                last_event_at: None,
                needs_recompute: false,
            })
            .unwrap();
        }

        let mut second_start = make_event("session-b-start", ts2, tt_core::EventType::AgentSession);
        second_start.action = Some("started".to_string());
        second_start.session_id = Some("session-b".to_string());
        second_start.stream_id = Some("stream-b".to_string());

        let mut first_start = make_event("session-a-start", ts1, tt_core::EventType::AgentSession);
        first_start.action = Some("started".to_string());
        first_start.session_id = Some("session-a".to_string());
        first_start.stream_id = Some("stream-a".to_string());

        let mut tool_use = make_event("session-a-tool", ts3, tt_core::EventType::AgentToolUse);
        tool_use.session_id = Some("session-a".to_string());
        tool_use.stream_id = Some("stream-a".to_string());

        let mut end = make_event("session-a-end", ts4, tt_core::EventType::AgentSession);
        end.action = Some("ended".to_string());
        end.session_id = Some("session-a".to_string());
        end.stream_id = Some("stream-a".to_string());

        for event in [&second_start, &first_start, &tool_use, &end] {
            db.insert_event(event).unwrap();
        }

        let events = db
            .get_agent_session_start_events(&["session-b".to_string(), "session-a".to_string()])
            .unwrap();

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id, "session-a-start");
        assert_eq!(events[1].id, "session-b-start");
    }

    #[test]
    fn test_get_agent_session_start_events_empty_input() {
        let db = Database::open_in_memory().unwrap();

        let events = db.get_agent_session_start_events(&[]).unwrap();

        assert!(events.is_empty());
    }

    #[test]
    fn test_allocate_for_period_populates_session_maps() {
        let db = Database::open_in_memory().unwrap();
        // Session with an explicit end_time; delegated time must stop at end_time,
        // not at the 30-min timeout heuristic.
        let start = ts(0);
        insert_test_stream(&db, start);
        insert_session_with_end(&db, "sess-a", start, start + chrono::Duration::minutes(10));
        insert_agent_session_event(&db, "e-start", start, "sess-a", "started");
        insert_tool_use_event(&db, "e-tool", start, "sess-a");

        let result = allocate_for_period(
            &db,
            start - chrono::Duration::hours(1),
            start + chrono::Duration::hours(2),
            Some(start + chrono::Duration::hours(2)),
            &tt_core::AllocationConfig::default(),
        )
        .unwrap();

        // 10 min (real end), not 30 min (timeout heuristic after last tool use)
        assert_eq!(total_delegated(&result), 10 * 60 * 1000);
    }

    #[test]
    fn test_allocate_for_period_backfills_missing_session_starts() {
        let db = Database::open_in_memory().unwrap();
        // Tool-use event in range whose session start predates the range:
        // the start event must be backfilled so allocation sees the session.
        insert_test_stream(&db, ts(0));
        insert_agent_session_event(&db, "e-start", ts(0), "sess-b", "started");
        insert_tool_use_event(&db, "e-tool", ts(0) + chrono::Duration::hours(3), "sess-b");
        let result = allocate_for_period(
            &db,
            ts(0) + chrono::Duration::hours(2),
            ts(0) + chrono::Duration::hours(4),
            Some(ts(0) + chrono::Duration::hours(4)),
            &tt_core::AllocationConfig::default(),
        )
        .unwrap();

        assert!(
            total_delegated(&result) > 0,
            "session start must be backfilled"
        );
    }

    #[test]
    fn test_allocate_for_period_excludes_event_at_exclusive_end() {
        // Given: focus events immediately before and at the period boundary.
        let db = Database::open_in_memory().unwrap();
        let start = ts(0);
        let end = start + chrono::Duration::seconds(5);
        insert_test_stream(&db, start);
        let config = tt_core::AllocationConfig {
            attention_window_ms: 10_000,
            ..Default::default()
        };

        for (id, timestamp) in [
            ("start", start),
            ("before-end", end - chrono::Duration::milliseconds(1)),
            ("at-end", end),
        ] {
            let mut event = make_event(id, timestamp, tt_core::EventType::TmuxPaneFocus);
            event.stream_id = Some("stream-a".to_string());
            db.insert_event(&event).unwrap();
        }

        // When: allocating the half-open [start, end) period.
        let result = allocate_for_period(&db, start, end, None, &config).unwrap();

        // Then: the event one millisecond before end contributes; the end event does not.
        assert_eq!(result.stream_times.len(), 1);
        assert_eq!(result.stream_times[0].time_direct_ms, 14_999);
    }

    #[test]
    fn test_allocate_for_period_returns_empty_result_for_degenerate_period() {
        // Given: an event exactly at a zero-width period's boundary.
        let db = Database::open_in_memory().unwrap();
        let start = ts(0);
        insert_test_stream(&db, start);
        let mut event = make_event("at-boundary", start, tt_core::EventType::TmuxPaneFocus);
        event.stream_id = Some("stream-a".to_string());
        db.insert_event(&event).unwrap();

        // When: allocating a period where start equals the exclusive end.
        let result = allocate_for_period(
            &db,
            start,
            start,
            Some(start + chrono::Duration::minutes(1)),
            &tt_core::AllocationConfig::default(),
        )
        .unwrap();

        // Then: allocation proceeds with no events and zero totals.
        assert!(result.stream_times.is_empty());
        assert_eq!(result.total_tracked_ms, 0);
        assert_eq!(result.unassigned_direct_ms, 0);
        assert_eq!(result.unassigned_delegated_ms, 0);
    }

    #[test]
    fn test_insert_event_with_null_optionals() {
        let db = Database::open_in_memory().unwrap();
        let ts = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();

        let event = StoredEvent {
            id: "no-optionals".to_string(),
            timestamp: ts,
            event_type: tt_core::EventType::WindowFocus,
            source: "remote.tmux".to_string(),
            machine_id: None,
            schema_version: 1,
            pane_id: None,
            tmux_session: None,
            window_index: None,
            git_project: None,
            git_workspace: None,
            status: None,
            idle_duration_ms: None,
            window_app_id: None,
            window_title: None,
            action: None,
            cwd: None,
            session_id: None,
            stream_id: None,
            assignment_source: None,
            data: json!({}),
        };

        db.insert_event(&event).unwrap();

        let events = db.get_events(None, None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].cwd, None);
        assert_eq!(events[0].session_id, None);
    }

    #[test]
    fn test_insert_events_batch() {
        let db = Database::open_in_memory().unwrap();

        let base_ts = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        let events: Vec<StoredEvent> = (0..100)
            .map(|i| {
                let ts = base_ts + chrono::Duration::seconds(i);
                make_event(&format!("batch-{i}"), ts, tt_core::EventType::TmuxPaneFocus)
            })
            .collect();

        let count = db.insert_events(&events).unwrap();
        assert_eq!(count, 100);

        let retrieved = db.get_events(None, None).unwrap();
        assert_eq!(retrieved.len(), 100);
    }

    #[test]
    fn test_insert_events_returns_inserted_count() {
        let db = Database::open_in_memory().unwrap();

        let ts1 = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 1).unwrap();

        // Insert first event
        db.insert_event(&make_event(
            "existing",
            ts1,
            tt_core::EventType::TmuxPaneFocus,
        ))
        .unwrap();

        // Batch insert: one new, one duplicate
        let events = vec![
            make_event("existing", ts1, tt_core::EventType::TmuxPaneFocus),
            make_event("new", ts2, tt_core::EventType::TmuxPaneFocus),
        ];

        let count = db.insert_events(&events).unwrap();
        assert_eq!(count, 1, "should only count the new insert");
    }

    #[test]
    fn test_get_events_skips_malformed_timestamp() {
        let db = Database::open_in_memory().unwrap();

        // Insert a valid event
        let ts = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        db.insert_event(&make_event("valid", ts, tt_core::EventType::TmuxPaneFocus))
            .unwrap();

        // Insert malformed timestamp directly
        db.conn
            .execute(
                "INSERT INTO events (id, timestamp, type, source, schema_version)
                 VALUES ('malformed', 'not a valid timestamp', 'test', 'test', 1)",
                [],
            )
            .unwrap();

        // Query should return only the valid event
        let events = db.get_events(None, None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "valid");
    }

    #[test]
    fn test_get_events_skips_unknown_event_type() {
        let db = Database::open_in_memory().unwrap();

        let ts = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        db.insert_event(&make_event("valid", ts, tt_core::EventType::TmuxPaneFocus))
            .unwrap();

        db.conn
            .execute(
                "INSERT INTO events (id, timestamp, type, source, schema_version)
                 VALUES ('unknown', ?1, 'not_a_real_type', 'test', 1)",
                params![format_timestamp(ts)],
            )
            .unwrap();

        let events = db.get_events(None, None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "valid");
    }

    #[test]
    fn test_schema_version_check() {
        // Create a temporary database file
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("test.db");

        // Create database with old schema version
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_info (version INTEGER NOT NULL);
                 INSERT INTO schema_info (version) VALUES (1);",
            )
            .unwrap();
        }

        // Opening with new version should fail
        let result = Database::open(&db_path);
        assert!(result.is_err());

        let err = result.unwrap_err();
        match err {
            DbError::SchemaVersionMismatch { found, expected } => {
                assert_eq!(found, 1);
                assert_eq!(expected, SCHEMA_VERSION);
            }
            DbError::Sqlite(_)
            | DbError::SlugTaken { .. }
            | DbError::InvalidProposalStatus(_)
            | DbError::InvalidProposalEventIds(_)
            | DbError::InvalidProposalNewStream(_)
            | DbError::ProposalNotPending { .. }
            | DbError::ProposalNotFound { .. }
            | DbError::InvalidProposalStreamTarget { .. }
            | DbError::InvalidProposalAssignmentTarget { .. }
            | DbError::ProposedStreamNotFound { .. }
            | DbError::RejectTargetStreamNotFound { .. }
            | DbError::MergeIntoSelf { .. }
            | DbError::MergeTargetNotFound { .. }
            | DbError::InvalidClassifierHealthTimestamp(_)
            | DbError::InvalidClassifierFailureCount(_)
            | DbError::InvalidClassifierHealthState(_) => {
                panic!("expected SchemaVersionMismatch error");
            }
        }
    }

    #[test]
    fn open_current_database_does_not_wait_for_a_concurrent_writer() {
        let temp_dir = tempfile::tempdir().unwrap();
        let database_path = temp_dir.path().join("current.db");
        Database::open(&database_path).unwrap();
        let writer = Connection::open(&database_path).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();

        let (sender, receiver) = std::sync::mpsc::channel();
        let path_for_open = database_path;
        let opener = std::thread::spawn(move || {
            sender.send(Database::open(&path_for_open)).unwrap();
        });
        let result = receiver.recv_timeout(std::time::Duration::from_millis(250));

        writer.execute_batch("ROLLBACK").unwrap();
        let _ = receiver.recv_timeout(std::time::Duration::from_secs(1));
        opener.join().unwrap();

        assert!(
            matches!(result, Ok(Ok(_))),
            "opening a current schema must not contend with a concurrent writer: {result:?}"
        );
    }

    #[test]
    fn test_migration_v8_to_v13_adds_columns_preserves_rows() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("v8.db");

        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_info (version INTEGER NOT NULL);
                 INSERT INTO schema_info (version) VALUES (8);
                 CREATE TABLE events (
                   id TEXT PRIMARY KEY,
                   timestamp TEXT NOT NULL,
                   type TEXT NOT NULL,
                   source TEXT NOT NULL,
                   machine_id TEXT,
                   schema_version INTEGER DEFAULT 1,
                   cwd TEXT,
                   git_project TEXT,
                   git_workspace TEXT,
                   pane_id TEXT,
                   tmux_session TEXT,
                   window_index INTEGER,
                   status TEXT,
                   idle_duration_ms INTEGER,
                   action TEXT,
                   session_id TEXT,
                   stream_id TEXT,
                   assignment_source TEXT DEFAULT 'inferred'
                 );
                 INSERT INTO events (id, timestamp, type, source)
                 VALUES ('old-1','2026-06-01T00:00:00.000Z','tmux_pane_focus','remote.tmux');
                 CREATE TABLE streams (
                    id TEXT PRIMARY KEY,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    name TEXT,
                    time_direct_ms INTEGER DEFAULT 0,
                    time_delegated_ms INTEGER DEFAULT 0,
                    first_event_at TEXT,
                    last_event_at TEXT,
                    needs_recompute INTEGER DEFAULT 0
                 );
                 INSERT INTO streams (id, created_at, updated_at, name)
                 VALUES ('old-stream', '2026-06-01T00:00:00.000Z', '2026-06-01T00:00:00.000Z', 'legacy stream');
                 CREATE TABLE proposals (
                   id TEXT PRIMARY KEY,
                   created_at TEXT NOT NULL,
                   session_id TEXT,
                   event_ids TEXT,
                   proposed_stream_id TEXT,
                   proposed_new_stream TEXT,
                   confidence REAL NOT NULL,
                   reasoning TEXT NOT NULL,
                   status TEXT NOT NULL DEFAULT 'pending'
                 );
                 INSERT INTO proposals (id, created_at, session_id, confidence, reasoning)
                 VALUES ('old-proposal', '2026-06-01T00:00:00.000Z', 'session-a', 0.5, 'unsure');",
            )
            .unwrap();
        }

        let db = Database::open(&db_path).unwrap();
        let events = db.get_events(None, None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "old-1");
        assert_eq!(events[0].window_app_id, None);
        assert_eq!(events[0].window_title, None);
        let stream = db.get_stream("old-stream").unwrap().unwrap();
        assert_eq!(stream.slug, None);

        // A question queued before generations existed reads as authored by a classifier
        // nobody can identify, which is what makes it re-askable exactly once rather
        // than either frozen or re-asked forever.
        let proposals = db.get_proposals(None).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].id, "old-proposal");
        assert_eq!(proposals[0].classifier_generation, None);

        let version = db
            .conn
            .query_row("SELECT version FROM schema_info LIMIT 1", [], |row| {
                row.get::<_, i32>(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        let ts = Utc.with_ymd_and_hms(2026, 6, 14, 10, 0, 0).unwrap();
        let mut event = make_event("win-2", ts, tt_core::EventType::WindowFocus);
        event.window_app_id = Some("slack".to_string());
        event.window_title = Some("Team chat".to_string());
        assert!(db.insert_event(&event).unwrap());

        let events = db.get_events(None, None).unwrap();
        let got = events.iter().find(|event| event.id == "win-2").unwrap();
        assert_eq!(got.window_app_id.as_deref(), Some("slack"));
        assert_eq!(got.window_title.as_deref(), Some("Team chat"));
    }

    /// The upgrade path every deployed machine actually takes, which the v8 fixture
    /// above does not exercise: at v12 the only arm that runs is the `proposals`
    /// `ALTER`, and getting it wrong hard-fails `tt` on every box at once with
    /// `SchemaVersionMismatch`.
    #[test]
    fn migration_from_v12_adds_the_classifier_generation_column() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("v12.db");

        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_info (version INTEGER NOT NULL);
                 INSERT INTO schema_info (version) VALUES (12);
                 CREATE TABLE streams (
                    id TEXT PRIMARY KEY,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    name TEXT,
                    slug TEXT,
                    description TEXT,
                    color TEXT,
                    time_direct_ms INTEGER DEFAULT 0,
                    time_delegated_ms INTEGER DEFAULT 0,
                    first_event_at TEXT,
                    last_event_at TEXT,
                    needs_recompute INTEGER DEFAULT 0
                 );
                 INSERT INTO streams (id, created_at, updated_at, name)
                 VALUES ('stream-a', '2026-08-01T00:00:00.000Z', '2026-08-01T00:00:00.000Z', 'live work');
                 CREATE TABLE proposals (
                    id TEXT PRIMARY KEY,
                    created_at TEXT NOT NULL,
                    session_id TEXT,
                    event_ids TEXT,
                    proposed_stream_id TEXT,
                    proposed_new_stream TEXT,
                    confidence REAL NOT NULL,
                    reasoning TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'pending'
                 );
                 INSERT INTO proposals (id, created_at, event_ids, proposed_stream_id, confidence, reasoning)
                 VALUES ('queued', '2026-08-06T00:00:00.000Z', '[\"window-a\"]', 'stream-a', 0.5, 'unsure');",
            )
            .unwrap();
        }

        let db = Database::open(&db_path).unwrap();

        // Then: the queued question survives, reads back, and carries no generation — so
        // it is re-asked exactly once instead of being taken for one this classifier
        // already answered.
        let run = vec!["window-a".to_string()];
        let queued = db.get_pending_proposal_for_events(&run).unwrap().unwrap();
        assert_eq!(queued.id, "queued");
        assert_eq!(queued.classifier_generation, None);
        assert!(
            !db.has_pending_proposal_for_events_at_generation(&run, 1)
                .unwrap()
        );

        // And: the stamp lands on the migrated column.
        assert_eq!(db.stamp_pending_proposals_for_events(&run, 1).unwrap(), 1);
        assert!(
            db.has_pending_proposal_for_events_at_generation(&run, 1)
                .unwrap()
        );
    }

    #[test]
    fn a_capture_only_machine_with_no_proposals_table_still_migrates() {
        // Given: a real schema-10 database from a machine that only ever captured events.
        //
        // devbox looked exactly like this — six tables, no `proposals`, because nothing
        // there ever ran the classifier. The v<=12 arm ALTERed that table unconditionally,
        // so the migration aborted and `tt` could not open the database at all. The other
        // migration fixtures all build a `proposals` table, which is precisely why none of
        // them caught it.
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("capture-only-v10.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_info (version INTEGER NOT NULL);
                 INSERT INTO schema_info (version) VALUES (10);
                 CREATE TABLE streams (
                    id TEXT PRIMARY KEY,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    name TEXT,
                    slug TEXT,
                    time_direct_ms INTEGER DEFAULT 0,
                    time_delegated_ms INTEGER DEFAULT 0,
                    first_event_at TEXT,
                    last_event_at TEXT,
                    needs_recompute INTEGER DEFAULT 0
                 );
                 CREATE TABLE events (
                    id TEXT PRIMARY KEY,
                    timestamp TEXT NOT NULL,
                    type TEXT NOT NULL,
                    source TEXT,
                    machine_id TEXT,
                    schema_version INTEGER,
                    cwd TEXT,
                    git_project TEXT,
                    git_workspace TEXT,
                    pane_id TEXT,
                    tmux_session TEXT,
                    window_index INTEGER,
                    status TEXT,
                    idle_duration_ms INTEGER,
                    action TEXT,
                    session_id TEXT,
                    stream_id TEXT,
                    assignment_source TEXT,
                    window_app_id TEXT,
                    window_title TEXT
                 );",
            )
            .unwrap();
        }

        // When: the current binary opens it.
        let db = Database::open(&db_path).unwrap();

        // Then: it migrated, and `proposals` was created complete rather than ALTERed.
        let run = vec!["window-a".to_string()];
        assert!(db.get_pending_proposal_for_events(&run).unwrap().is_none());
        assert!(
            !db.has_pending_proposal_for_events_at_generation(&run, 1)
                .unwrap(),
            "the generation column must exist on the freshly created table"
        );
    }

    /// Writes a raw `streams` row so a defect the type system forbids can be
    /// reproduced, and clears the one-shot marker so the repair runs again.
    fn plant_raw_created_at(db: &Database, stream_id: &str, created_at: &str) {
        db.insert_stream(&make_stream(stream_id, Some("planted")))
            .unwrap();
        db.conn
            .execute(
                "UPDATE streams SET created_at = ?1 WHERE id = ?2",
                params![created_at, stream_id],
            )
            .unwrap();
        db.conn
            .execute(
                "DELETE FROM meta WHERE key = ?1",
                params![STREAM_TIMESTAMPS_NORMALIZED_KEY],
            )
            .unwrap();
    }

    /// Given a `created_at` in `SQLite`'s `CURRENT_TIMESTAMP` shape, When the repair
    /// runs, Then the stream keeps its own creation time — read as UTC, not
    /// replaced by the wall clock, which is the defect this removes.
    #[test]
    fn normalize_recovers_the_original_creation_time_not_now() {
        let db = Database::open_in_memory().unwrap();
        plant_raw_created_at(&db, "office-admin-2026w14", "2026-04-05 21:12:35");

        db.normalize_stream_timestamps().unwrap();

        let stream = db.get_stream("office-admin-2026w14").unwrap().unwrap();
        assert_eq!(
            stream.created_at,
            Utc.with_ymd_and_hms(2026, 4, 5, 21, 12, 35).unwrap()
        );
    }

    /// Given a repaired stream, When its row is read back raw, Then the stored
    /// text is RFC 3339 so every later read parses without repair.
    #[test]
    fn normalize_rewrites_the_stored_text_as_rfc3339() {
        let db = Database::open_in_memory().unwrap();
        plant_raw_created_at(&db, "oc-voice-ce", "2026-03-04 14:32:13");

        db.normalize_stream_timestamps().unwrap();

        let stored: String = db
            .conn
            .query_row(
                "SELECT created_at FROM streams WHERE id = 'oc-voice-ce'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, "2026-03-04T14:32:13.000Z");
    }

    /// Given a timestamp no parser can read, When the stream is loaded, Then the
    /// read fails and names the stream — never substitutes a wall-clock time.
    #[test]
    fn unreadable_timestamp_errors_loudly_instead_of_substituting_now() {
        let db = Database::open_in_memory().unwrap();
        plant_raw_created_at(&db, "s-broken", "not a timestamp at all");

        db.normalize_stream_timestamps().unwrap();

        let err = db.get_stream("s-broken").unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains("s-broken"), "{rendered}");
        assert!(rendered.contains("created_at"), "{rendered}");
    }

    /// Given a database opened twice, When the second open runs, Then the repair
    /// is skipped — it is a one-shot data fix, not a per-open scan.
    #[test]
    fn normalize_runs_once_and_records_that_it_ran() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("normalize.db");

        {
            let db = Database::open(&db_path).unwrap();
            plant_raw_created_at(&db, "personal-2026w14", "2026-04-05 21:13:22");
        }

        let db = Database::open(&db_path).unwrap();
        assert_eq!(
            db.get_stream("personal-2026w14")
                .unwrap()
                .unwrap()
                .created_at,
            Utc.with_ymd_and_hms(2026, 4, 5, 21, 13, 22).unwrap()
        );

        let marker: String = db
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![STREAM_TIMESTAMPS_NORMALIZED_KEY],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(marker, "1");
    }

    /// Given the repair runs, When it commits, Then `db_version` is unchanged — a
    /// creation time feeds no attribution, so the daemon has nothing to recompute.
    #[test]
    fn normalize_does_not_bump_db_version() {
        let db = Database::open_in_memory().unwrap();
        plant_raw_created_at(&db, "startup-credits-2026w14", "2026-04-05 21:13:22");
        let before = db.get_db_version().unwrap();

        db.normalize_stream_timestamps().unwrap();

        assert_eq!(db.get_db_version().unwrap(), before);
    }

    /// Given no scan has completed, When the cursor is read, Then it is absent —
    /// a fresh database has no window it can safely skip, so ingest scans in full.
    #[test]
    fn session_scan_cursor_absent_on_fresh_db() {
        let db = Database::open_in_memory().unwrap();

        assert_eq!(db.get_session_scan_cursor().unwrap(), None);
    }

    /// Given a cursor is written, When it is read back, Then it round-trips at the
    /// millisecond precision every other timestamp in this schema uses.
    #[test]
    fn session_scan_cursor_round_trips() {
        let db = Database::open_in_memory().unwrap();
        let at = Utc.with_ymd_and_hms(2026, 8, 7, 1, 15, 30).unwrap();

        db.set_session_scan_cursor(at).unwrap();

        assert_eq!(db.get_session_scan_cursor().unwrap(), Some(at));
    }

    /// Given a cursor already exists, When a later one is written, Then it replaces
    /// the old value rather than erroring on the primary key.
    #[test]
    fn session_scan_cursor_overwrites_previous() {
        let db = Database::open_in_memory().unwrap();
        let first = Utc.with_ymd_and_hms(2026, 8, 7, 1, 0, 0).unwrap();
        let second = Utc.with_ymd_and_hms(2026, 8, 7, 2, 0, 0).unwrap();

        db.set_session_scan_cursor(first).unwrap();
        db.set_session_scan_cursor(second).unwrap();

        assert_eq!(db.get_session_scan_cursor().unwrap(), Some(second));
    }

    /// Given the cursor is written, When it commits, Then `db_version` is unchanged.
    ///
    /// A scan cursor is bookkeeping: it records how far ingest has read and changes
    /// no event, stream, or assignment. Bumping would make the daemon's 2s watcher
    /// recompute the status verdict on every ~30s ingest tick, for nothing.
    #[test]
    fn session_scan_cursor_does_not_bump_db_version() {
        let db = Database::open_in_memory().unwrap();
        let before = db.get_db_version().unwrap();

        db.set_session_scan_cursor(Utc::now()).unwrap();

        assert_eq!(db.get_db_version().unwrap(), before);
    }

    /// Given a cursor value that cannot be parsed, When it is read, Then the read
    /// fails rather than substituting a time.
    ///
    /// Silently treating an unreadable cursor as "no cursor" would be a full scan
    /// forever, and treating it as `now()` would skip every session written before
    /// this moment. Both hide the defect; erroring names it.
    #[test]
    fn session_scan_cursor_unreadable_value_is_an_error() {
        let db = Database::open_in_memory().unwrap();
        db.conn
            .execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES(?1, 'not-a-timestamp')",
                params![SESSION_SCAN_CURSOR_KEY],
            )
            .unwrap();

        assert!(db.get_session_scan_cursor().is_err());
    }

    #[test]
    fn test_open_fails_on_newer_schema() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("v14.db");

        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_info (version INTEGER NOT NULL);
                 INSERT INTO schema_info (version) VALUES (14);",
            )
            .unwrap();
        }

        assert!(matches!(
            Database::open(&db_path),
            Err(DbError::SchemaVersionMismatch { found: 14, .. })
        ));
    }

    fn make_event_with_source(id: &str, timestamp: DateTime<Utc>, source: &str) -> StoredEvent {
        StoredEvent {
            id: id.to_string(),
            timestamp,
            event_type: tt_core::EventType::TmuxPaneFocus,
            source: source.to_string(),
            machine_id: None,
            schema_version: 1,
            pane_id: None,
            tmux_session: None,
            window_index: None,
            git_project: None,
            git_workspace: None,
            status: None,
            idle_duration_ms: None,
            window_app_id: None,
            window_title: None,
            action: None,
            cwd: None,
            session_id: None,
            stream_id: None,
            assignment_source: None,
            data: json!({}),
        }
    }

    #[test]
    fn test_get_last_event_per_source_empty() {
        let db = Database::open_in_memory().unwrap();
        let statuses = db.get_last_event_per_source().unwrap();
        assert!(statuses.is_empty());
    }

    #[test]
    fn test_get_last_event_per_source_single_source() {
        let db = Database::open_in_memory().unwrap();

        let ts1 = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2025, 1, 15, 11, 0, 0).unwrap();

        db.insert_event(&make_event_with_source("e1", ts1, "remote.tmux"))
            .unwrap();
        db.insert_event(&make_event_with_source("e2", ts2, "remote.tmux"))
            .unwrap();

        let statuses = db.get_last_event_per_source().unwrap();

        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].source, "remote.tmux");
        assert_eq!(statuses[0].last_timestamp, ts2); // Should be the later timestamp
    }

    #[test]
    fn test_get_last_event_per_source_multiple_sources() {
        let db = Database::open_in_memory().unwrap();

        let ts_tmux_old = Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0).unwrap();
        let ts_tmux_new = Utc.with_ymd_and_hms(2025, 1, 15, 11, 0, 0).unwrap();
        let ts_agent = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();

        db.insert_event(&make_event_with_source("e1", ts_tmux_old, "remote.tmux"))
            .unwrap();
        db.insert_event(&make_event_with_source("e2", ts_tmux_new, "remote.tmux"))
            .unwrap();
        db.insert_event(&make_event_with_source("e3", ts_agent, "remote.agent"))
            .unwrap();

        let statuses = db.get_last_event_per_source().unwrap();

        assert_eq!(statuses.len(), 2);

        // Find each source in results (order is by last_timestamp DESC)
        let tmux_status = statuses.iter().find(|s| s.source == "remote.tmux").unwrap();
        let agent_status = statuses
            .iter()
            .find(|s| s.source == "remote.agent")
            .unwrap();

        assert_eq!(tmux_status.last_timestamp, ts_tmux_new);
        assert_eq!(agent_status.last_timestamp, ts_agent);
    }

    #[test]
    fn test_get_last_event_per_source_ordered_by_timestamp() {
        let db = Database::open_in_memory().unwrap();

        // remote.agent has the most recent event
        let ts_tmux = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        let ts_agent = Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap();
        let ts_local = Utc.with_ymd_and_hms(2025, 1, 15, 11, 0, 0).unwrap();

        db.insert_event(&make_event_with_source("e1", ts_tmux, "remote.tmux"))
            .unwrap();
        db.insert_event(&make_event_with_source("e2", ts_agent, "remote.agent"))
            .unwrap();
        db.insert_event(&make_event_with_source("e3", ts_local, "local.window"))
            .unwrap();

        let statuses = db.get_last_event_per_source().unwrap();

        assert_eq!(statuses.len(), 3);
        // Should be ordered by timestamp DESC (most recent first)
        assert_eq!(statuses[0].source, "remote.agent"); // 12:00
        assert_eq!(statuses[1].source, "local.window"); // 11:00
        assert_eq!(statuses[2].source, "remote.tmux"); // 10:00
    }

    // ========== Stream Tests ==========

    fn make_stream(id: &str, name: Option<&str>) -> Stream {
        let now = Utc::now();
        Stream {
            id: id.to_string(),
            name: name.map(String::from),
            slug: None,
            description: None,
            color: None,
            created_at: now,
            updated_at: now,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: false,
        }
    }

    #[test]
    fn sessions_spanning_multiple_streams_reports_only_the_split_ones() {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("stream-a", Some("A")))
            .unwrap();
        db.insert_stream(&make_stream("stream-b", Some("B")))
            .unwrap();

        let mut split_one = make_event(
            "e1",
            Utc.with_ymd_and_hms(2026, 3, 1, 9, 0, 0).unwrap(),
            EventType::UserMessage,
        );
        split_one.session_id = Some("split".to_string());
        split_one.stream_id = Some("stream-a".to_string());
        let mut split_two = make_event(
            "e2",
            Utc.with_ymd_and_hms(2026, 3, 1, 9, 1, 0).unwrap(),
            EventType::UserMessage,
        );
        split_two.session_id = Some("split".to_string());
        split_two.stream_id = Some("stream-b".to_string());
        let mut settled = make_event(
            "e3",
            Utc.with_ymd_and_hms(2026, 3, 1, 9, 2, 0).unwrap(),
            EventType::UserMessage,
        );
        settled.session_id = Some("settled".to_string());
        settled.stream_id = Some("stream-a".to_string());

        let mut split_unassigned = make_event(
            "e4",
            Utc.with_ymd_and_hms(2026, 3, 1, 9, 3, 0).unwrap(),
            EventType::UserMessage,
        );
        split_unassigned.session_id = Some("split".to_string());

        let mut early_one = make_event(
            "e5",
            Utc.with_ymd_and_hms(2026, 3, 1, 9, 4, 0).unwrap(),
            EventType::UserMessage,
        );
        early_one.session_id = Some("aaa".to_string());
        early_one.stream_id = Some("stream-b".to_string());

        let mut early_two = make_event(
            "e6",
            Utc.with_ymd_and_hms(2026, 3, 1, 9, 5, 0).unwrap(),
            EventType::UserMessage,
        );
        early_two.session_id = Some("aaa".to_string());
        early_two.stream_id = Some("stream-a".to_string());

        let mut settled_unassigned = make_event(
            "e7",
            Utc.with_ymd_and_hms(2026, 3, 1, 9, 6, 0).unwrap(),
            EventType::UserMessage,
        );
        settled_unassigned.session_id = Some("settled".to_string());

        db.insert_events(&[
            split_one,
            split_two,
            settled,
            split_unassigned,
            early_one,
            early_two,
            settled_unassigned,
        ])
        .unwrap();

        assert_eq!(
            db.sessions_spanning_multiple_streams().unwrap(),
            vec![
                (
                    "aaa".to_string(),
                    vec!["stream-a".to_string(), "stream-b".to_string()]
                ),
                (
                    "split".to_string(),
                    vec!["stream-a".to_string(), "stream-b".to_string()]
                )
            ]
        );
    }

    #[test]
    fn test_insert_and_get_stream() {
        let db = Database::open_in_memory().unwrap();
        let stream = make_stream("stream-1", Some("time-tracker"));

        db.insert_stream(&stream).unwrap();

        let retrieved = db.get_stream("stream-1").unwrap();
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.id, "stream-1");
        assert_eq!(retrieved.name, Some("time-tracker".to_string()));
    }

    #[test]
    fn test_get_stream_not_found() {
        let db = Database::open_in_memory().unwrap();
        let result = db.get_stream("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_streams_empty() {
        let db = Database::open_in_memory().unwrap();
        let streams = db.get_streams().unwrap();
        assert!(streams.is_empty());
    }

    #[test]
    fn test_get_streams_returns_all() {
        let db = Database::open_in_memory().unwrap();

        db.insert_stream(&make_stream("s1", Some("project-a")))
            .unwrap();
        db.insert_stream(&make_stream("s2", Some("project-b")))
            .unwrap();
        db.insert_stream(&make_stream("s3", None)).unwrap();

        let streams = db.get_streams().unwrap();
        assert_eq!(streams.len(), 3);
    }

    #[test]
    fn test_assign_event_to_stream() {
        let db = Database::open_in_memory().unwrap();

        // Create an event
        let ts = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        db.insert_event(&make_event("e1", ts, tt_core::EventType::TmuxPaneFocus))
            .unwrap();

        // Create a stream
        db.insert_stream(&make_stream("s1", Some("test"))).unwrap();

        // Assign event to stream
        db.assign_event_to_stream("e1", "s1", "inferred").unwrap();

        // Verify event is assigned
        let events = db.get_events_by_stream("s1").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "e1");
    }

    #[test]
    fn reassign_session_as_user_claims_every_event_of_the_session() {
        // Given: one session whose events were filed by three different writers.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("s1", Some("right"))).unwrap();
        db.insert_stream(&make_stream("s2", Some("wrong"))).unwrap();
        let t = Utc.with_ymd_and_hms(2026, 6, 24, 15, 30, 0).unwrap();
        for (id, source) in [
            ("unassigned", None),
            ("machine", Some("inferred")),
            ("inherited", Some("inherited")),
        ] {
            let mut event = make_event(id, t, tt_core::EventType::AgentToolUse);
            event.session_id = Some("ses-a".to_string());
            db.insert_event(&event).unwrap();
            if let Some(source) = source {
                db.assign_event_to_stream(id, "s2", source).unwrap();
            }
        }

        // When: a human says the whole session belongs elsewhere.
        let moved = db.reassign_session_as_user("ses-a", "s1").unwrap();

        // Then: every event moves and every one records the human as its source.
        assert_eq!(moved, 3);
        let assigned = db.get_events_by_stream("s1").unwrap();
        assert_eq!(assigned.len(), 3);
        assert!(
            assigned
                .iter()
                .all(|event| event.assignment_source.as_deref() == Some("user")),
            "a human's verdict is recorded as 'user' whoever filed the event before"
        );
        assert!(db.get_events_by_stream("s2").unwrap().is_empty());
    }

    #[test]
    fn reassign_session_as_user_lets_a_human_change_their_own_mind() {
        // Given: a session a human already filed once.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("s1", Some("first"))).unwrap();
        db.insert_stream(&make_stream("s2", Some("second")))
            .unwrap();
        let t = Utc.with_ymd_and_hms(2026, 6, 24, 15, 30, 0).unwrap();
        let mut event = make_event("e1", t, tt_core::EventType::AgentToolUse);
        event.session_id = Some("ses-a".to_string());
        db.insert_event(&event).unwrap();
        db.reassign_session_as_user("ses-a", "s1").unwrap();

        // When: they correct it again.
        let moved = db.reassign_session_as_user("ses-a", "s2").unwrap();

        // Then: the second correction lands. The 'user' guard protects a human from
        // machines, never from themselves.
        assert_eq!(moved, 1);
        assert!(db.get_events_by_stream("s1").unwrap().is_empty());
        assert_eq!(db.get_events_by_stream("s2").unwrap().len(), 1);
    }

    #[test]
    fn assign_events_by_session_id_still_refuses_to_overwrite_a_human() {
        // Given: an event a human filed.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("s1", Some("human"))).unwrap();
        db.insert_stream(&make_stream("s2", Some("machine")))
            .unwrap();
        let t = Utc.with_ymd_and_hms(2026, 6, 24, 15, 30, 0).unwrap();
        let mut event = make_event("e1", t, tt_core::EventType::AgentToolUse);
        event.session_id = Some("ses-a".to_string());
        db.insert_event(&event).unwrap();
        db.reassign_session_as_user("ses-a", "s1").unwrap();

        // When: a machine writer tries the same session.
        let moved = db
            .assign_events_by_session_id("ses-a", "s2", "inferred")
            .unwrap();

        // Then: it is refused. Adding the human override must not have relaxed this.
        assert_eq!(moved, 0);
        assert_eq!(db.get_events_by_stream("s1").unwrap().len(), 1);
    }

    #[test]
    fn test_unassigned_event_ids() {
        let db = Database::open_in_memory().unwrap();

        let ts1 = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2025, 1, 15, 11, 0, 0).unwrap();

        db.insert_event(&make_event("e1", ts1, tt_core::EventType::TmuxPaneFocus))
            .unwrap();
        db.insert_event(&make_event("e2", ts2, tt_core::EventType::TmuxPaneFocus))
            .unwrap();

        // Create a stream and assign one event
        db.insert_stream(&make_stream("s1", Some("test"))).unwrap();
        db.assign_event_to_stream("e1", "s1", "inferred").unwrap();

        // Get events without stream
        let ids = db.unassigned_event_ids().unwrap();
        assert_eq!(ids, vec!["e2".to_string()]);
    }

    #[test]
    fn test_assign_events_batch() {
        let db = Database::open_in_memory().unwrap();

        // Create events
        let ts1 = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2025, 1, 15, 11, 0, 0).unwrap();
        let ts3 = Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap();

        db.insert_event(&make_event("e1", ts1, tt_core::EventType::TmuxPaneFocus))
            .unwrap();
        db.insert_event(&make_event("e2", ts2, tt_core::EventType::TmuxPaneFocus))
            .unwrap();
        db.insert_event(&make_event("e3", ts3, tt_core::EventType::TmuxPaneFocus))
            .unwrap();

        // Create a stream
        db.insert_stream(&make_stream("s1", Some("test"))).unwrap();

        // Batch assign
        let assignments = vec![
            ("e1".to_string(), "s1".to_string()),
            ("e2".to_string(), "s1".to_string()),
        ];
        let count = db
            .assign_events_to_stream(&assignments, "inferred")
            .unwrap();
        assert_eq!(count, 2);

        // Verify assignments
        let events = db.get_events_by_stream("s1").unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_assign_events_by_ids_assigns_requested_ids_only() {
        let db = Database::open_in_memory().unwrap();
        let ts = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();

        db.insert_event(&make_event("e1", ts, tt_core::EventType::WindowFocus))
            .unwrap();
        db.insert_event(&make_event("e2", ts, tt_core::EventType::WindowFocus))
            .unwrap();
        db.insert_event(&make_event("e3", ts, tt_core::EventType::WindowFocus))
            .unwrap();
        db.insert_stream(&make_stream("s1", Some("window-work")))
            .unwrap();

        let ids = vec!["e1".to_string(), "e3".to_string(), "missing".to_string()];
        let count = db.assign_events_by_ids(&ids, "s1", "inferred").unwrap();

        assert_eq!(count, 2);
        let assigned = db.get_events_by_stream("s1").unwrap();
        let assigned_ids: Vec<_> = assigned.iter().map(|event| event.id.as_str()).collect();
        assert_eq!(assigned_ids, vec!["e1", "e3"]);
        let unassigned = db.unassigned_event_ids().unwrap();
        assert_eq!(unassigned, vec!["e2".to_string()]);
    }

    #[test]
    fn test_assign_events_by_ids_chunks_large_id_lists() {
        let db = Database::open_in_memory().unwrap();
        let ts = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();

        for index in 0..550 {
            db.insert_event(&make_event(
                &format!("e{index}"),
                ts + chrono::Duration::seconds(index),
                tt_core::EventType::WindowFocus,
            ))
            .unwrap();
        }
        db.insert_event(&make_event(
            "left-alone",
            ts,
            tt_core::EventType::WindowFocus,
        ))
        .unwrap();
        db.insert_stream(&make_stream("s1", Some("window-work")))
            .unwrap();

        let ids: Vec<String> = (0..550).map(|index| format!("e{index}")).collect();
        let count = db.assign_events_by_ids(&ids, "s1", "inferred").unwrap();

        assert_eq!(count, 550);
        assert_eq!(db.get_events_by_stream("s1").unwrap().len(), 550);
        let unassigned = db.unassigned_event_ids().unwrap();
        assert_eq!(unassigned, vec!["left-alone".to_string()]);
    }

    #[test]
    fn test_assign_events_by_ids_preserves_user_assignments() {
        let db = Database::open_in_memory().unwrap();
        let ts = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();

        db.insert_stream(&make_stream("s_user", Some("manual")))
            .unwrap();
        db.insert_stream(&make_stream("s1", Some("window-work")))
            .unwrap();

        // A user-assigned event must NOT be reassigned.
        let mut user_event = make_event("e_user", ts, tt_core::EventType::WindowFocus);
        user_event.stream_id = Some("s_user".to_string());
        user_event.assignment_source = Some("user".to_string());
        db.insert_event(&user_event).unwrap();

        // An unassigned event should be reassigned.
        db.insert_event(&make_event(
            "e_inferred",
            ts,
            tt_core::EventType::WindowFocus,
        ))
        .unwrap();

        let ids = vec!["e_user".to_string(), "e_inferred".to_string()];
        let count = db.assign_events_by_ids(&ids, "s1", "inferred").unwrap();

        // Only the non-user event was updated; the user assignment is preserved.
        assert_eq!(count, 1);
        let user_stream = db.get_events_by_stream("s_user").unwrap();
        assert_eq!(user_stream.len(), 1);
        assert_eq!(user_stream[0].id, "e_user");
        let s1 = db.get_events_by_stream("s1").unwrap();
        let s1_ids: Vec<_> = s1.iter().map(|event| event.id.as_str()).collect();
        assert_eq!(s1_ids, vec!["e_inferred"]);
    }

    // ========== Tag Tests ==========

    #[test]
    fn test_add_tag_to_stream() {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("s1", Some("project-x")))
            .unwrap();

        db.add_tag("s1", "acme-webapp").unwrap();

        let tags = db.get_tags("s1").unwrap();
        assert_eq!(tags, vec!["acme-webapp"]);
    }

    #[test]
    fn test_add_duplicate_tag_is_idempotent() {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("s1", Some("project-x")))
            .unwrap();

        db.add_tag("s1", "acme-webapp").unwrap();
        db.add_tag("s1", "acme-webapp").unwrap(); // Duplicate - should be ignored

        let tags = db.get_tags("s1").unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0], "acme-webapp");
    }

    #[test]
    fn test_get_tags_returns_sorted() {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("s1", Some("project-x")))
            .unwrap();

        db.add_tag("s1", "zebra").unwrap();
        db.add_tag("s1", "alpha").unwrap();
        db.add_tag("s1", "beta").unwrap();

        let tags = db.get_tags("s1").unwrap();
        assert_eq!(tags, vec!["alpha", "beta", "zebra"]);
    }

    #[test]
    fn test_get_tags_for_stream_without_tags() {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("s1", Some("project-x")))
            .unwrap();

        let tags = db.get_tags("s1").unwrap();
        assert!(tags.is_empty());
    }

    #[test]
    fn test_delete_tag() {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("s1", Some("project-x")))
            .unwrap();

        db.add_tag("s1", "acme-webapp").unwrap();
        db.add_tag("s1", "urgent").unwrap();

        db.delete_tag("s1", "acme-webapp").unwrap();

        let tags = db.get_tags("s1").unwrap();
        assert_eq!(tags, vec!["urgent"]);
    }

    #[test]
    fn test_delete_stream_cascades_to_tags() {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("s1", Some("project-x")))
            .unwrap();
        db.add_tag("s1", "acme-webapp").unwrap();

        // Delete the stream via orphan cleanup (after clearing its events)
        db.delete_orphaned_streams().unwrap();

        // Tags should be gone too (via cascade)
        let tags = db.get_tags("s1").unwrap();
        assert!(tags.is_empty());
    }

    #[test]
    fn test_get_all_tags() {
        let db = Database::open_in_memory().unwrap();

        db.insert_stream(&make_stream("s1", Some("project-x")))
            .unwrap();
        db.insert_stream(&make_stream("s2", Some("project-y")))
            .unwrap();

        db.add_tag("s1", "acme-webapp").unwrap();
        db.add_tag("s1", "urgent").unwrap();
        db.add_tag("s2", "internal").unwrap();

        let all_tags = db.get_all_tags().unwrap();

        // Should return (stream_id, tags) pairs
        assert_eq!(all_tags.len(), 2);

        let s1_tags = all_tags.iter().find(|(id, _)| id == "s1").unwrap();
        assert_eq!(s1_tags.1, vec!["acme-webapp", "urgent"]);

        let s2_tags = all_tags.iter().find(|(id, _)| id == "s2").unwrap();
        assert_eq!(s2_tags.1, vec!["internal"]);
    }

    #[test]
    fn test_get_streams_with_tags() {
        let db = Database::open_in_memory().unwrap();

        db.insert_stream(&make_stream("s1", Some("project-x")))
            .unwrap();
        db.insert_stream(&make_stream("s2", Some("project-y")))
            .unwrap();

        db.add_tag("s1", "acme-webapp").unwrap();
        db.add_tag("s2", "internal").unwrap();

        let streams = db.get_streams_with_tags().unwrap();
        assert_eq!(streams.len(), 2);

        let s1 = streams.iter().find(|(s, _)| s.id == "s1").unwrap();
        assert_eq!(s1.1, vec!["acme-webapp"]);

        let s2 = streams.iter().find(|(s, _)| s.id == "s2").unwrap();
        assert_eq!(s2.1, vec!["internal"]);
    }

    #[test]
    fn test_resolve_stream_by_id() {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("s1", Some("project-x")))
            .unwrap();

        let stream = db.resolve_stream("s1").unwrap();
        assert!(stream.is_some());
        assert_eq!(stream.unwrap().id, "s1");
    }

    #[test]
    fn test_resolve_stream_by_name() {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("s1", Some("project-x")))
            .unwrap();

        let stream = db.resolve_stream("project-x").unwrap();
        assert!(stream.is_some());
        assert_eq!(stream.unwrap().id, "s1");
    }

    #[test]
    fn test_resolve_stream_not_found() {
        let db = Database::open_in_memory().unwrap();
        let stream = db.resolve_stream("nonexistent").unwrap();
        assert!(stream.is_none());
    }

    #[test]
    fn test_agent_session_storage() {
        use chrono::TimeZone;
        use tt_core::session::{AgentSession, SessionSource};

        let db = Database::open_in_memory().unwrap();

        let entry = AgentSession {
            session_id: "test-session".to_string(),
            source: SessionSource::default(),
            parent_session_id: None,
            session_type: tt_core::session::SessionType::default(),
            project_path: "/home/user/project".to_string(),
            project_name: "project".to_string(),
            start_time: chrono::Utc.with_ymd_and_hms(2026, 1, 29, 10, 0, 0).unwrap(),
            end_time: Some(chrono::Utc.with_ymd_and_hms(2026, 1, 29, 11, 0, 0).unwrap()),
            message_count: 10,
            summary: Some("Test session".to_string()),
            user_prompts: vec!["implement feature".to_string(), "add tests".to_string()],
            starting_prompt: Some("implement feature".to_string()),
            assistant_message_count: 5,
            tool_call_count: 12,
            user_message_timestamps: Vec::new(),
            tool_call_timestamps: Vec::new(),
        };

        db.upsert_agent_session(&entry, None).unwrap();

        let start = chrono::Utc.with_ymd_and_hms(2026, 1, 29, 9, 0, 0).unwrap();
        let end = chrono::Utc.with_ymd_and_hms(2026, 1, 29, 12, 0, 0).unwrap();
        let sessions = db.agent_sessions_in_range(start, end).unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].project_name, "project");
        assert_eq!(
            sessions[0].user_prompts,
            vec!["implement feature", "add tests"]
        );
        assert_eq!(
            sessions[0].starting_prompt.as_deref(),
            Some("implement feature")
        );
        assert_eq!(sessions[0].assistant_message_count, 5);
        assert_eq!(sessions[0].tool_call_count, 12);
        assert_eq!(sessions[0].source, SessionSource::Claude);
    }

    #[test]
    fn test_agent_session_source_opencode_roundtrip() {
        use chrono::TimeZone;
        use tt_core::session::{AgentSession, SessionSource};

        let db = Database::open_in_memory().unwrap();

        let entry = AgentSession {
            session_id: "ses_opencode_test".to_string(),
            source: SessionSource::OpenCode,
            parent_session_id: None,
            session_type: tt_core::session::SessionType::default(),
            project_path: "/home/user/project".to_string(),
            project_name: "project".to_string(),
            start_time: chrono::Utc.with_ymd_and_hms(2026, 1, 29, 10, 0, 0).unwrap(),
            end_time: None,
            message_count: 1,
            summary: None,
            user_prompts: vec![],
            starting_prompt: None,
            assistant_message_count: 0,
            tool_call_count: 0,
            user_message_timestamps: Vec::new(),
            tool_call_timestamps: Vec::new(),
        };

        db.upsert_agent_session(&entry, None).unwrap();

        let start = chrono::Utc.with_ymd_and_hms(2026, 1, 29, 9, 0, 0).unwrap();
        let end = chrono::Utc.with_ymd_and_hms(2026, 1, 29, 12, 0, 0).unwrap();
        let sessions = db.agent_sessions_in_range(start, end).unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "ses_opencode_test");
        assert_eq!(sessions[0].source, SessionSource::OpenCode);
    }

    // NOTE: Migration tests removed. Schema v7 is a breaking change - old databases
    // must be deleted and re-imported from events.jsonl.

    // ========== streams_in_range Tests ==========

    /// A stream row with both event-timestamp columns NULL — the shape every stream
    /// in the live table has, because nothing writes them.
    fn make_stream_row(id: &str, name: Option<&str>) -> Stream {
        let now = Utc::now();
        Stream {
            id: id.to_string(),
            name: name.map(String::from),
            slug: None,
            description: None,
            color: None,
            created_at: now,
            updated_at: now,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: false,
        }
    }

    /// Inserts a stream plus the two events that give it a span of `first`..`last`.
    ///
    /// The events are what `streams_in_range` reads. Setting the columns that name
    /// the same span would test nothing: no code writes them.
    fn insert_stream_spanning(
        db: &Database,
        id: &str,
        name: &str,
        first: DateTime<Utc>,
        last: DateTime<Utc>,
    ) {
        db.insert_stream(&make_stream_row(id, Some(name))).unwrap();
        for (index, at) in [first, last].into_iter().enumerate() {
            let mut event = make_event(
                &format!("{id}-{index}"),
                at,
                tt_core::EventType::WindowFocus,
            );
            event.stream_id = Some(id.to_string());
            db.insert_event(&event).unwrap();
        }
    }

    #[test]
    fn test_streams_in_range_empty() {
        let db = Database::open_in_memory().unwrap();
        let start = Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap();

        let streams = db.streams_in_range(start, end).unwrap();
        assert!(streams.is_empty());
    }

    #[test]
    fn test_streams_in_range_overlapping() {
        let db = Database::open_in_memory().unwrap();

        // Stream that overlaps with the query range
        let stream_first = Utc.with_ymd_and_hms(2025, 1, 15, 8, 0, 0).unwrap();
        let stream_last = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        insert_stream_spanning(&db, "s1", "overlapping", stream_first, stream_last);

        let start = Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap();

        let streams = db.streams_in_range(start, end).unwrap();
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].id, "s1");
    }

    #[test]
    fn test_streams_in_range_fully_contained() {
        let db = Database::open_in_memory().unwrap();

        // Stream fully contained within query range
        let stream_first = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        let stream_last = Utc.with_ymd_and_hms(2025, 1, 15, 11, 0, 0).unwrap();
        insert_stream_spanning(&db, "s1", "contained", stream_first, stream_last);

        let start = Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap();

        let streams = db.streams_in_range(start, end).unwrap();
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].id, "s1");
    }

    #[test]
    fn test_streams_in_range_contains_query() {
        let db = Database::open_in_memory().unwrap();

        // Stream that contains the query range
        let stream_first = Utc.with_ymd_and_hms(2025, 1, 15, 8, 0, 0).unwrap();
        let stream_last = Utc.with_ymd_and_hms(2025, 1, 15, 14, 0, 0).unwrap();
        insert_stream_spanning(&db, "s1", "container", stream_first, stream_last);

        let start = Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap();

        let streams = db.streams_in_range(start, end).unwrap();
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].id, "s1");
    }

    #[test]
    fn test_streams_in_range_before_query() {
        let db = Database::open_in_memory().unwrap();

        // Stream that ends before query range starts
        let stream_first = Utc.with_ymd_and_hms(2025, 1, 15, 6, 0, 0).unwrap();
        let stream_last = Utc.with_ymd_and_hms(2025, 1, 15, 8, 0, 0).unwrap();
        insert_stream_spanning(&db, "s1", "before", stream_first, stream_last);

        let start = Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap();

        let streams = db.streams_in_range(start, end).unwrap();
        assert!(streams.is_empty());
    }

    #[test]
    fn test_streams_in_range_after_query() {
        let db = Database::open_in_memory().unwrap();

        // Stream that starts after query range ends
        let stream_first = Utc.with_ymd_and_hms(2025, 1, 15, 13, 0, 0).unwrap();
        let stream_last = Utc.with_ymd_and_hms(2025, 1, 15, 14, 0, 0).unwrap();
        insert_stream_spanning(&db, "s1", "after", stream_first, stream_last);

        let start = Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap();

        let streams = db.streams_in_range(start, end).unwrap();
        assert!(streams.is_empty());
    }

    #[test]
    fn test_streams_in_range_excludes_streams_with_no_events() {
        let db = Database::open_in_memory().unwrap();

        // Two streams no event has ever pointed at: they have no span at all,
        // which is the only shape a missing span can take once it is read from
        // `events` — MIN and MAX are absent together.
        db.insert_stream(&make_stream_row("s1", Some("no-events")))
            .unwrap();
        db.insert_stream(&make_stream_row("s2", Some("also-no-events")))
            .unwrap();

        // Stream with events
        let stream_first = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        let stream_last = Utc.with_ymd_and_hms(2025, 1, 15, 11, 0, 0).unwrap();
        insert_stream_spanning(&db, "s3", "valid", stream_first, stream_last);

        let start = Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap();

        let streams = db.streams_in_range(start, end).unwrap();
        assert_eq!(streams.len(), 1);
        assert_eq!(streams[0].id, "s3");
    }

    #[test]
    fn test_streams_in_range_multiple_streams() {
        let db = Database::open_in_memory().unwrap();

        // Stream 1: 8:00-10:00 (overlaps with start)
        insert_stream_spanning(
            &db,
            "s1",
            "early",
            Utc.with_ymd_and_hms(2025, 1, 15, 8, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap(),
        );

        // Stream 2: 10:00-11:00 (fully contained)
        insert_stream_spanning(
            &db,
            "s2",
            "middle",
            Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2025, 1, 15, 11, 0, 0).unwrap(),
        );

        // Stream 3: 11:00-13:00 (overlaps with end)
        insert_stream_spanning(
            &db,
            "s3",
            "late",
            Utc.with_ymd_and_hms(2025, 1, 15, 11, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2025, 1, 15, 13, 0, 0).unwrap(),
        );

        // Stream 4: 14:00-15:00 (completely outside)
        insert_stream_spanning(
            &db,
            "s4",
            "outside",
            Utc.with_ymd_and_hms(2025, 1, 15, 14, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2025, 1, 15, 15, 0, 0).unwrap(),
        );

        let start = Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap();

        let streams = db.streams_in_range(start, end).unwrap();
        assert_eq!(streams.len(), 3);

        // Should be ordered by earliest event
        assert_eq!(streams[0].id, "s1");
        assert_eq!(streams[1].id, "s2");
        assert_eq!(streams[2].id, "s3");
    }

    #[test]
    fn test_streams_in_range_boundary_conditions() {
        let db = Database::open_in_memory().unwrap();

        // Stream that ends exactly at query start
        insert_stream_spanning(
            &db,
            "s1",
            "boundary-end",
            Utc.with_ymd_and_hms(2025, 1, 15, 8, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0).unwrap(),
        );

        // Stream that starts exactly at query end
        insert_stream_spanning(
            &db,
            "s2",
            "boundary-start",
            Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2025, 1, 15, 13, 0, 0).unwrap(),
        );

        let start = Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap();

        let streams = db.streams_in_range(start, end).unwrap();
        // Both should be included (inclusive boundaries)
        assert_eq!(streams.len(), 2);
    }

    #[test]
    fn test_migrate_legacy_event_types_updates_actions() {
        let db = Database::open_in_memory().unwrap();
        let ts = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        let ts_str = ts.to_rfc3339();

        db.conn
            .execute(
                "INSERT INTO events (id, timestamp, type, source, schema_version)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    "sess-session_start",
                    ts_str,
                    "session_start",
                    "remote.agent",
                    1
                ],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO events (id, timestamp, type, source, schema_version)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params!["sess-session_end", ts_str, "session_end", "remote.agent", 1],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO events (id, timestamp, type, source, schema_version)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    "sess-agent-session_start",
                    ts_str,
                    "agent_session",
                    "remote.agent",
                    1
                ],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO events (id, timestamp, type, source, schema_version)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    "sess-agent-session_end",
                    ts_str,
                    "agent_session",
                    "remote.agent",
                    1
                ],
            )
            .unwrap();

        let (migrated_start, migrated_end) = db.migrate_legacy_event_types().unwrap();
        assert_eq!(migrated_start, 2);
        assert_eq!(migrated_end, 2);

        let events = db.get_events(None, None).unwrap();
        let start = events
            .iter()
            .find(|event| event.id == "sess-session_start")
            .unwrap();
        let end = events
            .iter()
            .find(|event| event.id == "sess-session_end")
            .unwrap();
        let legacy_start = events
            .iter()
            .find(|event| event.id == "sess-agent-session_start")
            .unwrap();
        let legacy_end = events
            .iter()
            .find(|event| event.id == "sess-agent-session_end")
            .unwrap();

        assert_eq!(start.action.as_deref(), Some("started"));
        assert_eq!(end.action.as_deref(), Some("ended"));
        assert_eq!(legacy_start.action.as_deref(), Some("started"));
        assert_eq!(legacy_end.action.as_deref(), Some("ended"));
    }

    #[test]
    fn test_upsert_agent_session_stores_machine_id() {
        let db = Database::open_in_memory().unwrap();
        let session = tt_core::session::AgentSession {
            session_id: "test-session-1".to_string(),
            source: tt_core::session::SessionSource::Claude,
            parent_session_id: None,
            session_type: tt_core::session::SessionType::User,
            project_path: "/home/test/project".to_string(),
            project_name: "test-project".to_string(),
            start_time: Utc.with_ymd_and_hms(2026, 1, 29, 9, 0, 0).unwrap(),
            end_time: Some(Utc.with_ymd_and_hms(2026, 1, 29, 10, 0, 0).unwrap()),
            message_count: 5,
            summary: Some("Test session".to_string()),
            user_prompts: vec![],
            starting_prompt: None,
            assistant_message_count: 3,
            tool_call_count: 1,
            user_message_timestamps: Vec::new(),
            tool_call_timestamps: Vec::new(),
        };

        // Upsert with machine_id = Some("test-machine-uuid")
        db.upsert_agent_session(&session, Some("test-machine-uuid"))
            .unwrap();

        // Query and verify machine_id is stored
        let row: String = db
            .conn
            .query_row(
                "SELECT machine_id FROM agent_sessions WHERE session_id = ?1",
                ["test-session-1"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(row, "test-machine-uuid");

        // Upsert same session with machine_id = None (should overwrite)
        db.upsert_agent_session(&session, None).unwrap();

        // Query and verify machine_id is now NULL
        let result: Option<String> = db
            .conn
            .query_row(
                "SELECT machine_id FROM agent_sessions WHERE session_id = ?1",
                ["test-session-1"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn last_event_timestamp_for_machine_returns_the_newest_event() {
        // Given: two machines with interleaved events
        let db = Database::open_in_memory().unwrap();
        for (id, hours, machine) in [
            ("e1", 0, "machine-a"),
            ("e2", 5, "machine-b"),
            ("e3", 2, "machine-a"),
        ] {
            let mut event = make_event(id, ts(hours), tt_core::EventType::TmuxPaneFocus);
            event.machine_id = Some(machine.to_string());
            db.insert_event(&event).unwrap();
        }

        // When / Then: each machine reports its own newest event
        assert_eq!(
            db.get_last_event_timestamp_for_machine("machine-a")
                .unwrap(),
            Some(format_timestamp(ts(2)))
        );
        assert_eq!(
            db.get_last_event_timestamp_for_machine("machine-b")
                .unwrap(),
            Some(format_timestamp(ts(5)))
        );
    }

    #[test]
    fn last_event_timestamp_for_machine_is_none_without_events() {
        // Given: a registered machine that has never sent an event
        let db = Database::open_in_memory().unwrap();
        db.upsert_machine("silent-machine", "ghost", None).unwrap();

        // When
        let latest = db
            .get_last_event_timestamp_for_machine("silent-machine")
            .unwrap();

        // Then
        assert_eq!(latest, None);
    }

    #[test]
    fn last_local_event_per_type_reports_only_this_machine() {
        // Given: a registered remote still emitting window_focus, plus a local watcher
        // whose newest window_focus is older, and an afk_change written before `tt init`
        // gave this machine an id. This is the incident's exact shape: a dead local
        // watcher while a synced remote keeps the event type looking alive.
        let db = Database::open_in_memory().unwrap();
        db.upsert_machine("remote-uuid", "devbox", None).unwrap();
        for (id, hours, event_type, machine) in [
            (
                "remote-1",
                10,
                tt_core::EventType::WindowFocus,
                Some("remote-uuid"),
            ),
            (
                "local-1",
                1,
                tt_core::EventType::WindowFocus,
                Some("local-uuid"),
            ),
            (
                "local-2",
                2,
                tt_core::EventType::WindowFocus,
                Some("local-uuid"),
            ),
            ("local-3", 3, tt_core::EventType::AfkChange, None),
        ] {
            let mut event = make_event(id, ts(hours), event_type);
            event.machine_id = machine.map(String::from);
            db.insert_event(&event).unwrap();
        }

        // When
        let statuses = db.last_local_event_per_type().unwrap();

        // Then: each type reports this machine's newest, and the remote's fresher
        // window_focus does not mask the local watcher's silence.
        assert_eq!(
            statuses,
            vec![
                LocalEventTypeStatus {
                    event_type: tt_core::EventType::AfkChange,
                    last_timestamp: ts(3),
                },
                LocalEventTypeStatus {
                    event_type: tt_core::EventType::WindowFocus,
                    last_timestamp: ts(2),
                },
            ]
        );
    }

    #[test]
    fn last_local_event_per_type_is_empty_without_local_events() {
        // Given: only a remote's synced events. A machine that never ran a watcher must
        // report nothing, because absence is not staleness.
        let db = Database::open_in_memory().unwrap();
        db.upsert_machine("remote-uuid", "devbox", None).unwrap();
        let mut event = make_event("remote-1", ts(1), tt_core::EventType::WindowFocus);
        event.machine_id = Some("remote-uuid".to_string());
        db.insert_event(&event).unwrap();

        // When / Then
        assert!(db.last_local_event_per_type().unwrap().is_empty());
    }

    #[test]
    fn fresh_db_streams_have_nullable_slug() {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("s1", Some("project-x")))
            .unwrap();
        let stream = db.get_stream("s1").unwrap().unwrap();
        assert_eq!(stream.slug, None);
    }

    #[test]
    fn migration_from_v9_adds_slug_column() {
        // Create a v9-shaped DB on disk, then reopen through Database::open.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tt.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_info (version INTEGER NOT NULL);
                 INSERT INTO schema_info (version) VALUES (9);
                 CREATE TABLE streams (
                    id TEXT PRIMARY KEY,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    name TEXT,
                    time_direct_ms INTEGER DEFAULT 0,
                    time_delegated_ms INTEGER DEFAULT 0,
                    first_event_at TEXT,
                    last_event_at TEXT,
                    needs_recompute INTEGER DEFAULT 0
                 );
                 INSERT INTO streams (id, created_at, updated_at, name)
                 VALUES ('old1', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'legacy stream');
                 CREATE TABLE proposals (
                    id TEXT PRIMARY KEY,
                    created_at TEXT NOT NULL,
                    session_id TEXT,
                    event_ids TEXT,
                    proposed_stream_id TEXT,
                    proposed_new_stream TEXT,
                    confidence REAL NOT NULL,
                    reasoning TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'pending'
                 );",
            )
            .unwrap();
        }
        let db = Database::open(&path).unwrap();
        let stream = db.get_stream("old1").unwrap().unwrap();
        assert_eq!(stream.name.as_deref(), Some("legacy stream"));
        assert_eq!(stream.slug, None);
    }

    #[test]
    fn get_stream_by_slug_roundtrip() {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("s1", Some("project-x")))
            .unwrap();
        db.set_stream_slug("s1", "proj-x").unwrap();
        let stream = db.get_stream_by_slug("proj-x").unwrap().unwrap();
        assert_eq!(stream.id, "s1");
        assert_eq!(stream.slug.as_deref(), Some("proj-x"));
        assert!(db.get_stream_by_slug("nope").unwrap().is_none());
    }

    #[test]
    fn set_stream_slug_overwrites_existing() {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("s1", Some("project-x")))
            .unwrap();
        db.set_stream_slug("s1", "old").unwrap();
        db.set_stream_slug("s1", "new").unwrap();
        assert!(db.get_stream_by_slug("old").unwrap().is_none());
        assert_eq!(db.get_stream_by_slug("new").unwrap().unwrap().id, "s1");
    }

    #[test]
    fn resolve_stream_prefers_id_then_slug_then_name() {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("s1", Some("long display name")))
            .unwrap();
        db.set_stream_slug("s1", "short").unwrap();
        assert_eq!(db.resolve_stream("s1").unwrap().unwrap().id, "s1");
        assert_eq!(db.resolve_stream("short").unwrap().unwrap().id, "s1");
        assert_eq!(
            db.resolve_stream("long display name").unwrap().unwrap().id,
            "s1"
        );
    }

    #[test]
    fn slug_unique_index_rejects_duplicates() {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("s1", Some("project-a")))
            .unwrap();
        db.insert_stream(&make_stream("s2", Some("project-b")))
            .unwrap();
        db.set_stream_slug("s1", "project").unwrap();

        let error = db.set_stream_slug("s2", "project").unwrap_err();
        assert!(matches!(
            error,
            DbError::SlugTaken { slug } if slug == "project"
        ));
    }

    #[test]
    fn schema_v11_migrates_v10_stream_metadata_and_initializes_db_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tt.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_info (version INTEGER NOT NULL);
                 INSERT INTO schema_info (version) VALUES (10);
                 CREATE TABLE streams (
                    id TEXT PRIMARY KEY,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    name TEXT,
                    slug TEXT,
                    time_direct_ms INTEGER DEFAULT 0,
                    time_delegated_ms INTEGER DEFAULT 0,
                    first_event_at TEXT,
                    last_event_at TEXT,
                    needs_recompute INTEGER DEFAULT 0
                 );
                 INSERT INTO streams (id, created_at, updated_at, name)
                 VALUES ('legacy', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'legacy');
                 CREATE TABLE proposals (
                    id TEXT PRIMARY KEY,
                    created_at TEXT NOT NULL,
                    session_id TEXT,
                    event_ids TEXT,
                    proposed_stream_id TEXT,
                    proposed_new_stream TEXT,
                    confidence REAL NOT NULL,
                    reasoning TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'pending'
                 );",
            )
            .unwrap();
        }

        let db = Database::open(&path).unwrap();

        let description: Option<String> = db
            .conn
            .query_row(
                "SELECT description FROM streams WHERE id = 'legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let version: i64 = db
            .conn
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'db_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(description, None);
        assert_eq!(version, 0);
    }

    #[test]
    fn proposal_round_trip_tracks_pending_and_rejected_memory() {
        let db = Database::open_in_memory().unwrap();
        let pending = Proposal {
            id: "proposal-pending".to_string(),
            created_at: ts(0),
            session_id: Some("session-a".to_string()),
            event_ids: None,
            proposed_stream_id: Some("stream-a".to_string()),
            proposed_new_stream: None,
            confidence: 0.91,
            reasoning: "matching project context".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        };
        let new_stream = Proposal {
            id: "proposal-new-stream".to_string(),
            created_at: ts(1),
            session_id: Some("session-a".to_string()),
            event_ids: None,
            proposed_stream_id: None,
            proposed_new_stream: Some(
                r#"{"name":"new work","description":null,"tags":[]}"#.to_string(),
            ),
            confidence: 0.76,
            reasoning: "no matching stream".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        };

        db.insert_proposal(&pending).unwrap();
        db.insert_proposal(&new_stream).unwrap();

        db.set_proposal_status("proposal-pending", ProposalStatus::Accepted)
            .unwrap();
        db.set_proposal_status("proposal-new-stream", ProposalStatus::Rejected)
            .unwrap();
        assert!(db.has_rejected_new_stream_proposal("session-a").unwrap());
    }

    fn session_proposal(id: &str, session_id: &str, created_at: DateTime<Utc>) -> Proposal {
        Proposal {
            id: id.to_string(),
            created_at,
            session_id: Some(session_id.to_string()),
            event_ids: None,
            proposed_stream_id: None,
            proposed_new_stream: Some(
                r#"{"name":"new work","description":null,"tags":[]}"#.to_string(),
            ),
            confidence: 0.7,
            reasoning: "review queue fixture".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        }
    }

    #[test]
    fn a_proposal_resolving_more_attention_outranks_an_older_one_resolving_less() {
        // Given: an old proposal covering one attention event, and a newer one covering
        // three. Age is the axis the review queue used to order by, so it is set against
        // attention here deliberately.
        let db = Database::open_in_memory().unwrap();
        let mut quiet = make_event("quiet-1", ts(0), EventType::UserMessage);
        quiet.session_id = Some("session-quiet".to_string());
        db.insert_event(&quiet).unwrap();
        for (index, event_type) in [
            EventType::UserMessage,
            EventType::WindowFocus,
            EventType::TmuxPaneFocus,
        ]
        .into_iter()
        .enumerate()
        {
            let mut busy = make_event(&format!("busy-{index}"), ts(2), event_type);
            busy.session_id = Some("session-busy".to_string());
            db.insert_event(&busy).unwrap();
        }
        db.insert_proposal(&session_proposal("proposal-quiet", "session-quiet", ts(0)))
            .unwrap();
        db.insert_proposal(&session_proposal("proposal-busy", "session-busy", ts(9)))
            .unwrap();

        // When
        let ranked = db.pending_proposals_by_attention().unwrap();

        // Then: the younger row leads, because reviewing it resolves more attention.
        assert_eq!(
            ranked
                .iter()
                .map(|entry| (entry.proposal.id.as_str(), entry.attention_events))
                .collect::<Vec<_>>(),
            [("proposal-busy", 3), ("proposal-quiet", 1)]
        );
    }

    #[test]
    fn a_window_run_proposal_counts_the_attention_inside_its_event_ids() {
        // Given: a proposal scoped to an explicit event set rather than a session. Two of
        // its members carry attention, one is agent activity, and a fourth attention event
        // is deliberately left outside the set.
        let db = Database::open_in_memory().unwrap();
        db.insert_events(&[
            make_event("focus-in-1", ts(0), EventType::WindowFocus),
            make_event("focus-in-2", ts(1), EventType::TmuxPaneFocus),
            make_event("tool-in", ts(2), EventType::AgentToolUse),
            make_event("focus-out", ts(3), EventType::WindowFocus),
        ])
        .unwrap();
        let mut run = session_proposal("proposal-run", "unused", ts(0));
        run.session_id = None;
        run.event_ids = Some(vec![
            "focus-in-1".to_string(),
            "focus-in-2".to_string(),
            "tool-in".to_string(),
        ]);
        db.insert_proposal(&run).unwrap();

        // When
        let ranked = db.pending_proposals_by_attention().unwrap();

        // Then: only the attention-bearing members of the named set are counted.
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].attention_events, 2);
    }

    #[test]
    fn a_proposal_resolving_no_attention_still_appears_and_sorts_last() {
        // Given: one proposal over attention and two over none — one whose session holds
        // only agent activity, one whose session holds nothing at all. This is a
        // re-ordering, never a filter, and equal attention has to order stably.
        let db = Database::open_in_memory().unwrap();
        let mut attention = make_event("focus", ts(0), EventType::WindowFocus);
        attention.session_id = Some("session-attention".to_string());
        let mut tool = make_event("tool", ts(0), EventType::AgentToolUse);
        tool.session_id = Some("session-silent-old".to_string());
        db.insert_events(&[attention, tool]).unwrap();
        db.insert_proposal(&session_proposal(
            "proposal-silent-old",
            "session-silent-old",
            ts(0),
        ))
        .unwrap();
        db.insert_proposal(&session_proposal(
            "proposal-silent-new",
            "session-silent-new",
            ts(4),
        ))
        .unwrap();
        db.insert_proposal(&session_proposal(
            "proposal-attention",
            "session-attention",
            ts(8),
        ))
        .unwrap();

        // When
        let ranked = db.pending_proposals_by_attention().unwrap();

        // Then: both silent proposals survive, behind the one holding attention and
        // oldest-first between themselves.
        assert_eq!(
            ranked
                .iter()
                .map(|entry| (entry.proposal.id.as_str(), entry.attention_events))
                .collect::<Vec<_>>(),
            [
                ("proposal-attention", 1),
                ("proposal-silent-old", 0),
                ("proposal-silent-new", 0),
            ]
        );
    }

    #[test]
    fn only_pending_proposals_reach_the_attention_ranked_queue() {
        // Given: one proposal in every terminal status alongside a pending one.
        let db = Database::open_in_memory().unwrap();
        for (id, status) in [
            ("proposal-accepted", ProposalStatus::Accepted),
            ("proposal-rejected", ProposalStatus::Rejected),
            ("proposal-superseded", ProposalStatus::Superseded),
            ("proposal-pending", ProposalStatus::Pending),
        ] {
            let mut proposal = session_proposal(id, "session-a", ts(0));
            proposal.status = status;
            db.insert_proposal(&proposal).unwrap();
        }

        // When / Then: only the question still waiting on a reviewer is queued.
        assert_eq!(
            db.pending_proposals_by_attention()
                .unwrap()
                .iter()
                .map(|entry| entry.proposal.id.as_str())
                .collect::<Vec<_>>(),
            ["proposal-pending"]
        );
    }

    #[test]
    fn get_proposals_still_lists_oldest_first_whatever_attention_it_holds() {
        // The attention ranking is a second view, not a change to an existing one:
        // `get_proposals` has other callers and keeps its created_at ordering.
        let db = Database::open_in_memory().unwrap();
        let mut busy = make_event("busy", ts(2), EventType::UserMessage);
        busy.session_id = Some("session-busy".to_string());
        db.insert_event(&busy).unwrap();
        db.insert_proposal(&session_proposal("proposal-quiet", "session-quiet", ts(0)))
            .unwrap();
        db.insert_proposal(&session_proposal("proposal-busy", "session-busy", ts(9)))
            .unwrap();

        // When
        let listed = db.get_proposals(Some(ProposalStatus::Pending)).unwrap();

        // Then
        assert_eq!(
            listed
                .iter()
                .map(|proposal| proposal.id.as_str())
                .collect::<Vec<_>>(),
            ["proposal-quiet", "proposal-busy"]
        );
    }

    #[test]
    fn event_proposal_lookup_and_classifier_helpers_protect_assignments() {
        let db = Database::open_in_memory().unwrap();
        for stream in ["user", "inferred", "classifier"] {
            db.insert_stream(&make_stream(stream, Some(stream)))
                .unwrap();
        }
        let mut user = make_event("user", ts(0), EventType::AgentToolUse);
        user.session_id = Some("session-a".to_string());
        user.stream_id = Some("user".to_string());
        user.assignment_source = Some("user".to_string());
        let mut inferred = make_event("inferred", ts(1), EventType::AgentToolUse);
        inferred.session_id = Some("session-a".to_string());
        inferred.stream_id = Some("inferred".to_string());
        inferred.assignment_source = Some("inferred".to_string());
        let mut todo_link = make_event("todo-link", ts(2), EventType::AgentToolUse);
        todo_link.session_id = Some("session-a".to_string());
        todo_link.stream_id = Some("user".to_string());
        todo_link.assignment_source = Some("todo_link".to_string());
        let mut unassigned = make_event("unassigned", ts(2), EventType::AgentToolUse);
        unassigned.session_id = Some("session-a".to_string());
        db.insert_events(&[user, inferred, todo_link, unassigned])
            .unwrap();

        assert_eq!(
            db.assign_unassigned_events_by_session_id("session-a", "classifier", "inferred")
                .unwrap(),
            1
        );
        let user_events = db.get_events_by_stream("user").unwrap();
        assert_eq!(user_events.len(), 2);
        assert_eq!(
            user_events
                .iter()
                .find(|event| event.id == "todo-link")
                .unwrap()
                .assignment_source
                .as_deref(),
            Some("todo_link")
        );
        assert_eq!(db.get_events_by_stream("inferred").unwrap().len(), 1);
        assert_eq!(
            db.reassign_inferred_events_by_session_id("session-a", "classifier", "inferred")
                .unwrap(),
            2
        );
        assert_eq!(db.get_events_by_stream("classifier").unwrap().len(), 2);

        let event_ids = vec!["user".to_string(), "inferred".to_string()];
        db.insert_proposal(&Proposal {
            id: "event-proposal".to_string(),
            created_at: ts(3),
            session_id: None,
            event_ids: Some(event_ids.clone()),
            proposed_stream_id: Some("classifier".to_string()),
            proposed_new_stream: None,
            confidence: 0.8,
            reasoning: "window run".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        })
        .unwrap();
        assert_eq!(
            db.get_pending_proposal_for_events(&event_ids)
                .unwrap()
                .unwrap()
                .id,
            "event-proposal"
        );
    }

    #[test]
    fn accept_proposal_assigns_session_events_to_existing_stream() {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("stream-a", Some("existing")))
            .unwrap();
        let mut event = make_event("event-a", ts(0), EventType::AgentToolUse);
        event.session_id = Some("session-a".to_string());
        db.insert_event(&event).unwrap();
        db.insert_proposal(&Proposal {
            id: "proposal-a".to_string(),
            created_at: ts(1),
            session_id: Some("session-a".to_string()),
            event_ids: None,
            proposed_stream_id: Some("stream-a".to_string()),
            proposed_new_stream: None,
            confidence: 0.9,
            reasoning: "existing stream".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        })
        .unwrap();
        let version_before = db.get_db_version().unwrap();

        let outcome = db.accept_proposal("proposal-a").unwrap();

        assert_eq!(outcome.stream_id, "stream-a");
        assert!(!outcome.created_stream);
        assert_eq!(outcome.events_assigned, 1);
        assert_eq!(db.get_events_by_stream("stream-a").unwrap().len(), 1);
        assert_eq!(
            db.get_proposals(Some(ProposalStatus::Accepted))
                .unwrap()
                .len(),
            1
        );
        assert!(db.get_stream("stream-a").unwrap().unwrap().needs_recompute);
        assert_eq!(db.get_db_version().unwrap(), version_before + 1);
    }

    #[test]
    fn accept_proposal_creates_stream_with_metadata_and_tags() {
        let db = Database::open_in_memory().unwrap();
        let mut event = make_event("event-a", ts(0), EventType::AgentToolUse);
        event.session_id = Some("session-a".to_string());
        db.insert_event(&event).unwrap();
        db.insert_proposal(&Proposal {
            id: "proposal-a".to_string(),
            created_at: ts(1),
            session_id: Some("session-a".to_string()),
            event_ids: None,
            proposed_stream_id: None,
            proposed_new_stream: Some(
                r#"{"name":"new work","description":"details","tags":["client","urgent"]}"#
                    .to_string(),
            ),
            confidence: 0.9,
            reasoning: "new stream".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        })
        .unwrap();
        let version_before = db.get_db_version().unwrap();

        let outcome = db.accept_proposal("proposal-a").unwrap();

        assert!(outcome.created_stream);
        assert_eq!(outcome.events_assigned, 1);
        let stream = db.get_stream(&outcome.stream_id).unwrap().unwrap();
        assert_eq!(stream.name.as_deref(), Some("new work"));
        assert_eq!(stream.description.as_deref(), Some("details"));
        assert!(stream.needs_recompute);
        assert_eq!(
            db.get_tags(&outcome.stream_id).unwrap(),
            vec!["client", "urgent"]
        );
        assert_eq!(db.get_db_version().unwrap(), version_before + 1);
    }

    #[test]
    fn accept_proposal_reuses_a_stream_already_carrying_the_proposed_name() {
        // Given: a stream, and a pending proposal that names a new stream by the same
        // name modulo whitespace. Accepting used to INSERT unconditionally, so this is
        // the second path that mints a duplicate — the classifier's own
        // `stream_named` is the first.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream(
            "held",
            Some("agent-c: eval-3 traccar environment"),
        ))
        .unwrap();
        let mut event = make_event("event-a", ts(0), EventType::AgentToolUse);
        event.session_id = Some("session-a".to_string());
        db.insert_event(&event).unwrap();
        db.insert_proposal(&Proposal {
            id: "proposal-a".to_string(),
            created_at: ts(1),
            session_id: Some("session-a".to_string()),
            event_ids: None,
            proposed_stream_id: None,
            proposed_new_stream: Some(
                r#"{"name":"  agent-c: eval-3 traccar environment ","tags":["env"]}"#.to_string(),
            ),
            confidence: 0.9,
            reasoning: "duplicate by whitespace".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        })
        .unwrap();

        // When
        let outcome = db.accept_proposal("proposal-a").unwrap();

        // Then: the events land on the stream that already held the name, and no
        // second row is minted.
        assert_eq!(outcome.stream_id, "held");
        assert!(!outcome.created_stream);
        assert_eq!(db.get_streams().unwrap().len(), 1);
        // The proposal's tags still arrive — accepting is a human's verdict about the
        // work, and reuse changes only which row holds it.
        assert_eq!(db.get_tags("held").unwrap(), vec!["env"]);
    }

    #[test]
    fn insert_stream_stores_a_normalized_name() {
        // Given: the shape the model emitted that minted
        // `" agent-c: eval-3 prometheus test-stage (round 2)"` beside its unspaced twin.
        let db = Database::open_in_memory().unwrap();

        // When
        db.insert_stream(&make_stream("s1", Some(" agent-c:  eval-3 prometheus ")))
            .unwrap();

        // Then: no new row can carry a name only whitespace tells apart.
        assert_eq!(
            db.get_stream("s1").unwrap().unwrap().name.as_deref(),
            Some("agent-c: eval-3 prometheus")
        );
    }

    #[test]
    fn rename_stream_stores_a_normalized_name() {
        // Given: the other writer of `streams.name`. The column's invariant has to hold
        // whichever path wrote it, or a rename re-opens the hole `insert_stream` closed.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("s1", Some("old name")))
            .unwrap();

        // When
        db.rename_stream("s1", "  workorder-5:  IPI envs ").unwrap();

        // Then
        assert_eq!(
            db.get_stream("s1").unwrap().unwrap().name.as_deref(),
            Some("workorder-5: IPI envs")
        );
    }

    #[test]
    fn find_stream_by_normalized_name_ignores_whitespace_on_either_side() {
        // Given: a stored name, and the same name as a model might re-emit it.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream(
            "s1",
            Some("agent-c: eval-3 saleor environment"),
        ))
        .unwrap();

        // When/Then
        for query in [
            "agent-c: eval-3 saleor environment",
            " agent-c: eval-3 saleor environment",
            "agent-c:  eval-3   saleor environment\n",
        ] {
            let found = db.find_stream_by_normalized_name(query).unwrap();
            assert_eq!(
                found.map(|stream| stream.id),
                Some("s1".to_owned()),
                "looking up {query:?}"
            );
        }
    }

    #[test]
    fn find_stream_by_normalized_name_normalizes_the_stored_side_too() {
        // Given: a legacy row that predates the write-side normalization. Three exist in
        // the live table, so the lookup cannot assume the column is already clean —
        // planted through the connection because no public path can produce it any more.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("s1", Some("placeholder")))
            .unwrap();
        db.conn
            .execute(
                "UPDATE streams SET name = ' agent-c: eval-3 prometheus test-stage ' WHERE id = 's1'",
                [],
            )
            .unwrap();

        // When/Then
        let found = db
            .find_stream_by_normalized_name("agent-c: eval-3 prometheus test-stage")
            .unwrap();
        assert_eq!(found.map(|stream| stream.id), Some("s1".to_owned()));
    }

    #[test]
    fn find_stream_by_normalized_name_returns_the_earliest_of_several_matches() {
        // Given: two rows already sharing a name — the live table holds three such
        // groups. Reuse has to converge on one row, or alternating answers would keep
        // splitting the same work across both.
        let db = Database::open_in_memory().unwrap();
        for (id, created) in [("younger", ts(50)), ("elder", ts(10))] {
            let mut stream = make_stream(id, Some("agent-c: eval-3 traccar environment"));
            stream.created_at = created;
            db.insert_stream(&stream).unwrap();
        }

        // When/Then
        let found = db
            .find_stream_by_normalized_name("agent-c: eval-3 traccar environment")
            .unwrap();
        assert_eq!(found.map(|stream| stream.id), Some("elder".to_owned()));
    }

    #[test]
    fn find_stream_by_normalized_name_does_not_fold_case() {
        // Given: the live table's `DPI:` (13 streams) and `dpi:` (7) prefixes. Merging
        // them is `tt streams merge`, not a side effect of a name lookup.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("s1", Some("DPI: ingest")))
            .unwrap();

        // When/Then
        assert!(
            db.find_stream_by_normalized_name("dpi: ingest")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn find_stream_by_normalized_name_ignores_unnamed_streams() {
        // Given: a stream with no name at all, which matches no query.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("s1", None)).unwrap();

        // When/Then
        assert!(
            db.find_stream_by_normalized_name("anything")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn stream_activity_windows_report_the_period_each_stream_spans() {
        // Given: two streams with events, and one with none. This is the roster's
        // ordering key, and it comes from `events` rather than `streams.first_event_at`/
        // `last_event_at`
        // because only `tt recompute` writes that column — 758 of the live table's 1,018
        // streams have it NULL, which would leave three quarters of the roster in an
        // undifferentiated tail.
        let db = Database::open_in_memory().unwrap();
        for id in ["busy", "quiet", "idle"] {
            db.insert_stream(&make_stream(id, Some(id))).unwrap();
        }
        for (event_id, at, stream) in [
            ("e1", ts(10), "busy"),
            ("e2", ts(90), "busy"),
            ("e3", ts(40), "quiet"),
        ] {
            let mut event = make_event(event_id, at, EventType::AgentToolUse);
            event.stream_id = Some(stream.to_owned());
            db.insert_event(&event).unwrap();
        }

        // When
        let windows = db.stream_activity_windows().unwrap();

        // Then: the full span per stream, and a stream with no events is simply absent —
        // it has no activity to report, and inventing one would order it as though it had.
        assert_eq!(
            windows.get("busy"),
            Some(&ActivityWindow {
                first: ts(10),
                last: ts(90),
            })
        );
        assert_eq!(
            windows.get("quiet"),
            Some(&ActivityWindow {
                first: ts(40),
                last: ts(40),
            })
        );
        assert_eq!(windows.get("idle"), None);
    }

    #[test]
    fn accept_proposal_preserves_newer_user_assignment() {
        let db = Database::open_in_memory().unwrap();
        for stream in ["original", "proposed"] {
            db.insert_stream(&make_stream(stream, Some(stream)))
                .unwrap();
        }
        let mut user_assigned = make_event("user", ts(0), EventType::AgentToolUse);
        user_assigned.session_id = Some("session-a".to_string());
        user_assigned.stream_id = Some("original".to_string());
        user_assigned.assignment_source = Some("user".to_string());
        let mut unassigned = make_event("unassigned", ts(1), EventType::AgentToolUse);
        unassigned.session_id = Some("session-a".to_string());
        db.insert_events(&[user_assigned, unassigned]).unwrap();
        db.insert_proposal(&Proposal {
            id: "proposal-a".to_string(),
            created_at: ts(2),
            session_id: Some("session-a".to_string()),
            event_ids: None,
            proposed_stream_id: Some("proposed".to_string()),
            proposed_new_stream: None,
            confidence: 0.9,
            reasoning: "preserve user assignment".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        })
        .unwrap();

        let outcome = db.accept_proposal("proposal-a").unwrap();

        assert_eq!(outcome.events_assigned, 1);
        assert_eq!(db.get_events_by_stream("original").unwrap().len(), 1);
        assert_eq!(db.get_events_by_stream("proposed").unwrap().len(), 1);
    }

    #[test]
    fn accept_proposal_rolls_back_when_existing_stream_is_missing() {
        let db = Database::open_in_memory().unwrap();
        let mut event = make_event("event-a", ts(0), EventType::AgentToolUse);
        event.session_id = Some("session-a".to_string());
        db.insert_event(&event).unwrap();
        db.insert_proposal(&Proposal {
            id: "proposal-a".to_string(),
            created_at: ts(1),
            session_id: Some("session-a".to_string()),
            event_ids: None,
            proposed_stream_id: Some("missing".to_string()),
            proposed_new_stream: None,
            confidence: 0.9,
            reasoning: "missing stream".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        })
        .unwrap();
        let version_before = db.get_db_version().unwrap();

        let result = db.accept_proposal("proposal-a");

        assert!(matches!(
            result,
            Err(DbError::ProposedStreamNotFound { stream_id }) if stream_id == "missing"
        ));
        assert!(db.get_streams().unwrap().is_empty());
        assert_eq!(db.unassigned_event_ids().unwrap().len(), 1);
        assert_eq!(
            db.get_proposals(Some(ProposalStatus::Pending))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(db.get_db_version().unwrap(), version_before);
    }

    #[test]
    fn accept_proposal_rolls_back_when_new_stream_json_is_malformed() {
        let db = Database::open_in_memory().unwrap();
        let mut event = make_event("event-a", ts(0), EventType::AgentToolUse);
        event.session_id = Some("session-a".to_string());
        db.insert_event(&event).unwrap();
        db.insert_proposal(&Proposal {
            id: "proposal-a".to_string(),
            created_at: ts(1),
            session_id: Some("session-a".to_string()),
            event_ids: None,
            proposed_stream_id: None,
            proposed_new_stream: Some("{malformed".to_string()),
            confidence: 0.9,
            reasoning: "bad new stream".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        })
        .unwrap();
        let version_before = db.get_db_version().unwrap();

        let result = db.accept_proposal("proposal-a");

        assert!(result.is_err());
        assert!(db.get_streams().unwrap().is_empty());
        assert_eq!(db.unassigned_event_ids().unwrap().len(), 1);
        assert_eq!(
            db.get_proposals(Some(ProposalStatus::Pending))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(db.get_db_version().unwrap(), version_before);
    }

    #[test]
    fn accept_proposal_rolls_back_writes_when_status_update_fails() {
        let db = Database::open_in_memory().unwrap();
        let mut event = make_event("event-a", ts(0), EventType::AgentToolUse);
        event.session_id = Some("session-a".to_string());
        db.insert_event(&event).unwrap();
        db.insert_proposal(&Proposal {
            id: "proposal-a".to_string(),
            created_at: ts(1),
            session_id: Some("session-a".to_string()),
            event_ids: None,
            proposed_stream_id: None,
            proposed_new_stream: Some(
                r#"{"name":"new work","description":"details","tags":["client"]}"#.to_string(),
            ),
            confidence: 0.9,
            reasoning: "database abort".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        })
        .unwrap();
        db.conn
            .execute_batch(
                "CREATE TRIGGER fail_proposal_accept BEFORE UPDATE OF status ON proposals
                 WHEN NEW.status = 'accepted'
                 BEGIN
                    SELECT RAISE(ABORT, 'status update failed');
                 END;",
            )
            .unwrap();
        let version_before = db.get_db_version().unwrap();

        let result = db.accept_proposal("proposal-a");

        assert!(result.is_err());
        assert!(db.get_streams().unwrap().is_empty());
        assert_eq!(db.unassigned_event_ids().unwrap().len(), 1);
        assert_eq!(
            db.get_proposals(Some(ProposalStatus::Pending))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(db.get_db_version().unwrap(), version_before);
    }

    #[test]
    fn reject_proposal_without_target_preserves_rejection_memory_and_events() {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("suggested", Some("suggested")))
            .unwrap();
        let mut event = make_event("event-a", ts(0), EventType::AgentToolUse);
        event.session_id = Some("session-a".to_string());
        db.insert_event(&event).unwrap();
        db.insert_proposal(&Proposal {
            id: "proposal-a".to_string(),
            created_at: ts(1),
            session_id: Some("session-a".to_string()),
            event_ids: None,
            proposed_stream_id: Some("suggested".to_string()),
            proposed_new_stream: None,
            confidence: 0.9,
            reasoning: "wrong stream".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        })
        .unwrap();
        let version_before = db.get_db_version().unwrap();

        let outcome = db.reject_proposal("proposal-a", None).unwrap();

        assert_eq!(outcome.stream_id, None);
        assert_eq!(outcome.events_assigned, 0);
        assert_eq!(db.unassigned_event_ids().unwrap().len(), 1);
        assert!(db.has_rejected_proposal("session-a", "suggested").unwrap());
        assert_eq!(
            db.get_proposals(Some(ProposalStatus::Rejected))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(db.get_db_version().unwrap(), version_before + 1);
    }

    #[test]
    fn reject_proposal_with_target_assigns_events_as_user() {
        let db = Database::open_in_memory().unwrap();
        for stream in ["suggested", "destination"] {
            db.insert_stream(&make_stream(stream, Some(stream)))
                .unwrap();
        }
        let mut event = make_event("event-a", ts(0), EventType::AgentToolUse);
        event.session_id = Some("session-a".to_string());
        db.insert_event(&event).unwrap();
        db.insert_proposal(&Proposal {
            id: "proposal-a".to_string(),
            created_at: ts(1),
            session_id: Some("session-a".to_string()),
            event_ids: None,
            proposed_stream_id: Some("suggested".to_string()),
            proposed_new_stream: None,
            confidence: 0.9,
            reasoning: "wrong stream".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        })
        .unwrap();

        let outcome = db
            .reject_proposal("proposal-a", Some("destination"))
            .unwrap();

        assert_eq!(outcome.stream_id.as_deref(), Some("destination"));
        assert_eq!(outcome.events_assigned, 1);
        let events = db.get_events_by_stream("destination").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].assignment_source.as_deref(), Some("user"));
        assert_eq!(
            db.get_proposals(Some(ProposalStatus::Rejected))
                .unwrap()
                .len(),
            1
        );
        assert!(
            db.get_stream("destination")
                .unwrap()
                .unwrap()
                .needs_recompute
        );
    }

    #[test]
    fn reject_proposal_rolls_back_assignments_when_status_update_fails() {
        let db = Database::open_in_memory().unwrap();
        for stream in ["suggested", "destination"] {
            db.insert_stream(&make_stream(stream, Some(stream)))
                .unwrap();
        }
        let mut event = make_event("event-a", ts(0), EventType::AgentToolUse);
        event.session_id = Some("session-a".to_string());
        db.insert_event(&event).unwrap();
        db.insert_proposal(&Proposal {
            id: "proposal-a".to_string(),
            created_at: ts(1),
            session_id: Some("session-a".to_string()),
            event_ids: None,
            proposed_stream_id: Some("suggested".to_string()),
            proposed_new_stream: None,
            confidence: 0.9,
            reasoning: "wrong stream".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        })
        .unwrap();
        db.conn
            .execute_batch(
                "CREATE TRIGGER fail_proposal_reject BEFORE UPDATE OF status ON proposals
                 WHEN NEW.status = 'rejected'
                 BEGIN
                    SELECT RAISE(ABORT, 'status update failed');
                 END;",
            )
            .unwrap();
        let version_before = db.get_db_version().unwrap();

        let result = db.reject_proposal("proposal-a", Some("destination"));

        assert!(result.is_err());
        assert_eq!(db.unassigned_event_ids().unwrap().len(), 1);
        assert!(db.get_events_by_stream("destination").unwrap().is_empty());
        assert_eq!(
            db.get_proposals(Some(ProposalStatus::Pending))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(db.get_db_version().unwrap(), version_before);
    }

    #[test]
    fn db_version_advances_for_attribution_mutations_not_bookkeeping() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.get_db_version().unwrap(), 0);
        db.insert_event(&make_event("event", ts(0), EventType::TmuxPaneFocus))
            .unwrap();
        assert_eq!(db.get_db_version().unwrap(), 1);
        db.insert_stream(&make_stream("stream", Some("stream")))
            .unwrap();
        assert_eq!(db.get_db_version().unwrap(), 2);
        db.assign_event_to_stream("event", "stream", "inferred")
            .unwrap();
        assert_eq!(db.get_db_version().unwrap(), 3);
        db.set_stream_description("stream", "description").unwrap();
        assert_eq!(db.get_db_version().unwrap(), 4);
        db.set_stream_color("stream", Some("#112233")).unwrap();
        assert_eq!(db.get_db_version().unwrap(), 5);
        let stream = db.get_stream("stream").unwrap().unwrap();
        assert_eq!(stream.description.as_deref(), Some("description"));
        assert_eq!(stream.color.as_deref(), Some("#112233"));
        db.upsert_machine("machine", "label", Some("event"))
            .unwrap();
        assert_eq!(db.get_db_version().unwrap(), 6);
        db.record_classification("session", 3).unwrap();
        db.mark_rechecked("session").unwrap();
        db.record_classifier_success(ts(1)).unwrap();
        db.record_classifier_failure(ts(2), "timeout").unwrap();
        db.record_classifier_unconfigured("ANTHROPIC_API_KEY is not set")
            .unwrap();
        assert_eq!(db.get_db_version().unwrap(), 6);
        let health = db.get_classifier_health().unwrap();
        assert_eq!(health.state, ClassifierHealthState::Unconfigured);
        assert_eq!(
            health.last_error.as_deref(),
            Some("ANTHROPIC_API_KEY is not set")
        );
        assert_eq!(health.last_failure_at, Some(ts(2)));
        assert_eq!(health.consecutive_failures, 0);

        let mut delete_me = make_event("delete-me", ts(3), EventType::TmuxPaneFocus);
        delete_me.machine_id = Some("machine".to_string());
        db.insert_event(&delete_me).unwrap();
        assert_eq!(db.get_db_version().unwrap(), 7);
        assert_eq!(db.delete_events_by_machine("machine").unwrap(), 1);
        assert_eq!(db.get_db_version().unwrap(), 8);
        assert_eq!(db.delete_events_by_machine("missing").unwrap(), 0);
        assert_eq!(db.get_db_version().unwrap(), 8);
    }

    #[test]
    fn classifier_support_accessors_round_trip_session_rechecks_and_rejections() {
        let db = Database::open_in_memory().unwrap();
        db.conn
            .execute(
                "INSERT INTO agent_sessions (session_id, source, project_path, project_name, start_time, message_count, machine_id)
                 VALUES ('session-a', 'claude', '/project', 'project', '2026-01-01T00:00:00Z', 2, 'machine-a')",
                [],
            )
            .unwrap();
        let (session, machine_id) = db.get_agent_session("session-a").unwrap().unwrap();
        assert_eq!(session.project_name, "project");
        assert_eq!(machine_id.as_deref(), Some("machine-a"));

        let mut event = make_event("unclassified", ts(0), EventType::AgentToolUse);
        event.session_id = Some("session-a".to_string());
        db.insert_event(&event).unwrap();

        db.record_classification("session-a", 2).unwrap();
        assert_eq!(
            db.get_recheck_candidates().unwrap(),
            vec![("session-a".to_string(), 2)]
        );
        db.mark_rechecked("session-a").unwrap();
        assert!(db.get_recheck_candidates().unwrap().is_empty());

        db.insert_proposal(&Proposal {
            id: "rejected-stream".to_string(),
            created_at: ts(0),
            session_id: Some("session-a".to_string()),
            event_ids: None,
            proposed_stream_id: Some("stream-a".to_string()),
            proposed_new_stream: None,
            confidence: 0.5,
            reasoning: "rejected before".to_string(),
            status: ProposalStatus::Rejected,
            classifier_generation: None,
        })
        .unwrap();
        assert!(db.has_rejected_proposal("session-a", "stream-a").unwrap());
        assert_eq!(
            db.get_proposals(Some(ProposalStatus::Rejected))
                .unwrap()
                .len(),
            1
        );
    }

    /// Builds a session whose only distinguishing fields are the ones selection reads.
    fn candidate_session(
        session_id: &str,
        session_type: tt_core::session::SessionType,
        start_time: DateTime<Utc>,
    ) -> tt_core::session::AgentSession {
        tt_core::session::AgentSession {
            session_id: session_id.to_string(),
            source: tt_core::session::SessionSource::OpenCode,
            parent_session_id: None,
            session_type,
            project_path: "/work/project".to_string(),
            project_name: "project".to_string(),
            start_time,
            end_time: None,
            message_count: 4,
            summary: None,
            user_prompts: vec!["do the thing".to_string()],
            starting_prompt: Some("do the thing".to_string()),
            assistant_message_count: 2,
            tool_call_count: 3,
            user_message_timestamps: Vec::new(),
            tool_call_timestamps: Vec::new(),
        }
    }

    /// Gives `session_id` one event that no stream owns yet.
    fn insert_unassigned_event(db: &Database, session_id: &str) {
        let mut event = make_event(
            &format!("event-{session_id}"),
            ts(0),
            EventType::AgentToolUse,
        );
        event.session_id = Some(session_id.to_string());
        db.insert_event(&event).unwrap();
    }

    #[test]
    fn unclassified_user_sessions_returns_only_user_sessions_newest_first() {
        // Given: three unclassified user sessions and one unclassified subagent.
        let db = Database::open_in_memory().unwrap();
        for (session_id, session_type, hours) in [
            ("user-oldest", SessionType::User, 0),
            ("user-newest", SessionType::User, 48),
            ("user-middle", SessionType::User, 24),
            ("subagent", SessionType::Subagent, 72),
        ] {
            db.upsert_agent_session(
                &candidate_session(session_id, session_type, ts(hours)),
                Some("machine-a"),
            )
            .unwrap();
            insert_unassigned_event(&db, session_id);
        }

        // When
        let selected = db.unclassified_user_sessions(10).unwrap();

        // Then: the subagent never reaches the classifier, and recency wins.
        let ids: Vec<&str> = selected
            .iter()
            .map(|(session, _)| session.session_id.as_str())
            .collect();
        assert_eq!(ids, ["user-newest", "user-middle", "user-oldest"]);
        assert_eq!(selected[0].1.as_deref(), Some("machine-a"));
    }

    #[test]
    fn unclassified_user_sessions_truncates_to_the_limit_keeping_the_newest() {
        // Given: three unclassified user sessions.
        let db = Database::open_in_memory().unwrap();
        for (session_id, hours) in [("oldest", 0), ("newest", 48), ("middle", 24)] {
            db.upsert_agent_session(
                &candidate_session(session_id, SessionType::User, ts(hours)),
                None,
            )
            .unwrap();
            insert_unassigned_event(&db, session_id);
        }

        // When: the pass is bounded at two.
        let selected = db.unclassified_user_sessions(2).unwrap();

        // Then: the bound drops the oldest, never the newest.
        let ids: Vec<&str> = selected
            .iter()
            .map(|(session, _)| session.session_id.as_str())
            .collect();
        assert_eq!(ids, ["newest", "middle"]);
    }

    #[test]
    fn unclassified_user_sessions_skips_sessions_whose_events_all_have_streams() {
        // Given: one classified user session and one still unassigned.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("stream-a", None)).unwrap();
        for session_id in ["classified", "unassigned"] {
            db.upsert_agent_session(
                &candidate_session(session_id, SessionType::User, ts(0)),
                None,
            )
            .unwrap();
            insert_unassigned_event(&db, session_id);
        }
        db.assign_events_by_session_id("classified", "stream-a", "inferred")
            .unwrap();

        // When
        let selected = db.unclassified_user_sessions(10).unwrap();

        // Then
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].0.session_id, "unassigned");
    }

    #[test]
    fn subagent_ids_for_parent_lists_only_that_parents_children() {
        // Given: two subagents of one parent and one subagent of another.
        let db = Database::open_in_memory().unwrap();
        db.upsert_agent_session(&candidate_session("parent", SessionType::User, ts(0)), None)
            .unwrap();
        db.upsert_agent_session(&candidate_session("other", SessionType::User, ts(0)), None)
            .unwrap();
        for (session_id, parent) in [("sub-a", "parent"), ("sub-b", "parent"), ("sub-c", "other")] {
            let mut subagent = candidate_session(session_id, SessionType::Subagent, ts(0));
            subagent.parent_session_id = Some(parent.to_string());
            db.upsert_agent_session(&subagent, None).unwrap();
        }

        // When
        let mut ids = db.subagent_ids_for_parent("parent").unwrap();
        ids.sort();

        // Then
        assert_eq!(ids, ["sub-a", "sub-b"]);
    }

    /// Stores a user session that ran no tool, with the message depth the junk rule reads.
    fn insert_junk_session(db: &Database, session_id: &str, message_count: i32) {
        let mut junk = candidate_session(session_id, SessionType::User, ts(0));
        junk.tool_call_count = 0;
        junk.message_count = message_count;
        db.upsert_agent_session(&junk, None).unwrap();
        insert_unassigned_event(db, session_id);
    }

    /// The stream and source one event ended up carrying.
    fn attribution_of(db: &Database, event_id: &str) -> (Option<String>, Option<String>) {
        let event = db
            .get_events(None, None)
            .unwrap()
            .into_iter()
            .find(|event| event.id == event_id)
            .unwrap();
        (event.stream_id, event.assignment_source)
    }

    #[test]
    fn bulk_junk_routing_routes_structurally_junk_sessions_and_records_them_as_classified() {
        // Given: two sessions that ran no tool and held at most one exchange — the shape
        // that used to occupy a model-call slot despite costing no call.
        let db = Database::open_in_memory().unwrap();
        insert_junk_session(&db, "junk-a", 2);
        insert_junk_session(&db, "junk-b", 1);

        // When
        let outcome = db.route_structurally_junk_sessions(100).unwrap();

        // Then: routed to the reserved junk stream, not deleted, and recorded as
        // classified so selection stops offering them.
        assert_eq!(outcome.sessions, 2);
        assert_eq!(outcome.events, 2);
        let junk_stream = db.junk_stream_id().unwrap();
        for session_id in ["junk-a", "junk-b"] {
            assert_eq!(
                attribution_of(&db, &format!("event-{session_id}")),
                (
                    Some(junk_stream.clone()),
                    Some(JUNK_ASSIGNMENT_SOURCE.to_string())
                ),
                "{session_id} was not routed to the junk stream"
            );
        }
        assert!(
            db.unclassified_user_sessions(100).unwrap().is_empty(),
            "routed junk still occupies the bounded candidate set"
        );
    }

    #[test]
    fn bulk_junk_routing_leaves_a_session_that_did_real_work_alone() {
        // Given: one junk session, one that called a tool, and one tool-free session with
        // real depth — the contract-review shape `is_structurally_junk` deliberately
        // sends to the classifier.
        let db = Database::open_in_memory().unwrap();
        insert_junk_session(&db, "junk-a", 2);
        db.upsert_agent_session(&candidate_session("worked", SessionType::User, ts(0)), None)
            .unwrap();
        insert_unassigned_event(&db, "worked");
        insert_junk_session(&db, "discussed", 6);

        // When
        let outcome = db.route_structurally_junk_sessions(100).unwrap();

        // Then: only the junk one moved, and the other two are still candidates.
        assert_eq!(outcome.sessions, 1);
        assert_eq!(attribution_of(&db, "event-worked"), (None, None));
        assert_eq!(attribution_of(&db, "event-discussed"), (None, None));
        let mut remaining: Vec<String> = db
            .unclassified_user_sessions(100)
            .unwrap()
            .into_iter()
            .map(|(session, _)| session.session_id)
            .collect();
        remaining.sort();
        assert_eq!(remaining, ["discussed", "worked"]);
    }

    #[test]
    fn bulk_junk_routing_never_touches_an_event_a_human_assigned() {
        // Given: a structurally junk session holding one unassigned event and one a human
        // spoke for. Only a human can speak for that content, so routing must not.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("stream-held", None)).unwrap();
        insert_junk_session(&db, "junk-a", 2);
        let mut human = make_event("event-human", ts(0), EventType::AgentToolUse);
        human.session_id = Some("junk-a".to_string());
        db.insert_event(&human).unwrap();
        db.assign_event_to_stream("event-human", "stream-held", "user")
            .unwrap();

        // When
        let outcome = db.route_structurally_junk_sessions(100).unwrap();

        // Then: the unassigned event moved, the human's verdict stands untouched.
        assert_eq!(outcome.events, 1);
        assert_eq!(
            attribution_of(&db, "event-human"),
            (Some("stream-held".to_string()), Some("user".to_string()))
        );
    }

    #[test]
    fn bulk_junk_routing_gives_subagents_of_a_junked_session_the_junk_stream() {
        // Given: a structurally junk parent with a subagent. A parent's zero tool calls
        // do not make this unreachable — `parent_session_id` is written by ingest, not
        // derived from a tool count — so dropping the inheritance the per-session path
        // performs would strand the subagent's events.
        let db = Database::open_in_memory().unwrap();
        insert_junk_session(&db, "junk-a", 2);
        let mut subagent = candidate_session("sub-a", SessionType::Subagent, ts(0));
        subagent.parent_session_id = Some("junk-a".to_string());
        db.upsert_agent_session(&subagent, None).unwrap();
        insert_unassigned_event(&db, "sub-a");

        // When
        let outcome = db.route_structurally_junk_sessions(100).unwrap();

        // Then: a subagent of work that does not exist is not work either.
        assert_eq!(outcome.sessions, 1);
        assert_eq!(outcome.events, 2);
        assert_eq!(
            attribution_of(&db, "event-sub-a"),
            (
                Some(db.junk_stream_id().unwrap()),
                Some(INHERITED_ASSIGNMENT_SOURCE.to_string())
            )
        );
    }

    #[test]
    fn bulk_junk_routing_stops_at_its_bound_and_bumps_db_version_only_when_it_moved_rows() {
        // Given: three junk sessions and a pass allowed to settle two.
        let db = Database::open_in_memory().unwrap();
        for index in 0..3 {
            insert_junk_session(&db, &format!("junk-{index}"), 2);
        }
        let before = db.get_db_version().unwrap();

        // When
        let bounded = db.route_structurally_junk_sessions(2).unwrap();
        let after_routing = db.get_db_version().unwrap();
        db.route_structurally_junk_sessions(100).unwrap();
        let idle = db.route_structurally_junk_sessions(100).unwrap();
        let after_idle = db.get_db_version().unwrap();

        // Then: the bound holds, a routing pass signals the daemon, and a pass that
        // moved nothing leaves `db_version` alone.
        assert_eq!(bounded.sessions, 2);
        assert!(after_routing > before);
        assert_eq!(idle, JunkRoutingOutcome::default());
        assert_eq!(after_idle, db.get_db_version().unwrap());
    }

    #[test]
    fn orphan_subagent_ids_lists_unclassified_subagents_whose_parent_was_never_indexed() {
        // Given: one subagent under an indexed parent, one under a parent that was
        // never ingested, and one orphan whose events are already classified.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("stream-a", None)).unwrap();
        db.upsert_agent_session(&candidate_session("parent", SessionType::User, ts(0)), None)
            .unwrap();
        for (session_id, parent) in [
            ("linked", "parent"),
            ("orphan", "never-ingested"),
            ("orphan-classified", "never-ingested"),
        ] {
            let mut subagent = candidate_session(session_id, SessionType::Subagent, ts(0));
            subagent.parent_session_id = Some(parent.to_string());
            db.upsert_agent_session(&subagent, None).unwrap();
            insert_unassigned_event(&db, session_id);
        }
        db.assign_events_by_session_id("orphan-classified", "stream-a", "inferred")
            .unwrap();

        // When
        let ids = db.orphan_subagent_ids().unwrap();

        // Then: only the orphan with work left to attribute.
        assert_eq!(ids, ["orphan"]);
    }

    /// Builds a classified session whose events all sit on one stream, plus one
    /// later event carrying that session id and no stream — the exact shape a pane
    /// focus stamped after its session was classified leaves behind.
    fn db_with_a_classified_session_and_a_late_event() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("stream-a", Some("eval harness")))
            .unwrap();
        insert_unassigned_event(&db, "ses-a");
        db.assign_events_by_session_id("ses-a", "stream-a", "inferred")
            .unwrap();
        db.record_classification("ses-a", 1).unwrap();

        let mut late = make_event("pane-late", ts(1), EventType::TmuxPaneFocus);
        late.session_id = Some("ses-a".to_string());
        db.insert_event(&late).unwrap();
        db
    }

    fn event_by_id(db: &Database, id: &str) -> StoredEvent {
        db.get_events(None, None)
            .unwrap()
            .into_iter()
            .find(|event| event.id == id)
            .unwrap()
    }

    #[test]
    fn a_classified_sessions_stream_reaches_an_event_stamped_after_it_was_classified() {
        // Given: the session→stream write already ran, so nothing else will ever
        // look at this event again.
        let db = db_with_a_classified_session_and_a_late_event();

        // When
        let claimed = db
            .claim_unassigned_events_for_classified_sessions()
            .unwrap();

        // Then: the event carries its own session's verdict, marked as membership
        // rather than as an inference of its own.
        assert_eq!(claimed, 1);
        let late = event_by_id(&db, "pane-late");
        assert_eq!(late.stream_id.as_deref(), Some("stream-a"));
        assert_eq!(
            late.assignment_source.as_deref(),
            Some(SESSION_MEMBERSHIP_ASSIGNMENT_SOURCE)
        );
    }

    #[test]
    fn a_classified_session_spanning_two_streams_is_skipped_entirely() {
        // Given: a classified session whose assigned events point at two streams —
        // one of them a human's correction — and one unassigned event beside them.
        let db = Database::open_in_memory().unwrap();
        for stream in ["stream-a", "stream-b"] {
            db.insert_stream(&make_stream(stream, Some(stream)))
                .unwrap();
        }
        let mut inferred = make_event("inferred", ts(0), EventType::AgentToolUse);
        inferred.session_id = Some("ses-split".to_string());
        inferred.stream_id = Some("stream-a".to_string());
        inferred.assignment_source = Some("inferred".to_string());
        let mut corrected = make_event("corrected", ts(1), EventType::AgentToolUse);
        corrected.session_id = Some("ses-split".to_string());
        corrected.stream_id = Some("stream-b".to_string());
        corrected.assignment_source = Some("user".to_string());
        let mut late = make_event("pane-late", ts(2), EventType::TmuxPaneFocus);
        late.session_id = Some("ses-split".to_string());
        db.insert_events(&[inferred, corrected, late]).unwrap();
        db.record_classification("ses-split", 1).unwrap();

        // When
        let claimed = db
            .claim_unassigned_events_for_classified_sessions()
            .unwrap();

        // Then: no winner is picked and no tie is broken.
        assert_eq!(claimed, 0);
        assert_eq!(event_by_id(&db, "pane-late").stream_id, None);
    }

    #[test]
    fn an_event_whose_session_is_not_classified_is_left_alone() {
        // Given: a session with one event on a stream — assigned by the terminal-focus
        // pass, not by a verdict about this session's content — and one unassigned.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("stream-a", Some("eval harness")))
            .unwrap();
        let mut focus = make_event("focus", ts(0), EventType::WindowFocus);
        focus.session_id = Some("ses-unclassified".to_string());
        focus.stream_id = Some("stream-a".to_string());
        focus.assignment_source = Some("terminal_focus".to_string());
        let mut late = make_event("pane-late", ts(1), EventType::TmuxPaneFocus);
        late.session_id = Some("ses-unclassified".to_string());
        db.insert_events(&[focus, late]).unwrap();

        // When
        let claimed = db
            .claim_unassigned_events_for_classified_sessions()
            .unwrap();

        // Then: propagation needs a verdict about the session, which no pass has made.
        assert_eq!(claimed, 0);
        assert_eq!(event_by_id(&db, "pane-late").stream_id, None);
    }

    #[test]
    fn claiming_never_overwrites_a_human_assignment() {
        // Given: a classified session whose stream a human has also spoken for, so the
        // session stays unambiguous — a human correction that *splits* a session makes
        // it ambiguous instead, which the two-stream test covers.
        let db = db_with_a_classified_session_and_a_late_event();
        let mut spoken_for = make_event("human", ts(2), EventType::TmuxPaneFocus);
        spoken_for.session_id = Some("ses-a".to_string());
        spoken_for.stream_id = Some("stream-a".to_string());
        spoken_for.assignment_source = Some("user".to_string());
        db.insert_event(&spoken_for).unwrap();

        // When
        let claimed = db
            .claim_unassigned_events_for_classified_sessions()
            .unwrap();

        // Then: only the unassigned event moved, and the human's row keeps its source
        // rather than being restamped as membership.
        assert_eq!(claimed, 1);
        let human = event_by_id(&db, "human");
        assert_eq!(human.stream_id.as_deref(), Some("stream-a"));
        assert_eq!(human.assignment_source.as_deref(), Some("user"));
        assert_eq!(
            event_by_id(&db, "pane-late").assignment_source.as_deref(),
            Some(SESSION_MEMBERSHIP_ASSIGNMENT_SOURCE)
        );
    }

    #[test]
    fn claiming_bumps_db_version_only_when_rows_changed() {
        // Given: one event this pass will claim.
        let db = db_with_a_classified_session_and_a_late_event();
        let before = db.get_db_version().unwrap();

        // When: the pass runs, then runs again with nothing left to claim.
        assert_eq!(
            db.claim_unassigned_events_for_classified_sessions()
                .unwrap(),
            1
        );
        let after_change = db.get_db_version().unwrap();
        assert_eq!(
            db.claim_unassigned_events_for_classified_sessions()
                .unwrap(),
            0
        );
        let after_noop = db.get_db_version().unwrap();

        // Then: the daemon is signalled once, by the run that changed something.
        assert!(
            after_change > before,
            "expected a bump: {before} -> {after_change}"
        );
        assert_eq!(after_noop, after_change);
    }

    #[test]
    fn junk_stream_id_creates_the_reserved_stream_once_and_reuses_it() {
        // Given: a database that has never junked anything.
        let db = Database::open_in_memory().unwrap();

        // When: two separate passes need the junk stream.
        let first = db.junk_stream_id().unwrap();
        let second = db.junk_stream_id().unwrap();

        // Then: one reserved stream, resolvable by its stable slug.
        assert_eq!(first, second);
        assert_eq!(db.get_streams().unwrap().len(), 1);
        let reserved = db.get_stream_by_slug(JUNK_STREAM_SLUG).unwrap().unwrap();
        assert_eq!(reserved.id, first);
    }

    #[test]
    fn a_session_awaiting_review_is_still_offered_to_a_later_pass() {
        // Given: two unclassified user sessions, one of which the classifier already
        // answered below the confidence threshold.
        let db = Database::open_in_memory().unwrap();
        for session_id in ["awaiting-review", "unanswered"] {
            db.upsert_agent_session(
                &candidate_session(session_id, SessionType::User, ts(0)),
                None,
            )
            .unwrap();
            insert_unassigned_event(&db, session_id);
        }
        db.insert_stream(&make_stream("stream-a", None)).unwrap();
        db.insert_proposal(&Proposal {
            id: "proposal-a".to_string(),
            created_at: ts(0),
            session_id: Some("awaiting-review".to_string()),
            event_ids: None,
            proposed_stream_id: Some("stream-a".to_string()),
            proposed_new_stream: None,
            confidence: 0.5,
            reasoning: "unsure".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        })
        .unwrap();

        // When
        let selected = db.unclassified_user_sessions(10).unwrap();

        // Then: escalating a question to a human does not retire the machine's ability
        // to answer it later. Excluding these froze every August candidate and 157 of
        // 185 July ones behind proposals no reviewer had reached.
        let mut session_ids: Vec<String> = selected
            .into_iter()
            .map(|(session, _)| session.session_id)
            .collect();
        session_ids.sort();
        assert_eq!(session_ids, ["awaiting-review", "unanswered"]);
    }

    #[test]
    fn a_pending_proposal_naming_a_vanished_stream_does_not_exile_its_session() {
        // Given: a session whose only pending proposal names a stream `tt streams
        // dissolve` has since deleted, so no human can ever accept it.
        let db = Database::open_in_memory().unwrap();
        db.upsert_agent_session(
            &candidate_session("stranded", SessionType::User, ts(0)),
            None,
        )
        .unwrap();
        insert_unassigned_event(&db, "stranded");
        db.insert_proposal(&Proposal {
            id: "proposal-a".to_string(),
            created_at: ts(0),
            session_id: Some("stranded".to_string()),
            event_ids: None,
            proposed_stream_id: Some("dissolved-stream".to_string()),
            proposed_new_stream: None,
            confidence: 0.5,
            reasoning: "unsure".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        })
        .unwrap();

        // When
        let selected = db.unclassified_user_sessions(10).unwrap();

        // Then: selection no longer reads proposals at all, so the shape that used to
        // need its own exemption is covered by the general rule.
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].0.session_id, "stranded");
    }

    #[test]
    fn a_pending_new_stream_proposal_no_longer_holds_its_session_back() {
        // Given: a proposal that would mint a stream. It names no existing id, so it is
        // answerable — and an answerable proposal is exactly the shape that used to
        // freeze its session for good.
        let db = Database::open_in_memory().unwrap();
        db.upsert_agent_session(
            &candidate_session("awaiting-review", SessionType::User, ts(0)),
            None,
        )
        .unwrap();
        insert_unassigned_event(&db, "awaiting-review");
        db.insert_proposal(&Proposal {
            id: "proposal-a".to_string(),
            created_at: ts(0),
            session_id: Some("awaiting-review".to_string()),
            event_ids: None,
            proposed_stream_id: None,
            proposed_new_stream: Some(
                json!({"name": "New stream", "description": "", "tags": []}).to_string(),
            ),
            confidence: 0.5,
            reasoning: "unsure".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        })
        .unwrap();

        // When / Then
        let selected = db.unclassified_user_sessions(10).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].0.session_id, "awaiting-review");
    }

    #[test]
    fn a_window_run_proposal_naming_a_vanished_stream_does_not_block_reclassification() {
        // Given: the window-run dedup lookup, holding an unacceptable answer.
        let db = Database::open_in_memory().unwrap();
        let event_ids = vec!["window-a".to_string()];
        db.insert_proposal(&Proposal {
            id: "proposal-a".to_string(),
            created_at: ts(0),
            session_id: None,
            event_ids: Some(event_ids.clone()),
            proposed_stream_id: Some("dissolved-stream".to_string()),
            proposed_new_stream: None,
            confidence: 0.5,
            reasoning: "unsure".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        })
        .unwrap();

        // When / Then
        assert!(
            db.get_pending_proposal_for_events(&event_ids)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_window_run_proposal_naming_a_live_stream_is_still_found_for_dedup() {
        // Given: the same lookup, holding an answer a human can still act on. Finding it
        // stops a second proposal being filed; it no longer stops the run being
        // re-asked.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("stream-a", None)).unwrap();
        let event_ids = vec!["window-a".to_string()];
        db.insert_proposal(&Proposal {
            id: "proposal-a".to_string(),
            created_at: ts(0),
            session_id: None,
            event_ids: Some(event_ids.clone()),
            proposed_stream_id: Some("stream-a".to_string()),
            proposed_new_stream: None,
            confidence: 0.5,
            reasoning: "unsure".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        })
        .unwrap();

        // When / Then
        assert!(
            db.get_pending_proposal_for_events(&event_ids)
                .unwrap()
                .is_some()
        );
    }

    /// The stored review state of one proposal, read back through the status parser.
    fn proposal_status(db: &Database, proposal_id: &str) -> ProposalStatus {
        db.get_proposals(None)
            .unwrap()
            .into_iter()
            .find(|proposal| proposal.id == proposal_id)
            .unwrap()
            .status
    }

    #[test]
    fn has_pending_proposal_for_session_reads_only_answerable_pending_rows() {
        // Given: one session per proposal state the duplicate check has to tell apart.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("stream-a", None)).unwrap();
        for (proposal_id, session_id, stream_id, status) in [
            (
                "answerable",
                "answerable",
                "stream-a",
                ProposalStatus::Pending,
            ),
            (
                "stranded",
                "stranded",
                "dissolved-stream",
                ProposalStatus::Pending,
            ),
            ("rejected", "rejected", "stream-a", ProposalStatus::Rejected),
            ("accepted", "accepted", "stream-a", ProposalStatus::Accepted),
            (
                "superseded",
                "superseded",
                "stream-a",
                ProposalStatus::Superseded,
            ),
        ] {
            db.insert_proposal(&Proposal {
                id: proposal_id.to_string(),
                created_at: ts(0),
                session_id: Some(session_id.to_string()),
                event_ids: None,
                proposed_stream_id: Some(stream_id.to_string()),
                proposed_new_stream: None,
                confidence: 0.5,
                reasoning: "unsure".to_string(),
                status,
                classifier_generation: None,
            })
            .unwrap();
        }

        // When / Then: only a proposal a reviewer can still act on suppresses a second
        // one. A stranded row suppresses nothing, exactly as it exiles nothing.
        assert!(db.has_pending_proposal_for_session("answerable").unwrap());
        assert!(!db.has_pending_proposal_for_session("stranded").unwrap());
        assert!(!db.has_pending_proposal_for_session("rejected").unwrap());
        assert!(!db.has_pending_proposal_for_session("accepted").unwrap());
        assert!(!db.has_pending_proposal_for_session("superseded").unwrap());
        assert!(!db.has_pending_proposal_for_session("never-asked").unwrap());
    }

    #[test]
    fn superseding_retires_the_question_without_manufacturing_a_rejection() {
        // Given: a session carrying one pending proposal and one the human rejected.
        let db = Database::open_in_memory().unwrap();
        for stream_id in ["stream-a", "stream-b"] {
            db.insert_stream(&make_stream(stream_id, None)).unwrap();
        }
        for (proposal_id, stream_id, status) in [
            ("pending", "stream-a", ProposalStatus::Pending),
            ("rejected", "stream-b", ProposalStatus::Rejected),
        ] {
            db.insert_proposal(&Proposal {
                id: proposal_id.to_string(),
                created_at: ts(0),
                session_id: Some("session-a".to_string()),
                event_ids: None,
                proposed_stream_id: Some(stream_id.to_string()),
                proposed_new_stream: None,
                confidence: 0.5,
                reasoning: "unsure".to_string(),
                status,
                classifier_generation: None,
            })
            .unwrap();
        }
        let version_before = db.get_db_version().unwrap();

        // When: a later verdict answers the same session.
        let superseded = db
            .supersede_pending_proposals_for_session("session-a")
            .unwrap();

        // Then: the queue loses the question and the human's own verdict is untouched.
        assert_eq!(superseded, 1);
        assert_eq!(proposal_status(&db, "pending"), ProposalStatus::Superseded);
        assert_eq!(proposal_status(&db, "rejected"), ProposalStatus::Rejected);
        // A superseded row is not a rejection, so it suppresses no later answer — the
        // whole point of not writing `rejected` on the user's behalf.
        assert!(!db.has_rejected_proposal("session-a", "stream-a").unwrap());
        assert!(db.has_rejected_proposal("session-a", "stream-b").unwrap());
        // Superseding changes no assignment of its own, so the daemon's change signal
        // stays put; it rides on the bump of the assignment that caused it.
        assert_eq!(db.get_db_version().unwrap(), version_before);
    }

    #[test]
    fn superseding_also_retires_a_proposal_no_reviewer_could_have_accepted() {
        // Given: a pending proposal naming a stream `tt streams dissolve` deleted.
        let db = Database::open_in_memory().unwrap();
        db.insert_proposal(&Proposal {
            id: "stranded".to_string(),
            created_at: ts(0),
            session_id: Some("session-a".to_string()),
            event_ids: None,
            proposed_stream_id: Some("dissolved-stream".to_string()),
            proposed_new_stream: None,
            confidence: 0.5,
            reasoning: "unsure".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        })
        .unwrap();

        // When
        let superseded = db
            .supersede_pending_proposals_for_session("session-a")
            .unwrap();

        // Then: deliberately wider than the read. A stranded row must not suppress a
        // fresh proposal, but it is still a question about this session, and a verdict
        // that answers it leaves nothing to strand.
        assert_eq!(superseded, 1);
        assert_eq!(proposal_status(&db, "stranded"), ProposalStatus::Superseded);
    }

    #[test]
    fn superseding_a_window_run_retires_only_that_runs_proposal() {
        // Given: two runs, each awaiting review.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("stream-a", None)).unwrap();
        let answered = vec!["window-a".to_string()];
        let untouched = vec!["window-b".to_string()];
        for (proposal_id, event_ids) in [("answered", &answered), ("untouched", &untouched)] {
            db.insert_proposal(&Proposal {
                id: proposal_id.to_string(),
                created_at: ts(0),
                session_id: None,
                event_ids: Some(event_ids.clone()),
                proposed_stream_id: Some("stream-a".to_string()),
                proposed_new_stream: None,
                confidence: 0.5,
                reasoning: "unsure".to_string(),
                status: ProposalStatus::Pending,
                classifier_generation: None,
            })
            .unwrap();
        }

        // When
        let superseded = db
            .supersede_pending_proposals_for_events(&answered)
            .unwrap();

        // Then: a run's proposal is keyed on its exact event set, so answering one run
        // says nothing about another.
        assert_eq!(superseded, 1);
        assert_eq!(proposal_status(&db, "answered"), ProposalStatus::Superseded);
        assert_eq!(proposal_status(&db, "untouched"), ProposalStatus::Pending);
        assert!(
            db.get_pending_proposal_for_events(&answered)
                .unwrap()
                .is_none()
        );
    }

    /// Files one pending window-run proposal carrying a stated generation.
    fn insert_window_proposal(
        db: &Database,
        proposal_id: &str,
        event_ids: &[String],
        stream_id: &str,
        classifier_generation: Option<u32>,
    ) {
        db.insert_proposal(&Proposal {
            id: proposal_id.to_string(),
            created_at: ts(0),
            session_id: None,
            event_ids: Some(event_ids.to_vec()),
            proposed_stream_id: Some(stream_id.to_string()),
            proposed_new_stream: None,
            confidence: 0.5,
            reasoning: "unsure".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation,
        })
        .unwrap();
    }

    #[test]
    fn a_window_run_is_only_answered_by_the_generation_its_proposal_names() {
        // Given: four runs, each queued by a different classifier or by none. The gate
        // exists because a bounded pass was spending its whole 101-call budget
        // re-asking 212 queued questions while 71,635 focus events went unreached, and
        // it is keyed on the generation because a *better* classifier must still be
        // able to re-answer every one of them.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("stream-a", None)).unwrap();
        let current = vec!["window-current".to_string()];
        let older = vec!["window-older".to_string()];
        let unstamped = vec!["window-unstamped".to_string()];
        let stranded = vec!["window-stranded".to_string()];
        insert_window_proposal(&db, "current", &current, "stream-a", Some(7));
        insert_window_proposal(&db, "older", &older, "stream-a", Some(6));
        insert_window_proposal(&db, "unstamped", &unstamped, "stream-a", None);
        insert_window_proposal(&db, "stranded", &stranded, "dissolved-stream", Some(7));

        // When / Then: only the question this classifier itself answered is suppressed.
        assert!(
            db.has_pending_proposal_for_events_at_generation(&current, 7)
                .unwrap()
        );
        assert!(
            !db.has_pending_proposal_for_events_at_generation(&older, 7)
                .unwrap()
        );
        // `NULL` is every row written before the column existed. It must not read as
        // "this generation", or the 212 already queued would stay frozen for good.
        assert!(
            !db.has_pending_proposal_for_events_at_generation(&unstamped, 7)
                .unwrap()
        );
        // A proposal naming a dissolved stream suppresses nothing, exactly as it does
        // for every other read: no reviewer can accept it.
        assert!(
            !db.has_pending_proposal_for_events_at_generation(&stranded, 7)
                .unwrap()
        );
    }

    #[test]
    fn stamping_records_the_answering_generation_without_touching_the_verdict() {
        // Given: a question queued before generations existed, on a session and on a
        // run. Stamping is what spends a re-ask, so a generation bump costs one pass
        // over the queue rather than every pass forever.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("stream-a", None)).unwrap();
        let run = vec!["window-a".to_string()];
        insert_window_proposal(&db, "run", &run, "stream-a", None);
        db.insert_proposal(&Proposal {
            id: "session".to_string(),
            created_at: ts(0),
            session_id: Some("session-a".to_string()),
            event_ids: None,
            proposed_stream_id: Some("stream-a".to_string()),
            proposed_new_stream: None,
            confidence: 0.5,
            reasoning: "unsure".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        })
        .unwrap();
        let version_before = db.get_db_version().unwrap();

        // When
        let runs_stamped = db.stamp_pending_proposals_for_events(&run, 7).unwrap();
        let sessions_stamped = db
            .stamp_pending_proposals_for_session("session-a", 7)
            .unwrap();

        // Then: both rows record who last answered them.
        assert_eq!(runs_stamped, 1);
        assert_eq!(sessions_stamped, 1);
        let proposals = db.get_proposals(None).unwrap();
        for proposal in &proposals {
            assert_eq!(proposal.classifier_generation, Some(7));
            // And nothing a reviewer sees has changed. Stamping is bookkeeping; it is
            // emphatically not a verdict, and above all not a rejection, which
            // `has_rejected_proposal` would read as a human silencing the classifier.
            assert_eq!(proposal.status, ProposalStatus::Pending);
            assert!((proposal.confidence - 0.5).abs() < f64::EPSILON);
            assert_eq!(proposal.reasoning, "unsure");
        }
        assert!(!db.has_rejected_proposal("session-a", "stream-a").unwrap());

        // And: it changes no assignment, so the daemon's change signal stays put.
        assert_eq!(db.get_db_version().unwrap(), version_before);
    }

    #[test]
    fn a_superseded_proposal_can_no_longer_be_accepted() {
        // Given: a proposal a later verdict already answered past.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("stream-a", None)).unwrap();
        db.insert_proposal(&Proposal {
            id: "proposal-a".to_string(),
            created_at: ts(0),
            session_id: Some("session-a".to_string()),
            event_ids: None,
            proposed_stream_id: Some("stream-a".to_string()),
            proposed_new_stream: None,
            confidence: 0.5,
            reasoning: "unsure".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        })
        .unwrap();
        db.supersede_pending_proposals_for_session("session-a")
            .unwrap();

        // When / Then: the assignment is already applied, so there is nothing to confirm.
        assert!(matches!(
            db.accept_proposal("proposal-a"),
            Err(DbError::ProposalNotPending { .. })
        ));
    }

    #[test]
    fn stream_exists_reports_only_ids_and_never_slugs_or_names() {
        // The check guards a write into `events.stream_id`, whose foreign key points at
        // `streams.id`. Resolving a slug here would silently rewrite the answer.
        let db = Database::open_in_memory().unwrap();
        let mut stream = make_stream("stream-a", None);
        stream.slug = Some("agentc-core".to_string());
        stream.name = Some("agent-c core".to_string());
        db.insert_stream(&stream).unwrap();

        assert!(db.stream_exists("stream-a").unwrap());
        assert!(!db.stream_exists("agentc-core").unwrap());
        assert!(!db.stream_exists("agent-c core").unwrap());
        assert!(!db.stream_exists("dissolved-stream").unwrap());
    }

    #[test]
    fn inherit_stream_for_session_claims_unassigned_and_previously_inherited_events() {
        // Given: a subagent holding one unassigned event, one it inherited from an
        // earlier verdict, one a human assigned, and one the classifier inferred.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("old", None)).unwrap();
        db.insert_stream(&make_stream("new", None)).unwrap();
        db.insert_stream(&make_stream("held", None)).unwrap();
        for (event_id, stream_id, source) in [
            ("unassigned", None, None),
            ("inherited", Some("old"), Some("inherited")),
            ("human", Some("held"), Some("user")),
            ("inferred", Some("held"), Some("inferred")),
        ] {
            let mut event = make_event(event_id, ts(0), EventType::AgentToolUse);
            event.session_id = Some("sub-a".to_string());
            event.stream_id = stream_id.map(String::from);
            event.assignment_source = source.map(String::from);
            db.insert_event(&event).unwrap();
        }

        // When: the parent resolves to a different stream.
        let claimed = db.inherit_stream_for_session("sub-a", "new").unwrap();

        // Then: inheritance follows the parent and never overrides a verdict of its own.
        assert_eq!(claimed, 2);
        assert_eq!(
            assignment_of(&db, "unassigned"),
            (Some("new".to_string()), Some("inherited".to_string()))
        );
        assert_eq!(
            assignment_of(&db, "inherited"),
            (Some("new".to_string()), Some("inherited".to_string()))
        );
        assert_eq!(
            assignment_of(&db, "human"),
            (Some("held".to_string()), Some("user".to_string()))
        );
        assert_eq!(
            assignment_of(&db, "inferred"),
            (Some("held".to_string()), Some("inferred".to_string()))
        );
    }

    /// Stores one `user_message` event per timestamp for `session_id`.
    fn insert_user_messages(db: &Database, session_id: &str, minutes: &[i64]) {
        let events: Vec<StoredEvent> = minutes
            .iter()
            .map(|minute| {
                let timestamp = ts(0) + chrono::Duration::minutes(*minute);
                let mut event = make_event(
                    &format!("{session_id}-user_message-{}", timestamp.timestamp_millis()),
                    timestamp,
                    tt_core::EventType::UserMessage,
                );
                event.session_id = Some(session_id.to_string());
                event
            })
            .collect();
        db.insert_events(&events).unwrap();
    }

    fn stored_user_message_minutes(db: &Database, session_id: &str) -> Vec<i64> {
        let mut minutes: Vec<i64> = db
            .get_events(None, None)
            .unwrap()
            .into_iter()
            .filter(|event| {
                event.event_type == tt_core::EventType::UserMessage
                    && event.session_id.as_deref() == Some(session_id)
            })
            .map(|event| (event.timestamp - ts(0)).num_minutes())
            .collect();
        minutes.sort_unstable();
        minutes
    }

    fn derived(session_id: &str, minutes: &[i64]) -> HashMap<String, HashSet<DateTime<Utc>>> {
        let mut map = HashMap::new();
        map.insert(
            session_id.to_string(),
            minutes
                .iter()
                .map(|minute| ts(0) + chrono::Duration::minutes(*minute))
                .collect(),
        );
        map
    }

    #[test]
    fn test_prune_user_message_events_removes_timestamps_no_longer_derived() {
        let db = Database::open_in_memory().unwrap();
        insert_user_messages(&db, "sess1", &[0, 5, 10]);

        let deleted = db
            .prune_user_message_events(&derived("sess1", &[0, 10]))
            .unwrap();

        assert_eq!(deleted, 1);
        assert_eq!(stored_user_message_minutes(&db, "sess1"), vec![0, 10]);
    }

    #[test]
    fn test_prune_user_message_events_empty_keep_set_clears_the_session() {
        // A session whose messages were all injected has no human attention.
        let db = Database::open_in_memory().unwrap();
        insert_user_messages(&db, "sess1", &[0, 5, 10]);

        let deleted = db
            .prune_user_message_events(&derived("sess1", &[]))
            .unwrap();

        assert_eq!(deleted, 3);
        assert!(stored_user_message_minutes(&db, "sess1").is_empty());
    }

    #[test]
    fn test_prune_user_message_events_leaves_sessions_it_cannot_re_derive() {
        // Events replicated from another machine must survive a prune driven by
        // this machine's transcripts, which do not contain that session.
        let db = Database::open_in_memory().unwrap();
        insert_user_messages(&db, "local", &[0, 5]);
        insert_user_messages(&db, "remote", &[0, 5]);

        let deleted = db
            .prune_user_message_events(&derived("local", &[0]))
            .unwrap();

        assert_eq!(deleted, 1);
        assert_eq!(stored_user_message_minutes(&db, "local"), vec![0]);
        assert_eq!(stored_user_message_minutes(&db, "remote"), vec![0, 5]);
    }

    #[test]
    fn test_prune_user_message_events_only_touches_user_messages() {
        let db = Database::open_in_memory().unwrap();
        insert_user_messages(&db, "sess1", &[0]);
        let mut tool_use = make_event("sess1-tool_use", ts(0), tt_core::EventType::AgentToolUse);
        tool_use.session_id = Some("sess1".to_string());
        db.insert_events(&[tool_use]).unwrap();

        db.prune_user_message_events(&derived("sess1", &[]))
            .unwrap();

        let remaining = db.get_events(None, None).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].event_type, tt_core::EventType::AgentToolUse);
    }

    #[test]
    fn test_prune_user_message_events_is_idempotent_and_repeatable() {
        // The temp tables are scratch space: a second call must not inherit the
        // first call's keep-set.
        let db = Database::open_in_memory().unwrap();
        insert_user_messages(&db, "sess1", &[0, 5]);
        insert_user_messages(&db, "sess2", &[0, 5]);

        assert_eq!(
            db.prune_user_message_events(&derived("sess1", &[0]))
                .unwrap(),
            1
        );
        assert_eq!(
            db.prune_user_message_events(&derived("sess1", &[0]))
                .unwrap(),
            0
        );
        assert_eq!(
            db.prune_user_message_events(&derived("sess2", &[5]))
                .unwrap(),
            1
        );
        assert_eq!(stored_user_message_minutes(&db, "sess1"), vec![0]);
        assert_eq!(stored_user_message_minutes(&db, "sess2"), vec![5]);
    }

    #[test]
    fn test_prune_user_message_events_bumps_db_version_only_when_rows_change() {
        let db = Database::open_in_memory().unwrap();
        insert_user_messages(&db, "sess1", &[0, 5]);

        let before = db.get_db_version().unwrap();
        db.prune_user_message_events(&derived("sess1", &[0, 5]))
            .unwrap();
        assert_eq!(db.get_db_version().unwrap(), before);

        db.prune_user_message_events(&derived("sess1", &[0]))
            .unwrap();
        assert!(db.get_db_version().unwrap() > before);
    }

    #[test]
    fn test_prune_user_message_events_no_derivations_is_a_no_op() {
        let db = Database::open_in_memory().unwrap();
        insert_user_messages(&db, "sess1", &[0, 5]);

        let deleted = db.prune_user_message_events(&HashMap::new()).unwrap();

        assert_eq!(deleted, 0);
        assert_eq!(stored_user_message_minutes(&db, "sess1"), vec![0, 5]);
    }

    #[test]
    fn unattributed_terminal_focus_events_skips_assigned_and_other_types() {
        // Given: an unassigned window_focus, an already-assigned one, and an
        // unassigned event of a different type.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("stream-a", Some("eval")))
            .unwrap();

        let mut unassigned = make_event("focus-open", ts(1), tt_core::EventType::WindowFocus);
        unassigned.cwd = None;
        unassigned.window_app_id = Some("com.mitchellh.ghostty".to_string());
        unassigned.window_title = Some("mosh devbox".to_string());
        db.insert_event(&unassigned).unwrap();

        let mut assigned = make_event("focus-taken", ts(2), tt_core::EventType::WindowFocus);
        assigned.cwd = None;
        assigned.stream_id = Some("stream-a".to_string());
        assigned.assignment_source = Some("user".to_string());
        db.insert_event(&assigned).unwrap();

        db.insert_event(&make_event(
            "pane-open",
            ts(3),
            tt_core::EventType::TmuxPaneFocus,
        ))
        .unwrap();

        // When: the terminal-focus candidates are fetched.
        let events = db.unattributed_terminal_focus_events().unwrap();

        // Then: only the unassigned window_focus row comes back.
        let ids: Vec<&str> = events.iter().map(|event| event.id.as_str()).collect();
        assert_eq!(ids, vec!["focus-open"]);
        assert_eq!(
            events[0].window_title.as_deref(),
            Some("mosh devbox"),
            "detection fields must survive the fetch"
        );
    }

    #[test]
    fn remote_activity_for_correlation_returns_classified_rows_in_time_order() {
        use tt_core::attribution::RemoteActivity;

        // Given: classified remote activity inserted out of order, plus rows that
        // must not be candidates — unclassified, out of range, and window_focus
        // (which would let this pass feed on its own output).
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("eval", Some("eval")))
            .unwrap();
        db.insert_stream(&make_stream("dpi", Some("dpi"))).unwrap();

        let mut later = make_event("tool-late", ts(3), tt_core::EventType::AgentToolUse);
        later.stream_id = Some("eval".to_string());
        db.insert_event(&later).unwrap();

        let mut earlier = make_event("pane-early", ts(1), tt_core::EventType::TmuxPaneFocus);
        earlier.stream_id = Some("dpi".to_string());
        db.insert_event(&earlier).unwrap();

        let mut middle = make_event("msg-mid", ts(2), tt_core::EventType::UserMessage);
        middle.stream_id = Some("eval".to_string());
        db.insert_event(&middle).unwrap();

        db.insert_event(&make_event(
            "pane-unclassified",
            ts(2),
            tt_core::EventType::TmuxPaneFocus,
        ))
        .unwrap();

        let mut out_of_range = make_event("pane-later", ts(9), tt_core::EventType::TmuxPaneFocus);
        out_of_range.stream_id = Some("dpi".to_string());
        db.insert_event(&out_of_range).unwrap();

        let mut window = make_event("focus-taken", ts(2), tt_core::EventType::WindowFocus);
        window.stream_id = Some("eval".to_string());
        db.insert_event(&window).unwrap();

        // When: correlation candidates are fetched for the span.
        let activity = db.remote_activity_for_correlation(ts(0), ts(4)).unwrap();

        // Then: only classified tmux/agent/user rows in range, ascending.
        assert_eq!(
            activity,
            vec![
                RemoteActivity {
                    timestamp: ts(1),
                    stream_id: "dpi".to_string(),
                },
                RemoteActivity {
                    timestamp: ts(2),
                    stream_id: "eval".to_string(),
                },
                RemoteActivity {
                    timestamp: ts(3),
                    stream_id: "eval".to_string(),
                },
            ]
        );
    }

    /// Registers a session whose text is `text`, in project `project`.
    fn insert_session_with_text(db: &Database, session_id: &str, project: &str, text: &str) {
        let session = tt_core::session::AgentSession {
            session_id: session_id.to_string(),
            source: tt_core::session::SessionSource::Claude,
            parent_session_id: None,
            session_type: tt_core::session::SessionType::User,
            project_path: format!("/home/sami/{project}"),
            project_name: project.to_string(),
            start_time: ts(0),
            end_time: Some(ts(1)),
            message_count: 1,
            summary: Some(text.to_string()),
            user_prompts: Vec::new(),
            starting_prompt: None,
            assistant_message_count: 0,
            tool_call_count: 0,
            user_message_timestamps: Vec::new(),
            tool_call_timestamps: Vec::new(),
        };
        db.upsert_agent_session(&session, None).unwrap();
    }

    /// Attaches `count` classified events on `stream_id` to `session_id`.
    fn attach_session_events(db: &Database, session_id: &str, stream_id: &str, ids: &[&str]) {
        for id in ids {
            let mut event = make_event(id, ts(1), tt_core::EventType::AgentToolUse);
            event.session_id = Some(session_id.to_string());
            event.stream_id = Some(stream_id.to_string());
            db.insert_event(&event).unwrap();
        }
    }

    #[test]
    fn artifact_mentions_carry_the_stream_of_the_work_that_referenced_them() {
        use tt_core::attribution::{ArtifactMention, ArtifactRef};

        // Given: one classified session that names a PR by URL.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("tracker", Some("tracker")))
            .unwrap();
        insert_session_with_text(
            &db,
            "s-url",
            "time-tracker",
            "shipped https://github.com/sjawhar/time-tracker/pull/46",
        );
        attach_session_events(&db, "s-url", "tracker", &["e1"]);

        // When
        let mentions = db.artifact_mentions_for_binding().unwrap();

        // Then: the URL form binds, and the bare `#46` is also recorded scoped to
        // the session's own project.
        assert!(mentions.contains(&ArtifactMention {
            artifact: ArtifactRef {
                owner: Some("sjawhar".to_string()),
                repo: "time-tracker".to_string(),
                number: "46".to_string(),
            },
            stream_id: "tracker".to_string(),
        }));
        assert!(mentions.iter().all(|m| m.stream_id == "tracker"));
    }

    #[test]
    fn a_session_split_evenly_between_streams_mentions_nothing() {
        // Given: a session whose events are tied between two streams, so it cannot
        // say which stream owns the artifact it names.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("a", Some("a"))).unwrap();
        db.insert_stream(&make_stream("b", Some("b"))).unwrap();
        insert_session_with_text(
            &db,
            "s-tied",
            "time-tracker",
            "see https://github.com/sjawhar/time-tracker/pull/46",
        );
        attach_session_events(&db, "s-tied", "a", &["e-a"]);
        attach_session_events(&db, "s-tied", "b", &["e-b"]);

        // When / Then
        assert!(db.artifact_mentions_for_binding().unwrap().is_empty());
    }

    #[test]
    fn an_unclassified_session_mentions_nothing() {
        // Given: a session that names a PR but whose events have no stream.
        let db = Database::open_in_memory().unwrap();
        insert_session_with_text(
            &db,
            "s-open",
            "time-tracker",
            "see https://github.com/sjawhar/time-tracker/pull/46",
        );
        db.insert_event(&make_event(
            "e-open",
            ts(1),
            tt_core::EventType::AgentToolUse,
        ))
        .unwrap();

        // When / Then
        assert!(db.artifact_mentions_for_binding().unwrap().is_empty());
    }

    /// Builds a stream holding one event per supplied `assignment_source`.
    ///
    /// Event ids are `<stream_id>-<source>`; a `None` source stores SQL NULL.
    fn db_with_assigned_stream(stream_id: &str, sources: &[Option<&str>]) -> Database {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream(stream_id, Some("misc: stragglers")))
            .unwrap();
        for (hour, source) in (0i64..).zip(sources) {
            let mut event = make_event(
                &format!("{stream_id}-{}", source.unwrap_or("null")),
                ts(hour),
                tt_core::EventType::WindowFocus,
            );
            event.stream_id = Some(stream_id.to_string());
            event.assignment_source = source.map(String::from);
            db.insert_event(&event).unwrap();
        }
        db
    }

    /// Reads an event's `(stream_id, assignment_source)` straight from SQL.
    fn assignment_of(db: &Database, event_id: &str) -> (Option<String>, Option<String>) {
        db.conn
            .query_row(
                "SELECT stream_id, assignment_source FROM events WHERE id = ?1",
                params![event_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    }

    #[test]
    fn dissolve_stream_releases_inferred_events_and_retires_the_stream() {
        // Given: a stream holding only machine-assigned events.
        let db = db_with_assigned_stream(
            "catch-all",
            &[Some("inferred"), Some("terminal_focus"), None],
        );
        let version_before = db.get_db_version().unwrap();

        // When: the stream is dissolved.
        let outcome = db
            .dissolve_stream("catch-all", DissolveMode::Apply)
            .unwrap();

        // Then: every event is unassigned, the stream is gone, db_version advanced.
        assert_eq!(
            outcome,
            DissolveOutcome {
                released: 3,
                retained: 0,
                retired: true,
            }
        );
        for event_id in [
            "catch-all-inferred",
            "catch-all-terminal_focus",
            "catch-all-null",
        ] {
            assert_eq!(assignment_of(&db, event_id), (None, None));
        }
        assert!(db.get_stream("catch-all").unwrap().is_none());
        assert_eq!(db.get_db_version().unwrap(), version_before + 1);
    }

    #[test]
    fn dissolve_stream_never_touches_user_assignments_and_keeps_the_stream() {
        // Given: a stream holding one human-assigned event alongside inferred ones.
        let db = db_with_assigned_stream("mixed", &[Some("inferred"), Some("user")]);

        // When: the stream is dissolved.
        let outcome = db.dissolve_stream("mixed", DissolveMode::Apply).unwrap();

        // Then: the human assignment survives and the stream stays.
        assert_eq!(
            outcome,
            DissolveOutcome {
                released: 1,
                retained: 1,
                retired: false,
            }
        );
        assert_eq!(
            assignment_of(&db, "mixed-user"),
            (Some("mixed".to_string()), Some("user".to_string()))
        );
        assert_eq!(assignment_of(&db, "mixed-inferred"), (None, None));
        assert!(db.get_stream("mixed").unwrap().is_some());
    }

    #[test]
    fn dissolve_stream_leaves_db_version_alone_when_nothing_changes() {
        // Given: a stream whose every event was assigned by a human.
        let db = db_with_assigned_stream("human-only", &[Some("user")]);
        let version_before = db.get_db_version().unwrap();

        // When: the stream is dissolved.
        let outcome = db
            .dissolve_stream("human-only", DissolveMode::Apply)
            .unwrap();

        // Then: nothing moved, so the daemon's change signal stays put.
        assert_eq!(
            outcome,
            DissolveOutcome {
                released: 0,
                retained: 1,
                retired: false,
            }
        );
        assert_eq!(db.get_db_version().unwrap(), version_before);
    }

    #[test]
    fn dissolve_stream_dry_run_reports_the_same_counts_without_writing() {
        // Given: a stream that a real dissolution would partly release.
        let db = db_with_assigned_stream("preview", &[Some("inferred"), Some("user")]);
        let version_before = db.get_db_version().unwrap();

        // When: the stream is dissolved in dry-run mode.
        let preview = db.dissolve_stream("preview", DissolveMode::DryRun).unwrap();

        // Then: the counts match the real run, but nothing was written.
        assert_eq!(
            preview,
            DissolveOutcome {
                released: 1,
                retained: 1,
                retired: false,
            }
        );
        assert_eq!(
            assignment_of(&db, "preview-inferred"),
            (Some("preview".to_string()), Some("inferred".to_string()))
        );
        assert!(db.get_stream("preview").unwrap().is_some());
        assert_eq!(db.get_db_version().unwrap(), version_before);
        assert_eq!(
            db.dissolve_stream("preview", DissolveMode::Apply).unwrap(),
            preview
        );
    }

    #[test]
    fn dissolve_stream_dry_run_of_a_retiring_stream_keeps_it() {
        // Given: a stream a real dissolution would retire.
        let db = db_with_assigned_stream("doomed", &[Some("inferred")]);
        let version_before = db.get_db_version().unwrap();

        // When: the stream is dissolved in dry-run mode.
        let preview = db.dissolve_stream("doomed", DissolveMode::DryRun).unwrap();

        // Then: retirement is predicted but the stream row survives.
        assert!(preview.retired);
        assert!(db.get_stream("doomed").unwrap().is_some());
        assert_eq!(db.get_db_version().unwrap(), version_before);
    }

    #[test]
    fn dissolve_stream_retires_a_stream_with_no_events() {
        // Given: a stream nothing points at.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("empty", Some("misc (Jun14-20)")))
            .unwrap();
        db.add_tag("empty", "stale").unwrap();
        let version_before = db.get_db_version().unwrap();

        // When: the stream is dissolved.
        let outcome = db.dissolve_stream("empty", DissolveMode::Apply).unwrap();

        // Then: it is retired, tags cascade away, db_version advances.
        assert_eq!(
            outcome,
            DissolveOutcome {
                released: 0,
                retained: 0,
                retired: true,
            }
        );
        assert!(db.get_stream("empty").unwrap().is_none());
        assert!(db.get_tags("empty").unwrap().is_empty());
        assert_eq!(db.get_db_version().unwrap(), version_before + 1);
    }

    #[test]
    fn dissolve_stream_leaves_other_streams_untouched() {
        // Given: two streams, each holding an inferred event.
        let db = db_with_assigned_stream("target", &[Some("inferred")]);
        db.insert_stream(&make_stream("bystander", Some("eval-3 moto")))
            .unwrap();
        let mut event = make_event("bystander-event", ts(9), tt_core::EventType::WindowFocus);
        event.stream_id = Some("bystander".to_string());
        event.assignment_source = Some("inferred".to_string());
        db.insert_event(&event).unwrap();

        // When: only the target is dissolved.
        db.dissolve_stream("target", DissolveMode::Apply).unwrap();

        // Then: the bystander keeps its stream and its assignment.
        assert_eq!(
            assignment_of(&db, "bystander-event"),
            (Some("bystander".to_string()), Some("inferred".to_string()))
        );
        assert!(db.get_stream("bystander").unwrap().is_some());
    }

    /// The stream every fixture event below is assigned to.
    const CONTAINER: &str = "cwd-container";

    /// Builds a database holding one container stream and no events.
    fn db_with_container() -> Database {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream(CONTAINER, Some("agent-c: tooling")))
            .unwrap();
        db
    }

    /// Assigns one event to the container, carrying the attribution state supplied.
    ///
    /// A `window_focus` gets a title, because that is the evidence the window-run
    /// classifier reads and the reason such an event is legitimately classifiable.
    fn insert_attributed(
        db: &Database,
        id: &str,
        event_type: tt_core::EventType,
        session_id: Option<&str>,
        source: Option<&str>,
    ) {
        let mut event = make_event(id, ts(0), event_type);
        event.session_id = session_id.map(String::from);
        event.stream_id = Some(CONTAINER.to_string());
        event.assignment_source = source.map(String::from);
        if matches!(event_type, tt_core::EventType::WindowFocus) {
            event.window_title = Some("PR #47 · sjawhar/time-tracker".to_string());
        }
        db.insert_event(&event).unwrap();
    }

    #[test]
    fn release_unattributable_pane_focus_releases_a_sessionless_inferred_pane() {
        // Given: a tmux pane focus holding a stream, with no session to have earned it.
        let db = db_with_container();
        insert_attributed(
            &db,
            "pane-inferred",
            tt_core::EventType::TmuxPaneFocus,
            None,
            Some("inferred"),
        );

        // When: the unattributable pane focus is released.
        let outcome = db
            .release_unattributable_pane_focus(ReleaseMode::Apply)
            .unwrap();

        // Then: the event is indistinguishable from one that was never classified.
        assert_eq!(
            outcome,
            ReleaseOutcome {
                released: 1,
                retained: 0,
                streams_affected: 1,
            }
        );
        assert_eq!(assignment_of(&db, "pane-inferred"), (None, None));
    }

    #[test]
    fn release_unattributable_pane_focus_leaves_a_titled_window_focus_alone() {
        // Given: a window focus carrying the title the window-run classifier reads.
        let db = db_with_container();
        insert_attributed(
            &db,
            "window-inferred",
            tt_core::EventType::WindowFocus,
            None,
            Some("inferred"),
        );

        // When: the unattributable pane focus is released.
        let outcome = db
            .release_unattributable_pane_focus(ReleaseMode::Apply)
            .unwrap();

        // Then: it is untouched — a title is evidence, so this event is classifiable.
        assert_eq!(
            outcome,
            ReleaseOutcome {
                released: 0,
                retained: 0,
                streams_affected: 0,
            }
        );
        assert_eq!(
            assignment_of(&db, "window-inferred"),
            (Some(CONTAINER.to_string()), Some("inferred".to_string()))
        );
    }

    #[test]
    fn release_unattributable_pane_focus_leaves_a_pane_carrying_a_session_alone() {
        // Given: a pane focus the process-tree stamp gave a session id, then classified.
        let db = db_with_container();
        insert_attributed(
            &db,
            "pane-sessioned",
            tt_core::EventType::TmuxPaneFocus,
            Some("ses_07f7d2d65f"),
            Some("inferred"),
        );

        // When: the unattributable pane focus is released.
        let outcome = db
            .release_unattributable_pane_focus(ReleaseMode::Apply)
            .unwrap();

        // Then: it survives — the session→stream path legitimately placed it.
        assert_eq!(
            outcome,
            ReleaseOutcome {
                released: 0,
                retained: 0,
                streams_affected: 0,
            }
        );
        assert_eq!(
            assignment_of(&db, "pane-sessioned"),
            (Some(CONTAINER.to_string()), Some("inferred".to_string()))
        );
    }

    #[test]
    fn release_unattributable_pane_focus_never_releases_a_human_assignment() {
        // Given: a sessionless pane focus a human assigned by hand.
        let db = db_with_container();
        insert_attributed(
            &db,
            "pane-user",
            tt_core::EventType::TmuxPaneFocus,
            None,
            Some("user"),
        );

        // When: the unattributable pane focus is released.
        let outcome = db
            .release_unattributable_pane_focus(ReleaseMode::Apply)
            .unwrap();

        // Then: the human's verdict stands, and is reported as retained.
        assert_eq!(
            outcome,
            ReleaseOutcome {
                released: 0,
                retained: 1,
                streams_affected: 0,
            }
        );
        assert_eq!(
            assignment_of(&db, "pane-user"),
            (Some(CONTAINER.to_string()), Some("user".to_string()))
        );
    }

    /// Inserts a sessionless pane focus holding no stream, with the source supplied.
    ///
    /// Covers the two shapes a stream-keyed rule cannot see: a row a half-cleanup left
    /// carrying only its provenance, and one that is already clean.
    fn insert_unassigned_pane(db: &Database, id: &str, source: Option<&str>) {
        let mut event = make_event(id, ts(1), tt_core::EventType::TmuxPaneFocus);
        event.assignment_source = source.map(String::from);
        db.insert_event(&event).unwrap();
    }

    /// Builds one event of every shape the release has an opinion about.
    fn db_with_mixed_focus() -> Database {
        let db = db_with_container();
        for (id, event_type, session_id, source) in [
            (
                "pane-inferred",
                tt_core::EventType::TmuxPaneFocus,
                None,
                Some("inferred"),
            ),
            (
                "pane-sessioned",
                tt_core::EventType::TmuxPaneFocus,
                Some("ses_07f7d2d65f"),
                Some("inferred"),
            ),
            (
                "pane-user",
                tt_core::EventType::TmuxPaneFocus,
                None,
                Some("user"),
            ),
            (
                "window-inferred",
                tt_core::EventType::WindowFocus,
                None,
                Some("inferred"),
            ),
        ] {
            insert_attributed(&db, id, event_type, session_id, source);
        }
        insert_unassigned_pane(&db, "pane-stale-source", Some("inferred"));
        insert_unassigned_pane(&db, "pane-clean", None);
        db
    }

    /// Counts every event row, whatever its attribution.
    fn event_count(db: &Database) -> u64 {
        db.conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn release_unattributable_pane_focus_deletes_no_event_row() {
        // Given: one event of every shape the release has an opinion about.
        let db = db_with_mixed_focus();
        let before = event_count(&db);

        // When: the release is applied.
        let outcome = db
            .release_unattributable_pane_focus(ReleaseMode::Apply)
            .unwrap();

        // Then: attribution moved and every row is still there — release, never delete.
        assert_eq!(before, 6);
        assert_eq!(outcome.released, 2);
        assert_eq!(event_count(&db), before);
    }

    #[test]
    fn release_unattributable_pane_focus_clears_a_stale_source_left_by_a_half_cleanup() {
        // Given: the live shape a stream-keyed cleanup leaves — provenance, no stream.
        let db = db_with_container();
        insert_unassigned_pane(&db, "pane-stale-source", Some("inferred"));

        // When: the release is applied.
        let outcome = db
            .release_unattributable_pane_focus(ReleaseMode::Apply)
            .unwrap();

        // Then: the claim of provenance goes too, and it counts as no stream's loss.
        assert_eq!(
            outcome,
            ReleaseOutcome {
                released: 1,
                retained: 0,
                streams_affected: 0,
            }
        );
        assert_eq!(assignment_of(&db, "pane-stale-source"), (None, None));
    }

    #[test]
    fn release_unattributable_pane_focus_dry_run_reports_the_same_counts_without_writing() {
        // Given: a database holding two releasable panes and one human assignment.
        let db = db_with_mixed_focus();
        let version_before = db.get_db_version().unwrap();

        // When: the release is previewed.
        let preview = db
            .release_unattributable_pane_focus(ReleaseMode::DryRun)
            .unwrap();

        // Then: nothing moved and the daemon was not signalled.
        assert_eq!(
            preview,
            ReleaseOutcome {
                released: 2,
                retained: 1,
                streams_affected: 1,
            }
        );
        assert_eq!(
            assignment_of(&db, "pane-inferred"),
            (Some(CONTAINER.to_string()), Some("inferred".to_string()))
        );
        assert_eq!(db.get_db_version().unwrap(), version_before);

        // And: a real run produces exactly what the preview promised.
        assert_eq!(
            db.release_unattributable_pane_focus(ReleaseMode::Apply)
                .unwrap(),
            preview
        );
    }

    #[test]
    fn release_unattributable_pane_focus_bumps_db_version_only_when_rows_change() {
        // Given: a database whose every pane is legitimately attributed or already clean.
        let db = db_with_container();
        insert_attributed(
            &db,
            "pane-sessioned",
            tt_core::EventType::TmuxPaneFocus,
            Some("ses_07f7d2d65f"),
            Some("inferred"),
        );
        insert_unassigned_pane(&db, "pane-clean", None);
        let version_before = db.get_db_version().unwrap();

        // When: a release finds nothing to do.
        let idle = db
            .release_unattributable_pane_focus(ReleaseMode::Apply)
            .unwrap();

        // Then: the daemon's change signal stays put.
        assert_eq!(idle.released, 0);
        assert_eq!(db.get_db_version().unwrap(), version_before);

        // And when: a releasable pane arrives, the signal advances exactly once.
        insert_attributed(
            &db,
            "pane-inferred",
            tt_core::EventType::TmuxPaneFocus,
            None,
            Some("inferred"),
        );
        let version_after_insert = db.get_db_version().unwrap();
        let released = db
            .release_unattributable_pane_focus(ReleaseMode::Apply)
            .unwrap();
        assert_eq!(released.released, 1);
        assert_eq!(db.get_db_version().unwrap(), version_after_insert + 1);
    }

    #[test]
    fn release_unattributable_pane_focus_finds_nothing_on_a_second_run() {
        // Given: a database whose unattributable panes have already been released.
        let db = db_with_mixed_focus();
        db.release_unattributable_pane_focus(ReleaseMode::Apply)
            .unwrap();

        // Then: a released event looks like one never classified, so nothing is left.
        assert_eq!(
            db.release_unattributable_pane_focus(ReleaseMode::Apply)
                .unwrap(),
            ReleaseOutcome {
                released: 0,
                retained: 1,
                streams_affected: 0,
            }
        );
    }

    /// Builds two streams: `into` with `into_sources`, `from` with `from_sources`.
    ///
    /// Event ids are `<stream_id>-<source>`, matching `db_with_assigned_stream`.
    fn db_for_merge(from_sources: &[Option<&str>], into_sources: &[Option<&str>]) -> Database {
        let db = Database::open_in_memory().unwrap();
        for (stream_id, sources) in [("from", from_sources), ("into", into_sources)] {
            db.insert_stream(&make_stream(stream_id, Some(stream_id)))
                .unwrap();
            for (hour, source) in (0i64..).zip(sources) {
                let mut event = make_event(
                    &format!("{stream_id}-{}", source.unwrap_or("null")),
                    ts(hour),
                    tt_core::EventType::WindowFocus,
                );
                event.stream_id = Some(stream_id.to_string());
                event.assignment_source = source.map(String::from);
                db.insert_event(&event).unwrap();
            }
        }
        db
    }

    fn refs(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_string()).collect()
    }

    #[test]
    fn merge_streams_moves_events_and_tags_then_deletes_the_source() {
        // Given: a source stream holding machine-assigned events and two tags, one
        // of which the target already carries.
        let db = db_for_merge(&[Some("inferred"), Some("terminal_focus")], &[None]);
        db.add_tag("from", "work").unwrap();
        db.add_tag("from", "infosec").unwrap();
        db.add_tag("into", "work").unwrap();
        let version_before = db.get_db_version().unwrap();

        // When: the source is merged into the target.
        let merged = db
            .merge_streams(&refs(&["from"]), "into", MergeMode::Apply)
            .unwrap();

        // Then: every event re-points, only the unheld tag lands, the source is gone.
        assert_eq!(
            merged,
            vec![MergedSource {
                stream_id: "from".to_string(),
                events_moved: 2,
                user_events_moved: 0,
                tags_moved: 1,
                proposals_repointed: 0,
                retired: true,
            }]
        );
        assert_eq!(db.get_events_by_stream("into").unwrap().len(), 3);
        assert!(db.get_events_by_stream("from").unwrap().is_empty());
        assert_eq!(db.get_tags("into").unwrap(), vec!["infosec", "work"]);
        assert!(db.get_stream("from").unwrap().is_none());
        assert!(db.get_stream("into").unwrap().is_some());
        assert_eq!(db.get_db_version().unwrap(), version_before + 1);
    }

    #[test]
    fn merge_streams_carries_human_assignments_across_unchanged() {
        // Given: a source stream a human partly classified by hand. A merge corrects
        // which row holds the work, never the human's judgement about what it was.
        let db = db_for_merge(&[Some("user"), Some("inferred")], &[]);

        // When: the source is merged into the target.
        let merged = db
            .merge_streams(&refs(&["from"]), "into", MergeMode::Apply)
            .unwrap();

        // Then: the human-assigned event moved with its source label intact.
        assert_eq!(merged[0].events_moved, 2);
        assert_eq!(merged[0].user_events_moved, 1);
        assert_eq!(
            assignment_of(&db, "from-user"),
            (Some("into".to_string()), Some("user".to_string()))
        );
        assert!(db.get_stream("from").unwrap().is_none());
    }

    #[test]
    fn merge_streams_never_deletes_an_event() {
        // Given: a source and a target, each holding events.
        let db = db_for_merge(&[Some("inferred"), Some("user")], &[Some("inferred")]);
        let before: u64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();

        // When: the source is merged into the target.
        db.merge_streams(&refs(&["from"]), "into", MergeMode::Apply)
            .unwrap();

        // Then: the event population is unchanged — a merge re-points, never deletes.
        let after: u64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(after, before);
        assert_eq!(after, 3);
    }

    #[test]
    fn merge_streams_dry_run_reports_the_same_counts_without_writing() {
        // Given: a source stream a real merge would empty and retire.
        let db = db_for_merge(&[Some("inferred")], &[]);
        db.add_tag("from", "work").unwrap();
        let version_before = db.get_db_version().unwrap();

        // When: the merge runs in dry-run mode.
        let preview = db
            .merge_streams(&refs(&["from"]), "into", MergeMode::DryRun)
            .unwrap();

        // Then: nothing moved, and a real run reports exactly the same counts.
        assert_eq!(
            preview,
            vec![MergedSource {
                stream_id: "from".to_string(),
                events_moved: 1,
                user_events_moved: 0,
                tags_moved: 1,
                proposals_repointed: 0,
                retired: true,
            }]
        );
        assert_eq!(
            assignment_of(&db, "from-inferred"),
            (Some("from".to_string()), Some("inferred".to_string()))
        );
        assert!(db.get_stream("from").unwrap().is_some());
        assert!(db.get_tags("into").unwrap().is_empty());
        assert_eq!(db.get_db_version().unwrap(), version_before);
        assert_eq!(
            db.merge_streams(&refs(&["from"]), "into", MergeMode::Apply)
                .unwrap(),
            preview
        );
    }

    #[test]
    fn merge_streams_refuses_to_merge_a_stream_into_itself() {
        // Given: a stream named as both source and target.
        let db = db_for_merge(&[Some("inferred")], &[]);

        // When: it is merged into itself.
        let error = db
            .merge_streams(&refs(&["into", "from"]), "from", MergeMode::Apply)
            .unwrap_err();

        // Then: the whole call is refused before any write.
        assert!(matches!(
            error,
            DbError::MergeIntoSelf { ref stream_id } if stream_id == "from"
        ));
        assert!(db.get_stream("into").unwrap().is_some());
        assert_eq!(db.get_events_by_stream("from").unwrap().len(), 1);
    }

    #[test]
    fn merge_streams_refuses_a_target_that_does_not_exist() {
        // Given: a target reference naming nothing — the roster a caller read can be
        // stale, and `events.stream_id` is a foreign key.
        let db = db_for_merge(&[Some("inferred")], &[]);

        // When: the source is merged into it.
        let error = db
            .merge_streams(&refs(&["from"]), "gone", MergeMode::Apply)
            .unwrap_err();

        // Then: it is refused by name rather than by foreign-key failure.
        assert!(matches!(
            error,
            DbError::MergeTargetNotFound { ref stream_id } if stream_id == "gone"
        ));
        assert_eq!(db.get_events_by_stream("from").unwrap().len(), 1);
    }

    #[test]
    fn merge_streams_leaves_db_version_alone_when_nothing_changes() {
        // Given: an empty, untagged source stream — a merge has nothing to move, but
        // retiring the row is still a change.
        let db = db_for_merge(&[], &[]);
        db.merge_streams(&refs(&["from"]), "into", MergeMode::Apply)
            .unwrap();
        let version_before = db.get_db_version().unwrap();

        // When: the same source is merged again, now that it is gone.
        let merged = db
            .merge_streams(&refs(&["from"]), "into", MergeMode::Apply)
            .unwrap();

        // Then: nothing moved, so the daemon's change signal stays put.
        assert_eq!(
            merged,
            vec![MergedSource {
                stream_id: "from".to_string(),
                events_moved: 0,
                user_events_moved: 0,
                tags_moved: 0,
                proposals_repointed: 0,
                retired: false,
            }]
        );
        assert_eq!(db.get_db_version().unwrap(), version_before);
    }

    #[test]
    fn merge_streams_marks_the_target_for_recompute() {
        // Given: a target whose materialized times were current.
        let db = db_for_merge(&[Some("inferred")], &[]);

        // When: a source's events are merged into it.
        db.merge_streams(&refs(&["from"]), "into", MergeMode::Apply)
            .unwrap();

        // Then: the target's totals are stale and say so — only `tt recompute` writes them.
        assert!(db.get_stream("into").unwrap().unwrap().needs_recompute);
    }

    #[test]
    fn merge_streams_merges_every_source_in_one_transaction() {
        // Given: two week-bucketed sources for one initiative.
        let db = db_for_merge(&[Some("inferred")], &[]);
        db.insert_stream(&make_stream("from2", Some("from2")))
            .unwrap();
        let mut event = make_event("from2-inferred", ts(7), tt_core::EventType::WindowFocus);
        event.stream_id = Some("from2".to_string());
        event.assignment_source = Some("inferred".to_string());
        db.insert_event(&event).unwrap();

        // When: both are merged into the target at once.
        let merged = db
            .merge_streams(&refs(&["from", "from2"]), "into", MergeMode::Apply)
            .unwrap();

        // Then: each source reports its own move and both are retired.
        assert_eq!(merged.len(), 2);
        assert!(merged.iter().all(|source| source.retired));
        assert_eq!(db.get_events_by_stream("into").unwrap().len(), 2);
    }

    #[test]
    fn rename_stream_sets_the_name_and_bumps_db_version() {
        // Given: a stream whose name carries a week suffix.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream(
            "s1",
            Some("workorder-5: IPI envs + wo-005 (Jun14-20)"),
        ))
        .unwrap();
        let version_before = db.get_db_version().unwrap();

        // When: the suffix is stripped.
        db.rename_stream("s1", "workorder-5: IPI envs + wo-005")
            .unwrap();

        // Then: the new name is stored and the daemon's change signal advances.
        assert_eq!(
            db.get_stream("s1").unwrap().unwrap().name.as_deref(),
            Some("workorder-5: IPI envs + wo-005")
        );
        assert_eq!(db.get_db_version().unwrap(), version_before + 1);
    }

    #[test]
    fn rename_stream_leaves_db_version_alone_when_no_row_matches() {
        // Given: a reference naming no stream.
        let db = Database::open_in_memory().unwrap();
        let version_before = db.get_db_version().unwrap();

        // When: a rename is attempted.
        db.rename_stream("gone", "anything").unwrap();

        // Then: nothing changed, so the change signal stays put.
        assert_eq!(db.get_db_version().unwrap(), version_before);
    }

    #[test]
    fn count_events_by_stream_counts_every_assignment_source() {
        // Given: a stream holding events assigned by machine and by hand.
        let db = db_with_assigned_stream("mixed", &[Some("inferred"), Some("user"), None]);

        // When/Then: all of them are counted, and an empty stream counts zero.
        assert_eq!(db.count_events_by_stream("mixed").unwrap(), 3);
        assert_eq!(db.count_events_by_stream("gone").unwrap(), 0);
    }

    /// Every column of one proposal row, as stored.
    ///
    /// Read as `Value` so the comparison covers the row exactly as `SQLite` holds it,
    /// rather than as whatever the parser makes of it.
    fn proposal_row(db: &Database, proposal_id: &str) -> Vec<rusqlite::types::Value> {
        db.conn
            .query_row(
                "SELECT * FROM proposals WHERE id = ?1",
                params![proposal_id],
                |row| {
                    (0..row.as_ref().column_count())
                        .map(|index| row.get(index))
                        .collect()
                },
            )
            .unwrap()
    }

    /// Files one pending proposal naming `stream_id` over `event_ids`.
    fn insert_stream_proposal(db: &Database, proposal_id: &str, stream_id: &str, events: usize) {
        db.insert_proposal(&Proposal {
            id: proposal_id.to_string(),
            created_at: ts(0),
            session_id: None,
            event_ids: Some(
                (0..events)
                    .map(|index| format!("{proposal_id}-e{index}"))
                    .collect(),
            ),
            proposed_stream_id: Some(stream_id.to_string()),
            proposed_new_stream: None,
            confidence: 0.6,
            reasoning: "unsure".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        })
        .unwrap();
    }

    #[test]
    fn pending_proposals_for_streams_names_the_rows_a_dissolution_would_strand() {
        // Given: pending proposals on two streams, one already decided, and one scoped
        // to a session rather than an event set.
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("doomed", Some("misc: stragglers")))
            .unwrap();
        db.insert_stream(&make_stream("kept", Some("time-tracker: tooling")))
            .unwrap();
        insert_stream_proposal(&db, "pending-doomed", "doomed", 3);
        insert_stream_proposal(&db, "pending-kept", "kept", 1);
        insert_stream_proposal(&db, "decided-doomed", "doomed", 2);
        db.set_proposal_status("decided-doomed", ProposalStatus::Rejected)
            .unwrap();
        db.insert_proposal(&Proposal {
            id: "session-doomed".to_string(),
            created_at: ts(1),
            session_id: Some("session-a".to_string()),
            event_ids: None,
            proposed_stream_id: Some("doomed".to_string()),
            proposed_new_stream: None,
            confidence: 0.6,
            reasoning: "unsure".to_string(),
            status: ProposalStatus::Pending,
            classifier_generation: None,
        })
        .unwrap();

        // When: the doomed stream is asked about.
        let stranded = db
            .pending_proposals_for_streams(&refs(&["doomed"]))
            .unwrap();

        // Then: only its pending rows come back, a session-scoped one counting no events
        // because its target is the session. A stream nobody named strands nothing, and
        // neither does an empty request.
        assert_eq!(
            stranded,
            vec![
                StrandedProposal {
                    proposal_id: "pending-doomed".to_string(),
                    stream_id: "doomed".to_string(),
                    event_count: 3,
                },
                StrandedProposal {
                    proposal_id: "session-doomed".to_string(),
                    stream_id: "doomed".to_string(),
                    event_count: 0,
                },
            ]
        );
        assert!(
            db.pending_proposals_for_streams(&refs(&["gone"]))
                .unwrap()
                .is_empty()
        );
        assert!(db.pending_proposals_for_streams(&[]).unwrap().is_empty());
    }

    #[test]
    fn dissolve_stream_leaves_a_pending_proposal_exactly_as_it_was() {
        // Given: a stream about to be dissolved, with a pending proposal naming it.
        // Dissolution asserts the work never happened, so there is no stream to
        // re-point the proposal to and no verdict of the human's to rewrite.
        let db = db_with_assigned_stream("catch-all", &[Some("inferred")]);
        insert_stream_proposal(&db, "queued", "catch-all", 2);
        let before = proposal_row(&db, "queued");

        // When: the stream is dissolved for real.
        db.dissolve_stream("catch-all", DissolveMode::Apply)
            .unwrap();

        // Then: the stream is gone and the proposal row is untouched, column for column.
        assert!(db.get_stream("catch-all").unwrap().is_none());
        assert_eq!(proposal_row(&db, "queued"), before);
    }

    #[test]
    fn merge_streams_repoints_a_pending_proposal_at_the_target() {
        // Given: a source carrying a pending proposal. A merge says the work belongs on
        // another row, so the question waiting on a human follows it there — accepting
        // then merging and merging then accepting must land the same events.
        let db = db_for_merge(&[Some("inferred")], &[]);
        insert_stream_proposal(&db, "queued", "from", 2);
        let version_before = db.get_db_version().unwrap();

        // When: the source is merged into the target.
        let merged = db
            .merge_streams(&refs(&["from"]), "into", MergeMode::Apply)
            .unwrap();

        // Then: the proposal names the target, still pending, and the daemon is signalled
        // once for the whole merge.
        assert_eq!(merged[0].proposals_repointed, 1);
        assert_eq!(
            db.pending_proposals_for_streams(&refs(&["into"])).unwrap(),
            vec![StrandedProposal {
                proposal_id: "queued".to_string(),
                stream_id: "into".to_string(),
                event_count: 2,
            }]
        );
        assert_eq!(proposal_status(&db, "queued"), ProposalStatus::Pending);
        assert_eq!(db.get_db_version().unwrap(), version_before + 1);
    }

    #[test]
    fn merge_streams_never_rewrites_a_proposal_a_human_decided() {
        // Given: one source carrying a proposal in each settled state. Those are
        // historical records, and `has_rejected_proposal` reads the rejected ones to
        // suppress future answers.
        let db = db_for_merge(&[Some("inferred")], &[]);
        for (proposal_id, status) in [
            ("was-accepted", ProposalStatus::Accepted),
            ("was-rejected", ProposalStatus::Rejected),
            ("was-superseded", ProposalStatus::Superseded),
        ] {
            insert_stream_proposal(&db, proposal_id, "from", 1);
            db.set_proposal_status(proposal_id, status).unwrap();
        }
        let before: Vec<_> = ["was-accepted", "was-rejected", "was-superseded"]
            .into_iter()
            .map(|proposal_id| proposal_row(&db, proposal_id))
            .collect();

        // When: the source is merged into the target.
        let merged = db
            .merge_streams(&refs(&["from"]), "into", MergeMode::Apply)
            .unwrap();

        // Then: none of them moved, and none of them changed at all.
        assert_eq!(merged[0].proposals_repointed, 0);
        for (proposal_id, row) in ["was-accepted", "was-rejected", "was-superseded"]
            .into_iter()
            .zip(before)
        {
            assert_eq!(proposal_row(&db, proposal_id), row);
        }
    }

    #[test]
    fn merge_streams_dry_run_counts_repointed_proposals_without_writing() {
        // Given: a source carrying a pending proposal a real merge would move.
        let db = db_for_merge(&[Some("inferred")], &[]);
        insert_stream_proposal(&db, "queued", "from", 1);
        let before = proposal_row(&db, "queued");
        let version_before = db.get_db_version().unwrap();

        // When: the merge runs in dry-run mode.
        let preview = db
            .merge_streams(&refs(&["from"]), "into", MergeMode::DryRun)
            .unwrap();

        // Then: the move is counted, the row is untouched, and a real run reports the
        // same count — the proposal rolls back with the events and the tags, in the one
        // transaction.
        assert_eq!(preview[0].proposals_repointed, 1);
        assert_eq!(proposal_row(&db, "queued"), before);
        assert_eq!(db.get_db_version().unwrap(), version_before);
        assert_eq!(
            db.merge_streams(&refs(&["from"]), "into", MergeMode::Apply)
                .unwrap(),
            preview
        );
    }

    #[test]
    fn merge_streams_repoints_every_source_in_the_one_transaction() {
        // Given: two week buckets of one initiative, each carrying a pending proposal.
        let db = db_for_merge(&[Some("inferred")], &[]);
        db.insert_stream(&make_stream("from2", Some("from2")))
            .unwrap();
        insert_stream_proposal(&db, "queued-1", "from", 1);
        insert_stream_proposal(&db, "queued-2", "from2", 1);
        let version_before = db.get_db_version().unwrap();

        // When: both are merged into the target at once.
        let merged = db
            .merge_streams(&refs(&["from", "from2"]), "into", MergeMode::Apply)
            .unwrap();

        // Then: both proposals moved, and `db_version` advanced exactly once — a second
        // transaction would have bumped it twice.
        assert!(merged.iter().all(|source| source.proposals_repointed == 1));
        assert_eq!(
            db.pending_proposals_for_streams(&refs(&["from", "from2"]))
                .unwrap(),
            vec![]
        );
        assert_eq!(
            db.pending_proposals_for_streams(&refs(&["into"]))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(db.get_db_version().unwrap(), version_before + 1);
    }

    /// A deferred transaction that reads before it writes cannot survive a concurrent
    /// writer, and `busy_timeout` does not save it.
    ///
    /// This needs a **file** database with two connections: `open_in_memory` gives each
    /// connection a private database, so two of them can never contend and the failure
    /// this pins cannot occur at all.
    #[test]
    fn a_read_then_write_transaction_survives_a_concurrent_writer() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("tt.db");
        let db = Database::open(&path).unwrap();
        db.conn
            .execute("INSERT INTO meta (key, value) VALUES ('probe', '0')", [])
            .unwrap();

        // A second connection, as the daemon's classify loop is to its ingest loop.
        let other = Connection::open(&path).unwrap();
        other.busy_timeout(Duration::from_secs(30)).unwrap();

        // The shape that failed: DEFERRED takes no lock, so the SELECT below fixes a read
        // snapshot and the UPDATE has to *promote* to a writer afterwards.
        let deferred = db.conn.unchecked_transaction().unwrap();
        let _: i64 = deferred
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        other
            .execute("UPDATE meta SET value = '1' WHERE key = 'probe'", [])
            .unwrap();
        let error = deferred
            .execute("UPDATE meta SET value = '2' WHERE key = 'probe'", [])
            .expect_err("a deferred read-then-write must fail after a concurrent commit");
        // Asserted on the extended result code, never on the rendered message: 517 is a
        // protocol number, "database is locked" is prose SQLite may reword, and the same
        // prose is also what a plain SQLITE_BUSY (5) carries.
        let extended = match &error {
            rusqlite::Error::SqliteFailure(inner, _) => inner.extended_code,
            other => panic!("expected a SqliteFailure, got: {other:?}"),
        };
        assert_eq!(
            extended, 517,
            "expected SQLITE_BUSY_SNAPSHOT, got: {error:?}"
        );
        drop(deferred);

        // What `write_tx` does instead: the write lock is held from BEGIN, so there is no
        // promotion left to fail and the same read-then-write shape commits.
        let immediate = db.write_tx().unwrap();
        let _: i64 = immediate
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        immediate
            .execute("UPDATE meta SET value = '3' WHERE key = 'probe'", [])
            .unwrap();
        immediate.commit().unwrap();

        let value: String = db
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'probe'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(value, "3");
    }
}

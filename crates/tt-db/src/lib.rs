//! Storage layer for the time tracker.
//!
//! Provides persistence for events and time entries using `rusqlite`.
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
//! This format is used by `chrono::DateTime<Utc>` serialization and ensures:
//! - Lexicographic ordering matches chronological ordering
//! - Human-readable values in the database
//! - Timezone-aware (always UTC)
//!
//! ## Event Payload Storage
//!
//! The `data` column stores a JSON payload (type-specific) and the `type` column stores
//! the event type (e.g., `pane_focus`, `user_message`).
//! When evolving event payloads:
//! - Adding fields: Old code should ignore unknown fields
//! - Removing fields: Old data may become unparseable (requires migration)
//! - Renaming fields: Breaks deserialization (requires migration)
//!
//! Consider adding a schema version table for future migrations.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{Connection, params, params_from_iter};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

/// Database errors.
#[derive(Debug, Error)]
pub enum DbError {
    /// An error from the underlying database.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Stream inference gap threshold is invalid.
    #[error("invalid stream gap threshold: {0}")]
    InvalidStreamGap(i64),
    /// Failed to parse an event timestamp.
    #[error("invalid timestamp for event {event_id}: {timestamp}")]
    TimestampParse {
        event_id: String,
        timestamp: String,
        #[source]
        source: chrono::ParseError,
    },
    /// Failed to parse event payload JSON or missing required fields.
    #[error("invalid event data for {event_id}: {message}")]
    InvalidEventData { event_id: String, message: String },
}

/// Database connection wrapper.
///
/// See the [module documentation](self) for thread safety considerations.
pub struct Database {
    conn: Connection,
}

/// A raw event ready to be stored in the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRecord {
    pub id: String,
    pub timestamp: String,
    pub kind: String,
    pub source: String,
    pub schema_version: i64,
    pub data: String,
    pub cwd: Option<String>,
    pub session_id: Option<String>,
    pub stream_id: Option<String>,
    pub assignment_source: Option<String>,
}

/// Stream metadata stored in the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamRecord {
    pub id: String,
    pub name: Option<String>,
}

/// Latest event timestamp grouped by source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLastEvent {
    pub source: String,
    pub last_event: String,
}

/// Summary of a stream inference pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamInferenceStats {
    pub events_assigned: usize,
    pub streams_created: usize,
}

impl Database {
    /// Opens a database at the given path, creating it if necessary.
    ///
    /// The database schema is automatically initialized on first open.
    pub fn open(path: &Path) -> Result<Self, DbError> {
        let conn = Connection::open(path)?;
        let db = Self { conn };
        db.init()?;
        Ok(db)
    }

    /// Opens an in-memory database.
    ///
    /// Useful for testing. The database is destroyed when the connection closes.
    pub fn open_in_memory() -> Result<Self, DbError> {
        let conn = Connection::open_in_memory()?;
        let db = Self { conn };
        db.init()?;
        Ok(db)
    }

    /// Initializes the database schema.
    ///
    /// This is idempotent - safe to call on an already-initialized database.
    fn init(&self) -> Result<(), DbError> {
        self.conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS streams (
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

            CREATE INDEX IF NOT EXISTS idx_streams_updated ON streams(updated_at);

            CREATE TABLE IF NOT EXISTS stream_tags (
                stream_id TEXT NOT NULL,
                tag TEXT NOT NULL,
                PRIMARY KEY (stream_id, tag),
                FOREIGN KEY (stream_id) REFERENCES streams(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_stream_tags_tag ON stream_tags(tag);

            -- Events table: stores raw activity signals
            -- timestamp: ISO 8601 format (e.g., '2024-01-15T10:30:00Z')
            -- type: event type (e.g., 'pane_focus')
            -- data: JSON payload with event fields
            CREATE TABLE IF NOT EXISTS events (
                id TEXT PRIMARY KEY,
                timestamp TEXT NOT NULL,
                type TEXT NOT NULL,
                source TEXT NOT NULL,
                schema_version INTEGER DEFAULT 1,
                data TEXT NOT NULL,
                cwd TEXT,
                session_id TEXT,
                stream_id TEXT,
                assignment_source TEXT DEFAULT 'inferred',
                FOREIGN KEY (stream_id) REFERENCES streams(id) ON DELETE SET NULL
            );

            CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events(timestamp);
            CREATE INDEX IF NOT EXISTS idx_events_type ON events(type);
            CREATE INDEX IF NOT EXISTS idx_events_stream ON events(stream_id);
            CREATE INDEX IF NOT EXISTS idx_events_cwd ON events(cwd);
            CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id);
            ",
        )?;
        Ok(())
    }

    /// Inserts a batch of events, ignoring duplicates by ID.
    pub fn insert_events(&mut self, events: &[EventRecord]) -> Result<usize, DbError> {
        if events.is_empty() {
            return Ok(0);
        }
        let tx = self.conn.transaction()?;
        let mut inserted = 0;
        {
            let mut stmt = tx.prepare(
                "
                INSERT OR IGNORE INTO events
                (id, timestamp, type, source, schema_version, data, cwd, session_id, stream_id, assignment_source)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ",
            )?;
            for event in events {
                let assignment_source = event.assignment_source.as_deref().unwrap_or("inferred");
                inserted += stmt.execute(params![
                    event.id,
                    event.timestamp,
                    event.kind,
                    event.source,
                    event.schema_version,
                    event.data,
                    event.cwd,
                    event.session_id,
                    event.stream_id,
                    assignment_source,
                ])?;
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Lists all events ordered by timestamp then ID.
    pub fn list_events(&self) -> Result<Vec<EventRecord>, DbError> {
        let mut stmt = self.conn.prepare(
            "
            SELECT id, timestamp, type, source, schema_version, data, cwd, session_id, stream_id, assignment_source
            FROM events
            ORDER BY timestamp ASC, id ASC
            ",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(EventRecord {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                kind: row.get(2)?,
                source: row.get(3)?,
                schema_version: row.get(4)?,
                data: row.get(5)?,
                cwd: row.get(6)?,
                session_id: row.get(7)?,
                stream_id: row.get(8)?,
                assignment_source: row.get(9)?,
            })
        })?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }

    /// Lists events within a time range.
    ///
    /// The range is inclusive of `start` and exclusive of `end`.
    pub fn list_events_in_range(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<EventRecord>, DbError> {
        if end <= start {
            return Ok(Vec::new());
        }
        let start = format_timestamp(start);
        let end = format_timestamp(end);
        let mut stmt = self.conn.prepare(
            "
            SELECT id, timestamp, type, source, schema_version, data, cwd, session_id, stream_id, assignment_source
            FROM events
            WHERE timestamp >= ? AND timestamp < ?
            ORDER BY timestamp ASC, id ASC
            ",
        )?;
        let rows = stmt.query_map([start, end], |row| {
            Ok(EventRecord {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                kind: row.get(2)?,
                source: row.get(3)?,
                schema_version: row.get(4)?,
                data: row.get(5)?,
                cwd: row.get(6)?,
                session_id: row.get(7)?,
                stream_id: row.get(8)?,
                assignment_source: row.get(9)?,
            })
        })?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }

    /// Lists streams ordered by ID.
    pub fn list_streams(&self) -> Result<Vec<StreamRecord>, DbError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name FROM streams ORDER BY id ASC")?;
        let rows = stmt.query_map([], |row| {
            Ok(StreamRecord {
                id: row.get(0)?,
                name: row.get(1)?,
            })
        })?;
        let mut streams = Vec::new();
        for row in rows {
            streams.push(row?);
        }
        Ok(streams)
    }

    /// Lists tags for all streams.
    pub fn list_stream_tags(&self) -> Result<HashMap<String, Vec<String>>, DbError> {
        let mut stmt = self.conn.prepare(
            "
            SELECT stream_id, tag
            FROM stream_tags
            ORDER BY stream_id ASC, tag ASC
            ",
        )?;
        let rows = stmt.query_map([], |row| {
            let stream_id: String = row.get(0)?;
            let tag: String = row.get(1)?;
            Ok((stream_id, tag))
        })?;
        let mut tags: HashMap<String, Vec<String>> = HashMap::new();
        for row in rows {
            let (stream_id, tag) = row?;
            tags.entry(stream_id).or_default().push(tag);
        }
        Ok(tags)
    }

    /// Adds a tag to a stream, ignoring duplicates.
    pub fn add_stream_tag(&mut self, stream_id: &str, tag: &str) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO stream_tags (stream_id, tag) VALUES (?, ?)",
            params![stream_id, tag],
        )?;
        Ok(())
    }

    /// Lists the last event timestamp per source, ordered by most recent.
    pub fn last_event_times_by_source(&self) -> Result<Vec<SourceLastEvent>, DbError> {
        let mut stmt = self.conn.prepare(
            "
            SELECT source, MAX(timestamp) AS last_event
            FROM events
            GROUP BY source
            ORDER BY last_event DESC, source ASC
            ",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SourceLastEvent {
                source: row.get(0)?,
                last_event: row.get(1)?,
            })
        })?;
        let mut sources = Vec::new();
        for row in rows {
            sources.push(row?);
        }
        Ok(sources)
    }

    /// Infers streams for events using directory + temporal clustering.
    pub fn infer_streams(
        &mut self,
        gap_threshold_ms: i64,
    ) -> Result<StreamInferenceStats, DbError> {
        let now = Utc::now();
        self.infer_streams_at(gap_threshold_ms, now)
    }

    fn infer_streams_at(
        &mut self,
        gap_threshold_ms: i64,
        now: DateTime<Utc>,
    ) -> Result<StreamInferenceStats, DbError> {
        if gap_threshold_ms < 0 {
            return Err(DbError::InvalidStreamGap(gap_threshold_ms));
        }
        let gap_threshold = chrono::Duration::milliseconds(gap_threshold_ms);
        let events = self.stream_events()?;
        let plan = build_inference_plan(events, gap_threshold)?;

        let updated_at = format_timestamp(now);
        let tx = self.conn.transaction()?;
        {
            let mut stream_stmt = tx.prepare(
                "
                INSERT INTO streams (id, created_at, updated_at, name, first_event_at, last_event_at, needs_recompute)
                VALUES (?, ?, ?, ?, ?, ?, 0)
                ON CONFLICT(id) DO UPDATE SET
                    updated_at = excluded.updated_at,
                    name = COALESCE(streams.name, excluded.name),
                    first_event_at = excluded.first_event_at,
                    last_event_at = excluded.last_event_at,
                    needs_recompute = 0
                ",
            )?;
            for (stream_id, bounds) in &plan.stream_bounds {
                stream_stmt.execute(params![
                    stream_id,
                    bounds.first_event_at,
                    updated_at,
                    bounds.name,
                    bounds.first_event_at,
                    bounds.last_event_at,
                ])?;
            }
        }
        {
            let mut event_stmt = tx.prepare(
                "
                UPDATE events
                SET stream_id = ?
                WHERE id = ? AND assignment_source = 'inferred'
                ",
            )?;
            for assignment in &plan.assignments {
                event_stmt.execute(params![assignment.stream_id, assignment.event_id])?;
            }
        }
        tx.commit()?;

        Ok(StreamInferenceStats {
            events_assigned: plan.assignments.len(),
            streams_created: plan.created_streams.len(),
        })
    }

    fn stream_events(&self) -> Result<Vec<StreamEventRow>, DbError> {
        let mut stmt = self.conn.prepare(
            "
            SELECT id, timestamp, cwd, stream_id, assignment_source
            FROM events
            ORDER BY timestamp ASC, id ASC
            ",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(StreamEventRow {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                cwd: row.get(2)?,
                stream_id: row.get(3)?,
                assignment_source: row.get(4)?,
            })
        })?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(events)
    }
}

#[derive(Debug)]
struct StreamEventRow {
    id: String,
    timestamp: String,
    cwd: Option<String>,
    stream_id: Option<String>,
    assignment_source: Option<String>,
}

#[derive(Debug)]
struct StreamState {
    stream_id: String,
    last_event_at: DateTime<Utc>,
}

#[derive(Debug)]
struct StreamBounds {
    first_event_at: String,
    last_event_at: String,
    name: Option<String>,
    first_event_at_parsed: DateTime<Utc>,
    last_event_at_parsed: DateTime<Utc>,
}

#[derive(Debug)]
struct EventAssignment {
    event_id: String,
    stream_id: Option<String>,
}

#[derive(Debug)]
struct InferencePlan {
    stream_bounds: HashMap<String, StreamBounds>,
    assignments: Vec<EventAssignment>,
    created_streams: HashSet<String>,
}

fn build_inference_plan(
    events: Vec<StreamEventRow>,
    gap_threshold: chrono::Duration,
) -> Result<InferencePlan, DbError> {
    let mut cwd_state: HashMap<String, StreamState> = HashMap::new();
    let mut stream_bounds: HashMap<String, StreamBounds> = HashMap::new();
    let mut assignments: Vec<EventAssignment> = Vec::new();
    let mut created_streams: HashSet<String> = HashSet::new();

    for event in events {
        let timestamp = parse_timestamp(&event.timestamp, &event.id)?;
        let assignment_source = event.assignment_source.as_deref().unwrap_or("inferred");
        let is_inferred = assignment_source == "inferred";

        if !is_inferred {
            handle_user_assignment(&event, timestamp, &mut stream_bounds, &mut cwd_state);
            continue;
        }

        let Some(cwd) = event.cwd.as_deref() else {
            assignments.push(EventAssignment {
                event_id: event.id,
                stream_id: None,
            });
            continue;
        };

        let stream_id = match cwd_state.get(cwd) {
            Some(state)
                if timestamp.signed_duration_since(state.last_event_at) <= gap_threshold =>
            {
                state.stream_id.clone()
            }
            _ => {
                let stream_id = deterministic_stream_id(cwd, &event.timestamp);
                created_streams.insert(stream_id.clone());
                stream_id
            }
        };

        assignments.push(EventAssignment {
            event_id: event.id,
            stream_id: Some(stream_id.clone()),
        });
        update_bounds(
            &mut stream_bounds,
            &stream_id,
            &event.timestamp,
            timestamp,
            Some(cwd),
        );
        cwd_state.insert(
            cwd.to_string(),
            StreamState {
                stream_id,
                last_event_at: timestamp,
            },
        );
    }

    Ok(InferencePlan {
        stream_bounds,
        assignments,
        created_streams,
    })
}

fn handle_user_assignment(
    event: &StreamEventRow,
    timestamp: DateTime<Utc>,
    stream_bounds: &mut HashMap<String, StreamBounds>,
    cwd_state: &mut HashMap<String, StreamState>,
) {
    let Some(stream_id) = event.stream_id.as_deref() else {
        return;
    };

    update_bounds(
        stream_bounds,
        stream_id,
        &event.timestamp,
        timestamp,
        event.cwd.as_deref(),
    );
    if let Some(cwd) = event.cwd.as_deref() {
        cwd_state.insert(
            cwd.to_string(),
            StreamState {
                stream_id: stream_id.to_string(),
                last_event_at: timestamp,
            },
        );
    }
}

fn deterministic_stream_id(cwd: &str, timestamp: &str) -> String {
    let content = format!("stream|{cwd}|{timestamp}");
    Uuid::new_v5(&Uuid::NAMESPACE_OID, content.as_bytes()).to_string()
}

fn parse_timestamp(timestamp: &str, event_id: &str) -> Result<DateTime<Utc>, DbError> {
    DateTime::parse_from_rfc3339(timestamp)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|source| DbError::TimestampParse {
            event_id: event_id.to_string(),
            timestamp: timestamp.to_string(),
            source,
        })
}

fn format_timestamp(timestamp: DateTime<Utc>) -> String {
    timestamp.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn update_bounds(
    bounds: &mut HashMap<String, StreamBounds>,
    stream_id: &str,
    timestamp: &str,
    timestamp_parsed: DateTime<Utc>,
    cwd: Option<&str>,
) {
    match bounds.get_mut(stream_id) {
        Some(existing) => {
            if timestamp_parsed < existing.first_event_at_parsed {
                existing.first_event_at_parsed = timestamp_parsed;
                existing.first_event_at = timestamp.to_string();
            }
            if timestamp_parsed > existing.last_event_at_parsed {
                existing.last_event_at_parsed = timestamp_parsed;
                existing.last_event_at = timestamp.to_string();
            }
            if existing.name.is_none() {
                existing.name = cwd.map(str::to_string);
            }
        }
        None => {
            bounds.insert(
                stream_id.to_string(),
                StreamBounds {
                    first_event_at: timestamp.to_string(),
                    last_event_at: timestamp.to_string(),
                    name: cwd.map(str::to_string),
                    first_event_at_parsed: timestamp_parsed,
                    last_event_at_parsed: timestamp_parsed,
                },
            );
        }
    }
}

const ATTENTION_WINDOW: chrono::Duration = chrono::Duration::seconds(120);
const AGENT_ACTIVITY_WINDOW: chrono::Duration = chrono::Duration::seconds(300);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TimeTotals {
    pub direct_ms: i64,
    pub delegated_ms: i64,
}

#[derive(Debug)]
struct TimelineEvent {
    timestamp: DateTime<Utc>,
    order: usize,
    priority: u8,
    kind: TimelineKind,
    stream_id: Option<String>,
}

#[derive(Debug, Clone)]
enum TimelineKind {
    AfkChange {
        status: AfkStatus,
        idle_duration_ms: Option<i64>,
    },
    WindowFocus {
        app: Option<String>,
    },
    BrowserTab,
    TmuxPaneFocus,
    TmuxScroll,
    UserMessage,
    AgentSession {
        action: AgentSessionAction,
    },
    AgentToolUse,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfkStatus {
    Idle,
    Active,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentSessionAction {
    Started,
    Ended,
}

#[derive(Debug, Default)]
struct AllocationState {
    afk_state: bool,
    last_attention_at: Option<DateTime<Utc>>,
    window_focus_app: Option<String>,
    window_focus_stream_id: Option<String>,
    tmux_focus_stream_id: Option<String>,
    browser_focus_stream_id: Option<String>,
    agent_sessions: HashMap<String, AgentSessionState>,
}

#[derive(Debug, Clone)]
struct AgentSessionState {
    active: bool,
    last_activity_at: DateTime<Utc>,
}

impl Database {
    /// Recomputes and persists direct/delegated time totals for all streams.
    pub fn recompute_time_allocations(&mut self) -> Result<(), DbError> {
        let events = self.list_events()?;
        let timeline = build_timeline_events(events)?;
        let totals = allocate_time(&timeline);

        let mut stream_ids: Vec<String> =
            totals.keys().filter_map(Option::as_ref).cloned().collect();
        stream_ids.sort();
        stream_ids.dedup();

        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "
                UPDATE streams
                SET time_direct_ms = ?, time_delegated_ms = ?
                WHERE id = ?
                ",
            )?;
            for (stream_id, totals) in totals
                .iter()
                .filter_map(|(k, v)| k.as_ref().map(|stream_id| (stream_id, v)))
            {
                stmt.execute(params![totals.direct_ms, totals.delegated_ms, stream_id])?;
            }
        }
        if stream_ids.is_empty() {
            tx.execute(
                "UPDATE streams SET time_direct_ms = 0, time_delegated_ms = 0",
                [],
            )?;
        } else {
            let placeholders = vec!["?"; stream_ids.len()].join(", ");
            let query = format!(
                "UPDATE streams SET time_direct_ms = 0, time_delegated_ms = 0 WHERE id NOT IN ({placeholders})"
            );
            tx.execute(&query, params_from_iter(stream_ids.iter()))?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Computes direct/delegated time totals for the provided events.
    pub fn allocate_time_for_events(
        &self,
        events: Vec<EventRecord>,
    ) -> Result<HashMap<Option<String>, TimeTotals>, DbError> {
        let timeline = build_timeline_events(events)?;
        Ok(allocate_time(&timeline))
    }
}

fn build_timeline_events(events: Vec<EventRecord>) -> Result<Vec<TimelineEvent>, DbError> {
    let mut timeline = Vec::with_capacity(events.len());
    for (order, event) in events.into_iter().enumerate() {
        let timestamp = parse_timestamp(&event.timestamp, &event.id)?;
        let data = parse_event_data(&event.data, &event.id)?;
        let kind = parse_timeline_kind(&event.kind, &data, &event.id)?;
        timeline.push(TimelineEvent {
            timestamp,
            order,
            priority: 1,
            kind: kind.clone(),
            stream_id: event.stream_id,
        });

        if let TimelineKind::AfkChange {
            status: AfkStatus::Idle,
            idle_duration_ms: Some(duration_ms),
        } = kind
        {
            if duration_ms > 0 {
                let start = timestamp - chrono::Duration::milliseconds(duration_ms);
                timeline.push(TimelineEvent {
                    timestamp: start,
                    order,
                    priority: 0,
                    kind: TimelineKind::AfkChange {
                        status: AfkStatus::Idle,
                        idle_duration_ms: None,
                    },
                    stream_id: None,
                });
            }
        }
    }

    timeline.sort_by(|a, b| {
        a.timestamp
            .cmp(&b.timestamp)
            .then_with(|| a.priority.cmp(&b.priority))
            .then_with(|| a.order.cmp(&b.order))
    });

    Ok(timeline)
}

fn parse_event_data(data: &str, event_id: &str) -> Result<Value, DbError> {
    serde_json::from_str(data).map_err(|err| DbError::InvalidEventData {
        event_id: event_id.to_string(),
        message: err.to_string(),
    })
}

fn parse_timeline_kind(kind: &str, data: &Value, event_id: &str) -> Result<TimelineKind, DbError> {
    match kind {
        "afk_change" => {
            let status = data.get("status").and_then(Value::as_str).ok_or_else(|| {
                DbError::InvalidEventData {
                    event_id: event_id.to_string(),
                    message: "missing afk status".to_string(),
                }
            })?;
            let status = match status {
                "idle" => AfkStatus::Idle,
                "active" => AfkStatus::Active,
                _ => {
                    return Err(DbError::InvalidEventData {
                        event_id: event_id.to_string(),
                        message: format!("unknown afk status {status}"),
                    });
                }
            };
            let idle_duration_ms = data.get("idle_duration_ms").and_then(Value::as_i64);
            if let Some(duration_ms) = idle_duration_ms {
                if duration_ms < 0 {
                    return Err(DbError::InvalidEventData {
                        event_id: event_id.to_string(),
                        message: "negative idle_duration_ms".to_string(),
                    });
                }
            }
            Ok(TimelineKind::AfkChange {
                status,
                idle_duration_ms,
            })
        }
        "window_focus" => Ok(TimelineKind::WindowFocus {
            app: data
                .get("app")
                .and_then(Value::as_str)
                .map(str::to_ascii_lowercase),
        }),
        "browser_tab" => Ok(TimelineKind::BrowserTab),
        "tmux_pane_focus" => Ok(TimelineKind::TmuxPaneFocus),
        "tmux_scroll" => Ok(TimelineKind::TmuxScroll),
        "user_message" => Ok(TimelineKind::UserMessage),
        "agent_session" => {
            let action = data.get("action").and_then(Value::as_str).ok_or_else(|| {
                DbError::InvalidEventData {
                    event_id: event_id.to_string(),
                    message: "missing agent_session action".to_string(),
                }
            })?;
            let action = match action {
                "started" | "start" => AgentSessionAction::Started,
                "ended" | "end" => AgentSessionAction::Ended,
                _ => {
                    return Err(DbError::InvalidEventData {
                        event_id: event_id.to_string(),
                        message: format!("unknown agent_session action {action}"),
                    });
                }
            };
            Ok(TimelineKind::AgentSession { action })
        }
        "agent_tool_use" => Ok(TimelineKind::AgentToolUse),
        _ => Ok(TimelineKind::Other),
    }
}

fn allocate_time(events: &[TimelineEvent]) -> HashMap<Option<String>, TimeTotals> {
    if events.len() < 2 {
        return HashMap::new();
    }

    let mut totals: HashMap<Option<String>, TimeTotals> = HashMap::new();
    let mut state = AllocationState::default();

    for window in events.windows(2) {
        let event = &window[0];
        let next_event = &window[1];
        apply_event(&mut state, event);

        let start = event.timestamp;
        let end = next_event.timestamp;
        if end <= start {
            continue;
        }

        let direct_ms = direct_interval_ms(&state, start, end);
        if let Some(duration_ms) = direct_ms {
            if duration_ms > 0 {
                let focus_stream = resolve_focus_stream(&state);
                let entry = totals.entry(focus_stream).or_default();
                entry.direct_ms += duration_ms;
            }
        }

        if !state.agent_sessions.is_empty() {
            for (stream_id, agent_state) in &state.agent_sessions {
                if !agent_state.active {
                    continue;
                }
                let window_end = agent_state.last_activity_at + AGENT_ACTIVITY_WINDOW;
                if window_end <= start {
                    continue;
                }
                let interval_end = if end < window_end { end } else { window_end };
                let duration_ms = interval_end.signed_duration_since(start).num_milliseconds();
                if duration_ms > 0 {
                    let entry = totals.entry(Some(stream_id.clone())).or_default();
                    entry.delegated_ms += duration_ms;
                }
            }
        }
    }

    totals
}

fn apply_event(state: &mut AllocationState, event: &TimelineEvent) {
    match &event.kind {
        TimelineKind::AfkChange { status, .. } => {
            state.afk_state = *status == AfkStatus::Idle;
        }
        TimelineKind::WindowFocus { app } => {
            state.window_focus_app.clone_from(app);
            state.window_focus_stream_id.clone_from(&event.stream_id);
            state.last_attention_at = Some(event.timestamp);
        }
        TimelineKind::BrowserTab => {
            state.browser_focus_stream_id.clone_from(&event.stream_id);
            state.last_attention_at = Some(event.timestamp);
        }
        TimelineKind::TmuxPaneFocus => {
            state.tmux_focus_stream_id.clone_from(&event.stream_id);
            state.last_attention_at = Some(event.timestamp);
        }
        TimelineKind::TmuxScroll | TimelineKind::UserMessage => {
            state.last_attention_at = Some(event.timestamp);
        }
        TimelineKind::AgentSession { action } => {
            if let Some(stream_id) = event.stream_id.as_deref() {
                let entry = state.agent_sessions.entry(stream_id.to_string());
                match action {
                    AgentSessionAction::Started => {
                        entry
                            .and_modify(|existing| {
                                existing.active = true;
                                existing.last_activity_at = event.timestamp;
                            })
                            .or_insert(AgentSessionState {
                                active: true,
                                last_activity_at: event.timestamp,
                            });
                    }
                    AgentSessionAction::Ended => {
                        entry
                            .and_modify(|existing| {
                                existing.active = false;
                            })
                            .or_insert(AgentSessionState {
                                active: false,
                                last_activity_at: event.timestamp,
                            });
                    }
                }
            }
        }
        TimelineKind::AgentToolUse => {
            if let Some(stream_id) = event.stream_id.as_deref() {
                state
                    .agent_sessions
                    .entry(stream_id.to_string())
                    .and_modify(|existing| {
                        existing.active = true;
                        existing.last_activity_at = event.timestamp;
                    })
                    .or_insert(AgentSessionState {
                        active: true,
                        last_activity_at: event.timestamp,
                    });
            }
        }
        TimelineKind::Other => {}
    }
}

fn direct_interval_ms(
    state: &AllocationState,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Option<i64> {
    if state.afk_state {
        return Some(0);
    }
    let last_attention_at = state.last_attention_at?;
    let window_end = last_attention_at + ATTENTION_WINDOW;
    if window_end <= start {
        return Some(0);
    }
    let interval_end = if end < window_end { end } else { window_end };
    let duration_ms = interval_end.signed_duration_since(start).num_milliseconds();
    Some(duration_ms.max(0))
}

fn resolve_focus_stream(state: &AllocationState) -> Option<String> {
    match state.window_focus_app.as_deref() {
        Some(app) if is_terminal_app(app) => state.tmux_focus_stream_id.clone(),
        Some(app) if is_browser_app(app) => state.browser_focus_stream_id.clone(),
        Some(_) => state.window_focus_stream_id.clone(),
        None => None,
    }
}

fn is_terminal_app(app: &str) -> bool {
    app.contains("terminal")
        || app.contains("iterm")
        || app.contains("alacritty")
        || app.contains("wezterm")
        || app.contains("kitty")
}

fn is_browser_app(app: &str) -> bool {
    app.contains("chrome")
        || app.contains("firefox")
        || app.contains("safari")
        || app.contains("edge")
        || app.contains("brave")
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn open_in_memory_database() {
        let db = Database::open_in_memory();
        assert!(db.is_ok());
    }

    #[test]
    fn schema_matches_data_model() {
        let db = Database::open_in_memory().expect("open in-memory db");

        let events_columns = table_columns(&db.conn, "events");
        assert_eq!(
            events_columns,
            vec![
                "id",
                "timestamp",
                "type",
                "source",
                "schema_version",
                "data",
                "cwd",
                "session_id",
                "stream_id",
                "assignment_source",
            ]
        );

        let streams_columns = table_columns(&db.conn, "streams");
        assert_eq!(
            streams_columns,
            vec![
                "id",
                "created_at",
                "updated_at",
                "name",
                "time_direct_ms",
                "time_delegated_ms",
                "first_event_at",
                "last_event_at",
                "needs_recompute",
            ]
        );

        let stream_tags_columns = table_columns(&db.conn, "stream_tags");
        assert_eq!(stream_tags_columns, vec!["stream_id", "tag"]);

        let event_indexes = index_names(&db.conn, "events");
        let expected_event_indexes: HashSet<String> = [
            "idx_events_timestamp",
            "idx_events_type",
            "idx_events_stream",
            "idx_events_cwd",
            "idx_events_session",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert!(expected_event_indexes.is_subset(&event_indexes));

        let streams_indexes = index_names(&db.conn, "streams");
        assert!(streams_indexes.contains("idx_streams_updated"));

        let stream_tags_indexes = index_names(&db.conn, "stream_tags");
        assert!(stream_tags_indexes.contains("idx_stream_tags_tag"));

        let events_foreign_keys = foreign_keys(&db.conn, "events");
        assert_eq!(events_foreign_keys.len(), 1);
        assert_eq!(
            events_foreign_keys[0],
            (
                "streams".to_string(),
                "stream_id".to_string(),
                "id".to_string(),
                "SET NULL".to_string(),
            )
        );

        let stream_tags_foreign_keys = foreign_keys(&db.conn, "stream_tags");
        assert_eq!(stream_tags_foreign_keys.len(), 1);
        assert_eq!(
            stream_tags_foreign_keys[0],
            (
                "streams".to_string(),
                "stream_id".to_string(),
                "id".to_string(),
                "CASCADE".to_string(),
            )
        );
    }

    fn table_columns(conn: &Connection, table: &str) -> Vec<String> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .expect("prepare table_info");
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query table_info");
        rows.map(|row| row.expect("table_info row")).collect()
    }

    fn index_names(conn: &Connection, table: &str) -> HashSet<String> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA index_list({table})"))
            .expect("prepare index_list");
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query index_list");
        rows.map(|row| row.expect("index_list row")).collect()
    }

    fn foreign_keys(conn: &Connection, table: &str) -> Vec<(String, String, String, String)> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA foreign_key_list({table})"))
            .expect("prepare foreign_key_list");
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(6)?,
                ))
            })
            .expect("query foreign_key_list");
        rows.map(|row| row.expect("foreign_key_list row")).collect()
    }

    #[test]
    fn insert_events_is_idempotent() {
        let mut db = Database::open_in_memory().expect("open in-memory db");
        let event = EventRecord {
            id: "event-1".to_string(),
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            kind: "tmux_pane_focus".to_string(),
            source: "remote.tmux".to_string(),
            schema_version: 1,
            data: r#"{"pane_id":"%1"}"#.to_string(),
            cwd: Some("/repo".to_string()),
            session_id: None,
            stream_id: None,
            assignment_source: None,
        };

        let inserted = db.insert_events(&[event.clone(), event]).unwrap();
        assert_eq!(inserted, 1);

        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn insert_events_applies_default_assignment_source() {
        let mut db = Database::open_in_memory().expect("open in-memory db");
        let event = EventRecord {
            id: "event-2".to_string(),
            timestamp: "2025-01-01T00:01:00Z".to_string(),
            kind: "agent_session".to_string(),
            source: "remote.agent".to_string(),
            schema_version: 1,
            data: r#"{"action":"started"}"#.to_string(),
            cwd: None,
            session_id: Some("sess-1".to_string()),
            stream_id: None,
            assignment_source: None,
        };

        db.insert_events(&[event]).unwrap();

        let stored: String = db
            .conn
            .query_row(
                "SELECT assignment_source FROM events WHERE id = ?",
                ["event-2"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, "inferred");
    }

    #[test]
    fn list_events_returns_ordered_rows() {
        let mut db = Database::open_in_memory().expect("open in-memory db");
        let event_a = EventRecord {
            id: "event-a".to_string(),
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            kind: "tmux_pane_focus".to_string(),
            source: "remote.tmux".to_string(),
            schema_version: 1,
            data: r#"{"pane_id":"%1"}"#.to_string(),
            cwd: Some("/repo".to_string()),
            session_id: None,
            stream_id: None,
            assignment_source: None,
        };
        let event_b = EventRecord {
            id: "event-b".to_string(),
            timestamp: "2025-01-01T00:02:00Z".to_string(),
            kind: "agent_session".to_string(),
            source: "remote.agent".to_string(),
            schema_version: 1,
            data: r#"{"action":"started"}"#.to_string(),
            cwd: None,
            session_id: Some("sess-1".to_string()),
            stream_id: None,
            assignment_source: Some("user".to_string()),
        };

        db.insert_events(&[event_b.clone(), event_a.clone()])
            .expect("insert events");

        let events = db.list_events().expect("list events");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id, event_a.id);
        assert_eq!(events[1].id, event_b.id);
        assert_eq!(events[0].cwd.as_deref(), Some("/repo"));
        assert_eq!(events[1].session_id.as_deref(), Some("sess-1"));
        assert_eq!(events[1].assignment_source.as_deref(), Some("user"));
    }

    #[test]
    fn last_event_times_by_source_returns_latest_per_source() {
        let mut db = Database::open_in_memory().expect("open in-memory db");
        let event_a1 = EventRecord {
            id: "event-a1".to_string(),
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            kind: "tmux_pane_focus".to_string(),
            source: "remote.tmux".to_string(),
            schema_version: 1,
            data: r#"{"pane_id":"%1"}"#.to_string(),
            cwd: Some("/repo".to_string()),
            session_id: None,
            stream_id: None,
            assignment_source: None,
        };
        let event_a2 = EventRecord {
            id: "event-a2".to_string(),
            timestamp: "2025-01-01T00:03:00Z".to_string(),
            kind: "tmux_pane_focus".to_string(),
            source: "remote.tmux".to_string(),
            schema_version: 1,
            data: r#"{"pane_id":"%2"}"#.to_string(),
            cwd: Some("/repo".to_string()),
            session_id: None,
            stream_id: None,
            assignment_source: None,
        };
        let event_b = EventRecord {
            id: "event-b".to_string(),
            timestamp: "2025-01-01T00:02:00Z".to_string(),
            kind: "agent_session".to_string(),
            source: "remote.agent".to_string(),
            schema_version: 1,
            data: r#"{"action":"started"}"#.to_string(),
            cwd: None,
            session_id: Some("sess-1".to_string()),
            stream_id: None,
            assignment_source: None,
        };

        db.insert_events(&[event_a1, event_b, event_a2])
            .expect("insert events");

        let sources = db
            .last_event_times_by_source()
            .expect("fetch last event times");
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].source, "remote.tmux");
        assert_eq!(sources[0].last_event, "2025-01-01T00:03:00Z");
        assert_eq!(sources[1].source, "remote.agent");
        assert_eq!(sources[1].last_event, "2025-01-01T00:02:00Z");
    }

    #[test]
    fn infer_streams_clusters_by_cwd_and_gap() {
        let mut db = Database::open_in_memory().expect("open in-memory db");
        let events = vec![
            EventRecord {
                id: "event-a1".to_string(),
                timestamp: "2025-01-01T00:00:00Z".to_string(),
                kind: "tmux_pane_focus".to_string(),
                source: "remote.tmux".to_string(),
                schema_version: 1,
                data: r#"{"pane_id":"%1"}"#.to_string(),
                cwd: Some("/repo-a".to_string()),
                session_id: None,
                stream_id: None,
                assignment_source: None,
            },
            EventRecord {
                id: "event-a2".to_string(),
                timestamp: "2025-01-01T00:10:00Z".to_string(),
                kind: "tmux_pane_focus".to_string(),
                source: "remote.tmux".to_string(),
                schema_version: 1,
                data: r#"{"pane_id":"%2"}"#.to_string(),
                cwd: Some("/repo-a".to_string()),
                session_id: None,
                stream_id: None,
                assignment_source: None,
            },
            EventRecord {
                id: "event-b1".to_string(),
                timestamp: "2025-01-01T00:05:00Z".to_string(),
                kind: "tmux_pane_focus".to_string(),
                source: "remote.tmux".to_string(),
                schema_version: 1,
                data: r#"{"pane_id":"%3"}"#.to_string(),
                cwd: Some("/repo-b".to_string()),
                session_id: None,
                stream_id: None,
                assignment_source: None,
            },
            EventRecord {
                id: "event-a3".to_string(),
                timestamp: "2025-01-01T01:00:00Z".to_string(),
                kind: "tmux_pane_focus".to_string(),
                source: "remote.tmux".to_string(),
                schema_version: 1,
                data: r#"{"pane_id":"%4"}"#.to_string(),
                cwd: Some("/repo-a".to_string()),
                session_id: None,
                stream_id: None,
                assignment_source: None,
            },
            EventRecord {
                id: "event-none".to_string(),
                timestamp: "2025-01-01T00:20:00Z".to_string(),
                kind: "agent_session".to_string(),
                source: "remote.agent".to_string(),
                schema_version: 1,
                data: r#"{"action":"started"}"#.to_string(),
                cwd: None,
                session_id: Some("sess-1".to_string()),
                stream_id: None,
                assignment_source: None,
            },
        ];

        db.insert_events(&events).expect("insert events");

        let now = DateTime::parse_from_rfc3339("2025-01-01T02:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let stats = db.infer_streams_at(1_800_000, now).expect("infer streams");
        assert_eq!(stats.streams_created, 3);

        let events = db.list_events().expect("list events");
        let mut by_id: HashMap<String, Option<String>> = HashMap::new();
        for event in events {
            by_id.insert(event.id, event.stream_id);
        }

        let a1 = by_id.get("event-a1").unwrap().clone();
        let a2 = by_id.get("event-a2").unwrap().clone();
        let a3 = by_id.get("event-a3").unwrap().clone();
        let b1 = by_id.get("event-b1").unwrap().clone();
        let none = by_id.get("event-none").unwrap().clone();

        assert_eq!(a1, a2);
        assert_ne!(a1, a3);
        assert_ne!(a1, b1);
        assert_ne!(a3, b1);
        assert!(none.is_none());

        let stream_count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM streams", [], |row| row.get(0))
            .unwrap();
        assert_eq!(stream_count, 3);
    }

    #[test]
    fn infer_streams_respects_user_assignments() {
        let mut db = Database::open_in_memory().expect("open in-memory db");
        db.conn
            .execute(
                "
                INSERT INTO streams (id, created_at, updated_at, name, first_event_at, last_event_at, needs_recompute)
                VALUES (?, ?, ?, ?, ?, ?, 0)
                ",
                params![
                    "user-stream",
                    "2025-01-01T00:00:00Z",
                    "2025-01-01T00:00:00Z",
                    "manual",
                    "2025-01-01T00:00:00Z",
                    "2025-01-01T00:00:00Z"
                ],
            )
            .unwrap();

        let events = vec![
            EventRecord {
                id: "event-user".to_string(),
                timestamp: "2025-01-01T00:00:00Z".to_string(),
                kind: "manual".to_string(),
                source: "local.manual".to_string(),
                schema_version: 1,
                data: r#"{"note":"seed"}"#.to_string(),
                cwd: Some("/repo-c".to_string()),
                session_id: None,
                stream_id: Some("user-stream".to_string()),
                assignment_source: Some("user".to_string()),
            },
            EventRecord {
                id: "event-inferred".to_string(),
                timestamp: "2025-01-01T00:10:00Z".to_string(),
                kind: "tmux_pane_focus".to_string(),
                source: "remote.tmux".to_string(),
                schema_version: 1,
                data: r#"{"pane_id":"%5"}"#.to_string(),
                cwd: Some("/repo-c".to_string()),
                session_id: None,
                stream_id: None,
                assignment_source: None,
            },
        ];

        db.insert_events(&events).expect("insert events");
        let now = DateTime::parse_from_rfc3339("2025-01-01T02:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        db.infer_streams_at(1_800_000, now).expect("infer streams");

        let events = db.list_events().expect("list events");
        let mut by_id: HashMap<String, Option<String>> = HashMap::new();
        for event in events {
            by_id.insert(event.id, event.stream_id);
        }

        assert_eq!(
            by_id.get("event-user").unwrap().as_deref(),
            Some("user-stream")
        );
        assert_eq!(
            by_id.get("event-inferred").unwrap().as_deref(),
            Some("user-stream")
        );

        let (first_event_at, last_event_at): (String, String) = db
            .conn
            .query_row(
                "SELECT first_event_at, last_event_at FROM streams WHERE id = ?",
                ["user-stream"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(first_event_at, "2025-01-01T00:00:00Z");
        assert_eq!(last_event_at, "2025-01-01T00:10:00Z");
    }

    #[test]
    fn direct_time_respects_attention_window() {
        let events = vec![
            event_record(
                "event-window",
                "2025-01-01T00:00:00Z",
                "window_focus",
                r#"{"app":"Terminal","title":"dev"}"#,
                None,
            ),
            event_record(
                "event-focus",
                "2025-01-01T00:00:00Z",
                "tmux_pane_focus",
                r#"{"pane_id":"%1"}"#,
                Some("stream-a"),
            ),
            event_record(
                "event-next",
                "2025-01-01T00:05:00Z",
                "tmux_scroll",
                r#"{"direction":"down"}"#,
                Some("stream-a"),
            ),
        ];

        let totals = compute_totals(events);
        let direct_ms = totals
            .get(&Some("stream-a".to_string()))
            .map(|total| total.direct_ms)
            .unwrap_or_default();
        assert_eq!(direct_ms, ATTENTION_WINDOW.num_milliseconds());
    }

    #[test]
    fn afk_idle_event_retroactively_zeroes_direct_time() {
        let events = vec![
            event_record(
                "event-window",
                "2025-01-01T00:00:00Z",
                "window_focus",
                r#"{"app":"Terminal","title":"dev"}"#,
                None,
            ),
            event_record(
                "event-focus",
                "2025-01-01T00:00:00Z",
                "tmux_pane_focus",
                r#"{"pane_id":"%1"}"#,
                Some("stream-a"),
            ),
            event_record(
                "event-afk",
                "2025-01-01T00:05:00Z",
                "afk_change",
                r#"{"status":"idle","idle_duration_ms":300000}"#,
                None,
            ),
        ];

        let totals = compute_totals(events);
        let direct_ms = totals
            .get(&Some("stream-a".to_string()))
            .map(|total| total.direct_ms)
            .unwrap_or_default();
        assert_eq!(direct_ms, 0);
    }

    #[test]
    fn focus_hierarchy_routes_terminal_and_browser_time() {
        let events = vec![
            event_record(
                "event-window-terminal",
                "2025-01-01T00:00:00Z",
                "window_focus",
                r#"{"app":"Terminal","title":"dev"}"#,
                None,
            ),
            event_record(
                "event-tmux-focus",
                "2025-01-01T00:00:00Z",
                "tmux_pane_focus",
                r#"{"pane_id":"%1"}"#,
                Some("stream-terminal"),
            ),
            event_record(
                "event-window-browser",
                "2025-01-01T00:01:00Z",
                "window_focus",
                r#"{"app":"Chrome","title":"docs"}"#,
                None,
            ),
            event_record(
                "event-browser-tab",
                "2025-01-01T00:01:00Z",
                "browser_tab",
                r#"{"url":"https://example.com"}"#,
                Some("stream-browser"),
            ),
            event_record(
                "event-next",
                "2025-01-01T00:02:00Z",
                "user_message",
                r#"{"length":10}"#,
                Some("stream-browser"),
            ),
        ];

        let totals = compute_totals(events);
        let terminal_direct = totals
            .get(&Some("stream-terminal".to_string()))
            .map(|total| total.direct_ms)
            .unwrap_or_default();
        let browser_direct = totals
            .get(&Some("stream-browser".to_string()))
            .map(|total| total.direct_ms)
            .unwrap_or_default();

        assert_eq!(terminal_direct, 60_000);
        assert_eq!(browser_direct, 60_000);
    }

    #[test]
    fn delegated_time_overlaps_for_parallel_sessions() {
        let events = vec![
            event_record(
                "event-agent-a",
                "2025-01-01T00:00:00Z",
                "agent_session",
                r#"{"action":"started"}"#,
                Some("stream-a"),
            ),
            event_record(
                "event-agent-b",
                "2025-01-01T00:00:00Z",
                "agent_session",
                r#"{"action":"started"}"#,
                Some("stream-b"),
            ),
            event_record(
                "event-next",
                "2025-01-01T00:01:00Z",
                "user_message",
                r#"{"length":10}"#,
                Some("stream-a"),
            ),
        ];

        let totals = compute_totals(events);
        let delegated_a = totals
            .get(&Some("stream-a".to_string()))
            .map(|total| total.delegated_ms)
            .unwrap_or_default();
        let delegated_b = totals
            .get(&Some("stream-b".to_string()))
            .map(|total| total.delegated_ms)
            .unwrap_or_default();

        assert_eq!(delegated_a, 60_000);
        assert_eq!(delegated_b, 60_000);
    }

    #[test]
    fn agent_tool_use_extends_delegated_time_window() {
        let events = vec![
            event_record(
                "event-agent-start",
                "2025-01-01T00:00:00Z",
                "agent_session",
                r#"{"action":"started"}"#,
                Some("stream-a"),
            ),
            event_record(
                "event-tool",
                "2025-01-01T00:04:10Z",
                "agent_tool_use",
                r#"{"tool":"Edit"}"#,
                Some("stream-a"),
            ),
            event_record(
                "event-next",
                "2025-01-01T00:09:10Z",
                "user_message",
                r#"{"length":10}"#,
                Some("stream-a"),
            ),
        ];

        let totals = compute_totals(events);
        let delegated = totals
            .get(&Some("stream-a".to_string()))
            .map(|total| total.delegated_ms)
            .unwrap_or_default();

        assert_eq!(delegated, 550_000);
    }

    #[test]
    fn recompute_time_allocations_updates_streams_and_zeroes_missing() {
        let mut db = Database::open_in_memory().expect("open in-memory db");
        db.conn
            .execute(
                "
                INSERT INTO streams (id, created_at, updated_at, name, first_event_at, last_event_at, needs_recompute)
                VALUES (?, ?, ?, ?, ?, ?, 0)
                ",
                params![
                    "stream-a",
                    "2025-01-01T00:00:00Z",
                    "2025-01-01T00:00:00Z",
                    "A",
                    "2025-01-01T00:00:00Z",
                    "2025-01-01T00:00:00Z"
                ],
            )
            .unwrap();
        db.conn
            .execute(
                "
                INSERT INTO streams (id, created_at, updated_at, name, first_event_at, last_event_at, needs_recompute)
                VALUES (?, ?, ?, ?, ?, ?, 0)
                ",
                params![
                    "stream-b",
                    "2025-01-01T00:00:00Z",
                    "2025-01-01T00:00:00Z",
                    "B",
                    "2025-01-01T00:00:00Z",
                    "2025-01-01T00:00:00Z"
                ],
            )
            .unwrap();

        let events = vec![
            event_record(
                "event-window",
                "2025-01-01T00:00:00Z",
                "window_focus",
                r#"{"app":"Terminal","title":"dev"}"#,
                None,
            ),
            event_record(
                "event-focus",
                "2025-01-01T00:00:00Z",
                "tmux_pane_focus",
                r#"{"pane_id":"%1"}"#,
                Some("stream-a"),
            ),
            event_record(
                "event-next",
                "2025-01-01T00:01:00Z",
                "tmux_scroll",
                r#"{"direction":"down"}"#,
                Some("stream-a"),
            ),
        ];
        db.insert_events(&events).unwrap();
        db.recompute_time_allocations().unwrap();

        let (direct_a, delegated_a): (i64, i64) = db
            .conn
            .query_row(
                "SELECT time_direct_ms, time_delegated_ms FROM streams WHERE id = ?",
                ["stream-a"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let (direct_b, delegated_b): (i64, i64) = db
            .conn
            .query_row(
                "SELECT time_direct_ms, time_delegated_ms FROM streams WHERE id = ?",
                ["stream-b"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert_eq!(direct_a, 60_000);
        assert_eq!(delegated_a, 0);
        assert_eq!(direct_b, 0);
        assert_eq!(delegated_b, 0);
    }

    fn compute_totals(events: Vec<EventRecord>) -> HashMap<Option<String>, TimeTotals> {
        let timeline = build_timeline_events(events).expect("build timeline");
        allocate_time(&timeline)
    }

    fn event_record(
        id: &str,
        timestamp: &str,
        kind: &str,
        data: &str,
        stream_id: Option<&str>,
    ) -> EventRecord {
        EventRecord {
            id: id.to_string(),
            timestamp: timestamp.to_string(),
            kind: kind.to_string(),
            source: "test".to_string(),
            schema_version: 1,
            data: data.to_string(),
            cwd: None,
            session_id: None,
            stream_id: stream_id.map(str::to_string),
            assignment_source: None,
        }
    }
}

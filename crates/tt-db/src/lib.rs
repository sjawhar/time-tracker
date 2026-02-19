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
//! if the schema version doesn't match the expected version, the database
//! fails fast rather than silently corrupting data.

use std::path::Path;

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Current schema version. Increment when making breaking schema changes.
const SCHEMA_VERSION: i32 = 8;

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

/// A coherent unit of work, grouping related events.
///
/// Streams are materialized for performance but can be recomputed from events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Stream {
    /// Unique identifier (UUID).
    pub id: String,

    /// Human-readable name (auto-generated or user-provided).
    pub name: Option<String>,

    /// When the stream was created.
    pub created_at: DateTime<Utc>,

    /// When the stream was last updated.
    pub updated_at: DateTime<Utc>,

    /// Total human attention time in milliseconds.
    pub time_direct_ms: i64,

    /// Total agent execution time in milliseconds.
    pub time_delegated_ms: i64,

    /// Timestamp of the first event in this stream.
    pub first_event_at: Option<DateTime<Utc>>,

    /// Timestamp of the last event in this stream.
    pub last_event_at: Option<DateTime<Utc>>,

    /// Flag for lazy recomputation.
    pub needs_recompute: bool,
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
    /// Checks schema version and creates tables if needed.
    /// Old schema versions are not supported - the database must be recreated.
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
            Some(v) if v == SCHEMA_VERSION => {
                // Schema already initialized and version matches
                return Ok(());
            }
            Some(v) => {
                // No migrations supported - schema v6 is a breaking change
                return Err(DbError::SchemaVersionMismatch {
                    found: v,
                    expected: SCHEMA_VERSION,
                });
            }
            None => {
                // No schema_info table, initialize fresh
            }
        }

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

                FOREIGN KEY (stream_id) REFERENCES streams(id) ON DELETE SET NULL
            );

            -- Streams table: coherent units of work
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
            CREATE INDEX IF NOT EXISTS idx_stream_tags_tag ON stream_tags(tag);

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

        // Insert schema version
        self.conn.execute(
            "INSERT INTO schema_info (version) VALUES (?1)",
            params![SCHEMA_VERSION],
        )?;

        Ok(())
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
        let tx = self.conn.unchecked_transaction()?;
        let mut count = 0;

        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO events (id, timestamp, type, source, machine_id, schema_version, cwd, git_project, git_workspace, pane_id, tmux_session, window_index, status, idle_duration_ms, action, session_id, stream_id, assignment_source)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
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
                ])?;

                count += rows;
            }
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
        let mut sql = String::from(
            "SELECT id, timestamp, type, source, machine_id, schema_version, cwd, git_project, git_workspace, pane_id, tmux_session, window_index, status, idle_duration_ms, action, session_id, stream_id, assignment_source
             FROM events WHERE 1=1",
        );
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
        let mut stmt = self.conn.prepare(
            "SELECT id, timestamp, type, source, machine_id, schema_version, cwd, git_project, git_workspace, pane_id, tmux_session, window_index, status, idle_duration_ms, action, session_id, stream_id, assignment_source
             FROM events
             WHERE timestamp >= ?1 AND timestamp <= ?2
             ORDER BY timestamp ASC",
        )?;

        let mut events = Vec::new();
        let mut rows = stmt.query(params![format_timestamp(start), format_timestamp(end)])?;

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
    /// Returns an error if a stream with the same ID already exists.
    pub fn insert_stream(&self, stream: &Stream) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT INTO streams (id, created_at, updated_at, name, time_direct_ms, time_delegated_ms, first_event_at, last_event_at, needs_recompute)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                stream.id,
                format_timestamp(stream.created_at),
                format_timestamp(stream.updated_at),
                stream.name,
                stream.time_direct_ms,
                stream.time_delegated_ms,
                format_timestamp_opt(stream.first_event_at),
                format_timestamp_opt(stream.last_event_at),
                i32::from(stream.needs_recompute),
            ],
        )?;
        Ok(())
    }

    /// Retrieves a stream by ID.
    ///
    /// Returns `None` if no stream with the given ID exists.
    pub fn get_stream(&self, id: &str) -> Result<Option<Stream>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, updated_at, name, time_direct_ms, time_delegated_ms, first_event_at, last_event_at, needs_recompute
             FROM streams WHERE id = ?1",
        )?;

        let mut rows = stmt.query(params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::row_to_stream(row)?)),
            None => Ok(None),
        }
    }

    /// Retrieves all streams.
    ///
    /// Returns streams ordered by `updated_at` descending.
    pub fn get_streams(&self) -> Result<Vec<Stream>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, updated_at, name, time_direct_ms, time_delegated_ms, first_event_at, last_event_at, needs_recompute
             FROM streams ORDER BY updated_at DESC",
        )?;

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
        self.conn.execute(
            "UPDATE events SET stream_id = ?1, assignment_source = ?2 WHERE id = ?3",
            params![stream_id, source, event_id],
        )?;
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
        let tx = self.conn.unchecked_transaction()?;
        let mut count = 0u64;

        {
            let mut stmt = tx.prepare(
                "UPDATE events SET stream_id = ?1, assignment_source = ?2 WHERE id = ?3",
            )?;

            for (event_id, stream_id) in assignments {
                count += stmt.execute(params![stream_id, source, event_id])? as u64;
            }
        }

        tx.commit()?;
        Ok(count)
    }

    /// Retrieves events assigned to a specific stream.
    ///
    /// Events are returned ordered by timestamp ascending.
    pub fn get_events_by_stream(&self, stream_id: &str) -> Result<Vec<StoredEvent>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, timestamp, type, source, machine_id, schema_version, cwd, git_project, git_workspace, pane_id, tmux_session, window_index, status, idle_duration_ms, action, session_id, stream_id, assignment_source
             FROM events WHERE stream_id = ?1 ORDER BY timestamp ASC",
        )?;

        let mut events = Vec::new();
        let mut rows = stmt.query(params![stream_id])?;
        while let Some(row) = rows.next()? {
            if let Some(event) = Self::row_to_event(row)? {
                events.push(event);
            }
        }

        Ok(events)
    }

    /// Retrieves events that are not assigned to any stream.
    ///
    /// Events are returned ordered by timestamp ascending.
    pub fn get_events_without_stream(&self) -> Result<Vec<StoredEvent>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, timestamp, type, source, machine_id, schema_version, cwd, git_project, git_workspace, pane_id, tmux_session, window_index, status, idle_duration_ms, action, session_id, stream_id, assignment_source
             FROM events WHERE stream_id IS NULL ORDER BY timestamp ASC",
        )?;

        let mut events = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            if let Some(event) = Self::row_to_event(row)? {
                events.push(event);
            }
        }

        Ok(events)
    }

    /// Clears all inferred stream assignments.
    ///
    /// User assignments (`assignment_source = 'user'`) are preserved.
    /// Returns the number of events cleared.
    pub fn clear_inferred_assignments(&self) -> Result<u64, DbError> {
        let count = self.conn.execute(
            "UPDATE events SET stream_id = NULL WHERE assignment_source = 'inferred'",
            [],
        )?;
        Ok(count as u64)
    }

    /// Deletes streams that have no events assigned to them.
    ///
    /// Returns the number of streams deleted.
    pub fn delete_orphaned_streams(&self) -> Result<u64, DbError> {
        let count = self.conn.execute(
            "DELETE FROM streams WHERE id NOT IN (SELECT DISTINCT stream_id FROM events WHERE stream_id IS NOT NULL)",
            [],
        )?;
        Ok(count as u64)
    }

    /// Updates time fields for multiple streams.
    ///
    /// Also clears the `needs_recompute` flag and updates `updated_at`.
    ///
    /// Returns the number of streams updated.
    pub fn update_stream_times(&self, times: &[tt_core::StreamTime]) -> Result<u64, DbError> {
        let tx = self.conn.unchecked_transaction()?;
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

        let count = self.conn.execute(&sql, params.as_slice())?;
        Ok(count as u64)
    }

    /// Gets streams that need recomputation.
    pub fn get_streams_needing_recompute(&self) -> Result<Vec<Stream>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, updated_at, name, time_direct_ms, time_delegated_ms, first_event_at, last_event_at, needs_recompute
             FROM streams WHERE needs_recompute = 1",
        )?;

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

    /// Resolves a stream by ID or name.
    ///
    /// First checks if the query matches a stream ID, then checks names.
    /// Returns None if no matching stream is found.
    pub fn resolve_stream(&self, query: &str) -> Result<Option<Stream>, DbError> {
        // First try by ID
        if let Some(stream) = self.get_stream(query)? {
            return Ok(Some(stream));
        }

        // Then try by name
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, updated_at, name, time_direct_ms, time_delegated_ms, first_event_at, last_event_at, needs_recompute
             FROM streams WHERE name = ?1",
        )?;

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
    /// `action`, `session_id`, `stream_id`, `assignment_source`
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

        // Parse timestamps - these should always be valid in our schema
        let created_at = DateTime::parse_from_rfc3339(&created_at_str)
            .map_or_else(
                |e| {
                    tracing::warn!(stream_id = %id, error = %e, "stream has malformed created_at, using current time");
                    Utc::now()
                },
                |dt| dt.with_timezone(&Utc),
            );
        let updated_at = DateTime::parse_from_rfc3339(&updated_at_str)
            .map_or_else(
                |e| {
                    tracing::warn!(stream_id = %id, error = %e, "stream has malformed updated_at, using current time");
                    Utc::now()
                },
                |dt| dt.with_timezone(&Utc),
            );
        let first_event_at = first_event_at_str
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc));
        let last_event_at = last_event_at_str
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        Ok(Stream {
            id,
            name,
            created_at,
            updated_at,
            time_direct_ms,
            time_delegated_ms,
            first_event_at,
            last_event_at,
            needs_recompute: needs_recompute != 0,
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
                Option::<String>::None,
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

    /// Retrieves streams that overlap with a time range.
    ///
    /// A stream overlaps if:
    /// - Its `first_event_at` is at or before the range end, AND
    /// - Its `last_event_at` is at or after the range start
    ///
    /// Streams are returned ordered by `first_event_at` ascending.
    /// Streams without timestamps (`first_event_at` or `last_event_at` is NULL) are excluded.
    pub fn streams_in_range(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<Stream>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, updated_at, name, time_direct_ms, time_delegated_ms, first_event_at, last_event_at, needs_recompute
             FROM streams
             WHERE first_event_at IS NOT NULL
               AND last_event_at IS NOT NULL
               AND first_event_at <= ?2
               AND last_event_at >= ?1
             ORDER BY first_event_at ASC",
        )?;

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
            action: None,
            cwd: Some("/home/sami/project-x".to_string()),
            session_id: None,
            stream_id: None,
            assignment_source: None,
            data: json!({}),
        }
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
            DbError::Sqlite(_) => panic!("expected SchemaVersionMismatch error"),
        }
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
    fn test_get_events_without_stream() {
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
        let events = db.get_events_without_stream().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "e2");
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
    fn test_clear_inferred_assignments() {
        let db = Database::open_in_memory().unwrap();

        let ts1 = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2025, 1, 15, 11, 0, 0).unwrap();

        db.insert_event(&make_event("e1", ts1, tt_core::EventType::TmuxPaneFocus))
            .unwrap();
        db.insert_event(&make_event("e2", ts2, tt_core::EventType::TmuxPaneFocus))
            .unwrap();

        db.insert_stream(&make_stream("s1", Some("test"))).unwrap();

        // Assign one as inferred, one as user
        db.assign_event_to_stream("e1", "s1", "inferred").unwrap();
        db.assign_event_to_stream("e2", "s1", "user").unwrap();

        // Clear inferred assignments
        let cleared = db.clear_inferred_assignments().unwrap();
        assert_eq!(cleared, 1);

        // Verify: e1 should be unassigned, e2 should still be assigned
        let unassigned = db.get_events_without_stream().unwrap();
        assert_eq!(unassigned.len(), 1);
        assert_eq!(unassigned[0].id, "e1");

        let assigned = db.get_events_by_stream("s1").unwrap();
        assert_eq!(assigned.len(), 1);
        assert_eq!(assigned[0].id, "e2");
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

        db.upsert_agent_session(&entry).unwrap();

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

        db.upsert_agent_session(&entry).unwrap();

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

    fn make_stream_with_times(
        id: &str,
        name: Option<&str>,
        first_event_at: Option<DateTime<Utc>>,
        last_event_at: Option<DateTime<Utc>>,
    ) -> Stream {
        let now = Utc::now();
        Stream {
            id: id.to_string(),
            name: name.map(String::from),
            created_at: now,
            updated_at: now,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at,
            last_event_at,
            needs_recompute: false,
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
        db.insert_stream(&make_stream_with_times(
            "s1",
            Some("overlapping"),
            Some(stream_first),
            Some(stream_last),
        ))
        .unwrap();

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
        db.insert_stream(&make_stream_with_times(
            "s1",
            Some("contained"),
            Some(stream_first),
            Some(stream_last),
        ))
        .unwrap();

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
        db.insert_stream(&make_stream_with_times(
            "s1",
            Some("container"),
            Some(stream_first),
            Some(stream_last),
        ))
        .unwrap();

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
        db.insert_stream(&make_stream_with_times(
            "s1",
            Some("before"),
            Some(stream_first),
            Some(stream_last),
        ))
        .unwrap();

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
        db.insert_stream(&make_stream_with_times(
            "s1",
            Some("after"),
            Some(stream_first),
            Some(stream_last),
        ))
        .unwrap();

        let start = Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap();

        let streams = db.streams_in_range(start, end).unwrap();
        assert!(streams.is_empty());
    }

    #[test]
    fn test_streams_in_range_excludes_null_timestamps() {
        let db = Database::open_in_memory().unwrap();

        // Stream with null first_event_at
        db.insert_stream(&make_stream_with_times(
            "s1",
            Some("null-first"),
            None,
            Some(Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap()),
        ))
        .unwrap();

        // Stream with null last_event_at
        db.insert_stream(&make_stream_with_times(
            "s2",
            Some("null-last"),
            Some(Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap()),
            None,
        ))
        .unwrap();

        // Stream with both timestamps
        let stream_first = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        let stream_last = Utc.with_ymd_and_hms(2025, 1, 15, 11, 0, 0).unwrap();
        db.insert_stream(&make_stream_with_times(
            "s3",
            Some("valid"),
            Some(stream_first),
            Some(stream_last),
        ))
        .unwrap();

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
        db.insert_stream(&make_stream_with_times(
            "s1",
            Some("early"),
            Some(Utc.with_ymd_and_hms(2025, 1, 15, 8, 0, 0).unwrap()),
            Some(Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap()),
        ))
        .unwrap();

        // Stream 2: 10:00-11:00 (fully contained)
        db.insert_stream(&make_stream_with_times(
            "s2",
            Some("middle"),
            Some(Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap()),
            Some(Utc.with_ymd_and_hms(2025, 1, 15, 11, 0, 0).unwrap()),
        ))
        .unwrap();

        // Stream 3: 11:00-13:00 (overlaps with end)
        db.insert_stream(&make_stream_with_times(
            "s3",
            Some("late"),
            Some(Utc.with_ymd_and_hms(2025, 1, 15, 11, 0, 0).unwrap()),
            Some(Utc.with_ymd_and_hms(2025, 1, 15, 13, 0, 0).unwrap()),
        ))
        .unwrap();

        // Stream 4: 14:00-15:00 (completely outside)
        db.insert_stream(&make_stream_with_times(
            "s4",
            Some("outside"),
            Some(Utc.with_ymd_and_hms(2025, 1, 15, 14, 0, 0).unwrap()),
            Some(Utc.with_ymd_and_hms(2025, 1, 15, 15, 0, 0).unwrap()),
        ))
        .unwrap();

        let start = Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap();

        let streams = db.streams_in_range(start, end).unwrap();
        assert_eq!(streams.len(), 3);

        // Should be ordered by first_event_at
        assert_eq!(streams[0].id, "s1");
        assert_eq!(streams[1].id, "s2");
        assert_eq!(streams[2].id, "s3");
    }

    #[test]
    fn test_streams_in_range_boundary_conditions() {
        let db = Database::open_in_memory().unwrap();

        // Stream that ends exactly at query start
        db.insert_stream(&make_stream_with_times(
            "s1",
            Some("boundary-end"),
            Some(Utc.with_ymd_and_hms(2025, 1, 15, 8, 0, 0).unwrap()),
            Some(Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0).unwrap()),
        ))
        .unwrap();

        // Stream that starts exactly at query end
        db.insert_stream(&make_stream_with_times(
            "s2",
            Some("boundary-start"),
            Some(Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap()),
            Some(Utc.with_ymd_and_hms(2025, 1, 15, 13, 0, 0).unwrap()),
        ))
        .unwrap();

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
}

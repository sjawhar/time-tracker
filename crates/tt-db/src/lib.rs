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
const SCHEMA_VERSION: i32 = 3;

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
    pub event_type: String,

    /// Event source (e.g., "remote.tmux", "remote.agent").
    pub source: String,

    /// Schema version of the payload (default: 1).
    #[serde(default = "default_schema_version")]
    pub schema_version: i32,

    /// Type-specific JSON payload.
    pub data: serde_json::Value,

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
}

const fn default_schema_version() -> i32 {
    1
}

// Implement InferableEvent for StoredEvent so it can be used with the inference algorithm
impl tt_core::InferableEvent for StoredEvent {
    fn event_id(&self) -> &str {
        &self.id
    }

    fn timestamp(&self) -> DateTime<Utc> {
        self.timestamp
    }

    fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    fn assignment_source(&self) -> Option<&str> {
        self.assignment_source.as_deref()
    }

    fn stream_id(&self) -> Option<&str> {
        self.stream_id.as_deref()
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

    /// Initializes the database schema.
    ///
    /// Checks schema version and creates tables if needed.
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
            Some(v) if v != SCHEMA_VERSION => {
                return Err(DbError::SchemaVersionMismatch {
                    found: v,
                    expected: SCHEMA_VERSION,
                });
            }
            Some(_) => {
                // Schema already initialized and version matches
                return Ok(());
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
                schema_version INTEGER DEFAULT 1,
                data TEXT NOT NULL,

                -- Extracted for indexing (duplicated from data)
                cwd TEXT,
                session_id TEXT,

                -- Stream assignment (null = 'Uncategorized')
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

            -- Indexes for common queries
            CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events(timestamp);
            CREATE INDEX IF NOT EXISTS idx_events_type ON events(type);
            CREATE INDEX IF NOT EXISTS idx_events_stream ON events(stream_id);
            CREATE INDEX IF NOT EXISTS idx_events_cwd ON events(cwd);
            CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id);
            CREATE INDEX IF NOT EXISTS idx_streams_updated ON streams(updated_at);
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
                "INSERT OR IGNORE INTO events (id, timestamp, type, source, schema_version, data, cwd, session_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;

            for event in events {
                let data_json = serde_json::to_string(&event.data).unwrap_or_default();
                // Use consistent format: always 'Z' suffix, second precision
                // This ensures lexicographic ordering matches chronological ordering
                let timestamp_str = event.timestamp.to_rfc3339_opts(SecondsFormat::Secs, true);

                let rows = stmt.execute(params![
                    event.id,
                    timestamp_str,
                    event.event_type,
                    event.source,
                    event.schema_version,
                    data_json,
                    event.cwd,
                    event.session_id,
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
    /// Events with malformed JSON in the `data` column are skipped with a warning.
    pub fn get_events(
        &self,
        after: Option<DateTime<Utc>>,
        before: Option<DateTime<Utc>>,
    ) -> Result<Vec<StoredEvent>, DbError> {
        let mut sql = String::from(
            "SELECT id, timestamp, type, source, schema_version, data, cwd, session_id, stream_id, assignment_source
             FROM events WHERE 1=1",
        );
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(ref after_ts) = after {
            sql.push_str(" AND timestamp > ?");
            params_vec.push(Box::new(
                after_ts.to_rfc3339_opts(SecondsFormat::Secs, true),
            ));
        }

        if let Some(ref before_ts) = before {
            sql.push_str(" AND timestamp < ?");
            params_vec.push(Box::new(
                before_ts.to_rfc3339_opts(SecondsFormat::Secs, true),
            ));
        }

        sql.push_str(" ORDER BY timestamp ASC");

        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(AsRef::as_ref).collect();
        let mut stmt = self.conn.prepare(&sql)?;

        let rows = stmt.query_map(params_refs.as_slice(), |row| {
            let id: String = row.get(0)?;
            let timestamp_str: String = row.get(1)?;
            let event_type: String = row.get(2)?;
            let source: String = row.get(3)?;
            let schema_version: i32 = row.get(4)?;
            let data_str: String = row.get(5)?;
            let cwd: Option<String> = row.get(6)?;
            let session_id: Option<String> = row.get(7)?;
            let stream_id: Option<String> = row.get(8)?;
            let assignment_source: Option<String> = row.get(9)?;

            Ok((
                id,
                timestamp_str,
                event_type,
                source,
                schema_version,
                data_str,
                cwd,
                session_id,
                stream_id,
                assignment_source,
            ))
        })?;

        let mut events = Vec::new();
        for row_result in rows {
            let (
                id,
                timestamp_str,
                event_type,
                source,
                schema_version,
                data_str,
                cwd,
                session_id,
                stream_id,
                assignment_source,
            ) = row_result?;

            // Parse timestamp
            let timestamp = match DateTime::parse_from_rfc3339(&timestamp_str) {
                Ok(dt) => dt.with_timezone(&Utc),
                Err(e) => {
                    tracing::warn!(event_id = %id, error = %e, "skipping event with malformed timestamp");
                    continue;
                }
            };

            // Parse data JSON
            let data: serde_json::Value = match serde_json::from_str(&data_str) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(event_id = %id, error = %e, "skipping event with malformed JSON data");
                    continue;
                }
            };

            events.push(StoredEvent {
                id,
                timestamp,
                event_type,
                source,
                schema_version,
                data,
                cwd,
                session_id,
                stream_id,
                assignment_source,
            });
        }

        Ok(events)
    }

    // ========== Stream Methods ==========

    /// Inserts a stream into the database.
    ///
    /// Returns an error if a stream with the same ID already exists.
    pub fn insert_stream(&self, stream: &Stream) -> Result<(), DbError> {
        let created_str = stream.created_at.to_rfc3339_opts(SecondsFormat::Secs, true);
        let updated_str = stream.updated_at.to_rfc3339_opts(SecondsFormat::Secs, true);
        let first_event_str = stream
            .first_event_at
            .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Secs, true));
        let last_event_str = stream
            .last_event_at
            .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Secs, true));

        self.conn.execute(
            "INSERT INTO streams (id, created_at, updated_at, name, time_direct_ms, time_delegated_ms, first_event_at, last_event_at, needs_recompute)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                stream.id,
                created_str,
                updated_str,
                stream.name,
                stream.time_direct_ms,
                stream.time_delegated_ms,
                first_event_str,
                last_event_str,
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
        let queried_stream_id = stream_id.to_string();
        let mut stmt = self.conn.prepare(
            "SELECT id, timestamp, type, source, schema_version, data, cwd, session_id, stream_id, assignment_source
             FROM events WHERE stream_id = ?1 ORDER BY timestamp ASC",
        )?;

        let rows = stmt.query_map(params![stream_id], |row| {
            let id: String = row.get(0)?;
            let timestamp_str: String = row.get(1)?;
            let event_type: String = row.get(2)?;
            let source: String = row.get(3)?;
            let schema_version: i32 = row.get(4)?;
            let data_str: String = row.get(5)?;
            let cwd: Option<String> = row.get(6)?;
            let session_id: Option<String> = row.get(7)?;
            let stream_id: Option<String> = row.get(8)?;
            let assignment_source: Option<String> = row.get(9)?;

            Ok((
                id,
                timestamp_str,
                event_type,
                source,
                schema_version,
                data_str,
                cwd,
                session_id,
                stream_id,
                assignment_source,
            ))
        })?;

        let mut events = Vec::new();
        for row_result in rows {
            let (
                id,
                timestamp_str,
                event_type,
                source,
                schema_version,
                data_str,
                cwd,
                session_id,
                stream_id,
                assignment_source,
            ) = row_result?;

            let timestamp = match DateTime::parse_from_rfc3339(&timestamp_str) {
                Ok(dt) => dt.with_timezone(&Utc),
                Err(e) => {
                    tracing::warn!(event_id = %id, error = %e, "skipping event with malformed timestamp");
                    continue;
                }
            };

            let data: serde_json::Value = match serde_json::from_str(&data_str) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(event_id = %id, error = %e, "skipping event with malformed JSON data");
                    continue;
                }
            };

            events.push(StoredEvent {
                id,
                timestamp,
                event_type,
                source,
                schema_version,
                data,
                cwd,
                session_id,
                stream_id,
                assignment_source,
            });
        }

        // Suppress unused variable warning
        let _ = queried_stream_id;

        Ok(events)
    }

    /// Retrieves events that are not assigned to any stream.
    ///
    /// Events are returned ordered by timestamp ascending.
    pub fn get_events_without_stream(&self) -> Result<Vec<StoredEvent>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, timestamp, type, source, schema_version, data, cwd, session_id, stream_id, assignment_source
             FROM events WHERE stream_id IS NULL ORDER BY timestamp ASC",
        )?;

        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let timestamp_str: String = row.get(1)?;
            let event_type: String = row.get(2)?;
            let source: String = row.get(3)?;
            let schema_version: i32 = row.get(4)?;
            let data_str: String = row.get(5)?;
            let cwd: Option<String> = row.get(6)?;
            let session_id: Option<String> = row.get(7)?;
            let stream_id: Option<String> = row.get(8)?;
            let assignment_source: Option<String> = row.get(9)?;

            Ok((
                id,
                timestamp_str,
                event_type,
                source,
                schema_version,
                data_str,
                cwd,
                session_id,
                stream_id,
                assignment_source,
            ))
        })?;

        let mut events = Vec::new();
        for row_result in rows {
            let (
                id,
                timestamp_str,
                event_type,
                source,
                schema_version,
                data_str,
                cwd,
                session_id,
                stream_id,
                assignment_source,
            ) = row_result?;

            let timestamp = match DateTime::parse_from_rfc3339(&timestamp_str) {
                Ok(dt) => dt.with_timezone(&Utc),
                Err(e) => {
                    tracing::warn!(event_id = %id, error = %e, "skipping event with malformed timestamp");
                    continue;
                }
            };

            let data: serde_json::Value = match serde_json::from_str(&data_str) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(event_id = %id, error = %e, "skipping event with malformed JSON data");
                    continue;
                }
            };

            events.push(StoredEvent {
                id,
                timestamp,
                event_type,
                source,
                schema_version,
                data,
                cwd,
                session_id,
                stream_id,
                assignment_source,
            });
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
            .map_or_else(|_| Utc::now(), |dt| dt.with_timezone(&Utc));
        let updated_at = DateTime::parse_from_rfc3339(&updated_at_str)
            .map_or_else(|_| Utc::now(), |dt| dt.with_timezone(&Utc));
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

    fn make_event(id: &str, timestamp: DateTime<Utc>) -> StoredEvent {
        StoredEvent {
            id: id.to_string(),
            timestamp,
            event_type: "tmux_pane_focus".to_string(),
            source: "remote.tmux".to_string(),
            schema_version: 1,
            data: json!({
                "pane_id": "%3",
                "session_name": "dev",
                "window_index": 1,
                "cwd": "/home/sami/project-x"
            }),
            cwd: Some("/home/sami/project-x".to_string()),
            session_id: None,
            stream_id: None,
            assignment_source: None,
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
            event_type: "agent_session".to_string(),
            source: "remote.agent".to_string(),
            schema_version: 1,
            data: json!({
                "action": "started",
                "agent": "claude-code",
                "session_id": "abc123",
                "cwd": "/home/sami/project"
            }),
            cwd: Some("/home/sami/project".to_string()),
            session_id: Some("abc123".to_string()),
            stream_id: None,
            assignment_source: None,
        };

        let inserted = db.insert_event(&event).unwrap();
        assert!(inserted);

        let events = db.get_events(None, None).unwrap();
        assert_eq!(events.len(), 1);

        let retrieved = &events[0];
        assert_eq!(retrieved.id, "test-event-123");
        assert_eq!(retrieved.timestamp, ts);
        assert_eq!(retrieved.event_type, "agent_session");
        assert_eq!(retrieved.source, "remote.agent");
        assert_eq!(retrieved.schema_version, 1);
        assert_eq!(retrieved.data["action"], "started");
        assert_eq!(retrieved.cwd, Some("/home/sami/project".to_string()));
        assert_eq!(retrieved.session_id, Some("abc123".to_string()));
    }

    #[test]
    fn test_insert_event_idempotent() {
        let db = Database::open_in_memory().unwrap();
        let ts = Utc.with_ymd_and_hms(2025, 1, 15, 10, 30, 0).unwrap();
        let event = make_event("duplicate-id", ts);

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

        db.insert_event(&make_event("e1", ts1)).unwrap();
        db.insert_event(&make_event("e2", ts2)).unwrap();
        db.insert_event(&make_event("e3", ts3)).unwrap();

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

        db.insert_event(&make_event("e1", ts1)).unwrap();
        db.insert_event(&make_event("e2", ts2)).unwrap();
        db.insert_event(&make_event("e3", ts3)).unwrap();

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

        db.insert_event(&make_event("e1", ts1)).unwrap();
        db.insert_event(&make_event("e2", ts2)).unwrap();
        db.insert_event(&make_event("e3", ts3)).unwrap();

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

        db.insert_event(&make_event("e2", ts2)).unwrap();
        db.insert_event(&make_event("e1", ts1)).unwrap();
        db.insert_event(&make_event("e3", ts3)).unwrap();

        let events = db.get_events(None, None).unwrap();

        assert_eq!(events.len(), 3);
        assert_eq!(events[0].id, "e1");
        assert_eq!(events[1].id, "e2");
        assert_eq!(events[2].id, "e3");
    }

    #[test]
    fn test_insert_event_with_null_optionals() {
        let db = Database::open_in_memory().unwrap();
        let ts = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();

        let event = StoredEvent {
            id: "no-optionals".to_string(),
            timestamp: ts,
            event_type: "heartbeat".to_string(),
            source: "remote.tmux".to_string(),
            schema_version: 1,
            data: json!({}),
            cwd: None,
            session_id: None,
            stream_id: None,
            assignment_source: None,
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
                make_event(&format!("batch-{i}"), ts)
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
        db.insert_event(&make_event("existing", ts1)).unwrap();

        // Batch insert: one new, one duplicate
        let events = vec![make_event("existing", ts1), make_event("new", ts2)];

        let count = db.insert_events(&events).unwrap();
        assert_eq!(count, 1, "should only count the new insert");
    }

    #[test]
    fn test_get_events_skips_malformed_json() {
        let db = Database::open_in_memory().unwrap();

        // Insert a valid event
        let ts = Utc.with_ymd_and_hms(2025, 1, 15, 10, 0, 0).unwrap();
        db.insert_event(&make_event("valid", ts)).unwrap();

        // Insert malformed JSON directly
        db.conn
            .execute(
                "INSERT INTO events (id, timestamp, type, source, schema_version, data)
                 VALUES ('malformed', '2025-01-15T11:00:00Z', 'test', 'test', 1, 'not valid json {')",
                [],
            )
            .unwrap();

        // Query should return only the valid event
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
            event_type: "test_event".to_string(),
            source: source.to_string(),
            schema_version: 1,
            data: json!({}),
            cwd: None,
            session_id: None,
            stream_id: None,
            assignment_source: None,
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
        db.insert_event(&make_event("e1", ts)).unwrap();

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

        db.insert_event(&make_event("e1", ts1)).unwrap();
        db.insert_event(&make_event("e2", ts2)).unwrap();

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

        db.insert_event(&make_event("e1", ts1)).unwrap();
        db.insert_event(&make_event("e2", ts2)).unwrap();
        db.insert_event(&make_event("e3", ts3)).unwrap();

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

        db.insert_event(&make_event("e1", ts1)).unwrap();
        db.insert_event(&make_event("e2", ts2)).unwrap();

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
}

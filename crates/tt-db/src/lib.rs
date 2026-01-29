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
const SCHEMA_VERSION: i32 = 2;

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
}

const fn default_schema_version() -> i32 {
    1
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
                assignment_source TEXT DEFAULT 'inferred'
            );

            -- Indexes for common queries
            CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events(timestamp);
            CREATE INDEX IF NOT EXISTS idx_events_type ON events(type);
            CREATE INDEX IF NOT EXISTS idx_events_stream ON events(stream_id);
            CREATE INDEX IF NOT EXISTS idx_events_cwd ON events(cwd);
            CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id);
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
            "SELECT id, timestamp, type, source, schema_version, data, cwd, session_id
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

            Ok((
                id,
                timestamp_str,
                event_type,
                source,
                schema_version,
                data_str,
                cwd,
                session_id,
            ))
        })?;

        let mut events = Vec::new();
        for row_result in rows {
            let (id, timestamp_str, event_type, source, schema_version, data_str, cwd, session_id) =
                row_result?;

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
            });
        }

        Ok(events)
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
}

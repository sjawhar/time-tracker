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
use rusqlite::{Connection, params};
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
}

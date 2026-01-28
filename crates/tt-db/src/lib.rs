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

use std::path::Path;

use rusqlite::Connection;
use thiserror::Error;

/// Database errors.
#[derive(Debug, Error)]
pub enum DbError {
    /// An error from the underlying database.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Database connection wrapper.
///
/// See the [module documentation](self) for thread safety considerations.
pub struct Database {
    conn: Connection,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

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
}

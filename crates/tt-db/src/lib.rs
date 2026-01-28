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
//! ## Event Kind Storage
//!
//! The `kind` column stores `EventKind` as JSON text (e.g., `{"type":"file_save","path":"/foo"}`).
//! When evolving the `EventKind` enum:
//! - Adding variants: Old code won't recognize new variants (handle gracefully)
//! - Removing variants: Old data becomes unparseable (requires migration)
//! - Renaming variants: Breaks deserialization (requires migration)
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
        self.conn.execute_batch(
            "
            -- Events table: stores raw activity signals
            -- timestamp: ISO 8601 format (e.g., '2024-01-15T10:30:00Z')
            -- kind: JSON object with 'type' field (e.g., '{\"type\":\"file_save\",\"path\":\"/foo\"}')
            -- stream: identifier for the event source (e.g., 'editor', 'terminal')
            CREATE TABLE IF NOT EXISTS events (
                id TEXT PRIMARY KEY,
                timestamp TEXT NOT NULL,
                kind TEXT NOT NULL,
                stream TEXT NOT NULL,
                metadata TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events(timestamp);
            CREATE INDEX IF NOT EXISTS idx_events_stream ON events(stream);
            ",
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_memory_database() {
        let db = Database::open_in_memory();
        assert!(db.is_ok());
    }
}

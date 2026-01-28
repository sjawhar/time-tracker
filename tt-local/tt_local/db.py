"""SQLite event store for Time Tracker."""

from __future__ import annotations

import hashlib
import json
import sqlite3
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from pydantic import BaseModel


class RawEvent(BaseModel):
    """Raw event for validation during import.

    ID is computed from content hash, not provided.
    """

    timestamp: str
    type: str
    source: str
    schema_version: int = 1
    data: dict[str, Any]
    cwd: str | None = None
    session_id: str | None = None

    def compute_id(self) -> str:
        """Compute deterministic ID from content hash.

        All fields that differentiate events are included to avoid collisions.
        """
        content = "|".join([
            self.source,
            self.type,
            self.timestamp,
            json.dumps(self.data, sort_keys=True),
            self.cwd or "",
            self.session_id or "",
        ])
        return hashlib.sha256(content.encode()).hexdigest()[:32]


class ImportedEvent(BaseModel):
    """Event from remote export with pre-computed ID.

    The remote `tt export` command outputs events with IDs already computed.
    We trust these IDs since it's our own code running on our machine.
    """

    id: str
    timestamp: str
    type: str
    source: str
    data: dict[str, Any]
    cwd: str | None = None


SCHEMA = """
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

CREATE TABLE IF NOT EXISTS stream_tags (
    stream_id TEXT NOT NULL,
    tag TEXT NOT NULL,
    PRIMARY KEY (stream_id, tag),
    FOREIGN KEY (stream_id) REFERENCES streams(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events(timestamp);
CREATE INDEX IF NOT EXISTS idx_events_type ON events(type);
CREATE INDEX IF NOT EXISTS idx_events_stream ON events(stream_id);
CREATE INDEX IF NOT EXISTS idx_events_cwd ON events(cwd);
CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id);
CREATE INDEX IF NOT EXISTS idx_streams_updated ON streams(updated_at);
CREATE INDEX IF NOT EXISTS idx_stream_tags_tag ON stream_tags(tag);
"""


class EventStore:
    """SQLite-backed event store.

    Not thread-safe. Each thread should have its own EventStore instance.
    """

    def __init__(self, conn: sqlite3.Connection) -> None:
        self._conn = conn
        self._conn.execute("PRAGMA foreign_keys = ON")
        self._init_schema()

    def __enter__(self) -> "EventStore":
        return self

    def __exit__(self, exc_type: type | None, exc_val: Exception | None, exc_tb: object) -> None:
        self.close()

    def close(self) -> None:
        """Close the database connection."""
        self._conn.close()

    def _init_schema(self) -> None:
        """Initialize database schema."""
        self._conn.executescript(SCHEMA)
        self._conn.commit()

    @classmethod
    def open(cls, path: Path) -> EventStore:
        """Open or create a database at the given path."""
        conn = sqlite3.connect(path)
        conn.row_factory = sqlite3.Row
        return cls(conn)

    @classmethod
    def open_in_memory(cls) -> EventStore:
        """Create an in-memory database for testing."""
        conn = sqlite3.connect(":memory:")
        conn.row_factory = sqlite3.Row
        return cls(conn)

    def insert_event(
        self,
        event: RawEvent,
        *,
        stream_id: str | None = None,
        assignment_source: str = "inferred",
    ) -> str:
        """Insert an event into the store. Returns the event ID.

        Uses INSERT OR IGNORE for idempotent inserts (same ID = no-op).
        """
        event_id = event.compute_id()
        self._conn.execute(
            """
            INSERT OR IGNORE INTO events
            (id, timestamp, type, source, schema_version, data, cwd, session_id, stream_id, assignment_source)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            (
                event_id,
                event.timestamp,
                event.type,
                event.source,
                event.schema_version,
                json.dumps(event.data),
                event.cwd,
                event.session_id,
                stream_id,
                assignment_source,
            ),
        )
        self._conn.commit()
        return event_id

    def insert_imported_event(self, event: ImportedEvent) -> bool:
        """Insert an event from remote export with pre-computed ID.

        Returns True if the event was inserted, False if it already existed.
        Uses INSERT OR IGNORE for idempotent inserts.
        """
        cursor = self._conn.execute(
            """
            INSERT OR IGNORE INTO events
            (id, timestamp, type, source, schema_version, data, cwd, session_id, stream_id, assignment_source)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            (
                event.id,
                event.timestamp,
                event.type,
                event.source,
                1,  # schema_version default
                json.dumps(event.data),
                event.cwd,
                None,  # session_id not provided by remote export
                None,  # stream_id
                "imported",  # assignment_source
            ),
        )
        self._conn.commit()
        return cursor.rowcount > 0

    def get_events(
        self,
        *,
        start: str | None = None,
        end: str | None = None,
        event_type: str | None = None,
        limit: int | None = None,
    ) -> list[dict[str, Any]]:
        """Query events, optionally filtered by time range and type.

        Args:
            start: ISO 8601 timestamp (inclusive lower bound)
            end: ISO 8601 timestamp (exclusive upper bound)
            event_type: Filter by event type
            limit: Maximum number of events to return

        Returns:
            List of event dicts ordered by timestamp ascending.
        """
        query = "SELECT * FROM events WHERE 1=1"
        params: list[str | int] = []

        if start is not None:
            query += " AND timestamp >= ?"
            params.append(start)
        if end is not None:
            query += " AND timestamp < ?"
            params.append(end)
        if event_type is not None:
            query += " AND type = ?"
            params.append(event_type)

        query += " ORDER BY timestamp ASC"

        if limit is not None:
            query += " LIMIT ?"
            params.append(limit)

        cursor = self._conn.execute(query, params)
        rows = cursor.fetchall()
        return [dict(row) for row in rows]

    def create_stream(self, *, name: str | None = None) -> str:
        """Create a new stream and return its ID."""
        stream_id = str(uuid.uuid4())
        now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        self._conn.execute(
            """
            INSERT INTO streams (id, created_at, updated_at, name)
            VALUES (?, ?, ?, ?)
            """,
            (stream_id, now, now, name),
        )
        self._conn.commit()
        return stream_id

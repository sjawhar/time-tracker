"""SQLite event store for Time Tracker."""

from __future__ import annotations

import hashlib
import json
import sqlite3
import uuid
from collections import defaultdict
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

    def create_stream(self, *, name: str | None = None, commit: bool = True) -> str:
        """Create a new stream and return its ID.

        Args:
            name: Optional stream name.
            commit: Whether to commit immediately (default True).
                    Set to False when called within a larger transaction.
        """
        stream_id = str(uuid.uuid4())
        now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        self._conn.execute(
            """
            INSERT INTO streams (id, created_at, updated_at, name)
            VALUES (?, ?, ?, ?)
            """,
            (stream_id, now, now, name),
        )
        if commit:
            self._conn.commit()
        return stream_id

    def get_last_event_per_source(self) -> list[dict[str, Any]]:
        """Get the most recent event timestamp for each source.

        Returns list of dicts with keys: source, last_timestamp, event_count.
        Ordered by last_timestamp descending (most recent first).
        """
        cursor = self._conn.execute("""
            SELECT
                source,
                MAX(timestamp) as last_timestamp,
                COUNT(*) as event_count
            FROM events
            GROUP BY source
            ORDER BY last_timestamp DESC
        """)
        return [dict(row) for row in cursor.fetchall()]

    def get_streams(self) -> list[dict[str, Any]]:
        """Get all streams."""
        cursor = self._conn.execute("SELECT * FROM streams ORDER BY created_at")
        return [dict(row) for row in cursor.fetchall()]

    def get_unassigned_events(self) -> list[dict[str, Any]]:
        """Get events where assignment_source != 'user' and stream_id IS NULL."""
        cursor = self._conn.execute("""
            SELECT * FROM events
            WHERE stream_id IS NULL AND assignment_source != 'user'
            ORDER BY timestamp ASC
        """)
        return [dict(row) for row in cursor.fetchall()]

    def assign_events_to_stream(self, event_ids: list[str], stream_id: str) -> None:
        """Assign multiple events to a stream (sets assignment_source = 'inferred').

        Uses batching (500 IDs per query) to stay under SQLite's 999-parameter limit.
        Does not commit - caller is responsible for transaction management.
        """
        if not event_ids:
            return
        batch_size = 500
        for i in range(0, len(event_ids), batch_size):
            batch = event_ids[i : i + batch_size]
            placeholders = ",".join("?" * len(batch))
            self._conn.execute(
                f"""
                UPDATE events
                SET stream_id = ?, assignment_source = 'inferred'
                WHERE id IN ({placeholders})
                """,
                [stream_id] + batch,
            )

    def run_stream_inference(self, gap_threshold_ms: int = 1_800_000) -> int:
        """Run stream inference on all unassigned events.

        Clusters events by working directory and temporal proximity.
        Events within gap_threshold_ms of each other in the same directory
        belong to the same stream.

        Returns the number of events assigned.
        """
        events = self.get_unassigned_events()
        if not events:
            return 0

        # Group events by normalized cwd
        cwd_groups: defaultdict[str, list[dict[str, Any]]] = defaultdict(list)
        for event in events:
            cwd = event["cwd"] or ""
            # Strip trailing slashes but preserve "/" as-is
            normalized_cwd = cwd.rstrip("/") or cwd
            cwd_groups[normalized_cwd].append(event)

        assigned_count = 0

        with self._conn:  # Automatic transaction handling (commits on success)
            for normalized_cwd, group_events in cwd_groups.items():
                # Events already sorted by timestamp from get_unassigned_events()
                # Split into temporal clusters
                clusters: list[list[dict[str, Any]]] = []
                current_cluster: list[dict[str, Any]] = []

                for event in group_events:
                    if not current_cluster:
                        current_cluster.append(event)
                    else:
                        # Calculate gap from previous event
                        prev_ts = datetime.fromisoformat(
                            current_cluster[-1]["timestamp"].replace("Z", "+00:00")
                        )
                        curr_ts = datetime.fromisoformat(
                            event["timestamp"].replace("Z", "+00:00")
                        )
                        gap_ms = (curr_ts - prev_ts).total_seconds() * 1000

                        if gap_ms > gap_threshold_ms:
                            # Start new cluster
                            clusters.append(current_cluster)
                            current_cluster = [event]
                        else:
                            current_cluster.append(event)

                if current_cluster:
                    clusters.append(current_cluster)

                # Create streams and assign events
                for cluster in clusters:
                    # Stream name from cwd: basename, or "/" for root, or "Uncategorized"
                    stream_name = (
                        normalized_cwd.rsplit("/", 1)[-1]
                        or normalized_cwd
                        or "Uncategorized"
                    )

                    stream_id = self.create_stream(name=stream_name, commit=False)
                    event_ids = [e["id"] for e in cluster]
                    self.assign_events_to_stream(event_ids, stream_id)
                    assigned_count += len(event_ids)

        return assigned_count

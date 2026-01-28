"""SQLite event store for Time Tracker."""

from __future__ import annotations

import hashlib
import json
import sqlite3
import uuid
from collections import defaultdict
import logging
from datetime import datetime, timedelta, timezone
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

logger = logging.getLogger(__name__)

# Event types that indicate user activity (reset idle timer)
# Note: afk_change is handled specially - only afk_change(active) resets activity
ACTIVITY_EVENT_TYPES = {"user_message", "tmux_pane_focus", "tmux_scroll"}

# Event types that set focus
FOCUS_EVENT_TYPES = {"user_message", "tmux_pane_focus", "window_focus"}


def _parse_timestamp(ts: str) -> datetime:
    """Parse ISO 8601 timestamp to datetime."""
    return datetime.fromisoformat(ts.replace("Z", "+00:00"))


def _seed_initial_state(
    pre_window_events: list[dict[str, Any]],
    start_dt: datetime,
    session_timeout_ms: int,
) -> dict[str, Any]:
    """Initialize state from events before the query window.

    Finds:
    - Most recent focus event → current_stream
    - Most recent activity event → last_activity
    - Most recent afk_change → is_afk
    - All active sessions (started but not ended)
    """
    state: dict[str, Any] = {
        "current_stream": None,
        "active_sessions": set(),
        "session_last_event": {},
        "last_activity": start_dt,
        "is_afk": False,
        "is_idle": True,  # Default to idle unless we find recent activity
        "previous_stream": None,  # For terminal focus restoration
    }

    if not pre_window_events:
        return state

    # Find most recent focus event
    focus_events = [e for e in pre_window_events if e["type"] in FOCUS_EVENT_TYPES]
    if focus_events:
        latest_focus = max(focus_events, key=lambda e: e["timestamp"])
        if latest_focus["type"] == "window_focus":
            data = json.loads(latest_focus.get("data", "{}") or "{}")
            if data.get("app") != "Terminal":
                state["current_stream"] = None
            # For terminal, we'd need previous_stream but it's not stored
        else:
            state["current_stream"] = latest_focus.get("stream_id")

    # Find most recent activity event → last_activity
    activity_events = [e for e in pre_window_events if e["type"] in ACTIVITY_EVENT_TYPES]
    if activity_events:
        latest_activity = max(activity_events, key=lambda e: e["timestamp"])
        state["last_activity"] = _parse_timestamp(latest_activity["timestamp"])

        # Check if still within attention window
        time_since_activity = (start_dt - state["last_activity"]).total_seconds() * 1000
        state["is_idle"] = time_since_activity > 120_000  # Default attention window

    # Find most recent afk_change → is_afk
    afk_events = [e for e in pre_window_events if e["type"] == "afk_change"]
    if afk_events:
        latest_afk = max(afk_events, key=lambda e: e["timestamp"])
        data = json.loads(latest_afk.get("data", "{}") or "{}")
        state["is_afk"] = data.get("status") == "idle"

    # Build active sessions (started but not ended, not timed out)
    session_starts: dict[str, datetime] = {}
    session_ends: set[str] = set()

    for event in sorted(pre_window_events, key=lambda e: e["timestamp"]):
        if event["type"] == "agent_session":
            data = json.loads(event.get("data", "{}") or "{}")
            sid = event.get("session_id")
            if not sid:
                continue
            if data.get("action") == "started":
                session_starts[sid] = _parse_timestamp(event["timestamp"])
                state["session_last_event"][sid] = _parse_timestamp(event["timestamp"])
            elif data.get("action") == "ended":
                session_ends.add(sid)
                state["session_last_event"].pop(sid, None)
        elif event["type"] == "agent_tool_use":
            sid = event.get("session_id")
            if sid and sid in session_starts:
                state["session_last_event"][sid] = _parse_timestamp(event["timestamp"])

    # Sessions that started but didn't end
    for sid in session_starts:
        if sid not in session_ends:
            # Check if timed out
            last_event = state["session_last_event"].get(sid)
            if last_event:
                time_since = (start_dt - last_event).total_seconds() * 1000
                if time_since <= session_timeout_ms:
                    state["active_sessions"].add(sid)

    return state


def _insert_idle_boundaries(
    events: list[dict[str, Any]],
    state: dict[str, Any],
    attention_window_ms: int,
    end_dt: datetime,
) -> list[dict[str, Any]]:
    """Insert synthetic _idle_start events where idle begins.

    When there's a gap longer than attention_window_ms after an activity event,
    insert a synthetic event marking when idle started.

    The algorithm tracks the "pending idle time" from the last activity event
    and inserts the idle marker once we're past that time.
    """
    result: list[dict[str, Any]] = []

    # Track when we'll become idle (from last activity event)
    pending_idle_at: datetime | None = None

    # Initialize from pre-window state
    if not state["is_idle"] and state["last_activity"]:
        pending_idle_at = state["last_activity"] + timedelta(milliseconds=attention_window_ms)

    for event in events:
        event_ts = _parse_timestamp(event["timestamp"])

        # Check if we should insert idle marker before this event
        if pending_idle_at and event_ts >= pending_idle_at:
            result.append({
                "type": "_idle_start",
                "timestamp": pending_idle_at.strftime("%Y-%m-%dT%H:%M:%SZ"),
            })
            pending_idle_at = None

        result.append(event)

        # Activity events reset the idle timer
        # Note: afk_change does NOT reset activity - user must actively do something
        if event["type"] in ACTIVITY_EVENT_TYPES:
            pending_idle_at = event_ts + timedelta(milliseconds=attention_window_ms)

    # Check if we should insert idle marker at the end
    if pending_idle_at and pending_idle_at <= end_dt:
        result.append({
            "type": "_idle_start",
            "timestamp": pending_idle_at.strftime("%Y-%m-%dT%H:%M:%SZ"),
        })

    return result


def _insert_session_timeouts(
    events: list[dict[str, Any]],
    state: dict[str, Any],
    session_timeout_ms: int,
    end_dt: datetime,
) -> list[dict[str, Any]]:
    """Insert synthetic _session_timeout events where sessions become stale.

    For each active session, if there's no event for session_timeout_ms,
    insert a synthetic event marking when the session times out.
    """
    result: list[dict[str, Any]] = []

    # Track when each session will timeout
    session_timeout_at: dict[str, datetime] = {}

    # Initialize from pre-window state
    for sid in state["active_sessions"]:
        last_event = state["session_last_event"].get(sid)
        if last_event:
            session_timeout_at[sid] = last_event + timedelta(milliseconds=session_timeout_ms)

    for event in events:
        event_ts = _parse_timestamp(event["timestamp"])

        # Check if any sessions should timeout before this event
        for sid in list(session_timeout_at.keys()):
            timeout_at = session_timeout_at[sid]
            if event_ts >= timeout_at:
                result.append({
                    "type": "_session_timeout",
                    "timestamp": timeout_at.strftime("%Y-%m-%dT%H:%M:%SZ"),
                    "session_id": sid,
                })
                del session_timeout_at[sid]

        result.append(event)

        # Session events update timeout tracking
        if event["type"] == "agent_session":
            data = json.loads(event.get("data", "{}") or "{}")
            sid = event.get("session_id")
            if sid:
                if data.get("action") == "started":
                    session_timeout_at[sid] = event_ts + timedelta(milliseconds=session_timeout_ms)
                elif data.get("action") == "ended":
                    session_timeout_at.pop(sid, None)
        elif event["type"] == "agent_tool_use":
            sid = event.get("session_id")
            if sid and sid in session_timeout_at:
                session_timeout_at[sid] = event_ts + timedelta(milliseconds=session_timeout_ms)

    # Insert timeout events for sessions that timeout before end_dt
    for sid, timeout_at in session_timeout_at.items():
        if timeout_at <= end_dt:
            result.append({
                "type": "_session_timeout",
                "timestamp": timeout_at.strftime("%Y-%m-%dT%H:%M:%SZ"),
                "session_id": sid,
            })

    return result


def _prune_stale_sessions(
    state: dict[str, Any],
    current_ts: datetime,
    session_timeout_ms: int,
) -> None:
    """Remove sessions that haven't had events for session_timeout_ms."""
    stale = []
    for sid in state["active_sessions"]:
        last_event = state["session_last_event"].get(sid)
        if last_event:
            time_since = (current_ts - last_event).total_seconds() * 1000
            if time_since > session_timeout_ms:
                stale.append(sid)
    for sid in stale:
        state["active_sessions"].discard(sid)
        state["session_last_event"].pop(sid, None)


def _apply_transition(
    state: dict[str, Any],
    event: dict[str, Any],
    session_stream_map: dict[str, str],
) -> None:
    """Apply state transition for an event."""
    event_type = event["type"]
    event_ts = _parse_timestamp(event["timestamp"])

    if event_type == "_idle_start":
        state["is_idle"] = True
        return

    if event_type == "_session_timeout":
        # Synthetic event: session timed out
        sid = event.get("session_id")
        if sid:
            state["active_sessions"].discard(sid)
            state["session_last_event"].pop(sid, None)
        return

    # Activity events reset idle and update last_activity
    if event_type in ACTIVITY_EVENT_TYPES:
        state["is_idle"] = False
        state["last_activity"] = event_ts

    if event_type == "tmux_pane_focus":
        # Focus shifts to the event's stream
        stream_id = event.get("stream_id")
        state["previous_stream"] = state["current_stream"]
        state["current_stream"] = stream_id

    elif event_type == "window_focus":
        data = json.loads(event.get("data", "{}") or "{}")
        if data.get("app") == "Terminal":
            # Restore previous stream if available
            if state["previous_stream"]:
                state["current_stream"] = state["previous_stream"]
            # else: current_stream stays as-is (could be None)
        else:
            # Non-terminal focus: no stream gets direct time
            state["previous_stream"] = state["current_stream"]
            state["current_stream"] = None

    elif event_type == "tmux_scroll":
        # Confirms current stream, updates last_activity (already done above)
        pass

    elif event_type == "user_message":
        # Sets focus to the message's session's stream
        session_id = event.get("session_id")
        if session_id:
            stream = session_stream_map.get(session_id) or event.get("stream_id")
            if stream:
                state["previous_stream"] = state["current_stream"]
                state["current_stream"] = stream

    elif event_type == "agent_session":
        data = json.loads(event.get("data", "{}") or "{}")
        session_id = event.get("session_id")
        if not session_id:
            return

        if data.get("action") == "started":
            state["active_sessions"].add(session_id)
            state["session_last_event"][session_id] = event_ts
        elif data.get("action") == "ended":
            state["active_sessions"].discard(session_id)
            state["session_last_event"].pop(session_id, None)

    elif event_type == "agent_tool_use":
        session_id = event.get("session_id")
        if session_id and session_id in state["active_sessions"]:
            state["session_last_event"][session_id] = event_ts

    elif event_type == "afk_change":
        data = json.loads(event.get("data", "{}") or "{}")
        if data.get("status") == "idle":
            state["is_afk"] = True
        else:
            state["is_afk"] = False
            # Note: returning from AFK does NOT reset is_idle or last_activity.
            # The user needs to actually do something (focus, message, scroll)
            # to start accruing direct time again.


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

    def calculate_time(
        self,
        start: str,
        end: str,
        *,
        attention_window_ms: int = 120_000,
        session_timeout_ms: int = 1_800_000,
    ) -> dict[str, dict[str, int]]:
        """Calculate direct and delegated time per stream for a time range.

        Direct time is when the user actively attends to a stream.
        Delegated time is when an agent works on a stream.

        Args:
            start: ISO 8601 timestamp (inclusive)
            end: ISO 8601 timestamp (inclusive)
            attention_window_ms: Grace period after last activity before idle (default 2 min)
            session_timeout_ms: Max gap before session considered stale (default 30 min)

        Returns:
            Dict mapping stream_id to {"direct_ms": int, "delegated_ms": int}
        """
        # Parse time range
        start_dt = _parse_timestamp(start)
        end_dt = _parse_timestamp(end)

        # Batch-load session → stream mapping
        session_stream_map = self._load_session_stream_map()

        # Fetch events with lookback for seeding initial state
        lookback_ms = max(attention_window_ms, session_timeout_ms)
        lookback_dt = start_dt - timedelta(milliseconds=lookback_ms)
        lookback_start = lookback_dt.strftime("%Y-%m-%dT%H:%M:%SZ")

        events = self.get_events(start=lookback_start, end=end)

        # Split into pre-window (for seeding) and window events
        pre_window = [e for e in events if _parse_timestamp(e["timestamp"]) < start_dt]
        window_events = [
            e for e in events if start_dt <= _parse_timestamp(e["timestamp"]) <= end_dt
        ]

        # Seed initial state from pre-window events
        state = _seed_initial_state(pre_window, start_dt, session_timeout_ms)

        # Insert synthetic idle-start events
        window_events = _insert_idle_boundaries(
            window_events, state, attention_window_ms, end_dt
        )

        # Insert synthetic session-timeout events
        window_events = _insert_session_timeouts(
            window_events, state, session_timeout_ms, end_dt
        )

        # Sort by timestamp, with user_message last at same timestamp
        window_events.sort(key=lambda e: (e["timestamp"], e["type"] == "user_message"))

        # Single-pass time accumulation
        results: dict[str, dict[str, int]] = defaultdict(
            lambda: {"direct_ms": 0, "delegated_ms": 0}
        )
        last_ts = start_dt

        for event in window_events:
            event_ts = _parse_timestamp(event["timestamp"])
            interval_ms = int((event_ts - last_ts).total_seconds() * 1000)

            # Prune stale sessions before attribution
            _prune_stale_sessions(state, last_ts, session_timeout_ms)

            # Direct time: not AFK, not idle, have focus
            if (
                not state["is_afk"]
                and not state["is_idle"]
                and state["current_stream"]
            ):
                results[state["current_stream"]]["direct_ms"] += interval_ms

            # Delegated time: all active sessions
            for sid in state["active_sessions"]:
                stream = session_stream_map.get(sid)
                if stream:
                    results[stream]["delegated_ms"] += interval_ms

            # Apply state transition
            _apply_transition(state, event, session_stream_map)
            last_ts = event_ts

        # Final interval to end
        final_interval_ms = int((end_dt - last_ts).total_seconds() * 1000)
        if final_interval_ms > 0:
            _prune_stale_sessions(state, last_ts, session_timeout_ms)

            if (
                not state["is_afk"]
                and not state["is_idle"]
                and state["current_stream"]
            ):
                results[state["current_stream"]]["direct_ms"] += final_interval_ms

            for sid in state["active_sessions"]:
                stream = session_stream_map.get(sid)
                if stream:
                    results[stream]["delegated_ms"] += final_interval_ms

        return dict(results)

    def _load_session_stream_map(self) -> dict[str, str]:
        """Load mapping from session_id to stream_id.

        For each session, returns the stream_id from the earliest event
        that has both session_id and stream_id set.
        """
        cursor = self._conn.execute("""
            SELECT e.session_id, e.stream_id
            FROM events e
            INNER JOIN (
                SELECT session_id, MIN(timestamp) as min_ts
                FROM events
                WHERE session_id IS NOT NULL AND stream_id IS NOT NULL
                GROUP BY session_id
            ) m ON e.session_id = m.session_id AND e.timestamp = m.min_ts
            WHERE e.stream_id IS NOT NULL
        """)
        return {row["session_id"]: row["stream_id"] for row in cursor.fetchall()}

    def get_stream_tags(
        self, stream_ids: list[str] | None = None
    ) -> dict[str, list[str]]:
        """Get tags for specified streams (or all if None).

        Args:
            stream_ids: List of stream IDs to filter by, or None for all.

        Returns:
            Dict mapping stream_id to list of tags.
        """
        result: dict[str, list[str]] = defaultdict(list)

        if stream_ids:
            # Batch queries to stay under SQLite's 999-parameter limit
            batch_size = 500
            for i in range(0, len(stream_ids), batch_size):
                batch = stream_ids[i : i + batch_size]
                placeholders = ",".join("?" * len(batch))
                cursor = self._conn.execute(
                    f"SELECT stream_id, tag FROM stream_tags WHERE stream_id IN ({placeholders})",
                    batch,
                )
                for row in cursor:
                    result[row["stream_id"]].append(row["tag"])
        else:
            cursor = self._conn.execute("SELECT stream_id, tag FROM stream_tags")
            for row in cursor:
                result[row["stream_id"]].append(row["tag"])

        return dict(result)

    def count_days_with_data(self, start: str, end: str) -> int:
        """Count distinct local-time days with at least one event.

        Args:
            start: ISO 8601 timestamp (inclusive lower bound)
            end: ISO 8601 timestamp (exclusive upper bound)

        Returns:
            Number of distinct days with events.
        """
        cursor = self._conn.execute(
            """
            SELECT DISTINCT DATE(timestamp, 'localtime')
            FROM events
            WHERE timestamp >= ? AND timestamp < ?
            """,
            (start, end),
        )
        return len(cursor.fetchall())

    def add_tag(self, stream_id: str, tag: str) -> bool:
        """Add a tag to a stream.

        Args:
            stream_id: The stream ID.
            tag: The tag to add.

        Returns:
            True if tag was added, False if it already existed.
        """
        try:
            self._conn.execute(
                "INSERT INTO stream_tags (stream_id, tag) VALUES (?, ?)",
                (stream_id, tag),
            )
            self._conn.commit()
            return True
        except sqlite3.IntegrityError:
            # Tag already exists (PRIMARY KEY violation)
            return False

    def remove_tag(self, stream_id: str, tag: str) -> bool:
        """Remove a tag from a stream.

        Args:
            stream_id: The stream ID.
            tag: The tag to remove.

        Returns:
            True if tag was removed, False if it didn't exist.
        """
        cursor = self._conn.execute(
            "DELETE FROM stream_tags WHERE stream_id = ? AND tag = ?",
            (stream_id, tag),
        )
        self._conn.commit()
        return cursor.rowcount > 0

    def get_top_tags(self, limit: int = 15) -> list[tuple[str, int]]:
        """Get most-used tags with their stream counts.

        Args:
            limit: Maximum number of tags to return.

        Returns:
            List of (tag, count) tuples, ordered by count descending.
        """
        cursor = self._conn.execute(
            """
            SELECT tag, COUNT(*) as count
            FROM stream_tags
            GROUP BY tag
            ORDER BY count DESC
            LIMIT ?
            """,
            (limit,),
        )
        return [(row["tag"], row["count"]) for row in cursor.fetchall()]

    def get_untagged_streams(self) -> list[dict[str, Any]]:
        """Get streams that have no tags.

        Returns:
            List of stream dicts without tags.
        """
        cursor = self._conn.execute(
            """
            SELECT s.*
            FROM streams s
            LEFT JOIN stream_tags st ON s.id = st.stream_id
            WHERE st.stream_id IS NULL
            ORDER BY s.updated_at DESC
            """
        )
        return [dict(row) for row in cursor.fetchall()]

    def get_stream_by_prefix(self, prefix: str) -> dict[str, Any] | None:
        """Find a stream by ID prefix.

        Args:
            prefix: The ID prefix to match.

        Returns:
            Stream dict if exactly one match, None if no match.

        Raises:
            ValueError: If prefix matches multiple streams.
        """
        # Escape LIKE metacharacters to prevent pattern injection
        escaped = prefix.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_")
        cursor = self._conn.execute(
            "SELECT * FROM streams WHERE id LIKE ? ESCAPE '\\'",
            (escaped + "%",),
        )
        rows = cursor.fetchall()
        if len(rows) == 0:
            return None
        if len(rows) > 1:
            ids = [row["id"][:7] for row in rows]
            raise ValueError(f"Ambiguous prefix '{prefix}' matches: {', '.join(ids)}")
        return dict(rows[0])

    def get_stream_events(
        self,
        stream_id: str,
        *,
        limit: int | None = None,
    ) -> list[dict[str, Any]]:
        """Get events for a specific stream.

        Args:
            stream_id: The stream ID.
            limit: Maximum number of events to return.

        Returns:
            List of event dicts ordered by timestamp ascending.
        """
        query = "SELECT * FROM events WHERE stream_id = ? ORDER BY timestamp ASC"
        params: list[str | int] = [stream_id]
        if limit is not None:
            query += " LIMIT ?"
            params.append(limit)
        cursor = self._conn.execute(query, params)
        return [dict(row) for row in cursor.fetchall()]

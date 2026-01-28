"""Tests for SQLite event store."""

from datetime import datetime, timezone

import pytest

from tt_local.db import EventStore, ImportedEvent, RawEvent


def make_event(
    *,
    event_type: str = "tmux_pane_focus",
    source: str = "remote.tmux",
    timestamp: str | None = None,
    data: dict | None = None,
    cwd: str | None = "/home/test/project",
    session_id: str | None = None,
) -> RawEvent:
    """Helper to create a RawEvent for testing."""
    if timestamp is None:
        timestamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    if data is None:
        data = {"pane_id": "%1", "session_name": "dev", "window_index": 0, "cwd": cwd}
    return RawEvent(
        timestamp=timestamp,
        type=event_type,
        source=source,
        data=data,
        cwd=cwd,
        session_id=session_id,
    )


class TestEventStoreCreation:
    """Tests for database creation and schema initialization."""

    def test_create_database_in_memory(self):
        """Verify schema initialization creates all tables."""
        store = EventStore.open_in_memory()
        # Schema should be created - verify by inserting an event
        event = make_event()
        store.insert_event(event)
        events = store.get_events()
        assert len(events) == 1


class TestEventInsert:
    """Tests for inserting events."""

    def test_different_cwd_produces_different_id(self):
        """Events with same data but different cwd should have different IDs."""
        event1 = make_event(
            timestamp="2025-01-25T10:00:00Z",
            cwd="/home/sami/project-a",
        )
        event2 = make_event(
            timestamp="2025-01-25T10:00:00Z",
            cwd="/home/sami/project-b",
        )
        assert event1.compute_id() != event2.compute_id()

    def test_different_session_id_produces_different_id(self):
        """Events with same data but different session_id should have different IDs."""
        event1 = make_event(
            timestamp="2025-01-25T10:00:00Z",
            session_id="session-a",
        )
        event2 = make_event(
            timestamp="2025-01-25T10:00:00Z",
            session_id="session-b",
        )
        assert event1.compute_id() != event2.compute_id()

    def test_insert_event(self):
        """Insert and retrieve a single event."""
        store = EventStore.open_in_memory()
        event = make_event(
            timestamp="2025-01-25T10:00:00Z",
            event_type="tmux_pane_focus",
            cwd="/home/sami/project-x",
        )
        store.insert_event(event)

        events = store.get_events()
        assert len(events) == 1
        retrieved = events[0]
        assert retrieved["type"] == "tmux_pane_focus"
        assert retrieved["source"] == "remote.tmux"
        assert retrieved["cwd"] == "/home/sami/project-x"
        assert retrieved["timestamp"] == "2025-01-25T10:00:00Z"

    def test_insert_duplicate_event(self):
        """Verify idempotent insert - same ID = no-op."""
        store = EventStore.open_in_memory()
        event = make_event(timestamp="2025-01-25T10:00:00Z")

        # Insert twice
        store.insert_event(event)
        store.insert_event(event)

        # Should only have one event
        events = store.get_events()
        assert len(events) == 1


class TestEventQuery:
    """Tests for querying events."""

    def test_query_events_by_time_range(self):
        """Filter events by timestamp range."""
        store = EventStore.open_in_memory()

        # Insert events at different times
        store.insert_event(make_event(timestamp="2025-01-25T09:00:00Z"))
        store.insert_event(make_event(timestamp="2025-01-25T10:00:00Z"))
        store.insert_event(make_event(timestamp="2025-01-25T11:00:00Z"))
        store.insert_event(make_event(timestamp="2025-01-25T12:00:00Z"))

        # Query middle range
        events = store.get_events(
            start="2025-01-25T09:30:00Z",
            end="2025-01-25T11:30:00Z",
        )
        assert len(events) == 2
        timestamps = [e["timestamp"] for e in events]
        assert "2025-01-25T10:00:00Z" in timestamps
        assert "2025-01-25T11:00:00Z" in timestamps


class TestForeignKeyConstraint:
    """Tests for foreign key behavior."""

    def test_foreign_key_constraint(self):
        """Insert event with stream_id, verify FK enforcement."""
        store = EventStore.open_in_memory()

        # Create a stream first
        stream_id = store.create_stream(name="Test Stream")

        # Insert event referencing the stream
        event = make_event()
        store.insert_event(event, stream_id=stream_id)

        events = store.get_events()
        assert len(events) == 1
        assert events[0]["stream_id"] == stream_id

    def test_foreign_key_rejects_invalid_stream(self):
        """Inserting event with non-existent stream_id should fail."""
        store = EventStore.open_in_memory()

        event = make_event()
        with pytest.raises(Exception):  # sqlite3.IntegrityError
            store.insert_event(event, stream_id="nonexistent-stream-id")


class TestImportedEvent:
    """Tests for importing events from remote export."""

    def test_insert_imported_event(self):
        """Insert and retrieve an imported event."""
        store = EventStore.open_in_memory()
        event = ImportedEvent(
            id="abc123",
            timestamp="2025-01-25T10:00:00Z",
            type="tmux_pane_focus",
            source="remote.tmux",
            data={"pane_id": "%1"},
            cwd="/home/sami/project",
        )

        inserted = store.insert_imported_event(event)
        assert inserted is True

        events = store.get_events()
        assert len(events) == 1
        retrieved = events[0]
        assert retrieved["id"] == "abc123"
        assert retrieved["type"] == "tmux_pane_focus"
        assert retrieved["source"] == "remote.tmux"
        assert retrieved["cwd"] == "/home/sami/project"
        assert retrieved["assignment_source"] == "imported"

    def test_insert_imported_event_without_cwd(self):
        """Import event without cwd field."""
        store = EventStore.open_in_memory()
        event = ImportedEvent(
            id="def456",
            timestamp="2025-01-25T10:00:00Z",
            type="agent_task_start",
            source="remote.agent",
            data={"session_id": "sess-123"},
        )

        inserted = store.insert_imported_event(event)
        assert inserted is True

        events = store.get_events()
        assert len(events) == 1
        assert events[0]["cwd"] is None

    def test_insert_imported_event_duplicate(self):
        """Duplicate imported events are silently skipped."""
        store = EventStore.open_in_memory()
        event = ImportedEvent(
            id="abc123",
            timestamp="2025-01-25T10:00:00Z",
            type="tmux_pane_focus",
            source="remote.tmux",
            data={"pane_id": "%1"},
        )

        # First insert succeeds
        inserted1 = store.insert_imported_event(event)
        assert inserted1 is True

        # Second insert returns False (duplicate)
        inserted2 = store.insert_imported_event(event)
        assert inserted2 is False

        # Still only one event
        events = store.get_events()
        assert len(events) == 1

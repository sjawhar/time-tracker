"""Tests for stream inference."""

import time

from tt_local.db import EventStore


def insert_event(
    store: EventStore,
    *,
    event_id: str,
    timestamp: str,
    cwd: str | None = "/home/test/project",
    assignment_source: str = "imported",
    stream_id: str | None = None,
) -> None:
    """Helper to insert a test event with specific assignment_source."""
    # Use the underlying connection to insert directly with full control
    store._conn.execute(
        """
        INSERT INTO events
        (id, timestamp, type, source, schema_version, data, cwd, session_id, stream_id, assignment_source)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        (
            event_id,
            timestamp,
            "tmux_pane_focus",
            "remote.tmux",
            1,
            "{}",
            cwd,
            None,
            stream_id,
            assignment_source,
        ),
    )
    store._conn.commit()


class TestStreamInferenceBasic:
    """Basic stream inference tests."""

    def test_empty_events_is_noop(self):
        """run_stream_inference() with no events returns 0."""
        store = EventStore.open_in_memory()
        count = store.run_stream_inference()
        assert count == 0

    def test_single_event_creates_one_stream(self):
        """One event creates one stream."""
        store = EventStore.open_in_memory()
        insert_event(store, event_id="e1", timestamp="2025-01-25T10:00:00Z")

        count = store.run_stream_inference()
        assert count == 1

        events = store.get_events()
        assert events[0]["stream_id"] is not None

    def test_same_cwd_within_gap_same_stream(self):
        """Two events 15 min apart → one stream."""
        store = EventStore.open_in_memory()
        insert_event(store, event_id="e1", timestamp="2025-01-25T10:00:00Z")
        insert_event(store, event_id="e2", timestamp="2025-01-25T10:15:00Z")

        count = store.run_stream_inference()
        assert count == 2

        events = store.get_events()
        assert events[0]["stream_id"] == events[1]["stream_id"]

    def test_same_cwd_exceeds_gap_two_streams(self):
        """Two events 45 min apart → two streams."""
        store = EventStore.open_in_memory()
        insert_event(store, event_id="e1", timestamp="2025-01-25T10:00:00Z")
        insert_event(store, event_id="e2", timestamp="2025-01-25T10:45:00Z")

        count = store.run_stream_inference()
        assert count == 2

        events = store.get_events()
        assert events[0]["stream_id"] != events[1]["stream_id"]

    def test_gap_boundary_exactly_30_min_same_stream(self):
        """Events exactly 30 min apart → same stream (gap > threshold, not >=)."""
        store = EventStore.open_in_memory()
        insert_event(store, event_id="e1", timestamp="2025-01-25T10:00:00Z")
        insert_event(store, event_id="e2", timestamp="2025-01-25T10:30:00Z")

        count = store.run_stream_inference()
        assert count == 2

        events = store.get_events()
        assert events[0]["stream_id"] == events[1]["stream_id"]

    def test_different_cwd_same_time_separate_streams(self):
        """Events in different cwds → separate streams."""
        store = EventStore.open_in_memory()
        insert_event(
            store, event_id="e1", timestamp="2025-01-25T10:00:00Z", cwd="/home/test/project-a"
        )
        insert_event(
            store, event_id="e2", timestamp="2025-01-25T10:00:00Z", cwd="/home/test/project-b"
        )

        count = store.run_stream_inference()
        assert count == 2

        events = store.get_events()
        assert events[0]["stream_id"] != events[1]["stream_id"]


class TestUserAssignments:
    """Tests for preserving user-assigned events."""

    def test_user_assigned_events_preserved(self):
        """Events with assignment_source='user' keep their stream_id."""
        store = EventStore.open_in_memory()
        stream_id = store.create_stream(name="User Stream")

        insert_event(
            store,
            event_id="e1",
            timestamp="2025-01-25T10:00:00Z",
            assignment_source="user",
            stream_id=stream_id,
        )

        count = store.run_stream_inference()
        assert count == 0  # No events to process

        events = store.get_events()
        assert events[0]["stream_id"] == stream_id
        assert events[0]["assignment_source"] == "user"

    def test_imported_events_get_inferred(self):
        """Events with assignment_source='imported' are eligible for inference."""
        store = EventStore.open_in_memory()
        insert_event(
            store, event_id="e1", timestamp="2025-01-25T10:00:00Z", assignment_source="imported"
        )

        count = store.run_stream_inference()
        assert count == 1

        events = store.get_events()
        assert events[0]["stream_id"] is not None
        assert events[0]["assignment_source"] == "inferred"


class TestCwdHandling:
    """Tests for cwd edge cases."""

    def test_null_cwd_uncategorized_stream(self):
        """Events without cwd go to 'Uncategorized' stream."""
        store = EventStore.open_in_memory()
        insert_event(store, event_id="e1", timestamp="2025-01-25T10:00:00Z", cwd=None)

        count = store.run_stream_inference()
        assert count == 1

        events = store.get_events()
        stream_id = events[0]["stream_id"]
        streams = store.get_streams()
        stream = next(s for s in streams if s["id"] == stream_id)
        assert stream["name"] == "Uncategorized"

    def test_empty_string_cwd_uncategorized_stream(self):
        """Events with cwd='' go to 'Uncategorized' stream (same as null)."""
        store = EventStore.open_in_memory()
        insert_event(store, event_id="e1", timestamp="2025-01-25T10:00:00Z", cwd="")

        count = store.run_stream_inference()
        assert count == 1

        events = store.get_events()
        stream_id = events[0]["stream_id"]
        streams = store.get_streams()
        stream = next(s for s in streams if s["id"] == stream_id)
        assert stream["name"] == "Uncategorized"


class TestStreamNaming:
    """Tests for auto-generated stream names."""

    def test_stream_name_from_cwd_basename(self):
        """Stream name is the basename of the cwd."""
        store = EventStore.open_in_memory()
        insert_event(
            store, event_id="e1", timestamp="2025-01-25T10:00:00Z", cwd="/home/sami/time-tracker"
        )

        store.run_stream_inference()

        events = store.get_events()
        stream_id = events[0]["stream_id"]
        streams = store.get_streams()
        stream = next(s for s in streams if s["id"] == stream_id)
        assert stream["name"] == "time-tracker"

    def test_same_basename_different_cwds_separate_streams(self):
        """/home/a/project and /home/b/project → separate streams (both named 'project')."""
        store = EventStore.open_in_memory()
        insert_event(
            store, event_id="e1", timestamp="2025-01-25T10:00:00Z", cwd="/home/a/project"
        )
        insert_event(
            store, event_id="e2", timestamp="2025-01-25T10:00:00Z", cwd="/home/b/project"
        )

        count = store.run_stream_inference()
        assert count == 2

        events = store.get_events()
        assert events[0]["stream_id"] != events[1]["stream_id"]

        # Both streams named "project"
        streams = store.get_streams()
        names = [s["name"] for s in streams]
        assert names == ["project", "project"]


class TestPathNormalization:
    """Tests for path normalization."""

    def test_trailing_slash_normalized(self):
        """/home/sami/project/ and /home/sami/project → same stream."""
        store = EventStore.open_in_memory()
        insert_event(
            store, event_id="e1", timestamp="2025-01-25T10:00:00Z", cwd="/home/sami/project/"
        )
        insert_event(
            store, event_id="e2", timestamp="2025-01-25T10:05:00Z", cwd="/home/sami/project"
        )

        count = store.run_stream_inference()
        assert count == 2

        events = store.get_events()
        assert events[0]["stream_id"] == events[1]["stream_id"]

    def test_root_directory_cwd(self):
        """cwd='/' should have stream name '/'."""
        store = EventStore.open_in_memory()
        insert_event(store, event_id="e1", timestamp="2025-01-25T10:00:00Z", cwd="/")

        store.run_stream_inference()

        events = store.get_events()
        stream_id = events[0]["stream_id"]
        streams = store.get_streams()
        stream = next(s for s in streams if s["id"] == stream_id)
        assert stream["name"] == "/"


class TestIdempotence:
    """Tests for idempotent behavior."""

    def test_running_inference_twice_no_duplicates(self):
        """Running inference twice assigns same events to same streams."""
        store = EventStore.open_in_memory()
        insert_event(store, event_id="e1", timestamp="2025-01-25T10:00:00Z")

        count1 = store.run_stream_inference()
        assert count1 == 1

        # Running again should be a no-op
        count2 = store.run_stream_inference()
        assert count2 == 0

        # Still only one stream
        streams = store.get_streams()
        assert len(streams) == 1

    def test_events_already_assigned_not_reprocessed(self):
        """Events with stream_id set and assignment_source='inferred' are not re-processed."""
        store = EventStore.open_in_memory()
        stream_id = store.create_stream(name="Existing Stream")
        insert_event(
            store,
            event_id="e1",
            timestamp="2025-01-25T10:00:00Z",
            assignment_source="inferred",
            stream_id=stream_id,
        )

        count = store.run_stream_inference()
        assert count == 0

        events = store.get_events()
        assert events[0]["stream_id"] == stream_id


class TestTemporalClustering:
    """Tests for temporal clustering behavior."""

    def test_three_events_in_sequence_same_stream(self):
        """Events at 9:00, 9:15, 9:30 (all within gap) → same stream."""
        store = EventStore.open_in_memory()
        insert_event(store, event_id="e1", timestamp="2025-01-25T09:00:00Z")
        insert_event(store, event_id="e2", timestamp="2025-01-25T09:15:00Z")
        insert_event(store, event_id="e3", timestamp="2025-01-25T09:30:00Z")

        count = store.run_stream_inference()
        assert count == 3

        events = store.get_events()
        stream_ids = {e["stream_id"] for e in events}
        assert len(stream_ids) == 1

    def test_across_midnight_same_stream(self):
        """11:59 PM and 12:01 AM in same cwd → same stream (2 min gap)."""
        store = EventStore.open_in_memory()
        insert_event(store, event_id="e1", timestamp="2025-01-25T23:59:00Z")
        insert_event(store, event_id="e2", timestamp="2025-01-26T00:01:00Z")

        count = store.run_stream_inference()
        assert count == 2

        events = store.get_events()
        assert events[0]["stream_id"] == events[1]["stream_id"]


class TestSpecialPaths:
    """Tests for special path cases."""

    def test_unicode_paths(self):
        """/home/sami/proyecto-espanol → stream name is 'proyecto-espanol'."""
        store = EventStore.open_in_memory()
        insert_event(
            store,
            event_id="e1",
            timestamp="2025-01-25T10:00:00Z",
            cwd="/home/sami/proyecto-español",
        )

        store.run_stream_inference()

        events = store.get_events()
        stream_id = events[0]["stream_id"]
        streams = store.get_streams()
        stream = next(s for s in streams if s["id"] == stream_id)
        assert stream["name"] == "proyecto-español"

    def test_deeply_nested_paths(self):
        """/home/sami/very/deep/structure/project → stream name is 'project'."""
        store = EventStore.open_in_memory()
        insert_event(
            store,
            event_id="e1",
            timestamp="2025-01-25T10:00:00Z",
            cwd="/home/sami/very/deep/structure/project",
        )

        store.run_stream_inference()

        events = store.get_events()
        stream_id = events[0]["stream_id"]
        streams = store.get_streams()
        stream = next(s for s in streams if s["id"] == stream_id)
        assert stream["name"] == "project"


class TestPerformance:
    """Performance tests."""

    def test_10000_events_under_1_second(self):
        """10,000 events in <1s."""
        store = EventStore.open_in_memory()

        # Insert 10,000 events across 10 different cwds
        # Events are 1 minute apart within each cwd group
        for i in range(10000):
            cwd_index = i % 10
            # Within each cwd, events are sequential by minute
            event_index_in_cwd = i // 10
            total_minutes = event_index_in_cwd
            day = 25 + (total_minutes // 1440)  # Roll over days
            minute_of_day = total_minutes % 1440
            hour = minute_of_day // 60
            minute = minute_of_day % 60
            timestamp = f"2025-01-{day:02d}T{hour:02d}:{minute:02d}:00Z"

            insert_event(
                store,
                event_id=f"e{i}",
                timestamp=timestamp,
                cwd=f"/home/test/project-{cwd_index}",
            )

        # Time the inference
        start = time.perf_counter()
        count = store.run_stream_inference()
        elapsed = time.perf_counter() - start

        assert count == 10000
        assert elapsed < 1.0, f"Stream inference took {elapsed:.2f}s, expected <1s"

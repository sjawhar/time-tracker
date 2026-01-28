"""Tests for direct/delegated time calculation."""

from datetime import datetime, timezone
from typing import Any

import pytest

from tt_local.db import EventStore


def insert_event(
    store: EventStore,
    *,
    event_id: str,
    timestamp: str,
    event_type: str = "tmux_pane_focus",
    source: str = "remote.tmux",
    data: dict[str, Any] | None = None,
    cwd: str | None = "/home/test/project",
    session_id: str | None = None,
    stream_id: str | None = None,
    assignment_source: str = "inferred",
) -> None:
    """Helper to insert a test event with full control over all fields."""
    import json

    data_json = json.dumps(data) if data else "{}"
    store._conn.execute(
        """
        INSERT INTO events
        (id, timestamp, type, source, schema_version, data, cwd, session_id, stream_id, assignment_source)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """,
        (
            event_id,
            timestamp,
            event_type,
            source,
            1,
            data_json,
            cwd,
            session_id,
            stream_id,
            assignment_source,
        ),
    )
    store._conn.commit()


class TestBasicTimeCalculation:
    """Basic time calculation scenarios."""

    def test_empty_time_range_returns_empty(self):
        """Empty time range with no events returns empty dict."""
        store = EventStore.open_in_memory()
        result = store.calculate_time("2025-01-25T10:00:00Z", "2025-01-25T11:00:00Z")
        assert result == {}

    def test_single_focus_event_direct_time(self):
        """Single focus event gives direct time up to attention window."""
        store = EventStore.open_in_memory()
        stream_id = store.create_stream(name="project")
        insert_event(
            store,
            event_id="e1",
            timestamp="2025-01-25T10:00:00Z",
            event_type="tmux_pane_focus",
            stream_id=stream_id,
        )

        # Query for 5 minutes - direct time capped at attention window (2 min)
        result = store.calculate_time("2025-01-25T10:00:00Z", "2025-01-25T10:05:00Z")

        assert stream_id in result
        assert result[stream_id]["direct_ms"] == 120_000  # 2 minutes
        assert result[stream_id]["delegated_ms"] == 0

    def test_direct_time_stops_at_attention_window(self):
        """Direct time stops after attention window even if window extends further."""
        store = EventStore.open_in_memory()
        stream_id = store.create_stream(name="project")
        insert_event(
            store,
            event_id="e1",
            timestamp="2025-01-25T10:00:00Z",
            event_type="tmux_pane_focus",
            stream_id=stream_id,
        )

        # Query for 10 minutes - direct time still capped at 2 min
        result = store.calculate_time("2025-01-25T10:00:00Z", "2025-01-25T10:10:00Z")

        assert result[stream_id]["direct_ms"] == 120_000  # Still 2 minutes


class TestSpecExample1:
    """Test Example 1 from spec: Single Agent Session.

    10:00:00  user_message(session=A, stream=S1)     → focus=S1, active={A}
    10:00:30  agent_tool_use(session=A)              → (no change)
    10:05:00  agent_session(A, ended)                → active={}

    Result for S1:
      direct_ms  = 120000  (10:00:00 to 10:02:00, then idle timeout)
      delegated_ms = 300000  (10:00:00 to 10:05:00)
    """

    def test_single_agent_session(self):
        store = EventStore.open_in_memory()
        stream_id = store.create_stream(name="S1")

        # user_message: sets focus and starts activity
        insert_event(
            store,
            event_id="e1",
            timestamp="2025-01-25T10:00:00Z",
            event_type="user_message",
            session_id="session-A",
            stream_id=stream_id,
        )

        # agent_session started (must be before or at user_message for session to be active)
        insert_event(
            store,
            event_id="e0",
            timestamp="2025-01-25T10:00:00Z",
            event_type="agent_session",
            data={"action": "started"},
            session_id="session-A",
            stream_id=stream_id,
        )

        # agent_tool_use: updates session last event time
        insert_event(
            store,
            event_id="e2",
            timestamp="2025-01-25T10:00:30Z",
            event_type="agent_tool_use",
            session_id="session-A",
            stream_id=stream_id,
        )

        # agent_session ended
        insert_event(
            store,
            event_id="e3",
            timestamp="2025-01-25T10:05:00Z",
            event_type="agent_session",
            data={"action": "ended"},
            session_id="session-A",
            stream_id=stream_id,
        )

        result = store.calculate_time("2025-01-25T10:00:00Z", "2025-01-25T10:05:00Z")

        assert stream_id in result
        assert result[stream_id]["direct_ms"] == 120_000  # 2 min attention window
        assert result[stream_id]["delegated_ms"] == 300_000  # 5 min session duration


class TestSpecExample2:
    """Test Example 2 from spec: Three Parallel Agents with Focus Switches.

    10:00:00  user_message(session=A, stream=S1)     → focus=S1, active={A}
    10:01:00  agent_session(B, started, stream=S2)  → active={A,B}
    10:02:00  tmux_pane_focus(cwd→S2)               → focus=S2
    10:03:00  agent_session(C, started, stream=S3)  → active={A,B,C}
    10:04:00  tmux_scroll                            → last_activity=10:04
    10:10:00  (all sessions ended)                   → active={}

    Result:
      S1: direct=120000 (10:00-10:02), delegated=600000 (10:00-10:10)
      S2: direct=240000 (10:02-10:06), delegated=540000 (10:01-10:10)
      S3: direct=0 (never focused), delegated=420000 (10:03-10:10)
    """

    def test_three_parallel_agents_with_focus_switches(self):
        store = EventStore.open_in_memory()
        s1 = store.create_stream(name="S1")
        s2 = store.create_stream(name="S2")
        s3 = store.create_stream(name="S3")

        # Session A starts
        insert_event(
            store,
            event_id="e0a",
            timestamp="2025-01-25T10:00:00Z",
            event_type="agent_session",
            data={"action": "started"},
            session_id="session-A",
            stream_id=s1,
        )

        # user_message in S1
        insert_event(
            store,
            event_id="e1",
            timestamp="2025-01-25T10:00:00Z",
            event_type="user_message",
            session_id="session-A",
            stream_id=s1,
        )

        # Session B starts in S2
        insert_event(
            store,
            event_id="e2",
            timestamp="2025-01-25T10:01:00Z",
            event_type="agent_session",
            data={"action": "started"},
            session_id="session-B",
            stream_id=s2,
        )

        # Focus switch to S2
        insert_event(
            store,
            event_id="e3",
            timestamp="2025-01-25T10:02:00Z",
            event_type="tmux_pane_focus",
            cwd="/home/test/project-s2",
            stream_id=s2,
        )

        # Session C starts in S3
        insert_event(
            store,
            event_id="e4",
            timestamp="2025-01-25T10:03:00Z",
            event_type="agent_session",
            data={"action": "started"},
            session_id="session-C",
            stream_id=s3,
        )

        # Scroll activity at 10:04 (extends direct time)
        insert_event(
            store,
            event_id="e5",
            timestamp="2025-01-25T10:04:00Z",
            event_type="tmux_scroll",
            stream_id=s2,
        )

        # All sessions end at 10:10
        for sid, label in [("session-A", "e6"), ("session-B", "e7"), ("session-C", "e8")]:
            insert_event(
                store,
                event_id=label,
                timestamp="2025-01-25T10:10:00Z",
                event_type="agent_session",
                data={"action": "ended"},
                session_id=sid,
            )

        result = store.calculate_time("2025-01-25T10:00:00Z", "2025-01-25T10:10:00Z")

        # S1: direct=120s (10:00-10:02), delegated=600s (10:00-10:10)
        assert result[s1]["direct_ms"] == 120_000
        assert result[s1]["delegated_ms"] == 600_000

        # S2: direct=240s (10:02-10:06 = 4min, scroll at 10:04 + 2min window)
        # delegated=540s (10:01-10:10)
        assert result[s2]["direct_ms"] == 240_000
        assert result[s2]["delegated_ms"] == 540_000

        # S3: direct=0 (never focused), delegated=420s (10:03-10:10)
        assert result[s3]["direct_ms"] == 0
        assert result[s3]["delegated_ms"] == 420_000


class TestSpecExample3:
    """Test Example 3 from spec: AFK Period with Running Agents.

    10:00:00  user_message(session=A, stream=S1)     → focus=S1, active={A}
    10:02:00  afk_change(idle)                       → is_afk=true
    10:15:00  afk_change(active)                     → is_afk=false
    10:15:30  agent_session(A, ended)                → active={}

    Result for S1:
      direct_ms  = 120000  (10:00:00 to 10:02:00)
      delegated_ms = 930000  (10:00:00 to 10:15:30)
    """

    def test_afk_period_with_running_agents(self):
        store = EventStore.open_in_memory()
        stream_id = store.create_stream(name="S1")

        # Session starts and user sends message
        insert_event(
            store,
            event_id="e0",
            timestamp="2025-01-25T10:00:00Z",
            event_type="agent_session",
            data={"action": "started"},
            session_id="session-A",
            stream_id=stream_id,
        )
        insert_event(
            store,
            event_id="e1",
            timestamp="2025-01-25T10:00:00Z",
            event_type="user_message",
            session_id="session-A",
            stream_id=stream_id,
        )

        # AFK at 10:02
        insert_event(
            store,
            event_id="e2",
            timestamp="2025-01-25T10:02:00Z",
            event_type="afk_change",
            data={"status": "idle"},
        )

        # Return from AFK at 10:15
        insert_event(
            store,
            event_id="e3",
            timestamp="2025-01-25T10:15:00Z",
            event_type="afk_change",
            data={"status": "active"},
        )

        # Session ends at 10:15:30
        insert_event(
            store,
            event_id="e4",
            timestamp="2025-01-25T10:15:30Z",
            event_type="agent_session",
            data={"action": "ended"},
            session_id="session-A",
        )

        result = store.calculate_time("2025-01-25T10:00:00Z", "2025-01-25T10:15:30Z")

        # Direct time: only 2 min (10:00-10:02, then AFK)
        assert result[stream_id]["direct_ms"] == 120_000

        # Delegated time: full 15.5 min (agents work during AFK)
        assert result[stream_id]["delegated_ms"] == 930_000


class TestEdgeCases:
    """Edge case tests."""

    def test_no_focus_events_for_session_direct_zero(self):
        """Session with no focus events gets delegated time only."""
        store = EventStore.open_in_memory()
        stream_id = store.create_stream(name="background")

        # Agent session runs with no user interaction
        insert_event(
            store,
            event_id="e1",
            timestamp="2025-01-25T10:00:00Z",
            event_type="agent_session",
            data={"action": "started"},
            session_id="session-bg",
            stream_id=stream_id,
        )
        insert_event(
            store,
            event_id="e2",
            timestamp="2025-01-25T10:05:00Z",
            event_type="agent_session",
            data={"action": "ended"},
            session_id="session-bg",
        )

        result = store.calculate_time("2025-01-25T10:00:00Z", "2025-01-25T10:05:00Z")

        assert result[stream_id]["direct_ms"] == 0
        assert result[stream_id]["delegated_ms"] == 300_000  # 5 min

    def test_session_timeout_prunes_stale_session(self):
        """Session with no events for SESSION_TIMEOUT stops accruing time."""
        store = EventStore.open_in_memory()
        stream_id = store.create_stream(name="stale")

        # Session starts at 10:00
        insert_event(
            store,
            event_id="e1",
            timestamp="2025-01-25T10:00:00Z",
            event_type="agent_session",
            data={"action": "started"},
            session_id="session-stale",
            stream_id=stream_id,
        )

        # No further events - query for 1 hour
        # Session should timeout after 30 min
        result = store.calculate_time(
            "2025-01-25T10:00:00Z",
            "2025-01-25T11:00:00Z",
            session_timeout_ms=1_800_000,  # 30 min
        )

        # Delegated time should be 30 min (session timeout), not 60 min
        assert result[stream_id]["delegated_ms"] == 1_800_000

    def test_query_starts_mid_activity_seeds_correctly(self):
        """Query starting after activity began seeds state from prior events."""
        store = EventStore.open_in_memory()
        stream_id = store.create_stream(name="mid")

        # Focus event at 9:59 (before query window)
        insert_event(
            store,
            event_id="e1",
            timestamp="2025-01-25T09:59:00Z",
            event_type="tmux_pane_focus",
            stream_id=stream_id,
        )

        # Query starts at 10:00, 1 min after focus
        result = store.calculate_time("2025-01-25T10:00:00Z", "2025-01-25T10:01:00Z")

        # Should get 1 min direct time (within attention window from 9:59)
        assert result[stream_id]["direct_ms"] == 60_000

    def test_same_timestamp_user_message_wins(self):
        """user_message at same timestamp as tmux_pane_focus wins for focus."""
        store = EventStore.open_in_memory()
        s1 = store.create_stream(name="focus-stream")
        s2 = store.create_stream(name="message-stream")

        # Both events at same timestamp
        insert_event(
            store,
            event_id="e1",
            timestamp="2025-01-25T10:00:00Z",
            event_type="tmux_pane_focus",
            stream_id=s1,
        )
        insert_event(
            store,
            event_id="e2",
            timestamp="2025-01-25T10:00:00Z",
            event_type="user_message",
            session_id="session-A",
            stream_id=s2,
        )
        insert_event(
            store,
            event_id="e0",
            timestamp="2025-01-25T10:00:00Z",
            event_type="agent_session",
            data={"action": "started"},
            session_id="session-A",
            stream_id=s2,
        )

        result = store.calculate_time("2025-01-25T10:00:00Z", "2025-01-25T10:02:00Z")

        # user_message wins, so s2 gets direct time
        assert result[s2]["direct_ms"] == 120_000
        assert s1 not in result or result[s1]["direct_ms"] == 0

    def test_null_stream_id_no_time_attributed(self):
        """Events with stream_id=null don't contribute to any stream's time."""
        store = EventStore.open_in_memory()

        # Focus event without stream
        insert_event(
            store,
            event_id="e1",
            timestamp="2025-01-25T10:00:00Z",
            event_type="tmux_pane_focus",
            stream_id=None,
        )

        result = store.calculate_time("2025-01-25T10:00:00Z", "2025-01-25T10:05:00Z")

        assert result == {}

    def test_terminal_focus_without_prior_stream_stays_null(self):
        """window_focus(terminal) without prior current_stream stays null."""
        store = EventStore.open_in_memory()
        stream_id = store.create_stream(name="test")

        # window_focus terminal without prior focus
        insert_event(
            store,
            event_id="e1",
            timestamp="2025-01-25T10:00:00Z",
            event_type="window_focus",
            data={"app": "Terminal"},
            stream_id=None,  # No stream - it's just "terminal is active"
        )

        result = store.calculate_time("2025-01-25T10:00:00Z", "2025-01-25T10:05:00Z")

        # No direct time since current_stream is null
        assert result == {}


class TestInvariants:
    """Tests verifying invariants from spec."""

    def test_direct_time_sum_lte_wall_clock(self):
        """Sum of direct_ms across streams <= wall clock time."""
        store = EventStore.open_in_memory()
        s1 = store.create_stream(name="S1")
        s2 = store.create_stream(name="S2")

        # Switch focus between streams
        insert_event(
            store,
            event_id="e1",
            timestamp="2025-01-25T10:00:00Z",
            event_type="tmux_pane_focus",
            stream_id=s1,
        )
        insert_event(
            store,
            event_id="e2",
            timestamp="2025-01-25T10:01:00Z",
            event_type="tmux_pane_focus",
            stream_id=s2,
        )
        insert_event(
            store,
            event_id="e3",
            timestamp="2025-01-25T10:02:00Z",
            event_type="tmux_pane_focus",
            stream_id=s1,
        )

        # 5 minute window
        result = store.calculate_time("2025-01-25T10:00:00Z", "2025-01-25T10:05:00Z")

        wall_clock_ms = 5 * 60 * 1000  # 5 minutes
        total_direct = sum(r["direct_ms"] for r in result.values())
        assert total_direct <= wall_clock_ms

    def test_direct_time_zero_when_afk_entire_range(self):
        """direct_ms = 0 when AFK for entire query range."""
        store = EventStore.open_in_memory()
        stream_id = store.create_stream(name="test")

        # AFK before query starts
        insert_event(
            store,
            event_id="e0",
            timestamp="2025-01-25T09:55:00Z",
            event_type="afk_change",
            data={"status": "idle"},
        )

        # Focus during query (but still AFK)
        insert_event(
            store,
            event_id="e1",
            timestamp="2025-01-25T10:00:00Z",
            event_type="tmux_pane_focus",
            stream_id=stream_id,
        )

        result = store.calculate_time("2025-01-25T10:00:00Z", "2025-01-25T10:05:00Z")

        # No direct time because AFK
        assert stream_id not in result or result[stream_id]["direct_ms"] == 0


class TestPerformance:
    """Performance tests."""

    def test_10000_events_under_1_second(self):
        """10,000 events processed in <1s."""
        import time

        store = EventStore.open_in_memory()
        stream_id = store.create_stream(name="perf-test")

        # Insert 10,000 events over 10 hours
        for i in range(10000):
            minute = i // 60
            second = i % 60
            hour = minute // 60
            minute = minute % 60
            timestamp = f"2025-01-25T{10 + hour:02d}:{minute:02d}:{second:02d}Z"

            event_type = ["tmux_pane_focus", "tmux_scroll", "agent_tool_use"][i % 3]
            insert_event(
                store,
                event_id=f"e{i}",
                timestamp=timestamp,
                event_type=event_type,
                stream_id=stream_id,
                session_id="session-perf" if event_type == "agent_tool_use" else None,
            )

        # Add session boundaries
        insert_event(
            store,
            event_id="e-start",
            timestamp="2025-01-25T10:00:00Z",
            event_type="agent_session",
            data={"action": "started"},
            session_id="session-perf",
            stream_id=stream_id,
        )
        insert_event(
            store,
            event_id="e-end",
            timestamp="2025-01-25T20:00:00Z",
            event_type="agent_session",
            data={"action": "ended"},
            session_id="session-perf",
        )

        start = time.perf_counter()
        result = store.calculate_time("2025-01-25T10:00:00Z", "2025-01-25T20:00:00Z")
        elapsed = time.perf_counter() - start

        assert elapsed < 1.0, f"Time calculation took {elapsed:.2f}s, expected <1s"
        assert stream_id in result

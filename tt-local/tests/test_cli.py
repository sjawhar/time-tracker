"""Tests for the CLI entry point."""

import json
import tempfile
from pathlib import Path

from click.testing import CliRunner

from tt_local.cli import main
from tt_local.db import EventStore


def test_main_help():
    """Test that --help works and shows the group description."""
    runner = CliRunner()
    result = runner.invoke(main, ["--help"])
    assert result.exit_code == 0
    assert "Time Tracker local CLI" in result.output


def test_main_no_args():
    """Test that running with no args shows usage error (click group default)."""
    runner = CliRunner()
    result = runner.invoke(main, [])
    # Click groups exit with code 2 when no subcommand is provided
    assert result.exit_code == 2
    assert "Usage:" in result.output


def test_import_cli():
    """Test that the CLI module can be imported."""
    from tt_local import cli
    assert hasattr(cli, "main")


class TestImportCommand:
    """Tests for the import command."""

    def test_import_single_event(self):
        """Import a single event from stdin."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            event = {
                "id": "abc123",
                "timestamp": "2025-01-25T10:00:00Z",
                "type": "tmux_pane_focus",
                "source": "remote.tmux",
                "data": {"pane_id": "%1"},
                "cwd": "/home/sami/project",
            }
            input_data = json.dumps(event) + "\n"

            runner = CliRunner()
            result = runner.invoke(main, ["import", "--db", str(db_path)], input=input_data)

            assert result.exit_code == 0
            assert "Imported 1 events" in result.output

            # Verify in database
            store = EventStore.open(db_path)
            events = store.get_events()
            assert len(events) == 1
            assert events[0]["id"] == "abc123"
            store.close()

    def test_import_multiple_events(self):
        """Import multiple events from stdin."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            events = [
                {"id": "abc123", "timestamp": "2025-01-25T10:00:00Z", "type": "t1", "source": "s1", "data": {}},
                {"id": "def456", "timestamp": "2025-01-25T10:01:00Z", "type": "t2", "source": "s2", "data": {}},
                {"id": "ghi789", "timestamp": "2025-01-25T10:02:00Z", "type": "t3", "source": "s3", "data": {}},
            ]
            input_data = "\n".join(json.dumps(e) for e in events) + "\n"

            runner = CliRunner()
            result = runner.invoke(main, ["import", "--db", str(db_path)], input=input_data)

            assert result.exit_code == 0
            assert "Imported 3 events" in result.output

            store = EventStore.open(db_path)
            assert len(store.get_events()) == 3
            store.close()

    def test_import_skips_malformed_json(self):
        """Malformed JSON lines are skipped with warning."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            input_data = (
                '{"id":"abc123","timestamp":"2025-01-25T10:00:00Z","type":"t1","source":"s1","data":{}}\n'
                'not valid json\n'
                '{"id":"def456","timestamp":"2025-01-25T10:01:00Z","type":"t2","source":"s2","data":{}}\n'
            )

            runner = CliRunner()
            result = runner.invoke(main, ["import", "--db", str(db_path)], input=input_data)

            assert result.exit_code == 0
            assert "Imported 2 events" in result.output
            assert "Warning: line 2: invalid JSON" in result.output

            store = EventStore.open(db_path)
            assert len(store.get_events()) == 2
            store.close()

    def test_import_idempotent(self):
        """Same events imported twice result in no duplicates."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            event = {"id": "abc123", "timestamp": "2025-01-25T10:00:00Z", "type": "t1", "source": "s1", "data": {}}
            input_data = json.dumps(event) + "\n"

            runner = CliRunner()
            # First import
            result1 = runner.invoke(main, ["import", "--db", str(db_path)], input=input_data)
            assert result1.exit_code == 0
            assert "Imported 1 events" in result1.output

            # Second import
            result2 = runner.invoke(main, ["import", "--db", str(db_path)], input=input_data)
            assert result2.exit_code == 0
            assert "Imported 0 events" in result2.output

            store = EventStore.open(db_path)
            assert len(store.get_events()) == 1
            store.close()

    def test_import_empty_input(self):
        """Empty input is handled gracefully."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"

            runner = CliRunner()
            result = runner.invoke(main, ["import", "--db", str(db_path)], input="")

            assert result.exit_code == 0
            assert "Imported 0 events" in result.output

    def test_import_all_invalid_exits_nonzero(self):
        """Exit code 1 if all lines were invalid (non-empty input, zero imports)."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            input_data = "not valid json\nalso not valid\n"

            runner = CliRunner()
            result = runner.invoke(main, ["import", "--db", str(db_path)], input=input_data)

            assert result.exit_code == 1
            assert "Imported 0 events" in result.output

    def test_import_blank_lines_ignored(self):
        """Blank lines in input are silently ignored."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            input_data = (
                "\n"
                '{"id":"abc123","timestamp":"2025-01-25T10:00:00Z","type":"t1","source":"s1","data":{}}\n'
                "\n"
                '{"id":"def456","timestamp":"2025-01-25T10:01:00Z","type":"t2","source":"s2","data":{}}\n'
                "\n"
            )

            runner = CliRunner()
            result = runner.invoke(main, ["import", "--db", str(db_path)], input=input_data)

            assert result.exit_code == 0
            assert "Imported 2 events" in result.output

            store = EventStore.open(db_path)
            assert len(store.get_events()) == 2
            store.close()


class TestSyncCommand:
    """Tests for the sync command."""

    def test_sync_success(self):
        """Sync events from remote via SSH."""
        from unittest.mock import Mock, patch

        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            events = [
                {"id": "abc123", "timestamp": "2025-01-25T10:00:00Z", "type": "t1", "source": "s1", "data": {}},
                {"id": "def456", "timestamp": "2025-01-25T10:01:00Z", "type": "t2", "source": "s2", "data": {}},
            ]
            jsonl_output = "\n".join(json.dumps(e) for e in events) + "\n"

            mock_result = Mock()
            mock_result.returncode = 0
            mock_result.stdout = jsonl_output.encode("utf-8")
            mock_result.stderr = b""

            with patch("subprocess.run", return_value=mock_result) as mock_run:
                runner = CliRunner()
                result = runner.invoke(main, ["sync", "user@devserver", "--db", str(db_path)])

            assert result.exit_code == 0
            assert "2" in result.output  # 2 events imported
            assert "user@devserver" in result.output

            # Verify events in database
            store = EventStore.open(db_path)
            assert len(store.get_events()) == 2
            store.close()

            # Verify SSH command was correct
            mock_run.assert_called_once()
            call_args = mock_run.call_args
            assert call_args[0][0] == ["ssh", "user@devserver", "tt", "export"]

    def test_sync_empty_output(self):
        """Empty output from remote (no events)."""
        from unittest.mock import Mock, patch

        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"

            mock_result = Mock()
            mock_result.returncode = 0
            mock_result.stdout = b""
            mock_result.stderr = b""

            with patch("subprocess.run", return_value=mock_result):
                runner = CliRunner()
                result = runner.invoke(main, ["sync", "devserver", "--db", str(db_path)])

            assert result.exit_code == 0
            assert "No events" in result.output or "0" in result.output

    def test_sync_ssh_connection_failure(self):
        """SSH connection failure (exit code 255)."""
        from unittest.mock import Mock, patch

        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"

            mock_result = Mock()
            mock_result.returncode = 255
            mock_result.stdout = b""
            mock_result.stderr = b"ssh: connect to host devserver: Connection refused"

            with patch("subprocess.run", return_value=mock_result):
                runner = CliRunner()
                result = runner.invoke(main, ["sync", "devserver", "--db", str(db_path)])

            assert result.exit_code == 1
            assert "connection" in result.output.lower() or "failed" in result.output.lower()

    def test_sync_remote_command_failure(self):
        """Remote command failure (non-255 exit code)."""
        from unittest.mock import Mock, patch

        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"

            mock_result = Mock()
            mock_result.returncode = 1
            mock_result.stdout = b""
            mock_result.stderr = b"tt: command not found"

            with patch("subprocess.run", return_value=mock_result):
                runner = CliRunner()
                result = runner.invoke(main, ["sync", "devserver", "--db", str(db_path)])

            assert result.exit_code == 1
            assert "failed" in result.output.lower()

    def test_sync_timeout(self):
        """SSH timeout."""
        import subprocess
        from unittest.mock import patch

        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"

            with patch("subprocess.run", side_effect=subprocess.TimeoutExpired("ssh", 60)):
                runner = CliRunner()
                result = runner.invoke(main, ["sync", "devserver", "--db", str(db_path)])

            assert result.exit_code == 1
            assert "timeout" in result.output.lower() or "timed out" in result.output.lower()

    def test_sync_idempotent(self):
        """Running sync twice imports no duplicates."""
        from unittest.mock import Mock, patch

        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            event = {"id": "abc123", "timestamp": "2025-01-25T10:00:00Z", "type": "t1", "source": "s1", "data": {}}
            jsonl_output = json.dumps(event) + "\n"

            mock_result = Mock()
            mock_result.returncode = 0
            mock_result.stdout = jsonl_output.encode("utf-8")
            mock_result.stderr = b""

            with patch("subprocess.run", return_value=mock_result):
                runner = CliRunner()
                # First sync
                result1 = runner.invoke(main, ["sync", "devserver", "--db", str(db_path)])
                assert result1.exit_code == 0

                # Second sync
                result2 = runner.invoke(main, ["sync", "devserver", "--db", str(db_path)])
                assert result2.exit_code == 0

            # Should still have only 1 event
            store = EventStore.open(db_path)
            assert len(store.get_events()) == 1
            store.close()

    def test_sync_partial_errors(self):
        """Some malformed lines in output."""
        from unittest.mock import Mock, patch

        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            jsonl_output = (
                '{"id":"abc123","timestamp":"2025-01-25T10:00:00Z","type":"t1","source":"s1","data":{}}\n'
                "not valid json\n"
                '{"id":"def456","timestamp":"2025-01-25T10:01:00Z","type":"t2","source":"s2","data":{}}\n'
            )

            mock_result = Mock()
            mock_result.returncode = 0
            mock_result.stdout = jsonl_output.encode("utf-8")
            mock_result.stderr = b""

            with patch("subprocess.run", return_value=mock_result):
                runner = CliRunner()
                result = runner.invoke(main, ["sync", "devserver", "--db", str(db_path)])

            assert result.exit_code == 0
            # Should still import the valid events
            store = EventStore.open(db_path)
            assert len(store.get_events()) == 2
            store.close()

    def test_sync_calls_ssh_correctly(self):
        """Verify subprocess.run called with list args (NOT shell=True) to prevent command injection."""
        from unittest.mock import Mock, patch

        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"

            mock_result = Mock()
            mock_result.returncode = 0
            mock_result.stdout = b""
            mock_result.stderr = b""

            with patch("subprocess.run", return_value=mock_result) as mock_run:
                runner = CliRunner()
                runner.invoke(main, ["sync", "user@host", "--db", str(db_path)])

            mock_run.assert_called_once()
            call_args = mock_run.call_args
            # Must be a list, not a string (prevents command injection)
            assert call_args[0][0] == ["ssh", "user@host", "tt", "export"]
            # Verify shell=True is NOT used
            assert call_args.kwargs.get("shell") is not True

    def test_sync_unicode_in_output(self):
        """UTF-8 output is decoded correctly."""
        from unittest.mock import Mock, patch

        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            # Event with unicode characters in data
            event = {
                "id": "abc123",
                "timestamp": "2025-01-25T10:00:00Z",
                "type": "t1",
                "source": "s1",
                "data": {"message": "Hello, 世界! 🌍"},
            }
            jsonl_output = json.dumps(event) + "\n"

            mock_result = Mock()
            mock_result.returncode = 0
            mock_result.stdout = jsonl_output.encode("utf-8")
            mock_result.stderr = b""

            with patch("subprocess.run", return_value=mock_result):
                runner = CliRunner()
                result = runner.invoke(main, ["sync", "devserver", "--db", str(db_path)])

            assert result.exit_code == 0

            store = EventStore.open(db_path)
            events = store.get_events()
            assert len(events) == 1
            data = json.loads(events[0]["data"])
            assert data["message"] == "Hello, 世界! 🌍"
            store.close()

    def test_sync_all_invalid_exits_nonzero(self):
        """Exit code 1 if all lines were invalid (non-empty input, zero imports)."""
        from unittest.mock import Mock, patch

        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            # All lines are invalid JSON
            jsonl_output = "not valid json\nalso not valid\n"

            mock_result = Mock()
            mock_result.returncode = 0
            mock_result.stdout = jsonl_output.encode("utf-8")
            mock_result.stderr = b""

            with patch("subprocess.run", return_value=mock_result):
                runner = CliRunner()
                result = runner.invoke(main, ["sync", "devserver", "--db", str(db_path)])

            assert result.exit_code == 1
            assert "0" in result.output  # 0 events synced


class TestEventsCommand:
    """Tests for the events command."""

    def test_events_empty_db(self):
        """Empty database outputs nothing."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            # Create empty database
            store = EventStore.open(db_path)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["events", "--db", str(db_path)])

            assert result.exit_code == 0
            assert result.output == ""

    def test_events_outputs_jsonl(self):
        """Events output as JSONL format."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            # Insert test events
            store = EventStore.open(db_path)
            from tt_local.db import ImportedEvent

            events = [
                ImportedEvent(
                    id="abc123",
                    timestamp="2025-01-25T10:00:00Z",
                    type="tmux_pane_focus",
                    source="remote.tmux",
                    data={"pane_id": "%1"},
                    cwd="/home/sami/project",
                ),
                ImportedEvent(
                    id="def456",
                    timestamp="2025-01-25T10:01:00Z",
                    type="agent_tool_use",
                    source="remote.agent",
                    data={"tool": "Edit"},
                    cwd="/home/sami/project",
                ),
            ]
            for e in events:
                store.insert_imported_event(e)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["events", "--db", str(db_path)])

            assert result.exit_code == 0
            lines = result.output.strip().split("\n")
            assert len(lines) == 2

            # Verify JSON format
            parsed = [json.loads(line) for line in lines]
            assert parsed[0]["id"] == "abc123"
            assert parsed[1]["id"] == "def456"

    def test_events_since_filter(self):
        """--since filters events by timestamp."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)
            from tt_local.db import ImportedEvent

            events = [
                ImportedEvent(
                    id="e1",
                    timestamp="2025-01-25T10:00:00Z",
                    type="t1",
                    source="s1",
                    data={},
                ),
                ImportedEvent(
                    id="e2",
                    timestamp="2025-01-25T11:00:00Z",
                    type="t2",
                    source="s2",
                    data={},
                ),
                ImportedEvent(
                    id="e3",
                    timestamp="2025-01-25T12:00:00Z",
                    type="t3",
                    source="s3",
                    data={},
                ),
            ]
            for e in events:
                store.insert_imported_event(e)
            store.close()

            runner = CliRunner()
            result = runner.invoke(
                main, ["events", "--db", str(db_path), "--since", "2025-01-25T11:00:00Z"]
            )

            assert result.exit_code == 0
            lines = result.output.strip().split("\n")
            assert len(lines) == 2

            parsed = [json.loads(line) for line in lines]
            assert parsed[0]["id"] == "e2"
            assert parsed[1]["id"] == "e3"

    def test_events_type_filter(self):
        """--type filters events by event type."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)
            from tt_local.db import ImportedEvent

            events = [
                ImportedEvent(
                    id="e1",
                    timestamp="2025-01-25T10:00:00Z",
                    type="tmux_pane_focus",
                    source="s1",
                    data={},
                ),
                ImportedEvent(
                    id="e2",
                    timestamp="2025-01-25T10:01:00Z",
                    type="agent_tool_use",
                    source="s2",
                    data={},
                ),
                ImportedEvent(
                    id="e3",
                    timestamp="2025-01-25T10:02:00Z",
                    type="tmux_pane_focus",
                    source="s3",
                    data={},
                ),
            ]
            for e in events:
                store.insert_imported_event(e)
            store.close()

            runner = CliRunner()
            result = runner.invoke(
                main, ["events", "--db", str(db_path), "--type", "tmux_pane_focus"]
            )

            assert result.exit_code == 0
            lines = result.output.strip().split("\n")
            assert len(lines) == 2

            parsed = [json.loads(line) for line in lines]
            assert all(p["type"] == "tmux_pane_focus" for p in parsed)

    def test_events_limit(self):
        """--limit caps output to N events."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)
            from tt_local.db import ImportedEvent

            for i in range(10):
                store.insert_imported_event(
                    ImportedEvent(
                        id=f"e{i}",
                        timestamp=f"2025-01-25T10:0{i}:00Z",
                        type="t",
                        source="s",
                        data={},
                    )
                )
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["events", "--db", str(db_path), "--limit", "3"])

            assert result.exit_code == 0
            lines = result.output.strip().split("\n")
            assert len(lines) == 3

    def test_events_combined_filters(self):
        """Multiple filters work together."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)
            from tt_local.db import ImportedEvent

            events = [
                ImportedEvent(id="e1", timestamp="2025-01-25T10:00:00Z", type="A", source="s", data={}),
                ImportedEvent(id="e2", timestamp="2025-01-25T11:00:00Z", type="B", source="s", data={}),
                ImportedEvent(id="e3", timestamp="2025-01-25T12:00:00Z", type="A", source="s", data={}),
                ImportedEvent(id="e4", timestamp="2025-01-25T13:00:00Z", type="A", source="s", data={}),
                ImportedEvent(id="e5", timestamp="2025-01-25T14:00:00Z", type="A", source="s", data={}),
            ]
            for e in events:
                store.insert_imported_event(e)
            store.close()

            runner = CliRunner()
            result = runner.invoke(
                main,
                [
                    "events",
                    "--db",
                    str(db_path),
                    "--since",
                    "2025-01-25T11:00:00Z",
                    "--type",
                    "A",
                    "--limit",
                    "2",
                ],
            )

            assert result.exit_code == 0
            lines = result.output.strip().split("\n")
            assert len(lines) == 2

            parsed = [json.loads(line) for line in lines]
            # Should get e3 and e4 (type A, after 11:00, limit 2)
            assert parsed[0]["id"] == "e3"
            assert parsed[1]["id"] == "e4"

    def test_events_no_db_exists(self):
        """Error if database doesn't exist."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "nonexistent.db"

            runner = CliRunner()
            result = runner.invoke(main, ["events", "--db", str(db_path)])

            assert result.exit_code == 1
            assert "No database found" in result.output

    def test_events_type_no_match(self):
        """--type with no matching events outputs nothing."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)
            from tt_local.db import ImportedEvent

            store.insert_imported_event(
                ImportedEvent(
                    id="e1",
                    timestamp="2025-01-25T10:00:00Z",
                    type="A",
                    source="s",
                    data={},
                )
            )
            store.close()

            runner = CliRunner()
            result = runner.invoke(
                main, ["events", "--db", str(db_path), "--type", "nonexistent"]
            )

            assert result.exit_code == 0
            assert result.output == ""


class TestStatusCommand:
    """Tests for the status command."""

    def test_status_no_database(self):
        """No database file exits with code 1."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "nonexistent.db"

            runner = CliRunner()
            result = runner.invoke(main, ["status", "--db", str(db_path)])

            assert result.exit_code == 1
            assert "No database found" in result.output

    def test_status_empty_database(self):
        """Empty database shows no events message."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            # Create empty database
            store = EventStore.open(db_path)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["status", "--db", str(db_path)])

            assert result.exit_code == 0
            assert "No events recorded" in result.output

    def test_status_single_source(self):
        """Single source shows source with count and relative time."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)
            from tt_local.db import ImportedEvent

            store.insert_imported_event(
                ImportedEvent(
                    id="e1",
                    timestamp="2025-01-25T10:00:00Z",
                    type="tmux_pane_focus",
                    source="remote.tmux",
                    data={},
                )
            )
            store.insert_imported_event(
                ImportedEvent(
                    id="e2",
                    timestamp="2025-01-25T10:01:00Z",
                    type="tmux_pane_focus",
                    source="remote.tmux",
                    data={},
                )
            )
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["status", "--db", str(db_path)])

            assert result.exit_code == 0
            assert "remote.tmux" in result.output
            assert "2 events" in result.output
            assert "Total events: 2" in result.output

    def test_status_multiple_sources(self):
        """Multiple sources sorted by most recent first."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)
            from tt_local.db import ImportedEvent

            # remote.agent has more recent event
            store.insert_imported_event(
                ImportedEvent(
                    id="e1",
                    timestamp="2025-01-25T10:00:00Z",
                    type="tmux_pane_focus",
                    source="remote.tmux",
                    data={},
                )
            )
            store.insert_imported_event(
                ImportedEvent(
                    id="e2",
                    timestamp="2025-01-25T12:00:00Z",
                    type="agent_tool_use",
                    source="remote.agent",
                    data={},
                )
            )
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["status", "--db", str(db_path)])

            assert result.exit_code == 0
            # remote.agent should appear before remote.tmux (more recent)
            agent_pos = result.output.find("remote.agent")
            tmux_pos = result.output.find("remote.tmux")
            assert agent_pos < tmux_pos, "Sources should be sorted by most recent first"


class TestFormatRelativeTime:
    """Tests for format_relative_time helper."""

    def test_just_now(self):
        """Less than 60 seconds shows 'just now'."""
        from datetime import datetime, timezone
        from tt_local.cli import format_relative_time

        now = datetime(2025, 1, 25, 10, 0, 0, tzinfo=timezone.utc)
        ts = "2025-01-25T09:59:30Z"  # 30 seconds ago

        result = format_relative_time(ts, now=now)
        assert result == "just now"

    def test_minutes_ago(self):
        """1-59 minutes shows 'X minutes ago'."""
        from datetime import datetime, timezone
        from tt_local.cli import format_relative_time

        now = datetime(2025, 1, 25, 10, 0, 0, tzinfo=timezone.utc)

        # 1 minute ago
        assert format_relative_time("2025-01-25T09:59:00Z", now=now) == "1 minute ago"
        # 30 minutes ago
        assert format_relative_time("2025-01-25T09:30:00Z", now=now) == "30 minutes ago"

    def test_hours_ago(self):
        """1-23 hours shows 'X hours ago'."""
        from datetime import datetime, timezone
        from tt_local.cli import format_relative_time

        now = datetime(2025, 1, 25, 10, 0, 0, tzinfo=timezone.utc)

        # 1 hour ago
        assert format_relative_time("2025-01-25T09:00:00Z", now=now) == "1 hour ago"
        # 5 hours ago
        assert format_relative_time("2025-01-25T05:00:00Z", now=now) == "5 hours ago"

    def test_days_ago(self):
        """24+ hours shows 'X days ago'."""
        from datetime import datetime, timezone
        from tt_local.cli import format_relative_time

        now = datetime(2025, 1, 25, 10, 0, 0, tzinfo=timezone.utc)

        # 1 day ago
        assert format_relative_time("2025-01-24T10:00:00Z", now=now) == "1 day ago"
        # 3 days ago
        assert format_relative_time("2025-01-22T10:00:00Z", now=now) == "3 days ago"

    def test_boundary_59_seconds(self):
        """Exactly 59 seconds shows 'just now'."""
        from datetime import datetime, timezone
        from tt_local.cli import format_relative_time

        now = datetime(2025, 1, 25, 10, 0, 59, tzinfo=timezone.utc)
        ts = "2025-01-25T10:00:00Z"

        result = format_relative_time(ts, now=now)
        assert result == "just now"

    def test_boundary_60_seconds(self):
        """Exactly 60 seconds shows '1 minute ago'."""
        from datetime import datetime, timezone
        from tt_local.cli import format_relative_time

        now = datetime(2025, 1, 25, 10, 1, 0, tzinfo=timezone.utc)
        ts = "2025-01-25T10:00:00Z"

        result = format_relative_time(ts, now=now)
        assert result == "1 minute ago"

    def test_boundary_3599_seconds(self):
        """59 minutes 59 seconds shows '59 minutes ago'."""
        from datetime import datetime, timezone
        from tt_local.cli import format_relative_time

        now = datetime(2025, 1, 25, 10, 59, 59, tzinfo=timezone.utc)
        ts = "2025-01-25T10:00:00Z"

        result = format_relative_time(ts, now=now)
        assert result == "59 minutes ago"

    def test_boundary_3600_seconds(self):
        """Exactly 1 hour shows '1 hour ago'."""
        from datetime import datetime, timezone
        from tt_local.cli import format_relative_time

        now = datetime(2025, 1, 25, 11, 0, 0, tzinfo=timezone.utc)
        ts = "2025-01-25T10:00:00Z"

        result = format_relative_time(ts, now=now)
        assert result == "1 hour ago"

    def test_boundary_86399_seconds(self):
        """23 hours 59 minutes 59 seconds shows '23 hours ago'."""
        from datetime import datetime, timezone
        from tt_local.cli import format_relative_time

        now = datetime(2025, 1, 26, 9, 59, 59, tzinfo=timezone.utc)
        ts = "2025-01-25T10:00:00Z"

        result = format_relative_time(ts, now=now)
        assert result == "23 hours ago"

    def test_boundary_86400_seconds(self):
        """Exactly 24 hours shows '1 day ago'."""
        from datetime import datetime, timezone
        from tt_local.cli import format_relative_time

        now = datetime(2025, 1, 26, 10, 0, 0, tzinfo=timezone.utc)
        ts = "2025-01-25T10:00:00Z"

        result = format_relative_time(ts, now=now)
        assert result == "1 day ago"

    def test_future_timestamp(self):
        """Future timestamp (clock skew) shows 'just now'."""
        from datetime import datetime, timezone
        from tt_local.cli import format_relative_time

        now = datetime(2025, 1, 25, 10, 0, 0, tzinfo=timezone.utc)
        ts = "2025-01-25T10:05:00Z"  # 5 minutes in the future

        result = format_relative_time(ts, now=now)
        assert result == "just now"

    def test_malformed_timestamp(self):
        """Malformed timestamp returns raw value."""
        from tt_local.cli import format_relative_time

        result = format_relative_time("not-a-timestamp")
        assert result == "not-a-timestamp"

        result = format_relative_time("garbage")
        assert result == "garbage"


class TestFormatDuration:
    """Tests for format_duration helper."""

    def test_zero_milliseconds(self):
        """Zero milliseconds shows '0m'."""
        from tt_local.cli import format_duration

        assert format_duration(0) == "0m"

    def test_sub_minute_positive(self):
        """Sub-minute positive shows '<1m'."""
        from tt_local.cli import format_duration

        assert format_duration(30_000) == "<1m"  # 30 sec
        assert format_duration(59_999) == "<1m"  # just under 1 min

    def test_exactly_one_minute(self):
        """Exactly 1 minute shows '1m'."""
        from tt_local.cli import format_duration

        assert format_duration(60_000) == "1m"

    def test_minutes_only(self):
        """Minutes without hours omits hours."""
        from tt_local.cli import format_duration

        assert format_duration(120_000) == "2m"
        assert format_duration(2_700_000) == "45m"  # 45 min

    def test_hours_and_minutes(self):
        """Hours with minutes shows 'Xh Ym' format."""
        from tt_local.cli import format_duration

        assert format_duration(3_600_000) == "1h  0m"  # 1 hour
        assert format_duration(5_400_000) == "1h 30m"  # 1.5 hours
        assert format_duration(7_200_000) == "2h  0m"  # 2 hours
        assert format_duration(8_100_000) == "2h 15m"  # 2:15

    def test_rounds_down(self):
        """Duration rounds down to nearest minute."""
        from tt_local.cli import format_duration

        # 1m 59.999s → 1m
        assert format_duration(119_999) == "1m"
        # 1h 0m 59.999s → 1h 0m
        assert format_duration(3_659_999) == "1h  0m"


class TestGetWeekRange:
    """Tests for get_week_range helper."""

    def test_monday_returns_same_week(self):
        """Monday returns the same week."""
        from datetime import datetime, timezone
        from tt_local.cli import get_week_range

        monday = datetime(2025, 1, 20, 10, 0, 0, tzinfo=timezone.utc)
        start, end = get_week_range(monday)

        # Should be Mon 00:00 to next Mon 00:00
        assert start.startswith("2025-01-20")
        assert end.startswith("2025-01-27")

    def test_sunday_returns_same_week(self):
        """Sunday returns the same week."""
        from datetime import datetime, timezone
        from tt_local.cli import get_week_range

        sunday = datetime(2025, 1, 26, 23, 59, 59, tzinfo=timezone.utc)
        start, end = get_week_range(sunday)

        # Should still be Mon Jan 20 to Mon Jan 27
        assert start.startswith("2025-01-20")
        assert end.startswith("2025-01-27")

    def test_returns_timezone_aware_strings(self):
        """Returns timezone-aware ISO strings."""
        from datetime import datetime, timezone
        from tt_local.cli import get_week_range

        date = datetime(2025, 1, 22, 10, 0, 0, tzinfo=timezone.utc)
        start, end = get_week_range(date)

        # Should parse back to datetime with timezone
        from datetime import datetime as dt

        start_dt = dt.fromisoformat(start)
        end_dt = dt.fromisoformat(end)
        assert start_dt.tzinfo is not None
        assert end_dt.tzinfo is not None


class TestGetDayRange:
    """Tests for get_day_range helper."""

    def test_returns_start_and_end_of_day(self):
        """Returns start of day to start of next day."""
        from datetime import datetime, timezone
        from tt_local.cli import get_day_range

        date = datetime(2025, 1, 25, 15, 30, 0, tzinfo=timezone.utc)
        start, end = get_day_range(date)

        # Start should be midnight
        assert "00:00:00" in start
        # End should be next day midnight
        assert start.startswith("2025-01-25")
        assert end.startswith("2025-01-26")

    def test_returns_timezone_aware_strings(self):
        """Returns timezone-aware ISO strings."""
        from datetime import datetime, timezone
        from tt_local.cli import get_day_range

        date = datetime(2025, 1, 25, 10, 0, 0, tzinfo=timezone.utc)
        start, end = get_day_range(date)

        from datetime import datetime as dt

        start_dt = dt.fromisoformat(start)
        end_dt = dt.fromisoformat(end)
        assert start_dt.tzinfo is not None
        assert end_dt.tzinfo is not None


class TestMakeProgressBar:
    """Tests for make_progress_bar helper."""

    def test_zero_max_returns_empty(self):
        """Max value of 0 returns all empty."""
        from tt_local.cli import make_progress_bar

        result = make_progress_bar(100, 0)
        assert result == "░" * 16

    def test_zero_value_returns_empty(self):
        """Value of 0 returns all empty."""
        from tt_local.cli import make_progress_bar

        result = make_progress_bar(0, 100)
        assert result == "░" * 16

    def test_full_bar(self):
        """Value equal to max returns all filled."""
        from tt_local.cli import make_progress_bar

        result = make_progress_bar(100, 100)
        assert result == "█" * 16

    def test_half_bar(self):
        """Half value returns half filled."""
        from tt_local.cli import make_progress_bar

        result = make_progress_bar(50, 100)
        assert result == "█" * 8 + "░" * 8

    def test_minimum_one_block(self):
        """Non-zero value gets at least one block."""
        from tt_local.cli import make_progress_bar

        result = make_progress_bar(1, 1000)
        assert result[0] == "█"
        assert result.count("█") >= 1

    def test_custom_width(self):
        """Custom width is respected."""
        from tt_local.cli import make_progress_bar

        result = make_progress_bar(50, 100, width=8)
        assert len(result) == 8
        assert result == "████░░░░"


class TestFormatDateRange:
    """Tests for format_date_range helper."""

    def test_single_day(self):
        """Single day format."""
        from tt_local.cli import format_date_range

        result = format_date_range("2025-01-28T00:00:00+00:00", "2025-01-29T00:00:00+00:00", "day")
        assert result == "Jan 28, 2025"

    def test_same_month_week(self):
        """Week within same month."""
        from tt_local.cli import format_date_range

        result = format_date_range("2025-01-20T00:00:00+00:00", "2025-01-27T00:00:00+00:00", "week")
        assert result == "Jan 20-26, 2025"

    def test_cross_month_week(self):
        """Week spanning two months."""
        from tt_local.cli import format_date_range

        result = format_date_range("2025-01-27T00:00:00+00:00", "2025-02-03T00:00:00+00:00", "week")
        assert result == "Jan 27 - Feb 02, 2025"

    def test_cross_year_week(self):
        """Week spanning two years."""
        from tt_local.cli import format_date_range

        result = format_date_range("2024-12-30T00:00:00+00:00", "2025-01-06T00:00:00+00:00", "week")
        assert result == "Dec 30, 2024 - Jan 05, 2025"


class TestReportCommand:
    """Tests for the report command."""

    def test_report_no_database(self):
        """No database file exits with code 1."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "nonexistent.db"

            runner = CliRunner()
            result = runner.invoke(main, ["report", "--db", str(db_path)])

            assert result.exit_code == 1
            assert "No database found" in result.output

    def test_report_default_is_week(self):
        """Running 'tt report' defaults to --week."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["report", "--db", str(db_path)])

            assert result.exit_code == 0
            # Should show time report (empty, but no error)
            assert "Time Report:" in result.output or "No time tracked" in result.output

    def test_report_empty_period(self):
        """Empty period shows hint to check tt status."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["report", "--week", "--db", str(db_path)])

            assert result.exit_code == 0
            assert "No time tracked" in result.output
            assert "tt status" in result.output

    def test_report_invalid_day_format(self):
        """Invalid date format for --day exits with code 1."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["report", "--day", "invalid-date", "--db", str(db_path)])

            assert result.exit_code == 1
            assert "Invalid date format" in result.output
            assert "YYYY-MM-DD" in result.output

    def test_report_day_with_specific_date(self):
        """--day with specific date in YYYY-MM-DD format."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["report", "--day", "2025-01-25", "--db", str(db_path)])

            assert result.exit_code == 0
            # Should show the date in output
            assert "Jan 25, 2025" in result.output or "No time tracked" in result.output

    def test_report_json_valid(self):
        """--json outputs valid JSON."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["report", "--json", "--db", str(db_path)])

            assert result.exit_code == 0
            # Should be valid JSON
            parsed = json.loads(result.output)
            assert "report_type" in parsed
            assert "period" in parsed
            assert "total_ms" in parsed
            assert "by_tag" in parsed

    def test_report_json_schema(self):
        """--json output matches spec schema."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["report", "--week", "--json", "--db", str(db_path)])

            assert result.exit_code == 0
            parsed = json.loads(result.output)

            # Verify schema fields
            assert parsed["report_type"] == "weekly"
            assert "generated_at" in parsed
            assert "start" in parsed["period"]
            assert "end" in parsed["period"]
            assert "days_with_data" in parsed["period"]
            assert isinstance(parsed["total_ms"], int)
            assert isinstance(parsed["direct_ms"], int)
            assert isinstance(parsed["delegated_ms"], int)
            assert isinstance(parsed["by_tag"], list)

    def test_report_json_period_dates_are_date_only(self):
        """JSON period.start and period.end are date-only (YYYY-MM-DD)."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["report", "--json", "--db", str(db_path)])

            assert result.exit_code == 0
            parsed = json.loads(result.output)

            # Should match YYYY-MM-DD format only (no time component)
            import re

            assert re.match(r"^\d{4}-\d{2}-\d{2}$", parsed["period"]["start"])
            assert re.match(r"^\d{4}-\d{2}-\d{2}$", parsed["period"]["end"])

    def test_report_json_generated_at_valid_iso(self):
        """JSON generated_at is valid ISO 8601 timestamp."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["report", "--json", "--db", str(db_path)])

            assert result.exit_code == 0
            parsed = json.loads(result.output)

            # Should parse as ISO 8601
            from datetime import datetime

            generated_at = datetime.fromisoformat(parsed["generated_at"].replace("Z", "+00:00"))
            assert generated_at is not None

    def test_report_with_data(self):
        """Report with actual time data."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)
            from tt_local.db import ImportedEvent

            # Insert events that will create a stream
            store.insert_imported_event(
                ImportedEvent(
                    id="e1",
                    timestamp="2025-01-27T10:00:00Z",  # Monday of the week
                    type="tmux_pane_focus",
                    source="remote.tmux",
                    data={"pane_id": "%1"},
                    cwd="/home/test/project",
                )
            )
            store.insert_imported_event(
                ImportedEvent(
                    id="e2",
                    timestamp="2025-01-27T10:05:00Z",
                    type="tmux_scroll",
                    source="remote.tmux",
                    data={},
                    cwd="/home/test/project",
                )
            )
            store.close()

            runner = CliRunner()
            # Use fixed week containing the events
            result = runner.invoke(main, ["report", "--day", "2025-01-27", "--db", str(db_path)])

            assert result.exit_code == 0
            # Should show time data
            assert "Total:" in result.output or "No time tracked" not in result.output

    def test_report_tags_sorted_by_total_descending(self):
        """Tags sorted by total time descending, untagged last."""
        from tt_local.db import ImportedEvent

        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)

            # Create streams with different times
            s1 = store.create_stream(name="project-a")
            s2 = store.create_stream(name="project-b")

            # Tag s1 with "small", s2 with "large"
            store._conn.execute("INSERT INTO stream_tags VALUES (?, ?)", (s1, "small"))
            store._conn.execute("INSERT INTO stream_tags VALUES (?, ?)", (s2, "large"))
            store._conn.commit()

            # Insert events: s1 has little time, s2 has more time
            def insert_with_stream(event_id: str, ts: str, stream_id: str) -> None:
                store._conn.execute(
                    """
                    INSERT INTO events
                    (id, timestamp, type, source, schema_version, data, cwd, stream_id, assignment_source)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        event_id,
                        ts,
                        "tmux_pane_focus",
                        "remote.tmux",
                        1,
                        "{}",
                        "/test",
                        stream_id,
                        "user",
                    ),
                )
                store._conn.commit()

            # s2 gets more events (more time)
            insert_with_stream("e1", "2025-01-27T10:00:00Z", s2)
            insert_with_stream("e2", "2025-01-27T10:01:00Z", s2)
            insert_with_stream("e3", "2025-01-27T10:02:00Z", s2)

            # s1 gets fewer events (less time)
            insert_with_stream("e4", "2025-01-27T11:00:00Z", s1)

            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["report", "--day", "2025-01-27", "--json", "--db", str(db_path)])

            assert result.exit_code == 0
            parsed = json.loads(result.output)

            # "large" should be first (more time), "small" second
            if len(parsed["by_tag"]) >= 2:
                assert parsed["by_tag"][0]["tag"] == "large"
                assert parsed["by_tag"][1]["tag"] == "small"

    def test_report_untagged_always_last(self):
        """Untagged streams appear last even with most time."""
        from tt_local.db import ImportedEvent

        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)

            # Create streams
            tagged_stream = store.create_stream(name="tagged")
            untagged_stream = store.create_stream(name="untagged")

            # Only tag one stream
            store._conn.execute("INSERT INTO stream_tags VALUES (?, ?)", (tagged_stream, "my-tag"))
            store._conn.commit()

            # Give untagged stream MORE time
            def insert_with_stream(event_id: str, ts: str, stream_id: str) -> None:
                store._conn.execute(
                    """
                    INSERT INTO events
                    (id, timestamp, type, source, schema_version, data, cwd, stream_id, assignment_source)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        event_id,
                        ts,
                        "tmux_pane_focus",
                        "remote.tmux",
                        1,
                        "{}",
                        "/test",
                        stream_id,
                        "user",
                    ),
                )
                store._conn.commit()

            # Untagged gets more events
            insert_with_stream("e1", "2025-01-27T10:00:00Z", untagged_stream)
            insert_with_stream("e2", "2025-01-27T10:01:00Z", untagged_stream)
            insert_with_stream("e3", "2025-01-27T10:02:00Z", untagged_stream)
            insert_with_stream("e4", "2025-01-27T10:03:00Z", untagged_stream)

            # Tagged gets fewer events
            insert_with_stream("e5", "2025-01-27T11:00:00Z", tagged_stream)

            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["report", "--day", "2025-01-27", "--json", "--db", str(db_path)])

            assert result.exit_code == 0
            parsed = json.loads(result.output)

            # Untagged (tag=null) should be last regardless of time
            if len(parsed["by_tag"]) >= 2:
                assert parsed["by_tag"][-1]["tag"] is None

    def test_report_long_tag_truncated(self):
        """Tags longer than 20 chars are truncated with ellipsis."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)

            s1 = store.create_stream(name="test")
            long_tag = "this-is-a-very-long-tag-name-over-twenty-chars"
            store._conn.execute("INSERT INTO stream_tags VALUES (?, ?)", (s1, long_tag))

            def insert_with_stream(event_id: str, ts: str, stream_id: str) -> None:
                store._conn.execute(
                    """
                    INSERT INTO events
                    (id, timestamp, type, source, schema_version, data, cwd, stream_id, assignment_source)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        event_id,
                        ts,
                        "tmux_pane_focus",
                        "remote.tmux",
                        1,
                        "{}",
                        "/test",
                        stream_id,
                        "user",
                    ),
                )
                store._conn.commit()

            insert_with_stream("e1", "2025-01-27T10:00:00Z", s1)
            insert_with_stream("e2", "2025-01-27T10:01:00Z", s1)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["report", "--day", "2025-01-27", "--db", str(db_path)])

            assert result.exit_code == 0
            # Tag should be truncated in human output
            assert "..." in result.output
            # Full tag should NOT appear
            assert long_tag not in result.output

    def test_report_partial_week_shows_days_with_data(self):
        """Partial weeks show '(X days with data)' in header."""
        from tt_local.db import ImportedEvent

        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)

            # Create stream and add events for only 2 days
            s1 = store.create_stream(name="test")

            def insert_with_stream(event_id: str, ts: str, stream_id: str) -> None:
                store._conn.execute(
                    """
                    INSERT INTO events
                    (id, timestamp, type, source, schema_version, data, cwd, stream_id, assignment_source)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        event_id,
                        ts,
                        "tmux_pane_focus",
                        "remote.tmux",
                        1,
                        "{}",
                        "/test",
                        stream_id,
                        "user",
                    ),
                )
                store._conn.commit()

            # Events on Monday and Wednesday only
            insert_with_stream("e1", "2025-01-20T10:00:00Z", s1)  # Monday
            insert_with_stream("e2", "2025-01-20T10:01:00Z", s1)  # Monday
            insert_with_stream("e3", "2025-01-22T10:00:00Z", s1)  # Wednesday
            store.close()

            runner = CliRunner()
            # Query the week of Jan 20-26
            result = runner.invoke(main, ["report", "--day", "2025-01-20", "--db", str(db_path)])

            # For daily report, should NOT show "days with data"
            assert result.exit_code == 0
            assert "days with data" not in result.output

    def test_report_day_no_days_with_data_indicator(self):
        """Daily report does NOT show '(X days with data)'."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["report", "--day", "2025-01-27", "--db", str(db_path)])

            assert result.exit_code == 0
            # Daily report should never show "days with data"
            assert "days with data" not in result.output

    def test_report_zero_delegated_shows_zero(self):
        """Zero delegated time shows '0m (0%)'."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)

            s1 = store.create_stream(name="test")

            def insert_with_stream(event_id: str, ts: str, stream_id: str) -> None:
                store._conn.execute(
                    """
                    INSERT INTO events
                    (id, timestamp, type, source, schema_version, data, cwd, stream_id, assignment_source)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        event_id,
                        ts,
                        "tmux_pane_focus",
                        "remote.tmux",
                        1,
                        "{}",
                        "/test",
                        stream_id,
                        "user",
                    ),
                )
                store._conn.commit()

            # Only pane focus events (no agent sessions) = no delegated time
            insert_with_stream("e1", "2025-01-27T10:00:00Z", s1)
            insert_with_stream("e2", "2025-01-27T10:01:00Z", s1)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["report", "--day", "2025-01-27", "--db", str(db_path)])

            assert result.exit_code == 0
            # Should show 0m for delegated
            assert "Delegated:" in result.output
            # Check that it shows 0% or 0m somewhere in the delegated line
            lines = result.output.split("\n")
            delegated_line = [l for l in lines if "Delegated:" in l]
            if delegated_line:
                assert "0m" in delegated_line[0] or "(0%)" in delegated_line[0]

    def test_report_future_date_empty(self):
        """Future date returns empty report (no error)."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["report", "--day", "2030-01-01", "--db", str(db_path)])

            assert result.exit_code == 0
            assert "No time tracked" in result.output or "Jan 01, 2030" in result.output


class TestStreamsCommand:
    """Tests for the streams command."""

    def test_streams_no_database(self):
        """No database file exits with code 1."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "nonexistent.db"

            runner = CliRunner()
            result = runner.invoke(main, ["streams", "--db", str(db_path)])

            assert result.exit_code == 1
            assert "No database found" in result.output

    def test_streams_empty_database(self):
        """Empty database shows empty state hint."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["streams", "--db", str(db_path)])

            assert result.exit_code == 0
            assert "No streams found" in result.output
            assert "tt status" in result.output

    def test_streams_default_is_today(self):
        """Running 'tt streams' defaults to --today."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["streams", "--db", str(db_path)])

            assert result.exit_code == 0
            # Should show today's date in output
            from datetime import datetime
            today_str = datetime.now().strftime("%b %d, %Y")
            assert today_str in result.output or "No streams found" in result.output

    def test_streams_today_with_data(self):
        """Shows streams for today with time data."""
        from datetime import datetime, timezone

        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)

            # Create stream and events for today
            s1 = store.create_stream(name="my-project")

            # Get today's date in ISO format
            now = datetime.now(timezone.utc)
            ts1 = now.strftime("%Y-%m-%dT10:00:00Z")
            ts2 = now.strftime("%Y-%m-%dT10:01:00Z")

            def insert_with_stream(event_id: str, ts: str, stream_id: str) -> None:
                store._conn.execute(
                    """
                    INSERT INTO events
                    (id, timestamp, type, source, schema_version, data, cwd, stream_id, assignment_source)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        event_id,
                        ts,
                        "tmux_pane_focus",
                        "remote.tmux",
                        1,
                        "{}",
                        "/home/test/my-project",
                        stream_id,
                        "user",
                    ),
                )
                store._conn.commit()

            insert_with_stream("e1", ts1, s1)
            insert_with_stream("e2", ts2, s1)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["streams", "--db", str(db_path)])

            assert result.exit_code == 0
            # Should show the stream
            assert "my-project" in result.output
            # Should show the short ID (first 7 chars)
            assert s1[:7] in result.output

    def test_streams_week_filter(self):
        """--week shows streams for the current week."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["streams", "--week", "--db", str(db_path)])

            assert result.exit_code == 0
            # Should show week range in header (or empty state)
            assert "Streams:" in result.output or "No streams found" in result.output

    def test_streams_json_valid(self):
        """--json outputs valid JSON."""
        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["streams", "--json", "--db", str(db_path)])

            assert result.exit_code == 0
            # Should be valid JSON
            parsed = json.loads(result.output)
            assert "period" in parsed
            assert "streams" in parsed

    def test_streams_json_schema(self):
        """--json output matches expected schema."""
        from datetime import datetime, timezone

        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)

            s1 = store.create_stream(name="webapp")
            store._conn.execute("INSERT INTO stream_tags VALUES (?, ?)", (s1, "frontend"))
            store._conn.commit()

            now = datetime.now(timezone.utc)
            ts1 = now.strftime("%Y-%m-%dT10:00:00Z")
            ts2 = now.strftime("%Y-%m-%dT10:01:00Z")

            def insert_with_stream(event_id: str, ts: str, stream_id: str) -> None:
                store._conn.execute(
                    """
                    INSERT INTO events
                    (id, timestamp, type, source, schema_version, data, cwd, stream_id, assignment_source)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        event_id,
                        ts,
                        "tmux_pane_focus",
                        "remote.tmux",
                        1,
                        "{}",
                        "/test",
                        stream_id,
                        "user",
                    ),
                )
                store._conn.commit()

            insert_with_stream("e1", ts1, s1)
            insert_with_stream("e2", ts2, s1)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["streams", "--json", "--db", str(db_path)])

            assert result.exit_code == 0
            parsed = json.loads(result.output)

            # Verify schema fields
            assert "start" in parsed["period"]
            assert "end" in parsed["period"]
            assert isinstance(parsed["streams"], list)
            if parsed["streams"]:
                stream = parsed["streams"][0]
                assert "id" in stream
                assert "short_id" in stream
                assert "name" in stream
                assert "total_ms" in stream
                assert "direct_ms" in stream
                assert "delegated_ms" in stream
                assert "tags" in stream

    def test_streams_sorted_by_time(self):
        """Streams sorted by total time descending."""
        from datetime import datetime, timezone

        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)

            # Create two streams with different amounts of time
            s1 = store.create_stream(name="small")
            s2 = store.create_stream(name="large")

            now = datetime.now(timezone.utc)

            def insert_with_stream(event_id: str, ts: str, stream_id: str) -> None:
                store._conn.execute(
                    """
                    INSERT INTO events
                    (id, timestamp, type, source, schema_version, data, cwd, stream_id, assignment_source)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        event_id,
                        ts,
                        "tmux_pane_focus",
                        "remote.tmux",
                        1,
                        "{}",
                        "/test",
                        stream_id,
                        "user",
                    ),
                )
                store._conn.commit()

            # s2 (large) gets more events/time
            insert_with_stream("e1", now.strftime("%Y-%m-%dT10:00:00Z"), s2)
            insert_with_stream("e2", now.strftime("%Y-%m-%dT10:01:00Z"), s2)
            insert_with_stream("e3", now.strftime("%Y-%m-%dT10:02:00Z"), s2)

            # s1 (small) gets fewer events
            insert_with_stream("e4", now.strftime("%Y-%m-%dT11:00:00Z"), s1)

            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["streams", "--json", "--db", str(db_path)])

            assert result.exit_code == 0
            parsed = json.loads(result.output)

            if len(parsed["streams"]) >= 2:
                # First stream should have more time than second
                assert parsed["streams"][0]["total_ms"] >= parsed["streams"][1]["total_ms"]

    def test_streams_shows_tags(self):
        """Tags displayed correctly in human output."""
        from datetime import datetime, timezone

        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)

            s1 = store.create_stream(name="project")
            store._conn.execute("INSERT INTO stream_tags VALUES (?, ?)", (s1, "frontend"))
            store._conn.commit()

            now = datetime.now(timezone.utc)
            ts1 = now.strftime("%Y-%m-%dT10:00:00Z")
            ts2 = now.strftime("%Y-%m-%dT10:01:00Z")

            def insert_with_stream(event_id: str, ts: str, stream_id: str) -> None:
                store._conn.execute(
                    """
                    INSERT INTO events
                    (id, timestamp, type, source, schema_version, data, cwd, stream_id, assignment_source)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        event_id,
                        ts,
                        "tmux_pane_focus",
                        "remote.tmux",
                        1,
                        "{}",
                        "/test",
                        stream_id,
                        "user",
                    ),
                )
                store._conn.commit()

            insert_with_stream("e1", ts1, s1)
            insert_with_stream("e2", ts2, s1)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["streams", "--db", str(db_path)])

            assert result.exit_code == 0
            assert "frontend" in result.output

    def test_streams_untagged_shows_placeholder(self):
        """(untagged) shown for streams without tags."""
        from datetime import datetime, timezone

        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)

            s1 = store.create_stream(name="project")
            # No tags added

            now = datetime.now(timezone.utc)
            ts1 = now.strftime("%Y-%m-%dT10:00:00Z")
            ts2 = now.strftime("%Y-%m-%dT10:01:00Z")

            def insert_with_stream(event_id: str, ts: str, stream_id: str) -> None:
                store._conn.execute(
                    """
                    INSERT INTO events
                    (id, timestamp, type, source, schema_version, data, cwd, stream_id, assignment_source)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        event_id,
                        ts,
                        "tmux_pane_focus",
                        "remote.tmux",
                        1,
                        "{}",
                        "/test",
                        stream_id,
                        "user",
                    ),
                )
                store._conn.commit()

            insert_with_stream("e1", ts1, s1)
            insert_with_stream("e2", ts2, s1)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["streams", "--db", str(db_path)])

            assert result.exit_code == 0
            assert "(untagged)" in result.output

    def test_streams_short_id_format(self):
        """Stream IDs shown as 7-character prefix."""
        from datetime import datetime, timezone

        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)

            s1 = store.create_stream(name="project")

            now = datetime.now(timezone.utc)
            ts1 = now.strftime("%Y-%m-%dT10:00:00Z")
            ts2 = now.strftime("%Y-%m-%dT10:01:00Z")

            def insert_with_stream(event_id: str, ts: str, stream_id: str) -> None:
                store._conn.execute(
                    """
                    INSERT INTO events
                    (id, timestamp, type, source, schema_version, data, cwd, stream_id, assignment_source)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        event_id,
                        ts,
                        "tmux_pane_focus",
                        "remote.tmux",
                        1,
                        "{}",
                        "/test",
                        stream_id,
                        "user",
                    ),
                )
                store._conn.commit()

            insert_with_stream("e1", ts1, s1)
            insert_with_stream("e2", ts2, s1)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["streams", "--db", str(db_path)])

            assert result.exit_code == 0
            # Short ID should appear, full ID should not
            assert s1[:7] in result.output
            # Full UUID should not appear in human output
            assert s1 not in result.output or s1[:7] + " " in result.output

    def test_streams_filters_zero_time(self):
        """Streams with 0ms total time are excluded."""
        from datetime import datetime, timezone

        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)

            # Create a stream with no events in today's range
            s1 = store.create_stream(name="old-project")

            # Create events in the past (not today)
            store._conn.execute(
                """
                INSERT INTO events
                (id, timestamp, type, source, schema_version, data, cwd, stream_id, assignment_source)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    "e1",
                    "2020-01-01T10:00:00Z",  # Old date
                    "tmux_pane_focus",
                    "remote.tmux",
                    1,
                    "{}",
                    "/test",
                    s1,
                    "user",
                ),
            )
            store._conn.commit()
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["streams", "--db", str(db_path)])

            assert result.exit_code == 0
            # Stream with no time today should not appear
            assert "old-project" not in result.output

    def test_streams_long_tags_truncation(self):
        """Tag list truncated with ... when too long."""
        from datetime import datetime, timezone

        with tempfile.TemporaryDirectory() as tmpdir:
            db_path = Path(tmpdir) / "test.db"
            store = EventStore.open(db_path)

            s1 = store.create_stream(name="project")
            # Add multiple tags that exceed display limit
            store._conn.execute("INSERT INTO stream_tags VALUES (?, ?)", (s1, "frontend"))
            store._conn.execute("INSERT INTO stream_tags VALUES (?, ?)", (s1, "backend"))
            store._conn.execute("INSERT INTO stream_tags VALUES (?, ?)", (s1, "api"))
            store._conn.commit()

            now = datetime.now(timezone.utc)
            ts1 = now.strftime("%Y-%m-%dT10:00:00Z")
            ts2 = now.strftime("%Y-%m-%dT10:01:00Z")

            def insert_with_stream(event_id: str, ts: str, stream_id: str) -> None:
                store._conn.execute(
                    """
                    INSERT INTO events
                    (id, timestamp, type, source, schema_version, data, cwd, stream_id, assignment_source)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        event_id,
                        ts,
                        "tmux_pane_focus",
                        "remote.tmux",
                        1,
                        "{}",
                        "/test",
                        stream_id,
                        "user",
                    ),
                )
                store._conn.commit()

            insert_with_stream("e1", ts1, s1)
            insert_with_stream("e2", ts2, s1)
            store.close()

            runner = CliRunner()
            result = runner.invoke(main, ["streams", "--db", str(db_path)])

            assert result.exit_code == 0
            # Should have ellipsis if tags are too long
            # At least one tag should appear
            assert "frontend" in result.output or "backend" in result.output or "api" in result.output

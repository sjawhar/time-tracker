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

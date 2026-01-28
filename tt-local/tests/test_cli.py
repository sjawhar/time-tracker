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

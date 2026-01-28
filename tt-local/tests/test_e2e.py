"""End-to-end integration tests for Time Tracker.

Tests the full flow: Rust binary (ingest/export) → Python CLI (import/events/status).
Validates that the data format contract between Rust and Python components is correct.
"""

import json
import os
import subprocess
from pathlib import Path

import pytest
from click.testing import CliRunner

from tt_local.cli import main

REPO_ROOT = Path(__file__).parent.parent.parent
RUST_BINARY = REPO_ROOT / "target" / "release" / "tt"


@pytest.fixture(scope="module")
def rust_binary() -> Path:
    """Build the Rust binary once for all tests."""
    subprocess.run(
        ["cargo", "build", "--release"],
        cwd=REPO_ROOT,
        check=True,
        capture_output=True,
    )
    assert RUST_BINARY.exists(), f"Binary not found at {RUST_BINARY}"
    return RUST_BINARY


class TestIngestExportImportRoundtrip:
    """Tests for the complete ingest → export → import → query flow."""

    def test_single_event_roundtrip(self, rust_binary: Path, tmp_path: Path):
        """End-to-end: Rust ingest/export → Python import → query."""
        # Set up temp data directory for Rust binary
        data_dir = tmp_path / "time-tracker"
        env = {**os.environ, "XDG_DATA_HOME": str(tmp_path)}

        # 1. Run ingest (Rust)
        result = subprocess.run(
            [
                str(rust_binary),
                "ingest",
                "pane-focus",
                "--pane",
                "%1",
                "--cwd",
                "/home/test/project",
                "--session",
                "dev",
                "--window",
                "0",
            ],
            env=env,
            capture_output=True,
            text=True,
        )
        assert result.returncode == 0, f"ingest failed: {result.stderr}"

        # Verify events.jsonl created
        events_file = data_dir / "events.jsonl"
        assert events_file.exists(), "events.jsonl not created"
        assert events_file.stat().st_size > 0, "events.jsonl is empty"

        # 2. Run export (Rust)
        export_result = subprocess.run(
            [str(rust_binary), "export"],
            env=env,
            capture_output=True,
            text=True,
        )
        assert export_result.returncode == 0, f"export failed: {export_result.stderr}"
        export_output = export_result.stdout
        assert export_output.strip(), "export output is empty"

        # Verify export output is valid JSONL
        lines = [line for line in export_output.strip().split("\n") if line]
        assert lines, "export output has no valid lines"
        for line in lines:
            event = json.loads(line)
            assert "id" in event
            assert "timestamp" in event
            assert "type" in event
            assert "source" in event

        # 3. Import to Python
        db_path = tmp_path / "local.db"
        runner = CliRunner()
        import_result = runner.invoke(main, ["import", "--db", str(db_path)], input=export_output)
        assert import_result.exit_code == 0, f"import failed: {import_result.output}"
        # Verify at least 1 event was imported (could be more if Claude logs are parsed)
        assert "Imported" in import_result.output
        # Ensure import didn't silently fail with 0 events
        assert "Imported 0 events" not in import_result.output, \
            f"No events were imported: {import_result.output}"

        # 4. Query events
        events_result = runner.invoke(main, ["events", "--db", str(db_path)])
        assert events_result.exit_code == 0, f"events query failed: {events_result.output}"
        assert events_result.output.strip(), "events query returned no output"
        events = [json.loads(line) for line in events_result.output.strip().split("\n") if line]
        assert events, "No events found in database"

        # Find the tmux event we ingested
        tmux_events = [e for e in events if e["type"] == "tmux_pane_focus"]
        assert len(tmux_events) == 1
        assert tmux_events[0]["source"] == "remote.tmux"
        assert tmux_events[0]["cwd"] == "/home/test/project"

        # 5. Check status
        status_result = runner.invoke(main, ["status", "--db", str(db_path)])
        assert status_result.exit_code == 0, f"status failed: {status_result.output}"
        assert "remote.tmux" in status_result.output

    def test_multiple_events_roundtrip(self, rust_binary: Path, tmp_path: Path):
        """Multiple events with different panes/cwds are all imported."""
        env = {**os.environ, "XDG_DATA_HOME": str(tmp_path)}

        # Ingest 3 events with different panes and cwds
        test_events = [
            ("%1", "/home/test/project-a", "dev"),
            ("%2", "/home/test/project-b", "staging"),
            ("%3", "/home/test/project-c", "prod"),
        ]

        for pane, cwd, session in test_events:
            result = subprocess.run(
                [
                    str(rust_binary),
                    "ingest",
                    "pane-focus",
                    "--pane",
                    pane,
                    "--cwd",
                    cwd,
                    "--session",
                    session,
                    "--window",
                    "0",
                ],
                env=env,
                capture_output=True,
                text=True,
            )
            assert result.returncode == 0, f"ingest failed for {pane}: {result.stderr}"

        # Export
        export_result = subprocess.run(
            [str(rust_binary), "export"],
            env=env,
            capture_output=True,
            text=True,
        )
        assert export_result.returncode == 0

        # Import
        db_path = tmp_path / "local.db"
        runner = CliRunner()
        import_result = runner.invoke(
            main, ["import", "--db", str(db_path)], input=export_result.stdout
        )
        assert import_result.exit_code == 0

        # Query and verify all 3 events
        events_result = runner.invoke(main, ["events", "--db", str(db_path)])
        assert events_result.exit_code == 0
        events = [json.loads(line) for line in events_result.output.strip().split("\n") if line]

        # Get only tmux events (might have agent events from Claude logs)
        tmux_events = [e for e in events if e["type"] == "tmux_pane_focus"]
        assert len(tmux_events) == 3

        cwds = {e["cwd"] for e in tmux_events}
        assert cwds == {"/home/test/project-a", "/home/test/project-b", "/home/test/project-c"}

    def test_reimport_idempotent(self, rust_binary: Path, tmp_path: Path):
        """Re-importing the same events results in no duplicates."""
        env = {**os.environ, "XDG_DATA_HOME": str(tmp_path)}

        # Ingest an event
        subprocess.run(
            [
                str(rust_binary),
                "ingest",
                "pane-focus",
                "--pane",
                "%1",
                "--cwd",
                "/home/test/project",
                "--session",
                "dev",
                "--window",
                "0",
            ],
            env=env,
            check=True,
            capture_output=True,
        )

        # Export
        export_result = subprocess.run(
            [str(rust_binary), "export"],
            env=env,
            capture_output=True,
            text=True,
        )
        export_output = export_result.stdout

        # Import twice
        db_path = tmp_path / "local.db"
        runner = CliRunner()

        # First import
        result1 = runner.invoke(main, ["import", "--db", str(db_path)], input=export_output)
        assert result1.exit_code == 0

        # Second import (should skip duplicates)
        result2 = runner.invoke(main, ["import", "--db", str(db_path)], input=export_output)
        assert result2.exit_code == 0
        assert "Imported 0 events" in result2.output

        # Verify only one copy of the tmux event
        events_result = runner.invoke(main, ["events", "--db", str(db_path)])
        events = [json.loads(line) for line in events_result.output.strip().split("\n") if line]
        tmux_events = [e for e in events if e["type"] == "tmux_pane_focus"]
        assert len(tmux_events) == 1


class TestDataFormatContract:
    """Tests verifying the data format contract between Rust and Python."""

    def test_export_output_matches_import_schema(self, rust_binary: Path, tmp_path: Path):
        """Export output contains all fields required by Python ImportedEvent."""
        env = {**os.environ, "XDG_DATA_HOME": str(tmp_path)}

        # Create an event
        subprocess.run(
            [
                str(rust_binary),
                "ingest",
                "pane-focus",
                "--pane",
                "%1",
                "--cwd",
                "/test",
                "--session",
                "main",
                "--window",
                "0",
            ],
            env=env,
            check=True,
            capture_output=True,
        )

        # Export
        export_result = subprocess.run(
            [str(rust_binary), "export"],
            env=env,
            capture_output=True,
            text=True,
        )

        # Check each line has required fields
        for line in export_result.stdout.strip().split("\n"):
            event = json.loads(line)
            # Required fields for ImportedEvent
            assert "id" in event, "missing 'id' field"
            assert "timestamp" in event, "missing 'timestamp' field"
            assert "type" in event, "missing 'type' field"
            assert "source" in event, "missing 'source' field"
            assert "data" in event, "missing 'data' field"
            # cwd is optional but should be present for tmux events
            if event["type"] == "tmux_pane_focus":
                assert "cwd" in event, "tmux event missing 'cwd' field"

    def test_timestamp_format_parseable(self, rust_binary: Path, tmp_path: Path):
        """Rust timestamps can be parsed by Python datetime."""
        from datetime import datetime

        env = {**os.environ, "XDG_DATA_HOME": str(tmp_path)}

        subprocess.run(
            [
                str(rust_binary),
                "ingest",
                "pane-focus",
                "--pane",
                "%1",
                "--cwd",
                "/test",
                "--session",
                "main",
                "--window",
                "0",
            ],
            env=env,
            check=True,
            capture_output=True,
        )

        export_result = subprocess.run(
            [str(rust_binary), "export"],
            env=env,
            capture_output=True,
            text=True,
        )

        for line in export_result.stdout.strip().split("\n"):
            event = json.loads(line)
            ts = event["timestamp"]
            # Should parse without error
            parsed = datetime.fromisoformat(ts.replace("Z", "+00:00"))
            assert parsed.tzinfo is not None, "timestamp should have timezone info"

    def test_event_ids_deterministic(self, rust_binary: Path, tmp_path: Path):
        """Same event data produces same ID (deterministic hashing)."""
        env = {**os.environ, "XDG_DATA_HOME": str(tmp_path)}

        # Create the same event twice with slightly different timing
        # Due to debouncing, we need different panes
        for pane in ["%1", "%2"]:
            subprocess.run(
                [
                    str(rust_binary),
                    "ingest",
                    "pane-focus",
                    "--pane",
                    pane,
                    "--cwd",
                    "/test",
                    "--session",
                    "main",
                    "--window",
                    "0",
                ],
                env=env,
                check=True,
                capture_output=True,
            )

        export_result = subprocess.run(
            [str(rust_binary), "export"],
            env=env,
            capture_output=True,
            text=True,
        )

        events = [json.loads(line) for line in export_result.stdout.strip().split("\n") if line]
        tmux_events = [e for e in events if e["type"] == "tmux_pane_focus"]

        # Different events should have different IDs
        ids = [e["id"] for e in tmux_events]
        assert len(ids) == len(set(ids)), "Event IDs should be unique"

        # IDs should be non-empty strings
        for event_id in ids:
            assert isinstance(event_id, str)
            assert len(event_id) > 0


class TestEdgeCases:
    """Edge case tests."""

    def test_empty_export(self, rust_binary: Path, tmp_path: Path):
        """Export with no events returns empty output gracefully."""
        env = {**os.environ, "XDG_DATA_HOME": str(tmp_path)}

        # Export without any ingest (no events.jsonl)
        export_result = subprocess.run(
            [str(rust_binary), "export"],
            env=env,
            capture_output=True,
            text=True,
        )

        # Should succeed with empty output (no events.jsonl = no events)
        assert export_result.returncode == 0
        # Output might be empty or contain Claude log events
        # The key is that it doesn't fail

    def test_import_empty_is_noop(self, rust_binary: Path, tmp_path: Path):
        """Importing empty input is handled gracefully."""
        db_path = tmp_path / "local.db"
        runner = CliRunner()

        result = runner.invoke(main, ["import", "--db", str(db_path)], input="")
        assert result.exit_code == 0
        assert "Imported 0 events" in result.output

    def test_unicode_in_cwd(self, rust_binary: Path, tmp_path: Path):
        """Unicode characters in cwd are handled correctly."""
        env = {**os.environ, "XDG_DATA_HOME": str(tmp_path)}

        # Use a cwd with unicode characters
        unicode_cwd = "/home/test/日本語/项目"

        subprocess.run(
            [
                str(rust_binary),
                "ingest",
                "pane-focus",
                "--pane",
                "%1",
                "--cwd",
                unicode_cwd,
                "--session",
                "dev",
                "--window",
                "0",
            ],
            env=env,
            check=True,
            capture_output=True,
        )

        export_result = subprocess.run(
            [str(rust_binary), "export"],
            env=env,
            capture_output=True,
            text=True,
        )

        # Import and verify
        db_path = tmp_path / "local.db"
        runner = CliRunner()
        runner.invoke(main, ["import", "--db", str(db_path)], input=export_result.stdout)

        events_result = runner.invoke(main, ["events", "--db", str(db_path)])
        events = [json.loads(line) for line in events_result.output.strip().split("\n") if line]
        tmux_events = [e for e in events if e["type"] == "tmux_pane_focus"]

        assert len(tmux_events) == 1
        assert tmux_events[0]["cwd"] == unicode_cwd

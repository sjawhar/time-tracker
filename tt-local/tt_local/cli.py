"""CLI entry point for Time Tracker."""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import click
from pydantic import ValidationError

from tt_local.db import EventStore, ImportedEvent

DEFAULT_DB_PATH = Path.home() / ".local" / "share" / "tt" / "events.db"


@click.group()
def main():
    """Time Tracker local CLI."""
    pass


@main.command("import")
@click.option(
    "--db",
    type=click.Path(path_type=Path),
    default=DEFAULT_DB_PATH,
    help="Path to SQLite database",
)
def import_events(db: Path):
    """Import events from stdin (JSONL format).

    Reads events from remote `tt export` output and inserts them into the local
    SQLite database. Duplicate events (same ID) are silently skipped.

    Example usage:
        ssh remote "tt export" | tt import
        cat events.jsonl | tt import
    """
    # Ensure database directory exists
    db.parent.mkdir(parents=True, exist_ok=True)

    imported_count = 0
    valid_count = 0
    has_input = False

    with EventStore.open(db) as store:
        for line_number, line in enumerate(sys.stdin, 1):
            stripped = line.strip()
            if not stripped:
                continue

            has_input = True

            try:
                data = json.loads(stripped)
                event = ImportedEvent.model_validate(data)
                valid_count += 1
                if store.insert_imported_event(event):
                    imported_count += 1
            except json.JSONDecodeError as e:
                click.echo(f"Warning: line {line_number}: invalid JSON: {e}", err=True)
            except ValidationError as e:
                click.echo(f"Warning: line {line_number}: validation error: {e}", err=True)

    click.echo(f"Imported {imported_count} events")

    # Exit code 1 if we had input but no valid events (all lines were errors)
    if has_input and valid_count == 0:
        sys.exit(1)


@main.command("sync")
@click.argument("remote")
@click.option(
    "--db",
    type=click.Path(path_type=Path),
    default=DEFAULT_DB_PATH,
    help="Path to SQLite database",
)
@click.option(
    "--timeout",
    type=int,
    default=60,
    help="SSH timeout in seconds",
)
def sync_events(remote: str, db: Path, timeout: int) -> None:
    """Sync events from a remote machine via SSH.

    Connects to REMOTE via SSH, runs `tt export` to get events,
    and imports them into the local database.

    Example:
        tt sync user@devserver
        tt sync devserver --db ~/custom.db
    """
    # Ensure database directory exists
    db.parent.mkdir(parents=True, exist_ok=True)

    # Execute SSH command (list args, NOT shell=True, to prevent command injection)
    try:
        result = subprocess.run(
            ["ssh", remote, "tt", "export"],
            capture_output=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        click.echo(f"SSH timed out after {timeout}s", err=True)
        sys.exit(1)

    # Handle SSH failures
    if result.returncode == 255:
        stderr = result.stderr.decode("utf-8", errors="replace").strip()
        click.echo(f"SSH connection failed: {stderr}", err=True)
        sys.exit(1)
    elif result.returncode != 0:
        stderr = result.stderr.decode("utf-8", errors="replace").strip()
        click.echo(f"Remote 'tt export' failed (exit {result.returncode}): {stderr}", err=True)
        sys.exit(1)

    # Parse and import events
    output = result.stdout.decode("utf-8", errors="replace")

    if not output.strip():
        click.echo(f"No events to import from {remote}")
        return

    imported_count = 0
    total_valid = 0
    has_input = False

    with EventStore.open(db) as store:
        for line_number, line in enumerate(output.splitlines(), 1):
            stripped = line.strip()
            if not stripped:
                continue

            has_input = True

            try:
                data = json.loads(stripped)
                event = ImportedEvent.model_validate(data)
                total_valid += 1
                if store.insert_imported_event(event):
                    imported_count += 1
            except json.JSONDecodeError as e:
                click.echo(f"Warning: line {line_number}: invalid JSON: {e}", err=True)
            except ValidationError as e:
                click.echo(f"Warning: line {line_number}: validation error: {e}", err=True)

    skipped = total_valid - imported_count
    click.echo(f"Synced from {remote}: {imported_count} new events ({total_valid} received, {skipped} already existed)")

    # Exit code 1 if we had input but no valid events (all lines were errors)
    if has_input and total_valid == 0:
        sys.exit(1)


@main.command("events")
@click.option(
    "--db",
    type=click.Path(path_type=Path),
    default=DEFAULT_DB_PATH,
    help="Path to SQLite database",
)
@click.option(
    "--since",
    help="ISO 8601 timestamp (show events at or after this time)",
)
@click.option(
    "--type",
    "event_type",
    help="Filter by event type",
)
@click.option(
    "--limit",
    type=int,
    help="Maximum number of events to output",
)
def events_command(
    db: Path,
    since: str | None,
    event_type: str | None,
    limit: int | None,
) -> None:
    """Query events from local database.

    Outputs events in JSONL format for debugging and analysis.

    Example:
        tt events
        tt events --since 2025-01-25T10:00:00Z
        tt events --type tmux_pane_focus --limit 10
    """
    if not db.exists():
        click.echo("No database found", err=True)
        sys.exit(1)

    with EventStore.open(db) as store:
        for event in store.get_events(
            start=since,
            event_type=event_type,
            limit=limit,
        ):
            click.echo(json.dumps(event))


if __name__ == "__main__":
    main()

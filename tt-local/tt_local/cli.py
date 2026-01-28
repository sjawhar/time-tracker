"""CLI entry point for Time Tracker."""

from __future__ import annotations

import json
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

import click
from pydantic import ValidationError

from tt_local.db import EventStore, ImportedEvent


def format_relative_time(iso_timestamp: str, *, now: datetime | None = None) -> str:
    """Format ISO timestamp as relative time (e.g., '5 minutes ago').

    Args:
        iso_timestamp: ISO 8601 timestamp string
        now: Optional current time for testing (defaults to UTC now)

    Returns:
        Relative time string, or the raw timestamp if parsing fails.
    """
    try:
        dt = datetime.fromisoformat(iso_timestamp.replace("Z", "+00:00"))
    except ValueError:
        # Malformed timestamp - return raw value
        return iso_timestamp

    if now is None:
        now = datetime.now(timezone.utc)
    delta = now - dt

    seconds = delta.total_seconds()
    if seconds < 0:
        # Future timestamp (clock skew from remote)
        return "just now"
    elif seconds < 60:
        return "just now"
    elif seconds < 3600:
        minutes = int(seconds / 60)
        return f"{minutes} minute{'s' if minutes != 1 else ''} ago"
    elif seconds < 86400:
        hours = int(seconds / 3600)
        return f"{hours} hour{'s' if hours != 1 else ''} ago"
    else:
        days = int(seconds / 86400)
        return f"{days} day{'s' if days != 1 else ''} ago"

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


@main.command("status")
@click.option(
    "--db",
    type=click.Path(path_type=Path),
    default=DEFAULT_DB_PATH,
    help="Path to SQLite database",
)
def status_command(db: Path) -> None:
    """Show time tracker status.

    Displays the last event time for each source and overall stats.
    """
    if not db.exists():
        click.echo("No database found", err=True)
        sys.exit(1)

    with EventStore.open(db) as store:
        sources = store.get_last_event_per_source()

    if not sources:
        click.echo("No events recorded")
        return

    click.echo(f"Database: {db}")
    click.echo()

    total_events = sum(s["event_count"] for s in sources)
    click.echo(f"Total events: {total_events}")
    click.echo()

    click.echo("Last event per source:")
    for source in sources:
        relative = format_relative_time(source["last_timestamp"])
        click.echo(f"  {source['source']}: {relative} ({source['event_count']} events)")


if __name__ == "__main__":
    main()

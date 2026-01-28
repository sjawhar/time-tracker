"""CLI entry point for Time Tracker."""

from __future__ import annotations

import json
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


if __name__ == "__main__":
    main()

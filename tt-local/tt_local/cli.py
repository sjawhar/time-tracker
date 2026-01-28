"""CLI entry point for Time Tracker."""

from __future__ import annotations

import json
import subprocess
import sys
from collections import defaultdict
from datetime import datetime, timedelta, timezone
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


def format_duration(ms: int) -> str:
    """Format milliseconds as 'Xh Ym', 'Ym', or '<1m'.

    Args:
        ms: Duration in milliseconds.

    Returns:
        Formatted duration string.
    """
    if ms < 60_000:  # Less than 1 minute
        return "<1m" if ms > 0 else "0m"
    total_minutes = ms // 60_000
    hours = total_minutes // 60
    minutes = total_minutes % 60
    if hours > 0:
        return f"{hours}h {minutes:2d}m"
    return f"{minutes}m"


def get_week_range(date: datetime | None = None) -> tuple[str, str]:
    """Get Monday 00:00:00 to next Monday 00:00:00 local time.

    Args:
        date: Date within the week (default: now).

    Returns:
        Tuple of (start, end) ISO 8601 strings (start inclusive, end exclusive).
    """
    if date is None:
        date = datetime.now().astimezone()  # Local timezone-aware
    elif date.tzinfo is None:
        date = date.astimezone()  # Make timezone-aware

    # Find Monday of this week
    monday = date - timedelta(days=date.weekday())
    monday = monday.replace(hour=0, minute=0, second=0, microsecond=0)
    next_monday = monday + timedelta(days=7)  # Exclusive end

    return monday.isoformat(), next_monday.isoformat()


def get_day_range(date: datetime | None = None) -> tuple[str, str]:
    """Get start of day to start of next day in local time.

    Args:
        date: Date to get range for (default: today).

    Returns:
        Tuple of (start, end) ISO 8601 strings (start inclusive, end exclusive).
    """
    if date is None:
        date = datetime.now().astimezone()
    elif date.tzinfo is None:
        date = date.astimezone()

    start = date.replace(hour=0, minute=0, second=0, microsecond=0)
    end = start + timedelta(days=1)  # Exclusive end

    return start.isoformat(), end.isoformat()


def format_date_range(start: str, end: str, period: str) -> str:
    """Format date range for report header.

    Args:
        start: ISO 8601 start timestamp.
        end: ISO 8601 end timestamp (exclusive).
        period: "day" or "week".

    Returns:
        Formatted string like "Jan 20-26, 2025" or "Jan 28, 2025".
    """
    start_dt = datetime.fromisoformat(start)
    # Subtract 1 second from end to get the last inclusive day
    end_dt = datetime.fromisoformat(end) - timedelta(seconds=1)

    if period == "day":
        return start_dt.strftime("%b %d, %Y")

    # Week range
    if start_dt.month == end_dt.month:
        return f"{start_dt.strftime('%b')} {start_dt.day}-{end_dt.day}, {start_dt.year}"
    elif start_dt.year == end_dt.year:
        return f"{start_dt.strftime('%b %d')} - {end_dt.strftime('%b %d')}, {start_dt.year}"
    else:
        return f"{start_dt.strftime('%b %d, %Y')} - {end_dt.strftime('%b %d, %Y')}"


def make_progress_bar(value: int, max_value: int, width: int = 16) -> str:
    """Create ASCII progress bar.

    Args:
        value: Current value.
        max_value: Maximum value (100%).
        width: Total width of bar (default: 16).

    Returns:
        Progress bar string like '████████░░░░░░░░'.
    """
    if max_value == 0 or value == 0:
        return "░" * width
    filled = max(1, min(width, round((value / max_value) * width)))
    return "█" * filled + "░" * (width - filled)


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


@main.command("report")
@click.option(
    "--db",
    type=click.Path(path_type=Path),
    default=DEFAULT_DB_PATH,
    help="Path to SQLite database",
)
@click.option(
    "--week",
    "period",
    flag_value="week",
    default=True,
    help="Weekly report (Mon-Sun)",
)
@click.option(
    "--day",
    "day_date",
    type=str,
    default=None,
    is_flag=False,
    flag_value="today",
    help="Daily report (YYYY-MM-DD, default: today)",
)
@click.option(
    "--json",
    "output_json",
    is_flag=True,
    help="Output as JSON",
)
def report_command(
    db: Path, period: str, day_date: str | None, output_json: bool
) -> None:
    """Show time report by tag.

    By default shows the current week (Monday-Sunday). Use --day for a single
    day report, optionally with a specific date in YYYY-MM-DD format.
    """
    if not db.exists():
        click.echo("No database found", err=True)
        sys.exit(1)

    # Parse date range
    if day_date is not None:
        period_type = "day"
        if day_date == "today":
            start, end = get_day_range()
        else:
            try:
                date = datetime.strptime(day_date, "%Y-%m-%d")
            except ValueError:
                click.echo(f"Invalid date format: {day_date}. Use YYYY-MM-DD.", err=True)
                sys.exit(1)
            start, end = get_day_range(date)
    else:
        period_type = "week"
        start, end = get_week_range()

    with EventStore.open(db) as store:
        # Run stream inference to ensure events are assigned to streams
        store.run_stream_inference()

        # Calculate time per stream
        time_by_stream = store.calculate_time(start, end)

        # Get stream tags (filtered by active streams)
        stream_ids = list(time_by_stream.keys())
        stream_tags = store.get_stream_tags(stream_ids) if stream_ids else {}

        # Count days with data
        days_with_data = store.count_days_with_data(start, end)

    # Check for empty data
    total_ms = sum(t["direct_ms"] + t["delegated_ms"] for t in time_by_stream.values())
    if total_ms == 0:
        if output_json:
            _output_json_report(period_type, start, end, days_with_data, time_by_stream, [])
        else:
            click.echo(f"Time Report: {format_date_range(start, end, period_type)}")
            click.echo()
            click.echo("No time tracked for this period.")
            click.echo()
            click.echo("Run 'tt status' to check if collectors are running.")
        return

    # Aggregate by tag
    tag_data: dict[str | None, dict[str, int]] = defaultdict(
        lambda: {"total_ms": 0, "direct_ms": 0, "delegated_ms": 0, "stream_count": 0}
    )

    for stream_id, times in time_by_stream.items():
        tags = stream_tags.get(stream_id, [])
        if not tags:
            tags = [None]  # Untagged

        # Skip streams with zero time
        if times["direct_ms"] == 0 and times["delegated_ms"] == 0:
            continue

        for tag in tags:
            tag_data[tag]["total_ms"] += times["direct_ms"] + times["delegated_ms"]
            tag_data[tag]["direct_ms"] += times["direct_ms"]
            tag_data[tag]["delegated_ms"] += times["delegated_ms"]
            tag_data[tag]["stream_count"] += 1

    # Sort tags: by total descending, untagged last
    def sort_key(item: tuple[str | None, dict[str, int]]) -> tuple[int, int]:
        tag, data = item
        if tag is None:
            return (1, 0)  # Untagged always last
        return (0, -data["total_ms"])

    sorted_tags = sorted(tag_data.items(), key=sort_key)

    if output_json:
        _output_json_report(period_type, start, end, days_with_data, time_by_stream, sorted_tags)
    else:
        _output_human_report(period_type, start, end, days_with_data, time_by_stream, sorted_tags)


def _output_json_report(
    period_type: str,
    start: str,
    end: str,
    days_with_data: int,
    time_by_stream: dict[str, dict[str, int]],
    sorted_tags: list[tuple[str | None, dict[str, int]]],
) -> None:
    """Output JSON report."""
    # Extract date-only strings
    start_date = datetime.fromisoformat(start).strftime("%Y-%m-%d")
    end_date = (datetime.fromisoformat(end) - timedelta(seconds=1)).strftime("%Y-%m-%d")

    # Calculate header totals from stream times (not tag totals) to avoid
    # double-counting multi-tagged streams
    total_ms = sum(t["direct_ms"] + t["delegated_ms"] for t in time_by_stream.values())
    direct_ms = sum(t["direct_ms"] for t in time_by_stream.values())
    delegated_ms = sum(t["delegated_ms"] for t in time_by_stream.values())

    output = {
        "report_type": "daily" if period_type == "day" else "weekly",
        "generated_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "period": {
            "start": start_date,
            "end": end_date,
            "days_with_data": days_with_data,
        },
        "total_ms": total_ms,
        "direct_ms": direct_ms,
        "delegated_ms": delegated_ms,
        "by_tag": [
            {
                "tag": tag,
                "total_ms": data["total_ms"],
                "direct_ms": data["direct_ms"],
                "delegated_ms": data["delegated_ms"],
                "stream_count": data["stream_count"],
            }
            for tag, data in sorted_tags
        ],
    }
    click.echo(json.dumps(output, indent=2))


def _output_human_report(
    period_type: str,
    start: str,
    end: str,
    days_with_data: int,
    time_by_stream: dict[str, dict[str, int]],
    sorted_tags: list[tuple[str | None, dict[str, int]]],
) -> None:
    """Output human-readable report."""
    # Calculate header totals from stream times
    total_ms = sum(t["direct_ms"] + t["delegated_ms"] for t in time_by_stream.values())
    direct_ms = sum(t["direct_ms"] for t in time_by_stream.values())
    delegated_ms = sum(t["delegated_ms"] for t in time_by_stream.values())

    # Format header
    header = format_date_range(start, end, period_type)
    if period_type == "week" and days_with_data < 7:
        header += f" ({days_with_data} days with data)"

    click.echo(f"Time Report: {header}")
    click.echo()

    # Total with breakdown
    click.echo(f"Total: {format_duration(total_ms)}")
    direct_pct = round(direct_ms * 100 / total_ms) if total_ms > 0 else 0
    delegated_pct = 100 - direct_pct
    click.echo(f"  Direct:    {format_duration(direct_ms):>9} ({direct_pct}%)")
    click.echo(f"  Delegated: {format_duration(delegated_ms):>9} ({delegated_pct}%)")
    click.echo()

    # By tag table
    click.echo("By Tag:")
    click.echo("                    Total     Direct    Delegated")

    # Find max for progress bar scaling
    max_total = max((data["total_ms"] for _, data in sorted_tags), default=0)

    for tag, data in sorted_tags:
        # Truncate long tags
        display_tag = tag if tag else "(untagged)"
        if len(display_tag) > 20:
            display_tag = display_tag[:17] + "..."

        bar = make_progress_bar(data["total_ms"], max_total)
        click.echo(
            f"  {display_tag:<20} {format_duration(data['total_ms']):>9} "
            f"{format_duration(data['direct_ms']):>9} {format_duration(data['delegated_ms']):>9}   {bar}"
        )


if __name__ == "__main__":
    main()

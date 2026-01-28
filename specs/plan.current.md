# Implement `tt status` Command

**Task**: Implement `tt status` command (show last event time per source)

## Spec Reference

The task is described in `specs/plan.md`:
> Implement `tt status` command (show last event time per source)

Additionally, the CLI spec (`specs/design/ux-cli.md`) has a preliminary sketch:
```bash
tt status                    # Show current contexts, running agents, today's summary
```

For the prototype phase, we'll implement a minimal version: show the most recent event timestamp for each source.

## Acceptance Criteria

1. `tt status` displays last event time grouped by `source` column
2. Shows human-readable relative time (e.g., "5 minutes ago")
3. Shows database path and total event count
4. Exits with code 0 on success
5. Exits with code 1 if database doesn't exist (consistent with `tt events`)

## Files to Modify

| File | Change |
|------|--------|
| `tt-local/tt_local/cli.py` | Add `status` command |
| `tt-local/tt_local/db.py` | Add `get_last_event_per_source()` method |
| `tt-local/tests/test_cli.py` | Add tests for `status` command |

## Implementation Approach

### 1. Add database query method

In `db.py`, add:
```python
def get_last_event_per_source(self) -> list[dict[str, Any]]:
    """Get the most recent event timestamp for each source.

    Returns list of dicts with keys: source, last_timestamp, event_count
    """
    cursor = self._conn.execute("""
        SELECT
            source,
            MAX(timestamp) as last_timestamp,
            COUNT(*) as event_count
        FROM events
        GROUP BY source
        ORDER BY last_timestamp DESC
    """)
    return [dict(row) for row in cursor.fetchall()]
```

### 2. Add status command

In `cli.py`, add:
```python
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
```

### 3. Add relative time formatting

```python
from datetime import datetime, timezone

def format_relative_time(iso_timestamp: str, *, now: datetime | None = None) -> str:
    """Format ISO timestamp as relative time (e.g., '5 minutes ago').

    Args:
        iso_timestamp: ISO 8601 timestamp string
        now: Optional current time for testing (defaults to UTC now)
    """
    dt = datetime.fromisoformat(iso_timestamp.replace("Z", "+00:00"))
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
```

Note: The `now` parameter allows deterministic testing without mocking.

## Test Cases

1. **test_status_no_database**: No database file → exit 1, "No database found"
2. **test_status_empty_database**: Database exists but no events → "No events recorded"
3. **test_status_single_source**: One source → shows source with count and relative time
4. **test_status_multiple_sources**: Multiple sources → sorted by most recent first
5. **test_format_relative_time**: Verify relative time formatting for various deltas
6. **test_format_relative_time_boundaries**: Test boundary conditions:
   - 59s → "just now"
   - 60s → "1 minute ago"
   - 3599s → "59 minutes ago"
   - 3600s → "1 hour ago"
   - 86399s → "23 hours ago"
   - 86400s → "1 day ago"
7. **test_format_relative_time_future**: Future timestamp (clock skew) → "just now"

## Output Format Example

```
Database: /home/user/.local/share/tt/events.db

Total events: 1234

Last event per source:
  remote.tmux: 5 minutes ago (800 events)
  remote.agent: 2 hours ago (434 events)
```

## No Open Questions

Implementation is straightforward following existing patterns.

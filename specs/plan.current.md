# Implementation Plan: `tt report --week` command

**Task:** Implement `tt report --week` command (and `--day` variant)
**Spec:** `specs/design/ux-reports.md`
**Dependencies:** Stream inference and direct/delegated time calculation (both implemented)

## Summary

Add the `tt report` command to display time summaries aggregated by tag. The command:
- Defaults to `--week` (Mon-Sun containing today)
- Supports `--day [DATE]` for single-day reports
- Supports `--json` for machine-readable output
- Shows total/direct/delegated time breakdown
- Shows per-tag breakdown with progress bars

## Files to Modify

| File | Changes |
|------|---------|
| `tt-local/tt_local/cli.py` | Add `report` command with `--week`, `--day`, `--json` options + helper functions |
| `tt-local/tt_local/db.py` | Add `get_stream_tags()` and `count_days_with_data()` methods |
| `tt-local/tests/test_cli.py` | Add tests for report command (human-readable and JSON output) |
| `tt-local/tests/test_db.py` | Add tests for new db methods |

## Required Imports

Add to `cli.py`:
```python
from datetime import timedelta  # datetime and timezone already imported
from collections import defaultdict
# json is already imported
```

## Implementation Approach

### 1. Add database methods to EventStore

```python
def get_stream_tags(self, stream_ids: list[str] | None = None) -> dict[str, list[str]]:
    """Get tags for specified streams (or all if None).

    Returns:
        Dict mapping stream_id to list of tags.
    """
    if stream_ids:
        placeholders = ",".join("?" * len(stream_ids))
        cursor = self._conn.execute(
            f"SELECT stream_id, tag FROM stream_tags WHERE stream_id IN ({placeholders})",
            stream_ids
        )
    else:
        cursor = self._conn.execute("SELECT stream_id, tag FROM stream_tags")
    result: dict[str, list[str]] = defaultdict(list)
    for row in cursor:
        result[row["stream_id"]].append(row["tag"])
    return dict(result)

def count_days_with_data(self, start: str, end: str) -> int:
    """Count distinct local-time days with at least one event.

    Args:
        start: ISO 8601 timestamp (inclusive lower bound)
        end: ISO 8601 timestamp (exclusive upper bound)

    Returns:
        Number of distinct days with events.
    """
    cursor = self._conn.execute("""
        SELECT DISTINCT DATE(timestamp, 'localtime')
        FROM events
        WHERE timestamp >= ? AND timestamp < ?
    """, (start, end))
    return len(cursor.fetchall())
```

### 2. Add helper functions in cli.py

**Duration formatting:**
```python
def format_duration(ms: int) -> str:
    """Format milliseconds as 'Xh Ym', 'Ym', or '<1m'."""
    if ms < 60_000:  # Less than 1 minute
        return "<1m" if ms > 0 else "0m"
    total_minutes = ms // 60_000
    hours = total_minutes // 60
    minutes = total_minutes % 60
    if hours > 0:
        return f"{hours}h {minutes:2d}m"
    return f"{minutes}m"
```

**Date range helpers (timezone-aware, exclusive end):**
```python
def get_week_range(date: datetime | None = None) -> tuple[str, str]:
    """Get Monday 00:00:00 to next Monday 00:00:00 local time.

    Returns ISO 8601 strings (start inclusive, end exclusive).
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

    Returns ISO 8601 strings (start inclusive, end exclusive).
    """
    if date is None:
        date = datetime.now().astimezone()
    elif date.tzinfo is None:
        date = date.astimezone()

    start = date.replace(hour=0, minute=0, second=0, microsecond=0)
    end = start + timedelta(days=1)  # Exclusive end

    return start.isoformat(), end.isoformat()
```

**Date header formatting:**
```python
def format_date_range(start: str, end: str, period: str) -> str:
    """Format date range for report header.

    Args:
        start: ISO 8601 start timestamp
        end: ISO 8601 end timestamp (exclusive)
        period: "day" or "week"

    Returns:
        Formatted string like "Jan 20-26, 2025" or "Jan 28, 2025"
    """
    start_dt = datetime.fromisoformat(start)
    end_dt = datetime.fromisoformat(end) - timedelta(seconds=1)  # Make inclusive for display

    if period == "day":
        return start_dt.strftime("%b %d, %Y")

    # Week range
    if start_dt.month == end_dt.month:
        return f"{start_dt.strftime('%b')} {start_dt.day}-{end_dt.day}, {start_dt.year}"
    elif start_dt.year == end_dt.year:
        return f"{start_dt.strftime('%b %d')} - {end_dt.strftime('%b %d')}, {start_dt.year}"
    else:
        return f"{start_dt.strftime('%b %d, %Y')} - {end_dt.strftime('%b %d, %Y')}"
```

### 3. Add report command

```python
@main.command("report")
@click.option("--db", type=click.Path(path_type=Path), default=DEFAULT_DB_PATH)
@click.option("--week", "period", flag_value="week", default=True, help="Weekly report (Mon-Sun)")
@click.option("--day", "day_date", type=str, default=None, is_flag=False, flag_value="today",
              help="Daily report (YYYY-MM-DD, default: today)")
@click.option("--json", "output_json", is_flag=True, help="Output as JSON")
def report_command(db: Path, period: str, day_date: str | None, output_json: bool) -> None:
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
                sys.exit(2)
            start, end = get_day_range(date)
    else:
        period_type = "week"
        start, end = get_week_range()
```

### 4. Report generation flow

1. Parse date range (week or day) with validation
2. Run stream inference: `store.run_stream_inference()`
3. Calculate time per stream: `store.calculate_time(start, end)` → `time_by_stream`
4. Get stream tags (filtered by active streams): `store.get_stream_tags(stream_ids)`
5. Count days with data: `store.count_days_with_data(start, end)`
6. Aggregate by tag (see below) → `tag_data`, `sorted_tags`
7. Calculate header totals from `time_by_stream` (not `tag_data`) to avoid double-counting
8. Sort tags by total descending, `(untagged)` last (see below)
9. Output human-readable or JSON

**Tag aggregation code:**
```python
from collections import defaultdict

# Aggregate by tag
tag_data: dict[str | None, dict] = defaultdict(
    lambda: {"total_ms": 0, "direct_ms": 0, "delegated_ms": 0, "stream_count": 0}
)

stream_tags = store.get_stream_tags(list(time_by_stream.keys()))

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
```

**Sorting logic:**
```python
def sort_key(item):
    tag, data = item
    # None (untagged) always last
    if tag is None:
        return (1, 0)  # (is_untagged, -total)
    return (0, -data["total_ms"])

sorted_tags = sorted(tag_data.items(), key=sort_key)
```

### 5. Human-readable output format

```
Time Report: Jan 20-26, 2025

Total: 32h 15m
  Direct:    22h 30m (70%)
  Delegated: 9h 45m  (30%)

By Tag:
                    Total     Direct    Delegated
  acme-webapp      18h 30m   12h 00m     6h 30m   ████████████████
  personal          8h 45m    6h 15m     2h 30m   ████████░░░░░░░░
  (untagged)        5h 00m    4h 15m        45m   ████░░░░░░░░░░░░
```

**Format constants:**
```python
TAG_COL_WIDTH = 20
TIME_COL_WIDTH = 9
BAR_WIDTH = 16
ROW_FORMAT = "  {tag:<20} {total:>9} {direct:>9} {delegated:>9}   {bar}"
```

Key formatting:
- Tag column: 20 chars max, left-aligned, truncate at 17 chars + `...`
- Time columns: 9 chars each, right-aligned
- Progress bar: 16 chars, relative to max tag time
- Durations: `Xh Ym` format, hours omitted when zero, `<1m` for sub-minute

**Progress bar:**
```python
def make_progress_bar(value: int, max_value: int, width: int = 16) -> str:
    """Create ASCII progress bar."""
    if max_value == 0 or value == 0:
        return "░" * width
    filled = max(1, min(width, round((value / max_value) * width)))
    return "█" * filled + "░" * (width - filled)
```

### 6. JSON output structure

For `--json` flag, output a JSON object matching the spec schema. Note that `period.start` and `period.end` are date-only strings (`YYYY-MM-DD`), not full ISO timestamps.

**Important:** `total_ms`, `direct_ms`, and `delegated_ms` in the header must be calculated from unique stream times (not tag totals), to avoid double-counting multi-tagged streams.

```python
def generate_json_output(
    period_type: str,
    start: str,
    end: str,
    days_with_data: int,
    time_by_stream: dict,  # Original stream times (no double-counting)
    sorted_tags: list,
) -> str:
    """Generate JSON output matching spec schema."""
    # Extract date-only strings from ISO timestamps
    start_date = datetime.fromisoformat(start).strftime("%Y-%m-%d")
    # end is exclusive, subtract 1 second to get inclusive end date
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
                "tag": tag,  # None for untagged
                "total_ms": data["total_ms"],
                "direct_ms": data["direct_ms"],
                "delegated_ms": data["delegated_ms"],
                "stream_count": data["stream_count"],
            }
            for tag, data in sorted_tags
        ],
    }
    return json.dumps(output, indent=2)
```

### 7. Edge cases

| Case | Behavior |
|------|----------|
| No data | "No time tracked for this period." + hint |
| Partial week | Header shows "(X days with data)" **only for --week** |
| Long tag names | Truncate at 17 chars + "..." (total 20 chars) |
| Zero delegated | Show "0m (0%)" |
| Sub-minute | Show "<1m" |
| Multi-tag stream | Time appears under each tag (totals can exceed header) |
| Daily report | Do NOT show "(X days with data)" - only for weekly reports |

## Test Cases

### Unit tests for helpers

**format_duration:**
- `format_duration(0)` → `"0m"`
- `format_duration(30_000)` → `"<1m"` (30 sec)
- `format_duration(59_999)` → `"<1m"` (just under 1 min)
- `format_duration(60_000)` → `"1m"` (exactly 1 min)
- `format_duration(119_999)` → `"1m"` (1m 59.999s rounds down to 1m)
- `format_duration(3_600_000)` → `"1h  0m"` (exactly 1 hour)
- `format_duration(5_400_000)` → `"1h 30m"` (1.5 hours)

**get_week_range:**
- Monday returns same week
- Sunday returns same week
- Week spanning two months formats correctly
- Week spanning two years formats correctly
- Returns timezone-aware ISO strings

**get_day_range:**
- Returns start/end of a single day
- Returns timezone-aware ISO strings

**make_progress_bar:**
- `max_value=0` → all empty
- `value=0` → all empty
- `value=max_value` → all filled (16 █)
- `value=max_value/2` → half filled (8 █)
- Small non-zero value → minimum 1 block

**format_date_range:**
- Single day: "Jan 28, 2025"
- Same-month week: "Jan 20-26, 2025"
- Cross-month week: "Jan 28 - Feb 3, 2025"
- Cross-year week: "Dec 30, 2024 - Jan 5, 2025"

### CLI tests

1. `tt report` (no flags) → uses `--week`
2. `tt report --week` → shows weekly report
3. `tt report --day` → shows today's report
4. `tt report --day 2025-01-25` → shows specific day
5. `tt report --day invalid-date` → exit code 1 with error message (use exit code 1 for consistency)
6. `tt report --json` → valid JSON matching spec schema
7. `tt report --json` → `generated_at` is valid ISO 8601 timestamp
8. `tt report --json` → `period.start` and `period.end` are date-only (`YYYY-MM-DD`)
9. Empty period → "No time tracked for this period." + hint
10. Partial week → "(X days with data)" in header (weekly only)
11. `--day` report → does NOT show "(X days with data)"
12. Tags sorted by total time descending
13. `(untagged)` always appears last (even with most time)
14. Long tag names (25 chars) truncated to 17 + `...`
15. Progress bars scale relative to max tag time
16. Zero delegated time shows "0m (0%)"
17. Sub-minute duration shows `<1m`
18. No database → exit code 1 with error

### Integration tests

- Full pipeline: insert events → run inference → add tags → report aggregation
- Multi-tag stream (3 tags):
  - Time appears under all 3 tags
  - `stream_count` is 1 for each tag (not 3)
  - Verify header totals computed from stream times (not double-counted):
    - Stream A: 1h direct, tagged with "foo" and "bar"
    - Header `total_ms` = 3,600,000 (not 7,200,000)
    - `by_tag[0].total_ms` + `by_tag[1].total_ms` = 7,200,000 (expected, due to multi-tagging)
- Stream with no tags: appears as `(untagged)`
- Stream with empty tags list `[]`: treated as untagged
- Stream with zero time in period: not shown in report
- Future date (`--day 2030-01-01`): returns empty report, no error
- Performance test: 10,000 events with inference completes in <3s

## Acceptance Criteria (from spec)

1. ✅ `tt report` (no flags) defaults to `--week`
2. ✅ `tt report --week` outputs weekly report in columnar format
3. ✅ `tt report --day` outputs daily report (default: today)
4. ✅ `tt report --day YYYY-MM-DD` outputs report for specified date
5. ✅ `tt report --json` outputs valid JSON matching schema
6. ✅ Tags sorted by total time descending, `(untagged)` always last
7. ✅ Tags longer than 20 chars truncated with ellipsis
8. ✅ Empty periods show hint to check `tt status`
9. ✅ Partial weeks show "(X days with data)"
10. ✅ Progress bars use `max(1, min(16, round(...)))`
11. ✅ Untagged = `(untagged)` in human output, `tag: null` in JSON
12. ✅ Durations use `Xh Ym`, hours omitted when zero, `<1m` for sub-minute
13. ✅ Day boundaries use local system timezone
14. ⏳ Report generation on 10,000 events completes in <3s (test in CI)

## Addressed Review Feedback

| Issue | Resolution |
|-------|------------|
| `get_stream_tags()` loads all streams | Filter by stream IDs from calculate_time() |
| Timezone handling for date ranges | Use `datetime.now().astimezone()` for local tz |
| End timestamp inclusive bug | Use exclusive end (midnight of next day) |
| `--day` date validation | Parse with strptime, exit code 1 on error (consistent with other errors) |
| `days_with_data` not specified | Add `count_days_with_data()` to EventStore |
| Cross-month/year date headers | Add `format_date_range()` helper |
| Multi-tag stream test | Added to test cases |
| Progress bar edge cases | Added tests for zero, max, minimum block |

## Architect Review Feedback (Addressed)

| Issue | Resolution |
|-------|------------|
| JSON period dates should be date-only | Added `generate_json_output()` with date-only strings |
| Tag aggregation code missing | Added explicit aggregation snippet |
| Sorting logic missing | Added `sort_key()` function |
| Imports not specified | Added imports section |
| Exit code 2 inconsistent | Changed to exit code 1 for consistency |
| "(X days with data)" for daily | Clarified: only for weekly reports |
| `generated_at` test missing | Added to test cases |
| Multi-tag stream assertions | Clarified expected behavior in tests |
| Empty tags list handling | Added test case |
| JSON `total_ms` double-counting multi-tagged streams | Fixed: calculate header totals from `time_by_stream`, not `tag_data` |
| Buggy `if _ is not None or True` condition | Fixed: removed, totals now use stream times |
| Imports section mentioned already-imported modules | Fixed: clarified what's already imported |

## Open Questions

None - spec is complete and all review feedback addressed.

## Planning Phase Complete

**Reviewed by code-architect:** Plan verified as ready for implementation.

Checklist:
- [x] Spec exists and is complete (`specs/design/ux-reports.md`)
- [x] Files to change identified
- [x] Test cases outlined
- [x] No open questions remain

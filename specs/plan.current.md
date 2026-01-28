# Implementation Plan: `tt streams` Command

## Task

Implement `tt streams [--today | --week] [--json]` command to list inferred work streams.

## Spec Reference

From `specs/design/ux-cli.md`:

```
tt streams [--today | --week] [--json]

Options:
- --today: Streams active today (default)
- --week: Streams active this week
- --json: Output as JSON

Output format:
ID       Time     Tags           Description
───────────────────────────────────────────────────────────
a1b2c3d  2h 15m   acme-webapp    /home/user/acme/webapp
e4f5g6h  1h 30m   personal       /home/user/side-project
i7j8k9l  45m      (untagged)     /home/user/experiments

Stream ID format: 7-character hex prefix (like git short SHA)

Empty state:
No streams found for today.
Hint: Run 'tt status' to check collection health.
```

## Files to Modify

### `tt-local/tt_local/cli.py`

Add `streams_command()` following the pattern of `report_command()`:

1. Add `@main.command("streams")` decorator
2. Options:
   - `--db`: Path to SQLite database (existing pattern)
   - `--today` / `--week`: Mutually exclusive using `flag_value` pattern (like `report_command`)
   - `--json`: Flag for JSON output
3. Logic:
   - Get date range using existing `get_day_range()` / `get_week_range()`
   - Run `store.run_stream_inference()` to ensure streams exist
   - Call `store.calculate_time(start, end)` to get time per stream
   - **Filter out zero-time streams** (direct_ms == 0 AND delegated_ms == 0)
   - Call `store.get_stream_tags(stream_ids)` for tags
   - Query stream names inline (no new DB method needed - small result set)
   - Format and output

### `tt-local/tt_local/db.py`

**No changes needed.** Stream names can be queried inline in the CLI command since the number of streams in a day/week is typically <50.

```python
# Inline query for stream names
if stream_ids:
    placeholders = ",".join("?" * len(stream_ids))
    cursor = store._conn.execute(
        f"SELECT id, name FROM streams WHERE id IN ({placeholders})",
        stream_ids,
    )
    stream_names = {row["id"]: row["name"] for row in cursor}
```

## Output Formatting

### Human-readable (default)

```
Streams: Jan 28, 2025

ID       Time     Tags           Description
───────────────────────────────────────────────────────────
a1b2c3d  2h 15m   acme-webapp    webapp
e4f5g6h  1h 30m   personal       side-project
i7j8k9l  45m      (untagged)     experiments
```

- ID: First 7 characters of stream UUID
- Time: Sum of direct + delegated time, formatted via `format_duration()`
- Tags: Comma-separated, show first tag(s) that fit within ~14 chars, append `...` if more exist (e.g., `frontend, ...`), or "(untagged)" if none
- Description: Stream name (cwd basename, as stored by `run_stream_inference()`)

### JSON output

```json
{
  "period": {"start": "2025-01-28", "end": "2025-01-28"},
  "streams": [
    {
      "id": "a1b2c3d4-5678-...",
      "short_id": "a1b2c3d",
      "name": "webapp",
      "total_ms": 8100000,
      "direct_ms": 5400000,
      "delegated_ms": 2700000,
      "tags": ["acme-webapp"]
    }
  ]
}
```

## Test Cases

Add `TestStreamsCommand` class in `tt-local/tests/test_cli.py`:

1. `test_streams_no_database` - Exit code 1, "No database found" message
2. `test_streams_empty_database` - Shows empty state hint
3. `test_streams_default_is_today` - `--today` is default
4. `test_streams_today_with_data` - Shows streams for today
5. `test_streams_week_filter` - `--week` shows week range
6. `test_streams_json_valid` - Valid JSON output
7. `test_streams_json_schema` - Matches expected schema
8. `test_streams_sorted_by_time` - Streams sorted by total time descending
9. `test_streams_shows_tags` - Tags displayed correctly
10. `test_streams_untagged_shows_placeholder` - "(untagged)" for streams without tags
11. `test_streams_short_id_format` - 7-character hex prefix
12. `test_streams_filters_zero_time` - Streams with 0ms total time are excluded
13. `test_streams_long_tags_truncation` - Tag list truncated with `...` when too long
14. `test_streams_today_week_last_wins` - When both `--today` and `--week` are given, last flag wins (Click behavior)

## Implementation Approach

1. Add `streams_command()` following `report_command()` pattern
2. Use existing utilities: `format_duration()`, `format_date_range()`, `get_day_range()`, `get_week_range()`
3. Use `flag_value` pattern for `--today` / `--week` mutual exclusivity
4. Filter zero-time streams from output
5. Truncate tag display column to ~14 chars with ellipsis
6. Handle empty state with helpful hint
7. Sort streams by total time descending

## Design Decisions

### Description column shows basename, not full path

The spec example shows full paths (`/home/user/acme/webapp`) but the JSON schema shows `name: "webapp"` (basename). We use **basename** because:

1. It's what `run_stream_inference()` already stores in `streams.name`
2. The JSON schema explicitly uses `name` (basename)
3. Tags provide project-level disambiguation (e.g., `acme-webapp` vs `personal`)
4. Stream ID prefix provides unique identification if needed
5. Avoids extra query to fetch first event's `cwd`

If users need full paths, they can query events directly.

### Default period differs from `tt report`

- `tt report` defaults to `--week` (weekly reporting is primary use case)
- `tt streams` defaults to `--today` (daily granularity for stream management)

This is intentional per the spec. Users managing streams typically care about today's activity.

## Acceptance Criteria

- [x] Spec exists and is complete (in `ux-cli.md`)
- [x] Files to change identified
- [x] Test cases outlined
- [x] No open questions remain

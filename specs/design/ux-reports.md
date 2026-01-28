# UX: Reports

This spec defines the report output format for Time Tracker (`tt`).

## Design Principles

1. **Glanceable** — See the big picture in 3 seconds: total time, direct/delegated split
2. **Scannable** — Find any tag quickly in a sorted, columnar list
3. **Actionable** — Identify untagged time that needs attention
4. **Honest** — Partial data is clearly indicated (e.g., "3 days with data")

## Default Command

`tt report` with no flags defaults to `--week` (the most common use case).

## Report Types

### Weekly Report (`tt report --week`)

The primary report for filling timesheets. Shows time breakdown by tag for the current week (Monday-Sunday).

**Example output:**

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

**Format specification:**

| Element | Format | Notes |
|---------|--------|-------|
| Header date range | `Mon DD-DD, YYYY` or `Mon DD - Mon DD, YYYY` | Cross-month ranges show both months; cross-year shows both years |
| Durations | `Xh Ym` or `Ym` | Hours omitted when zero; `<1m` for sub-minute |
| Time column widths | 9 characters each | Right-aligned within fixed width |
| Percentages | Whole numbers in parentheses | `(70%)` not `(69.7%)` |
| Tag column | Left-aligned, 20 char max | Truncate with `...` if needed |
| Time columns | Right-aligned | Consistent decimal alignment |
| Progress bar | 16 characters | `█` for filled, `░` for empty |

**Progress bar calculation:**

The bar shows proportion relative to the tag with the most total time (not total tracked time). The largest tag gets a full bar; others are proportional.

```
if max_time == 0:
    bar_length = 0  # All tags have zero time
elif tag.total == 0:
    bar_length = 0
else:
    bar_length = max(1, min(16, round((tag.total / max_time) * 16)))
```

**Rationale for relative-to-max**: Using percentage of total would make all bars tiny when one tag dominates. Relative-to-max ensures visual differentiation between tags.

### Daily Report (`tt report --day [DATE]`)

Same format as weekly, scoped to a single day.

**Arguments:**
- `DATE` — Date in `YYYY-MM-DD` format (default: today)

**Example output:**

```
Time Report: Jan 28, 2025

Total: 6h 30m
  Direct:    4h 15m (65%)
  Delegated: 2h 15m (35%)

By Tag:
                    Total     Direct    Delegated
  acme-webapp       4h 00m    2h 30m     1h 30m   ████████████████
  personal          1h 45m    1h 15m        30m   ███████░░░░░░░░░
  (untagged)           45m       30m        15m   ███░░░░░░░░░░░░░
```

---

## JSON Output (`--json`)

Machine-readable format for scripts and integrations.

**Schema:**

```json
{
  "report_type": "weekly",
  "generated_at": "2025-01-28T15:30:00Z",
  "period": {
    "start": "2025-01-20",
    "end": "2025-01-26",
    "days_with_data": 4
  },
  "total_ms": 116100000,
  "direct_ms": 81000000,
  "delegated_ms": 35100000,
  "by_tag": [
    {
      "tag": "acme-webapp",
      "total_ms": 66600000,
      "direct_ms": 43200000,
      "delegated_ms": 23400000,
      "stream_count": 3
    },
    {
      "tag": null,
      "total_ms": 18000000,
      "direct_ms": 15300000,
      "delegated_ms": 2700000,
      "stream_count": 2
    }
  ]
}
```

**Field definitions:**

| Field | Type | Description |
|-------|------|-------------|
| `report_type` | `"daily"` \| `"weekly"` | Report scope |
| `generated_at` | ISO 8601 timestamp | When report was generated |
| `period.start` | `YYYY-MM-DD` | First day of period |
| `period.end` | `YYYY-MM-DD` | Last day of period |
| `period.days_with_data` | integer | Days that have at least one event |
| `total_ms` | integer | Total tracked time in milliseconds |
| `direct_ms` | integer | Human attention time |
| `delegated_ms` | integer | Agent execution time |
| `by_tag[].tag` | string \| null | Tag name (`null` = untagged) |
| `by_tag[].total_ms` | integer | Total time for this tag |
| `by_tag[].direct_ms` | integer | Direct (human) time for this tag |
| `by_tag[].delegated_ms` | integer | Delegated (agent) time for this tag |
| `by_tag[].stream_count` | integer | Number of streams with this tag |

**Notes:**
- `tag: null` represents untagged streams (displayed as "(untagged)" in human output)
- `by_tag` is sorted descending by `total_ms`, except `tag: null` (untagged) is always last
- Sum of `by_tag[].total_ms` may exceed `total_ms` because streams can have multiple tags
- `stream_count` counts each stream once per tag (a multi-tagged stream increments both tags' counts)

---

## Edge Cases

### Empty Data

When no events exist for the requested period:

```
Time Report: Jan 28, 2025

No time tracked for this period.

Run 'tt status' to check if collectors are running.
```

The hint is minimal — complex troubleshooting belongs in documentation, not CLI output.

**JSON output for empty data:**

```json
{
  "report_type": "daily",
  "generated_at": "2025-01-28T15:30:00Z",
  "period": {
    "start": "2025-01-28",
    "end": "2025-01-28",
    "days_with_data": 0
  },
  "total_ms": 0,
  "direct_ms": 0,
  "delegated_ms": 0,
  "by_tag": []
}
```

### Partial Week

When the week has data for fewer than 7 days:

```
Time Report: Jan 20-22, 2025 (3 days with data)

Total: 18h 45m
  ...
```

The header shows "(X days with data)" when `days_with_data < 7`. This clearly indicates the report is based on partial data, whether because:
- The week hasn't ended yet (today is Wednesday)
- Some days had no activity (weekend, vacation)
- Data is missing (sync issues)

The indicator appears based on actual data coverage, not calendar position.

### Long Tag Names

Tags longer than 20 characters are truncated with ellipsis:

```
  my-very-long-proje...   4h 30m    3h 00m     1h 30m   ████████████░░░░
```

### Many Tags

MVP shows all tags, sorted by total time descending. If the list is long, users scroll.

(Post-MVP: `--top N` option to limit output)

### Untagged Streams

Streams without tags appear as "(untagged)" at the bottom of the list:

```
  acme-webapp       4h 00m    2h 30m     1h 30m   ████████████████
  personal          1h 45m    1h 15m        30m   ███████░░░░░░░░░
  (untagged)           45m       30m        15m   ███░░░░░░░░░░░░░
```

### Multi-Tag Streams

A stream can have multiple tags (e.g., `acme-webapp` and `billable`). The stream's time appears under each tag.

This means the sum of all tag totals may exceed the header total. This is expected behavior and documented here, not warned about in output (to avoid noise).

### Zero Delegated Time

When all time is direct (no agent activity):

```
Total: 6h 30m
  Direct:    6h 30m (100%)
  Delegated:     0m (0%)
```

### Sub-Minute Durations

Durations under 1 minute show as `<1m`:

```
  quick-fix            <1m       <1m        <1m   █░░░░░░░░░░░░░░░
```

This distinguishes "almost no time" from "exactly zero time" and avoids confusing users who see a tag row with "0m" everywhere.

Note: A tag with any non-zero time gets at least 1 filled bar segment (`█`).

### Timezone Handling

All day boundaries are computed in the **local system timezone**:
- "Today" means midnight-to-midnight in local time
- "This week" starts Monday 00:00:00 local time
- Events are stored in UTC but displayed/aggregated in local time

This matches user expectations — "today's work" means what they did since waking up, not UTC midnight.

**Future dates**: `tt report --day 2030-01-01` returns an empty report (no error). Users may legitimately query future dates by mistake; an empty report is self-explanatory.

---

## Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Layout | Columnar (3 columns + bar) | Faster scanning for timesheet use case |
| Progress bar width | 16 characters | Fits standard 80-column terminal |
| Progress bar proportion | Relative to max tag time | Largest tag gets full bar; avoids tiny bars when one tag dominates |
| Duration format | `Xh Ym`, `Ym`, or `<1m` | Hours omitted when zero; `<1m` for sub-minute |
| Percentage format | Whole numbers | `(70%)` not `(69.7%)` — cleaner appearance |
| Tag sorting | Descending by total time, untagged last | Most significant work at top; untagged always at bottom regardless of time |
| Tag truncation | 20 characters max | Prevent layout breaking |
| Untagged position | Bottom of list | Encourages users to tag their work |
| Partial week indicator | Header annotation | Honest about data coverage |
| Multi-tag overlap | Document, don't warn | Warnings in output add noise |

---

## Deferred to Post-MVP

These features are explicitly out of scope for MVP:

| Feature | Workaround |
|---------|------------|
| Day-by-day breakdown within week | Run `tt report --day YYYY-MM-DD` for each day |
| `--top N` to limit tags shown | Scroll through full list |
| CSV/Toggl export format | Use `--json` and transform externally |
| Custom date ranges | Use `--day` or `--week` only |
| Comparison views | Run reports manually and compare |
| Charts beyond ASCII bars | Use external visualization tools |

---

## Acceptance Criteria

1. `tt report` (no flags) defaults to `--week`
2. `tt report --week` outputs the weekly report in columnar format
3. `tt report --day` outputs the daily report (default: today) in columnar format
4. `tt report --day YYYY-MM-DD` outputs report for the specified date
5. `tt report --json` outputs valid JSON matching the schema above
6. Tags are sorted by total time descending, with "(untagged)" always last
7. Tags longer than 20 characters are truncated with ellipsis
8. Empty periods show a hint to check `tt status`
9. Partial weeks show "(X days with data)" when fewer than 7 days have events
10. Progress bars use `max(1, min(16, round(...)))` — minimum 1 for non-zero, maximum 16
11. Untagged streams appear as "(untagged)" in human output, `tag: null` in JSON
12. Durations use `Xh Ym` format, with hours omitted when zero, and `<1m` for sub-minute
13. Day boundaries use local system timezone
14. Report generation on 10,000 events completes in <3s

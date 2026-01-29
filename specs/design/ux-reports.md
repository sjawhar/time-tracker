# UX: Reports

Report output format for the Time Tracker CLI. This document is the authoritative reference for report structure; `ux-cli.md` references this for report command behavior.

## Report Types

### MVP

| Command | Period | Description |
|---------|--------|-------------|
| `tt report --week` | Current week (Mon-Sun) | Default report |
| `tt report --last-week` | Previous week | Most common for timesheets |
| `tt report --day` | Today | Quick status check |
| `tt report --last-day` | Yesterday | End-of-day review |

### Post-MVP

- Monthly/quarterly aggregation
- Custom date ranges (`--from`, `--to`)
- Daily breakdown within weekly reports (for daily timesheets)

---

## Human-Readable Output

### Structure

Reports follow a consistent structure:

```
TIME REPORT: <period description>

BY TAG
──────
<tag entries...>

(untagged)
<untagged entries...>

SUMMARY
───────
<totals...>
```

### Duration Formatting

| Duration | Format | Example |
|----------|--------|---------|
| ≥ 1 hour | `Xh Ym` | `2h 30m` |
| < 1 hour | `Xm` | `45m` |
| 0 | `0m` | `0m` |

Seconds are dropped (floor to minutes). Sub-minute durations display as `0m`.

**Very long durations**: Use hours regardless of length (e.g., `168h 0m` for a full week). No conversion to days.

### Progress Bars

Progress bars visualize relative time spent. Fixed width: 10 characters.

```
████████░░  (80%)
█████░░░░░  (50%)
██░░░░░░░░  (20%)
```

The maximum includes **all entries** (tagged and untagged). The longest total duration gets a full bar; others are proportional.

Tags with <5% of the maximum get a single `█` to remain visible. The 5% threshold is strict (`< 5%`, not `<= 5%`).

**Edge case**: If all entries have 0 time (shouldn't happen, but defensively), skip progress bars entirely.

### Tag Entries

Each tagged group shows:

```
<tag>                                     <total>  <bar>
  Direct:    <direct time>
  Delegated: <delegated time>
```

Example:

```
acme-webapp                               6h 45m  ████████░░
  Direct:    2h 45m
  Delegated: 4h 00m

internal                                  2h 30m  ███░░░░░░░
  Direct:    1h 30m
  Delegated: 1h 00m
```

**Blank lines** separate tag entries for scannability when there are 3+ tags.

### Untagged Section

Untagged streams appear in a special section with stream IDs and a hint:

```
(untagged)                                1h 15m  █░░░░░░░░░
  Direct:    45m
  Delegated: 30m
  Sessions:
    abc123  tmux/dev/session-2        (45m)
    def789  tmux/staging/session-1    (30m)

  Tip: Run 'tt tag abc123 <project>' to assign
```

**Truncation**: If there are more than 5 untagged sessions, truncate with a count:

```
  Sessions:
    abc123  tmux/dev/session-2        (45m)
    def789  tmux/staging/session-1    (30m)
    ghi012  tmux/dev/session-3        (20m)
    ... and 12 more

  Tip: Run 'tt streams --untagged' to see all
```

### Summary Section

```
SUMMARY
───────
Total tracked:  10h 30m
Direct time:    5h 00m (48%)
Delegated time: 5h 30m (52%)
```

**Percentages**: Only shown when total time ≥ 30 minutes. Below that threshold, percentages suggest false precision:

```
SUMMARY
───────
Total tracked:  15m
Direct time:    10m
Delegated time: 5m
```

### Multi-Tag Streams

When a stream has multiple tags (e.g., `[acme-webapp, urgent]`), time appears under **both** tags in BY TAG. The SUMMARY totals are de-duplicated—each stream's time is counted once regardless of tag count.

Example: A stream with 1h direct time tagged `[acme-webapp, urgent]`:
- BY TAG: `acme-webapp` shows 1h, `urgent` shows 1h
- SUMMARY: Total direct is 1h (not 2h)

**Note for users**: If you add up the BY TAG durations, the sum may exceed the SUMMARY total. This is because multi-tagged streams appear under each tag. The SUMMARY always shows accurate totals.

---

## Edge Cases

### No Events

When no events exist for the period:

```
TIME REPORT: Week of Jan 27, 2025

No events recorded this week.

Hint: Run 'tt status' to check tracking health.
```

If the user has configured remotes but none have been synced recently:

```
Hint: Run 'tt sync <remote>' to pull events from your dev server.
```

### All Untagged

When events exist but no tags are assigned:

```
TIME REPORT: Week of Jan 27, 2025

BY TAG
──────
(no tagged streams)

(untagged)                                3h 15m  ██████████
  Direct:    2h 00m
  Delegated: 1h 15m
  Sessions:
    abc123  tmux/dev/session-1        (2h 00m)
    def456  tmux/dev/session-2        (1h 15m)

  Tip: Run 'tt tag abc123 <project>' to assign

SUMMARY
───────
Total tracked:  3h 15m
Direct time:    2h 00m (62%)
Delegated time: 1h 15m (38%)
```

### Single Stream

Report displays normally even with a single stream. No special handling needed.

### Zero-Time Entries

Streams with 0 direct and 0 delegated time are excluded from reports. This can happen if a stream has only agent events that don't count toward time.

---

## Time Period Handling

### Week Boundaries

- Week starts Monday 00:00:00 local time
- Week ends Sunday 23:59:59 local time
- Events are stored in UTC but interpreted in local time for boundaries

**DST transitions**: During daylight saving transitions, the week may have 167 or 169 hours. Boundaries are based on wall clock time (what the user's clock shows), not duration.

### Stream Attribution (MVP)

For MVP, streams are attributed to a period based on `last_event_at`:

- If `last_event_at` falls within the period, the entire stream's time is included
- This is a simplification; event-level time slicing is deferred to post-MVP

**Limitation**: A stream spanning Sunday-Monday will be attributed entirely to whichever period contains its last event. This means:
- Users reviewing last week's report on Monday morning may see incomplete data
- Weekly timesheets filled out on Monday should use `--last-week` after syncing

For streams that span the period boundary, the report shows a note:

```
Note: 1 stream spans this period boundary. Run 'tt streams --date <date>' for details.
```

---

## Terminal Width

**Minimum width**: 80 characters

**Graceful degradation** (< 80 chars):
1. Truncate tag names with `…` (minimum 10 chars shown)
2. Use narrower progress bars (5 chars) before dropping entirely
3. Align durations right

**Non-ASCII**: Tag names with emoji or CJK characters may cause alignment issues. Implementation should use Unicode-aware width calculation (wcwidth).

Reports are always readable, though less visually polished in narrow terminals.

---

## JSON Output

Machine-readable output via `--json` flag. Uses a report-centric schema mirroring the human-readable structure.

### Schema

```json
{
  "generated_at": "2025-01-29T16:00:00Z",
  "timezone": "America/Los_Angeles",
  "period": {
    "start": "2025-01-27",
    "end": "2025-02-02",
    "type": "week"
  },
  "by_tag": [
    {
      "tag": "acme-webapp",
      "time_direct_ms": 9900000,
      "time_delegated_ms": 14400000,
      "streams": ["abc123", "ghi789"]
    },
    {
      "tag": "internal",
      "time_direct_ms": 5400000,
      "time_delegated_ms": 3600000,
      "streams": ["def456"]
    }
  ],
  "untagged": {
    "time_direct_ms": 2700000,
    "time_delegated_ms": 1800000,
    "streams": ["jkl012", "mno345"]
  },
  "totals": {
    "time_direct_ms": 18000000,
    "time_delegated_ms": 19800000,
    "stream_count": 4
  }
}
```

### Field Definitions

| Field | Type | Description |
|-------|------|-------------|
| `generated_at` | ISO 8601 | Timestamp when report was generated |
| `timezone` | string | IANA timezone used for period boundaries |
| `period.start` | ISO date | First day of period (inclusive) |
| `period.end` | ISO date | Last day of period (inclusive) |
| `period.type` | string | `"week"` or `"day"` |
| `by_tag[].tag` | string | Tag name |
| `by_tag[].time_direct_ms` | integer | Direct time in milliseconds |
| `by_tag[].time_delegated_ms` | integer | Delegated time in milliseconds |
| `by_tag[].streams` | string[] | Stream IDs with this tag |
| `untagged.time_direct_ms` | integer | Direct time for untagged streams |
| `untagged.time_delegated_ms` | integer | Delegated time for untagged streams |
| `untagged.streams` | string[] | Stream IDs without tags |
| `totals.time_direct_ms` | integer | De-duplicated total direct time |
| `totals.time_delegated_ms` | integer | De-duplicated total delegated time |
| `totals.stream_count` | integer | Number of unique streams in period |

**Important for consumers**: Do not sum `by_tag[].time_direct_ms` to calculate totals. Multi-tagged streams appear under each tag, so the sum will exceed `totals.time_direct_ms`. Always use the `totals` field for accurate aggregates.

### Empty Report JSON

```json
{
  "generated_at": "2025-01-29T16:00:00Z",
  "timezone": "America/Los_Angeles",
  "period": {
    "start": "2025-01-27",
    "end": "2025-02-02",
    "type": "week"
  },
  "by_tag": [],
  "untagged": {
    "time_direct_ms": 0,
    "time_delegated_ms": 0,
    "streams": []
  },
  "totals": {
    "time_direct_ms": 0,
    "time_delegated_ms": 0,
    "stream_count": 0
  }
}
```

### Null Handling

- `by_tag` is always an array (empty `[]` if no tagged streams)
- `untagged` is always an object (never null)
- `untagged.streams` is always an array (empty `[]` if all tagged)
- Stream IDs are never null or empty strings

---

## Examples

### Typical Weekly Report

```
TIME REPORT: Week of Jan 27, 2025

BY TAG
──────
acme-webapp                               6h 45m  ████████░░
  Direct:    2h 45m
  Delegated: 4h 00m

internal                                  2h 30m  ███░░░░░░░
  Direct:    1h 30m
  Delegated: 1h 00m

(untagged)                                1h 15m  █░░░░░░░░░
  Direct:    45m
  Delegated: 30m
  Sessions:
    def789  tmux/staging/session-1    (1h 15m)

  Tip: Run 'tt tag def789 <project>' to assign

SUMMARY
───────
Total tracked:  10h 30m
Direct time:    5h 00m (48%)
Delegated time: 5h 30m (52%)
```

### Daily Report

```
TIME REPORT: Wednesday, Jan 29, 2025

BY TAG
──────
acme-webapp                               2h 15m  ██████████
  Direct:    1h 00m
  Delegated: 1h 15m

SUMMARY
───────
Total tracked:  2h 15m
Direct time:    1h 00m (44%)
Delegated time: 1h 15m (56%)
```

---

## Deferred (Post-MVP)

### Export Formats

**Toggl CSV** for import:
```csv
Email,Start date,Start time,End date,End time,Duration,Project,Client,Description,Tags
user@example.com,2024-01-15,09:00:00,2024-01-15,11:30:00,02:30:00,acme-webapp,Acme Corp,Fix auth bug,bug;urgent
```

**Other formats**: PDF, HTML, Markdown

### Features

- `tt untag <stream> <tag>` — Remove tag from stream (noted as potentially needed earlier)
- Custom date ranges (`tt report --from 2025-01-01 --to 2025-01-15`)
- Monthly/quarterly aggregation
- Daily breakdown within weekly report (hours per day for timesheet entry)
- Stream-level detail in report output (which streams contributed to each tag)
- Event-level time slicing for accurate cross-boundary attribution
- `last_event_at` field in JSON stream objects for debugging attribution

---

## Relation to CLI Spec

This document (`ux-reports.md`) defines report structure and formatting. The CLI spec (`ux-cli.md`) defines:
- Command syntax and flags
- Shortcut commands (`tt week`, `tt today`, `tt yesterday`)
- Exit codes and error handling

The CLI spec references this document for report content.

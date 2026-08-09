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
| `tt report --start <d> --end <d>` | Custom range (local dates, `--end` exclusive) | Arbitrary spans |
| `tt report --weeks <n>` | The `n` most recent weeks, newest first | Trend review |

### Post-MVP

- Monthly/quarterly aggregation
- Daily breakdown within weekly reports (for daily timesheets)

---

## Human-Readable Output

### Structure

Every human-readable section answers one question: **where did the user's own attention go?** Direct time is therefore the primary axis — sections are ordered by it, bars are scaled to it, and it is the figure on each row. Delegated agent time trails each row as a subordinate `+` figure so leverage stays visible without competing for the eye.

Reports follow a consistent structure:

```
TIME REPORT: <period description>

BY STREAM
─────────
<column labels>
<stream rows, most direct time first...>
<zero-direct tail, if any>
<(unassigned) row, if any>

  Tip: <one actionable next step>

BY TAG
──────
<column labels>
<tag rows, most direct time first...>
<zero-direct tail, if any>
<(untagged) row, if any>

AGENT SESSIONS
──────────────
<session roll-up...>

SUMMARY
───────
<totals...>
```

BY STREAM comes first because streams are the finest-grained answer to the attention question. BY TAG rolls the same direct time up along the tag dimensions.

### Duration Formatting

| Duration | Format | Example |
|----------|--------|---------|
| ≥ 1 hour | `Xh Ym` | `2h 30m` |
| < 1 hour | `Xm` | `45m` |
| 0 | `0m` | `0m` |

Seconds are dropped (floor to minutes). Sub-minute durations display as `0m` — **except inside a BY STREAM or BY TAG row**, where they display as `<1m`. Flooring them there would print `0m` beside a filled bar cell and read as a rendering bug; `0m` in a row means genuinely no time, and pairs with an empty bar.

**Very long durations**: Use hours regardless of length (e.g., `168h 0m` for a full week). No conversion to days.

### Row Layout

Every row in BY STREAM and BY TAG uses the same columns, so figures line up vertically across sections:

```
<label:46><direct:>8>  <bar:10>  <delegated:>10>
```

That is 78 columns, fitting an 80-column terminal. Each section prints the column labels once, right-aligned over the figures they name:

```
                                                Direct               Delegated
43a092  workorder-5: cross-model eval           10h 54m  ██████████   +126h 4m
```

Stream rows spend their first six columns on the short stream id, which is what other commands take as an argument (`tt tag 43a092 <project>`). Labels longer than their column are truncated with `…`.

Delegated time carries a `+` prefix, reading as "on top of" the direct figure. It is **omitted entirely when zero**: an empty column is the strongest available signal that it is secondary to what the row is about.

### Progress Bars

Progress bars visualize relative **direct** time. Fixed width: 10 characters.

```
████████░░  (80% of the section's largest direct time)
█████░░░░░  (50%)
██░░░░░░░░  (20%)
```

The maximum is computed **per section**, over that section's rows plus its leftover bucket (`(unassigned)` for BY STREAM, `(untagged)` for BY TAG). Bars are therefore relative within a section, and the largest direct time in each section gets a full bar.

Scaling to `direct + delegated` is a bug, not a nicety: a stream with 6 minutes of attention and 200 hours of agent time would own the chart and answer the wrong question.

Rows with <5% of the maximum get a single `█` to remain visible. The 5% threshold is strict (`< 5%`, not `<= 5%`). Rows with exactly zero direct time get an empty bar — no minimum block.

### Rows Without Direct Time

Rows whose direct time is zero collapse into a single tail line at the end of their section's row list:

```
  (+ 8 streams with no direct time, 143h 2m delegated)
```

They are summarised rather than deleted, because their delegated time is real work the SUMMARY still counts — dropping it would hide the leverage signal. They are summarised rather than listed, because they answer the attention question with silence and would crowd out the rows that answer it. `tt streams list` shows them individually.

### Leftover Buckets

Two pseudo-rows carry activity that fits no real row. Each appears only when it has some time, and each uses the ordinary row layout, so its direct time is the headline figure.

`(unassigned)` closes BY STREAM: activity attributed to no stream at all. It stays a visible row of its own even when its direct time is zero, because unattributed *direct* time is the signal that classification has fallen behind.

```
(unassigned)                                       30m  ██░░░░░░░░   +19h 50m
  Not assigned to any stream. Run 'tt classify' to attribute this time.
```

`(untagged)` closes BY TAG: streams carrying no tag.

```
(untagged)                                      2h 45m  ██████████    +1h 45m
```

### Tip Line

BY STREAM closes with at most one actionable tip:

| Condition | Tip |
|-----------|-----|
| Some rows were folded into the zero-direct tail | `Run 'tt streams list' to see all` |
| Otherwise, some listed stream has no tag | `Run 'tt tag <id> <project>' to assign` |
| Otherwise | (no tip) |

The tag tip names the highest-direct **untagged** stream: suggesting `tt tag` for an already-tagged stream would be a no-op.

### Summary Section

```
SUMMARY
───────
Wall clock:      6h 45m
Direct time:     5h 00m
Delegated time:  5h 30m
Leverage:        1.1x
```

This block is the one place direct and delegated time are meant to be compared side by side, so it reports both plus the ratio. Elsewhere direct time leads.

**Leverage** is the delegation ratio (delegated ÷ direct). It is reported as `n/a` when there is no direct time to divide by — a stretch of pure agent execution has no attention to leverage:

```
SUMMARY
───────
Wall clock:      15m
Direct time:     0m
Delegated time:  15m
Leverage:        n/a
```

### Multi-Tag Streams

When a stream has multiple tags (e.g., `[acme-webapp, urgent]`), its time appears under **both** tags in BY TAG. The SUMMARY totals are de-duplicated — each stream's time is counted once regardless of tag count.

Example: A stream with 1h direct time tagged `[acme-webapp, urgent]`:
- BY TAG: `acme-webapp` shows 1h direct, `urgent` shows 1h direct
- SUMMARY: Total direct is 1h (not 2h)

**Note for users**: If you add up the BY TAG figures, the sum may exceed the SUMMARY total, because multi-tagged streams appear under each tag. The SUMMARY always shows accurate totals. BY STREAM never double-counts — each stream is one row.

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

BY STREAM
─────────
                                                Direct               Delegated
abc123  tmux/dev/session-1                       2h 0m  ██████████     +1h 15m
def456  tmux/dev/session-2                         45m  ████░░░░░░        +30m

  Tip: Run 'tt tag abc123 <project>' to assign

BY TAG
──────
(no tagged streams)
                                                Direct               Delegated
(untagged)                                      2h 45m  ██████████     +1h 45m

AGENT SESSIONS
──────────────
No agent sessions recorded.

SUMMARY
───────
Wall clock:      3h 0m
Direct time:     2h 45m
Delegated time:  1h 45m
Leverage:        0.6x
```

### Single Stream

Report displays normally even with a single stream. No special handling needed.

### Zero-Time Entries

Streams with 0 direct and 0 delegated time are excluded from reports entirely. This can happen if a stream has only agent events that don't count toward time.

Streams with 0 direct but some delegated time are **not** excluded — they collapse into the zero-direct tail line described above, so their delegated time still reconciles with the SUMMARY.

### No Direct Time At All

A period of pure agent execution renders BY STREAM as just its tail line, and SUMMARY reports `Leverage: n/a`. That is the honest answer to "where did my attention go": nowhere.

---

## Time Period Handling

### Week Boundaries

- Week starts Monday 00:00:00 local time
- Week ends Sunday 23:59:59 local time
- Events are stored in UTC but interpreted in local time for boundaries

**DST transitions**: During daylight saving transitions, the week may have 167 or 169 hours. Boundaries are based on wall clock time (what the user's clock shows), not duration.

### Stream Attribution

Time is computed per period from the events inside it, not from a stream's cumulative totals: `generate_report_data` runs the allocation algorithm over `[period_start, period_end)`. A stream that spans a period boundary therefore contributes only the slice of its time that falls inside the period, and each period's figures stand alone.

The `end` bound is exclusive: an event at exactly `period_end` belongs to the next period.

The cumulative `streams.time_direct_ms` / `time_delegated_ms` columns that `tt streams` prints are a different thing entirely — they are refreshed only by `tt recompute` and are never read by `tt report`.

---

## Terminal Width

**Minimum width**: 80 characters. Rows are laid out to 78 columns, so a standard terminal never wraps them.

**Graceful degradation** (< 80 chars):
1. Truncate labels with `…` (minimum 10 chars shown)
2. Use narrower progress bars (5 chars) before dropping entirely
3. Align durations right

**Non-ASCII**: labels with emoji or CJK characters may cause alignment issues. Truncation counts characters, not display cells, so a wide-character label can still overflow its column. Fixing this needs a wcwidth dependency the CLI does not currently carry.

Reports are always readable, though less visually polished in narrow terminals.

---

## JSON Output

Machine-readable output via `--json` flag. This shape is consumed by other tooling, so it is a contract: fields may be added, never renamed or removed. In particular, the JSON is **not** reordered or trimmed to match the human-readable rendering — `streams` is already sorted by `time_direct_ms` descending, and every stream with any time appears, including those with no direct time.

### Schema

```json
{
  "generated_at": "2025-01-29T16:00:00Z",
  "timezone": "America/Los_Angeles",
  "week_start_day": "monday",
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
  "streams": [
    {
      "id": "abc123",
      "name": "acme-webapp: auth rewrite",
      "time_direct_ms": 9900000,
      "time_delegated_ms": 14400000,
      "tags": ["acme-webapp"]
    }
  ],
  "untagged": {
    "time_direct_ms": 2700000,
    "time_delegated_ms": 1800000,
    "streams": ["jkl012", "mno345"]
  },
  "agent_sessions": {
    "total": 2,
    "by_source": { "claude": 1, "opencode": 1 },
    "by_type": { "user": 1, "subagent": 1 },
    "top_sessions": [
      {
        "session_id": "session-1",
        "source": "claude",
        "type": "user",
        "duration_ms": 1800000,
        "starting_prompt": "Fix the auth bug"
      }
    ]
  },
  "totals": {
    "time_direct_ms": 18000000,
    "time_delegated_ms": 19800000,
    "stream_count": 4,
    "unassigned_direct_ms": 0,
    "unassigned_delegated_ms": 3600000
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
| `week_start_day` | string | Day the week boundary falls on (always `"monday"`) |
| `by_tag[].tag` | string | Tag name |
| `by_tag[].time_direct_ms` | integer | Direct time in milliseconds |
| `by_tag[].time_delegated_ms` | integer | Delegated time in milliseconds |
| `by_tag[].streams` | string[] | Stream IDs with this tag |
| `streams[]` | object[] | Every stream with any time, sorted by `time_direct_ms` descending |
| `streams[].id` / `.name` | string / string\|null | Stream id and display name |
| `streams[].time_direct_ms` / `.time_delegated_ms` | integer | Per-stream time in milliseconds |
| `streams[].tags` | string[] | Sorted tags on this stream |
| `untagged.time_direct_ms` | integer | Direct time for untagged streams |
| `untagged.time_delegated_ms` | integer | Delegated time for untagged streams |
| `untagged.streams` | string[] | Stream IDs without tags |
| `agent_sessions.total` | integer | Agent sessions overlapping the period |
| `agent_sessions.by_source` / `.by_type` | object | Session counts keyed by source / type |
| `agent_sessions.top_sessions` | object[] | Five longest sessions, prompts truncated to 100 bytes |
| `totals.time_direct_ms` | integer | De-duplicated total direct time |
| `totals.time_delegated_ms` | integer | De-duplicated total delegated time |
| `totals.stream_count` | integer | Number of unique streams in period |
| `totals.unassigned_direct_ms` | integer | Direct time on activity with no stream (subset of `totals.time_direct_ms`) |
| `totals.unassigned_delegated_ms` | integer | Delegated time on activity with no stream (subset of `totals.time_delegated_ms`) |

**Important for consumers**: Do not sum `by_tag[].time_direct_ms` to calculate totals. Multi-tagged streams appear under each tag, so the sum will exceed `totals.time_direct_ms`. Always use the `totals` field for accurate aggregates.

### Empty Report JSON

```json
{
  "generated_at": "2025-01-29T16:00:00Z",
  "timezone": "America/Los_Angeles",
  "week_start_day": "monday",
  "period": {
    "start": "2025-01-27",
    "end": "2025-02-02",
    "type": "week"
  },
  "by_tag": [],
  "streams": [],
  "untagged": {
    "time_direct_ms": 0,
    "time_delegated_ms": 0,
    "streams": []
  },
  "agent_sessions": {
    "total": 0,
    "by_source": {},
    "by_type": {},
    "top_sessions": []
  },
  "totals": {
    "time_direct_ms": 0,
    "time_delegated_ms": 0,
    "stream_count": 0,
    "unassigned_direct_ms": 0,
    "unassigned_delegated_ms": 0
  }
}
```

### Null Handling

- `by_tag` and `streams` are always arrays (empty `[]` when there is nothing to report)
- `untagged`, `agent_sessions`, and `totals` are always objects (never null)
- `untagged.streams` is always an array (empty `[]` if all tagged)
- Stream IDs are never null or empty strings; `streams[].name` may be null for an unnamed stream

---

## Examples

### Typical Weekly Report

```
TIME REPORT: Week of Jan 27, 2025

BY STREAM
─────────
                                                Direct               Delegated
abc123  acme-webapp: auth rewrite               2h 45m  ██████████     +4h 0m
def456  internal: hiring + planning             1h 30m  █████░░░░░     +1h 0m
ghi789  acme-webapp: perf regression hunt          45m  ███░░░░░░░       +30m
  (+ 3 streams with no direct time, 22h 14m delegated)

  Tip: Run 'tt streams list' to see all

BY TAG
──────
                                                Direct               Delegated
acme-webapp                                     3h 30m  ██████████    +4h 30m
internal                                        1h 30m  ████░░░░░░     +1h 0m

AGENT SESSIONS
──────────────
Total sessions: 41
By source: claude=6, opencode=35
By type: subagent=12, user=29
Top sessions:
  4b91c2  opencode/user   6h 12m  Rewrite the auth middleware to use the new session store

SUMMARY
───────
Wall clock:      6h 45m
Direct time:     5h 0m
Delegated time:  27h 44m
Leverage:        5.5x
```

Read top to bottom, that says: five hours of attention went mostly to `acme-webapp`, and it moved roughly 28 machine-hours of work.

### Daily Report

```
TIME REPORT: Wednesday, Jan 29, 2025

BY STREAM
─────────
                                                Direct               Delegated
abc123  acme-webapp: auth rewrite                1h 0m  ██████████    +1h 15m

BY TAG
──────
                                                Direct               Delegated
acme-webapp                                      1h 0m  ██████████    +1h 15m

AGENT SESSIONS
──────────────
No agent sessions recorded.

SUMMARY
───────
Wall clock:      1h 30m
Direct time:     1h 0m
Delegated time:  1h 15m
Leverage:        1.2x
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
- Monthly/quarterly aggregation
- Daily breakdown within weekly report (hours per day for timesheet entry)
- Per-tag stream grouping in BY TAG (which streams contributed to each tag; BY STREAM lists them flat today)
- `last_event_at` field in JSON stream objects for debugging attribution

---

## Relation to CLI Spec

This document (`ux-reports.md`) defines report structure and formatting. The CLI spec (`ux-cli.md`) defines:
- Command syntax and flags
- Shortcut commands (`tt week`, `tt today`, `tt yesterday`)
- Exit codes and error handling

The CLI spec references this document for report content.

# UX: Reports

## Problem Statement

Users need a single, predictable weekly report they can glance at to understand where their time went, without exporting or navigating a UI. The MVP must define a stable text format for `tt report --week` so the CLI output is consistent and testable.

## Design Goals

- **Weekly overview**: one report for the current week, focused on totals and a project/stream breakdown.
- **Actionable at a glance**: show direct vs delegated time, daily totals, and top streams.
- **Stable output**: deterministic ordering and formatting suitable for snapshot tests.
- **Minimal scope**: no charts, exports, or customization in MVP.

## Research Findings

- Clockify’s Summary report groups time by project, client, user, tag, or date and includes daily totals, indicating that weekly reports should pair totals with grouped breakdowns.
- Clockify’s Weekly report is a fixed weekly view and can be grouped by user or project, reinforcing a project/stream grouping for weekly summaries.
- Toggl’s Weekly report shows a chart for the selected week and supports grouping by project or user, suggesting a week-scoped, single-primary-grouping format.
- RescueTime’s Categories report breaks time down by category and supports drill-downs, showing the value of a categorized breakdown in reports.
- WakaTime’s Summaries API returns daily summaries over a date range, supporting inclusion of per-day totals alongside per-project totals.

## Proposed MVP Report Format

### Command

```
tt report --week
```

### Time Range

- Uses **local timezone**.
- Week starts on **Monday at 00:00** (ISO week).
- Range ends at **report generation time** (current week-to-date).
- The report still lists **all seven days**; future days show `0h 00m`.

### Sections (in order)

1. **Header**
   - `TIME REPORT (WEEK)`
   - `Week: YYYY-MM-DD..YYYY-MM-DD (Mon-Sun)`
   - `Generated: <local timestamp with offset>`

2. **Summary**
   - `Total tracked` = `Direct + Delegated`
   - `Direct time` (human attention)
   - `Delegated time` (agent execution)
   - `Unassigned` (stream_id = null), shown only if non-zero

3. **Daily Totals**
   - One line per day (Mon-Sun)
   - Shows total and split: `D` (direct) / `A` (delegated)

4. **By Stream**
   - Table of streams (including `Uncategorized` for `stream_id = null`)
   - Columns: `Direct`, `Delegated`, `Total`, `Tags`
   - Sorted by `Total` descending, tie-breaker by stream name (A-Z)

### Duration Formatting

- Round to nearest minute.
- Format as `Hh MMm` (always two-digit minutes, e.g., `2h 05m`).
- `0h 00m` is valid and used for empty days.

### Example Output

```
TIME REPORT (WEEK)
Week: 2026-01-19..2026-01-25 (Mon-Sun)
Generated: 2026-01-28T09:42:10-08:00

SUMMARY
Total tracked: 32h 15m
Direct time:   21h 40m (67%)
Delegated:     10h 35m (33%)
Unassigned:     1h 10m ( 3%)

DAILY TOTALS
Mon 01-19  5h 10m  (D 3h 40m / A 1h 30m)
Tue 01-20  6h 00m  (D 4h 10m / A 1h 50m)
Wed 01-21  4h 45m  (D 3h 30m / A 1h 15m)
Thu 01-22  3h 20m  (D 2h 00m / A 1h 20m)
Fri 01-23  7h 10m  (D 4h 55m / A 2h 15m)
Sat 01-24  4h 30m  (D 2h 35m / A 1h 55m)
Sun 01-25  1h 20m  (D 0h 50m / A 0h 30m)

BY STREAM
Stream                              Direct   Delegated   Total   Tags
acme-webapp                         8h 20m   3h 10m      11h 30m  client:acme, backend
infra-docs                          4h 05m   1h 20m       5h 25m  writing
research-notes                      3h 10m   0h 55m       4h 05m  research
Uncategorized                       1h 10m   0h 00m       1h 10m  -
```

## Edge Cases and Failure Modes

- **No events in range**: Output header + summary with `0h 00m`, daily totals all zero, and an empty `BY STREAM` section (header only).
- **Only delegated time**: Direct is `0h 00m`; percentages still show for both.
- **Only direct time**: Delegated is `0h 00m`; percentages still show for both.
- **Uncategorized only**: `BY STREAM` includes only `Uncategorized`.
- **Partial week**: Days after `Generated` show `0h 00m`.

## Acceptance Criteria

- `specs/design/ux-reports.md` defines the MVP report format for `tt report --week`.
- Report time range, ordering, and formatting rules are explicit and deterministic.
- The spec includes research findings with citations and a concrete example output.
- Edge cases are documented with expected output behavior.

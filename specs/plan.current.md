# Plan: Finalize Report Format (ux-reports.md)

## Task
Update `specs/design/ux-reports.md` with MVP report output specification.

## Research Summary

### Competitor Analysis
- **Toggl**: Summary reports by project/client/tag, bar charts by period, PDF/CSV export
- **WakaTime**: Automatic tracking with daily totals by project/file/language, JSON export
- **ActivityWatch**: JSON buckets via API, requires external processing for reports
- **CLI Tools** (klog, timetrace, timewarrior): ASCII tables, human-readable durations (`2h 15m`) or decimal hours, aggregation by day/week/month

### Key Patterns from Industry
1. Primary aggregation by **tag/project/client**
2. Support **human-readable** and **machine-readable** (JSON) output
3. Weekly reports include **day-by-day breakdown** alongside totals
4. Visual elements (progress bars) show proportion at a glance

### Existing Design (from ux-cli.md)
The CLI spec already defines `tt report --week` with an example. However, reviewers identified that the **nested format is hard to scan** for the primary use case (filling timesheets). We'll propose a columnar format instead.

The unique value proposition is **direct vs delegated** time — this is what differentiates the tool.

## Dependencies

This spec depends on:
1. **Attention allocation algorithm** — defines how direct/delegated time is computed (Critical TODO in `architecture/overview.md`)
2. **Stream inference** — reports aggregate time from materialized streams

## What the Spec Needs

The current `ux-reports.md` is empty placeholders. It should define:

1. **Report types**: Daily (`--day`) and weekly (`--week`) — already in CLI spec
2. **Output format specification**: Exact structure, not just example
3. **Edge cases**: Empty data, untagged streams, partial weeks, long tag names, many tags
4. **JSON schema**: For programmatic use and potential export
5. **Post-MVP scope**: What's deferred (charts, export formats, custom date ranges)

## Recommended Approach

### Output Format: Columnar Layout

**Change from existing CLI spec**: Use columnar format for easier scanning.

```
Time Report: Jan 20-26, 2025

Total: 32h 15m
  Direct:    22h 30m (70%)
  Delegated: 9h 45m  (30%)

By Tag:
                    Total     Direct    Delegated
  acme-webapp      18h 30m   12h 00m     6h 30m   ████████████░░░░
  personal          8h 45m    6h 15m     2h 30m   █████░░░░░░░░░░░
  untagged          5h 00m    4h 15m        45m   ███░░░░░░░░░░░░░
```

**Rationale**: Users filling timesheets need to quickly scan the "Direct" column. The nested format requires zigzag eye movement; columnar lets users scan a single column.

### What to Include in MVP Spec

1. **Weekly report** (`tt report --week`) — primary use case for timesheet filling
   - Header with date range and partial-week indicator if applicable
   - Total with direct/delegated breakdown
   - Columnar by-tag breakdown with progress bars
   - Untagged time shown as "(untagged)" at bottom

2. **Daily report** (`tt report --day [DATE]`) — same format, single day scope
   - Default to today
   - Accept `YYYY-MM-DD` format

3. **JSON output** (`--json`) — machine-readable for scripts/integrations
   - All durations in milliseconds
   - ISO 8601 timestamps
   - Metadata fields: `report_type`, `generated_at`

### What to Defer to Post-MVP
- Day-by-day breakdown within weekly view (limitation: users run `--day` 7 times)
- `--top N` option to limit tags shown
- CSV/Toggl export format
- Custom date ranges (arbitrary start/end)
- Charts/graphs beyond ASCII progress bars
- Comparison views (this week vs last week)

### Design Decisions to Document

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Layout | Columnar (3 columns + bar) | Faster scanning for timesheet use case |
| Progress bar width | 16 characters | Fits standard 80-col terminal |
| Progress bar proportion | Relative to max tag time | Largest tag gets full bar, others proportional |
| Duration format | `Xh Ym` (no seconds) | Appropriate precision for reports |
| Percentage format | Whole numbers in parentheses | Keep it simple |
| Tag sorting | Descending by total time | Most significant work at top |
| Tag truncation | 20 characters max with ellipsis | Prevent layout breaking |
| Empty state | Helpful message with hints | Guide user to fix issues |
| Partial week | Show "Jan 20-22, 2025 (3 of 7 days)" | Honest about data coverage |
| Multi-tag overlap | Document but don't warn in output | Tag totals may exceed header total |

### Edge Cases

1. **Empty data**: Show message with hint to sync or check status
2. **Untagged streams**: Show as "(untagged)" row at bottom
3. **Unassigned events**: Events not yet assigned to streams are not shown in reports (user should run stream inference first)
4. **Partial weeks**: Header shows actual date range with "(X of 7 days)"
5. **Long tag names**: Truncate at 20 chars with ellipsis (`my-very-long-pro...`)
6. **Many tags (>10)**: MVP shows all; defer `--top N` to post-MVP
7. **Multi-tag streams**: Same stream's time appears under each tag; document that tag totals may exceed header total

### JSON Schema

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

**Notes:**
- `tag: null` represents untagged streams
- `stream_count` helps users understand data aggregation
- Dates in `period` use `YYYY-MM-DD` (not full ISO 8601) since they're date ranges, not timestamps
- `days_with_data` helps clarify partial week coverage

## Spec Outline

The updated `ux-reports.md` will contain:

```
# UX: Reports

## Design Principles
- Glanceable: see the big picture in 3 seconds
- Scannable: find any tag in the list quickly
- Actionable: identify untagged time that needs attention

## Report Types

### Weekly Report (`tt report --week`)
[Full output format spec with columnar layout]

### Daily Report (`tt report --day`)
[Same format, single day scope]

### JSON Output (`--json`)
[Full schema definition]

## Edge Cases
[All cases listed above with exact output]

## Design Decisions
[Table of decisions with rationale]

## Deferred to Post-MVP
[What's explicitly not included]

## Acceptance Criteria
[Testable requirements]
```

## Open Questions (Resolved)

1. **Should weekly report show day-by-day breakdown?**
   → No, defer to post-MVP. Users can run `--day` multiple times as workaround.

2. **What's the minimum useful report?**
   → Weekly by-tag with direct/delegated split. Enough to fill timesheets.

3. **How are tags sorted in output?**
   → Descending by total time. Most significant work at top.

4. **Columnar vs nested layout?**
   → Columnar. Better scanning for the primary timesheet use case.

5. **What does "proportional" mean for progress bars?**
   → Relative to max tag time (not total time). Largest tag gets full bar.

6. **How to handle multi-tag streams in totals?**
   → Document that tag totals may exceed header total. Don't add warning to output (noise).

## Ready for Implementation

This plan proposes finalizing `ux-reports.md` with:
- Columnar output format for faster scanning
- Concrete edge case handling with exact output examples
- JSON schema with metadata fields for programmatic use
- Clear post-MVP deferrals with documented limitations

**Note**: Updates `ux-cli.md` example format to use columnar layout (breaking change to existing example, but spec is not yet implemented).

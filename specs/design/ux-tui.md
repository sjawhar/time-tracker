# UX: Terminal UI Dashboard

## Problem Statement

Users need a fast, read-only terminal dashboard to scan current activity and
weekly totals without running multiple CLI commands. The CLI remains the source
of truth for reporting, tagging, and export, but a TUI can provide a quick
overview and drill-down in one place while staying within the CLI startup
budget.

## Research Findings

- ActivityWatch organizes around a dashboard plus drill-down views (timeline,
  activity browser, raw data), suggesting a split between overview and detail.
- Toggl and WakaTime emphasize weekly and daily summaries with grouped
  breakdowns (project, language), reinforcing week-first navigation and
  rollups.
- Timewarrior's terminal summary uses tables with daily totals, indicating that
  scan-friendly tables are effective in TUIs.
- Privacy-first, local-first tools keep interaction lightweight and avoid heavy
  charts; a TUI should do the same.

## Proposed Approach

Build a **read-only TUI dashboard** with a small set of views. It mirrors the
CLI report format while adding quick "today" stats and a limited drill-down.

### View Hierarchy

1. **Dashboard (default)**
   - Today totals (direct, delegated, combined)
   - Week-to-date totals (direct, delegated, combined)
   - Top streams list with tags and totals
   - Active agents/panes list if recent events exist
2. **Streams view**
   - Table of streams with totals, tags, last-seen timestamp
   - Drill into a stream to see daily totals for the current week
3. **Report view**
   - Render the same weekly report format as `tt report --week`
4. **Events view (optional)**
   - Recent events list for debugging and verification

### Interaction Model

- Keyboard-driven navigation with a minimal help overlay.
- No editing flows, no manual timers, no tag changes. All edits remain in the
  CLI (`tt tag`, etc.).
- Default scope is current week (Mon-Sun). Optional day selector for the
  stream drill-down view.

### Data Requirements

- Week-to-date time entries with direct/delegated breakdowns.
- Stream list with totals, tags, and last-seen timestamps.
- Recent events (bounded window) for the optional events view.
- Active agent/pane list derived from recent events (same bounded window).

### Performance Constraints

- Startup should remain within the CLI budget by limiting queries to the
  current week and a small recent window (e.g., last 24h) for active lists.
- Avoid loading long history or unbounded event scans.

## Edge Cases and Failure Modes

- **No events yet**: Show empty-state message and zero totals.
- **Only delegated time**: Direct totals show zero; combined totals remain
  accurate.
- **Missing tags**: Display "untagged" or blank tag column consistently.
- **Timezone differences**: Week boundaries must be explicit and consistent
  with CLI (Mon-Sun, local timezone).
- **Large stream count**: Only show top N streams; allow scrolling in Streams
  view.

## Non-Goals

- No historical browsing beyond the current week.
- No charts or graphs.
- No editing, tagging, or corrections within the TUI.
- No configuration of filters, date ranges, or layouts.

## Acceptance Criteria

- TUI starts with a dashboard that shows today and week totals plus top streams.
- Streams view lists streams with totals, tags, and last-seen time; supports
  drill-down to daily totals for current week.
- Report view displays the same weekly report format as `tt report --week`.
- Navigation is keyboard-only with a help overlay and quit control.
- Data queries are bounded (current week + recent window) to meet startup
  performance constraints.

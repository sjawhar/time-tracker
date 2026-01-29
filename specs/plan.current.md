# Implementation Plan: `tt report --week` command

## Task
Implement `tt report --week` command per `specs/design/ux-reports.md` and `specs/design/ux-cli.md`.

## Spec Summary

### MVP Report Commands
- `tt report --week` — Current week (Mon-Sun), default
- `tt report --last-week` — Previous week
- `tt report --day` — Today
- `tt report --last-day` — Yesterday
- `--json` flag for machine-readable output

### Shortcut Commands
- `tt week` → `tt report --week`
- `tt today` → `tt report --day`
- `tt yesterday` → `tt report --last-day`

### Key Behaviors
1. Week boundaries use local timezone (Mon 00:00:00 to next Mon 00:00:00, half-open interval)
2. Streams attributed by `last_event_at` (MVP simplification)
3. Tags not implemented yet—all streams go to "(untagged)" section
4. Zero-time streams excluded from reports
5. Progress bars: 10-char fixed width, proportional to max

### Output Format (Human-Readable)
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
```

When >5 untagged streams, truncate with different tip:
```
  Sessions:
    abc123  tmux/dev/session-2        (45m)
    def789  tmux/staging/session-1    (30m)
    ghi012  tmux/dev/session-3        (20m)
    ... and 12 more

  Tip: Run 'tt streams --untagged' to see all
```

### Date Formatting
- Week: "Week of Jan 27, 2025"
- Day: "Wednesday, Jan 29, 2025"

### JSON Schema
See `ux-reports.md` lines 263-344 for full schema. Key fields:
- `generated_at`: ISO 8601 timestamp
- `timezone`: IANA timezone name (e.g., "America/Los_Angeles")

### Deferred for MVP
- "Streams spanning boundary" note (requires `tt streams --date` which doesn't exist)
- Hint variant for configured remotes (just use generic hint)
- Terminal width handling / truncation (assume 80+ chars)
- Unicode-aware width calculation (tags not implemented yet)

## Files to Create/Modify

### Create: `crates/tt-cli/src/commands/report.rs`
New command module containing:
- `run()` function with signature `fn run(db: &Database, period: Period, json: bool) -> Result<()>`
- `format_report()` for testable output generation (following `status.rs` pattern)
- Period enum: `Week`, `LastWeek`, `Day`, `LastDay`
- Period date calculation (local timezone aware)
- Report data aggregation from streams
- Human-readable formatter
- JSON formatter

### Modify: `crates/tt-cli/src/cli.rs`
Add to `Commands` enum with mutual exclusion via `ArgGroup`:
```rust
/// Generate a time report.
Report {
    /// Current week (default)
    #[arg(long, group = "period")]
    week: bool,

    /// Previous week
    #[arg(long, group = "period")]
    last_week: bool,

    /// Today
    #[arg(long, group = "period")]
    day: bool,

    /// Yesterday
    #[arg(long, group = "period")]
    last_day: bool,

    /// Output as JSON
    #[arg(long)]
    json: bool,
},

/// Shortcut for `report --week`
Week {
    #[arg(long)]
    json: bool,
},

/// Shortcut for `report --day`
Today {
    #[arg(long)]
    json: bool,
},

/// Shortcut for `report --last-day`
Yesterday {
    #[arg(long)]
    json: bool,
},
```

### Modify: `crates/tt-cli/src/commands/mod.rs`
Add `pub mod report;`

### Modify: `crates/tt-cli/src/main.rs`
Wire up new commands to `report::run()`

### Database Layer
**No changes needed.** Use existing `db.get_streams()` and filter in command layer:
```rust
let all_streams = db.get_streams()?;
let streams_in_period: Vec<_> = all_streams
    .into_iter()
    .filter(|s| s.last_event_at.map_or(false, |t| t >= start && t < end))
    .filter(|s| s.time_direct_ms > 0 || s.time_delegated_ms > 0) // Exclude zero-time
    .collect();
```

## Implementation Approach

### 1. Period Date Calculation
Use `chrono` with local timezone. Use half-open intervals `[start, end)` for cleaner boundary handling:
```rust
use chrono::{Local, Datelike, NaiveTime, TimeZone, LocalResult};

fn week_boundaries() -> (DateTime<Utc>, DateTime<Utc>) {
    let today = Local::now().date_naive();
    let days_since_monday = today.weekday().num_days_from_monday();
    let monday = today - chrono::Duration::days(days_since_monday as i64);
    let next_monday = monday + chrono::Duration::days(7);

    let start = local_midnight_to_utc(monday);
    let end = local_midnight_to_utc(next_monday);
    (start, end)
}

/// Convert a local date at midnight to UTC.
/// Handles DST ambiguity by picking the earlier time.
fn local_midnight_to_utc(date: NaiveDate) -> DateTime<Utc> {
    let midnight = date.and_time(NaiveTime::from_hms_opt(0, 0, 0).unwrap());
    match Local.from_local_datetime(&midnight) {
        LocalResult::Single(dt) => dt.with_timezone(&Utc),
        LocalResult::Ambiguous(dt, _) => dt.with_timezone(&Utc), // DST fall-back: use earlier
        LocalResult::None => {
            // DST spring-forward gap at midnight is rare but possible
            // Use 1am local which is guaranteed to exist
            let one_am = date.and_time(NaiveTime::from_hms_opt(1, 0, 0).unwrap());
            Local.from_local_datetime(&one_am).unwrap().with_timezone(&Utc)
        }
    }
}
```

**Note**: Half-open intervals `[start, end)` mean:
- `t >= start && t < end` for filtering
- Week: Monday 00:00:00 local <= t < Next Monday 00:00:00 local
- Day: Today 00:00:00 local <= t < Tomorrow 00:00:00 local

### 2. Report Data Structure
Reuse `tt_db::Stream` directly—no new types needed. Totals computed during rendering:
```rust
struct ReportData {
    generated_at: DateTime<Utc>, // Injected for testability
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
    period_type: PeriodType,
    timezone: String, // IANA timezone for JSON output
    streams: Vec<tt_db::Stream>, // Filtered streams in period
}
```

Add `iana-time-zone` crate for timezone name:
```rust
let timezone = iana_time_zone::get_timezone()
    .unwrap_or_else(|_| "UTC".to_string());
```

**Testability**: The `generated_at` timestamp is passed as a parameter rather than using `Utc::now()` directly. This allows tests to use a fixed timestamp for reproducible output.

### 3. Duration Formatting
```rust
fn format_duration(ms: i64) -> String {
    let total_minutes = ms / 60_000;
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;

    if hours >= 1 {
        format!("{}h {}m", hours, minutes)
    } else {
        format!("{}m", minutes)
    }
}
```

### 4. Progress Bar
```rust
fn progress_bar(value: i64, max: i64) -> String {
    // Zero-time streams are filtered out before rendering, so max should always be > 0
    // Defensive check returns empty bar if somehow reached
    if max == 0 { return "░░░░░░░░░░".to_string(); }

    let ratio = value as f64 / max as f64;
    let filled = if ratio < 0.05 && value > 0 {
        1 // Minimum 1 for visibility (spec: <5% gets single block)
    } else {
        (ratio * 10.0).round() as usize
    };

    let empty = 10 - filled;
    format!("{}{}", "█".repeat(filled), "░".repeat(empty))
}
```

**Note**: The `max == 0` case is defensive. Zero-time streams are excluded during filtering, so this branch shouldn't execute in practice. The empty bar ("░░░░░░░░░░") maintains visual consistency if somehow reached.

### 5. Dependencies
- `chrono` with `clock` feature (already in use)
- `serde_json` for JSON output (already in use)
- `iana-time-zone` for timezone name in JSON output (add directly to `tt-cli/Cargo.toml`, not workspace)

## Test Cases

### Unit Tests (in `report.rs`)
Use fixed dates, not "current" to avoid flakiness:
1. `test_week_boundaries_for_known_date` — Monday-Sunday calculation for Jan 27, 2025
2. `test_week_boundaries_last_week_for_known_date` — Previous week calculation
3. `test_day_boundaries_for_known_date` — Day start/end
4. `test_format_duration_hours_and_minutes` — "2h 30m" format
5. `test_format_duration_minutes_only` — "45m" format
6. `test_format_duration_zero` — "0m"
7. `test_progress_bar_full` — "██████████"
8. `test_progress_bar_partial` — "████░░░░░░"
9. `test_progress_bar_minimum` — "█░░░░░░░░░" for <5%
10. `test_progress_bar_zero` — Empty bar (defensive case)
11. `test_percentage_shown_at_30min` — Percentages shown when total == 30min
12. `test_percentage_hidden_below_30min` — Percentages hidden when total < 30min (e.g., 29min)
13. `test_zero_time_streams_excluded` — Streams with 0 direct and 0 delegated excluded
14. `test_dst_fall_back_boundary` — March date (for US DST spring-forward handling)

### Integration Tests (snapshot tests with `insta`)
1. `test_report_empty_period` — No events message with hint
2. `test_report_all_untagged` — Streams in untagged section
3. `test_report_json_output` — JSON schema compliance
4. `test_report_single_stream` — Single stream formatting
5. `test_report_truncation` — >5 untagged streams truncated with correct tip

### Edge Cases
- No streams in period
- All streams have zero time (should show "No events" message)

## Acceptance Criteria
From spec:
- [x] `--week` shows Mon-Sun current week
- [x] `--last-week` shows previous week
- [x] `--day` shows today
- [x] `--last-day` shows yesterday
- [x] Default is `--week`
- [x] `--json` produces valid JSON per schema
- [x] Week boundaries use local timezone
- [x] Duration formatted as `Xh Ym` or `Xm`
- [x] Progress bars 10 chars, proportional
- [x] Untagged streams listed with IDs and names
- [x] >5 untagged truncated with count
- [x] Summary shows totals with percentages (if ≥30m)
- [x] Empty period shows hint message

## Resolved Questions
- **Database method?** No—filter in command layer (YAGNI)
- **Unicode width?** No—tags not implemented, stream names are ASCII
- **Terminal width?** No—assume 80+ chars (defer to post-MVP)
- **Boundary-spanning note?** No—requires `tt streams --date` which doesn't exist
- **Period boundaries?** Use half-open intervals `[start, end)` for cleaner comparison
- **DST handling?** Handle `LocalResult::Ambiguous` by picking earlier time; handle gaps by using 1am
- **Testable timestamps?** Inject `generated_at` as parameter, don't use `Utc::now()` directly

## Performance Notes
- Streams query is O(n) where n = total streams
- Filter in Rust is O(n) for small n, acceptable for MVP
- If slow later, add index on `last_event_at` in database

//! Report command for generating time reports.
//!
//! This module implements `tt report` with various period options
//! (--week, --last-week, --day, --last-day) and output formats (human-readable, JSON).

use std::fmt::Write;

use anyhow::Result;
use chrono::{DateTime, Datelike, Local, LocalResult, NaiveDate, NaiveTime, TimeZone, Utc};
use serde::Serialize;
use tt_db::{Database, Stream};

/// Report period type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    Week,
    LastWeek,
    Day,
    LastDay,
}

/// Period type for JSON output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PeriodType {
    Week,
    Day,
}

/// Computed report data.
#[derive(Debug)]
pub struct ReportData {
    pub generated_at: DateTime<Utc>,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub period_type: PeriodType,
    pub timezone: String,
    pub streams: Vec<Stream>,
}

// ========== Period Date Calculation ==========

/// Converts a local date at midnight to UTC.
/// Handles DST ambiguity by picking the earlier time.
fn local_midnight_to_utc(local_date: NaiveDate) -> DateTime<Utc> {
    let midnight = local_date.and_time(NaiveTime::from_hms_opt(0, 0, 0).unwrap());
    match Local.from_local_datetime(&midnight) {
        // Single or ambiguous (DST fall-back): use the earlier time
        LocalResult::Single(dt) | LocalResult::Ambiguous(dt, _) => dt.with_timezone(&Utc),
        LocalResult::None => {
            // DST spring-forward gap at midnight is rare but possible
            // Use 1am local which is guaranteed to exist
            let one_am = local_date.and_time(NaiveTime::from_hms_opt(1, 0, 0).unwrap());
            Local
                .from_local_datetime(&one_am)
                .unwrap()
                .with_timezone(&Utc)
        }
    }
}

/// Calculates week boundaries (Mon 00:00 to next Mon 00:00 local time) as half-open interval.
fn week_boundaries(today: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    let days_since_monday = today.weekday().num_days_from_monday();
    let monday = today - chrono::Duration::days(i64::from(days_since_monday));
    let next_monday = monday + chrono::Duration::days(7);

    let start = local_midnight_to_utc(monday);
    let end = local_midnight_to_utc(next_monday);
    (start, end)
}

/// Calculates last week boundaries (previous Mon 00:00 to this Mon 00:00 local time).
fn last_week_boundaries(today: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    let days_since_monday = today.weekday().num_days_from_monday();
    let this_monday = today - chrono::Duration::days(i64::from(days_since_monday));
    let last_monday = this_monday - chrono::Duration::days(7);

    let start = local_midnight_to_utc(last_monday);
    let end = local_midnight_to_utc(this_monday);
    (start, end)
}

/// Calculates day boundaries (today 00:00 to tomorrow 00:00 local time).
fn day_boundaries(today: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    let tomorrow = today + chrono::Duration::days(1);

    let start = local_midnight_to_utc(today);
    let end = local_midnight_to_utc(tomorrow);
    (start, end)
}

/// Calculates yesterday boundaries (yesterday 00:00 to today 00:00 local time).
fn last_day_boundaries(today: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    let yesterday = today - chrono::Duration::days(1);

    let start = local_midnight_to_utc(yesterday);
    let end = local_midnight_to_utc(today);
    (start, end)
}

/// Get boundaries for a given period, using the provided date as reference.
pub fn get_period_boundaries(period: Period, today: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    match period {
        Period::Week => week_boundaries(today),
        Period::LastWeek => last_week_boundaries(today),
        Period::Day => day_boundaries(today),
        Period::LastDay => last_day_boundaries(today),
    }
}

// ========== Duration Formatting ==========

/// Formats milliseconds as duration string.
/// Returns "Xh Ym" if >= 1 hour, "Xm" if < 1 hour.
/// Negative durations are treated as 0m (defensive).
pub fn format_duration(ms: i64) -> String {
    if ms < 0 {
        return "0m".to_string();
    }
    let total_minutes = ms / 60_000;
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;

    if hours >= 1 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

// ========== Progress Bar ==========

/// Generates a 10-character progress bar.
/// Values <5% of max get a single block for visibility.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
pub fn progress_bar(value: i64, max: i64) -> String {
    // Zero-time streams are filtered out before rendering, so max should always be > 0
    // Defensive check returns empty bar if somehow reached
    if max == 0 {
        return "░░░░░░░░░░".to_string();
    }

    let ratio = value as f64 / max as f64;
    let filled = if ratio < 0.05 && value > 0 {
        1 // Minimum 1 for visibility (spec: <5% gets single block)
    } else {
        // Clamp to 10 in case value > max (shouldn't happen, but defensive)
        (ratio * 10.0).round().min(10.0) as usize
    };

    let empty = 10 - filled;
    format!("{}{}", "█".repeat(filled), "░".repeat(empty))
}

// ========== Report Generation ==========

/// Generates report data from the database.
pub fn generate_report_data(
    db: &Database,
    period: Period,
    generated_at: DateTime<Utc>,
) -> Result<ReportData> {
    let today = Local::now().date_naive();
    let (period_start, period_end) = get_period_boundaries(period, today);

    let period_type = match period {
        Period::Week | Period::LastWeek => PeriodType::Week,
        Period::Day | Period::LastDay => PeriodType::Day,
    };

    let timezone = iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_string());

    // Get all streams and filter by period
    let all_streams = db.get_streams()?;
    let streams: Vec<_> = all_streams
        .into_iter()
        .filter(|s| {
            s.last_event_at
                .is_some_and(|t| t >= period_start && t < period_end)
        })
        .filter(|s| s.time_direct_ms > 0 || s.time_delegated_ms > 0) // Exclude zero-time
        .collect();

    Ok(ReportData {
        generated_at,
        period_start,
        period_end,
        period_type,
        timezone,
        streams,
    })
}

/// Formats the period description for the report header.
fn format_period_description(report_data: &ReportData) -> String {
    // Convert period_start from UTC to local for display
    let local_start = report_data.period_start.with_timezone(&Local);
    let start_date = local_start.date_naive();

    match report_data.period_type {
        PeriodType::Week => {
            // "Week of Jan 27, 2025"
            format!("Week of {}", start_date.format("%b %-d, %Y"))
        }
        PeriodType::Day => {
            // "Wednesday, Jan 29, 2025"
            format!("{}", start_date.format("%A, %b %-d, %Y"))
        }
    }
}

/// Formats the human-readable report output.
pub fn format_report(data: &ReportData) -> String {
    let mut output = String::new();

    // Header
    let period_desc = format_period_description(data);
    writeln!(output, "TIME REPORT: {period_desc}").unwrap();

    if data.streams.is_empty() {
        // Empty period
        let period_word = match data.period_type {
            PeriodType::Week => "week",
            PeriodType::Day => "day",
        };
        writeln!(output).unwrap();
        writeln!(output, "No events recorded this {period_word}.").unwrap();
        writeln!(output).unwrap();
        writeln!(output, "Hint: Run 'tt status' to check tracking health.").unwrap();
        return output;
    }

    // Calculate totals
    let total_direct: i64 = data.streams.iter().map(|s| s.time_direct_ms).sum();
    let total_delegated: i64 = data.streams.iter().map(|s| s.time_delegated_ms).sum();
    let total_time = total_direct + total_delegated;

    // For progress bar scaling: max is the total time (since all streams are untagged for MVP)
    // When tags are implemented, this should be the max tag total
    let max_total = total_time;

    // BY TAG section
    writeln!(output).unwrap();
    writeln!(output, "BY TAG").unwrap();
    writeln!(output, "──────").unwrap();

    // For MVP, all streams are untagged
    writeln!(output, "(no tagged streams)").unwrap();

    // Untagged section
    writeln!(output).unwrap();
    let untagged_total = format_duration(total_time);
    let untagged_bar = progress_bar(total_time, max_total);
    writeln!(
        output,
        "(untagged)                                {untagged_total:>7}  {untagged_bar}"
    )
    .unwrap();
    writeln!(output, "  Direct:    {}", format_duration(total_direct)).unwrap();
    writeln!(output, "  Delegated: {}", format_duration(total_delegated)).unwrap();

    // Sessions list
    writeln!(output, "  Sessions:").unwrap();

    // Sort streams by total time descending
    let mut sorted_streams: Vec<_> = data.streams.iter().collect();
    sorted_streams.sort_by_key(|s| std::cmp::Reverse(s.time_direct_ms + s.time_delegated_ms));

    let show_count = 5.min(sorted_streams.len());
    let remaining = sorted_streams.len().saturating_sub(5);

    for stream in sorted_streams.iter().take(show_count) {
        let id_short = &stream.id[..6.min(stream.id.len())];
        let name = stream.name.as_deref().unwrap_or("(unnamed)");
        let stream_total = stream.time_direct_ms + stream.time_delegated_ms;
        let duration = format_duration(stream_total);
        writeln!(output, "    {id_short}  {name:<26}({duration})").unwrap();
    }

    if remaining > 0 {
        writeln!(output, "    ... and {remaining} more").unwrap();
    }

    // Tip
    writeln!(output).unwrap();
    if remaining > 0 {
        writeln!(output, "  Tip: Run 'tt streams --untagged' to see all").unwrap();
    } else if let Some(first_stream) = sorted_streams.first() {
        let id_short = &first_stream.id[..6.min(first_stream.id.len())];
        writeln!(output, "  Tip: Run 'tt tag {id_short} <project>' to assign").unwrap();
    }

    // SUMMARY section
    writeln!(output).unwrap();
    writeln!(output, "SUMMARY").unwrap();
    writeln!(output, "───────").unwrap();
    writeln!(output, "Total tracked:  {}", format_duration(total_time)).unwrap();

    // Show percentages only if total >= 30 minutes (1_800_000 ms)
    let total_minutes = total_time / 60_000;
    if total_minutes >= 30 {
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        let direct_pct = if total_time > 0 {
            (total_direct as f64 / total_time as f64 * 100.0).round() as i64
        } else {
            0
        };
        let delegated_pct = 100 - direct_pct;
        writeln!(
            output,
            "Direct time:    {} ({direct_pct}%)",
            format_duration(total_direct)
        )
        .unwrap();
        writeln!(
            output,
            "Delegated time: {} ({delegated_pct}%)",
            format_duration(total_delegated)
        )
        .unwrap();
    } else {
        writeln!(output, "Direct time:    {}", format_duration(total_direct)).unwrap();
        writeln!(
            output,
            "Delegated time: {}",
            format_duration(total_delegated)
        )
        .unwrap();
    }

    output
}

// ========== JSON Output ==========

/// JSON report structure.
#[derive(Debug, Serialize)]
pub struct JsonReport {
    pub generated_at: String,
    pub timezone: String,
    pub period: JsonPeriod,
    pub by_tag: Vec<JsonTagEntry>,
    pub untagged: JsonUntagged,
    pub totals: JsonTotals,
}

#[derive(Debug, Serialize)]
pub struct JsonPeriod {
    pub start: String,
    pub end: String,
    #[serde(rename = "type")]
    pub period_type: PeriodType,
}

#[derive(Debug, Serialize)]
pub struct JsonTagEntry {
    pub tag: String,
    pub time_direct_ms: i64,
    pub time_delegated_ms: i64,
    pub streams: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct JsonUntagged {
    pub time_direct_ms: i64,
    pub time_delegated_ms: i64,
    pub streams: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct JsonTotals {
    pub time_direct_ms: i64,
    pub time_delegated_ms: i64,
    pub stream_count: usize,
}

/// Formats report data as JSON.
pub fn format_report_json(data: &ReportData) -> Result<String> {
    let local_start = data.period_start.with_timezone(&Local);
    let local_end = data.period_end.with_timezone(&Local);

    // For end date in JSON, we need the last day of the period (inclusive)
    // Since period_end is the first moment of the next period, subtract 1 day
    let end_date = (local_end.date_naive() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();

    let total_direct: i64 = data.streams.iter().map(|s| s.time_direct_ms).sum();
    let total_delegated: i64 = data.streams.iter().map(|s| s.time_delegated_ms).sum();

    let report = JsonReport {
        generated_at: data.generated_at.to_rfc3339(),
        timezone: data.timezone.clone(),
        period: JsonPeriod {
            start: local_start.date_naive().format("%Y-%m-%d").to_string(),
            end: end_date,
            period_type: data.period_type,
        },
        by_tag: vec![], // For MVP, no tags
        untagged: JsonUntagged {
            time_direct_ms: total_direct,
            time_delegated_ms: total_delegated,
            streams: data.streams.iter().map(|s| s.id.clone()).collect(),
        },
        totals: JsonTotals {
            time_direct_ms: total_direct,
            time_delegated_ms: total_delegated,
            stream_count: data.streams.len(),
        },
    };

    Ok(serde_json::to_string_pretty(&report)?)
}

// ========== Public Interface ==========

/// Runs the report command.
pub fn run(db: &Database, period: Period, json: bool) -> Result<()> {
    let generated_at = Utc::now();
    let data = generate_report_data(db, period, generated_at)?;

    if json {
        let output = format_report_json(&data)?;
        println!("{output}");
    } else {
        let output = format_report(&data);
        print!("{output}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use insta::assert_snapshot;

    // ========== Period Date Calculation Tests ==========

    #[test]
    fn test_week_boundaries_for_known_date() {
        // Jan 29, 2025 is a Wednesday
        let wednesday = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
        let (start, end) = week_boundaries(wednesday);

        // Week should be Jan 27 (Mon) to Feb 3 (Mon) in local time
        // Convert back to local to verify dates
        let start_local = start.with_timezone(&Local).date_naive();
        let end_local = end.with_timezone(&Local).date_naive();

        assert_eq!(start_local, NaiveDate::from_ymd_opt(2025, 1, 27).unwrap());
        assert_eq!(end_local, NaiveDate::from_ymd_opt(2025, 2, 3).unwrap());
    }

    #[test]
    fn test_week_boundaries_on_monday() {
        // Jan 27, 2025 is a Monday
        let monday = NaiveDate::from_ymd_opt(2025, 1, 27).unwrap();
        let (start, end) = week_boundaries(monday);

        let start_local = start.with_timezone(&Local).date_naive();
        let end_local = end.with_timezone(&Local).date_naive();

        assert_eq!(start_local, NaiveDate::from_ymd_opt(2025, 1, 27).unwrap());
        assert_eq!(end_local, NaiveDate::from_ymd_opt(2025, 2, 3).unwrap());
    }

    #[test]
    fn test_week_boundaries_on_sunday() {
        // Feb 2, 2025 is a Sunday
        let sunday = NaiveDate::from_ymd_opt(2025, 2, 2).unwrap();
        let (start, end) = week_boundaries(sunday);

        let start_local = start.with_timezone(&Local).date_naive();
        let end_local = end.with_timezone(&Local).date_naive();

        assert_eq!(start_local, NaiveDate::from_ymd_opt(2025, 1, 27).unwrap());
        assert_eq!(end_local, NaiveDate::from_ymd_opt(2025, 2, 3).unwrap());
    }

    #[test]
    fn test_last_week_boundaries_for_known_date() {
        // Jan 29, 2025 is a Wednesday
        let wednesday = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
        let (start, end) = last_week_boundaries(wednesday);

        let start_local = start.with_timezone(&Local).date_naive();
        let end_local = end.with_timezone(&Local).date_naive();

        // Last week should be Jan 20 (Mon) to Jan 27 (Mon)
        assert_eq!(start_local, NaiveDate::from_ymd_opt(2025, 1, 20).unwrap());
        assert_eq!(end_local, NaiveDate::from_ymd_opt(2025, 1, 27).unwrap());
    }

    #[test]
    fn test_day_boundaries_for_known_date() {
        let date = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
        let (start, end) = day_boundaries(date);

        let start_local = start.with_timezone(&Local).date_naive();
        let end_local = end.with_timezone(&Local).date_naive();

        assert_eq!(start_local, NaiveDate::from_ymd_opt(2025, 1, 29).unwrap());
        assert_eq!(end_local, NaiveDate::from_ymd_opt(2025, 1, 30).unwrap());
    }

    #[test]
    fn test_last_day_boundaries_for_known_date() {
        let date = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
        let (start, end) = last_day_boundaries(date);

        let start_local = start.with_timezone(&Local).date_naive();
        let end_local = end.with_timezone(&Local).date_naive();

        assert_eq!(start_local, NaiveDate::from_ymd_opt(2025, 1, 28).unwrap());
        assert_eq!(end_local, NaiveDate::from_ymd_opt(2025, 1, 29).unwrap());
    }

    // ========== Duration Formatting Tests ==========

    #[test]
    fn test_format_duration_hours_and_minutes() {
        assert_eq!(format_duration(9_000_000), "2h 30m"); // 2.5 hours
        assert_eq!(format_duration(3_600_000), "1h 0m"); // 1 hour
        assert_eq!(format_duration(5_400_000), "1h 30m"); // 1.5 hours
    }

    #[test]
    fn test_format_duration_minutes_only() {
        assert_eq!(format_duration(2_700_000), "45m"); // 45 minutes
        assert_eq!(format_duration(60_000), "1m"); // 1 minute
        assert_eq!(format_duration(1_800_000), "30m"); // 30 minutes
    }

    #[test]
    fn test_format_duration_zero() {
        assert_eq!(format_duration(0), "0m");
    }

    #[test]
    fn test_format_duration_floors_seconds() {
        // 45.9 minutes should floor to 45m
        assert_eq!(format_duration(2_754_000), "45m");
    }

    #[test]
    fn test_format_duration_negative_is_zero() {
        // Negative durations should be treated as 0 (defensive)
        assert_eq!(format_duration(-1), "0m");
        assert_eq!(format_duration(-3_600_000), "0m");
    }

    // ========== Progress Bar Tests ==========

    #[test]
    fn test_progress_bar_full() {
        assert_eq!(progress_bar(100, 100), "██████████");
    }

    #[test]
    fn test_progress_bar_partial() {
        assert_eq!(progress_bar(50, 100), "█████░░░░░"); // 50%
        assert_eq!(progress_bar(80, 100), "████████░░"); // 80%
        assert_eq!(progress_bar(20, 100), "██░░░░░░░░"); // 20%
    }

    #[test]
    fn test_progress_bar_minimum() {
        // <5% should get single block for visibility
        assert_eq!(progress_bar(4, 100), "█░░░░░░░░░"); // 4%
        assert_eq!(progress_bar(1, 100), "█░░░░░░░░░"); // 1%
    }

    #[test]
    fn test_progress_bar_zero() {
        // max == 0 (defensive case)
        assert_eq!(progress_bar(0, 0), "░░░░░░░░░░");
    }

    #[test]
    fn test_progress_bar_at_5_percent() {
        // Exactly 5% should round to 1 block (0.05 * 10 = 0.5, rounds to 1)
        assert_eq!(progress_bar(5, 100), "█░░░░░░░░░");
    }

    // ========== Integration Tests (Snapshot) ==========

    fn make_test_stream(id: &str, name: &str, direct_ms: i64, delegated_ms: i64) -> Stream {
        let now = Utc::now();
        Stream {
            id: id.to_string(),
            name: Some(name.to_string()),
            created_at: now,
            updated_at: now,
            time_direct_ms: direct_ms,
            time_delegated_ms: delegated_ms,
            first_event_at: Some(now),
            last_event_at: Some(now),
            needs_recompute: false,
        }
    }

    #[test]
    fn test_report_empty_period() {
        let data = ReportData {
            generated_at: Utc.with_ymd_and_hms(2025, 1, 29, 16, 0, 0).unwrap(),
            period_start: Utc.with_ymd_and_hms(2025, 1, 27, 8, 0, 0).unwrap(), // Mon midnight UTC (assuming UTC-8)
            period_end: Utc.with_ymd_and_hms(2025, 2, 3, 8, 0, 0).unwrap(),
            period_type: PeriodType::Week,
            timezone: "America/Los_Angeles".to_string(),
            streams: vec![],
        };

        let output = format_report(&data);
        assert_snapshot!(output);
    }

    #[test]
    fn test_report_all_untagged() {
        let data = ReportData {
            generated_at: Utc.with_ymd_and_hms(2025, 1, 29, 16, 0, 0).unwrap(),
            period_start: Utc.with_ymd_and_hms(2025, 1, 27, 8, 0, 0).unwrap(),
            period_end: Utc.with_ymd_and_hms(2025, 2, 3, 8, 0, 0).unwrap(),
            period_type: PeriodType::Week,
            timezone: "America/Los_Angeles".to_string(),
            streams: vec![
                make_test_stream("abc123def456", "tmux/dev/session-1", 7_200_000, 4_500_000), // 2h direct, 1h15m delegated
                make_test_stream("def456ghi789", "tmux/dev/session-2", 2_700_000, 1_800_000), // 45m direct, 30m delegated
            ],
        };

        let output = format_report(&data);
        assert_snapshot!(output);
    }

    #[test]
    fn test_report_json_output() {
        let data = ReportData {
            generated_at: Utc.with_ymd_and_hms(2025, 1, 29, 16, 0, 0).unwrap(),
            period_start: Utc.with_ymd_and_hms(2025, 1, 27, 8, 0, 0).unwrap(),
            period_end: Utc.with_ymd_and_hms(2025, 2, 3, 8, 0, 0).unwrap(),
            period_type: PeriodType::Week,
            timezone: "America/Los_Angeles".to_string(),
            streams: vec![make_test_stream(
                "abc123def456",
                "tmux/dev/session-1",
                7_200_000,
                4_500_000,
            )],
        };

        let output = format_report_json(&data).unwrap();
        assert_snapshot!(output);
    }

    #[test]
    fn test_report_single_stream() {
        let data = ReportData {
            generated_at: Utc.with_ymd_and_hms(2025, 1, 29, 16, 0, 0).unwrap(),
            period_start: Utc.with_ymd_and_hms(2025, 1, 29, 8, 0, 0).unwrap(),
            period_end: Utc.with_ymd_and_hms(2025, 1, 30, 8, 0, 0).unwrap(),
            period_type: PeriodType::Day,
            timezone: "America/Los_Angeles".to_string(),
            streams: vec![make_test_stream(
                "abc123def456",
                "tmux/dev/session-1",
                3_600_000, // 1h direct
                4_500_000, // 1h15m delegated
            )],
        };

        let output = format_report(&data);
        assert_snapshot!(output);
    }

    #[test]
    fn test_report_truncation() {
        // Create 8 streams to test truncation (>5)
        let streams: Vec<Stream> = (0..8)
            .map(|i| {
                make_test_stream(
                    &format!("stream{i:02}abcdef"),
                    &format!("tmux/dev/session-{i}"),
                    3_600_000 - i64::from(i * 300_000), // Decreasing time
                    1_800_000 - i64::from(i * 100_000),
                )
            })
            .collect();

        let data = ReportData {
            generated_at: Utc.with_ymd_and_hms(2025, 1, 29, 16, 0, 0).unwrap(),
            period_start: Utc.with_ymd_and_hms(2025, 1, 27, 8, 0, 0).unwrap(),
            period_end: Utc.with_ymd_and_hms(2025, 2, 3, 8, 0, 0).unwrap(),
            period_type: PeriodType::Week,
            timezone: "America/Los_Angeles".to_string(),
            streams,
        };

        let output = format_report(&data);
        assert_snapshot!(output);
    }

    #[test]
    fn test_percentage_shown_at_30min() {
        // 30 minutes total - percentages should be shown
        let data = ReportData {
            generated_at: Utc.with_ymd_and_hms(2025, 1, 29, 16, 0, 0).unwrap(),
            period_start: Utc.with_ymd_and_hms(2025, 1, 29, 8, 0, 0).unwrap(),
            period_end: Utc.with_ymd_and_hms(2025, 1, 30, 8, 0, 0).unwrap(),
            period_type: PeriodType::Day,
            timezone: "America/Los_Angeles".to_string(),
            streams: vec![make_test_stream(
                "abc123def456",
                "tmux/dev/session-1",
                1_200_000, // 20m direct
                600_000,   // 10m delegated = 30m total
            )],
        };

        let output = format_report(&data);
        assert!(output.contains('('), "should show percentages at 30m");
    }

    #[test]
    fn test_percentage_hidden_below_30min() {
        // 29 minutes total - percentages should NOT be shown
        let data = ReportData {
            generated_at: Utc.with_ymd_and_hms(2025, 1, 29, 16, 0, 0).unwrap(),
            period_start: Utc.with_ymd_and_hms(2025, 1, 29, 8, 0, 0).unwrap(),
            period_end: Utc.with_ymd_and_hms(2025, 1, 30, 8, 0, 0).unwrap(),
            period_type: PeriodType::Day,
            timezone: "America/Los_Angeles".to_string(),
            streams: vec![make_test_stream(
                "abc123def456",
                "tmux/dev/session-1",
                1_140_000, // 19m direct
                600_000,   // 10m delegated = 29m total
            )],
        };

        let output = format_report(&data);
        // The summary section should not have percentages
        let summary_section = output.split("SUMMARY").nth(1).unwrap_or("");
        assert!(
            !summary_section.contains("%)"),
            "should NOT show percentages below 30m"
        );
    }

    #[test]
    fn test_zero_time_streams_excluded() {
        let db = tt_db::Database::open_in_memory().unwrap();

        // Create a stream with zero time
        let now = Utc::now();
        let zero_stream = Stream {
            id: "zero-stream".to_string(),
            name: Some("empty".to_string()),
            created_at: now,
            updated_at: now,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: Some(now),
            last_event_at: Some(now),
            needs_recompute: false,
        };
        db.insert_stream(&zero_stream).unwrap();

        // Generate report
        let data = generate_report_data(&db, Period::Week, now).unwrap();

        // Zero-time stream should be excluded
        assert!(
            data.streams.is_empty(),
            "zero-time streams should be excluded"
        );
    }
}

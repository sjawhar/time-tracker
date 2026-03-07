//! Report command for generating time reports.
//!
//! This module implements `tt report` with various period options
//! (--week, --last-week, --day, --last-day) and output formats (human-readable, JSON).
//!
//! Time is calculated from events within the period using the allocation algorithm,
//! not from cumulative stream totals. This ensures accurate per-period reporting.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write;

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Local, LocalResult, NaiveDate, NaiveTime, TimeZone, Utc};
use serde::Serialize;
use tt_core::session::AgentSession;
use tt_core::{AllocationConfig, allocate_time};
use tt_db::Database;

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

/// Computed time for a stream within the report period.
#[derive(Debug, Clone)]
pub struct ReportStreamTime {
    pub id: String,
    pub name: Option<String>,
    pub time_direct_ms: i64,
    pub time_delegated_ms: i64,
}

/// Computed report data.
#[derive(Debug)]
pub struct ReportData {
    pub generated_at: DateTime<Utc>,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub period_type: PeriodType,
    pub timezone: String,
    /// Time computed for each stream from events within the period.
    pub streams: Vec<ReportStreamTime>,
    /// Tag mappings for streams included in the report period.
    pub tags_by_stream: HashMap<String, Vec<String>>,
    /// Agent sessions overlapping the report period.
    pub agent_sessions: Vec<AgentSession>,
}

const DEFAULT_WEEK_START_DAY: &str = "monday";

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

// ========== Agent Session Summary ==========

const STARTING_PROMPT_MAX_CHARS: usize = 100;

fn truncate_starting_prompt(prompt: &str) -> String {
    if prompt.len() <= STARTING_PROMPT_MAX_CHARS {
        return prompt.to_string();
    }

    let mut end = STARTING_PROMPT_MAX_CHARS;
    while end > 0 && !prompt.is_char_boundary(end) {
        end -= 1;
    }

    format!("{}...", &prompt[..end])
}

fn session_duration_ms(
    session: &AgentSession,
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
) -> i64 {
    let end_time = session.end_time.unwrap_or(period_end);
    let clamped_start = std::cmp::max(session.start_time, period_start);
    let clamped_end = std::cmp::min(end_time, period_end);
    let duration = clamped_end - clamped_start;
    duration.num_milliseconds().max(0)
}

fn build_agent_session_summary(
    sessions: &[AgentSession],
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
) -> JsonAgentSessionSummary {
    let mut by_source: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_type: BTreeMap<String, usize> = BTreeMap::new();

    let mut top_sessions: Vec<JsonAgentSessionEntry> = sessions
        .iter()
        .map(|session| {
            let duration_ms = session_duration_ms(session, period_start, period_end);
            let starting_prompt = session
                .starting_prompt
                .as_deref()
                .map(truncate_starting_prompt)
                .unwrap_or_default();

            *by_source
                .entry(session.source.as_str().to_string())
                .or_insert(0) += 1;
            *by_type
                .entry(session.session_type.as_str().to_string())
                .or_insert(0) += 1;

            JsonAgentSessionEntry {
                session_id: session.session_id.clone(),
                source: session.source.as_str().to_string(),
                session_type: session.session_type.as_str().to_string(),
                duration_ms,
                starting_prompt,
            }
        })
        .collect();

    top_sessions.sort_by(|a, b| {
        b.duration_ms
            .cmp(&a.duration_ms)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    top_sessions.truncate(5);

    JsonAgentSessionSummary {
        total: sessions.len(),
        by_source,
        by_type,
        top_sessions,
    }
}

// ========== Report Generation ==========

/// Generates report data from the database.
///
/// Time is calculated from events within the period using the allocation algorithm,
/// ensuring accurate per-period reporting (not cumulative totals).
pub fn generate_report_data(
    db: &Database,
    period: Period,
    generated_at: DateTime<Utc>,
) -> Result<ReportData> {
    let today = generated_at.with_timezone(&Local).date_naive();
    let timezone = iana_time_zone::get_timezone().unwrap_or_else(|_| "Etc/UTC".to_string());
    generate_report_data_for_date(db, period, generated_at, today, timezone)
}

/// Generates report data from the database for a specific reference date.
pub fn generate_report_data_for_date(
    db: &Database,
    period: Period,
    generated_at: DateTime<Utc>,
    reference_date: NaiveDate,
    timezone: String,
) -> Result<ReportData> {
    let (period_start, period_end) = get_period_boundaries(period, reference_date);

    let period_type = match period {
        Period::Week | Period::LastWeek => PeriodType::Week,
        Period::Day | Period::LastDay => PeriodType::Day,
    };

    // Get events within the period
    let events = db
        .get_events_in_range(period_start, period_end)
        .context("failed to get events in period")?;

    // Calculate time from events using the allocation algorithm
    let config = AllocationConfig::default();
    let result = allocate_time(&events, &config, Some(period_end), &HashMap::new());

    // Get stream metadata (names) for display
    let all_streams = db.get_streams().context("failed to get streams")?;
    let stream_names: HashMap<String, Option<String>> =
        all_streams.into_iter().map(|s| (s.id, s.name)).collect();

    let tags_by_stream: HashMap<String, Vec<String>> = db
        .get_all_tags()
        .context("failed to get stream tags")?
        .into_iter()
        .collect();

    let agent_sessions = db
        .agent_sessions_in_range(period_start, period_end)
        .context("failed to get agent sessions in period")?;

    // Convert allocation results to report format, excluding zero-time streams
    let streams: Vec<ReportStreamTime> = result
        .stream_times
        .into_iter()
        .filter(|t| t.time_direct_ms > 0 || t.time_delegated_ms > 0)
        .map(|t| ReportStreamTime {
            name: stream_names.get(&t.stream_id).cloned().flatten(),
            id: t.stream_id,
            time_direct_ms: t.time_direct_ms,
            time_delegated_ms: t.time_delegated_ms,
        })
        .collect();

    Ok(ReportData {
        generated_at,
        period_start,
        period_end,
        period_type,
        timezone,
        streams,
        tags_by_stream,
        agent_sessions,
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

fn write_agent_session_summary(output: &mut String, summary: &JsonAgentSessionSummary) {
    writeln!(output).unwrap();
    writeln!(output, "AGENT SESSIONS").unwrap();
    writeln!(output, "──────────────").unwrap();

    if summary.total == 0 {
        writeln!(output, "No agent sessions recorded.").unwrap();
        return;
    }

    writeln!(output, "Total sessions: {}", summary.total).unwrap();

    if !summary.by_source.is_empty() {
        let by_source = summary
            .by_source
            .iter()
            .map(|(source, count)| format!("{source}={count}"))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(output, "By source: {by_source}").unwrap();
    }

    if !summary.by_type.is_empty() {
        let by_type = summary
            .by_type
            .iter()
            .map(|(session_type, count)| format!("{session_type}={count}"))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(output, "By type: {by_type}").unwrap();
    }

    writeln!(output, "Top sessions:").unwrap();
    if summary.top_sessions.is_empty() {
        writeln!(output, "  (none)").unwrap();
        return;
    }

    for session in &summary.top_sessions {
        let id_short = &session.session_id[..6.min(session.session_id.len())];
        let duration = format_duration(session.duration_ms);
        let prompt = if session.starting_prompt.is_empty() {
            "(no prompt)"
        } else {
            session.starting_prompt.as_str()
        };
        writeln!(
            output,
            "  {id_short}  {}/{}  {duration:>6}  {prompt}",
            session.source, session.session_type
        )
        .unwrap();
    }
}

/// Formats the human-readable report output.
#[allow(clippy::too_many_lines)]
pub fn format_report(data: &ReportData) -> String {
    let mut output = String::new();

    // Header
    let period_desc = format_period_description(data);
    writeln!(output, "TIME REPORT: {period_desc}").unwrap();

    let agent_session_summary =
        build_agent_session_summary(&data.agent_sessions, data.period_start, data.period_end);

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
        write_agent_session_summary(&mut output, &agent_session_summary);
        return output;
    }

    // Calculate totals
    let total_direct: i64 = data.streams.iter().map(|s| s.time_direct_ms).sum();
    let total_delegated: i64 = data.streams.iter().map(|s| s.time_delegated_ms).sum();
    let total_time = total_direct + total_delegated;

    let tag_entries = build_tag_entries(&data.streams, &data.tags_by_stream);
    let mut untagged_direct_ms = 0;
    let mut untagged_delegated_ms = 0;
    for stream in &data.streams {
        match data.tags_by_stream.get(&stream.id) {
            Some(tags) if !tags.is_empty() => {}
            _ => {
                untagged_direct_ms += stream.time_direct_ms;
                untagged_delegated_ms += stream.time_delegated_ms;
            }
        }
    }
    let untagged_total_ms = untagged_direct_ms + untagged_delegated_ms;

    let max_total = if tag_entries.is_empty() {
        total_time
    } else {
        let max_tag_total = tag_entries
            .iter()
            .map(|entry| entry.time_direct_ms + entry.time_delegated_ms)
            .max()
            .unwrap_or(0);
        std::cmp::max(max_tag_total, untagged_total_ms)
    };

    // BY TAG section
    writeln!(output).unwrap();
    writeln!(output, "BY TAG").unwrap();
    writeln!(output, "──────").unwrap();

    if tag_entries.is_empty() {
        writeln!(output, "(no tagged streams)").unwrap();
    } else {
        let mut sorted_tags = tag_entries;
        sorted_tags.sort_by(|a, b| {
            let a_total = a.time_direct_ms + a.time_delegated_ms;
            let b_total = b.time_direct_ms + b.time_delegated_ms;
            b_total.cmp(&a_total).then_with(|| a.tag.cmp(&b.tag))
        });

        for entry in sorted_tags {
            let total_ms = entry.time_direct_ms + entry.time_delegated_ms;
            let duration = format_duration(total_ms);
            let bar = progress_bar(total_ms, max_total);
            writeln!(output, "{:<36}{:>7}  {}", entry.tag, duration, bar).unwrap();
        }
    }

    // Untagged section
    writeln!(output).unwrap();
    let untagged_total = format_duration(untagged_total_ms);
    let untagged_bar = progress_bar(untagged_total_ms, max_total);
    writeln!(
        output,
        "(untagged)                                {untagged_total:>7}  {untagged_bar}"
    )
    .unwrap();
    writeln!(
        output,
        "  Direct:    {}",
        format_duration(untagged_direct_ms)
    )
    .unwrap();
    writeln!(
        output,
        "  Delegated: {}",
        format_duration(untagged_delegated_ms)
    )
    .unwrap();

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
        writeln!(output, "  Tip: Run 'tt streams list' to see all").unwrap();
    } else if let Some(first_stream) = sorted_streams.first() {
        let id_short = &first_stream.id[..6.min(first_stream.id.len())];
        writeln!(output, "  Tip: Run 'tt tag {id_short} <project>' to assign").unwrap();
    }

    write_agent_session_summary(&mut output, &agent_session_summary);

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
    pub week_start_day: String,
    pub period: JsonPeriod,
    pub by_tag: Vec<JsonTagEntry>,
    pub untagged: JsonUntagged,
    pub agent_sessions: JsonAgentSessionSummary,
    pub totals: JsonTotals,
}

#[derive(Debug, Serialize)]
pub struct JsonWeeksReport {
    pub weeks: Vec<JsonReport>,
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

#[derive(Debug, Serialize)]
pub struct JsonAgentSessionSummary {
    pub total: usize,
    pub by_source: BTreeMap<String, usize>,
    pub by_type: BTreeMap<String, usize>,
    pub top_sessions: Vec<JsonAgentSessionEntry>,
}

#[derive(Debug, Serialize)]
pub struct JsonAgentSessionEntry {
    pub session_id: String,
    pub source: String,
    #[serde(rename = "type")]
    pub session_type: String,
    pub duration_ms: i64,
    pub starting_prompt: String,
}

#[derive(Debug, Default)]
struct TagAggregate {
    time_direct_ms: i64,
    time_delegated_ms: i64,
    streams: BTreeSet<String>,
}

/// Builds tag-level time aggregation from stream data.
///
/// **Multi-tag attribution**: Streams with multiple tags have their FULL time
/// attributed to EACH tag. This means `sum(by_tag.time_direct_ms)` may exceed
/// `totals.time_direct_ms` when streams have overlapping tags. This is intentional —
/// tags represent orthogonal dimensions (e.g., project + activity), so each dimension
/// should reflect the complete time spent.
fn build_tag_entries(
    streams: &[ReportStreamTime],
    tags_by_stream: &HashMap<String, Vec<String>>,
) -> Vec<JsonTagEntry> {
    let mut by_tag: BTreeMap<String, TagAggregate> = BTreeMap::new();

    for stream in streams {
        if let Some(tags) = tags_by_stream.get(&stream.id) {
            for tag in tags {
                let entry = by_tag.entry(tag.clone()).or_default();
                entry.time_direct_ms += stream.time_direct_ms;
                entry.time_delegated_ms += stream.time_delegated_ms;
                entry.streams.insert(stream.id.clone());
            }
        }
    }

    by_tag
        .into_iter()
        .map(|(tag, aggregate)| JsonTagEntry {
            tag,
            time_direct_ms: aggregate.time_direct_ms,
            time_delegated_ms: aggregate.time_delegated_ms,
            streams: aggregate.streams.into_iter().collect(),
        })
        .collect()
}

/// Formats report data as JSON.
pub fn format_report_json(data: &ReportData) -> Result<String> {
    let report = build_json_report(data);
    Ok(serde_json::to_string_pretty(&report)?)
}

fn build_json_report(data: &ReportData) -> JsonReport {
    let local_start = data.period_start.with_timezone(&Local);
    let local_end = data.period_end.with_timezone(&Local);

    // For end date in JSON, we need the last day of the period (inclusive)
    // Since period_end is the first moment of the next period, subtract 1 day
    let end_date = (local_end.date_naive() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();

    let total_direct: i64 = data.streams.iter().map(|s| s.time_direct_ms).sum();
    let total_delegated: i64 = data.streams.iter().map(|s| s.time_delegated_ms).sum();
    let agent_sessions =
        build_agent_session_summary(&data.agent_sessions, data.period_start, data.period_end);

    let by_tag = build_tag_entries(&data.streams, &data.tags_by_stream);
    let mut untagged_direct_ms = 0;
    let mut untagged_delegated_ms = 0;
    let mut untagged_streams = Vec::new();
    for stream in &data.streams {
        match data.tags_by_stream.get(&stream.id) {
            Some(tags) if !tags.is_empty() => {}
            _ => {
                untagged_direct_ms += stream.time_direct_ms;
                untagged_delegated_ms += stream.time_delegated_ms;
                untagged_streams.push(stream.id.clone());
            }
        }
    }

    JsonReport {
        generated_at: data.generated_at.to_rfc3339(),
        timezone: data.timezone.clone(),
        week_start_day: DEFAULT_WEEK_START_DAY.to_string(),
        period: JsonPeriod {
            start: local_start.date_naive().format("%Y-%m-%d").to_string(),
            end: end_date,
            period_type: data.period_type,
        },
        by_tag,
        untagged: JsonUntagged {
            time_direct_ms: untagged_direct_ms,
            time_delegated_ms: untagged_delegated_ms,
            streams: untagged_streams,
        },
        agent_sessions,
        totals: JsonTotals {
            time_direct_ms: total_direct,
            time_delegated_ms: total_delegated,
            stream_count: data.streams.len(),
        },
    }
}

// ========== Public Interface ==========

/// Runs the report command.
pub fn run(db: &Database, period: Period, json: bool, weeks: Option<u32>) -> Result<()> {
    let generated_at = Utc::now();
    run_with_weeks(db, period, json, weeks, generated_at)
}

fn run_with_weeks(
    db: &Database,
    period: Period,
    json: bool,
    weeks: Option<u32>,
    generated_at: DateTime<Utc>,
) -> Result<()> {
    if let Some(weeks) = weeks {
        let reports = generate_weekly_reports(db, weeks, generated_at)?;
        if json {
            let weeks_report = JsonWeeksReport {
                weeks: reports.iter().map(build_json_report).collect(),
            };
            println!("{}", serde_json::to_string_pretty(&weeks_report)?);
        } else {
            let separator = "\n\n────────────────────────\n\n";
            let output = reports
                .iter()
                .map(format_report)
                .collect::<Vec<_>>()
                .join(separator);
            print!("{output}");
        }
        return Ok(());
    }

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

fn generate_weekly_reports(
    db: &Database,
    weeks: u32,
    generated_at: DateTime<Utc>,
) -> Result<Vec<ReportData>> {
    let today = Local::now().date_naive();
    let timezone = iana_time_zone::get_timezone().unwrap_or_else(|_| "Etc/UTC".to_string());
    let mut reports = Vec::with_capacity(weeks as usize);
    for offset in 0..weeks {
        let reference_date = today - chrono::Duration::days(i64::from(offset) * 7);
        let data = generate_report_data_for_date(
            db,
            Period::Week,
            generated_at,
            reference_date,
            timezone.clone(),
        )?;
        reports.push(data);
    }
    Ok(reports)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use insta::assert_snapshot;
    use serde_json::Value;
    use tt_core::session::{SessionSource, SessionType};

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

    fn make_test_stream(
        id: &str,
        name: &str,
        direct_ms: i64,
        delegated_ms: i64,
    ) -> ReportStreamTime {
        ReportStreamTime {
            id: id.to_string(),
            name: Some(name.to_string()),
            time_direct_ms: direct_ms,
            time_delegated_ms: delegated_ms,
        }
    }

    fn make_test_session(
        session_id: &str,
        source: SessionSource,
        session_type: SessionType,
        start_time: DateTime<Utc>,
        end_time: Option<DateTime<Utc>>,
        starting_prompt: Option<&str>,
    ) -> AgentSession {
        AgentSession {
            session_id: session_id.to_string(),
            source,
            parent_session_id: None,
            session_type,
            project_path: "/home/sami/time-tracker/default".to_string(),
            project_name: "time-tracker".to_string(),
            start_time,
            end_time,
            message_count: 3,
            summary: None,
            user_prompts: Vec::new(),
            starting_prompt: starting_prompt.map(ToString::to_string),
            assistant_message_count: 1,
            tool_call_count: 0,
            user_message_timestamps: Vec::new(),
            tool_call_timestamps: Vec::new(),
        }
    }

    fn build_weeks_json(reference_dates: &[NaiveDate]) -> String {
        let db = tt_db::Database::open_in_memory().unwrap();
        let generated_at = Utc.with_ymd_and_hms(2025, 2, 5, 12, 0, 0).unwrap();
        let reports = reference_dates
            .iter()
            .map(|date| {
                generate_report_data_for_date(
                    &db,
                    Period::Week,
                    generated_at,
                    *date,
                    "Etc/UTC".to_string(),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let weeks_report = JsonWeeksReport {
            weeks: reports.iter().map(build_json_report).collect(),
        };
        serde_json::to_string_pretty(&weeks_report).unwrap()
    }

    #[test]
    fn test_weekly_reports_json_shape() {
        let reference_dates = vec![
            NaiveDate::from_ymd_opt(2025, 2, 5).unwrap(),
            NaiveDate::from_ymd_opt(2025, 1, 29).unwrap(),
        ];
        let output = build_weeks_json(&reference_dates);
        let json: Value = serde_json::from_str(&output).unwrap();
        let weeks = json
            .get("weeks")
            .and_then(|value| value.as_array())
            .unwrap();

        assert_eq!(json.as_object().unwrap().len(), 1);
        assert_eq!(weeks.len(), 2);
        let first_start = weeks[0]["period"]["start"].as_str().unwrap();
        let second_start = weeks[1]["period"]["start"].as_str().unwrap();
        assert!(first_start > second_start);

        assert_snapshot!(output, @r###"
{
  "weeks": [
    {
      "generated_at": "2025-02-05T12:00:00+00:00",
      "timezone": "Etc/UTC",
      "week_start_day": "monday",
      "period": {
        "start": "2025-02-03",
        "end": "2025-02-09",
        "type": "week"
      },
      "by_tag": [],
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
        "stream_count": 0
      }
    },
    {
      "generated_at": "2025-02-05T12:00:00+00:00",
      "timezone": "Etc/UTC",
      "week_start_day": "monday",
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
      "agent_sessions": {
        "total": 0,
        "by_source": {},
        "by_type": {},
        "top_sessions": []
      },
      "totals": {
        "time_direct_ms": 0,
        "time_delegated_ms": 0,
        "stream_count": 0
      }
    }
  ]
}
"###);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn test_weekly_reports_ordering() {
        let reference_dates = vec![
            NaiveDate::from_ymd_opt(2025, 2, 5).unwrap(),
            NaiveDate::from_ymd_opt(2025, 1, 29).unwrap(),
            NaiveDate::from_ymd_opt(2025, 1, 22).unwrap(),
        ];
        let output = build_weeks_json(&reference_dates);
        let json: Value = serde_json::from_str(&output).unwrap();
        let weeks = json
            .get("weeks")
            .and_then(|value| value.as_array())
            .unwrap();

        assert_eq!(weeks.len(), 3);
        let first_start = weeks[0]["period"]["start"].as_str().unwrap();
        let second_start = weeks[1]["period"]["start"].as_str().unwrap();
        let third_start = weeks[2]["period"]["start"].as_str().unwrap();
        assert!(first_start > second_start);
        assert!(second_start > third_start);

        assert_snapshot!(output, @r###"
{
  "weeks": [
    {
      "generated_at": "2025-02-05T12:00:00+00:00",
      "timezone": "Etc/UTC",
      "week_start_day": "monday",
      "period": {
        "start": "2025-02-03",
        "end": "2025-02-09",
        "type": "week"
      },
      "by_tag": [],
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
        "stream_count": 0
      }
    },
    {
      "generated_at": "2025-02-05T12:00:00+00:00",
      "timezone": "Etc/UTC",
      "week_start_day": "monday",
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
      "agent_sessions": {
        "total": 0,
        "by_source": {},
        "by_type": {},
        "top_sessions": []
      },
      "totals": {
        "time_direct_ms": 0,
        "time_delegated_ms": 0,
        "stream_count": 0
      }
    },
    {
      "generated_at": "2025-02-05T12:00:00+00:00",
      "timezone": "Etc/UTC",
      "week_start_day": "monday",
      "period": {
        "start": "2025-01-20",
        "end": "2025-01-26",
        "type": "week"
      },
      "by_tag": [],
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
        "stream_count": 0
      }
    }
  ]
}
"###);
    }

    #[test]
    fn test_report_empty_period() {
        let data = ReportData {
            generated_at: Utc.with_ymd_and_hms(2025, 1, 29, 16, 0, 0).unwrap(),
            period_start: Utc.with_ymd_and_hms(2025, 1, 27, 8, 0, 0).unwrap(), // Mon midnight UTC (assuming UTC-8)
            period_end: Utc.with_ymd_and_hms(2025, 2, 3, 8, 0, 0).unwrap(),
            period_type: PeriodType::Week,
            timezone: "Etc/UTC".to_string(),
            streams: vec![],
            tags_by_stream: HashMap::new(),
            agent_sessions: vec![],
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
            timezone: "Etc/UTC".to_string(),
            streams: vec![
                make_test_stream("abc123def456", "tmux/dev/session-1", 7_200_000, 4_500_000), // 2h direct, 1h15m delegated
                make_test_stream("def456ghi789", "tmux/dev/session-2", 2_700_000, 1_800_000), // 45m direct, 30m delegated
            ],
            tags_by_stream: HashMap::new(),
            agent_sessions: vec![],
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
            timezone: "Etc/UTC".to_string(),
            streams: vec![make_test_stream(
                "abc123def456",
                "tmux/dev/session-1",
                7_200_000,
                4_500_000,
            )],
            tags_by_stream: HashMap::new(),
            agent_sessions: vec![],
        };

        let output = format_report_json(&data).unwrap();
        assert_snapshot!(output);
    }

    #[test]
    fn test_report_json_by_tag_aggregates() {
        let mut tags_by_stream = HashMap::new();
        tags_by_stream.insert("abc123def456".to_string(), vec!["dev".to_string()]);
        tags_by_stream.insert("def456ghi789".to_string(), vec!["ops".to_string()]);

        let data = ReportData {
            generated_at: Utc.with_ymd_and_hms(2025, 1, 29, 16, 0, 0).unwrap(),
            period_start: Utc.with_ymd_and_hms(2025, 1, 27, 8, 0, 0).unwrap(),
            period_end: Utc.with_ymd_and_hms(2025, 2, 3, 8, 0, 0).unwrap(),
            period_type: PeriodType::Week,
            timezone: "Etc/UTC".to_string(),
            streams: vec![
                make_test_stream("abc123def456", "tmux/dev/session-1", 3_600_000, 0),
                make_test_stream("def456ghi789", "tmux/dev/session-2", 1_800_000, 600_000),
            ],
            tags_by_stream,
            agent_sessions: vec![],
        };

        let output = format_report_json(&data).unwrap();
        assert_snapshot!(output);
    }

    #[test]
    fn test_report_json_multitag_stream_duplicate() {
        let mut tags_by_stream = HashMap::new();
        tags_by_stream.insert(
            "abc123def456".to_string(),
            vec!["development".to_string(), "time-tracker".to_string()],
        );

        let data = ReportData {
            generated_at: Utc.with_ymd_and_hms(2025, 1, 29, 16, 0, 0).unwrap(),
            period_start: Utc.with_ymd_and_hms(2025, 1, 27, 8, 0, 0).unwrap(),
            period_end: Utc.with_ymd_and_hms(2025, 2, 3, 8, 0, 0).unwrap(),
            period_type: PeriodType::Week,
            timezone: "Etc/UTC".to_string(),
            streams: vec![make_test_stream(
                "abc123def456",
                "tmux/dev/session-1",
                7_200_000,
                4_500_000,
            )],
            tags_by_stream,
            agent_sessions: vec![],
        };

        let output = format_report_json(&data).unwrap();
        assert_snapshot!(output);
    }

    #[test]
    fn test_report_json_tagged_and_untagged() {
        let mut tags_by_stream = HashMap::new();
        tags_by_stream.insert("abc123def456".to_string(), vec!["dev".to_string()]);

        let data = ReportData {
            generated_at: Utc.with_ymd_and_hms(2025, 1, 29, 16, 0, 0).unwrap(),
            period_start: Utc.with_ymd_and_hms(2025, 1, 27, 8, 0, 0).unwrap(),
            period_end: Utc.with_ymd_and_hms(2025, 2, 3, 8, 0, 0).unwrap(),
            period_type: PeriodType::Week,
            timezone: "Etc/UTC".to_string(),
            streams: vec![
                make_test_stream("abc123def456", "tmux/dev/session-1", 1_200_000, 0),
                make_test_stream("def456ghi789", "tmux/dev/session-2", 600_000, 300_000),
            ],
            tags_by_stream,
            agent_sessions: vec![],
        };

        let output = format_report_json(&data).unwrap();
        assert_snapshot!(output);
    }

    #[test]
    fn test_report_json_with_agent_sessions_summary() {
        let period_end = Utc.with_ymd_and_hms(2025, 2, 3, 8, 0, 0).unwrap();
        let long_prompt = "x".repeat(140);
        let data = ReportData {
            generated_at: Utc.with_ymd_and_hms(2025, 1, 29, 16, 0, 0).unwrap(),
            period_start: Utc.with_ymd_and_hms(2025, 1, 27, 8, 0, 0).unwrap(),
            period_end,
            period_type: PeriodType::Week,
            timezone: "Etc/UTC".to_string(),
            streams: vec![make_test_stream(
                "abc123def456",
                "tmux/dev/session-1",
                7_200_000,
                4_500_000,
            )],
            tags_by_stream: HashMap::new(),
            agent_sessions: vec![
                make_test_session(
                    "session-1",
                    SessionSource::Claude,
                    SessionType::User,
                    Utc.with_ymd_and_hms(2025, 1, 28, 10, 0, 0).unwrap(),
                    Some(Utc.with_ymd_and_hms(2025, 1, 28, 10, 30, 0).unwrap()),
                    Some(&long_prompt),
                ),
                make_test_session(
                    "session-2",
                    SessionSource::OpenCode,
                    SessionType::Subagent,
                    Utc.with_ymd_and_hms(2025, 1, 29, 9, 0, 0).unwrap(),
                    None,
                    Some("Short prompt"),
                ),
            ],
        };

        let output = format_report_json(&data).unwrap();
        assert_snapshot!(output);
    }

    #[test]
    fn test_report_json_top_sessions_sorted() {
        let period_end = Utc.with_ymd_and_hms(2025, 2, 3, 8, 0, 0).unwrap();
        let base_start = Utc.with_ymd_and_hms(2025, 1, 28, 9, 0, 0).unwrap();
        let data = ReportData {
            generated_at: Utc.with_ymd_and_hms(2025, 1, 29, 16, 0, 0).unwrap(),
            period_start: Utc.with_ymd_and_hms(2025, 1, 27, 8, 0, 0).unwrap(),
            period_end,
            period_type: PeriodType::Week,
            timezone: "Etc/UTC".to_string(),
            streams: vec![make_test_stream(
                "abc123def456",
                "tmux/dev/session-1",
                7_200_000,
                4_500_000,
            )],
            tags_by_stream: HashMap::new(),
            agent_sessions: vec![
                make_test_session(
                    "session-a",
                    SessionSource::Claude,
                    SessionType::User,
                    base_start,
                    Some(base_start + chrono::Duration::minutes(5)),
                    Some("short"),
                ),
                make_test_session(
                    "session-b",
                    SessionSource::Claude,
                    SessionType::User,
                    base_start,
                    Some(base_start + chrono::Duration::minutes(20)),
                    Some("longer"),
                ),
                make_test_session(
                    "session-c",
                    SessionSource::OpenCode,
                    SessionType::Subagent,
                    base_start,
                    Some(base_start + chrono::Duration::minutes(15)),
                    Some("mid"),
                ),
                make_test_session(
                    "session-d",
                    SessionSource::Claude,
                    SessionType::Subagent,
                    base_start,
                    Some(base_start + chrono::Duration::minutes(30)),
                    Some("longest"),
                ),
                make_test_session(
                    "session-e",
                    SessionSource::OpenCode,
                    SessionType::User,
                    base_start,
                    Some(base_start + chrono::Duration::minutes(25)),
                    Some("second"),
                ),
                make_test_session(
                    "session-f",
                    SessionSource::Claude,
                    SessionType::User,
                    base_start,
                    Some(base_start + chrono::Duration::minutes(10)),
                    Some("cutoff"),
                ),
            ],
        };

        let output = format_report_json(&data).unwrap();
        assert_snapshot!(output);
    }

    #[test]
    fn test_report_json_agent_session_counts_match_total() {
        let period_end = Utc.with_ymd_and_hms(2025, 2, 3, 8, 0, 0).unwrap();
        let data = ReportData {
            generated_at: Utc.with_ymd_and_hms(2025, 1, 29, 16, 0, 0).unwrap(),
            period_start: Utc.with_ymd_and_hms(2025, 1, 27, 8, 0, 0).unwrap(),
            period_end,
            period_type: PeriodType::Week,
            timezone: "Etc/UTC".to_string(),
            streams: vec![make_test_stream(
                "abc123def456",
                "tmux/dev/session-1",
                7_200_000,
                4_500_000,
            )],
            tags_by_stream: HashMap::new(),
            agent_sessions: vec![
                make_test_session(
                    "session-1",
                    SessionSource::Claude,
                    SessionType::User,
                    Utc.with_ymd_and_hms(2025, 1, 28, 10, 0, 0).unwrap(),
                    Some(Utc.with_ymd_and_hms(2025, 1, 28, 10, 30, 0).unwrap()),
                    Some("One"),
                ),
                make_test_session(
                    "session-2",
                    SessionSource::OpenCode,
                    SessionType::Subagent,
                    Utc.with_ymd_and_hms(2025, 1, 28, 11, 0, 0).unwrap(),
                    Some(Utc.with_ymd_and_hms(2025, 1, 28, 11, 20, 0).unwrap()),
                    Some("Two"),
                ),
                make_test_session(
                    "session-3",
                    SessionSource::Claude,
                    SessionType::Subagent,
                    Utc.with_ymd_and_hms(2025, 1, 28, 12, 0, 0).unwrap(),
                    Some(Utc.with_ymd_and_hms(2025, 1, 28, 12, 10, 0).unwrap()),
                    Some("Three"),
                ),
            ],
        };

        let output = format_report_json(&data).unwrap();
        let json: Value = serde_json::from_str(&output).unwrap();
        let total = json["agent_sessions"]["total"].as_u64().unwrap();
        let by_source_total: u64 = json["agent_sessions"]["by_source"]
            .as_object()
            .unwrap()
            .values()
            .map(|v| v.as_u64().unwrap())
            .sum();
        let by_type_total: u64 = json["agent_sessions"]["by_type"]
            .as_object()
            .unwrap()
            .values()
            .map(|v| v.as_u64().unwrap())
            .sum();
        assert_eq!(total, by_source_total);
        assert_eq!(total, by_type_total);
        assert_snapshot!(output);
    }

    #[test]
    fn test_report_single_stream() {
        let data = ReportData {
            generated_at: Utc.with_ymd_and_hms(2025, 1, 29, 16, 0, 0).unwrap(),
            period_start: Utc.with_ymd_and_hms(2025, 1, 29, 8, 0, 0).unwrap(),
            period_end: Utc.with_ymd_and_hms(2025, 1, 30, 8, 0, 0).unwrap(),
            period_type: PeriodType::Day,
            timezone: "Etc/UTC".to_string(),
            streams: vec![make_test_stream(
                "abc123def456",
                "tmux/dev/session-1",
                3_600_000, // 1h direct
                4_500_000, // 1h15m delegated
            )],
            tags_by_stream: HashMap::new(),
            agent_sessions: vec![],
        };

        let output = format_report(&data);
        assert_snapshot!(output);
    }

    #[test]
    fn test_report_truncation() {
        // Create 8 streams to test truncation (>5)
        let streams: Vec<ReportStreamTime> = (0..8)
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
            timezone: "Etc/UTC".to_string(),
            streams,
            tags_by_stream: HashMap::new(),
            agent_sessions: vec![],
        };

        let output = format_report(&data);
        assert_snapshot!(output, @r###"
TIME REPORT: Week of Jan 27, 2025

BY TAG
──────
(no tagged streams)

(untagged)                                 8h 53m  ██████████
  Direct:    5h 40m
  Delegated: 3h 13m
  Sessions:
    stream  tmux/dev/session-0        (1h 30m)
    stream  tmux/dev/session-1        (1h 23m)
    stream  tmux/dev/session-2        (1h 16m)
    stream  tmux/dev/session-3        (1h 10m)
    stream  tmux/dev/session-4        (1h 3m)
    ... and 3 more

  Tip: Run 'tt streams list' to see all

AGENT SESSIONS
──────────────
No agent sessions recorded.

SUMMARY
───────
Total tracked:  8h 53m
Direct time:    5h 40m (64%)
Delegated time: 3h 13m (36%)
"###);
    }

    #[test]
    fn test_percentage_shown_at_30min() {
        // 30 minutes total - percentages should be shown
        let data = ReportData {
            generated_at: Utc.with_ymd_and_hms(2025, 1, 29, 16, 0, 0).unwrap(),
            period_start: Utc.with_ymd_and_hms(2025, 1, 29, 8, 0, 0).unwrap(),
            period_end: Utc.with_ymd_and_hms(2025, 1, 30, 8, 0, 0).unwrap(),
            period_type: PeriodType::Day,
            timezone: "Etc/UTC".to_string(),
            streams: vec![make_test_stream(
                "abc123def456",
                "tmux/dev/session-1",
                1_200_000, // 20m direct
                600_000,   // 10m delegated = 30m total
            )],
            tags_by_stream: HashMap::new(),
            agent_sessions: vec![],
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
            timezone: "Etc/UTC".to_string(),
            streams: vec![make_test_stream(
                "abc123def456",
                "tmux/dev/session-1",
                1_140_000, // 19m direct
                600_000,   // 10m delegated = 29m total
            )],
            tags_by_stream: HashMap::new(),
            agent_sessions: vec![],
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

        // Create a stream with zero time (using tt_db::Stream for database insertion)
        let now = Utc::now();
        let zero_stream = tt_db::Stream {
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

        // Generate report - with no events, the allocation returns no time
        let data = generate_report_data(&db, Period::Week, now).unwrap();

        // Zero-time stream should be excluded (no events = no time allocated)
        assert!(
            data.streams.is_empty(),
            "zero-time streams should be excluded"
        );
    }
}

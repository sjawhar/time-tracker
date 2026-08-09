//! Report command for generating time reports.
//!
//! This module implements `tt report` with various period options
//! (--week, --last-week, --day, --last-day) and output formats (human-readable, JSON).
//!
//! Time is calculated from events within the period using the allocation algorithm,
//! not from cumulative stream totals. This ensures accurate per-period reporting.

use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Local, NaiveDate, Utc};
use serde::Serialize;
use tt_core::AllocationConfig;
use tt_db::{Database, WindowedAgentSession, allocate_for_period};

mod format;
mod json;
mod period;
mod render;
mod sessions;
mod tags;

pub use format::{format_duration, format_leverage, progress_bar};
pub use json::{
    JsonPeriod, JsonReport, JsonStreamEntry, JsonTagEntry, JsonTotals, JsonUntagged,
    JsonWeeksReport, build_json_report, format_report_json,
};
pub use period::{get_period_boundaries, local_midnight_to_utc};
pub use render::format_report;
pub use sessions::{JsonAgentSessionEntry, JsonAgentSessionSummary};

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

/// Report period type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    Week,
    LastWeek,
    Day,
    LastDay,
    Custom(DateTime<Utc>, DateTime<Utc>),
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
    /// Wall-clock time with any activity (union of all intervals, not a sum).
    pub total_tracked_ms: i64,
    /// Tag mappings for streams included in the report period.
    pub tags_by_stream: HashMap<String, Vec<String>>,
    /// Agent sessions that emitted activity inside the report period.
    ///
    /// Scoped by observed activity, not by nominal span: a session whose
    /// `agent_sessions` row brackets the period but which did nothing in it is
    /// absent. See `tt_db::session_activity`.
    pub agent_sessions: Vec<WindowedAgentSession>,
    /// Direct (human attention) time on activity not assigned to any stream.
    pub unassigned_direct_ms: i64,
    /// Delegated (agent) time on activity not assigned to any stream.
    pub unassigned_delegated_ms: i64,
    /// Id of the reserved junk stream, when the database has one.
    ///
    /// Carried so the renderer can lift junk out of the rankings while still
    /// reporting its totals. Resolved by slug, because the reserved row's id is
    /// only incidentally equal to it.
    pub junk_stream_id: Option<String>,
}

impl ReportData {
    /// Total human attention in the period: the union of focus intervals across
    /// every stream, plus whatever attention landed on unattributed activity.
    pub fn total_direct_ms(&self) -> i64 {
        self.streams.iter().map(|s| s.time_direct_ms).sum::<i64>() + self.unassigned_direct_ms
    }

    /// Total agent time in the period. A *sum* across concurrent sessions, so it
    /// routinely exceeds wall clock — that is the leverage signal, not a bug.
    pub fn total_delegated_ms(&self) -> i64 {
        self.streams
            .iter()
            .map(|s| s.time_delegated_ms)
            .sum::<i64>()
            + self.unassigned_delegated_ms
    }
}

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
        Period::Day | Period::LastDay | Period::Custom(_, _) => PeriodType::Day,
    };

    let config = AllocationConfig::default();
    let result = allocate_for_period(db, period_start, period_end, Some(period_end), &config)
        .context("failed to allocate time for period")?;
    let agent_sessions = db
        .agent_sessions_active_in_range(period_start, period_end)
        .context("failed to get agent sessions active in period")?;

    // Get stream metadata (names) for display
    let all_streams = db.get_streams().context("failed to get streams")?;
    let junk_stream_id = all_streams
        .iter()
        .find(|s| s.slug.as_deref() == Some(tt_db::JUNK_STREAM_SLUG))
        .map(|s| s.id.clone());
    let stream_names: HashMap<String, Option<String>> =
        all_streams.into_iter().map(|s| (s.id, s.name)).collect();

    let tags_by_stream: HashMap<String, Vec<String>> = db
        .get_all_tags()
        .context("failed to get stream tags")?
        .into_iter()
        .collect();

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
        total_tracked_ms: result.total_tracked_ms,
        tags_by_stream,
        agent_sessions,
        unassigned_direct_ms: result.unassigned_direct_ms,
        unassigned_delegated_ms: result.unassigned_delegated_ms,
        junk_stream_id,
    })
}

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

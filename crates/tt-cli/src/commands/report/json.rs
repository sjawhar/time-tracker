//! Machine-readable report output (`tt report --json`).
//!
//! Consumed by other tooling, so the shape here is a contract: add fields,
//! never rename or remove them.

use anyhow::Result;
use chrono::Local;
use serde::Serialize;

use super::sessions::{JsonAgentSessionSummary, build_agent_session_summary};
use super::tags::{build_tag_times, is_tagged};
use super::{PeriodType, ReportData};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod weeks_tests;

const DEFAULT_WEEK_START_DAY: &str = "monday";

/// JSON report structure.
#[derive(Debug, Serialize)]
pub struct JsonReport {
    pub generated_at: String,
    pub timezone: String,
    pub week_start_day: String,
    pub period: JsonPeriod,
    pub by_tag: Vec<JsonTagEntry>,
    pub streams: Vec<JsonStreamEntry>,
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

/// Per-stream computed time within the report period.
#[derive(Debug, Serialize)]
pub struct JsonStreamEntry {
    pub id: String,
    pub name: Option<String>,
    pub time_direct_ms: i64,
    pub time_delegated_ms: i64,
    pub tags: Vec<String>,
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
    /// Direct time on activity not assigned to any stream (subset of `time_direct_ms`).
    pub unassigned_direct_ms: i64,
    /// Delegated time on activity not assigned to any stream (subset of `time_delegated_ms`).
    pub unassigned_delegated_ms: i64,
    pub total_tracked_ms: i64,
}
/// Formats report data as JSON.
pub fn format_report_json(data: &ReportData) -> Result<String> {
    let report = build_json_report(data);
    Ok(serde_json::to_string_pretty(&report)?)
}

pub fn build_json_report(data: &ReportData) -> JsonReport {
    let local_start = data.period_start.with_timezone(&Local);
    let local_end = data.period_end.with_timezone(&Local);

    // For end date in JSON, we need the last day of the period (inclusive)
    // Since period_end is the first moment of the next period, subtract 1 day
    let end_date = (local_end.date_naive() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();

    // The junk stream stays in the machine-readable contract: consumers can
    // recognise it by id, and dropping it here would make `totals` unreconcilable
    // with `streams`. Only the human-readable report lifts it out of the ranking.
    let all_streams: Vec<&super::ReportStreamTime> = data.streams.iter().collect();
    let by_tag = build_tag_times(&all_streams, &data.tags_by_stream)
        .into_iter()
        .map(|entry| JsonTagEntry {
            tag: entry.tag,
            time_direct_ms: entry.time_direct_ms,
            time_delegated_ms: entry.time_delegated_ms,
            streams: entry.streams,
        })
        .collect();

    let mut stream_entries: Vec<JsonStreamEntry> = data
        .streams
        .iter()
        .filter(|s| s.time_direct_ms > 0 || s.time_delegated_ms > 0)
        .map(|s| {
            let mut tags = data.tags_by_stream.get(&s.id).cloned().unwrap_or_default();
            tags.sort();
            JsonStreamEntry {
                id: s.id.clone(),
                name: s.name.clone(),
                time_direct_ms: s.time_direct_ms,
                time_delegated_ms: s.time_delegated_ms,
                tags,
            }
        })
        .collect();
    // Ties are broken by id, not left to the caller's ordering: `data.streams`
    // arrives in `allocate_time`'s HashMap order, so a stable sort alone made a
    // consumed contract emit a different stream order on every run.
    stream_entries.sort_by(|a, b| {
        b.time_direct_ms
            .cmp(&a.time_direct_ms)
            .then_with(|| b.time_delegated_ms.cmp(&a.time_delegated_ms))
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut untagged_direct_ms = 0;
    let mut untagged_delegated_ms = 0;
    let mut untagged_streams = Vec::new();
    for stream in data
        .streams
        .iter()
        .filter(|s| !is_tagged(&data.tags_by_stream, &s.id))
    {
        untagged_direct_ms += stream.time_direct_ms;
        untagged_delegated_ms += stream.time_delegated_ms;
        untagged_streams.push(stream.id.clone());
    }
    // Same reason as above: `data.streams` order is not reproducible.
    untagged_streams.sort();

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
        streams: stream_entries,
        untagged: JsonUntagged {
            time_direct_ms: untagged_direct_ms,
            time_delegated_ms: untagged_delegated_ms,
            streams: untagged_streams,
        },
        agent_sessions: build_agent_session_summary(&data.agent_sessions),
        totals: JsonTotals {
            time_direct_ms: data.total_direct_ms(),
            time_delegated_ms: data.total_delegated_ms(),
            stream_count: data.streams.len(),
            unassigned_direct_ms: data.unassigned_direct_ms,
            unassigned_delegated_ms: data.unassigned_delegated_ms,
            total_tracked_ms: data.total_tracked_ms,
        },
    }
}

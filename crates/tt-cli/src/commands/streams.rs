//! Streams command for listing streams with time totals and tags.
//!
//! This module implements `tt streams` which displays all streams
//! from the last 7 days with their direct/delegated time and tags.

use std::fmt::Write;

use anyhow::Result;
use chrono::{DateTime, Local, LocalResult, NaiveDate, NaiveTime, TimeZone, Utc};
use serde::Serialize;
use tt_db::Database;

use super::report::format_duration;

mod link;
pub use link::{LinkOptions, link};

// ========== Period Calculation ==========

/// Converts a local date at midnight to UTC.
/// Handles DST ambiguity by picking the earlier time.
fn local_midnight_to_utc(local_date: NaiveDate) -> DateTime<Utc> {
    let midnight = local_date.and_time(NaiveTime::from_hms_opt(0, 0, 0).unwrap());
    match Local.from_local_datetime(&midnight) {
        LocalResult::Single(dt) | LocalResult::Ambiguous(dt, _) => dt.with_timezone(&Utc),
        LocalResult::None => {
            // DST spring-forward gap at midnight
            let one_am = local_date.and_time(NaiveTime::from_hms_opt(1, 0, 0).unwrap());
            Local
                .from_local_datetime(&one_am)
                .unwrap()
                .with_timezone(&Utc)
        }
    }
}

/// Get the last 7 days boundary (inclusive of today).
fn last_7_days_boundary(today: NaiveDate) -> DateTime<Utc> {
    let start_date = today - chrono::Duration::days(6); // Today + 6 days back = 7 days
    local_midnight_to_utc(start_date)
}

// ========== Stream Data ==========

/// Stream data for display.
#[derive(Debug, Clone, Serialize)]
pub struct StreamEntry {
    pub id: String,
    pub id_short: String,
    pub name: Option<String>,
    pub time_direct_ms: i64,
    pub time_delegated_ms: i64,
    pub tags: Vec<String>,
}

/// Get streams from the last 7 days, filtered and sorted.
pub fn get_streams_for_display(db: &Database, today: NaiveDate) -> Result<Vec<StreamEntry>> {
    let period_start = last_7_days_boundary(today);

    let streams_with_tags = db.get_streams_with_tags()?;

    let mut entries: Vec<StreamEntry> = streams_with_tags
        .into_iter()
        .filter(|(stream, _)| {
            // Filter by period: last_event_at must be within last 7 days
            stream.last_event_at.is_some_and(|t| t >= period_start)
        })
        .filter(|(stream, _)| {
            // Exclude zero-time streams
            stream.time_direct_ms > 0 || stream.time_delegated_ms > 0
        })
        .map(|(stream, tags)| {
            let id_short: String = stream.id.chars().take(6).collect();
            StreamEntry {
                id: stream.id,
                id_short,
                name: stream.name,
                time_direct_ms: stream.time_direct_ms,
                time_delegated_ms: stream.time_delegated_ms,
                tags,
            }
        })
        .collect();

    // Sort by total time descending
    entries.sort_by_key(|e| std::cmp::Reverse(e.time_direct_ms + e.time_delegated_ms));

    Ok(entries)
}

// ========== Human-Readable Output ==========

/// Format streams for human-readable output.
pub fn format_streams(entries: &[StreamEntry]) -> String {
    let mut output = String::new();

    writeln!(output, "STREAMS (last 7 days)").unwrap();
    writeln!(output).unwrap();

    if entries.is_empty() {
        writeln!(output, "No streams with activity in the last 7 days.").unwrap();
        writeln!(output).unwrap();
        writeln!(
            output,
            "Hint: Run 'ssh <remote> tt export | tt import' to import events from a remote host."
        )
        .unwrap();
        return output;
    }

    // Header
    writeln!(
        output,
        "{:<7}  {:<22}  {:>8}  {:>9}  Tags",
        "ID", "Name", "Direct", "Delegated"
    )
    .unwrap();
    writeln!(
        output,
        "───────  ──────────────────────  ────────  ─────────  ──────────────────"
    )
    .unwrap();

    // Rows
    for entry in entries {
        let name = entry.name.as_deref().unwrap_or("(unnamed)");
        // Truncate by characters, not bytes, to avoid panics on multi-byte UTF-8
        let name_display = if name.chars().count() > 22 {
            format!("{}...", name.chars().take(19).collect::<String>())
        } else {
            name.to_string()
        };
        let direct = format_duration(entry.time_direct_ms);
        let delegated = format_duration(entry.time_delegated_ms);
        let tags = entry.tags.join(", ");

        writeln!(
            output,
            "{:<7}  {:<22}  {:>8}  {:>9}  {}",
            entry.id_short, name_display, direct, delegated, tags
        )
        .unwrap();
    }

    // Tip
    writeln!(output).unwrap();
    writeln!(
        output,
        "Tip: Use 'tt tag <id> <tag>' to group sessions into projects."
    )
    .unwrap();

    output
}

// ========== JSON Output ==========

/// JSON output structure.
#[derive(Debug, Serialize)]
pub struct JsonStreams {
    pub streams: Vec<StreamEntry>,
    pub period: JsonPeriod,
}

#[derive(Debug, Serialize)]
pub struct JsonPeriod {
    pub start: String,
    pub end: String,
}

/// Format streams as JSON.
pub fn format_streams_json(entries: &[StreamEntry], today: NaiveDate) -> Result<String> {
    let start_date = today - chrono::Duration::days(6);

    let json_streams = JsonStreams {
        streams: entries.to_vec(),
        period: JsonPeriod {
            start: start_date.format("%Y-%m-%d").to_string(),
            end: today.format("%Y-%m-%d").to_string(),
        },
    };

    Ok(serde_json::to_string_pretty(&json_streams)?)
}

// ========== Public Interface ==========

/// Runs the streams command.
pub fn run(db: &Database, json: bool) -> Result<()> {
    let today = Local::now().date_naive();
    let entries = get_streams_for_display(db, today)?;

    if json {
        let output = format_streams_json(&entries, today)?;
        println!("{output}");
    } else {
        let output = format_streams(&entries);
        print!("{output}");
    }

    Ok(())
}

/// Create a new stream with the given name.
///
/// Generates a UUID, inserts the stream into the database, and prints the ID to stdout.
pub fn create(db: &Database, name: String) -> Result<()> {
    use anyhow::Context;
    use tt_db::Stream;
    use uuid::Uuid;

    let now = Utc::now();

    let stream = Stream {
        id: Uuid::new_v4().to_string(),
        name: Some(name),
        created_at: now,
        updated_at: now,
        time_direct_ms: 0,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: true,
    };

    db.insert_stream(&stream)
        .context("failed to create stream")?;
    println!("{}", stream.id);
    Ok(())
}

#[cfg(test)]
mod tests;

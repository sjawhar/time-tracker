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
use super::util::format_age;

mod link;
pub use link::{LinkOptions, link};
mod describe;
pub use describe::{backfill, describe};
mod dissolve;
pub use dissolve::dissolve;
mod release_pane_focus;
pub use release_pane_focus::release_pane_focus;
mod slug;
pub use slug::set_slug;
mod misnamed;
pub use misnamed::run as misnamed_report;
mod merge;
pub use merge::merge;
mod rename;
pub use rename::rename;
mod assign;
pub use assign::assign;

/// Labels a stream as `name (id-prefix)` for report headers and confirmations.
fn format_stream_label(name: Option<&str>, id: &str) -> String {
    format!(
        "{} ({})",
        name.unwrap_or("(unnamed)"),
        id.chars().take(8).collect::<String>()
    )
}

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
    pub slug: Option<String>,
    pub name: Option<String>,
    pub time_direct_ms: i64,
    pub time_delegated_ms: i64,
    /// When `tt recompute` last wrote this stream's times.
    ///
    /// Display-only: the aggregate surfaces in JSON as `times_computed_at`, so
    /// repeating it per row would just be noise.
    #[serde(skip)]
    pub updated_at: DateTime<Utc>,
    pub tags: Vec<String>,
}

/// Get streams from the last 7 days, filtered and sorted.
///
/// Recency comes from `events`, via [`Database::stream_activity_windows`]. The
/// `streams.last_event_at` column names exactly this and answers it wrongly:
/// nothing writes it, so 985 of the live table's 1,245 streams have it NULL and
/// the newest value among the other 260 is 2026-04-30 — 99 days behind the
/// newest event. Reading it printed "No streams with activity in the last 7
/// days." on a database with 90 such streams. See `tt-db`'s `AGENTS.md`.
pub fn get_streams_for_display(db: &Database, today: NaiveDate) -> Result<Vec<StreamEntry>> {
    let period_start = last_7_days_boundary(today);

    let activity = db.stream_activity_windows()?;
    let streams_with_tags = db.get_streams_with_tags()?;

    let mut entries: Vec<StreamEntry> = streams_with_tags
        .into_iter()
        .filter(|(stream, _)| {
            // Filter by period: the stream's newest event must be within the
            // last 7 days. A stream with no events has no window and is out.
            activity
                .get(&stream.id)
                .is_some_and(|window| window.last >= period_start)
        })
        .filter(|(stream, _)| {
            // Exclude zero-time streams
            stream.time_direct_ms > 0 || stream.time_delegated_ms > 0
        })
        .map(|(stream, tags)| {
            let id_short: String = stream.id.chars().take(6).collect();
            StreamEntry {
                slug: stream.slug,
                id: stream.id,
                id_short,
                name: stream.name,
                time_direct_ms: stream.time_direct_ms,
                time_delegated_ms: stream.time_delegated_ms,
                updated_at: stream.updated_at,
                tags,
            }
        })
        .collect();

    // Direct time descending, the same axis `tt report`'s BY STREAM uses, because this is a
    // human-readable view and "where did my time go" means direct time.
    //
    // This used to sort by `direct + delegated`, which is the regression the root AGENTS.md
    // names outright: delegated routinely exceeds direct by 10-100x, so the sum *is* the
    // delegated ordering, and a stream with minutes of attention and hundreds of agent-hours
    // takes the top. Measured on the live table, `workorder-5: agent-c core` (13h 56m direct,
    // 51h 57m delegated) sorted below three streams with 6h 32m, 4h 48m and 3h 6m of direct
    // time. Summing them is also meaningless arithmetic: one is wall-clock hours and the
    // other machine-hours, so they share no denominator.
    entries.sort_by(|a, b| {
        b.time_direct_ms
            .cmp(&a.time_direct_ms)
            .then_with(|| b.time_delegated_ms.cmp(&a.time_delegated_ms))
            .then_with(|| a.id.cmp(&b.id))
    });

    Ok(entries)
}

/// Most recent `updated_at` among `entries` — when `tt recompute` last wrote
/// their times.
///
/// `None` exactly when `entries` is empty.
fn times_computed_at(entries: &[StreamEntry]) -> Option<DateTime<Utc>> {
    entries.iter().map(|entry| entry.updated_at).max()
}

// ========== Human-Readable Output ==========

/// Format streams for human-readable output.
///
/// The reserved junk stream is lifted out of the listing and reported on one
/// line below it, for the same reason `tt report` does: junk is not work, so
/// ranking it beside real streams is noise, but its totals must stay visible or
/// a junk rule that starts eating real work becomes silent. JSON keeps it as an
/// ordinary entry — machine consumers can recognise it by slug.
pub fn format_streams(entries: &[StreamEntry], now: DateTime<Utc>) -> Result<String> {
    let mut output = String::new();

    writeln!(output, "STREAMS (last 7 days)")?;
    writeln!(output)?;

    let Some(times_computed_at) = times_computed_at(entries) else {
        writeln!(output, "No streams with activity in the last 7 days.")?;
        writeln!(output)?;
        writeln!(
            output,
            "Hint: Run 'ssh <remote> tt export | tt import' to import events from a remote host."
        )?;
        return Ok(output);
    };

    // Header
    writeln!(
        output,
        "{:<7}  {:<16}  {:<22}  {:>8}  {:>9}  Tags",
        "ID", "Slug", "Name", "Direct", "Delegated"
    )?;
    writeln!(
        output,
        "───────  ────────────────  ──────────────────────  ────────  ─────────  ──────────────────"
    )?;

    // Rows
    let (listed, junk): (Vec<&StreamEntry>, Vec<&StreamEntry>) = entries
        .iter()
        .partition(|entry| entry.slug.as_deref() != Some(tt_db::JUNK_STREAM_SLUG));
    for entry in listed {
        let slug = entry.slug.as_deref().unwrap_or("-");
        let slug_display = if slug.chars().count() > 16 {
            format!("{}...", slug.chars().take(13).collect::<String>())
        } else {
            slug.to_string()
        };
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
            "{:<7}  {:<16}  {:<22}  {:>8}  {:>9}  {}",
            entry.id_short, slug_display, name_display, direct, delegated, tags
        )?;
    }

    for entry in junk {
        writeln!(
            output,
            "  (junk: {} direct, {} delegated — not listed above; 'tt streams dissolve junk' releases it)",
            format_duration(entry.time_direct_ms),
            format_duration(entry.time_delegated_ms),
        )?;
    }

    // Freshness note + tip
    writeln!(output)?;
    writeln!(
        output,
        "Times last computed {} ago. Run 'tt recompute' to refresh.",
        format_age(times_computed_at, now)
    )?;
    writeln!(
        output,
        "Tip: Use 'tt tag <id> <tag>' to group sessions into projects."
    )?;

    Ok(output)
}

// ========== JSON Output ==========

/// JSON output structure.
#[derive(Debug, Serialize)]
pub struct JsonStreams {
    pub streams: Vec<StreamEntry>,
    pub period: JsonPeriod,
    /// When `tt recompute` last wrote the listed streams' times; `None` when
    /// no streams are listed.
    pub times_computed_at: Option<String>,
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
        times_computed_at: times_computed_at(entries)
            .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
    };

    Ok(serde_json::to_string_pretty(&json_streams)?)
}

// ========== Public Interface ==========

/// Runs the streams command.
pub fn run(db: &Database, json: bool) -> Result<()> {
    let now = Local::now();
    let today = now.date_naive();
    let entries = get_streams_for_display(db, today)?;

    if json {
        let output = format_streams_json(&entries, today)?;
        println!("{output}");
    } else {
        let output = format_streams(&entries, now.with_timezone(&Utc))?;
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
        slug: None,
        description: None,
        color: None,
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

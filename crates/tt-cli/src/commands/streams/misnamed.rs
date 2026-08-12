//! `tt streams list --misnamed` — the existing streams whose name does not
//! describe work.
//!
//! `tt_core::is_misnamed_stream` judges names as they are proposed, so it says
//! nothing about the containers minted before it existed. Those are still
//! standing and still receiving assignments. This lists them.
//!
//! It is a report and nothing else: no row is renamed, merged, or dissolved here.
//! The reason column is what decides which of those three a row deserves, and the
//! three are not interchangeable — a `date_range` name is usually a real
//! initiative bucketed into a week, so dissolving it destroys correct
//! attribution, while a `catch_all` never described work at all.

use std::fmt::Write;

use anyhow::{Context, Result};
use serde::Serialize;
use tt_core::classification::{MisnamedReason, is_misnamed_stream};
use tt_db::Database;

use crate::commands::report::format_duration;

/// One existing stream whose name does not describe work.
#[derive(Debug, Clone, Serialize)]
pub struct MisnamedStream {
    pub id: String,
    pub id_short: String,
    pub name: String,
    /// What the name describes instead: `activity_type`, `date_range`, `instance_suffix`, or
    /// `catch_all`.
    pub reason: &'static str,
    /// Events currently pointing at this stream, whoever assigned them.
    pub events: u64,
    /// Materialized direct time, as last written by `tt recompute`.
    pub time_direct_ms: i64,
}

const fn reason_key(reason: MisnamedReason) -> &'static str {
    match reason {
        MisnamedReason::ActivityType => "activity_type",
        MisnamedReason::DateRange => "date_range",
        MisnamedReason::InstanceSuffix => "instance_suffix",
        MisnamedReason::CatchAll => "catch_all",
        MisnamedReason::Fragmented => "fragmented",
    }
}

/// Collects every named stream whose name does not describe work.
///
/// Reads only. Unnamed streams are skipped — there is no name to judge.
///
/// Ordered by event count descending, because the question this report answers is
/// what is at stake in acting on a row, and events are the unit that moves. Direct
/// time rides alongside for the attention a row is holding.
pub fn collect_misnamed(db: &Database) -> Result<Vec<MisnamedStream>> {
    let mut misnamed: Vec<MisnamedStream> = db
        .get_streams()
        .context("failed to load streams")?
        .into_iter()
        .filter_map(|stream| {
            let name = stream.name?;
            let reason = is_misnamed_stream(&name)?;
            Some((stream.id, name, reason, stream.time_direct_ms))
        })
        .map(|(id, name, reason, time_direct_ms)| {
            let events = db
                .count_events_by_stream(&id)
                .with_context(|| format!("failed to count events for stream {id}"))?;
            Ok(MisnamedStream {
                id_short: id.chars().take(8).collect(),
                id,
                name,
                reason: reason_key(reason),
                events,
                time_direct_ms,
            })
        })
        .collect::<Result<_>>()?;

    misnamed.sort_by_key(|entry| std::cmp::Reverse(entry.events));
    Ok(misnamed)
}

/// Truncates by characters, not bytes, to avoid panics on multi-byte UTF-8.
fn truncate(value: &str, width: usize) -> String {
    if value.chars().count() > width {
        format!("{}...", value.chars().take(width - 3).collect::<String>())
    } else {
        value.to_string()
    }
}

/// Renders one row per misnamed stream, then the totals and how to act on them.
pub fn format_misnamed(entries: &[MisnamedStream]) -> Result<String> {
    let mut output = String::new();
    writeln!(output, "MISNAMED STREAMS")?;
    writeln!(output)?;

    if entries.is_empty() {
        writeln!(output, "Every existing stream name describes work.")?;
        return Ok(output);
    }

    writeln!(
        output,
        "These names describe an activity, a date, an execution instance, or a leftover bucket rather than the work."
    )?;
    writeln!(output)?;

    writeln!(
        output,
        "{:<44}  {:<8}  {:<13}  {:>8}  {:>8}",
        "Name", "ID", "Reason", "Events", "Direct"
    )?;
    writeln!(
        output,
        "────────────────────────────────────────────  ────────  \
         ─────────────  ────────  ────────"
    )?;
    for entry in entries {
        writeln!(
            output,
            "{:<44}  {:<8}  {:<13}  {:>8}  {:>8}",
            truncate(&entry.name, 44),
            entry.id_short,
            entry.reason,
            entry.events,
            format_duration(entry.time_direct_ms)
        )?;
    }

    let events: u64 = entries.iter().map(|entry| entry.events).sum();
    let direct: i64 = entries.iter().map(|entry| entry.time_direct_ms).sum();
    writeln!(output)?;
    writeln!(
        output,
        "Total: {} streams, {events} events, {} direct.",
        entries.len(),
        format_duration(direct)
    )?;
    writeln!(
        output,
        "Nothing was written. A date_range name is usually a real initiative bucketed \
         into a week:\nstrip the suffix with 'tt streams rename', then collapse the weeks \
         with 'tt streams merge'.\nOnly a name that never described work belongs in \
         'tt streams dissolve'."
    )?;
    Ok(output)
}

/// JSON shape of the misnamed-stream report.
#[derive(Debug, Serialize)]
struct JsonMisnamed<'a> {
    streams: &'a [MisnamedStream],
    total_events: u64,
    total_direct_ms: i64,
}

/// Renders the misnamed-stream report as JSON.
pub fn format_misnamed_json(entries: &[MisnamedStream]) -> Result<String> {
    let json = JsonMisnamed {
        streams: entries,
        total_events: entries.iter().map(|entry| entry.events).sum(),
        total_direct_ms: entries.iter().map(|entry| entry.time_direct_ms).sum(),
    };
    Ok(serde_json::to_string_pretty(&json)?)
}

/// Runs the misnamed-stream report.
pub fn run(db: &Database, json: bool) -> Result<()> {
    let entries = collect_misnamed(db)?;
    if json {
        println!("{}", format_misnamed_json(&entries)?);
    } else {
        print!("{}", format_misnamed(&entries)?);
    }
    Ok(())
}

#[cfg(test)]
mod tests;

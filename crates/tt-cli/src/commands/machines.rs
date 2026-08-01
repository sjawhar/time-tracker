//! Machines command for listing known remotes and flagging the ones gone dark.

use std::fmt::Write;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use tt_db::Database;

use crate::commands::util::format_age;

/// A remote that has produced no events for this many days has gone dark.
///
/// Deliberately a constant rather than a config key: this is a "something is
/// broken" alarm, not a preference worth tuning away.
const DARK_AFTER_DAYS: i64 = 7;

/// A known machine together with the freshness of the events it has produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineStatus {
    pub machine_id: String,
    pub label: String,
    pub last_sync_at: Option<String>,
    pub last_event_at: Option<DateTime<Utc>>,
}

/// Whether a machine has gone dark: no events at all, or none within
/// [`DARK_AFTER_DAYS`].
///
/// Keyed on event timestamps rather than `last_sync_at` on purpose — a sync
/// that succeeds but returns nothing is exactly the silent failure worth
/// catching.
fn is_dark(last_event_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    last_event_at.is_none_or(|last| now - last >= Duration::days(DARK_AFTER_DAYS))
}

/// Loads every known machine alongside its most recent event timestamp.
pub fn load_statuses(db: &Database) -> Result<Vec<MachineStatus>> {
    db.list_machines()
        .context("failed to list machines")?
        .into_iter()
        .map(|machine| {
            let last_event_at = db
                .get_last_event_timestamp_for_machine(&machine.machine_id)
                .with_context(|| {
                    format!("failed to read last event for machine {}", machine.label)
                })?
                .map(|raw| {
                    DateTime::parse_from_rfc3339(&raw)
                        .with_context(|| {
                            format!("invalid event timestamp for machine {}", machine.label)
                        })
                        .map(|timestamp| timestamp.with_timezone(&Utc))
                })
                .transpose()?;
            Ok(MachineStatus {
                machine_id: machine.machine_id,
                label: machine.label,
                last_sync_at: machine.last_sync_at,
                last_event_at,
            })
        })
        .collect()
}

/// One warning line per dark machine; empty when every remote is still
/// reporting.
pub fn dark_machine_warnings(statuses: &[MachineStatus], now: DateTime<Utc>) -> Vec<String> {
    statuses
        .iter()
        .filter(|status| is_dark(status.last_event_at, now))
        .map(|status| {
            let silence = status.last_event_at.map_or_else(
                || "has never sent any events".to_owned(),
                |last| format!("has sent no events for {}", format_age(last, now)),
            );
            format!(
                "⚠ {} {silence} — check that tt is still running there.",
                status.label
            )
        })
        .collect()
}

/// Renders the machine table, marking every dark machine on its own row.
pub fn format_machines(statuses: &[MachineStatus], now: DateTime<Utc>) -> Result<String> {
    let mut output = String::new();

    if statuses.is_empty() {
        writeln!(
            output,
            "No machines registered yet. Run 'tt sync <remote>' to import from a remote."
        )?;
        return Ok(output);
    }

    writeln!(
        output,
        "{:<38} {:<20} {:<26} {:<11} STATUS",
        "MACHINE ID", "LABEL", "LAST SYNC", "LAST EVENT"
    )?;
    for status in statuses {
        let last_sync = status.last_sync_at.as_deref().unwrap_or("never");
        let last_event = status.last_event_at.map_or_else(
            || "never".to_owned(),
            |last| format!("{} ago", format_age(last, now)),
        );
        let health = if is_dark(status.last_event_at, now) {
            "⚠ DARK"
        } else {
            "ok"
        };
        writeln!(
            output,
            "{:<38} {:<20} {:<26} {last_event:<11} {health}",
            status.machine_id, status.label, last_sync
        )?;
    }

    let warnings = dark_machine_warnings(statuses, now);
    if !warnings.is_empty() {
        writeln!(output)?;
        for warning in &warnings {
            writeln!(output, "{warning}")?;
        }
    }

    Ok(output)
}

/// Runs the machines command.
pub fn run(db: &Database) -> Result<()> {
    let statuses = load_statuses(db)?;
    print!("{}", format_machines(&statuses, Utc::now())?);
    Ok(())
}

#[cfg(test)]
mod tests;

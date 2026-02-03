//! Events command for querying local `SQLite` database.
//!
//! This module outputs events from the local database as JSONL for debugging.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use tt_db::Database;

/// Runs the events command, outputting events as JSONL to stdout.
pub fn run(db: &Database, after: Option<&str>, before: Option<&str>) -> Result<()> {
    let after = parse_timestamp(after, "after")?;
    let before = parse_timestamp(before, "before")?;

    let events = db.get_events(after, before)?;

    for event in events {
        let json = serde_json::to_string(&event)?;
        println!("{json}");
    }

    Ok(())
}

fn parse_timestamp(s: Option<&str>, name: &str) -> Result<Option<DateTime<Utc>>> {
    match s {
        None => Ok(None),
        Some(s) => {
            let dt = DateTime::parse_from_rfc3339(s).with_context(|| {
                format!(
                    "invalid --{name} timestamp, expected ISO 8601 (e.g., 2025-01-29T12:00:00Z)"
                )
            })?;
            Ok(Some(dt.with_timezone(&Utc)))
        }
    }
}

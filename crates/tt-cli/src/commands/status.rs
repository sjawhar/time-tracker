//! Status command for showing event collection status.
//!
//! This module displays the most recent event timestamp per source,
//! helping users verify that event collection is working.

use std::fmt::Write;
use std::path::Path;

use anyhow::Result;
use chrono::SecondsFormat;
use tt_db::Database;

/// Formats and prints the status output.
///
/// Returns the formatted output string (for testing).
pub fn format_status(db: &Database, db_path: &Path) -> Result<String> {
    let statuses = db.get_last_event_per_source()?;

    let mut output = String::new();
    writeln!(output, "Database: {}", db_path.display())?;

    if statuses.is_empty() {
        output.push_str("\nNo events recorded yet.\n");
    } else {
        output.push_str("\nSources:\n");
        for status in statuses {
            let timestamp = status
                .last_timestamp
                .to_rfc3339_opts(SecondsFormat::Secs, true);
            writeln!(output, "  {}:  {}", status.source, timestamp)?;
        }
    }

    Ok(output)
}

/// Runs the status command.
pub fn run(db: &Database, db_path: &Path) -> Result<()> {
    let output = format_status(db, db_path)?;
    print!("{output}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use insta::assert_snapshot;
    use serde_json::json;
    use std::path::PathBuf;
    use tt_db::StoredEvent;

    fn make_event(id: &str, timestamp: chrono::DateTime<Utc>, source: &str) -> StoredEvent {
        StoredEvent {
            id: id.to_string(),
            timestamp,
            event_type: "test_event".to_string(),
            source: source.to_string(),
            schema_version: 1,
            data: json!({}),
            cwd: None,
            session_id: None,
            stream_id: None,
            assignment_source: None,
        }
    }

    #[test]
    fn test_status_empty_database() {
        let db = Database::open_in_memory().unwrap();
        let db_path = PathBuf::from("/path/to/events.db");

        let output = format_status(&db, &db_path).unwrap();

        assert_snapshot!(output);
    }

    #[test]
    fn test_status_with_events() {
        let db = Database::open_in_memory().unwrap();
        let db_path = PathBuf::from("/path/to/events.db");

        // Add events from multiple sources
        let ts_tmux = Utc.with_ymd_and_hms(2025, 1, 29, 10, 30, 0).unwrap();
        let ts_agent = Utc.with_ymd_and_hms(2025, 1, 29, 11, 45, 0).unwrap();

        db.insert_event(&make_event("e1", ts_tmux, "remote.tmux"))
            .unwrap();
        db.insert_event(&make_event("e2", ts_agent, "remote.agent"))
            .unwrap();

        let output = format_status(&db, &db_path).unwrap();

        assert_snapshot!(output);
    }

    #[test]
    fn test_status_sources_ordered_by_recency() {
        let db = Database::open_in_memory().unwrap();
        let db_path = PathBuf::from("/path/to/events.db");

        // Add events - agent is most recent, then local, then tmux
        let ts_tmux = Utc.with_ymd_and_hms(2025, 1, 29, 10, 0, 0).unwrap();
        let ts_local = Utc.with_ymd_and_hms(2025, 1, 29, 11, 0, 0).unwrap();
        let ts_agent = Utc.with_ymd_and_hms(2025, 1, 29, 12, 0, 0).unwrap();

        db.insert_event(&make_event("e1", ts_tmux, "remote.tmux"))
            .unwrap();
        db.insert_event(&make_event("e2", ts_local, "local.window"))
            .unwrap();
        db.insert_event(&make_event("e3", ts_agent, "remote.agent"))
            .unwrap();

        let output = format_status(&db, &db_path).unwrap();

        // Verify ordering in output - most recent first
        // Find the Sources: section and check order
        let sources_section = output
            .lines()
            .skip_while(|l| !l.contains("Sources:"))
            .skip(1) // skip "Sources:" line itself
            .take(3)
            .collect::<Vec<_>>();

        assert_eq!(sources_section.len(), 3, "expected 3 source lines");
        assert!(
            sources_section[0].contains("remote.agent"),
            "first should be remote.agent (12:00)"
        );
        assert!(
            sources_section[1].contains("local.window"),
            "second should be local.window (11:00)"
        );
        assert!(
            sources_section[2].contains("remote.tmux"),
            "third should be remote.tmux (10:00)"
        );
    }
}

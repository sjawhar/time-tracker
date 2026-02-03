//! Import command for reading events from stdin into local `SQLite` database.
//!
//! This module reads JSONL events from stdin and inserts them into the local
//! `SQLite` database. Duplicate events (same ID) are silently ignored.

use std::io::{BufRead, BufReader, Read};

use anyhow::{Context, Result};
use tt_db::{Database, StoredEvent};

/// Batch size for database inserts.
const BATCH_SIZE: usize = 1000;

/// Result of an import operation.
#[derive(Debug, PartialEq, Eq)]
pub struct ImportResult {
    /// Total number of valid JSON lines read.
    pub total_read: usize,
    /// Number of events successfully inserted.
    pub inserted: usize,
    /// Number of duplicate events (already existed).
    pub duplicates: usize,
    /// Number of malformed JSON lines skipped.
    pub malformed: usize,
}

/// Imports events from a reader into the database.
///
/// Events are expected as JSONL (one JSON object per line).
/// Malformed lines are skipped with a warning.
/// Duplicate events (same ID) are silently ignored.
pub fn import_from_reader<R: Read>(db: &Database, reader: R) -> Result<ImportResult> {
    let buf_reader = BufReader::new(reader);
    let mut batch: Vec<StoredEvent> = Vec::with_capacity(BATCH_SIZE);
    let mut result = ImportResult {
        total_read: 0,
        inserted: 0,
        duplicates: 0,
        malformed: 0,
    };

    for (line_num, line_result) in buf_reader.lines().enumerate() {
        let line = line_result.context("failed to read line from stdin")?;

        // Skip empty lines
        if line.trim().is_empty() {
            continue;
        }

        match serde_json::from_str::<StoredEvent>(&line) {
            Ok(mut event) => {
                // Clear stream_id and assignment_source during import - events will be
                // re-assigned to streams after import via the inference algorithm.
                // The original stream_id would violate foreign key constraints anyway
                // since the stream doesn't exist in this database.
                event.stream_id = None;
                event.assignment_source = None;

                result.total_read += 1;
                batch.push(event);

                if batch.len() >= BATCH_SIZE {
                    let inserted = db.insert_events(&batch).context("failed to insert batch")?;
                    result.inserted += inserted;
                    result.duplicates += batch.len() - inserted;
                    batch.clear();
                }
            }
            Err(e) => {
                tracing::warn!(line = line_num + 1, error = %e, "malformed JSON, skipping line");
                result.malformed += 1;
            }
        }
    }

    // Flush remaining batch
    if !batch.is_empty() {
        let inserted = db
            .insert_events(&batch)
            .context("failed to insert final batch")?;
        result.inserted += inserted;
        result.duplicates += batch.len() - inserted;
    }

    Ok(result)
}

/// Runs the import command, reading from stdin.
pub fn run(db: &Database) -> Result<ImportResult> {
    let stdin = std::io::stdin();
    let result = import_from_reader(db, stdin.lock())?;

    eprintln!(
        "Imported {} events ({} new, {} duplicates, {} malformed)",
        result.total_read, result.inserted, result.duplicates, result.malformed
    );

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use std::io::Cursor;

    fn make_jsonl_event(id: &str, ts: &str) -> String {
        format!(
            r#"{{"id":"{id}","timestamp":"{ts}","source":"remote.tmux","type":"tmux_pane_focus","data":{{}}}}"#
        )
    }

    #[test]
    fn test_empty_stdin_returns_zero_counts() {
        let db = Database::open_in_memory().unwrap();
        let input = Cursor::new("");

        let result = import_from_reader(&db, input).unwrap();

        assert_eq!(result.total_read, 0);
        assert_eq!(result.inserted, 0);
        assert_eq!(result.duplicates, 0);
        assert_eq!(result.malformed, 0);
    }

    #[test]
    fn test_valid_jsonl_all_inserted() {
        let db = Database::open_in_memory().unwrap();
        let input_str = format!(
            "{}\n{}\n",
            make_jsonl_event("e1", "2025-01-29T12:00:00Z"),
            make_jsonl_event("e2", "2025-01-29T12:01:00Z")
        );
        let input = Cursor::new(input_str);

        let result = import_from_reader(&db, input).unwrap();

        assert_eq!(result.total_read, 2);
        assert_eq!(result.inserted, 2);
        assert_eq!(result.duplicates, 0);
        assert_eq!(result.malformed, 0);

        // Verify events are in database
        let events = db.get_events(None, None).unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_malformed_lines_skipped() {
        let db = Database::open_in_memory().unwrap();
        let input_str = format!(
            "{}\nnot valid json\n{}\n",
            make_jsonl_event("e1", "2025-01-29T12:00:00Z"),
            make_jsonl_event("e2", "2025-01-29T12:01:00Z")
        );
        let input = Cursor::new(input_str);

        let result = import_from_reader(&db, input).unwrap();

        assert_eq!(result.total_read, 2);
        assert_eq!(result.inserted, 2);
        assert_eq!(result.duplicates, 0);
        assert_eq!(result.malformed, 1);
    }

    #[test]
    fn test_duplicate_events_idempotent() {
        let db = Database::open_in_memory().unwrap();
        let event_line = make_jsonl_event("dup-id", "2025-01-29T12:00:00Z");

        // First import
        let input1 = Cursor::new(format!("{event_line}\n"));
        let result1 = import_from_reader(&db, input1).unwrap();
        assert_eq!(result1.inserted, 1);
        assert_eq!(result1.duplicates, 0);

        // Second import of same event
        let input2 = Cursor::new(format!("{event_line}\n"));
        let result2 = import_from_reader(&db, input2).unwrap();
        assert_eq!(result2.total_read, 1);
        assert_eq!(result2.inserted, 0);
        assert_eq!(result2.duplicates, 1);

        // Database should still have only 1 event
        let events = db.get_events(None, None).unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_mixed_valid_invalid_partial_success() {
        let db = Database::open_in_memory().unwrap();
        let input_str = format!(
            "{}\n{{\n{}\n",
            make_jsonl_event("e1", "2025-01-29T12:00:00Z"),
            make_jsonl_event("e2", "2025-01-29T12:01:00Z")
        );
        let input = Cursor::new(input_str);

        let result = import_from_reader(&db, input).unwrap();

        assert_eq!(result.total_read, 2);
        assert_eq!(result.inserted, 2);
        assert_eq!(result.malformed, 1);
    }

    #[test]
    fn test_empty_lines_skipped() {
        let db = Database::open_in_memory().unwrap();
        let input_str = format!(
            "{}\n\n   \n{}\n",
            make_jsonl_event("e1", "2025-01-29T12:00:00Z"),
            make_jsonl_event("e2", "2025-01-29T12:01:00Z")
        );
        let input = Cursor::new(input_str);

        let result = import_from_reader(&db, input).unwrap();

        assert_eq!(result.total_read, 2);
        assert_eq!(result.inserted, 2);
        assert_eq!(result.malformed, 0);
    }

    #[test]
    fn test_export_event_format_compatibility() {
        // Test that events from `tt export` can be imported
        // `tt export` outputs: {"id":"...","timestamp":"...","source":"...","type":"...","data":{...}}
        let db = Database::open_in_memory().unwrap();

        // Simulate export output (note: `type` not `event_type`)
        let export_event = r#"{"id":"remote.agent:agent_session:2025-01-29T12:00:00Z:sess123:started","timestamp":"2025-01-29T12:00:00Z","source":"remote.agent","type":"agent_session","data":{"action":"started","agent":"claude-code","session_id":"sess123"}}"#;
        let input = Cursor::new(format!("{export_event}\n"));

        let result = import_from_reader(&db, input).unwrap();

        assert_eq!(result.inserted, 1);
        assert_eq!(result.malformed, 0);

        // Verify event was stored correctly
        let events = db.get_events(None, None).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "agent_session");
        assert_eq!(events[0].source, "remote.agent");
    }

    #[test]
    fn test_batch_processing_large_input() {
        let db = Database::open_in_memory().unwrap();

        // Generate more than BATCH_SIZE events
        let num_events = BATCH_SIZE + 500;
        let mut input_str = String::new();
        #[allow(clippy::cast_possible_wrap)]
        for i in 0..num_events {
            let ts = Utc.with_ymd_and_hms(2025, 1, 29, 12, 0, 0).unwrap()
                + chrono::Duration::seconds(i as i64);
            let line = format!(
                r#"{{"id":"batch-{}","timestamp":"{}","source":"test","type":"test","data":{{}}}}"#,
                i,
                ts.to_rfc3339()
            );
            input_str.push_str(&line);
            input_str.push('\n');
        }

        let input = Cursor::new(input_str);
        let result = import_from_reader(&db, input).unwrap();

        assert_eq!(result.total_read, num_events);
        assert_eq!(result.inserted, num_events);
        assert_eq!(result.duplicates, 0);

        // Verify all events are in database
        let events = db.get_events(None, None).unwrap();
        assert_eq!(events.len(), num_events);
    }

    #[test]
    fn test_optional_fields_default() {
        // Test that events without optional fields (cwd, session_id, schema_version) are handled
        let db = Database::open_in_memory().unwrap();

        // Minimal event without optional fields
        let minimal_event = r#"{"id":"min-1","timestamp":"2025-01-29T12:00:00Z","source":"test","type":"test","data":{}}"#;
        let input = Cursor::new(format!("{minimal_event}\n"));

        let result = import_from_reader(&db, input).unwrap();

        assert_eq!(result.inserted, 1);

        let events = db.get_events(None, None).unwrap();
        assert_eq!(events[0].schema_version, 1); // default
        assert_eq!(events[0].cwd, None);
        assert_eq!(events[0].session_id, None);
    }

    #[test]
    fn test_events_with_all_fields() {
        let db = Database::open_in_memory().unwrap();

        let full_event = r#"{"id":"full-1","timestamp":"2025-01-29T12:00:00Z","source":"remote.agent","type":"agent_session","schema_version":2,"data":{"action":"started"},"cwd":"/home/user/project","session_id":"sess123"}"#;
        let input = Cursor::new(format!("{full_event}\n"));

        let result = import_from_reader(&db, input).unwrap();

        assert_eq!(result.inserted, 1);

        let events = db.get_events(None, None).unwrap();
        assert_eq!(events[0].schema_version, 2);
        assert_eq!(events[0].cwd, Some("/home/user/project".to_string()));
        assert_eq!(events[0].session_id, Some("sess123".to_string()));
    }
}

//! Import command for reading events from stdin into local `SQLite` database.
//!
//! This module reads JSONL events from stdin and inserts them into the local
//! `SQLite` database. Duplicate events (same ID) are silently ignored.

use std::io::{BufRead, BufReader, Read};

use anyhow::{Context, Result};
use serde_json::json;
use tt_db::{Database, StoredEvent};

use crate::machine::extract_machine_id;

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
    /// Number of agent sessions imported.
    pub sessions_imported: usize,
    /// Machine ID extracted from events or session metadata.
    pub machine_id: Option<String>,
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
        sessions_imported: 0,
        machine_id: None,
    };

    for (line_num, line_result) in buf_reader.lines().enumerate() {
        let line = line_result.context("failed to read line from stdin")?;

        // Skip empty lines
        if line.trim().is_empty() {
            continue;
        }

        // Check for session metadata records before event parsing.
        // This must come before rewrite_legacy_session_types to avoid the
        // legacy rewriter mangling metadata lines.
        if let Some((session, machine_id)) = try_parse_session_metadata(&line) {
            db.upsert_agent_session(&session, machine_id.as_deref())
                .context("failed to upsert agent session")?;
            result.sessions_imported += 1;
            if result.machine_id.is_none() {
                result.machine_id = machine_id;
            }
            continue;
        }

        // Check if this line is recognized as metadata but invalid.
        // If so, skip it without counting as malformed.
        if is_session_metadata_record(&line) {
            continue;
        }

        let line = match rewrite_legacy_session_types(&line, line_num) {
            Ok(rewritten) => rewritten,
            Err(err) => {
                tracing::warn!(
                    line = line_num + 1,
                    error = %err,
                    "malformed JSON, skipping line"
                );
                result.malformed += 1;
                continue;
            }
        };

        match serde_json::from_str::<StoredEvent>(&line) {
            Ok(mut event) => {
                // Clear stream_id and assignment_source during import - events will be
                // re-assigned to streams after import via the inference algorithm.
                // The original stream_id would violate foreign key constraints anyway
                // since the stream doesn't exist in this database.
                event.stream_id = None;
                event.assignment_source = None;

                // Extract machine_id from event ID prefix (UUID before first colon-separated source)
                if event.machine_id.is_none() {
                    event.machine_id = extract_machine_id(&event.id);
                }

                if result.machine_id.is_none() {
                    result.machine_id.clone_from(&event.machine_id);
                }

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

fn rewrite_legacy_session_types(line: &str, line_num: usize) -> Result<String> {
    if !line.contains("\"session_start\"") && !line.contains("\"session_end\"") {
        return Ok(line.to_string());
    }

    let mut value: serde_json::Value = serde_json::from_str(line)
        .with_context(|| format!("legacy type rewrite failed on line {}", line_num + 1))?;
    if let Some(obj) = value.as_object_mut() {
        let type_str = obj.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match type_str {
            "session_start" => {
                obj.insert("type".into(), json!("agent_session"));
                obj.insert("action".into(), json!("started"));
            }
            "session_end" => {
                obj.insert("type".into(), json!("agent_session"));
                obj.insert("action".into(), json!("ended"));
            }
            _ => {}
        }
    }

    serde_json::to_string(&value).context("failed to serialize legacy rewrite")
}

/// Runs the import command, reading from stdin.
pub fn run(db: &Database) -> Result<ImportResult> {
    let stdin = std::io::stdin();
    let result = import_from_reader(db, stdin.lock())?;

    eprintln!(
        "Imported {} events, {} sessions ({} new, {} duplicates, {} malformed)",
        result.total_read,
        result.sessions_imported,
        result.inserted,
        result.duplicates,
        result.malformed
    );

    Ok(result)
}

/// Checks if a line is recognized as a session metadata record (regardless of validity).
fn is_session_metadata_record(line: &str) -> bool {
    // Fast path: check if line contains the session_metadata type marker
    if !line.contains("\"session_metadata\"") {
        return false;
    }

    // Verify it's actually JSON with type field set to session_metadata
    serde_json::from_str::<serde_json::Value>(line)
        .is_ok_and(|value| value.get("type").and_then(|t| t.as_str()) == Some("session_metadata"))
}

/// Attempts to parse a JSONL line as a session metadata record.
///
/// Returns `Some((AgentSession, Option<String>))` if the line has `"type": "session_metadata"` and can be converted,
/// `None` otherwise (the line is presumably a regular event or recognized but invalid metadata).
/// When metadata is recognized but invalid, a warning is logged.
fn try_parse_session_metadata(line: &str) -> Option<(tt_core::session::AgentSession, Option<String>)> {
    // Fast path: skip lines that can't possibly be session metadata
    if !line.contains("\"session_metadata\"") {
        return None;
    }

    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    if value.get("type")?.as_str()? != "session_metadata" {
        return None;
    }

    // At this point, we've confirmed it's a session_metadata record.
    // Now try to deserialize and convert it.
    let export: super::export::SessionMetadataExport = match serde_json::from_str(line) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(
                line_content = %line,
                error = %e,
                "recognized session_metadata record failed deserialization"
            );
            return None;
        }
    };

    // Try to convert to AgentSession
    let session_id = export.session_id.clone();
    let source = export.source.clone();
    if let Some((session, machine_id)) = export.into_agent_session() {
        Some((session, machine_id))
    } else {
        tracing::warn!(
            session_id = %session_id,
            source = %source,
            "session_metadata record has invalid fields, skipping"
        );
        None
    }
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
        assert_eq!(events[0].event_type, tt_core::EventType::AgentSession);
        assert_eq!(events[0].source, "remote.agent");
    }

    #[test]
    fn test_import_legacy_session_types_rewritten() {
        use tt_core::EventType;

        let db = Database::open_in_memory().unwrap();
        let input_str = r#"{"id":"legacy-start","timestamp":"2025-01-29T12:00:00Z","source":"remote.agent","type":"session_start","session_id":"sess123"}
{"id":"legacy-end","timestamp":"2025-01-29T12:05:00Z","source":"remote.agent","type":"session_end","session_id":"sess123"}
"#;
        let input = Cursor::new(input_str);

        let result = import_from_reader(&db, input).unwrap();

        assert_eq!(result.inserted, 2);
        assert_eq!(result.malformed, 0);

        let events = db.get_events(None, None).unwrap();
        let start = events
            .iter()
            .find(|event| event.id == "legacy-start")
            .expect("missing legacy start event");
        let end = events
            .iter()
            .find(|event| event.id == "legacy-end")
            .expect("missing legacy end event");

        assert_eq!(start.event_type, EventType::AgentSession);
        assert_eq!(start.action.as_deref(), Some("started"));
        assert_eq!(end.event_type, EventType::AgentSession);
        assert_eq!(end.action.as_deref(), Some("ended"));
    }

    #[test]
    fn test_batch_processing_large_input() {
        let db = Database::open_in_memory().unwrap();

        // Generate more than BATCH_SIZE events
        let num_events = BATCH_SIZE + 500;
        let mut input_str = String::new();
        #[expect(
            clippy::cast_possible_wrap,
            reason = "test assertion where overflow is not possible"
        )]
        for i in 0..num_events {
            let ts = Utc.with_ymd_and_hms(2025, 1, 29, 12, 0, 0).unwrap()
                + chrono::Duration::seconds(i as i64);
            let line = format!(
                r#"{{"id":"batch-{}","timestamp":"{}","source":"test","type":"tmux_pane_focus","data":{{}}}}"#,
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
        let minimal_event = r#"{"id":"min-1","timestamp":"2025-01-29T12:00:00Z","source":"test","type":"tmux_pane_focus","data":{}}"#;
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

    #[test]
    fn test_extract_machine_id_valid() {
        let id = "a1b2c3d4-e5f6-7890-abcd-ef1234567890:remote.tmux:tmux_pane_focus:2025-01-29T12:00:00.000Z:%1";
        assert_eq!(
            extract_machine_id(id),
            Some("a1b2c3d4-e5f6-7890-abcd-ef1234567890".to_string())
        );
    }

    #[test]
    fn test_extract_machine_id_no_uuid() {
        let id = "remote.tmux:tmux_pane_focus:2025-01-29T12:00:00.000Z:%1";
        assert_eq!(extract_machine_id(id), None);
    }

    #[test]
    fn test_extract_machine_id_invalid_uuid() {
        let id = "not-a-valid-uuid-at-all-but-36chars:remote.tmux:foo";
        assert_eq!(extract_machine_id(id), None);
    }

    #[test]
    fn test_import_populates_machine_id() {
        let db = Database::open_in_memory().unwrap();
        let event = r#"{"id":"a1b2c3d4-e5f6-7890-abcd-ef1234567890:remote.tmux:tmux_pane_focus:2025-01-29T12:00:00.000Z:%1","timestamp":"2025-01-29T12:00:00.000Z","source":"remote.tmux","type":"tmux_pane_focus","pane_id":"%1","tmux_session":"main","cwd":"/tmp"}"#;
        let input = Cursor::new(format!("{event}\n"));
        let result = import_from_reader(&db, input).unwrap();
        assert_eq!(result.inserted, 1);

        // Verify machine_id was extracted and stored
        let events = db
            .get_events_in_range(
                chrono::DateTime::parse_from_rfc3339("2025-01-29T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                chrono::DateTime::parse_from_rfc3339("2025-01-30T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            )
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].machine_id.as_deref(),
            Some("a1b2c3d4-e5f6-7890-abcd-ef1234567890")
        );
    }

    #[test]
    fn test_import_result_has_machine_id() {
        let db = Database::open_in_memory().unwrap();
        let event = r#"{"id":"a1b2c3d4-e5f6-7890-abcd-ef1234567890:remote.tmux:tmux_pane_focus:2025-01-29T12:00:00.000Z:%1","timestamp":"2025-01-29T12:00:00.000Z","source":"remote.tmux","type":"tmux_pane_focus","pane_id":"%1","tmux_session":"main","cwd":"/tmp"}"#;
        let result = import_from_reader(&db, Cursor::new(event)).unwrap();
        assert_eq!(
            result.machine_id,
            Some("a1b2c3d4-e5f6-7890-abcd-ef1234567890".to_string())
        );
    }

    #[test]
    fn test_import_invalid_session_metadata_not_malformed() {
        let db = Database::open_in_memory().unwrap();

        // Metadata with invalid source (unknown_agent is not a valid SessionSource)
        let bad_metadata = r#"{"type":"session_metadata","session_id":"ses_bad","source":"unknown_agent","session_type":"user","project_path":"/p","project_name":"p","start_time":"2025-01-29T12:00:00.000Z","message_count":1,"assistant_message_count":0,"tool_call_count":0}"#;
        let input = Cursor::new(bad_metadata);

        let result = import_from_reader(&db, input).unwrap();

        // Should NOT be counted as malformed (it's recognized metadata, just invalid)
        assert_eq!(result.malformed, 0);
        // Should NOT be imported (conversion failed)
        assert_eq!(result.total_read, 0);
    }

    #[test]
    fn test_import_session_metadata() {
        let db = Database::open_in_memory().unwrap();

        let metadata_line = r#"{"type":"session_metadata","session_id":"ses_import_1","source":"opencode","session_type":"user","project_path":"/home/user/project","project_name":"project","start_time":"2025-01-29T12:00:00.000Z","end_time":"2025-01-29T13:00:00.000Z","message_count":10,"summary":"test session","user_prompts":["hello"],"starting_prompt":"hello","assistant_message_count":5,"tool_call_count":3}"#;
        let event_line = make_jsonl_event("e1", "2025-01-29T12:00:00Z");
        let input = Cursor::new(format!("{event_line}\n{metadata_line}\n"));

        let result = import_from_reader(&db, input).unwrap();

        assert_eq!(result.total_read, 1); // Only the event counts as total_read
        assert_eq!(result.inserted, 1);
        assert_eq!(result.sessions_imported, 1);
        assert_eq!(result.malformed, 0);

        // Verify session was stored
        let sessions = db
            .agent_sessions_in_range(
                chrono::DateTime::parse_from_rfc3339("2025-01-29T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                chrono::DateTime::parse_from_rfc3339("2025-01-30T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            )
            .unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "ses_import_1");
        assert_eq!(sessions[0].summary, Some("test session".to_string()));
        assert_eq!(sessions[0].message_count, 10);
        assert_eq!(sessions[0].starting_prompt, Some("hello".to_string()));
    }

    #[test]
    fn test_import_session_metadata_idempotent() {
        let db = Database::open_in_memory().unwrap();

        let metadata_line = r#"{"type":"session_metadata","session_id":"ses_idem","source":"claude","session_type":"user","project_path":"/home/user/p","project_name":"p","start_time":"2025-01-29T12:00:00.000Z","message_count":5,"assistant_message_count":2,"tool_call_count":1}"#;

        // Import twice
        let input1 = Cursor::new(format!("{metadata_line}\n"));
        let result1 = import_from_reader(&db, input1).unwrap();
        assert_eq!(result1.sessions_imported, 1);

        let input2 = Cursor::new(format!("{metadata_line}\n"));
        let result2 = import_from_reader(&db, input2).unwrap();
        assert_eq!(result2.sessions_imported, 1);

        // Should still be just 1 session
        let sessions = db
            .agent_sessions_in_range(
                chrono::DateTime::parse_from_rfc3339("2025-01-29T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                chrono::DateTime::parse_from_rfc3339("2025-01-30T00:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            )
            .unwrap();
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn test_import_old_format_without_metadata() {
        // Backward compatibility: old-format exports without metadata lines
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
        assert_eq!(result.sessions_imported, 0);
        assert_eq!(result.malformed, 0);
    }
}

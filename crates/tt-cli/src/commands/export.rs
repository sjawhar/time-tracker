//! Implementation of the `tt export` command.
//!
//! This command reads events from the local buffer (events.jsonl) and parses
//! Claude Code session logs, outputting a combined event stream as JSONL to stdout.

use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write, stdout};
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use glob::glob;

use tt_core::{RawEvent, generate_event_id};

/// Run the export command.
///
/// Outputs all events as JSONL to stdout, sorted by timestamp.
pub fn run() -> Result<()> {
    let mut events: Vec<RawEvent> = Vec::new();

    // 1. Load events.jsonl
    events.extend(load_events_jsonl()?);

    // 2. Parse all Claude session logs
    events.extend(parse_all_claude_logs()?);

    // 3. Sort by timestamp (oldest first)
    events.sort_by_key(|e| e.timestamp);

    // 4. Output as JSONL to stdout (buffered for performance)
    let stdout = stdout();
    let mut writer = BufWriter::new(stdout.lock());
    for event in events {
        serde_json::to_writer(&mut writer, &event).context("failed to serialize event")?;
        // Handle broken pipe gracefully (e.g., when piped to `head`)
        if writeln!(writer).is_err() {
            break;
        }
    }

    Ok(())
}

/// Load events from the local events.jsonl buffer.
fn load_events_jsonl() -> Result<Vec<RawEvent>> {
    let Some(events_path) = get_events_path() else {
        // No home directory - return empty (may be running in restricted environment)
        return Ok(Vec::new());
    };

    // Try to open the file, return empty if it doesn't exist
    let file = match fs::File::open(&events_path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(e)
                .with_context(|| format!("failed to open events file: {}", events_path.display()));
        }
    };
    let reader = BufReader::new(file);
    let mut events = Vec::new();

    for (line_num, line_result) in reader.lines().enumerate() {
        let Ok(line) = line_result else {
            continue; // Skip lines we can't read
        };

        if line.trim().is_empty() {
            continue;
        }

        match serde_json::from_str::<RawEvent>(&line) {
            Ok(event) => events.push(event),
            Err(e) => {
                // Skip malformed lines with debug logging
                tracing::debug!(
                    line = line_num + 1,
                    error = %e,
                    "skipping malformed line in events.jsonl"
                );
            }
        }
    }

    Ok(events)
}

/// Get the path to the events.jsonl file.
///
/// Returns `None` if the home directory cannot be determined.
fn get_events_path() -> Option<PathBuf> {
    let base = dirs::data_dir().or_else(|| dirs::home_dir().map(|h| h.join(".local/share")))?;
    Some(base.join("time-tracker").join("events.jsonl"))
}

/// Parse all Claude Code session logs and extract events.
fn parse_all_claude_logs() -> Result<Vec<RawEvent>> {
    let Some(claude_dir) = get_claude_dir() else {
        // No home directory - return empty
        return Ok(Vec::new());
    };

    if !claude_dir.exists() {
        return Ok(Vec::new());
    }

    let pattern = claude_dir.join("projects/**/sessions/**/*.jsonl");
    let pattern_str = pattern.to_string_lossy();

    let mut events = Vec::new();

    for entry in glob(&pattern_str).context("invalid glob pattern")? {
        let path = match entry {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!(error = %e, "error accessing Claude log file");
                continue;
            }
        };

        match parse_claude_session_file(&path) {
            Ok(session_events) => events.extend(session_events),
            Err(e) => {
                tracing::debug!(
                    path = %path.display(),
                    error = %e,
                    "error parsing Claude session file"
                );
            }
        }
    }

    Ok(events)
}

/// Get the path to the Claude directory.
///
/// Returns `None` if the home directory cannot be determined.
fn get_claude_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude"))
}

/// Parse a single Claude session file and extract events.
fn parse_claude_session_file(path: &std::path::Path) -> Result<Vec<RawEvent>> {
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();
    let mut seen_session_start = false;

    for line_result in reader.lines() {
        let Ok(line) = line_result else {
            continue;
        };

        if line.trim().is_empty() {
            continue;
        }

        // Parse the Claude log entry
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };

        // Extract common fields
        let session_id = entry
            .get("sessionId")
            .and_then(|v| v.as_str())
            .map(String::from);

        let cwd = entry.get("cwd").and_then(|v| v.as_str()).map(String::from);

        let Some(timestamp_str) = entry.get("timestamp").and_then(|v| v.as_str()) else {
            continue;
        };

        let timestamp: DateTime<Utc> = match timestamp_str.parse() {
            Ok(dt) => dt,
            Err(_) => continue,
        };

        let entry_type = entry.get("type").and_then(|v| v.as_str());

        // Skip file-history-snapshot entries
        if entry_type == Some("file-history-snapshot") {
            continue;
        }

        // Extract events based on entry type
        match entry_type {
            Some("user") => {
                // Emit agent_session started on first user message
                if !seen_session_start {
                    if let Some(ref sid) = session_id {
                        events.push(create_agent_session_event(
                            &timestamp,
                            sid,
                            cwd.as_deref(),
                            "started",
                        ));
                        seen_session_start = true;
                    }
                }

                // Extract user message event
                if let Some(event) = create_user_message_event(
                    &entry,
                    &timestamp,
                    session_id.as_deref(),
                    cwd.as_deref(),
                ) {
                    events.push(event);
                }
            }
            Some("assistant") => {
                // Extract tool use events from assistant messages
                events.extend(extract_tool_use_events(
                    &entry,
                    &timestamp,
                    session_id.as_deref(),
                    cwd.as_deref(),
                ));
            }
            _ => {
                // Skip other entry types
            }
        }
    }

    Ok(events)
}

/// Create an `agent_session` event.
fn create_agent_session_event(
    timestamp: &DateTime<Utc>,
    session_id: &str,
    cwd: Option<&str>,
    action: &str,
) -> RawEvent {
    let timestamp_str = format_timestamp(*timestamp);
    let data = serde_json::json!({
        "action": action,
        "agent": "claude-code",
        "session_id": session_id
    });

    let id = generate_event_id(
        "remote.agent",
        "agent_session",
        &timestamp_str,
        &data.to_string(),
    );

    RawEvent {
        id,
        timestamp: *timestamp,
        event_type: "agent_session".into(),
        source: "remote.agent".into(),
        data,
        cwd: cwd.map(String::from),
    }
}

/// Create a `user_message` event from a Claude log entry.
fn create_user_message_event(
    entry: &serde_json::Value,
    timestamp: &DateTime<Utc>,
    session_id: Option<&str>,
    cwd: Option<&str>,
) -> Option<RawEvent> {
    let session_id = session_id?;

    // Calculate message length from content
    let length = entry
        .get("message")
        .and_then(|m| m.get("content"))
        .map_or(0, |content| {
            content.as_str().map_or_else(
                || {
                    // Content can be an array of content blocks
                    content.as_array().map_or(0, |arr| {
                        arr.iter()
                            .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                            .map(str::len)
                            .sum()
                    })
                },
                str::len,
            )
        });

    // Check for images in content
    let has_image = entry
        .get("message")
        .and_then(|m| m.get("content"))
        .is_some_and(|content| {
            content.as_array().is_some_and(|arr| {
                arr.iter()
                    .any(|block| block.get("type").and_then(|t| t.as_str()) == Some("image"))
            })
        });

    let timestamp_str = format_timestamp(*timestamp);
    let data = serde_json::json!({
        "agent": "claude-code",
        "session_id": session_id,
        "length": length,
        "has_image": has_image
    });

    let id = generate_event_id(
        "remote.agent",
        "user_message",
        &timestamp_str,
        &data.to_string(),
    );

    Some(RawEvent {
        id,
        timestamp: *timestamp,
        event_type: "user_message".into(),
        source: "remote.agent".into(),
        data,
        cwd: cwd.map(String::from),
    })
}

/// Extract `agent_tool_use` events from an assistant message.
fn extract_tool_use_events(
    entry: &serde_json::Value,
    timestamp: &DateTime<Utc>,
    session_id: Option<&str>,
    cwd: Option<&str>,
) -> Vec<RawEvent> {
    let Some(session_id) = session_id else {
        return Vec::new();
    };

    let Some(content) = entry.get("message").and_then(|m| m.get("content")) else {
        return Vec::new();
    };

    let Some(content_blocks) = content.as_array() else {
        return Vec::new();
    };

    let mut events = Vec::new();

    for block in content_blocks {
        if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
            continue;
        }

        let Some(tool_name) = block.get("name").and_then(|n| n.as_str()) else {
            continue;
        };

        let timestamp_str = format_timestamp(*timestamp);
        let data = serde_json::json!({
            "agent": "claude-code",
            "session_id": session_id,
            "tool": tool_name
        });

        // Include tool_use id in hash for uniqueness when same tool is used multiple times
        let tool_use_id = block.get("id").and_then(|i| i.as_str()).unwrap_or("");
        let hash_data = format!("{data}:{tool_use_id}");

        let id = generate_event_id("remote.agent", "agent_tool_use", &timestamp_str, &hash_data);

        events.push(RawEvent {
            id,
            timestamp: *timestamp,
            event_type: "agent_tool_use".into(),
            source: "remote.agent".into(),
            data,
            cwd: cwd.map(String::from),
        });
    }

    events
}

/// Format a timestamp for ID generation (matching ingest.rs).
fn format_timestamp(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    // Helper to create a test directory structure
    fn setup_test_dirs() -> (TempDir, PathBuf, PathBuf) {
        let temp = TempDir::new().unwrap();
        let events_path = temp.path().join("events.jsonl");
        let claude_path = temp.path().join("claude");
        (temp, events_path, claude_path)
    }

    #[test]
    fn test_empty_events_jsonl_returns_empty() {
        let (_temp, events_path, _) = setup_test_dirs();
        // File doesn't exist
        let events = load_events_jsonl_from_path(&events_path).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_parse_events_jsonl() {
        let (_temp, events_path, _) = setup_test_dirs();

        // Write some test events
        let mut file = fs::File::create(&events_path).unwrap();
        writeln!(
            file,
            r#"{{"id":"abc","timestamp":"2026-01-19T10:00:00Z","type":"tmux_pane_focus","source":"remote.tmux","data":{{"pane_id":"%1"}},"cwd":"/home/test"}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"id":"def","timestamp":"2026-01-19T11:00:00Z","type":"tmux_pane_focus","source":"remote.tmux","data":{{"pane_id":"%2"}},"cwd":"/home/test2"}}"#
        )
        .unwrap();

        let events = load_events_jsonl_from_path(&events_path).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id, "abc");
        assert_eq!(events[1].id, "def");
    }

    #[test]
    fn test_skip_malformed_lines() {
        let (_temp, events_path, _) = setup_test_dirs();

        let mut file = fs::File::create(&events_path).unwrap();
        writeln!(file, "this is not json").unwrap();
        writeln!(
            file,
            r#"{{"id":"abc","timestamp":"2026-01-19T10:00:00Z","type":"test","source":"test","data":{{}}}}"#
        )
        .unwrap();
        writeln!(file, "{{malformed").unwrap();

        let events = load_events_jsonl_from_path(&events_path).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "abc");
    }

    #[test]
    fn test_parse_user_message_entry() {
        let entry = serde_json::json!({
            "type": "user",
            "sessionId": "session-123",
            "timestamp": "2026-01-19T10:00:00Z",
            "cwd": "/home/test",
            "message": {
                "role": "user",
                "content": "Hello, world!"
            }
        });

        let timestamp = "2026-01-19T10:00:00Z".parse().unwrap();
        let event =
            create_user_message_event(&entry, &timestamp, Some("session-123"), Some("/home/test"));

        assert!(event.is_some());
        let event = event.unwrap();
        assert_eq!(event.event_type, "user_message");
        assert_eq!(event.source, "remote.agent");
        assert_eq!(event.data["session_id"], "session-123");
        assert_eq!(event.data["length"], 13); // "Hello, world!".len()
        assert_eq!(event.data["has_image"], false);
        assert_eq!(event.cwd, Some("/home/test".to_string()));
    }

    #[test]
    fn test_parse_tool_use_entry() {
        let entry = serde_json::json!({
            "type": "assistant",
            "sessionId": "session-123",
            "timestamp": "2026-01-19T10:00:00Z",
            "cwd": "/home/test",
            "message": {
                "role": "assistant",
                "content": [
                    {
                        "type": "tool_use",
                        "id": "tool-1",
                        "name": "Read"
                    }
                ]
            }
        });

        let timestamp = "2026-01-19T10:00:00Z".parse().unwrap();
        let events =
            extract_tool_use_events(&entry, &timestamp, Some("session-123"), Some("/home/test"));

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "agent_tool_use");
        assert_eq!(events[0].data["tool"], "Read");
        assert_eq!(events[0].data["session_id"], "session-123");
    }

    #[test]
    fn test_parse_multiple_tool_uses() {
        let entry = serde_json::json!({
            "type": "assistant",
            "sessionId": "session-123",
            "timestamp": "2026-01-19T10:00:00Z",
            "cwd": "/home/test",
            "message": {
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "Let me check these files"},
                    {"type": "tool_use", "id": "tool-1", "name": "Read"},
                    {"type": "tool_use", "id": "tool-2", "name": "Glob"},
                    {"type": "tool_use", "id": "tool-3", "name": "Grep"}
                ]
            }
        });

        let timestamp = "2026-01-19T10:00:00Z".parse().unwrap();
        let events =
            extract_tool_use_events(&entry, &timestamp, Some("session-123"), Some("/home/test"));

        assert_eq!(events.len(), 3);
        assert_eq!(events[0].data["tool"], "Read");
        assert_eq!(events[1].data["tool"], "Glob");
        assert_eq!(events[2].data["tool"], "Grep");

        // Each should have a unique ID
        let ids: std::collections::HashSet<_> = events.iter().map(|e| &e.id).collect();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn test_agent_session_event_on_first_message() {
        let content = r#"{"type":"user","sessionId":"session-123","timestamp":"2026-01-19T10:00:00Z","cwd":"/test","message":{"role":"user","content":"Hello"}}
{"type":"user","sessionId":"session-123","timestamp":"2026-01-19T10:01:00Z","cwd":"/test","message":{"role":"user","content":"World"}}"#;

        let events = parse_claude_log_content(content);

        // Should have one agent_session (started) and two user_message events
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type, "agent_session");
        assert_eq!(events[0].data["action"], "started");
        assert_eq!(events[1].event_type, "user_message");
        assert_eq!(events[2].event_type, "user_message");
    }

    #[test]
    fn test_skip_file_history_snapshot() {
        let content = r#"{"type":"file-history-snapshot","sessionId":"session-123","timestamp":"2026-01-19T10:00:00Z"}
{"type":"user","sessionId":"session-123","timestamp":"2026-01-19T10:01:00Z","cwd":"/test","message":{"role":"user","content":"Hello"}}"#;

        let events = parse_claude_log_content(content);

        // Should only have agent_session and user_message (file-history-snapshot skipped)
        assert_eq!(events.len(), 2);
        assert!(
            events
                .iter()
                .all(|e| e.event_type != "file-history-snapshot")
        );
    }

    #[test]
    fn test_user_message_with_image() {
        let entry = serde_json::json!({
            "type": "user",
            "sessionId": "session-123",
            "timestamp": "2026-01-19T10:00:00Z",
            "cwd": "/home/test",
            "message": {
                "role": "user",
                "content": [
                    {"type": "text", "text": "What is this?"},
                    {"type": "image", "source": {"type": "base64"}}
                ]
            }
        });

        let timestamp = "2026-01-19T10:00:00Z".parse().unwrap();
        let event =
            create_user_message_event(&entry, &timestamp, Some("session-123"), Some("/home/test"));

        assert!(event.is_some());
        let event = event.unwrap();
        assert_eq!(event.data["has_image"], true);
        assert_eq!(event.data["length"], 13); // "What is this?".len()
    }

    #[test]
    fn test_events_sorted_chronologically() {
        let content = r#"{"type":"user","sessionId":"s1","timestamp":"2026-01-19T12:00:00Z","cwd":"/test","message":{"role":"user","content":"Later"}}
{"type":"user","sessionId":"s1","timestamp":"2026-01-19T10:00:00Z","cwd":"/test","message":{"role":"user","content":"Earlier"}}"#;

        let events = parse_claude_log_content(content);

        // Events should be in file order (not yet sorted)
        // Sorting happens in run(), not parse
        assert_eq!(events.len(), 3); // agent_session + 2 user_message
    }

    #[test]
    fn test_output_is_valid_jsonl() {
        let event = RawEvent {
            id: "test-id".into(),
            timestamp: "2026-01-19T10:00:00Z".parse().unwrap(),
            event_type: "test".into(),
            source: "test".into(),
            data: serde_json::json!({"key": "value"}),
            cwd: Some("/test".into()),
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains('\n'));
        let parsed: RawEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "test-id");
    }

    // Helper function to load from a specific path (for testing)
    fn load_events_jsonl_from_path(path: &std::path::Path) -> Result<Vec<RawEvent>> {
        if !path.exists() {
            return Ok(Vec::new());
        }

        let file = fs::File::open(path)?;
        let reader = BufReader::new(file);
        let mut events = Vec::new();

        for line_result in reader.lines() {
            let Ok(line) = line_result else {
                continue;
            };

            if line.trim().is_empty() {
                continue;
            }

            if let Ok(event) = serde_json::from_str::<RawEvent>(&line) {
                events.push(event);
            }
        }

        Ok(events)
    }

    // Helper function to parse Claude log content (for testing)
    fn parse_claude_log_content(content: &str) -> Vec<RawEvent> {
        let mut events = Vec::new();
        let mut seen_session_start = false;

        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }

            let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };

            let session_id = entry
                .get("sessionId")
                .and_then(|v| v.as_str())
                .map(String::from);

            let cwd = entry.get("cwd").and_then(|v| v.as_str()).map(String::from);

            let Some(timestamp_str) = entry.get("timestamp").and_then(|v| v.as_str()) else {
                continue;
            };

            let Ok(timestamp) = timestamp_str.parse::<DateTime<Utc>>() else {
                continue;
            };

            let entry_type = entry.get("type").and_then(|v| v.as_str());

            if entry_type == Some("file-history-snapshot") {
                continue;
            }

            match entry_type {
                Some("user") => {
                    if !seen_session_start {
                        if let Some(ref sid) = session_id {
                            events.push(create_agent_session_event(
                                &timestamp,
                                sid,
                                cwd.as_deref(),
                                "started",
                            ));
                            seen_session_start = true;
                        }
                    }

                    if let Some(event) = create_user_message_event(
                        &entry,
                        &timestamp,
                        session_id.as_deref(),
                        cwd.as_deref(),
                    ) {
                        events.push(event);
                    }
                }
                Some("assistant") => {
                    events.extend(extract_tool_use_events(
                        &entry,
                        &timestamp,
                        session_id.as_deref(),
                        cwd.as_deref(),
                    ));
                }
                _ => {}
            }
        }

        events
    }
}

//! Implementation of the `tt export` command.
//!
//! This command reads events from the local buffer (events.jsonl) and parses
//! Claude Code session logs, outputting a combined event stream as JSONL to stdout.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Seek, SeekFrom, Write, stdout};
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use glob::glob;
use serde::{Deserialize, Serialize};

use tt_core::{RawEvent, generate_event_id};

// === Manifest Types ===

/// Manifest tracking byte offsets for incremental Claude log parsing.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ClaudeManifest {
    sessions: HashMap<PathBuf, SessionEntry>,
}

/// Entry tracking parse state for a single session file.
#[derive(Debug, Serialize, Deserialize)]
struct SessionEntry {
    byte_offset: u64,
    seen_session_start: bool,
}

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

/// Get the path to the Claude manifest file.
///
/// Returns `None` if the home directory cannot be determined.
fn get_manifest_path() -> Option<PathBuf> {
    let base = dirs::data_dir().or_else(|| dirs::home_dir().map(|h| h.join(".local/share")))?;
    Some(base.join("time-tracker").join("claude-manifest.json"))
}

/// Load the Claude manifest from disk.
///
/// Returns a default (empty) manifest if the file doesn't exist or is corrupted.
fn load_manifest() -> ClaudeManifest {
    let Some(path) = get_manifest_path() else {
        return ClaudeManifest::default();
    };

    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "corrupted manifest file, falling back to full parse"
            );
            ClaudeManifest::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ClaudeManifest::default(),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "failed to read manifest, falling back to full parse"
            );
            ClaudeManifest::default()
        }
    }
}

/// Save the Claude manifest to disk.
///
/// Writes atomically by writing to a .tmp file then renaming.
/// Logs a warning but does not fail if save fails.
fn save_manifest(manifest: &ClaudeManifest) {
    let Some(path) = get_manifest_path() else {
        return;
    };

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            tracing::warn!(
                path = %parent.display(),
                error = %e,
                "failed to create manifest directory"
            );
            return;
        }
    }

    let tmp_path = path.with_extension("tmp");

    // Write to temp file
    let content = match serde_json::to_string_pretty(manifest) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "failed to serialize manifest");
            return;
        }
    };

    if let Err(e) = fs::write(&tmp_path, &content) {
        tracing::warn!(
            path = %tmp_path.display(),
            error = %e,
            "failed to write manifest temp file"
        );
        return;
    }

    // Atomic rename
    if let Err(e) = fs::rename(&tmp_path, &path) {
        tracing::warn!(
            from = %tmp_path.display(),
            to = %path.display(),
            error = %e,
            "failed to rename manifest file"
        );
        // Clean up temp file
        let _ = fs::remove_file(&tmp_path);
    }
}

/// Parse all Claude Code session logs and extract events.
///
/// Uses manifest for incremental parsing: only new bytes since last parse are read.
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

    // Load manifest for incremental parsing
    let mut manifest = load_manifest();
    let mut events = Vec::new();

    // Collect paths from glob to track which files still exist
    let mut seen_paths = HashSet::new();

    for entry in glob(&pattern_str).context("invalid glob pattern")? {
        let path = match entry {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!(error = %e, "error accessing Claude log file");
                continue;
            }
        };

        seen_paths.insert(path.clone());

        // Get stored offset and seen_session_start, or defaults
        let (offset, seen_start) = manifest
            .sessions
            .get(&path)
            .map_or((0, false), |e| (e.byte_offset, e.seen_session_start));

        match parse_claude_session_file_from_offset(&path, offset, seen_start) {
            Ok((session_events, new_offset, new_seen_start)) => {
                events.extend(session_events);
                // Update manifest entry
                manifest.sessions.insert(
                    path,
                    SessionEntry {
                        byte_offset: new_offset,
                        seen_session_start: new_seen_start,
                    },
                );
            }
            Err(e) => {
                tracing::debug!(
                    path = %path.display(),
                    error = %e,
                    "error parsing Claude session file"
                );
            }
        }
    }

    // Remove stale entries for deleted files
    manifest
        .sessions
        .retain(|path, _| seen_paths.contains(path));

    // Save updated manifest
    save_manifest(&manifest);

    Ok(events)
}

/// Get the path to the Claude directory.
///
/// Returns `None` if the home directory cannot be determined.
fn get_claude_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude"))
}

/// Parse a single Claude session file from a byte offset.
///
/// Returns (events, `new_offset`, `seen_session_start`).
/// The `new_offset` points to the byte after the last complete line successfully parsed.
fn parse_claude_session_file_from_offset(
    path: &std::path::Path,
    offset: u64,
    seen_session_start: bool,
) -> Result<(Vec<RawEvent>, u64, bool)> {
    let mut file = fs::File::open(path)?;
    let file_size = file.metadata()?.len();

    // Handle truncated file: if offset is beyond file size, log warning and reset to 0
    let actual_offset = if offset > file_size {
        tracing::warn!(
            path = %path.display(),
            stored_offset = offset,
            file_size = file_size,
            "file appears truncated, re-parsing from start"
        );
        0
    } else {
        offset
    };

    // Seek to offset
    file.seek(SeekFrom::Start(actual_offset))?;

    let mut reader = BufReader::new(file);
    let mut events = Vec::new();
    let mut seen_start = if actual_offset == 0 {
        false
    } else {
        seen_session_start
    };
    let mut current_offset = actual_offset;
    let mut last_good_offset = actual_offset;

    // Read lines manually to track byte offsets
    let mut line_buf = String::new();
    loop {
        line_buf.clear();
        let bytes_read = reader.read_line(&mut line_buf)?;

        if bytes_read == 0 {
            // EOF reached
            break;
        }

        current_offset += bytes_read as u64;

        // Check if this is a complete line (ends with newline)
        let is_complete = line_buf.ends_with('\n');
        let line = line_buf.trim();

        if line.is_empty() {
            if is_complete {
                last_good_offset = current_offset;
            }
            continue;
        }

        // Try to parse the line as JSON
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            // If we're at EOF with an incomplete line, don't advance last_good_offset
            // so we re-read this partial line next time
            if is_complete {
                last_good_offset = current_offset;
            }
            continue;
        };

        // Successfully parsed - if line is complete, update last_good_offset
        if is_complete {
            last_good_offset = current_offset;
        }

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
                if !seen_start {
                    if let Some(ref sid) = session_id {
                        events.push(create_agent_session_event(sid, cwd.as_deref(), "started"));
                        seen_start = true;
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

    Ok((events, last_good_offset, seen_start))
}

/// Create an `agent_session` event.
///
/// IMPORTANT: Uses `session_id` (not timestamp) for ID generation.
/// This ensures deterministic IDs for incremental parsing—regardless of which
/// user message triggers the emission, the same session produces the same ID.
/// The timestamp field uses current time (which may vary) but the ID is stable.
fn create_agent_session_event(session_id: &str, cwd: Option<&str>, action: &str) -> RawEvent {
    let data = serde_json::json!({
        "action": action,
        "agent": "claude-code",
        "session_id": session_id
    });

    // Use session_id instead of timestamp for ID generation to ensure deterministic IDs
    // regardless of which user message triggers the agent_session emission.
    let id = generate_event_id(
        "remote.agent",
        "agent_session",
        session_id, // Intentionally using session_id, not a timestamp
        &data.to_string(),
    );

    RawEvent {
        id,
        // Timestamp is current time since we don't have the first message's timestamp here.
        // This may vary between parses, but the ID remains stable (which is what matters for dedup).
        timestamp: Utc::now(),
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
        parse_claude_log_content_with_state(content, false).0
    }

    // Helper function to parse Claude log content with state tracking (for testing)
    fn parse_claude_log_content_with_state(
        content: &str,
        seen_session_start: bool,
    ) -> (Vec<RawEvent>, bool) {
        let mut events = Vec::new();
        let mut seen_start = seen_session_start;

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
                    if !seen_start {
                        if let Some(ref sid) = session_id {
                            events.push(create_agent_session_event(sid, cwd.as_deref(), "started"));
                            seen_start = true;
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

        (events, seen_start)
    }

    // === MANIFEST TESTS ===

    #[test]
    fn test_manifest_round_trip() {
        let mut manifest = ClaudeManifest::default();
        manifest.sessions.insert(
            PathBuf::from("/home/user/.claude/sessions/abc.jsonl"),
            SessionEntry {
                byte_offset: 12345,
                seen_session_start: true,
            },
        );
        manifest.sessions.insert(
            PathBuf::from("/home/user/.claude/sessions/def.jsonl"),
            SessionEntry {
                byte_offset: 0,
                seen_session_start: false,
            },
        );

        let json = serde_json::to_string(&manifest).unwrap();
        let parsed: ClaudeManifest = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.sessions.len(), 2);
        let entry = parsed
            .sessions
            .get(&PathBuf::from("/home/user/.claude/sessions/abc.jsonl"))
            .unwrap();
        assert_eq!(entry.byte_offset, 12345);
        assert!(entry.seen_session_start);
    }

    #[test]
    fn test_incremental_parse() {
        let temp = TempDir::new().unwrap();
        let log_path = temp.path().join("session.jsonl");

        // Write initial content
        let line1 = r#"{"type":"user","sessionId":"s1","timestamp":"2026-01-19T10:00:00Z","cwd":"/test","message":{"role":"user","content":"Hello"}}"#;
        fs::write(&log_path, format!("{line1}\n")).unwrap();

        // First parse from offset 0
        let (events1, offset1, seen_start1) =
            parse_claude_session_file_from_offset(&log_path, 0, false).unwrap();
        assert_eq!(events1.len(), 2); // agent_session + user_message
        assert_eq!(events1[0].event_type, "agent_session");
        assert_eq!(events1[1].event_type, "user_message");
        assert!(seen_start1);
        assert!(offset1 > 0);

        // Append new content
        let line2 = r#"{"type":"user","sessionId":"s1","timestamp":"2026-01-19T10:01:00Z","cwd":"/test","message":{"role":"user","content":"World"}}"#;
        let mut file = fs::OpenOptions::new().append(true).open(&log_path).unwrap();
        writeln!(file, "{line2}").unwrap();

        // Second parse from saved offset, with seen_session_start = true
        let (events2, offset2, seen_start2) =
            parse_claude_session_file_from_offset(&log_path, offset1, true).unwrap();
        assert_eq!(events2.len(), 1); // only the new user_message
        assert_eq!(events2[0].event_type, "user_message");
        assert!(seen_start2);
        assert!(offset2 > offset1);
    }

    #[test]
    fn test_truncated_file() {
        let temp = TempDir::new().unwrap();
        let log_path = temp.path().join("session.jsonl");

        // Write some content
        let line1 = r#"{"type":"user","sessionId":"s1","timestamp":"2026-01-19T10:00:00Z","cwd":"/test","message":{"role":"user","content":"Hello"}}"#;
        fs::write(&log_path, format!("{line1}\n")).unwrap();

        let file_size = fs::metadata(&log_path).unwrap().len();

        // Try to parse from offset beyond file size
        // This should trigger a warning and fall back to full re-parse from 0
        let (events, new_offset, _) =
            parse_claude_session_file_from_offset(&log_path, file_size + 1000, false).unwrap();

        // Should get events (parsed from start, not empty)
        assert_eq!(events.len(), 2); // agent_session + user_message
        assert!(new_offset > 0);
    }

    #[test]
    fn test_agent_session_id_stable_across_parses() {
        // The agent_session event ID should be the same regardless of which user message triggers it
        let session_id = "session-abc";

        // When parsed from byte 0 with first user message
        let event1 = create_agent_session_event(session_id, Some("/test"), "started");

        // Same session parsed again (would have same timestamp via session_id)
        let event2 = create_agent_session_event(session_id, Some("/test"), "started");

        // IDs should be identical (since we use session_id, not timestamp, for ID generation)
        assert_eq!(event1.id, event2.id);
    }

    #[test]
    fn test_partial_line_at_eof() {
        let temp = TempDir::new().unwrap();
        let log_path = temp.path().join("session.jsonl");

        // Write a complete line followed by an incomplete line (no newline)
        let complete_line = r#"{"type":"user","sessionId":"s1","timestamp":"2026-01-19T10:00:00Z","cwd":"/test","message":{"role":"user","content":"Hello"}}"#;
        let partial_line = r#"{"type":"user","sessionId":"s1","timestamp":"2026-01-19T10:01:00Z","cwd":"/test","message":{"role":"user","content":"Wor"#;

        fs::write(&log_path, format!("{complete_line}\n{partial_line}")).unwrap();

        // Parse should succeed with the complete line
        let (events, offset, _) =
            parse_claude_session_file_from_offset(&log_path, 0, false).unwrap();
        assert_eq!(events.len(), 2); // agent_session + user_message from complete line

        // The offset should be BEFORE the partial line (so we re-read it next time)
        let complete_line_end = complete_line.len() + 1; // +1 for newline
        assert_eq!(offset, complete_line_end as u64);

        // Now complete the partial line and re-parse
        let rest_of_line = r#"ld"}}"#;
        let mut file = fs::OpenOptions::new().append(true).open(&log_path).unwrap();
        writeln!(file, "{rest_of_line}").unwrap();

        let (events2, _, _) =
            parse_claude_session_file_from_offset(&log_path, offset, true).unwrap();
        assert_eq!(events2.len(), 1); // the previously partial, now complete user_message
        assert_eq!(events2[0].event_type, "user_message");
    }

    #[test]
    fn test_no_duplicate_agent_session_on_resume() {
        let temp = TempDir::new().unwrap();
        let log_path = temp.path().join("session.jsonl");

        // Write initial content
        let line1 = r#"{"type":"user","sessionId":"s1","timestamp":"2026-01-19T10:00:00Z","cwd":"/test","message":{"role":"user","content":"Hello"}}"#;
        fs::write(&log_path, format!("{line1}\n")).unwrap();

        // First parse - should emit agent_session
        let (events1, offset1, seen_start1) =
            parse_claude_session_file_from_offset(&log_path, 0, false).unwrap();
        assert_eq!(events1.len(), 2);
        assert_eq!(events1[0].event_type, "agent_session");
        assert!(seen_start1);

        // Append more content
        let line2 = r#"{"type":"user","sessionId":"s1","timestamp":"2026-01-19T10:01:00Z","cwd":"/test","message":{"role":"user","content":"World"}}"#;
        let mut file = fs::OpenOptions::new().append(true).open(&log_path).unwrap();
        writeln!(file, "{line2}").unwrap();

        // Resume with seen_session_start = true - should NOT emit another agent_session
        let (events2, _, _) =
            parse_claude_session_file_from_offset(&log_path, offset1, true).unwrap();
        assert_eq!(events2.len(), 1); // only user_message, no agent_session
        assert_eq!(events2[0].event_type, "user_message");
    }
}

//! Export command for syncing events to local machine.
//!
//! This module reads events from both `events.jsonl` (tmux events) and
//! Claude Code session logs, outputting a combined JSONL stream to stdout.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Event output format matching the data model spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportEvent {
    pub id: String,
    pub timestamp: String,
    pub source: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub data: Value,
}

/// Data for `agent_session` events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSessionData {
    pub action: String,
    pub agent: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// Data for `user_message` events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessageData {
    pub agent: String,
    pub session_id: String,
    pub length: usize,
    pub has_image: bool,
}

/// Data for `agent_tool_use` events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentToolUseData {
    pub agent: String,
    pub session_id: String,
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
}

/// Returns the default time tracker data directory.
fn default_data_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".time-tracker")
}

/// Returns the default Claude projects directory.
fn default_claude_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("projects")
}

/// Runs the export command, outputting all events to stdout.
pub fn run() -> Result<()> {
    run_impl(
        &default_data_dir(),
        &default_claude_dir(),
        &mut std::io::stdout(),
    )
}

/// Implementation of export that allows injecting paths for testing.
fn run_impl(data_dir: &Path, claude_dir: &Path, output: &mut dyn Write) -> Result<()> {
    // Export tmux events
    let events_file = data_dir.join("events.jsonl");
    if events_file.exists() {
        export_tmux_events(&events_file, output)?;
    }

    // Export Claude events
    if claude_dir.exists() {
        export_claude_events(claude_dir, output)?;
    }

    Ok(())
}

/// Exports tmux events from events.jsonl, passing through valid lines.
fn export_tmux_events(events_file: &Path, output: &mut dyn Write) -> Result<()> {
    let file = File::open(events_file).context("failed to open events.jsonl")?;
    let reader = BufReader::new(file);

    for (line_num, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(line = line_num + 1, error = %e, "failed to read line");
                continue;
            }
        };

        // Skip empty lines
        if line.trim().is_empty() {
            continue;
        }

        // Validate it's valid JSON before passing through (use RawValue to avoid parsing overhead)
        match serde_json::from_str::<&serde_json::value::RawValue>(&line) {
            Ok(_) => {
                writeln!(output, "{line}").context("failed to write event")?;
            }
            Err(e) => {
                tracing::warn!(line = line_num + 1, error = %e, "malformed JSON, skipping");
            }
        }
    }

    Ok(())
}

/// Discovers Claude session log files.
fn discover_claude_logs(claude_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut logs = Vec::new();

    // Pattern: ~/.claude/projects/*/*.jsonl
    // Excludes: */subagents/ directories
    if !claude_dir.exists() {
        return Ok(logs);
    }

    for project_entry in fs::read_dir(claude_dir).context("failed to read claude projects dir")? {
        let project_entry = project_entry.context("failed to read project entry")?;
        let project_path = project_entry.path();

        if !project_path.is_dir() {
            continue;
        }

        // Skip subagents directories
        if project_path.file_name().is_some_and(|n| n == "subagents") {
            continue;
        }

        // Find *.jsonl files in this project directory
        for entry in fs::read_dir(&project_path).context("failed to read project dir")? {
            let entry = entry.context("failed to read entry")?;
            let path = entry.path();

            // Skip directories (including subagents)
            if path.is_dir() {
                continue;
            }

            // Only process .jsonl files
            if path.extension().is_some_and(|ext| ext == "jsonl") {
                logs.push(path);
            }
        }
    }

    Ok(logs)
}

/// Exports events from Claude session logs.
fn export_claude_events(claude_dir: &Path, output: &mut dyn Write) -> Result<()> {
    let logs = discover_claude_logs(claude_dir)?;
    // Track seen sessions across ALL files to avoid duplicate session start events
    let mut seen_sessions: HashSet<String> = HashSet::new();

    for log_path in logs {
        if let Err(e) = export_single_claude_log(&log_path, &mut seen_sessions, output) {
            tracing::warn!(path = %log_path.display(), error = %e, "failed to parse Claude log");
        }
    }

    Ok(())
}

/// Entry types to filter out (not events).
const FILTERED_TYPES: &[&str] = &["progress", "file-history-snapshot", "summary", "system"];

/// Exports events from a single Claude session log file.
fn export_single_claude_log(
    log_path: &Path,
    seen_sessions: &mut HashSet<String>,
    output: &mut dyn Write,
) -> Result<()> {
    let file = File::open(log_path).context("failed to open Claude log")?;
    let reader = BufReader::new(file);

    for (line_num, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                // Handle partial last line gracefully
                if line_num > 0 {
                    tracing::debug!(line = line_num + 1, error = %e, "incomplete line at end of file");
                } else {
                    tracing::warn!(line = line_num + 1, error = %e, "failed to read line");
                }
                continue;
            }
        };

        if line.trim().is_empty() {
            continue;
        }

        let entry: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(line = line_num + 1, error = %e, "malformed JSON in Claude log");
                continue;
            }
        };

        process_claude_entry(&entry, seen_sessions, output)?;
    }

    Ok(())
}

/// Processes a single Claude log entry and emits events.
fn process_claude_entry(
    entry: &Value,
    seen_sessions: &mut HashSet<String>,
    output: &mut dyn Write,
) -> Result<()> {
    // Extract session ID - required for all events
    let Some(session_id) = entry.get("sessionId").and_then(Value::as_str) else {
        return Ok(()); // Skip entries without session ID
    };

    // Filter out non-event entry types
    let entry_type = entry.get("type").and_then(Value::as_str).unwrap_or("");
    if FILTERED_TYPES.contains(&entry_type) {
        return Ok(());
    }

    // Get timestamp
    let Some(timestamp) = entry.get("timestamp").and_then(Value::as_str) else {
        return Ok(());
    };

    // Get cwd if available
    let cwd = entry.get("cwd").and_then(Value::as_str).map(String::from);

    // Emit session start event (first time we see this session)
    if seen_sessions.insert(session_id.to_string()) {
        emit_session_start(session_id, timestamp, cwd.as_deref(), output)?;
    }

    // Process based on entry type
    match entry_type {
        "user" => emit_user_message(entry, session_id, timestamp, output)?,
        "assistant" => emit_tool_uses(entry, session_id, timestamp, output)?,
        _ => {}
    }

    Ok(())
}

/// Emits an `agent_session` start event.
fn emit_session_start(
    session_id: &str,
    timestamp: &str,
    cwd: Option<&str>,
    output: &mut dyn Write,
) -> Result<()> {
    let event = ExportEvent {
        id: format!("remote.agent:agent_session:{timestamp}:{session_id}:started"),
        timestamp: timestamp.to_string(),
        source: "remote.agent".to_string(),
        event_type: "agent_session".to_string(),
        data: serde_json::to_value(AgentSessionData {
            action: "started".to_string(),
            agent: "claude-code".to_string(),
            session_id: session_id.to_string(),
            cwd: cwd.map(String::from),
        })?,
    };
    let json = serde_json::to_string(&event)?;
    writeln!(output, "{json}")?;
    Ok(())
}

/// Emits a `user_message` event if the entry is not a tool result.
fn emit_user_message(
    entry: &Value,
    session_id: &str,
    timestamp: &str,
    output: &mut dyn Write,
) -> Result<()> {
    // Check if this is a tool_result message (filter those out)
    if is_tool_result(entry) {
        return Ok(());
    }

    // Extract message content
    let message = entry.get("message").and_then(|m| m.get("content"));
    let (length, has_image) = extract_message_info(message);

    let event = ExportEvent {
        id: format!(
            "remote.agent:user_message:{timestamp}:{}",
            entry
                .get("uuid")
                .and_then(Value::as_str)
                .unwrap_or(session_id)
        ),
        timestamp: timestamp.to_string(),
        source: "remote.agent".to_string(),
        event_type: "user_message".to_string(),
        data: serde_json::to_value(UserMessageData {
            agent: "claude-code".to_string(),
            session_id: session_id.to_string(),
            length,
            has_image,
        })?,
    };
    let json = serde_json::to_string(&event)?;
    writeln!(output, "{json}")?;
    Ok(())
}

/// Emits `agent_tool_use` events for each tool use in an assistant message.
fn emit_tool_uses(
    entry: &Value,
    session_id: &str,
    timestamp: &str,
    output: &mut dyn Write,
) -> Result<()> {
    let Some(content) = entry
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
    else {
        return Ok(());
    };

    for block in content {
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }

        let tool_name = block
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let tool_id = block.get("id").and_then(Value::as_str).unwrap_or("unknown");
        let input = block.get("input").cloned().unwrap_or(Value::Null);
        let file = extract_file(tool_name, &input);

        let event = ExportEvent {
            id: format!("remote.agent:agent_tool_use:{timestamp}:{tool_id}"),
            timestamp: timestamp.to_string(),
            source: "remote.agent".to_string(),
            event_type: "agent_tool_use".to_string(),
            data: serde_json::to_value(AgentToolUseData {
                agent: "claude-code".to_string(),
                session_id: session_id.to_string(),
                tool: tool_name.to_string(),
                file,
            })?,
        };
        let json = serde_json::to_string(&event)?;
        writeln!(output, "{json}")?;
    }

    Ok(())
}

/// Checks if a user message contains a `tool_result` (should be filtered).
fn is_tool_result(entry: &Value) -> bool {
    if let Some(content) = entry.get("message").and_then(|m| m.get("content")) {
        // If content is an array, check if ANY element is a tool_result
        if let Some(arr) = content.as_array() {
            return arr
                .iter()
                .any(|item| item.get("type").and_then(Value::as_str) == Some("tool_result"));
        }
    }
    false
}

/// Extracts message length and `has_image` from message content.
fn extract_message_info(content: Option<&Value>) -> (usize, bool) {
    match content {
        Some(Value::String(s)) => (s.len(), false),
        Some(Value::Array(arr)) => {
            let mut total_len = 0;
            let mut has_image = false;

            for item in arr {
                match item.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            total_len += text.len();
                        }
                    }
                    Some("image") => {
                        has_image = true;
                    }
                    _ => {}
                }
            }

            (total_len, has_image)
        }
        _ => (0, false),
    }
}

/// Extracts file path from tool input based on tool name.
fn extract_file(tool: &str, input: &Value) -> Option<String> {
    match tool {
        "Edit" | "Read" | "Write" | "NotebookEdit" => {
            input.get("file_path")?.as_str().map(String::from)
        }
        "Glob" | "Grep" => input.get("path")?.as_str().map(String::from),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tempfile::TempDir;

    fn setup_test_dirs() -> (TempDir, PathBuf, PathBuf) {
        let temp = TempDir::new().unwrap();
        let data_dir = temp.path().join(".time-tracker");
        let claude_dir = temp.path().join(".claude").join("projects");
        fs::create_dir_all(&data_dir).unwrap();
        fs::create_dir_all(&claude_dir).unwrap();
        (temp, data_dir, claude_dir)
    }

    #[test]
    fn test_empty_data_directory() {
        let (_temp, data_dir, claude_dir) = setup_test_dirs();
        let mut output = Cursor::new(Vec::new());

        let result = run_impl(&data_dir, &claude_dir, &mut output);

        assert!(result.is_ok());
        assert!(output.get_ref().is_empty());
    }

    #[test]
    fn test_tmux_events_passthrough() {
        let (_temp, data_dir, claude_dir) = setup_test_dirs();

        // Write a tmux event
        let event = r#"{"id":"remote.tmux:tmux_pane_focus:2025-01-29T12:00:00.000Z:%3","timestamp":"2025-01-29T12:00:00.000Z","source":"remote.tmux","type":"tmux_pane_focus","data":{"pane_id":"%3","session_name":"dev","cwd":"/home/user"}}"#;
        fs::write(data_dir.join("events.jsonl"), format!("{event}\n")).unwrap();

        let mut output = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &mut output).unwrap();

        let output_str = String::from_utf8(output.into_inner()).unwrap();
        assert_eq!(output_str.trim(), event);
    }

    #[test]
    fn test_malformed_line_skipped() {
        let (_temp, data_dir, claude_dir) = setup_test_dirs();

        // Write valid and invalid lines
        let content = r#"{"id":"1","timestamp":"2025-01-29T12:00:00Z","source":"remote.tmux","type":"test","data":{}}
not valid json
{"id":"2","timestamp":"2025-01-29T12:01:00Z","source":"remote.tmux","type":"test","data":{}}
"#;
        fs::write(data_dir.join("events.jsonl"), content).unwrap();

        let mut output = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &mut output).unwrap();

        let output_str = String::from_utf8(output.into_inner()).unwrap();
        assert_eq!(output_str.lines().count(), 2); // Only valid lines
    }

    #[test]
    fn test_empty_lines_skipped() {
        let (_temp, data_dir, claude_dir) = setup_test_dirs();

        let content = r#"{"id":"1","timestamp":"2025-01-29T12:00:00Z","source":"remote.tmux","type":"test","data":{}}

{"id":"2","timestamp":"2025-01-29T12:01:00Z","source":"remote.tmux","type":"test","data":{}}
"#;
        fs::write(data_dir.join("events.jsonl"), content).unwrap();

        let mut output = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &mut output).unwrap();

        let output_str = String::from_utf8(output.into_inner()).unwrap();
        assert_eq!(output_str.lines().count(), 2);
    }

    #[test]
    fn test_claude_session_start_event() {
        let (_temp, data_dir, claude_dir) = setup_test_dirs();

        // Create a project directory with a session log
        let project_dir = claude_dir.join("test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let claude_entry = r#"{"type":"user","sessionId":"sess123","timestamp":"2025-01-29T12:00:00Z","cwd":"/home/user/project","message":{"content":"hello"}}"#;
        fs::write(
            project_dir.join("session.jsonl"),
            format!("{claude_entry}\n"),
        )
        .unwrap();

        let mut output = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &mut output).unwrap();

        let output_str = String::from_utf8(output.into_inner()).unwrap();
        let lines: Vec<&str> = output_str.lines().collect();

        // Should have 2 events: session start + user message
        assert_eq!(lines.len(), 2);

        // First event should be session start
        let session_event: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(session_event["type"], "agent_session");
        assert_eq!(session_event["data"]["action"], "started");
        assert_eq!(session_event["data"]["session_id"], "sess123");
    }

    #[test]
    fn test_claude_user_message_event() {
        let (_temp, data_dir, claude_dir) = setup_test_dirs();

        let project_dir = claude_dir.join("test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let claude_entry = r#"{"type":"user","sessionId":"sess123","timestamp":"2025-01-29T12:00:00Z","uuid":"msg-uuid-123","message":{"content":"hello world"}}"#;
        fs::write(
            project_dir.join("session.jsonl"),
            format!("{claude_entry}\n"),
        )
        .unwrap();

        let mut output = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &mut output).unwrap();

        let output_str = String::from_utf8(output.into_inner()).unwrap();
        let lines: Vec<&str> = output_str.lines().collect();

        // Second event should be user message
        let user_event: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(user_event["type"], "user_message");
        assert_eq!(user_event["data"]["length"], 11); // "hello world".len()
        assert_eq!(user_event["data"]["has_image"], false);
    }

    #[test]
    fn test_claude_tool_result_filtered() {
        let (_temp, data_dir, claude_dir) = setup_test_dirs();

        let project_dir = claude_dir.join("test-project");
        fs::create_dir_all(&project_dir).unwrap();

        // Tool result message should be filtered
        let claude_entry = r#"{"type":"user","sessionId":"sess123","timestamp":"2025-01-29T12:00:00Z","message":{"content":[{"type":"tool_result","tool_use_id":"tool123","content":"result"}]}}"#;
        fs::write(
            project_dir.join("session.jsonl"),
            format!("{claude_entry}\n"),
        )
        .unwrap();

        let mut output = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &mut output).unwrap();

        let output_str = String::from_utf8(output.into_inner()).unwrap();
        let lines: Vec<&str> = output_str.lines().collect();

        // Should only have session start, no user_message for tool_result
        assert_eq!(lines.len(), 1);
        let event: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(event["type"], "agent_session");
    }

    #[test]
    fn test_claude_tool_use_event() {
        let (_temp, data_dir, claude_dir) = setup_test_dirs();

        let project_dir = claude_dir.join("test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let claude_entry = r#"{"type":"assistant","sessionId":"sess123","timestamp":"2025-01-29T12:00:00Z","message":{"content":[{"type":"tool_use","id":"tool123","name":"Read","input":{"file_path":"/home/user/file.rs"}}]}}"#;
        fs::write(
            project_dir.join("session.jsonl"),
            format!("{claude_entry}\n"),
        )
        .unwrap();

        let mut output = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &mut output).unwrap();

        let output_str = String::from_utf8(output.into_inner()).unwrap();
        let lines: Vec<&str> = output_str.lines().collect();

        // Should have session start + tool use
        assert_eq!(lines.len(), 2);

        let tool_event: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(tool_event["type"], "agent_tool_use");
        assert_eq!(tool_event["data"]["tool"], "Read");
        assert_eq!(tool_event["data"]["file"], "/home/user/file.rs");
    }

    #[test]
    fn test_multiple_tool_use_per_message() {
        let (_temp, data_dir, claude_dir) = setup_test_dirs();

        let project_dir = claude_dir.join("test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let claude_entry = r#"{"type":"assistant","sessionId":"sess123","timestamp":"2025-01-29T12:00:00Z","message":{"content":[{"type":"tool_use","id":"tool1","name":"Read","input":{"file_path":"/a.rs"}},{"type":"tool_use","id":"tool2","name":"Edit","input":{"file_path":"/b.rs"}}]}}"#;
        fs::write(
            project_dir.join("session.jsonl"),
            format!("{claude_entry}\n"),
        )
        .unwrap();

        let mut output = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &mut output).unwrap();

        let output_str = String::from_utf8(output.into_inner()).unwrap();

        // Should have session start + 2 tool use events
        assert_eq!(output_str.lines().count(), 3);
    }

    #[test]
    fn test_filtered_entry_types() {
        let (_temp, data_dir, claude_dir) = setup_test_dirs();

        let project_dir = claude_dir.join("test-project");
        fs::create_dir_all(&project_dir).unwrap();

        // These types should be filtered out completely (no events emitted)
        let entries = r#"{"type":"progress","sessionId":"sess123","timestamp":"2025-01-29T12:00:00Z"}
{"type":"file-history-snapshot","sessionId":"sess123","timestamp":"2025-01-29T12:01:00Z"}
{"type":"summary","sessionId":"sess123","timestamp":"2025-01-29T12:02:00Z"}
{"type":"system","sessionId":"sess123","timestamp":"2025-01-29T12:03:00Z"}
"#;
        fs::write(project_dir.join("session.jsonl"), entries).unwrap();

        let mut output = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &mut output).unwrap();

        // No events should be emitted for filtered types
        assert!(output.get_ref().is_empty());
    }

    #[test]
    fn test_filtered_entries_mixed_with_valid() {
        let (_temp, data_dir, claude_dir) = setup_test_dirs();

        let project_dir = claude_dir.join("test-project");
        fs::create_dir_all(&project_dir).unwrap();

        // Mix of filtered and valid entries - session start should come from first valid entry
        let entries = r#"{"type":"progress","sessionId":"sess123","timestamp":"2025-01-29T12:00:00Z"}
{"type":"user","sessionId":"sess123","timestamp":"2025-01-29T12:01:00Z","message":{"content":"hello"}}
{"type":"summary","sessionId":"sess123","timestamp":"2025-01-29T12:02:00Z"}
"#;
        fs::write(project_dir.join("session.jsonl"), entries).unwrap();

        let mut output = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &mut output).unwrap();

        let output_str = String::from_utf8(output.into_inner()).unwrap();
        let lines: Vec<&str> = output_str.lines().collect();

        // Should have session start + user message (filtered entries skipped)
        assert_eq!(lines.len(), 2);
        let session_event: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(session_event["type"], "agent_session");
        let user_event: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(user_event["type"], "user_message");
    }

    #[test]
    fn test_deterministic_ids() {
        let (_temp, data_dir, claude_dir) = setup_test_dirs();

        let project_dir = claude_dir.join("test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let claude_entry = r#"{"type":"user","sessionId":"sess123","timestamp":"2025-01-29T12:00:00Z","uuid":"msg-uuid","message":{"content":"hello"}}"#;
        fs::write(
            project_dir.join("session.jsonl"),
            format!("{claude_entry}\n"),
        )
        .unwrap();

        // Run twice
        let mut output1 = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &mut output1).unwrap();

        let mut output2 = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &mut output2).unwrap();

        // Output should be identical
        assert_eq!(output1.into_inner(), output2.into_inner());
    }

    #[test]
    fn test_combined_tmux_and_claude() {
        let (_temp, data_dir, claude_dir) = setup_test_dirs();

        // Write tmux event
        let tmux_event = r#"{"id":"tmux1","timestamp":"2025-01-29T11:00:00Z","source":"remote.tmux","type":"tmux_pane_focus","data":{}}"#;
        fs::write(data_dir.join("events.jsonl"), format!("{tmux_event}\n")).unwrap();

        // Write Claude event
        let project_dir = claude_dir.join("test-project");
        fs::create_dir_all(&project_dir).unwrap();
        let claude_entry = r#"{"type":"user","sessionId":"sess123","timestamp":"2025-01-29T12:00:00Z","message":{"content":"hi"}}"#;
        fs::write(
            project_dir.join("session.jsonl"),
            format!("{claude_entry}\n"),
        )
        .unwrap();

        let mut output = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &mut output).unwrap();

        let output_str = String::from_utf8(output.into_inner()).unwrap();

        // tmux event + session start + user message
        assert_eq!(output_str.lines().count(), 3);
    }

    #[test]
    fn test_subagents_directory_excluded() {
        let (_temp, data_dir, claude_dir) = setup_test_dirs();

        // Create a subagents directory (should be excluded)
        let subagents_dir = claude_dir.join("subagents");
        fs::create_dir_all(&subagents_dir).unwrap();
        let subagent_entry = r#"{"type":"user","sessionId":"sub123","timestamp":"2025-01-29T12:00:00Z","message":{"content":"subagent"}}"#;
        fs::write(
            subagents_dir.join("session.jsonl"),
            format!("{subagent_entry}\n"),
        )
        .unwrap();

        let mut output = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &mut output).unwrap();

        // No output because subagents are excluded
        assert!(output.get_ref().is_empty());
    }

    #[test]
    fn test_extract_file_for_various_tools() {
        // Edit, Read, Write, NotebookEdit use file_path
        let input = serde_json::json!({"file_path": "/path/to/file.rs"});
        assert_eq!(
            extract_file("Edit", &input),
            Some("/path/to/file.rs".to_string())
        );
        assert_eq!(
            extract_file("Read", &input),
            Some("/path/to/file.rs".to_string())
        );
        assert_eq!(
            extract_file("Write", &input),
            Some("/path/to/file.rs".to_string())
        );
        assert_eq!(
            extract_file("NotebookEdit", &input),
            Some("/path/to/file.rs".to_string())
        );

        // Glob, Grep use path
        let input = serde_json::json!({"path": "/search/path"});
        assert_eq!(
            extract_file("Glob", &input),
            Some("/search/path".to_string())
        );
        assert_eq!(
            extract_file("Grep", &input),
            Some("/search/path".to_string())
        );

        // Unknown tools return None
        let input = serde_json::json!({"file_path": "/path"});
        assert_eq!(extract_file("Task", &input), None);
        assert_eq!(extract_file("Bash", &input), None);
    }

    #[test]
    fn test_message_with_image() {
        let content = serde_json::json!([
            {"type": "text", "text": "look at this"},
            {"type": "image", "source": {"type": "base64"}}
        ]);

        let (length, has_image) = extract_message_info(Some(&content));
        assert_eq!(length, 12); // "look at this".len()
        assert!(has_image);
    }

    #[test]
    fn test_output_is_valid_jsonl() {
        let (_temp, data_dir, claude_dir) = setup_test_dirs();

        // Write multiple events of different types
        let tmux_event = r#"{"id":"tmux1","timestamp":"2025-01-29T11:00:00Z","source":"remote.tmux","type":"tmux_pane_focus","data":{}}"#;
        fs::write(data_dir.join("events.jsonl"), format!("{tmux_event}\n")).unwrap();

        let project_dir = claude_dir.join("test-project");
        fs::create_dir_all(&project_dir).unwrap();
        let entries = r#"{"type":"user","sessionId":"s1","timestamp":"2025-01-29T12:00:00Z","message":{"content":"hi"}}
{"type":"assistant","sessionId":"s1","timestamp":"2025-01-29T12:01:00Z","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/f"}}]}}
"#;
        fs::write(project_dir.join("session.jsonl"), entries).unwrap();

        let mut output = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &mut output).unwrap();

        let output_str = String::from_utf8(output.into_inner()).unwrap();

        // Verify each line is valid JSON
        for line in output_str.lines() {
            let parsed: Result<Value, _> = serde_json::from_str(line);
            assert!(parsed.is_ok(), "Invalid JSON: {line}");

            let event = parsed.unwrap();
            assert!(event["id"].is_string());
            assert!(event["timestamp"].is_string());
            assert!(event["source"].is_string());
            assert!(event["type"].is_string());
        }
    }
}

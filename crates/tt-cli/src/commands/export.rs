//! Export command for syncing events to local machine.
//!
//! This module reads events from both `events.jsonl` (tmux events) and
//! Claude Code session logs, outputting a combined JSONL stream to stdout.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
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

/// Manifest tracking byte offsets for incremental Claude log parsing.
/// Maps file path to byte offset after last successfully parsed line.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClaudeManifest {
    /// Byte offset per session file path
    pub sessions: HashMap<PathBuf, u64>,
}

impl ClaudeManifest {
    /// Loads manifest from file path, returning empty manifest if file is missing or corrupted.
    fn load(path: &Path) -> Self {
        // Single syscall - try to open directly, handle NotFound
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                tracing::warn!(error = %e, "failed to read manifest, performing full re-parse");
                return Self::default();
            }
        };

        // Stream directly from file without intermediate String allocation
        match serde_json::from_reader(BufReader::new(file)) {
            Ok(manifest) => manifest,
            Err(e) => {
                tracing::warn!(error = %e, "failed to parse manifest, performing full re-parse");
                Self::default()
            }
        }
    }

    /// Saves manifest to file using atomic write (temp file + rename).
    fn save(&self, path: &Path) -> Result<()> {
        use std::io::BufWriter;

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("failed to create manifest directory")?;
        }

        let temp_path = path.with_extension("json.tmp");
        let file = File::create(&temp_path).context("failed to create temp manifest")?;
        let writer = BufWriter::new(file);

        // Stream directly to file, no intermediate String allocation
        serde_json::to_writer(writer, self).context("failed to write manifest")?;

        fs::rename(&temp_path, path).context("failed to rename temp manifest")?;
        Ok(())
    }
}

/// Returns the default time tracker data directory.
fn default_data_dir() -> PathBuf {
    crate::config::dirs_data_path()
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Returns the default Claude projects directory.
fn default_claude_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("projects")
}

/// Runs the export command, outputting all events to stdout.
pub fn run(after: Option<&str>) -> Result<()> {
    let identity = crate::machine::require_machine_identity()?;
    let data_dir = default_data_dir();
    let state_dir = crate::config::dirs_state_path()
        .unwrap_or_else(|| data_dir.clone());
    run_impl(
        &data_dir,
        &default_claude_dir(),
        &state_dir,
        &identity.machine_id,
        after,
        &mut std::io::stdout(),
    )
}

/// Implementation of export that allows injecting paths for testing.
fn run_impl(
    data_dir: &Path,
    claude_dir: &Path,
    state_dir: &Path,
    machine_id: &str,
    after: Option<&str>,
    output: &mut dyn Write,
) -> Result<()> {
    // Export tmux events
    let events_file = data_dir.join("events.jsonl");
    if events_file.exists() {
        export_tmux_events(&events_file, after, output)?;
    }

    // Export Claude events with incremental parsing
    if claude_dir.exists() {
        let manifest_path = state_dir.join("claude-manifest.json");
        export_claude_events(claude_dir, &manifest_path, machine_id, output)?;
    }

    Ok(())
}

/// Exports tmux events from events.jsonl, passing through valid lines.
/// When `after` is provided, only events after the line containing that ID are exported.
fn export_tmux_events(events_file: &Path, after: Option<&str>, output: &mut dyn Write) -> Result<()> {
    let file = File::open(events_file).context("failed to open events.jsonl")?;
    let reader = BufReader::new(file);
    let mut past_marker = after.is_none();

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

        if !past_marker {
            if let Some(after_id) = after {
                if line.contains(after_id) {
                    past_marker = true;
                }
            }
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

/// Exports events from Claude session logs with incremental parsing.
fn export_claude_events(
    claude_dir: &Path,
    manifest_path: &Path,
    machine_id: &str,
    output: &mut dyn Write,
) -> Result<()> {
    let logs = discover_claude_logs(claude_dir)?;
    let mut manifest = ClaudeManifest::load(manifest_path);

    // Track seen sessions across ALL files to avoid duplicate session start events
    let mut seen_sessions: HashSet<String> = HashSet::new();
    // Track which files we've processed (to clean up deleted files from manifest)
    let mut processed_files: HashSet<PathBuf> = HashSet::new();

    for log_path in logs {
        let start_offset = manifest.sessions.get(&log_path).copied().unwrap_or(0);
        match export_single_claude_log(&log_path, &mut seen_sessions, machine_id, output, start_offset) {
            Ok(final_offset) => {
                manifest.sessions.insert(log_path.clone(), final_offset);
                processed_files.insert(log_path);
            }
            Err(e) => {
                tracing::warn!(path = %log_path.display(), error = %e, "failed to parse Claude log");
                // Keep existing manifest entry on error (don't lose progress)
                processed_files.insert(log_path);
            }
        }
    }

    // Remove entries for files that no longer exist
    manifest
        .sessions
        .retain(|path, _| processed_files.contains(path));

    // Save manifest (log warning on failure, don't fail export)
    if let Err(e) = manifest.save(manifest_path) {
        tracing::warn!(error = %e, "failed to save manifest, next export may reprocess some events");
    }

    Ok(())
}

/// Entry types to filter out (not events).
const FILTERED_TYPES: &[&str] = &["progress", "file-history-snapshot", "summary", "system"];

/// Exports events from a single Claude session log file.
/// Returns the byte offset after the last successfully parsed line.
fn export_single_claude_log(
    log_path: &Path,
    seen_sessions: &mut HashSet<String>,
    machine_id: &str,
    output: &mut dyn Write,
    start_offset: u64,
) -> Result<u64> {
    let file = File::open(log_path).context("failed to open Claude log")?;
    let file_size = file.metadata()?.len();

    // If offset is beyond file size, file was truncated - restart from 0
    let actual_offset = if start_offset > file_size {
        tracing::info!(
            path = %log_path.display(),
            start_offset,
            file_size,
            "file smaller than recorded offset, restarting from beginning"
        );
        0
    } else {
        start_offset
    };

    let mut reader = BufReader::with_capacity(32 * 1024, file);
    reader.seek(SeekFrom::Start(actual_offset))?;

    // Track position mathematically - avoids syscalls on every line
    let mut current_position = actual_offset;
    let mut last_good_position = actual_offset;
    let mut line_num = 0;
    // Reuse String buffer across iterations to avoid repeated allocations
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(bytes_read) => {
                line_num += 1;
                current_position += bytes_read as u64;

                // Skip empty lines
                if line.trim().is_empty() {
                    last_good_position = current_position;
                    continue;
                }

                let entry: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(line = line_num, error = %e, "malformed JSON in Claude log");
                        // Don't update last_good_position - this line may be incomplete
                        // If it's a partial line at EOF, we'll re-read it next time
                        continue;
                    }
                };

                process_claude_entry(&entry, seen_sessions, machine_id, output)?;
                last_good_position = current_position;
            }
            Err(e) => {
                tracing::debug!(line = line_num, error = %e, "error reading line");
                break;
            }
        }
    }

    Ok(last_good_position)
}

/// Processes a single Claude log entry and emits events.
fn process_claude_entry(
    entry: &Value,
    seen_sessions: &mut HashSet<String>,
    machine_id: &str,
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
    // Check contains() first to avoid allocation when session already seen
    if !seen_sessions.contains(session_id) {
        emit_session_start(session_id, timestamp, cwd.as_deref(), machine_id, output)?;
        seen_sessions.insert(session_id.to_string());
    }

    // Process based on entry type
    match entry_type {
        "user" => emit_user_message(entry, session_id, timestamp, machine_id, output)?,
        "assistant" => emit_tool_uses(entry, session_id, timestamp, machine_id, output)?,
        _ => {}
    }

    Ok(())
}

/// Emits an `agent_session` start event.
fn emit_session_start(
    session_id: &str,
    timestamp: &str,
    cwd: Option<&str>,
    machine_id: &str,
    output: &mut dyn Write,
) -> Result<()> {
    let event = ExportEvent {
        id: format!("{machine_id}:remote.agent:agent_session:{timestamp}:{session_id}:started"),
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
    machine_id: &str,
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
            "{machine_id}:remote.agent:user_message:{timestamp}:{}",
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
    machine_id: &str,
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
            id: format!("{machine_id}:remote.agent:agent_tool_use:{timestamp}:{tool_id}"),
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

    const TEST_MACHINE_ID: &str = "00000000-0000-0000-0000-000000000000";

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

        let result = run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output);

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
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output).unwrap();

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
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output).unwrap();

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
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output).unwrap();

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
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output).unwrap();

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
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output).unwrap();

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
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output).unwrap();

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
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output).unwrap();

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
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output).unwrap();

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
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output).unwrap();

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
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output).unwrap();

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
        // Test that event IDs are deterministic - same input produces same IDs
        // We need separate temp directories to avoid manifest affecting second run
        let temp1 = TempDir::new().unwrap();
        let data_dir1 = temp1.path().join(".time-tracker");
        let claude_dir1 = temp1.path().join(".claude").join("projects");
        fs::create_dir_all(&data_dir1).unwrap();
        fs::create_dir_all(&claude_dir1).unwrap();

        let temp2 = TempDir::new().unwrap();
        let data_dir2 = temp2.path().join(".time-tracker");
        let claude_dir2 = temp2.path().join(".claude").join("projects");
        fs::create_dir_all(&data_dir2).unwrap();
        fs::create_dir_all(&claude_dir2).unwrap();

        // Same content in both directories
        let claude_entry = r#"{"type":"user","sessionId":"sess123","timestamp":"2025-01-29T12:00:00Z","uuid":"msg-uuid","message":{"content":"hello"}}"#;

        let project_dir1 = claude_dir1.join("test-project");
        fs::create_dir_all(&project_dir1).unwrap();
        fs::write(
            project_dir1.join("session.jsonl"),
            format!("{claude_entry}\n"),
        )
        .unwrap();

        let project_dir2 = claude_dir2.join("test-project");
        fs::create_dir_all(&project_dir2).unwrap();
        fs::write(
            project_dir2.join("session.jsonl"),
            format!("{claude_entry}\n"),
        )
        .unwrap();

        // Run on both directories
        let mut output1 = Cursor::new(Vec::new());
        run_impl(&data_dir1, &claude_dir1, &data_dir1, TEST_MACHINE_ID, None, &mut output1).unwrap();

        let mut output2 = Cursor::new(Vec::new());
        run_impl(&data_dir2, &claude_dir2, &data_dir2, TEST_MACHINE_ID, None, &mut output2).unwrap();

        // Output should be identical (same IDs for same input)
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
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output).unwrap();

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
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output).unwrap();

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
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output).unwrap();

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

    // ============ Manifest/Incremental Parsing Tests ============

    #[test]
    fn test_manifest_created_on_fresh_export() {
        let (_temp, data_dir, claude_dir) = setup_test_dirs();

        let project_dir = claude_dir.join("test-project");
        fs::create_dir_all(&project_dir).unwrap();
        let log_path = project_dir.join("session.jsonl");

        let entry = r#"{"type":"user","sessionId":"sess1","timestamp":"2025-01-29T12:00:00Z","message":{"content":"hello"}}"#;
        fs::write(&log_path, format!("{entry}\n")).unwrap();

        let mut output = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output).unwrap();

        // Manifest should be created
        let manifest_path = data_dir.join("claude-manifest.json");
        assert!(manifest_path.exists(), "manifest should be created");

        // Manifest should contain the session file with correct offset
        let manifest: ClaudeManifest =
            serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
        assert!(
            manifest.sessions.contains_key(&log_path),
            "manifest should track the session file"
        );

        // Offset should be at the end of the file (after the newline)
        let file_size = fs::metadata(&log_path).unwrap().len();
        assert_eq!(
            manifest.sessions[&log_path], file_size,
            "offset should be at end of file"
        );
    }

    #[test]
    fn test_incremental_export_only_new_lines() {
        let (_temp, data_dir, claude_dir) = setup_test_dirs();

        let project_dir = claude_dir.join("test-project");
        fs::create_dir_all(&project_dir).unwrap();
        let log_path = project_dir.join("session.jsonl");

        // Initial content
        let entry1 = r#"{"type":"user","sessionId":"sess1","timestamp":"2025-01-29T12:00:00Z","message":{"content":"hello"}}"#;
        fs::write(&log_path, format!("{entry1}\n")).unwrap();

        // First export - full parse
        let mut output1 = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output1).unwrap();
        let output1_str = String::from_utf8(output1.into_inner()).unwrap();
        let first_count = output1_str.lines().count();

        // Append new content
        let entry2 = r#"{"type":"user","sessionId":"sess1","timestamp":"2025-01-29T12:01:00Z","message":{"content":"world"}}"#;
        let mut file = fs::OpenOptions::new().append(true).open(&log_path).unwrap();
        writeln!(file, "{entry2}").unwrap();

        // Second export - should only parse new bytes
        let mut output2 = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output2).unwrap();
        let output2_str = String::from_utf8(output2.into_inner()).unwrap();
        let second_count = output2_str.lines().count();

        // First export: session_start + user_message = 2
        assert_eq!(first_count, 2, "first export should have 2 events");

        // Second export: session_start (re-emitted, same ID) + user_message = 2
        // Session start is re-emitted because seen_sessions is reset between exports.
        // The event ID is deterministic so downstream import can dedupe.
        // The key assertion is that we only parse NEW bytes (not the old user message).
        assert_eq!(
            second_count, 2,
            "second export should have session_start + new user_message"
        );

        // Verify second export has the "world" message (not the old "hello")
        let events: Vec<Value> = output2_str
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(events[0]["type"], "agent_session");
        assert_eq!(events[1]["type"], "user_message");
        // The key test: we should NOT have re-parsed the first user message
        // which had content "hello" - we should only see events from the new line
    }

    #[test]
    fn test_corrupted_manifest_full_reparse() {
        let (_temp, data_dir, claude_dir) = setup_test_dirs();

        let project_dir = claude_dir.join("test-project");
        fs::create_dir_all(&project_dir).unwrap();
        let log_path = project_dir.join("session.jsonl");

        let entry = r#"{"type":"user","sessionId":"sess1","timestamp":"2025-01-29T12:00:00Z","message":{"content":"hello"}}"#;
        fs::write(&log_path, format!("{entry}\n")).unwrap();

        // Write corrupted manifest
        let manifest_path = data_dir.join("claude-manifest.json");
        fs::write(&manifest_path, "not valid json {{{").unwrap();

        // Export should succeed with full re-parse
        let mut output = Cursor::new(Vec::new());
        let result = run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output);
        assert!(
            result.is_ok(),
            "export should succeed despite corrupted manifest"
        );

        let output_str = String::from_utf8(output.into_inner()).unwrap();
        // Should have session_start + user_message = 2 (full re-parse)
        assert_eq!(output_str.lines().count(), 2);
    }

    #[test]
    fn test_file_truncated_restart_from_zero() {
        let (_temp, data_dir, claude_dir) = setup_test_dirs();

        let project_dir = claude_dir.join("test-project");
        fs::create_dir_all(&project_dir).unwrap();
        let log_path = project_dir.join("session.jsonl");

        // Initial large content
        let entry1 = r#"{"type":"user","sessionId":"sess1","timestamp":"2025-01-29T12:00:00Z","message":{"content":"hello world this is a longer message"}}"#;
        fs::write(&log_path, format!("{entry1}\n")).unwrap();

        // First export
        let mut output1 = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output1).unwrap();

        // Record the offset
        let manifest_path = data_dir.join("claude-manifest.json");
        let manifest: ClaudeManifest =
            serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
        let old_offset = manifest.sessions[&log_path];

        // Replace file with shorter content (simulating truncation)
        let entry2 = r#"{"type":"user","sessionId":"sess2","timestamp":"2025-01-29T13:00:00Z","message":{"content":"hi"}}"#;
        fs::write(&log_path, format!("{entry2}\n")).unwrap();

        // Verify new file is smaller
        let new_size = fs::metadata(&log_path).unwrap().len();
        assert!(
            new_size < old_offset,
            "new file should be smaller than old offset"
        );

        // Second export should restart from 0
        let mut output2 = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output2).unwrap();

        let output2_str = String::from_utf8(output2.into_inner()).unwrap();
        // Should have session_start + user_message = 2 (full re-parse from start)
        assert_eq!(output2_str.lines().count(), 2);

        // Verify we got sess2 (not trying to read at invalid offset)
        let events: Vec<Value> = output2_str
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let session_event = &events[0];
        assert_eq!(session_event["data"]["session_id"], "sess2");
    }

    #[test]
    fn test_deleted_file_removed_from_manifest() {
        let (_temp, data_dir, claude_dir) = setup_test_dirs();

        let project_dir = claude_dir.join("test-project");
        fs::create_dir_all(&project_dir).unwrap();
        let log_path = project_dir.join("session.jsonl");

        let entry = r#"{"type":"user","sessionId":"sess1","timestamp":"2025-01-29T12:00:00Z","message":{"content":"hello"}}"#;
        fs::write(&log_path, format!("{entry}\n")).unwrap();

        // First export
        let mut output1 = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output1).unwrap();

        // Verify manifest has the file
        let manifest_path = data_dir.join("claude-manifest.json");
        let manifest: ClaudeManifest =
            serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
        assert!(manifest.sessions.contains_key(&log_path));

        // Delete the file
        fs::remove_file(&log_path).unwrap();

        // Second export
        let mut output2 = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output2).unwrap();

        // Manifest should no longer contain the deleted file
        let manifest: ClaudeManifest =
            serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
        assert!(
            !manifest.sessions.contains_key(&log_path),
            "deleted file should be removed from manifest"
        );
    }

    #[test]
    fn test_multiple_files_tracked_independently() {
        let (_temp, data_dir, claude_dir) = setup_test_dirs();

        let project_dir = claude_dir.join("test-project");
        fs::create_dir_all(&project_dir).unwrap();

        let log1_path = project_dir.join("session1.jsonl");
        let log2_path = project_dir.join("session2.jsonl");

        let entry1 = r#"{"type":"user","sessionId":"sess1","timestamp":"2025-01-29T12:00:00Z","message":{"content":"file1"}}"#;
        let entry2 = r#"{"type":"user","sessionId":"sess2","timestamp":"2025-01-29T12:00:00Z","message":{"content":"file2"}}"#;
        fs::write(&log1_path, format!("{entry1}\n")).unwrap();
        fs::write(&log2_path, format!("{entry2}\n")).unwrap();

        // First export
        let mut output1 = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output1).unwrap();
        let output1_str = String::from_utf8(output1.into_inner()).unwrap();
        // 2 sessions x (session_start + user_message) = 4 events
        assert_eq!(output1_str.lines().count(), 4);

        // Verify manifest has both files
        let manifest_path = data_dir.join("claude-manifest.json");
        let manifest: ClaudeManifest =
            serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest.sessions.len(), 2);
        assert!(manifest.sessions.contains_key(&log1_path));
        assert!(manifest.sessions.contains_key(&log2_path));

        // Append to only one file
        let entry3 = r#"{"type":"user","sessionId":"sess1","timestamp":"2025-01-29T12:01:00Z","message":{"content":"more"}}"#;
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&log1_path)
            .unwrap();
        writeln!(file, "{entry3}").unwrap();

        // Second export
        let mut output2 = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output2).unwrap();

        let output2_str = String::from_utf8(output2.into_inner()).unwrap();
        // session_start (re-emitted for sess1) + user_message = 2 events
        // No events from file2 (no new content)
        assert_eq!(output2_str.lines().count(), 2);

        // Verify the events are from sess1
        let events: Vec<Value> = output2_str
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(events[0]["data"]["session_id"], "sess1");
        assert_eq!(events[1]["data"]["session_id"], "sess1");
    }

    #[test]
    fn test_offset_at_eof_no_new_content() {
        let (_temp, data_dir, claude_dir) = setup_test_dirs();

        let project_dir = claude_dir.join("test-project");
        fs::create_dir_all(&project_dir).unwrap();
        let log_path = project_dir.join("session.jsonl");

        let entry = r#"{"type":"user","sessionId":"sess1","timestamp":"2025-01-29T12:00:00Z","message":{"content":"hello"}}"#;
        fs::write(&log_path, format!("{entry}\n")).unwrap();

        // First export
        let mut output1 = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output1).unwrap();

        // Second export without any changes
        let mut output2 = Cursor::new(Vec::new());
        run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output2).unwrap();

        // Should have no output (no new content)
        assert!(output2.get_ref().is_empty());
    }

    #[test]
    fn test_empty_file_handled_gracefully() {
        let (_temp, data_dir, claude_dir) = setup_test_dirs();

        let project_dir = claude_dir.join("test-project");
        fs::create_dir_all(&project_dir).unwrap();
        let log_path = project_dir.join("session.jsonl");

        // Create empty file
        fs::write(&log_path, "").unwrap();

        let mut output = Cursor::new(Vec::new());
        let result = run_impl(&data_dir, &claude_dir, &data_dir, TEST_MACHINE_ID, None, &mut output);

        assert!(result.is_ok());
        assert!(output.get_ref().is_empty());

        // Manifest should track the file with offset 0
        let manifest_path = data_dir.join("claude-manifest.json");
        let manifest: ClaudeManifest =
            serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest.sessions[&log_path], 0);
    }

    #[test]
    fn test_bufreader_stream_position_semantics() {
        // Unit test to verify BufReader behavior: after reading lines,
        // stream_position should give us the position to resume from
        use std::io::Cursor;

        let content = "line1\nline2\nline3\n";
        let cursor = Cursor::new(content.as_bytes().to_vec());
        let mut reader = BufReader::new(cursor);

        // Read first two lines
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "line1\n");

        let pos_after_line1 = reader.stream_position().unwrap();
        assert_eq!(pos_after_line1, 6); // "line1\n" = 6 bytes

        line.clear();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line, "line2\n");

        let pos_after_line2 = reader.stream_position().unwrap();
        assert_eq!(pos_after_line2, 12); // 6 + "line2\n" = 12 bytes

        // Now create a new reader and seek to pos_after_line1
        let cursor2 = Cursor::new(content.as_bytes().to_vec());
        let mut reader2 = BufReader::new(cursor2);
        reader2.seek(SeekFrom::Start(pos_after_line1)).unwrap();

        line.clear();
        reader2.read_line(&mut line).unwrap();
        assert_eq!(line, "line2\n", "should resume at line2");
    }

    #[test]
    fn test_export_after_filters_events() {
        let (temp, data_dir, _claude_dir) = setup_test_dirs();
        // Write 3 events
        let events = vec![
            format!(r#"{{"id":"{TEST_MACHINE_ID}:remote.tmux:tmux_pane_focus:2025-01-01T00:00:00.000Z:%1","timestamp":"2025-01-01T00:00:00.000Z","source":"remote.tmux","type":"tmux_pane_focus","pane_id":"%1","tmux_session":"main","cwd":"/tmp"}}"#),
            format!(r#"{{"id":"{TEST_MACHINE_ID}:remote.tmux:tmux_pane_focus:2025-01-01T00:01:00.000Z:%1","timestamp":"2025-01-01T00:01:00.000Z","source":"remote.tmux","type":"tmux_pane_focus","pane_id":"%1","tmux_session":"main","cwd":"/tmp"}}"#),
            format!(r#"{{"id":"{TEST_MACHINE_ID}:remote.tmux:tmux_pane_focus:2025-01-01T00:02:00.000Z:%1","timestamp":"2025-01-01T00:02:00.000Z","source":"remote.tmux","type":"tmux_pane_focus","pane_id":"%1","tmux_session":"main","cwd":"/tmp"}}"#),
        ];
        std::fs::write(data_dir.join("events.jsonl"), events.join("\n") + "\n").unwrap();

        // Export with --after pointing to the second event
        let after_id = format!("{TEST_MACHINE_ID}:remote.tmux:tmux_pane_focus:2025-01-01T00:01:00.000Z:%1");
        let mut output = Vec::new();
        let state_dir = data_dir.clone();
        run_impl(&data_dir, &temp.path().join(".claude/projects"), &state_dir, TEST_MACHINE_ID, Some(&after_id), &mut output).unwrap();

        let output_str = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = output_str.lines().collect();
        // Should only get the third event (after the marker)
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("00:02:00"));
    }
}

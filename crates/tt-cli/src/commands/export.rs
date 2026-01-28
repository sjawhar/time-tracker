//! Export command for combining remote buffer events with Claude logs.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use clap::Args;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

const AGENT_NAME: &str = "claude-code";
const EVENT_SOURCE_REMOTE_AGENT: &str = "remote.agent";
const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Args, Default)]
pub struct ExportArgs {}

pub fn run(_args: ExportArgs) -> Result<()> {
    let exporter = Exporter::from_default_home()?;
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    exporter.export(&mut handle)?;
    Ok(())
}

#[derive(Debug, Clone)]
struct Exporter {
    events_file: PathBuf,
    claude_projects_dir: PathBuf,
}

impl Exporter {
    fn from_default_home() -> Result<Self> {
        let home = dirs::home_dir().context("failed to determine home directory")?;
        Ok(Self::from_home_dir(&home))
    }

    fn from_home_dir(home: &Path) -> Self {
        Self {
            events_file: home.join(".time-tracker").join("events.jsonl"),
            claude_projects_dir: home.join(".claude").join("projects"),
        }
    }

    fn export<W: Write>(&self, writer: &mut W) -> Result<()> {
        self.export_buffered_events(writer)?;
        self.export_claude_events(writer)?;
        Ok(())
    }

    fn export_buffered_events<W: Write>(&self, writer: &mut W) -> Result<()> {
        if !self.events_file.exists() {
            return Ok(());
        }
        let file = File::open(&self.events_file).with_context(|| {
            format!("failed to open events file {}", self.events_file.display())
        })?;
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line.context("failed to read events line")?;
            if line.trim().is_empty() {
                continue;
            }
            writer
                .write_all(line.as_bytes())
                .context("failed to write event line")?;
            writer
                .write_all(b"\n")
                .context("failed to write newline")?;
        }
        Ok(())
    }

    fn export_claude_events<W: Write>(&self, writer: &mut W) -> Result<()> {
        let log_files = find_claude_log_files(&self.claude_projects_dir)?;
        for log_file in log_files {
            let session_fallback = log_file
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(ToString::to_string);
            let file = File::open(&log_file)
                .with_context(|| format!("failed to open {}", log_file.display()))?;
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let line = line.context("failed to read Claude log line")?;
                if line.trim().is_empty() {
                    continue;
                }
                let parsed: Value = match serde_json::from_str(&line) {
                    Ok(value) => value,
                    Err(_) => continue,
                };
                if let Some(event) = parse_claude_event(&parsed, session_fallback.as_deref()) {
                    let json = serde_json::to_string(&event)
                        .context("failed to serialize Claude event")?;
                    writer
                        .write_all(json.as_bytes())
                        .context("failed to write Claude event")?;
                    writer
                        .write_all(b"\n")
                        .context("failed to write Claude newline")?;
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct ExportEvent {
    id: String,
    timestamp: String,
    #[serde(rename = "type")]
    kind: String,
    source: String,
    schema_version: u32,
    data: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assignment_source: Option<String>,
}

fn parse_claude_event(value: &Value, session_fallback: Option<&str>) -> Option<ExportEvent> {
    let timestamp = extract_timestamp(value)?;
    let session_id = extract_session_id(value).or_else(|| session_fallback.map(ToString::to_string));
    let cwd = extract_cwd(value);

    let kind = extract_event_kind(value)?;
    let (event_type, data) = match kind {
        ClaudeEventKind::UserMessage { length, has_image } => (
            "user_message",
            serde_json::json!({
                "agent": AGENT_NAME,
                "session_id": session_id,
                "length": length,
                "has_image": has_image,
            }),
        ),
        ClaudeEventKind::ToolUse { tool, file } => (
            "agent_tool_use",
            serde_json::json!({
                "agent": AGENT_NAME,
                "session_id": session_id,
                "tool": tool,
                "file": file,
            }),
        ),
        ClaudeEventKind::Session { action } => (
            "agent_session",
            serde_json::json!({
                "action": action,
                "agent": AGENT_NAME,
                "session_id": session_id,
                "cwd": cwd,
            }),
        ),
    };

    let timestamp_str = format_timestamp(timestamp);
    let data_json = serde_json::to_string(&data).ok()?;
    let id = deterministic_event_id(EVENT_SOURCE_REMOTE_AGENT, event_type, &timestamp_str, &data_json);

    Some(ExportEvent {
        id,
        timestamp: timestamp_str,
        kind: event_type.to_string(),
        source: EVENT_SOURCE_REMOTE_AGENT.to_string(),
        schema_version: SCHEMA_VERSION,
        data,
        cwd,
        session_id,
        stream_id: None,
        assignment_source: None,
    })
}

#[derive(Debug)]
enum ClaudeEventKind {
    UserMessage { length: usize, has_image: bool },
    ToolUse { tool: String, file: Option<String> },
    Session { action: String },
}

fn extract_event_kind(value: &Value) -> Option<ClaudeEventKind> {
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| value.get("event").and_then(Value::as_str))
        .unwrap_or_default();

    if is_session_event(event_type) {
        let action = session_action(event_type, value)?;
        return Some(ClaudeEventKind::Session { action });
    }

    if is_tool_event(event_type, value) {
        let tool = extract_tool_name(value)?;
        let file = extract_tool_file(value);
        return Some(ClaudeEventKind::ToolUse { tool, file });
    }

    if is_message_event(event_type, value) {
        let (length, has_image) = extract_message_details(value);
        return Some(ClaudeEventKind::UserMessage { length, has_image });
    }

    None
}

fn is_session_event(event_type: &str) -> bool {
    matches!(
        event_type,
        "session_start"
            | "session_started"
            | "session_opened"
            | "session_end"
            | "session_ended"
            | "session_closed"
            | "session"
    )
}

fn session_action(event_type: &str, value: &Value) -> Option<String> {
    if event_type == "session" {
        return value
            .get("action")
            .and_then(Value::as_str)
            .map(ToString::to_string);
    }

    if event_type.contains("start") || event_type.contains("opened") {
        return Some("started".to_string());
    }

    if event_type.contains("end") || event_type.contains("closed") {
        return Some("ended".to_string());
    }

    None
}

fn is_tool_event(event_type: &str, value: &Value) -> bool {
    if matches!(event_type, "tool_use" | "tool" | "tool_used") {
        return true;
    }

    value.pointer("/tool/name").is_some() || value.pointer("/tool_use/name").is_some()
}

fn extract_tool_name(value: &Value) -> Option<String> {
    value
        .get("tool")
        .and_then(Value::as_str)
        .or_else(|| value.pointer("/tool/name").and_then(Value::as_str))
        .or_else(|| value.pointer("/tool_use/name").and_then(Value::as_str))
        .map(ToString::to_string)
}

fn extract_tool_file(value: &Value) -> Option<String> {
    value
        .get("file")
        .and_then(Value::as_str)
        .or_else(|| value.pointer("/tool/file").and_then(Value::as_str))
        .or_else(|| value.pointer("/tool/input/file").and_then(Value::as_str))
        .or_else(|| value.pointer("/tool/input/path").and_then(Value::as_str))
        .or_else(|| value.pointer("/path").and_then(Value::as_str))
        .map(ToString::to_string)
}

fn is_message_event(event_type: &str, value: &Value) -> bool {
    if matches!(event_type, "message" | "user_message" | "user") {
        return message_role(value).is_some();
    }
    message_role(value).is_some()
}

fn message_role(value: &Value) -> Option<&str> {
    value
        .get("role")
        .and_then(Value::as_str)
        .or_else(|| value.pointer("/message/role").and_then(Value::as_str))
        .filter(|role| *role == "user")
}

fn extract_message_details(value: &Value) -> (usize, bool) {
    let content = value
        .pointer("/message/content")
        .or_else(|| value.get("content"))
        .or_else(|| value.pointer("/message/text"));

    match content {
        Some(Value::String(text)) => (text.len(), false),
        Some(Value::Array(items)) => {
            let mut text_len = 0;
            let mut has_image = false;
            for item in items {
                match item {
                    Value::String(text) => text_len += text.len(),
                    Value::Object(map) => {
                        if let Some(Value::String(text)) = map.get("text") {
                            text_len += text.len();
                        }
                        if let Some(Value::String(kind)) = map.get("type") {
                            if kind.contains("image") {
                                has_image = true;
                            }
                        }
                        if let Some(Value::String(media)) = map.get("media_type") {
                            if media.starts_with("image/") {
                                has_image = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
            (text_len, has_image)
        }
        Some(Value::Object(map)) => {
            let text_len = map
                .get("text")
                .and_then(Value::as_str)
                .map_or(0, str::len);
            let has_image = map
                .get("type")
                .and_then(Value::as_str)
                .map_or(false, |kind| kind.contains("image"));
            (text_len, has_image)
        }
        _ => (0, false),
    }
}

fn extract_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    let candidates = ["/timestamp", "/created_at", "/time", "/ts", "/started_at"];
    for pointer in candidates {
        if let Some(ts) = value.pointer(pointer) {
            if let Some(parsed) = parse_timestamp(ts) {
                return Some(parsed);
            }
        }
    }
    None
}

fn parse_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    match value {
        Value::String(text) => DateTime::parse_from_rfc3339(text)
            .ok()
            .map(|dt| dt.with_timezone(&Utc)),
        Value::Number(num) => {
            let Some(raw) = num.as_i64() else {
                return None;
            };
            if raw >= 1_000_000_000_000 {
                let secs = raw / 1000;
                let nanos = (raw % 1000) * 1_000_000;
                DateTime::<Utc>::from_timestamp(secs, nanos as u32)
            } else {
                DateTime::<Utc>::from_timestamp(raw, 0)
            }
        }
        _ => None,
    }
}

fn extract_session_id(value: &Value) -> Option<String> {
    let pointers = [
        "/session_id",
        "/sessionId",
        "/session/id",
        "/session/uuid",
        "/session",
        "/conversation_id",
        "/conversationId",
        "/chat_id",
    ];
    for pointer in pointers {
        if let Some(Value::String(val)) = value.pointer(pointer) {
            if !val.trim().is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

fn extract_cwd(value: &Value) -> Option<String> {
    let pointers = [
        "/cwd",
        "/working_dir",
        "/workingDirectory",
        "/project_path",
        "/project/path",
    ];
    for pointer in pointers {
        if let Some(Value::String(val)) = value.pointer(pointer) {
            if !val.trim().is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

fn format_timestamp(timestamp: DateTime<Utc>) -> String {
    timestamp.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn deterministic_event_id(source: &str, event_type: &str, timestamp: &str, data: &str) -> String {
    let content = format!("{source}|{event_type}|{timestamp}|{data}");
    Uuid::new_v5(&Uuid::NAMESPACE_OID, content.as_bytes()).to_string()
}

fn find_claude_log_files(claude_projects_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let projects = match fs::read_dir(claude_projects_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(files),
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "failed to read Claude projects dir {}",
                    claude_projects_dir.display()
                )
            })
        }
    };

    for entry in projects {
        let entry = entry.context("failed to read Claude project entry")?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let sessions_dir = path.join("sessions");
        if !sessions_dir.is_dir() {
            continue;
        }
        for session in fs::read_dir(&sessions_dir)
            .with_context(|| format!("failed to read {}", sessions_dir.display()))?
        {
            let session = session.context("failed to read session entry")?;
            let session_path = session.path();
            if session_path
                .extension()
                .and_then(|ext| ext.to_str())
                == Some("jsonl")
            {
                files.push(session_path);
            }
        }
    }

    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Cursor;

    use serde_json::Value;
    use tempfile::TempDir;

    fn exporter_for_temp() -> (TempDir, Exporter) {
        let temp_dir = TempDir::new().expect("temp dir");
        let exporter = Exporter::from_home_dir(temp_dir.path());
        (temp_dir, exporter)
    }

    #[test]
    fn exports_buffer_and_claude_events() {
        let (temp_dir, exporter) = exporter_for_temp();
        let events_dir = temp_dir.path().join(".time-tracker");
        fs::create_dir_all(&events_dir).unwrap();
        fs::write(
            events_dir.join("events.jsonl"),
            r#"{"id":"abc","timestamp":"2025-01-01T00:00:00.000Z","type":"tmux_pane_focus","source":"remote.tmux","schema_version":1,"data":{"pane_id":"%1","session_name":"dev","cwd":"/repo"}}"#,
        )
        .unwrap();

        let session_dir = temp_dir
            .path()
            .join(".claude")
            .join("projects")
            .join("proj1")
            .join("sessions");
        fs::create_dir_all(&session_dir).unwrap();
        let log_path = session_dir.join("session-1.jsonl");
        let log_contents = r#"
{"type":"message","timestamp":"2025-01-01T00:10:00Z","session_id":"sess1","message":{"role":"user","content":"Hello"}}
{"type":"tool_use","timestamp":"2025-01-01T00:11:00Z","session_id":"sess1","tool":{"name":"Edit","file":"/repo/main.rs"}}
{"type":"session_start","timestamp":"2025-01-01T00:05:00Z","session_id":"sess1","cwd":"/repo"}
"#;
        fs::write(&log_path, log_contents.trim()).unwrap();

        let mut buffer = Cursor::new(Vec::new());
        exporter.export(&mut buffer).unwrap();
        let output = String::from_utf8(buffer.into_inner()).unwrap();
        let lines: Vec<Value> = output
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();

        assert_eq!(lines.len(), 4);
        assert!(lines.iter().any(|value| value["type"] == "tmux_pane_focus"));
        assert!(lines.iter().any(|value| value["type"] == "user_message"));
        assert!(lines.iter().any(|value| value["type"] == "agent_tool_use"));
        assert!(lines.iter().any(|value| value["type"] == "agent_session"));

        let user_message = lines
            .iter()
            .find(|value| value["type"] == "user_message")
            .unwrap();
        assert_eq!(user_message["source"], EVENT_SOURCE_REMOTE_AGENT);
        assert_eq!(user_message["data"]["length"], 5);

        let tool_use = lines
            .iter()
            .find(|value| value["type"] == "agent_tool_use")
            .unwrap();
        assert_eq!(tool_use["data"]["tool"], "Edit");
        assert_eq!(tool_use["data"]["file"], "/repo/main.rs");

        let session = lines
            .iter()
            .find(|value| value["type"] == "agent_session")
            .unwrap();
        assert_eq!(session["data"]["action"], "started");
        assert_eq!(session["data"]["cwd"], "/repo");
    }
}

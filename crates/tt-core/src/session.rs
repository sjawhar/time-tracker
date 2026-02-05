//! Coding assistant session indexing with performance optimizations.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use chrono::{DateTime, Utc};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Source of the coding session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionSource {
    #[default]
    Claude,
    #[serde(rename = "opencode")]
    OpenCode,
}

impl SessionSource {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::OpenCode => "opencode",
        }
    }
}

impl std::fmt::Display for SessionSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for SessionSource {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claude" => Ok(Self::Claude),
            "opencode" => Ok(Self::OpenCode),
            _ => Err(format!("invalid session source: {s}")),
        }
    }
}

/// Type of coding session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionType {
    /// Direct user session (UUID format, no agent- prefix)
    #[default]
    User,
    /// Background agent (`prompt_suggestion`, `compact`)
    Agent,
    /// Task tool subagent (agent-a{hash})
    Subagent,
}

impl SessionType {
    /// Derive session type from session ID.
    #[must_use]
    pub fn from_session_id(session_id: &str) -> Self {
        if !session_id.starts_with("agent-") {
            Self::User
        } else if session_id.contains("prompt_suggestion") || session_id.contains("compact") {
            Self::Agent
        } else {
            Self::Subagent
        }
    }

    /// Returns the string representation for SQL storage.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
            Self::Subagent => "subagent",
        }
    }
}

impl std::fmt::Display for SessionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for SessionType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user" => Ok(Self::User),
            "agent" => Ok(Self::Agent),
            "subagent" => Ok(Self::Subagent),
            _ => Err(format!("invalid session type: {s}")),
        }
    }
}

/// Buffer size for `BufReader` (64KB for optimal performance on large files)
const BUFFER_SIZE: usize = 64 * 1024;

/// Common jj workspace directory names.
const WORKSPACE_NAMES: &[&str] = &["default", "main", "dev", "feature", "master"];

/// Maximum number of user prompts to extract per session.
pub(crate) const MAX_USER_PROMPTS: usize = 5;

/// Maximum length of each user prompt (bytes). ~500 tokens, covers P90.
pub(crate) const MAX_PROMPT_LENGTH: usize = 2000;

/// Maximum number of user message timestamps to store per session.
/// Prevents unbounded memory growth for very long sessions.
pub(crate) const MAX_USER_MESSAGE_TIMESTAMPS: usize = 1000;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("no messages found in session")]
    NoMessages,
    #[error("no project path found in session")]
    NoProjectPath,
    #[error("invalid timestamp: {0} ms")]
    InvalidTimestamp(i64),
    #[error("empty session ID")]
    EmptySessionId,
}

/// An indexed coding assistant session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSession {
    pub session_id: String,
    /// Source tool (Claude Code or `OpenCode`).
    #[serde(default)]
    pub source: SessionSource,
    pub parent_session_id: Option<String>,
    /// Type of session (user, agent, subagent).
    #[serde(default)]
    pub session_type: SessionType,
    pub project_path: String,
    pub project_name: String,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pub message_count: i32,
    pub summary: Option<String>,
    /// First N user prompts (truncated to `MAX_PROMPT_LENGTH` bytes).
    #[serde(default)]
    pub user_prompts: Vec<String>,
    /// The first user prompt (starting prompt for the session).
    #[serde(default)]
    pub starting_prompt: Option<String>,
    /// Count of assistant messages.
    #[serde(default)]
    pub assistant_message_count: i32,
    /// Count of `tool_use` blocks in assistant messages.
    #[serde(default)]
    pub tool_call_count: i32,
    /// Timestamps of user messages (not tool results).
    #[serde(default)]
    pub user_message_timestamps: Vec<DateTime<Utc>>,
}

/// Minimal struct for typed deserialization (faster than `serde_json::Value`)
#[derive(Debug, Deserialize)]
struct MessageHeader {
    #[serde(rename = "type")]
    msg_type: Option<String>,
    cwd: Option<String>,
    summary: Option<String>,
    timestamp: Option<String>,
    message: Option<MessageContent>,
}

/// Message content for extracting user prompts.
#[derive(Debug, Deserialize)]
struct MessageContent {
    content: Option<MessageContentValue>,
}

/// Message content can be a string or an array of content blocks.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum MessageContentValue {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

/// A content block in a message (text, `tool_use`, etc.).
#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: Option<String>,
}

/// Check if a line might contain relevant data (pre-filter before JSON parse)
fn might_be_relevant(line: &str) -> bool {
    // Note: Also check without colon to handle JSON with whitespace like "type" : "value"
    line.contains("\"type\"") || line.contains("\"cwd\"")
}

/// Truncate a string to a maximum length, adding "..." if truncated.
pub(crate) fn truncate_prompt(content: &str) -> String {
    if content.len() <= MAX_PROMPT_LENGTH {
        return content.to_string();
    }

    // Find a safe UTF-8 boundary
    let mut end = MAX_PROMPT_LENGTH;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &content[..end])
}

/// Parse and track timestamp from a message header.
///
/// Returns the parsed timestamp if valid, also updating first/last tracking.
fn update_timestamps(
    header: &MessageHeader,
    first_timestamp: &mut Option<DateTime<Utc>>,
    last_timestamp: &mut Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    let ts = header
        .timestamp
        .as_ref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))?;

    first_timestamp.get_or_insert(ts);
    *last_timestamp = Some(ts);
    Some(ts)
}

/// Parse a Claude Code session JSONL file.
pub fn parse_session_file(
    path: &Path,
    session_id: &str,
    parent_session_id: Option<&str>,
) -> Result<AgentSession, SessionError> {
    let file = File::open(path)?;
    let reader = BufReader::with_capacity(BUFFER_SIZE, file);

    let mut message_count = 0i32;
    let mut assistant_message_count = 0i32;
    let mut tool_call_count = 0i32;
    let mut first_timestamp: Option<DateTime<Utc>> = None;
    let mut last_timestamp: Option<DateTime<Utc>> = None;
    let mut summary: Option<String> = None;
    let mut project_path: Option<String> = None;
    let mut user_prompts: Vec<String> = Vec::new();
    let mut starting_prompt: Option<String> = None;
    let mut user_message_timestamps: Vec<DateTime<Utc>> = Vec::new();

    for line in reader.lines() {
        let line = line?;

        if line.len() < 10 || !might_be_relevant(&line) {
            continue;
        }

        let header: MessageHeader = match serde_json::from_str(&line) {
            Ok(h) => h,
            Err(e) => {
                tracing::trace!(error = %e, "skipping malformed JSON line");
                continue;
            }
        };

        if project_path.is_none() {
            if let Some(ref cwd) = header.cwd {
                project_path = Some(cwd.clone());
            }
        }

        match header.msg_type.as_deref() {
            Some("summary") => {
                summary = header.summary;
            }
            Some("user") => {
                message_count = message_count.saturating_add(1);
                let parsed_ts =
                    update_timestamps(&header, &mut first_timestamp, &mut last_timestamp);
                // Extract user prompt content (up to MAX_USER_PROMPTS)
                // Only extract from string content (actual user prompts), not array content (tool results)
                if let Some(MessageContentValue::Text(text)) =
                    header.message.as_ref().and_then(|m| m.content.as_ref())
                {
                    if !text.is_empty() {
                        if starting_prompt.is_none() {
                            starting_prompt = Some(truncate_prompt(text));
                        }
                        if user_prompts.len() < MAX_USER_PROMPTS {
                            user_prompts.push(truncate_prompt(text));
                        }
                        // Capture timestamp for user message events (bounded to prevent unbounded growth)
                        if let Some(ts) = parsed_ts {
                            if user_message_timestamps.len() < MAX_USER_MESSAGE_TIMESTAMPS {
                                user_message_timestamps.push(ts);
                            }
                        }
                    }
                }
            }
            Some("assistant") => {
                message_count = message_count.saturating_add(1);
                assistant_message_count = assistant_message_count.saturating_add(1);
                let _ = update_timestamps(&header, &mut first_timestamp, &mut last_timestamp);
                // Count tool_use blocks in assistant message content
                if let Some(MessageContentValue::Blocks(blocks)) =
                    header.message.as_ref().and_then(|m| m.content.as_ref())
                {
                    let count = blocks
                        .iter()
                        .filter(|b| b.block_type.as_deref() == Some("tool_use"))
                        .count();
                    // Safe: tool_use count per message won't exceed i32::MAX
                    tool_call_count =
                        tool_call_count.saturating_add(i32::try_from(count).unwrap_or(i32::MAX));
                }
            }
            _ => {}
        }
    }

    let start_time = first_timestamp.ok_or(SessionError::NoMessages)?;
    let project_path = project_path.ok_or(SessionError::NoProjectPath)?;

    Ok(AgentSession {
        session_id: session_id.to_string(),
        source: SessionSource::Claude,
        parent_session_id: parent_session_id.map(String::from),
        session_type: SessionType::from_session_id(session_id),
        project_name: extract_project_name(&project_path),
        project_path,
        start_time,
        end_time: if last_timestamp == first_timestamp {
            None
        } else {
            last_timestamp
        },
        message_count,
        summary,
        user_prompts,
        starting_prompt,
        assistant_message_count,
        tool_call_count,
        user_message_timestamps,
    })
}

/// Extract project name from path.
pub(crate) fn extract_project_name(path: &str) -> String {
    let path_obj = Path::new(path);
    let basename = path_obj
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    if WORKSPACE_NAMES.contains(&basename) {
        path_obj
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or(basename)
            .to_string()
    } else {
        basename.to_string()
    }
}

#[derive(Debug)]
struct SessionFile {
    path: std::path::PathBuf,
    session_id: String,
    parent_session_id: Option<String>,
}

/// Scan Claude Code projects directory and build session index.
pub fn scan_claude_sessions(projects_dir: &Path) -> Result<Vec<AgentSession>, SessionError> {
    if !projects_dir.exists() {
        return Ok(Vec::new());
    }

    let mut session_files: Vec<SessionFile> = Vec::new();

    for project_entry in std::fs::read_dir(projects_dir)? {
        let project_entry = project_entry?;
        let project_path = project_entry.path();

        if !project_path.is_dir() {
            continue;
        }

        for session_entry in std::fs::read_dir(&project_path)? {
            let session_entry = session_entry?;
            let session_path = session_entry.path();

            if session_path.is_file() && session_path.extension().is_some_and(|e| e == "jsonl") {
                let session_id = session_path
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();

                // Skip files with empty session IDs to prevent invalid event ID generation
                if session_id.is_empty() {
                    tracing::warn!(path = ?session_path, "skipping session file with empty session ID");
                    continue;
                }

                session_files.push(SessionFile {
                    path: session_path,
                    session_id,
                    parent_session_id: None,
                });
            } else if session_path.is_dir() {
                let subagents_dir = session_path.join("subagents");
                if subagents_dir.exists() {
                    let parent_session_id = session_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(String::from);

                    if let Ok(subagent_entries) = std::fs::read_dir(&subagents_dir) {
                        for subagent_entry in subagent_entries.flatten() {
                            let subagent_path = subagent_entry.path();

                            if subagent_path.is_file()
                                && subagent_path.extension().is_some_and(|e| e == "jsonl")
                            {
                                let session_id = subagent_path
                                    .file_stem()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("")
                                    .to_string();

                                // Skip files with empty session IDs to prevent invalid event ID generation
                                if session_id.is_empty() {
                                    tracing::warn!(path = ?subagent_path, "skipping subagent session file with empty session ID");
                                    continue;
                                }

                                session_files.push(SessionFile {
                                    path: subagent_path,
                                    session_id,
                                    parent_session_id: parent_session_id.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    let mut entries: Vec<AgentSession> = session_files
        .par_iter()
        .filter_map(|sf| {
            match parse_session_file(&sf.path, &sf.session_id, sf.parent_session_id.as_deref()) {
                Ok(entry) => Some(entry),
                Err(e) => {
                    tracing::warn!(path = ?sf.path, error = %e, "skipping invalid session");
                    None
                }
            }
        })
        .collect();

    entries.sort_by_key(|e| e.start_time);
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_parse_session_extracts_cwd_and_summary() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"hello"}},"timestamp":"2026-01-29T10:58:45.000Z","cwd":"/home/sami/time-tracker/default"}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"role":"assistant","content":"hi"}},"timestamp":"2026-01-29T10:59:00.000Z"}}"#).unwrap();
        writeln!(
            file,
            r#"{{"type":"summary","summary":"Implementing export command","leafUuid":"abc"}}"#
        )
        .unwrap();

        let entry = parse_session_file(file.path(), "test-session", None).unwrap();

        assert_eq!(entry.project_path, "/home/sami/time-tracker/default");
        assert_eq!(
            entry.summary.as_deref(),
            Some("Implementing export command")
        );
        assert_eq!(entry.message_count, 2);
        assert_eq!(entry.user_prompts, vec!["hello"]);
    }

    #[test]
    fn test_parse_session_extracts_user_prompts() {
        let mut file = NamedTempFile::new().unwrap();
        // First user message
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"implement feature X"}},"timestamp":"2026-01-29T10:58:45.000Z","cwd":"/home/sami/project"}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"role":"assistant","content":"I'll implement feature X."}},"timestamp":"2026-01-29T10:59:00.000Z"}}"#).unwrap();
        // Second user message
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"add tests"}},"timestamp":"2026-01-29T11:00:00.000Z"}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"role":"assistant","content":"Adding tests."}},"timestamp":"2026-01-29T11:01:00.000Z"}}"#).unwrap();
        // Third user message
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"fix the bug"}},"timestamp":"2026-01-29T11:02:00.000Z"}}"#).unwrap();

        let entry = parse_session_file(file.path(), "test-session", None).unwrap();

        assert_eq!(entry.user_prompts.len(), 3);
        assert_eq!(entry.user_prompts[0], "implement feature X");
        assert_eq!(entry.user_prompts[1], "add tests");
        assert_eq!(entry.user_prompts[2], "fix the bug");
    }

    #[test]
    fn test_parse_session_extracts_user_message_timestamps() {
        let mut file = NamedTempFile::new().unwrap();
        // First user message
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"first message"}},"timestamp":"2026-01-29T10:00:00.000Z","cwd":"/home/sami/project"}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"role":"assistant","content":"response"}},"timestamp":"2026-01-29T10:01:00.000Z"}}"#).unwrap();
        // Second user message
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"second message"}},"timestamp":"2026-01-29T10:05:00.000Z"}}"#).unwrap();
        // Tool result (should NOT capture timestamp)
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"123","content":"result"}}]}},"timestamp":"2026-01-29T10:06:00.000Z"}}"#).unwrap();
        // Third user message
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"third message"}},"timestamp":"2026-01-29T10:10:00.000Z"}}"#).unwrap();

        let entry = parse_session_file(file.path(), "test-session", None).unwrap();

        // Should have 3 timestamps (only actual user messages, not tool results)
        assert_eq!(entry.user_message_timestamps.len(), 3);
        assert_eq!(
            entry.user_message_timestamps[0],
            DateTime::parse_from_rfc3339("2026-01-29T10:00:00.000Z")
                .unwrap()
                .with_timezone(&Utc)
        );
        assert_eq!(
            entry.user_message_timestamps[1],
            DateTime::parse_from_rfc3339("2026-01-29T10:05:00.000Z")
                .unwrap()
                .with_timezone(&Utc)
        );
        assert_eq!(
            entry.user_message_timestamps[2],
            DateTime::parse_from_rfc3339("2026-01-29T10:10:00.000Z")
                .unwrap()
                .with_timezone(&Utc)
        );
    }

    #[test]
    fn test_parse_session_limits_user_prompts() {
        let mut file = NamedTempFile::new().unwrap();
        // Write more than MAX_USER_PROMPTS (5) user messages
        for i in 0..10 {
            writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"prompt {i}"}},"timestamp":"2026-01-29T10:{i:02}:00.000Z","cwd":"/home/sami/project"}}"#).unwrap();
        }

        let entry = parse_session_file(file.path(), "test-session", None).unwrap();

        assert_eq!(entry.user_prompts.len(), 5); // MAX_USER_PROMPTS
        assert_eq!(entry.user_prompts[0], "prompt 0");
        assert_eq!(entry.user_prompts[4], "prompt 4");
    }

    #[test]
    fn test_parse_session_truncates_long_prompts() {
        let mut file = NamedTempFile::new().unwrap();
        // Create a very long prompt (> 2000 chars)
        let long_prompt = "x".repeat(3000);
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"{long_prompt}"}},"timestamp":"2026-01-29T10:00:00.000Z","cwd":"/home/sami/project"}}"#).unwrap();

        let entry = parse_session_file(file.path(), "test-session", None).unwrap();

        assert_eq!(entry.user_prompts.len(), 1);
        assert!(entry.user_prompts[0].len() <= 2003); // MAX_PROMPT_LENGTH + "..."
        assert!(entry.user_prompts[0].ends_with("..."));
    }

    #[test]
    fn test_parse_session_ignores_user_content_blocks() {
        let mut file = NamedTempFile::new().unwrap();
        // User message with content blocks array (tool results) should NOT be counted as user prompts
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"123","content":"result"}}]}},"timestamp":"2026-01-29T10:00:00.000Z","cwd":"/home/sami/project"}}"#).unwrap();
        // But this string content should be captured
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"actual user prompt"}},"timestamp":"2026-01-29T10:01:00.000Z"}}"#).unwrap();

        let entry = parse_session_file(file.path(), "test-session", None).unwrap();

        // Only the string content should be captured as a user prompt
        assert_eq!(entry.user_prompts.len(), 1);
        assert_eq!(entry.user_prompts[0], "actual user prompt");
        assert_eq!(entry.starting_prompt.as_deref(), Some("actual user prompt"));
    }

    #[test]
    fn test_extract_project_name_from_workspace_path() {
        assert_eq!(
            extract_project_name("/home/sami/time-tracker/default"),
            "time-tracker"
        );
        assert_eq!(extract_project_name("/home/sami/pivot/main"), "pivot");
        assert_eq!(extract_project_name("/home/sami/.dotfiles"), ".dotfiles");
    }

    #[test]
    fn test_parse_session_counts_assistant_messages() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"hello"}},"timestamp":"2026-01-29T10:00:00.000Z","cwd":"/home/sami/project"}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"role":"assistant","content":"hi"}},"timestamp":"2026-01-29T10:01:00.000Z"}}"#).unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"thanks"}},"timestamp":"2026-01-29T10:02:00.000Z"}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"role":"assistant","content":"you're welcome"}},"timestamp":"2026-01-29T10:03:00.000Z"}}"#).unwrap();

        let entry = parse_session_file(file.path(), "test-session", None).unwrap();

        assert_eq!(entry.message_count, 4);
        assert_eq!(entry.assistant_message_count, 2);
    }

    #[test]
    fn test_parse_session_counts_tool_calls() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"read file"}},"timestamp":"2026-01-29T10:00:00.000Z","cwd":"/home/sami/project"}}"#).unwrap();
        // Assistant message with tool_use blocks
        writeln!(file, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"I'll read the file."}},{{"type":"tool_use","id":"123","name":"Read","input":{{"file_path":"/test.txt"}}}},{{"type":"tool_use","id":"456","name":"Grep","input":{{"pattern":"test"}}}}]}},"timestamp":"2026-01-29T10:01:00.000Z"}}"#).unwrap();
        // Tool results (user message with array content)
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"123","content":"file contents"}}]}},"timestamp":"2026-01-29T10:02:00.000Z"}}"#).unwrap();
        // Another assistant message with one tool call
        writeln!(file, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"789","name":"Edit","input":{{}}}}]}},"timestamp":"2026-01-29T10:03:00.000Z"}}"#).unwrap();

        let entry = parse_session_file(file.path(), "test-session", None).unwrap();

        assert_eq!(entry.assistant_message_count, 2);
        assert_eq!(entry.tool_call_count, 3); // 2 + 1 tool_use blocks
    }

    #[test]
    fn test_parse_session_sets_starting_prompt() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"first prompt"}},"timestamp":"2026-01-29T10:00:00.000Z","cwd":"/home/sami/project"}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"role":"assistant","content":"response"}},"timestamp":"2026-01-29T10:01:00.000Z"}}"#).unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"second prompt"}},"timestamp":"2026-01-29T10:02:00.000Z"}}"#).unwrap();

        let entry = parse_session_file(file.path(), "test-session", None).unwrap();

        assert_eq!(entry.starting_prompt.as_deref(), Some("first prompt"));
        assert_eq!(entry.user_prompts.len(), 2);
    }

    #[test]
    fn test_parse_session_starting_prompt_skips_tool_results() {
        let mut file = NamedTempFile::new().unwrap();
        // Tool result first (should NOT be starting prompt)
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"123","content":"result"}}]}},"timestamp":"2026-01-29T10:00:00.000Z","cwd":"/home/sami/project"}}"#).unwrap();
        // Then actual user prompt
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"actual first prompt"}},"timestamp":"2026-01-29T10:01:00.000Z"}}"#).unwrap();

        let entry = parse_session_file(file.path(), "test-session", None).unwrap();

        assert_eq!(
            entry.starting_prompt.as_deref(),
            Some("actual first prompt")
        );
    }

    #[test]
    fn test_parse_session_incomplete_json_line() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"hello"}},"timestamp":"2026-01-29T10:00:00Z","cwd":"/home/sami/project"}}"#).unwrap();
        // Incomplete line at end (simulates file being written to)
        write!(
            file,
            r#"{{"type":"user","message":{{"role":"user","content":"incomplete"#
        )
        .unwrap();
        file.flush().unwrap();

        // Should parse the complete line and skip the incomplete one
        let entry = parse_session_file(file.path(), "test-session", None).unwrap();
        assert_eq!(entry.message_count, 1);
        assert_eq!(entry.user_prompts.len(), 1);
    }

    #[test]
    fn test_parse_session_empty_file() {
        let file = NamedTempFile::new().unwrap();
        // Empty file should error (no messages)
        let result = parse_session_file(file.path(), "test-session", None);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), SessionError::NoMessages));
    }

    #[test]
    fn test_parse_session_only_non_user_messages() {
        let mut file = NamedTempFile::new().unwrap();
        // Only system messages, no user messages
        writeln!(
            file,
            r#"{{"type":"summary","summary":"Test summary","leafUuid":"abc"}}"#
        )
        .unwrap();

        let result = parse_session_file(file.path(), "test-session", None);
        // Should error because no user messages means no timestamps
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_session_missing_cwd() {
        let mut file = NamedTempFile::new().unwrap();
        // Message without cwd field
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"hello"}},"timestamp":"2026-01-29T10:00:00Z"}}"#).unwrap();

        let result = parse_session_file(file.path(), "test-session", None);
        // Should error (NoProjectPath)
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), SessionError::NoProjectPath));
    }

    #[test]
    fn test_parse_session_invalid_timestamp() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"hello"}},"timestamp":"not-a-timestamp","cwd":"/test"}}"#).unwrap();

        // Should parse but skip the invalid timestamp
        let result = parse_session_file(file.path(), "test-session", None);
        // Will error with NoMessages because timestamp is required
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_session_with_parent_id() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"hello"}},"timestamp":"2026-01-29T10:00:00Z","cwd":"/test"}}"#).unwrap();

        let entry =
            parse_session_file(file.path(), "child-session", Some("parent-session")).unwrap();
        assert_eq!(entry.session_id, "child-session");
        assert_eq!(entry.parent_session_id.as_deref(), Some("parent-session"));
    }

    #[test]
    fn test_parse_session_empty_user_prompt_ignored() {
        let mut file = NamedTempFile::new().unwrap();
        // User message with empty content should not be added to user_prompts
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":""}},"timestamp":"2026-01-29T10:00:00Z","cwd":"/test"}}"#).unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"actual prompt"}},"timestamp":"2026-01-29T10:01:00Z"}}"#).unwrap();

        let entry = parse_session_file(file.path(), "test-session", None).unwrap();
        assert_eq!(entry.message_count, 2);
        assert_eq!(entry.user_prompts.len(), 1);
        assert_eq!(entry.user_prompts[0], "actual prompt");
        assert_eq!(entry.starting_prompt.as_deref(), Some("actual prompt"));
    }

    #[test]
    fn test_parse_session_message_count_saturation() {
        use std::io::Write;

        let mut file = NamedTempFile::new().unwrap();
        // First user message for timestamps/cwd
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"hello"}},"timestamp":"2026-01-29T10:00:00Z","cwd":"/test"}}"#).unwrap();

        // Can't easily test i32::MAX overflow without creating a huge file,
        // but we verify saturating_add is used in the code

        let entry = parse_session_file(file.path(), "test-session", None).unwrap();
        assert_eq!(entry.message_count, 1);
    }

    #[test]
    fn test_scan_claude_sessions_nonexistent_dir() {
        use std::path::PathBuf;

        let nonexistent = PathBuf::from("/nonexistent/directory/that/does/not/exist");
        let result = scan_claude_sessions(&nonexistent).unwrap();

        // Should return empty vec for nonexistent directory
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_scan_claude_sessions_with_subagents() {
        use std::io::Write;
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let projects_dir = temp.path();

        // Create project structure with subagents
        let test_project_dir = projects_dir.join("test-project");
        std::fs::create_dir_all(&test_project_dir).unwrap();

        // Parent session as a directory
        let parent_session_dir = test_project_dir.join("parent-session-id");
        let subagents_dir = parent_session_dir.join("subagents");
        std::fs::create_dir_all(&subagents_dir).unwrap();

        // Create subagent session file
        let subagent_file = subagents_dir.join("subagent-1.jsonl");
        let mut file = std::fs::File::create(&subagent_file).unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"subagent task"}},"timestamp":"2026-01-29T10:00:00Z","cwd":"/test"}}"#).unwrap();

        let sessions = scan_claude_sessions(projects_dir).unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "subagent-1");
        assert_eq!(
            sessions[0].parent_session_id.as_deref(),
            Some("parent-session-id")
        );
    }

    #[test]
    fn test_scan_claude_sessions_mixed_files_and_dirs() {
        use std::io::Write;
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let projects_dir = temp.path();

        let test_project_dir = projects_dir.join("test-project");
        std::fs::create_dir_all(&test_project_dir).unwrap();

        // Regular session file
        let session_file = test_project_dir.join("regular-session.jsonl");
        let mut file = std::fs::File::create(&session_file).unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"hello"}},"timestamp":"2026-01-29T10:00:00Z","cwd":"/test"}}"#).unwrap();

        // Session directory without subagents (should be ignored)
        let empty_dir = test_project_dir.join("empty-session-dir");
        std::fs::create_dir(&empty_dir).unwrap();

        let sessions = scan_claude_sessions(projects_dir).unwrap();

        // Should only find the regular session file
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "regular-session");
        assert!(sessions[0].parent_session_id.is_none());
    }

    #[test]
    fn test_extract_project_name_edge_cases() {
        // Root directory - file_name() returns None, falls back to "unknown"
        assert_eq!(extract_project_name("/"), "unknown");

        // Single component path
        assert_eq!(extract_project_name("project"), "project");

        // Workspace name without parent - "/default" has file_name "default" which is a workspace name,
        // parent is "/", which has no file_name, so falls back to basename ("default")
        assert_eq!(extract_project_name("/default"), "default");

        // Multiple workspace indicators in path - "main" is a workspace name,
        // so it looks at parent "default" which is also a workspace name, returns "default"
        assert_eq!(extract_project_name("/home/user/default/main"), "default");
    }

    #[test]
    fn test_truncate_prompt_utf8_boundary() {
        // Test that truncation respects UTF-8 boundaries
        let emoji_string = "😀".repeat(1000); // Multi-byte UTF-8 characters
        let truncated = truncate_prompt(&emoji_string);

        // Should truncate without panic and end with ...
        assert!(truncated.ends_with("..."));
        assert!(truncated.len() <= MAX_PROMPT_LENGTH + 3); // +3 for "..."

        // Verify it's still valid UTF-8
        assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
    }

    #[test]
    fn test_parse_session_assistant_without_content() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"hello"}},"timestamp":"2026-01-29T10:00:00Z","cwd":"/test"}}"#).unwrap();
        // Assistant message without content field
        writeln!(file, r#"{{"type":"assistant","message":{{"role":"assistant"}},"timestamp":"2026-01-29T10:01:00Z"}}"#).unwrap();

        let entry = parse_session_file(file.path(), "test-session", None).unwrap();

        assert_eq!(entry.assistant_message_count, 1);
        assert_eq!(entry.tool_call_count, 0); // No content means no tool calls
    }

    #[test]
    fn test_session_type_from_user_session() {
        let session_id = "d66718b7-3b37-47c8-b3a6-f01b637d8c13";
        assert_eq!(SessionType::from_session_id(session_id), SessionType::User);
    }

    #[test]
    fn test_session_type_from_prompt_suggestion_agent() {
        let session_id = "agent-aprompt_suggestion-05a0b3";
        assert_eq!(SessionType::from_session_id(session_id), SessionType::Agent);
    }

    #[test]
    fn test_session_type_from_compact_agent() {
        let session_id = "agent-acompact-63da16";
        assert_eq!(SessionType::from_session_id(session_id), SessionType::Agent);
    }

    #[test]
    fn test_session_type_from_task_subagent() {
        let session_id = "agent-a913a65";
        assert_eq!(
            SessionType::from_session_id(session_id),
            SessionType::Subagent
        );
    }

    #[test]
    fn test_session_type_roundtrip() {
        for st in [SessionType::User, SessionType::Agent, SessionType::Subagent] {
            let s = st.as_str();
            let parsed: SessionType = s.parse().unwrap();
            assert_eq!(parsed, st);
        }
    }

    #[test]
    fn test_session_source_roundtrip() {
        for src in [SessionSource::Claude, SessionSource::OpenCode] {
            let s = src.as_str();
            let parsed: SessionSource = s.parse().unwrap();
            assert_eq!(parsed, src);
            assert_eq!(src.to_string(), s);
        }
    }

    #[test]
    fn test_session_source_serde_matches_as_str() {
        // Verify serde serialization produces the same string as as_str().
        // This prevents inconsistency between JSON export and DB storage.
        for src in [SessionSource::Claude, SessionSource::OpenCode] {
            let serde_value = serde_json::to_value(src).unwrap();
            assert_eq!(
                serde_value.as_str().unwrap(),
                src.as_str(),
                "serde serialization of {src:?} should match as_str()"
            );
        }
    }

    #[test]
    fn test_session_type_serde_matches_as_str() {
        // Verify serde serialization produces the same string as as_str().
        for st in [SessionType::User, SessionType::Agent, SessionType::Subagent] {
            let serde_value = serde_json::to_value(st).unwrap();
            assert_eq!(
                serde_value.as_str().unwrap(),
                st.as_str(),
                "serde serialization of {st:?} should match as_str()"
            );
        }
    }

    #[test]
    fn test_session_source_invalid() {
        let result = "invalid".parse::<SessionSource>();
        assert!(result.is_err());
    }
}

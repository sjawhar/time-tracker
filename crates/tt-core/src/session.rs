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
    #[serde(rename = "omp")]
    Omp,
}

impl SessionSource {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::OpenCode => "opencode",
            Self::Omp => "omp",
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
            "omp" => Ok(Self::Omp),
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

pub(crate) const MAX_TOOL_CALLS_PER_MESSAGE: usize = 100;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    /// The session's storage was never written to.
    ///
    /// An agent session that is aborted before its first message leaves an
    /// empty file (Claude) or a shard holding no `message` table
    /// (`OpenCode`). Nothing was lost and nothing can be parsed, so this is an
    /// expected condition rather than a failure: `scan_claude_sessions` and
    /// `scan_opencode_sessions` log it at `debug` while every other variant
    /// still warns.
    #[error("session is empty (never written to)")]
    EmptySession,
    /// The file holds records, none of which is a message — a session that only
    /// ever fired a `SessionStart` hook is the common case. Every line parsed;
    /// there is simply nothing to index. Expected, so it is logged at `debug`
    /// alongside [`SessionError::EmptySession`].
    #[error("session holds no message records")]
    NoMessageRecords,
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
    /// Timestamps of agent tool calls (for delegated time computation).
    #[serde(default)]
    pub tool_call_timestamps: Vec<DateTime<Utc>>,
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
#[expect(
    clippy::too_many_lines,
    reason = "Session parser keeps the IO loop in one function for clarity"
)]
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
    let mut tool_call_timestamps: Vec<DateTime<Utc>> = Vec::new();
    // Three states, not two. An aborted session leaves an empty file; a session
    // that only ever fired hooks leaves `progress` records that are correctly
    // skipped as irrelevant; a session whose lines fail to parse is a genuine
    // defect. Only the last warrants a warning — the first two are expected and
    // recur on every ~30s ingest, so warning about them fills the daemon log.
    let mut saw_content = false;
    let mut saw_parse_failure = false;

    for line in reader.lines() {
        let line = line?;
        saw_content |= !line.is_empty();

        if line.len() < 10 || !might_be_relevant(&line) {
            continue;
        }

        let header: MessageHeader = match serde_json::from_str(&line) {
            Ok(h) => h,
            Err(e) => {
                saw_parse_failure = true;
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
                let parsed_ts =
                    update_timestamps(&header, &mut first_timestamp, &mut last_timestamp);
                // Only string content is an actual user prompt; array content
                // carries tool results.
                let text = match header.message.as_ref().and_then(|m| m.content.as_ref()) {
                    Some(MessageContentValue::Text(text)) => Some(text.as_str()),
                    Some(MessageContentValue::Blocks(_)) | None => None,
                };
                if text.is_some_and(crate::injection::is_injected) {
                    // Injected text is the harness talking to the agent, not a
                    // person: it must not count as a message, a prompt, or a
                    // moment of attention. The timestamps above still advanced —
                    // the session was alive, just unattended.
                    continue;
                }
                message_count = message_count.saturating_add(1);
                if let Some(text) = text {
                    if !text.is_empty() {
                        if starting_prompt.is_none() {
                            starting_prompt = Some(truncate_prompt(text));
                        }
                        if user_prompts.len() < MAX_USER_PROMPTS {
                            user_prompts.push(truncate_prompt(text));
                        }
                        // Bounded to prevent unbounded growth on long sessions.
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
                let parsed_ts =
                    update_timestamps(&header, &mut first_timestamp, &mut last_timestamp);
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
                    if let Some(ts) = parsed_ts {
                        let capped_count = count.min(MAX_TOOL_CALLS_PER_MESSAGE);
                        tool_call_timestamps.extend(std::iter::repeat_n(ts, capped_count));
                    }
                }
            }
            _ => {}
        }
    }

    let start_time = first_timestamp.ok_or(match (saw_content, saw_parse_failure) {
        (_, true) => SessionError::NoMessages,
        (true, false) => SessionError::NoMessageRecords,
        (false, false) => SessionError::EmptySession,
    })?;
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
        tool_call_timestamps,
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

/// One transcript file's contribution to a scan.
///
/// `clean` is false only for a *defective* file. A file that simply held nothing to
/// index is clean: that is an expected state, and treating it as a defect would hold
/// the scan cursor still forever.
pub(crate) struct ParsedFile {
    pub(crate) session: Option<AgentSession>,
    pub(crate) clean: bool,
}

impl ParsedFile {
    pub(crate) const NOTHING_TO_INDEX: Self = Self {
        session: None,
        clean: true,
    };
    pub(crate) const DEFECTIVE: Self = Self {
        session: None,
        clean: false,
    };

    pub(crate) const fn yielded(session: AgentSession) -> Self {
        Self {
            session: Some(session),
            clean: true,
        }
    }
}

/// Whether a transcript file may be skipped because it has not changed since `since`.
///
/// Reads mtime off the directory entry the walk already holds, so a skipped file is
/// never opened. A metadata read that fails answers `false`: when the filter cannot
/// be evaluated, parse the file.
pub(crate) fn unchanged_since(entry: &std::fs::DirEntry, since: Option<DateTime<Utc>>) -> bool {
    let Some(since) = since else {
        return false;
    };
    entry
        .metadata()
        .and_then(|meta| meta.modified())
        .is_ok_and(|modified| DateTime::<Utc>::from(modified) <= since)
}

/// What one scan of a transcript store found, and whether it read the whole store.
///
/// `complete` exists so an incremental caller can tell "nothing changed" apart from
/// "I could not read it". Both look like an empty session list, but only the first
/// permits advancing a scan cursor: advancing on the second skips that window of
/// sessions permanently, because the next pass would ask for a later `since`.
///
/// A scan is incomplete when the store was present but some part of it could not be
/// read — an unopenable database, a failed query, or a session file the parser
/// rejected as defective. An **absent** store is complete (most machines run only one
/// harness), and so is a session skipped as empty, which is an expected state rather
/// than a defect.
#[derive(Debug)]
pub struct ScanOutcome {
    pub sessions: Vec<AgentSession>,
    pub complete: bool,
}

impl ScanOutcome {
    /// A scan that read everything it was asked to read.
    #[must_use]
    pub const fn complete(sessions: Vec<AgentSession>) -> Self {
        Self {
            sessions,
            complete: true,
        }
    }
}

/// Scan Claude Code projects directory and build session index.
///
/// Reads every session file. Callers on the ~30s ingest path want
/// [`scan_claude_sessions_incremental`] instead.
pub fn scan_claude_sessions(projects_dir: &Path) -> Result<Vec<AgentSession>, SessionError> {
    Ok(scan_claude_sessions_incremental(projects_dir, None)?.sessions)
}

/// Scan Claude Code sessions, optionally skipping files unmodified since `since`.
///
/// The filter is the file's **mtime**, taken from the directory entry, so a skipped
/// file is never opened or parsed. That is sound for the same reason the byte-offset
/// manifest in `tt export` is: a transcript is written by appending, and every write
/// on this platform advances mtime. It is deliberately paired with a safety overlap
/// and a force-full flag at the call site rather than trusted on its own — see
/// `tt_cli::commands::ingest`.
///
/// A file whose mtime cannot be read is parsed rather than skipped: the cheap answer
/// when the filter cannot be evaluated is to do the work.
pub fn scan_claude_sessions_incremental(
    projects_dir: &Path,
    since: Option<DateTime<Utc>>,
) -> Result<ScanOutcome, SessionError> {
    if !projects_dir.exists() {
        return Ok(ScanOutcome::complete(Vec::new()));
    }

    let mut complete = true;
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

                if unchanged_since(&session_entry, since) {
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

                                if unchanged_since(&subagent_entry, since) {
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

    let parsed: Vec<ParsedFile> = session_files
        .par_iter()
        .map(|sf| {
            match parse_session_file(&sf.path, &sf.session_id, sf.parent_session_id.as_deref()) {
                Ok(entry) => ParsedFile::yielded(entry),
                Err(e @ (SessionError::EmptySession | SessionError::NoMessageRecords)) => {
                    tracing::debug!(path = ?sf.path, error = %e, "skipping session with nothing to index");
                    ParsedFile::NOTHING_TO_INDEX
                }
                Err(e) => {
                    tracing::warn!(path = ?sf.path, error = %e, "skipping invalid session");
                    ParsedFile::DEFECTIVE
                }
            }
        })
        .collect();

    complete &= parsed.iter().all(|file| file.clean);
    let mut entries: Vec<AgentSession> = parsed.into_iter().filter_map(|f| f.session).collect();

    entries.sort_by_key(|e| e.start_time);
    Ok(ScanOutcome {
        sessions: entries,
        complete,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    use chrono::TimeZone;
    use std::time::{Duration as StdDuration, SystemTime};

    /// Writes a minimal but parseable Claude session file and stamps its mtime.
    fn plant_claude_session(projects_dir: &Path, session_id: &str, modified: SystemTime) {
        let project = projects_dir.join("-home-sami-proj");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join(format!("{session_id}.jsonl"));
        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(
            file,
            r#"{{"type":"user","message":{{"role":"user","content":"hello"}},"timestamp":"2026-01-29T10:58:45.000Z","cwd":"/home/sami/proj"}}"#
        )
        .unwrap();
        file.sync_all().unwrap();
        drop(file);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
    }

    /// Given a session file older than the cursor, When an incremental scan runs,
    /// Then it is not re-derived — that skip is the whole performance fix.
    #[test]
    fn claude_incremental_skips_files_older_than_since() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        let long_ago = SystemTime::UNIX_EPOCH + StdDuration::from_secs(1_700_000_000);
        plant_claude_session(&projects, "stale-session", long_ago);
        let since = Utc.timestamp_opt(1_800_000_000, 0).unwrap();

        let outcome = scan_claude_sessions_incremental(&projects, Some(since)).unwrap();

        assert!(
            outcome.sessions.is_empty(),
            "stale file must not be re-parsed"
        );
        assert!(outcome.complete, "skipping a stale file is a complete scan");
    }

    /// Given a session file newer than the cursor, When an incremental scan runs,
    /// Then it is re-derived.
    #[test]
    fn claude_incremental_reparses_files_newer_than_since() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        let recent = SystemTime::UNIX_EPOCH + StdDuration::from_secs(1_900_000_000);
        plant_claude_session(&projects, "fresh-session", recent);
        let since = Utc.timestamp_opt(1_800_000_000, 0).unwrap();

        let outcome = scan_claude_sessions_incremental(&projects, Some(since)).unwrap();

        assert_eq!(outcome.sessions.len(), 1);
        assert_eq!(outcome.sessions[0].session_id, "fresh-session");
    }

    /// Given no cursor, When a scan runs, Then every file is derived regardless of
    /// age — this is the force-full and first-run path.
    #[test]
    fn claude_incremental_without_since_reads_everything() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        let long_ago = SystemTime::UNIX_EPOCH + StdDuration::from_secs(1_700_000_000);
        plant_claude_session(&projects, "stale-session", long_ago);

        let outcome = scan_claude_sessions_incremental(&projects, None).unwrap();

        assert_eq!(outcome.sessions.len(), 1);
        assert!(outcome.complete);
    }

    /// Given a store that is simply absent, When a scan runs, Then it is complete.
    ///
    /// Most machines run only one of the two harnesses; an absent store is not a
    /// failure and must not freeze the cursor forever.
    #[test]
    fn claude_absent_store_is_a_complete_scan() {
        let dir = tempfile::tempdir().unwrap();

        let outcome = scan_claude_sessions_incremental(&dir.path().join("nope"), None).unwrap();

        assert!(outcome.sessions.is_empty());
        assert!(outcome.complete);
    }

    /// Given a file the parser rejects as defective, When a scan runs, Then the scan
    /// reports itself incomplete.
    ///
    /// The session is skipped either way, but the cursor must not advance past it:
    /// under a full scan every tick retried it, and an incremental scan that advanced
    /// would strand it forever.
    #[test]
    fn claude_unparseable_session_marks_scan_incomplete() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        let project = projects.join("-home-sami-proj");
        std::fs::create_dir_all(&project).unwrap();
        let mut file = std::fs::File::create(project.join("broken.jsonl")).unwrap();
        // Message-shaped enough to reach the deserializer, malformed enough to fail.
        writeln!(file, r#"{{"type":"user","cwd":"/x","message":{{"#).unwrap();
        drop(file);

        let outcome = scan_claude_sessions_incremental(&projects, None).unwrap();

        assert!(outcome.sessions.is_empty());
        assert!(
            !outcome.complete,
            "a defective session must hold the cursor back"
        );
    }

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
    fn test_parse_session_extracts_tool_call_timestamps() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"run tool"}},"timestamp":"2026-01-29T10:00:00.000Z","cwd":"/home/sami/project"}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"123","name":"Read","input":{{"file_path":"/test.txt"}}}},{{"type":"tool_use","id":"456","name":"Grep","input":{{"pattern":"test"}}}}]}},"timestamp":"2026-01-29T10:01:00.000Z"}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"789","name":"Edit","input":{{}}}}]}},"timestamp":"2026-01-29T10:03:00.000Z"}}"#).unwrap();

        let entry = parse_session_file(file.path(), "test-session", None).unwrap();

        assert_eq!(entry.tool_call_timestamps.len(), 3);
        let first_ts = DateTime::parse_from_rfc3339("2026-01-29T10:01:00.000Z")
            .unwrap()
            .with_timezone(&Utc);
        let second_ts = DateTime::parse_from_rfc3339("2026-01-29T10:03:00.000Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(entry.tool_call_timestamps[0], first_ts);
        assert_eq!(entry.tool_call_timestamps[1], first_ts);
        assert_eq!(entry.tool_call_timestamps[2], second_ts);
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

    /// Given an aborted session (a zero-byte JSONL), When it is parsed, Then it
    /// reports `EmptySession` so the caller can stay silent about it.
    #[test]
    fn test_parse_session_empty_file() {
        let file = NamedTempFile::new().unwrap();
        let result = parse_session_file(file.path(), "test-session", None);
        assert!(matches!(result.unwrap_err(), SessionError::EmptySession));
    }

    /// Given a JSONL holding records none of which is a message — a session that
    /// only fired a `SessionStart` hook — When it is parsed, Then it reports
    /// `NoMessageRecords` so the caller stays silent. Every line parsed; there is
    /// simply nothing to index, and this recurs on every ~30s ingest.
    #[test]
    fn test_parse_session_hook_only_reports_no_message_records() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{"parentUuid":null,"sessionId":"s","type":"progress","data":{{"hookEvent":"SessionStart"}}}}"#
        )
        .unwrap();
        file.flush().unwrap();

        let result = parse_session_file(file.path(), "test-session", None);
        assert!(matches!(
            result.unwrap_err(),
            SessionError::NoMessageRecords
        ));
    }

    /// Given a JSONL whose content is not message-shaped at all, When it is
    /// parsed, Then it is also `NoMessageRecords` — the line never reaches the
    /// deserializer, so calling it a parse failure would warn about noise.
    #[test]
    fn test_parse_session_irrelevant_content_reports_no_message_records() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "this is not JSON at all, but it is content").unwrap();
        file.flush().unwrap();

        let result = parse_session_file(file.path(), "test-session", None);
        assert!(matches!(
            result.unwrap_err(),
            SessionError::NoMessageRecords
        ));
    }

    /// Given a JSONL holding only blank lines, When it is parsed, Then it counts
    /// as empty rather than as a parse failure.
    #[test]
    fn test_parse_session_blank_lines_only_is_empty_session() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file).unwrap();
        writeln!(file).unwrap();
        file.flush().unwrap();

        let result = parse_session_file(file.path(), "test-session", None);
        assert!(matches!(result.unwrap_err(), SessionError::EmptySession));
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
        for src in [
            SessionSource::Claude,
            SessionSource::OpenCode,
            SessionSource::Omp,
        ] {
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
        for src in [
            SessionSource::Claude,
            SessionSource::OpenCode,
            SessionSource::Omp,
        ] {
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

    /// Parses a Claude session whose user messages are exactly `texts`.
    fn claude_session_with_user_texts(texts: &[&str]) -> AgentSession {
        let mut file = NamedTempFile::new().unwrap();
        for (i, text) in texts.iter().enumerate() {
            let line = serde_json::json!({
                "type": "user",
                "message": { "role": "user", "content": text },
                "timestamp": format!("2026-01-29T10:{i:02}:00.000Z"),
                "cwd": "/home/sami/project",
            });
            writeln!(file, "{line}").unwrap();
        }
        file.flush().unwrap();
        parse_session_file(file.path(), "test-session", None).unwrap()
    }

    #[test]
    fn test_claude_system_reminder_produces_no_user_message_timestamp() {
        let entry = claude_session_with_user_texts(&[
            "<system-reminder>\n[BACKGROUND TASK COMPLETED]\n**ID:** `bg_f60bcb1c`",
        ]);

        assert!(entry.user_message_timestamps.is_empty());
        assert!(entry.user_prompts.is_empty());
        assert!(entry.starting_prompt.is_none());
        assert_eq!(entry.message_count, 0);
    }

    #[test]
    fn test_claude_boulder_continuation_produces_no_user_message_timestamp() {
        let entry = claude_session_with_user_texts(&[
            "[SYSTEM DIRECTIVE: OH-MY-OPENCODE - BOULDER CONTINUATION]\n\nContinue the plan.",
        ]);

        assert!(entry.user_message_timestamps.is_empty());
        assert!(entry.user_prompts.is_empty());
        assert_eq!(entry.message_count, 0);
    }

    #[test]
    fn test_claude_skill_instruction_produces_a_user_message_timestamp() {
        let entry = claude_session_with_user_texts(&[
            "<skill-instruction>\nUse the using-jj skill for version control.",
        ]);

        assert_eq!(entry.user_message_timestamps.len(), 1);
        assert_eq!(entry.user_prompts.len(), 1);
        assert!(entry.starting_prompt.is_some());
    }

    #[test]
    fn test_claude_mode_banner_messages_produce_user_message_timestamps() {
        let entry = claude_session_with_user_texts(&[
            "[analyze-mode]\nANALYSIS MODE. Gather context before diving deep.",
            "[CONTEXT]\nWe are mid-refactor of the allocation algorithm.",
            "[search-mode]\nMAXIMIZE SEARCH EFFORT.",
        ]);

        assert_eq!(entry.user_message_timestamps.len(), 3);
        assert_eq!(entry.user_prompts.len(), 3);
        assert_eq!(entry.message_count, 3);
    }

    #[test]
    fn test_claude_session_of_only_injected_messages_has_no_user_message_timestamps() {
        let entry = claude_session_with_user_texts(&[
            "<system-reminder>\nbackground task finished",
            "---\n\n[SYSTEM DIRECTIVE: OH-MY-OPENCODE - SINGLE TASK ONLY]\n\nProceed.",
            "[NOTIFICATION from agent (reply-to: ses_abc)]\nstatus update",
            "<local-command-caveat>Caveat: the messages below were generated",
            "This session is being continued from a previous conversation.",
        ]);

        assert!(entry.user_message_timestamps.is_empty());
        assert!(entry.user_prompts.is_empty());
        assert!(entry.starting_prompt.is_none());
        assert_eq!(entry.message_count, 0);
    }

    #[test]
    fn test_claude_injected_first_message_still_anchors_session_start() {
        // The session began when the harness spoke, even though nobody was
        // watching; only the attention signal is dropped.
        let entry = claude_session_with_user_texts(&[
            "<system-reminder>\nbackground task finished",
            "actually fix the allocation bug",
        ]);

        assert_eq!(
            entry.start_time,
            DateTime::parse_from_rfc3339("2026-01-29T10:00:00.000Z")
                .unwrap()
                .with_timezone(&Utc)
        );
        assert_eq!(entry.user_message_timestamps.len(), 1);
        assert_eq!(
            entry.starting_prompt.as_deref(),
            Some("actually fix the allocation bug")
        );
    }
}

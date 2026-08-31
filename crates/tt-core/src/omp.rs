//! `omp` (oh-my-pi) session parsing.
//!
//! omp writes one JSONL transcript file per top-level session under
//! `<sessions_dir>/<cwd-slug>/`, named `<ISO-8601-timestamp>_<uuid>.jsonl`. A
//! subagent spawned mid-session gets its own transcript inside a directory that
//! sits beside the parent file and shares its filename stem:
//! `<cwd-slug>/<parent-stem>/<name>.jsonl`. A subagent that itself spawns children
//! nests a further directory one level deeper still (`<parent-stem>/<name>/<name>.<child>.jsonl`),
//! but this scanner follows only the one level every session always writes for its
//! own direct subagents, mirroring how [`crate::session`] only follows Claude's
//! single `subagents/` directory rather than recursing arbitrarily. Those
//! grandchild transcripts, along with the `*.md` prompt scaffolds and `*.log` tool
//! output files omp writes beside every transcript, are ignored.
//!
//! # Line shapes
//!
//! This is a survey of real transcripts (~1600 files sampled across two weeks of
//! machine use as of 2026-08-20), not the harness's schema documentation — omp
//! visibly grew its format over that window, so this scanner tolerates unknown
//! `"type"` and `"role"` values rather than erroring on them.
//!
//! Every line is a JSON object with a `"type"` discriminant:
//!
//! - `"session"` (always first): `{"type":"session","version":3,"id","timestamp","cwd`,
//!   and, on newer transcripts, `"title"`/`"titleSource"` inline. A top-level
//!   continuation also carries `"parentSession"` as either a parent UUID or absolute
//!   path to the parent's transcript.
//! - `"title"` (older transcripts; rewritten in place as the title changes, padded
//!   with trailing spaces inside the JSON string so the rewrite never changes the
//!   line's byte length): `{"type":"title","v":1,"title","updatedAt","pad"}`. Note
//!   the timestamp field is `updatedAt`, not `timestamp` — confirmed absent across
//!   every sample — so this line contributes no timestamp to the scan.
//! - `"title_change"` (newer transcripts; appended, not rewritten, on every
//!   update): `{"type":"title_change","title","source","timestamp",...}`.
//! - `"message"`: the only line type carrying conversational content:
//!   `{"type":"message","id","parentId","timestamp","message":{"role","content"?}}`.
//!   `role` observed: `"user"` (a human turn — content is always exactly one
//!   `[{"type":"text","text":...}]` block in every sample), `"assistant"` (a model
//!   turn — content blocks of type `"text"`, `"thinking"`, `"toolCall"`, `"image"`),
//!   `"toolResult"` (a tool call's return value folded back into context),
//!   `"bashExecution"` (an inline shell pane run; carries `command`/`output`
//!   instead of `content`), `"fileMention"` (an `@file` reference; carries `files`
//!   instead of `content`), `"developer"` (harness banners — every sample observed
//!   opens with `<system-reminder>`; this is the harness talking to itself dressed
//!   as a turn, so — like `"custom"`/`"custom_message"` below — it is never treated
//!   as a user message no matter what its text says).
//! - `"custom"` / `"custom_message"`: harness-authored side-channel records (tool
//!   execution telemetry, device-inventory notices). Never a message.
//! - `"model_change"`, `"thinking_level_change"`, `"mode_change"`, `"compaction"`,
//!   `"credential_pin"`, `"ttsr_injection"`, `"service_tier_change"`: harness/session
//!   metadata events. Never a message.
//!
//! # `message_count`
//!
//! omp has no separate `"text"`/`"toolCall"` *line* types the way the task brief
//! for this scanner assumed — those are content-block types nested inside a single
//! `"message"` line's `message.content` array. `message_count` is therefore a count
//! `"message"` lines, one per turn, with the same rule Claude's scanner applies:
//! a `"user"` turn whose text is harness-injected (see [`crate::injection`]) is
//! skipped entirely rather than counted, because it carries no human attention. In a
//! continuation, every message line earlier than the `"session"` creation timestamp is
//! embedded parent history and skipped before it can affect counts, prompts, timestamps,
//! or the session end. Other roles — including tool results, bash panes, file mentions, and
//! `"developer"` banners — still count when they belong to the continuation, but only
//! `"user"` and `"assistant"` turns feed prompts, timestamps, or `assistant_message_count`.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rayon::prelude::*;
use serde::{Deserialize, Deserializer};
use uuid::Uuid;

use crate::session::{
    AgentSession, MAX_TOOL_CALLS_PER_MESSAGE, MAX_USER_MESSAGE_TIMESTAMPS, MAX_USER_PROMPTS,
    ParsedFile, ScanOutcome, SessionError, SessionSource, SessionType, extract_project_name,
    truncate_prompt, unchanged_since,
};

/// Buffer size for `BufReader`, matching [`crate::session`]'s constant.
const BUFFER_SIZE: usize = 64 * 1024;

/// Minimal struct for typed deserialization of an omp transcript line.
#[derive(Debug, Deserialize)]
struct OmpLine {
    #[serde(rename = "type")]
    line_type: Option<String>,
    /// The session's real id — present on the `"session"` line only.
    id: Option<String>,
    /// The session's working directory — present on the `"session"` line only.
    cwd: Option<String>,
    /// ISO-8601 timestamp, present on every line type except the older-style
    /// `"title"` line (see module docs).
    timestamp: Option<String>,
    /// Present on the `"session"` line (new format) and the `"title"` /
    /// `"title_change"` lines.
    title: Option<String>,
    /// Present only on `"message"` lines.
    message: Option<OmpMessage>,
    /// Present on a top-level continuation's `"session"` line. Non-string values
    /// are malformed metadata and intentionally ignored.
    #[serde(
        rename = "parentSession",
        default,
        deserialize_with = "deserialize_optional_string"
    )]
    parent_session: Option<String>,
}

/// Deserializes optional metadata without rejecting an otherwise valid transcript line.
fn deserialize_optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| value.as_str().map(str::to_owned)))
}

#[derive(Debug, Deserialize)]
struct OmpMessage {
    role: Option<String>,
    /// Absent on `"bashExecution"` (which carries `command`/`output`) and
    /// `"fileMention"` (which carries `files`) — both parse fine with this left
    /// `None`, since neither role reads it.
    content: Option<Vec<OmpContentBlock>>,
}

/// A content block inside a message's `content` array.
#[derive(Debug, Deserialize)]
struct OmpContentBlock {
    #[serde(rename = "type")]
    block_type: Option<String>,
    /// Present on `"text"` blocks.
    text: Option<String>,
}

/// Parses an RFC 3339 timestamp into UTC, discarding anything that fails to parse.
fn parse_ts(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Returns the text of the first `"text"` content block, if any.
///
/// Every `"user"` message sampled carries exactly one block, but this does not
/// assume that: it is the same "first text block wins" contract Claude's own
/// content extraction uses.
fn first_text_block(blocks: Option<&[OmpContentBlock]>) -> Option<&str> {
    blocks?
        .iter()
        .find_map(|b| (b.block_type.as_deref() == Some("text")).then_some(b.text.as_deref()))
        .flatten()
}

/// Derives a session id from a transcript's own filename stem, used only when its
/// `"session"` line is missing or fails to parse.
///
/// A top-level session file is named `<ISO-8601-timestamp>_<uuid>.jsonl`; the
/// fallback there is the uuid segment, matching what a healthy `"session"` line
/// would have reported. A subagent file's stem carries no such convention (it is
/// named after the subagent itself, e.g. `CognitoDomains.jsonl`), so the fallback
/// there is the whole stem.
fn fallback_session_id(stem: &str) -> String {
    stem.rsplit_once('_')
        .map(|(_, suffix)| suffix)
        .filter(|candidate| candidate.len() == 36 && candidate.matches('-').count() == 4)
        .unwrap_or(stem)
        .to_string()
}

/// Parses omp's continuation-parent forms without trusting malformed metadata.
///
/// Omp writes either the parent's bare UUID or an absolute transcript path whose
/// filename stem ends in `_<uuid>`. Both forms retain their original UUID spelling so
/// they match the id stored from the parent's own session line.
fn parse_continuation_parent_session_id(value: &str) -> Option<String> {
    let candidate = if Path::new(value).is_absolute() {
        Path::new(value).file_stem()?.to_str()?.rsplit_once('_')?.1
    } else {
        value
    };
    Uuid::parse_str(candidate)
        .ok()
        .map(|_| candidate.to_string())
}

/// Parses one omp transcript file into an [`AgentSession`].
///
/// `fallback_id` is used only when the `"session"` line is missing or its `id`
/// field fails to parse — see [`fallback_session_id`]. `parent_session_id` is
/// `Some` for a subagent transcript nested one directory below its parent; top-level
/// continuations derive their own parent from omp's `"session"` metadata.
#[expect(
    clippy::too_many_lines,
    reason = "Session parser keeps the IO loop in one function for clarity, matching session::parse_session_file"
)]
pub fn parse_session_file(
    path: &Path,
    fallback_id: &str,
    parent_session_id: Option<&str>,
) -> Result<AgentSession, SessionError> {
    let file = File::open(path)?;
    let reader = BufReader::with_capacity(BUFFER_SIZE, file);

    let mut session_id: Option<String> = None;
    let mut project_path: Option<String> = None;
    let mut session_timestamp: Option<DateTime<Utc>> = None;
    let mut first_message_timestamp: Option<DateTime<Utc>> = None;
    let mut last_timestamp: Option<DateTime<Utc>> = None;

    let mut message_count = 0i32;
    let mut assistant_message_count = 0i32;
    let mut tool_call_count = 0i32;
    let mut user_prompts: Vec<String> = Vec::new();
    let mut starting_prompt: Option<String> = None;
    let mut user_message_timestamps: Vec<DateTime<Utc>> = Vec::new();
    let mut tool_call_timestamps: Vec<DateTime<Utc>> = Vec::new();
    let mut continuation_parent_session_id: Option<String> = None;

    // Title resolution prefers the most recently known update mechanism: an
    // appended `"title_change"` record beats the session line's own inline title
    // (present from the first line in the newer format), which in turn beats the
    // older rewritten-in-place `"title"` line (only present in older transcripts).
    let mut session_line_title: Option<String> = None;
    let mut title_line_title: Option<String> = None;
    let mut title_change_title: Option<String> = None;

    // Three states, matching session::parse_session_file: an aborted session
    // leaves an empty file; a session whose lines are all harness metadata
    // (no "message" line at all) is expected; a session whose lines fail to
    // parse as JSON is a genuine defect. Only the last warrants a warning.
    let mut saw_content = false;
    let mut saw_parse_failure = false;
    let mut saw_message_line = false;

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        saw_content = true;

        let parsed: OmpLine = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                saw_parse_failure = true;
                tracing::trace!(error = %e, "skipping malformed omp line");
                continue;
            }
        };

        let timestamp = parsed.timestamp.as_deref().and_then(parse_ts);
        let is_replayed_continuation_message = parsed.line_type.as_deref() == Some("message")
            && continuation_parent_session_id.is_some()
            && session_timestamp.is_some_and(|created_at| {
                timestamp.is_some_and(|message_at| message_at < created_at)
            });
        if is_replayed_continuation_message {
            continue;
        }

        // Every line type except the older-style "title" line carries a
        // "timestamp" field (that one uses "updatedAt" instead — see module
        // docs), so this tracks the transcript's last-touched moment regardless
        // of what produced it.
        if let Some(timestamp) = timestamp {
            last_timestamp = Some(timestamp);
        }

        match parsed.line_type.as_deref() {
            Some("session") => {
                if session_id.is_none() {
                    session_id.clone_from(&parsed.id);
                }
                if project_path.is_none() {
                    project_path.clone_from(&parsed.cwd);
                }
                if session_timestamp.is_none() {
                    session_timestamp = timestamp;
                }
                if continuation_parent_session_id.is_none() {
                    continuation_parent_session_id = parsed
                        .parent_session
                        .as_deref()
                        .and_then(parse_continuation_parent_session_id);
                }
                if let Some(title) = parsed.title.filter(|t| !t.is_empty()) {
                    session_line_title = Some(title);
                }
            }
            Some("title") => {
                if let Some(title) = parsed.title.filter(|t| !t.is_empty()) {
                    title_line_title = Some(title);
                }
            }
            Some("title_change") => {
                if let Some(title) = parsed.title.filter(|t| !t.is_empty()) {
                    title_change_title = Some(title);
                }
            }
            Some("message") => {
                let Some(msg) = parsed.message else {
                    continue;
                };
                saw_message_line = true;
                let ts = timestamp;
                if let Some(ts) = ts {
                    first_message_timestamp.get_or_insert(ts);
                }

                match msg.role.as_deref() {
                    Some("user") => {
                        let text = first_text_block(msg.content.as_deref());
                        if text.is_some_and(crate::injection::is_injected) {
                            // Injected text is the harness talking to the agent,
                            // not a person — see the module docs on "developer".
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
                                if let Some(ts) = ts {
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
                        if let Some(blocks) = msg.content.as_deref() {
                            let count = blocks
                                .iter()
                                .filter(|b| b.block_type.as_deref() == Some("toolCall"))
                                .count();
                            tool_call_count = tool_call_count
                                .saturating_add(i32::try_from(count).unwrap_or(i32::MAX));
                            if let Some(ts) = ts {
                                let capped_count = count.min(MAX_TOOL_CALLS_PER_MESSAGE);
                                tool_call_timestamps.extend(std::iter::repeat_n(ts, capped_count));
                            }
                        }
                    }
                    // toolResult, bashExecution, fileMention, "developer" banners,
                    // and anything the harness adds later: harness-carried
                    // content, never a person (see module docs). Still a turn
                    // that occupied the transcript, so it counts toward
                    // message_count but never contributes a prompt.
                    Some(_) | None => {
                        message_count = message_count.saturating_add(1);
                    }
                }
            }
            _ => {}
        }
    }

    if session_id.is_none() && !saw_message_line {
        return Err(match (saw_content, saw_parse_failure) {
            (_, true) => SessionError::NoMessages,
            (true, false) => SessionError::NoMessageRecords,
            (false, false) => SessionError::EmptySession,
        });
    }

    let session_id = session_id.unwrap_or_else(|| fallback_id.to_string());
    let continuation_parent_session_id =
        continuation_parent_session_id.filter(|parent_id| parent_id != &session_id);
    let project_path = project_path.ok_or(SessionError::NoProjectPath)?;
    let start_time = session_timestamp
        .or(first_message_timestamp)
        .ok_or(SessionError::NoMessageRecords)?;

    let session_type = if parent_session_id.is_some() {
        SessionType::Subagent
    } else if continuation_parent_session_id.is_some() {
        SessionType::Continuation
    } else {
        SessionType::User
    };
    let linked_parent_session_id = parent_session_id
        .map(String::from)
        .or(continuation_parent_session_id);

    Ok(AgentSession {
        session_id,
        source: SessionSource::Omp,
        parent_session_id: linked_parent_session_id,
        session_type,
        project_name: extract_project_name(&project_path),
        project_path,
        start_time,
        end_time: if last_timestamp == Some(start_time) {
            None
        } else {
            last_timestamp
        },
        message_count,
        summary: title_change_title
            .or(session_line_title)
            .or(title_line_title),
        user_prompts,
        starting_prompt,
        assistant_message_count,
        tool_call_count,
        user_message_timestamps,
        tool_call_timestamps,
    })
}

/// One discovered omp transcript queued for parsing.
struct SessionFile {
    path: PathBuf,
    fallback_id: String,
    parent_session_id: Option<String>,
}

/// Scan the omp sessions directory and build a session index.
///
/// Reads every transcript. Callers on the ~30s ingest path want
/// [`scan_omp_sessions_incremental`] instead.
pub fn scan_omp_sessions(sessions_dir: &Path) -> Result<Vec<AgentSession>, SessionError> {
    Ok(scan_omp_sessions_incremental(sessions_dir, None)?.sessions)
}

/// Scan omp sessions, optionally skipping files unmodified since `since`.
///
/// Walks `<sessions_dir>/<cwd-slug>/` for top-level `*.jsonl` transcripts, and one
/// directory deeper — `<cwd-slug>/<parent-stem>/*.jsonl` — for the direct
/// subagents of a session whose stem matches that directory's name. See the
/// module docs for why deeper subagent-of-subagent nesting is not followed.
///
/// A file's mtime, taken from the directory entry the walk already holds, decides
/// whether it may be skipped unopened — the same incremental contract
/// [`crate::session::scan_claude_sessions_incremental`] documents.
pub fn scan_omp_sessions_incremental(
    sessions_dir: &Path,
    since: Option<DateTime<Utc>>,
) -> Result<ScanOutcome, SessionError> {
    if !sessions_dir.exists() {
        return Ok(ScanOutcome::complete(Vec::new()));
    }

    let mut session_files: Vec<SessionFile> = Vec::new();

    for cwd_entry in std::fs::read_dir(sessions_dir)? {
        let cwd_entry = cwd_entry?;
        let cwd_path = cwd_entry.path();
        if !cwd_path.is_dir() {
            continue;
        }

        for entry in std::fs::read_dir(&cwd_path)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
                let stem = path
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                if stem.is_empty() {
                    tracing::warn!(path = ?path, "skipping omp session file with empty stem");
                    continue;
                }
                if unchanged_since(&entry, since) {
                    continue;
                }
                session_files.push(SessionFile {
                    fallback_id: fallback_session_id(&stem),
                    path,
                    parent_session_id: None,
                });
            } else if path.is_dir() {
                // A subagent directory sits beside the parent file and shares its
                // stem — only treat it as one if that sibling file actually
                // exists, so an unrelated directory is never misread as subagents.
                let stem = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                if stem.is_empty() || !cwd_path.join(format!("{stem}.jsonl")).is_file() {
                    continue;
                }
                let parent_id = fallback_session_id(&stem);

                let Ok(subagent_entries) = std::fs::read_dir(&path) else {
                    continue;
                };
                for subagent_entry in subagent_entries.flatten() {
                    let subagent_path = subagent_entry.path();
                    if !(subagent_path.is_file()
                        && subagent_path.extension().is_some_and(|e| e == "jsonl"))
                    {
                        // Ignores *.md prompt scaffolds, *.log tool output, and
                        // any deeper subagent-of-subagent directory — see module
                        // docs.
                        continue;
                    }
                    let sub_stem = subagent_path
                        .file_stem()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string();
                    if sub_stem.is_empty() {
                        tracing::warn!(path = ?subagent_path, "skipping omp subagent file with empty stem");
                        continue;
                    }
                    if unchanged_since(&subagent_entry, since) {
                        continue;
                    }
                    session_files.push(SessionFile {
                        fallback_id: fallback_session_id(&sub_stem),
                        path: subagent_path,
                        parent_session_id: Some(parent_id.clone()),
                    });
                }
            }
        }
    }

    let parsed: Vec<ParsedFile> = session_files
        .par_iter()
        .map(
            |sf| match parse_session_file(&sf.path, &sf.fallback_id, sf.parent_session_id.as_deref())
            {
                Ok(entry) => ParsedFile::yielded(entry),
                Err(e @ (SessionError::EmptySession | SessionError::NoMessageRecords)) => {
                    tracing::debug!(path = ?sf.path, error = %e, "skipping omp session with nothing to index");
                    ParsedFile::NOTHING_TO_INDEX
                }
                Err(e) => {
                    tracing::warn!(path = ?sf.path, error = %e, "skipping invalid omp session");
                    ParsedFile::DEFECTIVE
                }
            },
        )
        .collect();

    let complete = parsed.iter().all(|f| f.clean);
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

    fn write_lines(path: &Path, lines: &[&str]) {
        let mut file = File::create(path).expect("create fixture");
        for line in lines {
            writeln!(file, "{line}").expect("write fixture line");
        }
    }

    const SESSION_LINE: &str = r#"{"type":"session","version":3,"id":"01a0082b-2c9d-7000-b52c-10ef998c3061","timestamp":"2026-08-16T01:24:02.000Z","cwd":"/tmp/proj"}"#;

    fn user_line(text: &str, timestamp: &str) -> String {
        format!(
            r#"{{"type":"message","id":"a","parentId":null,"timestamp":"{timestamp}","message":{{"role":"user","content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
    }

    fn assistant_line(timestamp: &str, tool_calls: usize) -> String {
        let mut blocks = vec![r#"{"type":"text","text":"ok"}"#.to_string()];
        for i in 0..tool_calls {
            blocks.push(format!(
                r#"{{"type":"toolCall","id":"tc{i}","name":"read","arguments":{{}}}}"#
            ));
        }
        format!(
            r#"{{"type":"message","id":"b","parentId":"a","timestamp":"{timestamp}","message":{{"role":"assistant","content":[{}]}}}}"#,
            blocks.join(",")
        )
    }

    #[test]
    fn empty_sessions_dir_scans_clean() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outcome = scan_omp_sessions_incremental(temp.path(), None).expect("scan");
        assert!(outcome.sessions.is_empty());
        assert!(outcome.complete);
    }

    #[test]
    fn missing_sessions_dir_is_complete_and_empty() {
        let temp = tempfile::tempdir().expect("tempdir");
        let missing = temp.path().join("does-not-exist");
        let outcome = scan_omp_sessions_incremental(&missing, None).expect("scan");
        assert!(outcome.sessions.is_empty());
        assert!(outcome.complete);
    }

    #[test]
    fn since_filter_skips_unmodified_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cwd_dir = temp.path().join("-tmp-proj");
        std::fs::create_dir_all(&cwd_dir).expect("create cwd dir");
        let path =
            cwd_dir.join("2026-08-16T01-24-02-000Z_01a0082b-2c9d-7000-b52c-10ef998c3061.jsonl");
        write_lines(
            &path,
            &[SESSION_LINE, &user_line("hi", "2026-08-16T01:24:03.000Z")],
        );

        let far_future = Utc::now() + chrono::Duration::days(365);
        let outcome = scan_omp_sessions_incremental(temp.path(), Some(far_future)).expect("scan");
        assert!(
            outcome.sessions.is_empty(),
            "a since bound past the file's mtime must skip it unopened"
        );
        assert!(outcome.complete);

        let outcome = scan_omp_sessions_incremental(temp.path(), None).expect("scan");
        assert_eq!(outcome.sessions.len(), 1, "no since bound must read it");
    }

    #[test]
    fn injected_user_text_is_excluded() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cwd_dir = temp.path().join("-tmp-proj");
        std::fs::create_dir_all(&cwd_dir).expect("create cwd dir");
        let path =
            cwd_dir.join("2026-08-16T01-24-02-000Z_01a0082b-2c9d-7000-b52c-10ef998c3061.jsonl");
        let injected = user_line(
            "<system-reminder>\\nContinue.\\n</system-reminder>",
            "2026-08-16T01:24:03.000Z",
        );
        let real = user_line("please fix the bug", "2026-08-16T01:24:04.000Z");
        write_lines(&path, &[SESSION_LINE, &injected, &real]);

        let sessions = scan_omp_sessions(temp.path()).expect("scan");
        assert_eq!(sessions.len(), 1);
        let session = &sessions[0];
        assert_eq!(session.message_count, 1, "the injected turn must not count");
        assert_eq!(
            session.starting_prompt.as_deref(),
            Some("please fix the bug")
        );
        assert_eq!(session.user_message_timestamps.len(), 1);
    }

    #[test]
    fn subagent_directory_nests_with_parent_id() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cwd_dir = temp.path().join("-tmp-proj");
        std::fs::create_dir_all(&cwd_dir).expect("create cwd dir");
        let stem = "2026-08-16T01-24-02-000Z_01a0082b-2c9d-7000-b52c-10ef998c3061";
        let parent_path = cwd_dir.join(format!("{stem}.jsonl"));
        write_lines(
            &parent_path,
            &[
                SESSION_LINE,
                &user_line("do the thing", "2026-08-16T01:24:03.000Z"),
            ],
        );

        let subagent_dir = cwd_dir.join(stem);
        std::fs::create_dir_all(&subagent_dir).expect("create subagent dir");
        let sub_session_line = r#"{"type":"session","version":3,"id":"01a0082d-014b-7000-9fdb-71cdbcef63be","timestamp":"2026-08-16T01:26:02.000Z","cwd":"/tmp/proj"}"#;
        write_lines(
            &subagent_dir.join("Worker.jsonl"),
            &[
                sub_session_line,
                &assistant_line("2026-08-16T01:26:03.000Z", 0),
            ],
        );
        // A prompt scaffold beside the transcript, and a non-subagent directory
        // that happens to hold jsonl but has no sibling file — neither is a
        // subagent.
        std::fs::write(subagent_dir.join("Worker.md"), b"scratch notes").expect("write md");
        let unrelated_dir = cwd_dir.join("not-a-session-stem");
        std::fs::create_dir_all(&unrelated_dir).expect("create unrelated dir");
        write_lines(&unrelated_dir.join("stray.jsonl"), &[SESSION_LINE]);

        let mut sessions = scan_omp_sessions(temp.path()).expect("scan");
        sessions.sort_by(|a, b| a.session_type.as_str().cmp(b.session_type.as_str()));

        let parent_id = "01a0082b-2c9d-7000-b52c-10ef998c3061";
        let subagent = sessions
            .iter()
            .find(|s| s.session_type == SessionType::Subagent)
            .expect("subagent session present");
        assert_eq!(subagent.parent_session_id.as_deref(), Some(parent_id));
        assert_eq!(subagent.session_id, "01a0082d-014b-7000-9fdb-71cdbcef63be");

        assert!(
            !sessions.iter().any(|s| s.session_id == "stray"),
            "a directory without a sibling parent file must not be read as subagents"
        );
    }

    #[test]
    fn malformed_line_is_tolerated() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cwd_dir = temp.path().join("-tmp-proj");
        std::fs::create_dir_all(&cwd_dir).expect("create cwd dir");
        let path =
            cwd_dir.join("2026-08-16T01-24-02-000Z_01a0082b-2c9d-7000-b52c-10ef998c3061.jsonl");
        write_lines(
            &path,
            &[
                SESSION_LINE,
                "{not valid json",
                &user_line("hello", "2026-08-16T01:24:03.000Z"),
            ],
        );

        let sessions = scan_omp_sessions(temp.path()).expect("scan");
        assert_eq!(
            sessions.len(),
            1,
            "a malformed line must not abort the file"
        );
        assert_eq!(sessions[0].message_count, 1);
    }

    #[test]
    fn missing_session_and_message_lines_errors() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("orphan.jsonl");
        write_lines(&path, &[r#"{"type":"custom","customType":"noise"}"#]);

        let err = parse_session_file(&path, "orphan", None).expect_err("must error");
        assert!(matches!(err, SessionError::NoMessageRecords));
    }

    #[test]
    fn fallback_id_extracts_uuid_from_top_level_stem() {
        let stem = "2026-08-16T01-24-02-000Z_01a0082b-2c9d-7000-b52c-10ef998c3061";
        assert_eq!(
            fallback_session_id(stem),
            "01a0082b-2c9d-7000-b52c-10ef998c3061"
        );
        assert_eq!(fallback_session_id("Worker"), "Worker");
    }

    #[test]
    fn tool_call_and_thinking_blocks_are_counted() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cwd_dir = temp.path().join("-tmp-proj");
        std::fs::create_dir_all(&cwd_dir).expect("create cwd dir");
        let path =
            cwd_dir.join("2026-08-16T01-24-02-000Z_01a0082b-2c9d-7000-b52c-10ef998c3061.jsonl");
        write_lines(
            &path,
            &[
                SESSION_LINE,
                &user_line("go", "2026-08-16T01:24:03.000Z"),
                &assistant_line("2026-08-16T01:24:04.000Z", 2),
            ],
        );

        let sessions = scan_omp_sessions(temp.path()).expect("scan");
        assert_eq!(sessions.len(), 1);
        let session = &sessions[0];
        assert_eq!(session.assistant_message_count, 1);
        assert_eq!(session.tool_call_count, 2);
        assert_eq!(session.tool_call_timestamps.len(), 2);
        assert_eq!(session.message_count, 2);
    }

    #[test]
    fn title_change_wins_over_session_and_title_line() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("titled.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"title","v":1,"title":"old style title","updatedAt":"2026-08-16T01:24:02.000Z","pad":""}"#,
                r#"{"type":"session","version":3,"id":"01a0082b-2c9d-7000-b52c-10ef998c3061","timestamp":"2026-08-16T01:24:02.000Z","cwd":"/tmp/proj","title":"inline title"}"#,
                &user_line("go", "2026-08-16T01:24:03.000Z"),
                r#"{"type":"title_change","id":"x","parentId":"a","timestamp":"2026-08-16T01:24:05.000Z","title":"final title","source":"auto"}"#,
            ],
        );

        let session = parse_session_file(&path, "titled", None).expect("parse");
        assert_eq!(session.summary.as_deref(), Some("final title"));
    }
    #[test]
    fn top_level_continuations_record_bare_and_path_parent_session_ids() {
        let temp = tempfile::tempdir().expect("tempdir");
        let cwd_dir = temp.path().join("-tmp-proj");
        std::fs::create_dir_all(&cwd_dir).expect("create cwd dir");
        let parent_id = "01a0082b-2c9d-7000-b52c-10ef998c3061";
        let bare_id = "01a0082d-014b-7000-9fdb-71cdbcef63be";
        let path_id = "01a0082e-014b-7000-9fdb-71cdbcef63be";
        let parent_path = cwd_dir.join(format!("2026-08-16T01-24-02-000Z_{parent_id}.jsonl"));
        write_lines(
            &parent_path,
            &[
                SESSION_LINE,
                &user_line("start the work", "2026-08-16T01:24:03.000Z"),
            ],
        );
        let parent_transcript = parent_path.display();
        write_lines(
            &cwd_dir.join(format!("2026-08-16T02-24-02-000Z_{bare_id}.jsonl")),
            &[
                &format!(
                    r#"{{"type":"session","version":3,"id":"{bare_id}","parentSession":"{parent_id}","timestamp":"2026-08-16T02:24:02.000Z","cwd":"/tmp/proj"}}"#
                ),
                &user_line("continue the work", "2026-08-16T02:24:03.000Z"),
            ],
        );
        write_lines(
            &cwd_dir.join(format!("2026-08-16T03-24-02-000Z_{path_id}.jsonl")),
            &[
                &format!(
                    r#"{{"type":"session","version":3,"id":"{path_id}","parentSession":"{parent_transcript}","timestamp":"2026-08-16T03:24:02.000Z","cwd":"/tmp/proj"}}"#
                ),
                &user_line("finish the work", "2026-08-16T03:24:03.000Z"),
            ],
        );

        let sessions = scan_omp_sessions(temp.path()).expect("scan");
        for continuation_id in [bare_id, path_id] {
            let continuation = sessions
                .iter()
                .find(|session| session.session_id == continuation_id)
                .expect("continuation present");
            assert_eq!(continuation.parent_session_id.as_deref(), Some(parent_id));
            assert_eq!(continuation.session_type, SessionType::Continuation);
        }
    }

    #[test]
    fn continuation_replayed_message_lines_are_not_derived() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parent_id = "01a0082b-2c9d-7000-b52c-10ef998c3061";
        let continuation_id = "01a0082d-014b-7000-9fdb-71cdbcef63be";
        let path = temp.path().join("continuation.jsonl");
        write_lines(
            &path,
            &[
                &format!(
                    r#"{{"type":"session","version":3,"id":"{continuation_id}","parentSession":"{parent_id}","timestamp":"2026-08-16T02:00:00.000Z","cwd":"/tmp/proj"}}"#
                ),
                &user_line("continue from the parent", "2026-08-16T02:01:00.000Z"),
                &assistant_line("2026-08-16T02:02:00.000Z", 0),
                &user_line("the parent already asked this", "2026-08-16T01:00:00.000Z"),
                &assistant_line("2026-08-16T01:01:00.000Z", 2),
                r#"{"type":"message","timestamp":"2026-08-16T01:02:00.000Z","message":{"role":"toolResult"}}"#,
            ],
        );

        let continuation = parse_session_file(&path, continuation_id, None).expect("parse");

        assert_eq!(continuation.session_type, SessionType::Continuation);
        assert_eq!(continuation.message_count, 2);
        assert_eq!(continuation.assistant_message_count, 1);
        assert_eq!(continuation.tool_call_count, 0);
        assert!(continuation.tool_call_timestamps.is_empty());
        assert_eq!(
            continuation.user_prompts,
            vec!["continue from the parent".to_string()]
        );
        assert_eq!(
            continuation.starting_prompt.as_deref(),
            Some("continue from the parent")
        );
        assert_eq!(continuation.user_message_timestamps.len(), 1);
        assert_eq!(
            continuation.end_time,
            Some(
                DateTime::parse_from_rfc3339("2026-08-16T02:02:00.000Z")
                    .expect("timestamp")
                    .with_timezone(&Utc)
            )
        );
        assert!(
            continuation
                .end_time
                .is_none_or(|end_time| end_time >= continuation.start_time)
        );
    }

    #[test]
    fn malformed_continuation_parent_is_ignored() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("malformed-parent.jsonl");
        write_lines(
            &path,
            &[
                r#"{"type":"session","version":3,"id":"01a0082d-014b-7000-9fdb-71cdbcef63be","parentSession":7,"timestamp":"2026-08-16T02:00:00.000Z","cwd":"/tmp/proj"}"#,
                &user_line("continue normally", "2026-08-16T02:01:00.000Z"),
            ],
        );

        let session =
            parse_session_file(&path, "01a0082d-014b-7000-9fdb-71cdbcef63be", None).expect("parse");
        assert!(session.parent_session_id.is_none());
        assert_eq!(session.session_type, SessionType::User);
    }

    #[test]
    fn self_referential_continuation_parent_is_ignored() {
        let temp = tempfile::tempdir().expect("tempdir");
        let session_id = "01a0082d-014b-7000-9fdb-71cdbcef63be";
        let path = temp.path().join("self-parent.jsonl");
        write_lines(
            &path,
            &[
                &format!(
                    r#"{{"type":"session","version":3,"id":"{session_id}","parentSession":"{session_id}","timestamp":"2026-08-16T02:00:00.000Z","cwd":"/tmp/proj"}}"#
                ),
                &user_line("continue normally", "2026-08-16T02:01:00.000Z"),
            ],
        );

        let session = parse_session_file(&path, session_id, None).expect("parse");

        assert!(session.parent_session_id.is_none());
        assert_eq!(session.session_type, SessionType::User);
    }
}

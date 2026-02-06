//! `OpenCode` session parsing.

use std::fs;
use std::path::Path;

use chrono::{DateTime, TimeZone, Utc};
use rayon::prelude::*;
use serde::Deserialize;

use crate::session::{
    AgentSession, MAX_USER_MESSAGE_TIMESTAMPS, MAX_USER_PROMPTS, SessionError, SessionSource,
    SessionType, extract_project_name, truncate_prompt,
};

/// `OpenCode` session metadata.
#[derive(Debug, Deserialize)]
struct OpenCodeSession {
    id: String,
    directory: String,
    title: Option<String>,
    #[serde(rename = "parentID")]
    parent_id: Option<String>,
    time: OpenCodeTime,
}

#[derive(Debug, Deserialize)]
struct OpenCodeTime {
    created: i64,
    updated: Option<i64>,
}

/// `OpenCode` message role.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum MessageRole {
    User,
    Assistant,
    #[serde(other)]
    Other,
}

/// `OpenCode` message metadata.
#[derive(Debug, Deserialize)]
struct OpenCodeMessage {
    id: String,
    role: MessageRole,
    time: OpenCodeMessageTime,
}

#[derive(Debug, Deserialize)]
struct OpenCodeMessageTime {
    created: i64,
}

/// `OpenCode` message part type.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum PartType {
    Text,
    Tool,
    #[serde(other)]
    Other,
}

/// `OpenCode` message part.
#[derive(Debug, Deserialize)]
struct OpenCodePart {
    /// Part ID -- used for deterministic ordering since `read_json_files`
    /// returns entries in filesystem-dependent order.
    id: String,
    #[serde(rename = "type")]
    part_type: PartType,
    text: Option<String>,
}

fn unix_ms_to_datetime(ms: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_millis_opt(ms).single()
}

/// Read and deserialize all JSON files from a directory.
fn read_json_files<T: serde::de::DeserializeOwned>(dir: &Path) -> Vec<T> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };

    entries
        .filter_map(std::result::Result::ok)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|e| {
            let path = e.path();
            let data = match fs::read_to_string(&path) {
                Ok(d) => d,
                Err(err) => {
                    tracing::warn!(path = ?path, error = %err, "failed to read JSON file");
                    return None;
                }
            };
            match serde_json::from_str(&data) {
                Ok(v) => Some(v),
                Err(err) => {
                    tracing::warn!(path = ?path, error = %err, "failed to parse JSON file");
                    None
                }
            }
        })
        .collect()
}

/// Parse an `OpenCode` session from its session file.
pub fn parse_opencode_session(
    storage_dir: &Path,
    session_file: &Path,
) -> Result<AgentSession, SessionError> {
    let data = fs::read_to_string(session_file)?;
    let session: OpenCodeSession = serde_json::from_str(&data)?;

    if session.id.is_empty() {
        return Err(SessionError::EmptySessionId);
    }

    let message_dir = storage_dir.join("message").join(&session.id);

    let mut messages: Vec<OpenCodeMessage> = read_json_files(&message_dir);
    messages.sort_by_key(|m| m.time.created);

    let mut user_message_count = 0i32;
    let mut assistant_message_count = 0i32;
    let mut tool_call_count = 0i32;
    let mut user_prompts: Vec<String> = Vec::new();
    let mut starting_prompt: Option<String> = None;
    let mut user_message_timestamps: Vec<DateTime<Utc>> = Vec::new();
    let mut last_message_time: Option<i64> = None;

    for msg in &messages {
        last_message_time = Some(msg.time.created);

        let mut parts: Vec<OpenCodePart> = match msg.role {
            MessageRole::User | MessageRole::Assistant => {
                read_json_files(&storage_dir.join("part").join(&msg.id))
            }
            MessageRole::Other => continue,
        };
        // Sort by ID for deterministic ordering (fs::read_dir order is platform-dependent)
        parts.sort_by(|a, b| a.id.cmp(&b.id));

        match msg.role {
            MessageRole::User => {
                user_message_count = user_message_count.saturating_add(1);

                let text: String = parts
                    .iter()
                    .filter(|p| p.part_type == PartType::Text)
                    .filter_map(|p| p.text.as_deref())
                    .collect::<Vec<_>>()
                    .join("\n");

                if !text.is_empty() {
                    if starting_prompt.is_none() {
                        starting_prompt = Some(truncate_prompt(&text));
                    }
                    if user_prompts.len() < MAX_USER_PROMPTS {
                        user_prompts.push(truncate_prompt(&text));
                    }
                    if user_message_timestamps.len() < MAX_USER_MESSAGE_TIMESTAMPS {
                        if let Some(ts) = unix_ms_to_datetime(msg.time.created) {
                            user_message_timestamps.push(ts);
                        }
                    }
                }
            }
            MessageRole::Assistant => {
                assistant_message_count = assistant_message_count.saturating_add(1);

                let count = parts
                    .iter()
                    .filter(|p| p.part_type == PartType::Tool)
                    .count();
                tool_call_count =
                    tool_call_count.saturating_add(i32::try_from(count).unwrap_or(i32::MAX));
            }
            MessageRole::Other => unreachable!(),
        }
    }

    let message_count = user_message_count.saturating_add(assistant_message_count);
    let start_time = unix_ms_to_datetime(session.time.created)
        .ok_or(SessionError::InvalidTimestamp(session.time.created))?;

    // end_time: latest of last message time or session updated time
    let end_ms = match (last_message_time, session.time.updated) {
        (Some(msg), Some(upd)) => Some(msg.max(upd)),
        (any, other) => any.or(other),
    };
    let end_time = end_ms
        .and_then(unix_ms_to_datetime)
        .filter(|t| *t > start_time);

    let session_type = if session.parent_id.is_some() {
        SessionType::Subagent
    } else {
        SessionType::User
    };

    let project_name = extract_project_name(&session.directory);

    Ok(AgentSession {
        session_id: session.id,
        source: SessionSource::OpenCode,
        parent_session_id: session.parent_id,
        session_type,
        project_path: session.directory,
        project_name,
        start_time,
        end_time,
        message_count,
        summary: session.title,
        user_prompts,
        starting_prompt,
        assistant_message_count,
        tool_call_count,
        user_message_timestamps,
    })
}

/// Scan `OpenCode` storage directory for sessions.
pub fn scan_opencode_sessions(storage_dir: &Path) -> Result<Vec<AgentSession>, SessionError> {
    let session_dir = storage_dir.join("session");
    if !session_dir.exists() {
        return Ok(Vec::new());
    }

    let mut session_files = Vec::new();

    for project_entry in fs::read_dir(&session_dir)? {
        let project_path = project_entry?.path();
        if !project_path.is_dir() {
            continue;
        }
        for session_entry in fs::read_dir(&project_path)? {
            let session_path = session_entry?.path();
            if session_path.extension().is_some_and(|e| e == "json") {
                session_files.push(session_path);
            }
        }
    }

    let mut sessions: Vec<AgentSession> = session_files
        .par_iter()
        .filter_map(|path| match parse_opencode_session(storage_dir, path) {
            Ok(session) => Some(session),
            Err(e) => {
                tracing::warn!(path = ?path, error = %e, "skipping invalid OpenCode session");
                None
            }
        })
        .collect();

    sessions.sort_by_key(|e| e.start_time);
    Ok(sessions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a minimal `OpenCode` session fixture.
    #[expect(clippy::too_many_arguments, reason = "test fixture helper")]
    fn create_session_fixture(
        storage_dir: &Path,
        project_hash: &str,
        session_id: &str,
        directory: &str,
        title: Option<&str>,
        parent_id: Option<&str>,
        created_ms: i64,
        updated_ms: Option<i64>,
    ) -> std::path::PathBuf {
        let session_dir = storage_dir.join("session").join(project_hash);
        fs::create_dir_all(&session_dir).unwrap();

        let session_data = serde_json::json!({
            "id": session_id,
            "directory": directory,
            "title": title,
            "parentID": parent_id,
            "time": {
                "created": created_ms,
                "updated": updated_ms,
            }
        });

        let path = session_dir.join(format!("{session_id}.json"));
        fs::write(&path, serde_json::to_string(&session_data).unwrap()).unwrap();
        path
    }

    fn create_message_fixture(
        storage_dir: &Path,
        session_id: &str,
        message_id: &str,
        role: &str,
        created_ms: i64,
    ) {
        let msg_dir = storage_dir.join("message").join(session_id);
        fs::create_dir_all(&msg_dir).unwrap();

        let msg_data = serde_json::json!({
            "id": message_id,
            "sessionID": session_id,
            "role": role,
            "time": { "created": created_ms }
        });

        fs::write(
            msg_dir.join(format!("{message_id}.json")),
            serde_json::to_string(&msg_data).unwrap(),
        )
        .unwrap();
    }

    fn create_part_fixture(
        storage_dir: &Path,
        message_id: &str,
        part_id: &str,
        part_type: &str,
        text: Option<&str>,
    ) {
        let part_dir = storage_dir.join("part").join(message_id);
        fs::create_dir_all(&part_dir).unwrap();

        let mut part_data = serde_json::json!({
            "id": part_id,
            "type": part_type,
        });
        if let Some(t) = text {
            part_data["text"] = serde_json::Value::String(t.to_string());
        }

        fs::write(
            part_dir.join(format!("{part_id}.json")),
            serde_json::to_string(&part_data).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn test_parse_basic_session() {
        let temp = TempDir::new().unwrap();
        let storage = temp.path();

        let session_file = create_session_fixture(
            storage,
            "proj1",
            "ses_test1",
            "/home/user/my-project",
            Some("Test session"),
            None,
            1_700_000_000_000,
            Some(1_700_000_060_000),
        );

        let session = parse_opencode_session(storage, &session_file).unwrap();

        assert_eq!(session.session_id, "ses_test1");
        assert_eq!(session.source, SessionSource::OpenCode);
        assert_eq!(session.session_type, SessionType::User);
        assert_eq!(session.project_path, "/home/user/my-project");
        assert_eq!(session.project_name, "my-project");
        assert_eq!(session.summary.as_deref(), Some("Test session"));
        assert_eq!(session.message_count, 0);
        // end_time should come from session.time.updated when no messages
        assert_eq!(session.end_time, unix_ms_to_datetime(1_700_000_060_000));
    }

    #[test]
    fn test_parse_session_with_messages() {
        let temp = TempDir::new().unwrap();
        let storage = temp.path();

        let session_file = create_session_fixture(
            storage,
            "proj1",
            "ses_msg",
            "/home/user/project",
            None,
            None,
            1_700_000_000_000,
            None,
        );

        // User message
        create_message_fixture(storage, "ses_msg", "msg_u1", "user", 1_700_000_001_000);
        create_part_fixture(storage, "msg_u1", "prt_u1", "text", Some("hello world"));

        // Assistant message with tool
        create_message_fixture(storage, "ses_msg", "msg_a1", "assistant", 1_700_000_002_000);
        create_part_fixture(storage, "msg_a1", "prt_a1_text", "text", Some("I'll help"));
        create_part_fixture(storage, "msg_a1", "prt_a1_tool", "tool", None);

        let session = parse_opencode_session(storage, &session_file).unwrap();

        assert_eq!(session.message_count, 2);
        assert_eq!(session.assistant_message_count, 1);
        assert_eq!(session.tool_call_count, 1);
        assert_eq!(session.user_prompts, vec!["hello world"]);
        assert_eq!(session.starting_prompt.as_deref(), Some("hello world"));
        assert_eq!(session.user_message_timestamps.len(), 1);
        assert!(session.end_time.is_some());
    }

    #[test]
    fn test_parse_subagent_session() {
        let temp = TempDir::new().unwrap();
        let storage = temp.path();

        let session_file = create_session_fixture(
            storage,
            "proj1",
            "ses_child",
            "/home/user/project",
            None,
            Some("ses_parent"),
            1_700_000_000_000,
            None,
        );

        let session = parse_opencode_session(storage, &session_file).unwrap();

        assert_eq!(session.session_type, SessionType::Subagent);
        assert_eq!(session.parent_session_id.as_deref(), Some("ses_parent"));
    }

    #[test]
    fn test_parse_session_missing_messages_dir() {
        let temp = TempDir::new().unwrap();
        let storage = temp.path();

        let session_file = create_session_fixture(
            storage,
            "proj1",
            "ses_empty",
            "/home/user/project",
            None,
            None,
            1_700_000_000_000,
            None,
        );

        // No message dir created
        let session = parse_opencode_session(storage, &session_file).unwrap();

        assert_eq!(session.message_count, 0);
        assert!(session.user_prompts.is_empty());
        assert!(session.end_time.is_none());
    }

    #[test]
    fn test_scan_opencode_sessions() {
        let temp = TempDir::new().unwrap();
        let storage = temp.path();

        create_session_fixture(
            storage,
            "proj1",
            "ses_a",
            "/home/user/project-a",
            None,
            None,
            1_700_000_000_000,
            None,
        );
        create_session_fixture(
            storage,
            "proj2",
            "ses_b",
            "/home/user/project-b",
            None,
            None,
            1_700_000_100_000,
            None,
        );

        let sessions = scan_opencode_sessions(storage).unwrap();

        assert_eq!(sessions.len(), 2);
        // Sorted by start_time
        assert_eq!(sessions[0].session_id, "ses_a");
        assert_eq!(sessions[1].session_id, "ses_b");
    }

    #[test]
    fn test_scan_nonexistent_dir() {
        let result = scan_opencode_sessions(Path::new("/nonexistent")).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_user_prompts_limited() {
        let temp = TempDir::new().unwrap();
        let storage = temp.path();

        let session_file = create_session_fixture(
            storage,
            "proj1",
            "ses_many",
            "/home/user/project",
            None,
            None,
            1_700_000_000_000,
            None,
        );

        for i in 0..10 {
            let msg_id = format!("msg_u{i}");
            let part_id = format!("prt_u{i}");
            create_message_fixture(
                storage,
                "ses_many",
                &msg_id,
                "user",
                1_700_000_000_000 + i64::from(i) * 1000,
            );
            create_part_fixture(
                storage,
                &msg_id,
                &part_id,
                "text",
                Some(&format!("prompt {i}")),
            );
        }

        let session = parse_opencode_session(storage, &session_file).unwrap();

        assert_eq!(session.user_prompts.len(), MAX_USER_PROMPTS);
        assert_eq!(session.starting_prompt.as_deref(), Some("prompt 0"));
    }

    #[test]
    fn test_parse_session_with_invalid_timestamp() {
        let temp = TempDir::new().unwrap();
        let storage = temp.path();

        // Use an out-of-range timestamp (i64::MAX milliseconds)
        let session_file = create_session_fixture(
            storage,
            "proj1",
            "ses_bad_ts",
            "/home/user/project",
            None,
            None,
            i64::MAX,
            None,
        );

        let result = parse_opencode_session(storage, &session_file);
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), SessionError::InvalidTimestamp(ts) if ts == i64::MAX)
        );
    }

    #[test]
    fn test_end_time_none_when_equal_to_start_time() {
        let temp = TempDir::new().unwrap();
        let storage = temp.path();

        // updated == created → end_time should be None
        let session_file = create_session_fixture(
            storage,
            "proj1",
            "ses_same_ts",
            "/home/user/project",
            None,
            None,
            1_700_000_000_000,
            Some(1_700_000_000_000),
        );

        let session = parse_opencode_session(storage, &session_file).unwrap();
        assert!(session.end_time.is_none());
    }

    #[test]
    fn test_end_time_from_last_message_beats_updated() {
        let temp = TempDir::new().unwrap();
        let storage = temp.path();

        // session.updated is earlier than last message
        let session_file = create_session_fixture(
            storage,
            "proj1",
            "ses_msg_later",
            "/home/user/project",
            None,
            None,
            1_700_000_000_000,
            Some(1_700_000_010_000),
        );

        create_message_fixture(storage, "ses_msg_later", "msg_1", "user", 1_700_000_020_000);
        create_part_fixture(storage, "msg_1", "prt_1", "text", Some("hi"));

        let session = parse_opencode_session(storage, &session_file).unwrap();
        // end_time should be from last message (20s), not session.updated (10s)
        assert_eq!(session.end_time, unix_ms_to_datetime(1_700_000_020_000));
    }

    #[test]
    fn test_parse_session_malformed_json_file() {
        let temp = TempDir::new().unwrap();
        let storage = temp.path();

        let session_dir = storage.join("session").join("proj1");
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(session_dir.join("bad.json"), "not valid json").unwrap();

        let result = parse_opencode_session(storage, &session_dir.join("bad.json"));
        assert!(matches!(result, Err(SessionError::Json(_))));
    }

    #[test]
    fn test_parse_session_missing_required_fields() {
        let temp = TempDir::new().unwrap();
        let storage = temp.path();

        let session_dir = storage.join("session").join("proj1");
        fs::create_dir_all(&session_dir).unwrap();
        // Valid JSON but missing required fields (no id, directory, time)
        fs::write(session_dir.join("incomplete.json"), r#"{"title": "test"}"#).unwrap();

        let result = parse_opencode_session(storage, &session_dir.join("incomplete.json"));
        assert!(matches!(result, Err(SessionError::Json(_))));
    }

    #[test]
    fn test_scan_skips_malformed_sessions() {
        let temp = TempDir::new().unwrap();
        let storage = temp.path();

        // One good session
        create_session_fixture(
            storage,
            "proj1",
            "ses_good",
            "/home/user/project",
            None,
            None,
            1_700_000_000_000,
            None,
        );

        // One bad session (invalid JSON)
        let bad_dir = storage.join("session").join("proj1");
        fs::write(bad_dir.join("ses_bad.json"), "not json").unwrap();

        let sessions = scan_opencode_sessions(storage).unwrap();
        // Should only contain the good session
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "ses_good");
    }

    #[test]
    fn test_parse_session_with_messages_verifies_end_time() {
        let temp = TempDir::new().unwrap();
        let storage = temp.path();

        let session_file = create_session_fixture(
            storage,
            "proj1",
            "ses_verify_end",
            "/home/user/project",
            None,
            None,
            1_700_000_000_000,
            None,
        );

        create_message_fixture(
            storage,
            "ses_verify_end",
            "msg_u1",
            "user",
            1_700_000_001_000,
        );
        create_part_fixture(storage, "msg_u1", "prt_u1", "text", Some("hello"));
        create_message_fixture(
            storage,
            "ses_verify_end",
            "msg_a1",
            "assistant",
            1_700_000_005_000,
        );

        let session = parse_opencode_session(storage, &session_file).unwrap();

        // end_time should be the last message's timestamp
        assert_eq!(session.end_time, unix_ms_to_datetime(1_700_000_005_000));
    }

    #[test]
    fn test_end_time_none_when_updated_before_created() {
        let temp = TempDir::new().unwrap();
        let storage = temp.path();

        // session.updated < session.created (clock skew or data corruption)
        let session_file = create_session_fixture(
            storage,
            "proj1",
            "ses_skew",
            "/home/user/project",
            None,
            None,
            1_700_000_000_000,
            Some(1_699_999_000_000), // 1000 seconds before created
        );

        let session = parse_opencode_session(storage, &session_file).unwrap();
        assert!(
            session.end_time.is_none(),
            "end_time should be None when updated is before created"
        );
    }

    #[test]
    fn test_empty_session_id_rejected() {
        let temp = TempDir::new().unwrap();
        let storage = temp.path();

        let session_dir = storage.join("session").join("proj1");
        fs::create_dir_all(&session_dir).unwrap();

        let data = serde_json::json!({
            "id": "",
            "directory": "/home/user/project",
            "time": { "created": 1_700_000_000_000i64 }
        });
        let path = session_dir.join("empty_id.json");
        fs::write(&path, serde_json::to_string(&data).unwrap()).unwrap();

        let result = parse_opencode_session(storage, &path);
        assert!(matches!(result, Err(SessionError::EmptySessionId)));
    }
}

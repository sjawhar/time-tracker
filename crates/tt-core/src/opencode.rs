//! `OpenCode` session parsing.

use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use rusqlite::{Connection, OpenFlags, params};

use crate::session::{
    AgentSession, MAX_USER_MESSAGE_TIMESTAMPS, MAX_USER_PROMPTS, SessionError, SessionSource,
    SessionType, extract_project_name, truncate_prompt,
};

const MAX_TOOL_CALL_TIMESTAMPS: usize = 5000;

fn unix_ms_to_datetime(ms: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_millis_opt(ms).single()
}

/// Open a read-only connection to the per-session message/part shard if it exists.
///
/// The user's `OpenCode` fork shards messages and parts out of the monolithic
/// `opencode.db` into per-session `SQLite` files at `<sessions_dir>/<session_id>.db`.
/// Returns `None` if `sessions_dir` is unknown, the shard file is missing, the shard
/// fails to open, or the file isn't a valid `SQLite` database. Callers should fall
/// back to the monolithic connection.
fn open_session_shard(sessions_dir: Option<&Path>, session_id: &str) -> Option<Connection> {
    let path = sessions_dir?.join(format!("{session_id}.db"));
    if !path.exists() {
        return None;
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    match Connection::open_with_flags(&path, flags) {
        Ok(conn) => {
            if let Err(err) = conn.busy_timeout(Duration::from_secs(5)) {
                tracing::warn!(
                    path = ?path,
                    error = %err,
                    "failed to set OpenCode shard timeout"
                );
                return None;
            }
            // SQLite validates the file header lazily — a non-database file opens
            // successfully but fails on first query. Probe sqlite_master so corrupt
            // shards fall back to the monolithic connection instead of skipping the
            // session entirely.
            if let Err(err) = conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |row| {
                row.get::<_, i64>(0)
            }) {
                tracing::warn!(
                    path = ?path,
                    error = %err,
                    "OpenCode session shard is not a valid SQLite database"
                );
                return None;
            }
            Some(conn)
        }
        Err(err) => {
            tracing::warn!(
                path = ?path,
                error = %err,
                "failed to open OpenCode session shard"
            );
            None
        }
    }
}

#[derive(Debug)]
struct SessionRow {
    id: String,
    directory: String,
    title: String,
    parent_id: Option<String>,
    time_created: i64,
    time_updated: i64,
}

#[derive(Debug)]
struct MessageStats {
    user_message_count: i32,
    assistant_message_count: i32,
    user_prompts: Vec<String>,
    starting_prompt: Option<String>,
    user_message_timestamps: Vec<DateTime<Utc>>,
    last_message_time: Option<i64>,
}

#[expect(
    clippy::too_many_lines,
    reason = "two near-identical match arms for since/no-since are kept inline; refactoring would obscure the SQL"
)]
pub fn scan_opencode_sessions(
    db_path: &Path,
    since: Option<DateTime<Utc>>,
) -> Result<Vec<AgentSession>, SessionError> {
    // NO_MUTEX is safe: single connection used from a single thread (no rayon).
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = match Connection::open_with_flags(db_path, flags) {
        Ok(conn) => conn,
        Err(err) => {
            tracing::warn!(path = ?db_path, error = %err, "failed to open OpenCode database");
            return Ok(Vec::new());
        }
    };

    if let Err(err) = conn.busy_timeout(Duration::from_secs(5)) {
        tracing::warn!(path = ?db_path, error = %err, "failed to set OpenCode db timeout");
        return Ok(Vec::new());
    }

    let sessions_dir = db_path.parent().map(|p| p.join("sessions"));
    let mut sessions = Vec::new();

    if let Some(ts) = since {
        let mut stmt = match conn.prepare(
            "SELECT id, directory, title, parent_id, time_created, time_updated FROM session \
             WHERE time_updated > ?",
        ) {
            Ok(stmt) => stmt,
            Err(err) => {
                tracing::warn!(path = ?db_path, error = %err, "failed to query OpenCode sessions");
                return Ok(Vec::new());
            }
        };

        match stmt.query_map(params![ts.timestamp_millis()], |row| {
            Ok(SessionRow {
                id: row.get::<_, String>(0)?,
                directory: row.get::<_, String>(1)?,
                title: row.get::<_, String>(2)?,
                parent_id: row.get::<_, Option<String>>(3)?,
                time_created: row.get::<_, i64>(4)?,
                time_updated: row.get::<_, i64>(5)?,
            })
        }) {
            Ok(rows) => {
                for session_row in rows {
                    match session_row {
                        Ok(row) => {
                            if let Err(err) =
                                build_agent_session(&conn, sessions_dir.as_deref(), row)
                                    .map(|s| sessions.push(s))
                            {
                                tracing::warn!(error = %err, "skipping invalid OpenCode session");
                            }
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, "skipping invalid OpenCode session row");
                        }
                    }
                }
            }
            Err(err) => {
                tracing::warn!(path = ?db_path, error = %err, "failed to iterate OpenCode sessions");
                return Ok(Vec::new());
            }
        }
    } else {
        let mut stmt = match conn.prepare(
            "SELECT id, directory, title, parent_id, time_created, time_updated FROM session",
        ) {
            Ok(stmt) => stmt,
            Err(err) => {
                tracing::warn!(path = ?db_path, error = %err, "failed to query OpenCode sessions");
                return Ok(Vec::new());
            }
        };

        match stmt.query_map([], |row| {
            Ok(SessionRow {
                id: row.get::<_, String>(0)?,
                directory: row.get::<_, String>(1)?,
                title: row.get::<_, String>(2)?,
                parent_id: row.get::<_, Option<String>>(3)?,
                time_created: row.get::<_, i64>(4)?,
                time_updated: row.get::<_, i64>(5)?,
            })
        }) {
            Ok(rows) => {
                for session_row in rows {
                    match session_row {
                        Ok(row) => {
                            if let Err(err) =
                                build_agent_session(&conn, sessions_dir.as_deref(), row)
                                    .map(|s| sessions.push(s))
                            {
                                tracing::warn!(error = %err, "skipping invalid OpenCode session");
                            }
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, "skipping invalid OpenCode session row");
                        }
                    }
                }
            }
            Err(err) => {
                tracing::warn!(path = ?db_path, error = %err, "failed to iterate OpenCode sessions");
                return Ok(Vec::new());
            }
        }
    }

    sessions.sort_by_key(|e| e.start_time);
    Ok(sessions)
}

fn build_agent_session(
    main_conn: &Connection,
    sessions_dir: Option<&Path>,
    session_row: SessionRow,
) -> Result<AgentSession, SessionError> {
    if session_row.id.is_empty() {
        return Err(SessionError::EmptySessionId);
    }

    let shard_conn = open_session_shard(sessions_dir, &session_row.id);
    let stats_conn = shard_conn.as_ref().unwrap_or(main_conn);
    let message_stats = collect_message_stats(stats_conn, &session_row.id)?;
    let tool_call_count = count_tool_calls(stats_conn, &session_row.id)?;
    let tool_call_timestamps = collect_tool_call_timestamps(stats_conn, &session_row.id)?;
    let message_count = message_stats
        .user_message_count
        .saturating_add(message_stats.assistant_message_count);
    let start_time = unix_ms_to_datetime(session_row.time_created)
        .ok_or(SessionError::InvalidTimestamp(session_row.time_created))?;

    let end_ms = message_stats
        .last_message_time
        .map_or(session_row.time_updated, |msg| {
            msg.max(session_row.time_updated)
        });
    let end_time = unix_ms_to_datetime(end_ms).filter(|t| *t > start_time);

    let session_type = if session_row.parent_id.is_some() {
        SessionType::Subagent
    } else {
        SessionType::User
    };

    let summary = (!session_row.title.is_empty()).then_some(session_row.title);

    let project_name = extract_project_name(&session_row.directory);

    Ok(AgentSession {
        session_id: session_row.id,
        source: SessionSource::OpenCode,
        parent_session_id: session_row.parent_id,
        session_type,
        project_path: session_row.directory,
        project_name,
        start_time,
        end_time,
        message_count,
        summary,
        user_prompts: message_stats.user_prompts,
        starting_prompt: message_stats.starting_prompt,
        assistant_message_count: message_stats.assistant_message_count,
        tool_call_count,
        user_message_timestamps: message_stats.user_message_timestamps,
        tool_call_timestamps,
    })
}

fn collect_tool_call_timestamps(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<DateTime<Utc>>, rusqlite::Error> {
    let mut stmt = match conn.prepare_cached(&format!(
        "SELECT p.time_created FROM part p \
         JOIN message m ON p.message_id = m.id \
         WHERE p.session_id = ?1 AND json_valid(p.data) \
         AND json_extract(p.data, '$.type') = 'tool' \
         AND json_valid(m.data) \
         AND json_extract(m.data, '$.role') = 'assistant' \
         ORDER BY p.time_created \
         LIMIT {}",
        MAX_TOOL_CALL_TIMESTAMPS + 1
    )) {
        Ok(stmt) => stmt,
        Err(err) => {
            if is_missing_part_table(&err) {
                return Ok(Vec::new());
            }
            return Err(err);
        }
    };

    let rows = match stmt.query_map([session_id], |row| {
        let millis: i64 = row.get(0)?;
        Ok(DateTime::from_timestamp_millis(millis))
    }) {
        Ok(rows) => rows,
        Err(err) => {
            if is_missing_part_table(&err) {
                return Ok(Vec::new());
            }
            return Err(err);
        }
    };

    let mut timestamps: Vec<DateTime<Utc>> = rows.filter_map(|r| r.ok().flatten()).collect();
    let truncated = timestamps.len() > MAX_TOOL_CALL_TIMESTAMPS;
    if truncated {
        tracing::warn!(
            session_id,
            count = timestamps.len(),
            "tool call timestamps truncated at {MAX_TOOL_CALL_TIMESTAMPS}"
        );
        timestamps.truncate(MAX_TOOL_CALL_TIMESTAMPS);

        if let Ok(last_ms) = conn.query_row(
            "SELECT p.time_created FROM part p \
             JOIN message m ON p.message_id = m.id \
             WHERE p.session_id = ?1 AND json_valid(p.data) \
             AND json_extract(p.data, '$.type') = 'tool' \
             AND json_valid(m.data) \
             AND json_extract(m.data, '$.role') = 'assistant' \
             ORDER BY p.time_created DESC LIMIT 1",
            [session_id],
            |row| row.get::<_, i64>(0),
        ) {
            if let Some(last_ts) = DateTime::from_timestamp_millis(last_ms) {
                if timestamps.last() != Some(&last_ts) {
                    timestamps.push(last_ts);
                }
            }
        }
    }

    Ok(timestamps)
}

fn is_missing_part_table(err: &rusqlite::Error) -> bool {
    err.to_string().contains("no such table: part")
}

fn count_tool_calls(conn: &Connection, session_id: &str) -> Result<i32, SessionError> {
    let mut tool_stmt = conn.prepare_cached(
        "SELECT COUNT(*) FROM part p \
         JOIN message m ON p.message_id = m.id \
         WHERE p.session_id = ?1 AND json_valid(p.data) \
         AND json_extract(p.data, '$.type') = 'tool' \
         AND json_valid(m.data) \
         AND json_extract(m.data, '$.role') = 'assistant'",
    )?;
    let tool_count: i64 = tool_stmt.query_row([session_id], |row| row.get::<_, i64>(0))?;
    Ok(i32::try_from(tool_count).unwrap_or(i32::MAX))
}

fn collect_message_stats(
    conn: &Connection,
    session_id: &str,
) -> Result<MessageStats, SessionError> {
    let mut message_stmt = conn.prepare_cached(
        "SELECT id, time_created, \
                CASE WHEN json_valid(data) THEN json_extract(data, '$.role') END as role \
         FROM message WHERE session_id = ?1 ORDER BY time_created",
    )?;
    let mut part_stmt = conn.prepare_cached(
        "SELECT CASE WHEN json_valid(data) THEN json_extract(data, '$.text') END as text \
         FROM part WHERE message_id = ?1 AND json_valid(data) \
         AND json_extract(data, '$.type') = 'text' \
         ORDER BY id",
    )?;

    let mut stats = MessageStats {
        user_message_count: 0,
        assistant_message_count: 0,
        user_prompts: Vec::new(),
        starting_prompt: None,
        user_message_timestamps: Vec::new(),
        last_message_time: None,
    };

    let messages = message_stmt.query_map([session_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;

    for message in messages {
        let (message_id, created_ms, role) = message?;
        let Some(role) = role else {
            continue;
        };
        stats.last_message_time = Some(created_ms);
        match role.as_str() {
            "user" => {
                stats.user_message_count = stats.user_message_count.saturating_add(1);

                let text = collect_text_parts(&mut part_stmt, &message_id)?;
                if !text.is_empty() {
                    if stats.starting_prompt.is_none() {
                        stats.starting_prompt = Some(truncate_prompt(&text));
                    }
                    if stats.user_prompts.len() < MAX_USER_PROMPTS {
                        stats.user_prompts.push(truncate_prompt(&text));
                    }
                    if stats.user_message_timestamps.len() < MAX_USER_MESSAGE_TIMESTAMPS {
                        if let Some(ts) = unix_ms_to_datetime(created_ms) {
                            stats.user_message_timestamps.push(ts);
                        }
                    }
                }
            }
            "assistant" => {
                stats.assistant_message_count = stats.assistant_message_count.saturating_add(1);
            }
            _ => {}
        }
    }

    Ok(stats)
}

fn collect_text_parts(
    part_stmt: &mut rusqlite::Statement<'_>,
    message_id: &str,
) -> Result<String, SessionError> {
    let text_parts = part_stmt.query_map([message_id], |row| row.get::<_, Option<String>>(0))?;
    let mut text_values = Vec::new();
    for part in text_parts {
        if let Some(text) = part? {
            text_values.push(text);
        }
    }
    Ok(text_values.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_db() -> (TempDir, std::path::PathBuf) {
        let temp = TempDir::new().unwrap();
        let db_path = temp.path().join("opencode.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL DEFAULT '',
                parent_id TEXT,
                slug TEXT NOT NULL DEFAULT '',
                directory TEXT NOT NULL,
                title TEXT NOT NULL DEFAULT '',
                version TEXT NOT NULL DEFAULT '',
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL
            );
            CREATE TABLE message (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                data TEXT NOT NULL
            );
            CREATE INDEX message_session_idx ON message(session_id);
            CREATE TABLE part (
                id TEXT PRIMARY KEY,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                data TEXT NOT NULL
            );
            CREATE INDEX part_message_idx ON part(message_id);
            CREATE INDEX part_session_idx ON part(session_id);",
        )
        .unwrap();
        (temp, db_path)
    }

    fn insert_session(
        db_path: &Path,
        id: &str,
        directory: &str,
        title: &str,
        parent_id: Option<&str>,
        created_ms: i64,
        updated_ms: i64,
    ) {
        let conn = Connection::open(db_path).unwrap();
        conn.execute(
            "INSERT INTO session (id, directory, title, parent_id, time_created, time_updated)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            (id, directory, title, parent_id, created_ms, updated_ms),
        )
        .unwrap();
    }

    fn insert_message(db_path: &Path, id: &str, session_id: &str, role: &str, created_ms: i64) {
        let conn = Connection::open(db_path).unwrap();
        let data = serde_json::json!({ "role": role }).to_string();
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (id, session_id, created_ms, created_ms, data),
        )
        .unwrap();
    }

    fn insert_part(
        db_path: &Path,
        id: &str,
        message_id: &str,
        session_id: &str,
        part_type: &str,
        text: Option<&str>,
        created_ms: i64,
    ) {
        let conn = Connection::open(db_path).unwrap();
        let mut data = serde_json::json!({ "type": part_type });
        if let Some(value) = text {
            data["text"] = serde_json::Value::String(value.to_string());
        }
        let data = data.to_string();
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            (id, message_id, session_id, created_ms, created_ms, data),
        )
        .unwrap();
    }

    /// Create a per-session shard db at `<base_dir>/sessions/<session_id>.db` with the
    /// `message` and `part` tables (no `session` table — that lives in the monolithic db).
    fn create_test_shard(base_dir: &Path, session_id: &str) -> std::path::PathBuf {
        let sessions_dir = base_dir.join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        let shard_path = sessions_dir.join(format!("{session_id}.db"));
        let conn = Connection::open(&shard_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE message (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                time_created INTEGER,
                time_updated INTEGER,
                data TEXT NOT NULL
            );
            CREATE INDEX message_session_idx ON message(session_id);
            CREATE TABLE part (
                id TEXT PRIMARY KEY,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                time_created INTEGER,
                time_updated INTEGER,
                data TEXT NOT NULL
            );
            CREATE INDEX part_message_idx ON part(message_id);
            CREATE INDEX part_session_idx ON part(session_id);",
        )
        .unwrap();
        shard_path
    }

    #[test]
    fn test_basic_session() {
        let (_temp, db_path) = create_test_db();
        insert_session(
            &db_path,
            "ses_test1",
            "/home/user/my-project",
            "Test session",
            None,
            1_700_000_000_000,
            1_700_000_060_000,
        );

        let sessions = scan_opencode_sessions(&db_path, None).unwrap();
        assert_eq!(sessions.len(), 1);
        let session = &sessions[0];
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
    fn test_session_with_messages() {
        let (_temp, db_path) = create_test_db();
        insert_session(
            &db_path,
            "ses_msg",
            "/home/user/project",
            "",
            None,
            1_700_000_000_000,
            1_700_000_010_000,
        );
        // User message
        insert_message(&db_path, "msg_u1", "ses_msg", "user", 1_700_000_001_000);
        insert_part(
            &db_path,
            "prt_u1",
            "msg_u1",
            "ses_msg",
            "text",
            Some("hello world"),
            1_700_000_001_000,
        );

        // Assistant message with tool
        insert_message(
            &db_path,
            "msg_a1",
            "ses_msg",
            "assistant",
            1_700_000_002_000,
        );
        insert_part(
            &db_path,
            "prt_a1_text",
            "msg_a1",
            "ses_msg",
            "text",
            Some("I'll help"),
            1_700_000_002_000,
        );
        insert_part(
            &db_path,
            "prt_a1_tool",
            "msg_a1",
            "ses_msg",
            "tool",
            None,
            1_700_000_002_000,
        );

        let sessions = scan_opencode_sessions(&db_path, None).unwrap();
        let session = &sessions[0];

        assert_eq!(session.message_count, 2);
        assert_eq!(session.assistant_message_count, 1);
        assert_eq!(session.tool_call_count, 1);
        assert_eq!(session.user_prompts, vec!["hello world"]);
        assert_eq!(session.starting_prompt.as_deref(), Some("hello world"));
        assert_eq!(session.user_message_timestamps.len(), 1);
        assert!(session.end_time.is_some());
    }

    #[test]
    fn test_messages_and_parts_read_from_shard_when_present() {
        let (temp, db_path) = create_test_db();
        insert_session(
            &db_path,
            "ses_shard",
            "/home/user/project",
            "Sharded session",
            None,
            1_700_000_000_000,
            1_700_000_010_000,
        );
        // Note: NO messages/parts inserted into the monolithic db — the shard owns them.

        let shard_path = create_test_shard(temp.path(), "ses_shard");
        insert_message(
            &shard_path,
            "msg_u1",
            "ses_shard",
            "user",
            1_700_000_001_000,
        );
        insert_part(
            &shard_path,
            "prt_u1",
            "msg_u1",
            "ses_shard",
            "text",
            Some("hello from shard"),
            1_700_000_001_000,
        );
        insert_message(
            &shard_path,
            "msg_a1",
            "ses_shard",
            "assistant",
            1_700_000_002_000,
        );
        insert_part(
            &shard_path,
            "prt_a1_tool",
            "msg_a1",
            "ses_shard",
            "tool",
            None,
            1_700_000_002_000,
        );

        let sessions = scan_opencode_sessions(&db_path, None).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.session_id == "ses_shard")
            .expect("session should be found");

        assert_eq!(session.tool_call_count, 1);
        assert_eq!(session.assistant_message_count, 1);
        assert_eq!(session.user_prompts, vec!["hello from shard"]);
        assert_eq!(session.starting_prompt.as_deref(), Some("hello from shard"));
    }

    #[test]
    fn test_falls_back_to_monolithic_when_no_shard() {
        // Regression coverage for the absence path: when no shard file exists,
        // build_agent_session must read messages/parts from the monolithic db.
        let (temp, db_path) = create_test_db();
        insert_session(
            &db_path,
            "ses_mono",
            "/home/user/project",
            "",
            None,
            1_700_000_000_000,
            1_700_000_010_000,
        );
        insert_message(&db_path, "msg_u1", "ses_mono", "user", 1_700_000_001_000);
        insert_part(
            &db_path,
            "prt_u1",
            "msg_u1",
            "ses_mono",
            "text",
            Some("from monolithic"),
            1_700_000_001_000,
        );

        // Confirm no shard file exists at the expected path.
        assert!(!temp.path().join("sessions").join("ses_mono.db").exists());

        let sessions = scan_opencode_sessions(&db_path, None).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.session_id == "ses_mono")
            .expect("session should be found");
        assert_eq!(session.user_prompts, vec!["from monolithic"]);
    }

    #[test]
    fn test_shard_takes_precedence_over_monolithic_when_both_exist() {
        // If a shard exists, it owns the messages/parts for that session_id.
        // Decoy data in the monolithic db must be ignored — we are NOT a UNION.
        let (temp, db_path) = create_test_db();
        insert_session(
            &db_path,
            "ses_both",
            "/home/user/project",
            "",
            None,
            1_700_000_000_000,
            1_700_000_010_000,
        );
        // Decoy: monolithic has a user prompt AND a tool call.
        insert_message(
            &db_path,
            "msg_mono_u",
            "ses_both",
            "user",
            1_700_000_001_000,
        );
        insert_part(
            &db_path,
            "prt_mono_text",
            "msg_mono_u",
            "ses_both",
            "text",
            Some("decoy from monolithic"),
            1_700_000_001_000,
        );
        insert_message(
            &db_path,
            "msg_mono_a",
            "ses_both",
            "assistant",
            1_700_000_001_500,
        );
        insert_part(
            &db_path,
            "prt_mono_tool",
            "msg_mono_a",
            "ses_both",
            "tool",
            None,
            1_700_000_001_500,
        );

        // Authoritative: shard has just one user text message and zero tool calls.
        let shard_path = create_test_shard(temp.path(), "ses_both");
        insert_message(
            &shard_path,
            "msg_shard_u",
            "ses_both",
            "user",
            1_700_000_002_000,
        );
        insert_part(
            &shard_path,
            "prt_shard_text",
            "msg_shard_u",
            "ses_both",
            "text",
            Some("authoritative from shard"),
            1_700_000_002_000,
        );

        let sessions = scan_opencode_sessions(&db_path, None).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.session_id == "ses_both")
            .expect("session should be found");

        // Shard wins on both axes: prompts and tool count.
        assert_eq!(session.user_prompts, vec!["authoritative from shard"]);
        assert_eq!(session.tool_call_count, 0);
        assert_eq!(session.assistant_message_count, 0);
    }

    #[test]
    fn test_corrupt_shard_falls_back_to_monolithic() {
        // A non-SQLite "shard" file must not break the scan or drop the session.
        // We log a warning and degrade to reading from the monolithic db.
        let (temp, db_path) = create_test_db();
        insert_session(
            &db_path,
            "ses_corrupt",
            "/home/user/project",
            "",
            None,
            1_700_000_000_000,
            1_700_000_010_000,
        );
        insert_message(&db_path, "msg_u1", "ses_corrupt", "user", 1_700_000_001_000);
        insert_part(
            &db_path,
            "prt_u1",
            "msg_u1",
            "ses_corrupt",
            "text",
            Some("from monolithic"),
            1_700_000_001_000,
        );

        // Write garbage to the expected shard path.
        let sessions_dir = temp.path().join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::write(sessions_dir.join("ses_corrupt.db"), b"not a sqlite db").unwrap();

        let sessions = scan_opencode_sessions(&db_path, None).unwrap();
        let session = sessions
            .iter()
            .find(|s| s.session_id == "ses_corrupt")
            .expect("session should be found despite corrupt shard");
        assert_eq!(session.user_prompts, vec!["from monolithic"]);
    }

    #[test]
    fn test_tool_call_timestamps_collected() {
        let (_temp, db_path) = create_test_db();
        insert_session(
            &db_path,
            "ses_tool",
            "/home/user/project",
            "",
            None,
            1_700_000_000_000,
            1_700_000_010_000,
        );

        insert_message(
            &db_path,
            "msg_a1",
            "ses_tool",
            "assistant",
            1_700_000_002_000,
        );
        insert_part(
            &db_path,
            "prt_a1_tool",
            "msg_a1",
            "ses_tool",
            "tool",
            None,
            1_700_000_002_000,
        );
        insert_part(
            &db_path,
            "prt_a1_text",
            "msg_a1",
            "ses_tool",
            "text",
            Some("text"),
            1_700_000_002_000,
        );
        insert_message(
            &db_path,
            "msg_a2",
            "ses_tool",
            "assistant",
            1_700_000_003_000,
        );
        insert_part(
            &db_path,
            "prt_a2_tool",
            "msg_a2",
            "ses_tool",
            "tool",
            None,
            1_700_000_003_000,
        );

        let sessions = scan_opencode_sessions(&db_path, None).unwrap();
        let session = &sessions[0];

        assert_eq!(
            session.tool_call_timestamps,
            vec![
                unix_ms_to_datetime(1_700_000_002_000).unwrap(),
                unix_ms_to_datetime(1_700_000_003_000).unwrap(),
            ]
        );
    }

    #[test]
    fn test_collect_tool_call_timestamps_missing_table_returns_empty() {
        let conn = Connection::open_in_memory().unwrap();
        let timestamps = collect_tool_call_timestamps(&conn, "ses_missing").unwrap();
        assert!(timestamps.is_empty());
    }

    #[test]
    fn test_collect_tool_call_timestamps_filters_non_assistant() {
        let (_temp, db_path) = create_test_db();
        insert_session(
            &db_path,
            "ses_role",
            "/home/user/project",
            "",
            None,
            1_700_000_000_000,
            1_700_000_010_000,
        );
        insert_message(&db_path, "msg_user", "ses_role", "user", 1_700_000_001_000);
        insert_part(
            &db_path,
            "prt_user_tool",
            "msg_user",
            "ses_role",
            "tool",
            None,
            1_700_000_001_000,
        );
        insert_message(
            &db_path,
            "msg_assistant",
            "ses_role",
            "assistant",
            1_700_000_002_000,
        );
        insert_part(
            &db_path,
            "prt_assistant_tool",
            "msg_assistant",
            "ses_role",
            "tool",
            None,
            1_700_000_002_000,
        );

        let conn = Connection::open(&db_path).unwrap();
        let timestamps = collect_tool_call_timestamps(&conn, "ses_role").unwrap();

        assert_eq!(
            timestamps,
            vec![unix_ms_to_datetime(1_700_000_002_000).unwrap()]
        );
    }

    #[test]
    fn test_collect_tool_call_timestamps_preserves_last_when_truncated() {
        const MAX_TOOL_CALLS: usize = 5000;

        let (_temp, db_path) = create_test_db();
        insert_session(
            &db_path,
            "ses_many_tools",
            "/home/user/project",
            "",
            None,
            1_700_000_000_000,
            1_700_000_020_000,
        );
        insert_message(
            &db_path,
            "msg_assistant",
            "ses_many_tools",
            "assistant",
            1_700_000_001_000,
        );

        let conn = Connection::open(&db_path).unwrap();
        let tx = conn.unchecked_transaction().unwrap();
        let data = serde_json::json!({ "type": "tool" }).to_string();
        let base_ms = 1_700_000_010_000i64;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )
                .unwrap();
            for offset in 0..=MAX_TOOL_CALLS {
                let offset_ms = i64::try_from(offset).expect("tool call offset should fit in i64");
                let created_ms = base_ms + offset_ms;
                let part_id = format!("prt_tool_{offset}");
                stmt.execute((
                    part_id,
                    "msg_assistant",
                    "ses_many_tools",
                    created_ms,
                    created_ms,
                    &data,
                ))
                .unwrap();
            }
        }
        tx.commit().unwrap();

        let timestamps = collect_tool_call_timestamps(&conn, "ses_many_tools").unwrap();
        let last_offset =
            i64::try_from(MAX_TOOL_CALLS).expect("tool call offset should fit in i64");
        let last_expected = unix_ms_to_datetime(base_ms + last_offset).unwrap();

        assert!(
            timestamps.contains(&last_expected),
            "last tool call timestamp should be preserved"
        );
    }

    #[test]
    fn test_subagent_session() {
        let (_temp, db_path) = create_test_db();
        insert_session(
            &db_path,
            "ses_child",
            "/home/user/project",
            "",
            Some("ses_parent"),
            1_700_000_000_000,
            1_700_000_010_000,
        );

        let sessions = scan_opencode_sessions(&db_path, None).unwrap();
        let session = &sessions[0];

        assert_eq!(session.session_type, SessionType::Subagent);
        assert_eq!(session.parent_session_id.as_deref(), Some("ses_parent"));
    }

    #[test]
    fn test_session_with_no_messages() {
        let (_temp, db_path) = create_test_db();
        insert_session(
            &db_path,
            "ses_empty",
            "/home/user/project",
            "",
            None,
            1_700_000_000_000,
            1_700_000_000_000,
        );

        let sessions = scan_opencode_sessions(&db_path, None).unwrap();
        let session = &sessions[0];

        assert_eq!(session.message_count, 0);
        assert!(session.user_prompts.is_empty());
        assert!(session.end_time.is_none());
    }

    #[test]
    fn test_scan_multiple_sessions() {
        let (_temp, db_path) = create_test_db();

        insert_session(
            &db_path,
            "ses_a",
            "/home/user/project-a",
            "",
            None,
            1_700_000_000_000,
            1_700_000_000_000,
        );
        insert_session(
            &db_path,
            "ses_b",
            "/home/user/project-b",
            "",
            None,
            1_700_000_100_000,
            1_700_000_100_000,
        );

        let sessions = scan_opencode_sessions(&db_path, None).unwrap();

        assert_eq!(sessions.len(), 2);
        // Sorted by start_time
        assert_eq!(sessions[0].session_id, "ses_a");
        assert_eq!(sessions[1].session_id, "ses_b");
    }

    #[test]
    fn test_scan_since_none_returns_all_sessions() {
        let (_temp, db_path) = create_test_db();

        insert_session(
            &db_path,
            "ses_old",
            "/home/user/project-old",
            "",
            None,
            1_700_000_000_000,
            1_700_000_001_000,
        );
        insert_session(
            &db_path,
            "ses_new",
            "/home/user/project-new",
            "",
            None,
            1_700_000_100_000,
            1_700_000_101_000,
        );

        let sessions = scan_opencode_sessions(&db_path, None).unwrap();

        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].session_id, "ses_old");
        assert_eq!(sessions[1].session_id, "ses_new");
    }

    #[test]
    fn test_scan_since_very_old_timestamp_returns_all_sessions() {
        let (_temp, db_path) = create_test_db();

        insert_session(
            &db_path,
            "ses_a",
            "/home/user/project-a",
            "",
            None,
            1_700_000_000_000,
            1_700_000_010_000,
        );
        insert_session(
            &db_path,
            "ses_b",
            "/home/user/project-b",
            "",
            None,
            1_700_000_100_000,
            1_700_000_110_000,
        );

        let since = Utc.timestamp_millis_opt(1).single().unwrap();
        let sessions = scan_opencode_sessions(&db_path, Some(since)).unwrap();

        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn test_scan_since_between_two_sessions_returns_only_newer_sessions() {
        let (_temp, db_path) = create_test_db();

        insert_session(
            &db_path,
            "ses_before",
            "/home/user/project-before",
            "",
            None,
            1_700_000_000_000,
            1_700_000_010_000,
        );
        insert_session(
            &db_path,
            "ses_after",
            "/home/user/project-after",
            "",
            None,
            1_700_000_020_000,
            1_700_000_030_000,
        );

        let since = Utc
            .timestamp_millis_opt(1_700_000_015_000)
            .single()
            .unwrap();
        let sessions = scan_opencode_sessions(&db_path, Some(since)).unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "ses_after");
    }

    #[test]
    fn test_scan_since_exact_updated_boundary_excludes_equal_timestamp() {
        let (_temp, db_path) = create_test_db();

        insert_session(
            &db_path,
            "ses_exact",
            "/home/user/project-exact",
            "",
            None,
            1_700_000_000_000,
            1_700_000_020_000,
        );
        insert_session(
            &db_path,
            "ses_after",
            "/home/user/project-after",
            "",
            None,
            1_700_000_030_000,
            1_700_000_021_000,
        );

        let since = Utc
            .timestamp_millis_opt(1_700_000_020_000)
            .single()
            .unwrap();
        let sessions = scan_opencode_sessions(&db_path, Some(since)).unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "ses_after");
    }

    #[test]
    fn test_scan_since_very_recent_timestamp_returns_no_sessions() {
        let (_temp, db_path) = create_test_db();

        insert_session(
            &db_path,
            "ses_only",
            "/home/user/project",
            "",
            None,
            1_700_000_000_000,
            1_700_000_010_000,
        );

        let since = Utc
            .timestamp_millis_opt(1_800_000_000_000)
            .single()
            .unwrap();
        let sessions = scan_opencode_sessions(&db_path, Some(since)).unwrap();

        assert!(sessions.is_empty());
    }

    #[test]
    fn test_scan_since_includes_updated_old_session() {
        let (_temp, db_path) = create_test_db();

        insert_session(
            &db_path,
            "ses_old_but_updated",
            "/home/user/project",
            "",
            None,
            1_700_000_000_000,
            1_700_000_200_000,
        );

        let since = Utc
            .timestamp_millis_opt(1_700_000_100_000)
            .single()
            .unwrap();
        let sessions = scan_opencode_sessions(&db_path, Some(since)).unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "ses_old_but_updated");
    }

    #[test]
    fn test_scan_nonexistent_db() {
        let result = scan_opencode_sessions(Path::new("/nonexistent"), None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_user_prompts_limited() {
        let (_temp, db_path) = create_test_db();
        insert_session(
            &db_path,
            "ses_many",
            "/home/user/project",
            "",
            None,
            1_700_000_000_000,
            1_700_000_100_000,
        );
        for i in 0..10 {
            let msg_id = format!("msg_u{i}");
            let part_id = format!("prt_u{i}");
            let created_ms = 1_700_000_000_000 + i64::from(i) * 1000;
            insert_message(&db_path, &msg_id, "ses_many", "user", created_ms);
            insert_part(
                &db_path,
                &part_id,
                &msg_id,
                "ses_many",
                "text",
                Some(&format!("prompt {i}")),
                created_ms,
            );
        }

        let sessions = scan_opencode_sessions(&db_path, None).unwrap();
        let session = &sessions[0];

        assert_eq!(session.user_prompts.len(), MAX_USER_PROMPTS);
        assert_eq!(session.starting_prompt.as_deref(), Some("prompt 0"));
    }

    #[test]
    fn test_invalid_timestamp() {
        let (_temp, db_path) = create_test_db();
        let conn = Connection::open(&db_path).unwrap();
        let session_row = SessionRow {
            id: "ses_bad_ts".to_string(),
            directory: "/home/user/project".to_string(),
            title: String::new(),
            parent_id: None,
            time_created: i64::MAX,
            time_updated: i64::MAX,
        };

        let result = build_agent_session(&conn, None, session_row);
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), SessionError::InvalidTimestamp(ts) if ts == i64::MAX)
        );
    }

    #[test]
    fn test_end_time_none_when_equal_to_start_time() {
        let (_temp, db_path) = create_test_db();
        let conn = Connection::open(&db_path).unwrap();
        let session_row = SessionRow {
            id: "ses_same_ts".to_string(),
            directory: "/home/user/project".to_string(),
            title: String::new(),
            parent_id: None,
            time_created: 1_700_000_000_000,
            time_updated: 1_700_000_000_000,
        };

        let session = build_agent_session(&conn, None, session_row).unwrap();
        assert!(session.end_time.is_none());
    }

    #[test]
    fn test_end_time_from_last_message_beats_updated() {
        let (_temp, db_path) = create_test_db();
        insert_session(
            &db_path,
            "ses_msg_later",
            "/home/user/project",
            "",
            None,
            1_700_000_000_000,
            1_700_000_010_000,
        );

        insert_message(
            &db_path,
            "msg_1",
            "ses_msg_later",
            "user",
            1_700_000_020_000,
        );
        insert_part(
            &db_path,
            "prt_1",
            "msg_1",
            "ses_msg_later",
            "text",
            Some("hi"),
            1_700_000_020_000,
        );

        let conn = Connection::open(&db_path).unwrap();
        let session_row = SessionRow {
            id: "ses_msg_later".to_string(),
            directory: "/home/user/project".to_string(),
            title: String::new(),
            parent_id: None,
            time_created: 1_700_000_000_000,
            time_updated: 1_700_000_010_000,
        };
        let session = build_agent_session(&conn, None, session_row).unwrap();
        // end_time should be from last message (20s), not session.updated (10s)
        assert_eq!(session.end_time, unix_ms_to_datetime(1_700_000_020_000));
    }

    #[test]
    fn test_malformed_message_data() {
        let (_temp, db_path) = create_test_db();
        insert_session(
            &db_path,
            "ses_bad_msg",
            "/home/user/project",
            "",
            None,
            1_700_000_000_000,
            1_700_000_010_000,
        );

        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                "msg_bad",
                "ses_bad_msg",
                1_700_000_001_000i64,
                1_700_000_001_000i64,
                "not json",
            ),
        )
        .unwrap();

        let sessions = scan_opencode_sessions(&db_path, None).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].message_count, 0);
    }

    #[test]
    fn test_scan_skips_malformed_sessions() {
        let (_temp, db_path) = create_test_db();

        insert_session(
            &db_path,
            "ses_good",
            "/home/user/project",
            "",
            None,
            1_700_000_000_000,
            1_700_000_010_000,
        );
        insert_session(
            &db_path,
            "",
            "/home/user/bad",
            "",
            None,
            1_700_000_100_000,
            1_700_000_110_000,
        );

        let sessions = scan_opencode_sessions(&db_path, None).unwrap();
        // Should only contain the good session
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "ses_good");
    }

    #[test]
    fn test_parse_session_with_messages_verifies_end_time() {
        let (_temp, db_path) = create_test_db();
        insert_session(
            &db_path,
            "ses_verify_end",
            "/home/user/project",
            "",
            None,
            1_700_000_000_000,
            1_700_000_002_000,
        );

        insert_message(
            &db_path,
            "msg_u1",
            "ses_verify_end",
            "user",
            1_700_000_001_000,
        );
        insert_part(
            &db_path,
            "prt_u1",
            "msg_u1",
            "ses_verify_end",
            "text",
            Some("hello"),
            1_700_000_001_000,
        );
        insert_message(
            &db_path,
            "msg_a1",
            "ses_verify_end",
            "assistant",
            1_700_000_005_000,
        );

        let conn = Connection::open(&db_path).unwrap();
        let session_row = SessionRow {
            id: "ses_verify_end".to_string(),
            directory: "/home/user/project".to_string(),
            title: String::new(),
            parent_id: None,
            time_created: 1_700_000_000_000,
            time_updated: 1_700_000_002_000,
        };
        let session = build_agent_session(&conn, None, session_row).unwrap();

        // end_time should be the last message's timestamp
        assert_eq!(session.end_time, unix_ms_to_datetime(1_700_000_005_000));
    }

    #[test]
    fn test_end_time_none_when_updated_before_created() {
        let (_temp, db_path) = create_test_db();
        let conn = Connection::open(&db_path).unwrap();
        let session_row = SessionRow {
            id: "ses_skew".to_string(),
            directory: "/home/user/project".to_string(),
            title: String::new(),
            parent_id: None,
            time_created: 1_700_000_000_000,
            time_updated: 1_699_999_000_000,
        };

        let session = build_agent_session(&conn, None, session_row).unwrap();
        assert!(
            session.end_time.is_none(),
            "end_time should be None when updated is before created"
        );
    }

    #[test]
    fn test_empty_session_id_rejected() {
        let (_temp, db_path) = create_test_db();
        let conn = Connection::open(&db_path).unwrap();
        let session_row = SessionRow {
            id: String::new(),
            directory: "/home/user/project".to_string(),
            title: String::new(),
            parent_id: None,
            time_created: 1_700_000_000_000,
            time_updated: 1_700_000_000_000,
        };

        let result = build_agent_session(&conn, None, session_row);
        assert!(matches!(result, Err(SessionError::EmptySessionId)));
    }

    #[test]
    fn test_scan_corrupt_db() {
        let temp = TempDir::new().unwrap();
        let db_path = temp.path().join("opencode.db");
        fs::write(&db_path, "").unwrap();

        let sessions = scan_opencode_sessions(&db_path, None).unwrap();
        assert!(sessions.is_empty());
    }
}

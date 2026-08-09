//! `OpenCode` session parsing.

use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use rayon::prelude::*;
use rusqlite::{Connection, OpenFlags, params};

use crate::session::{
    AgentSession, MAX_USER_MESSAGE_TIMESTAMPS, MAX_USER_PROMPTS, ScanOutcome, SessionError,
    SessionSource, SessionType, extract_project_name, truncate_prompt,
};

const MAX_TOOL_CALL_TIMESTAMPS: usize = 5000;

fn unix_ms_to_datetime(ms: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_millis_opt(ms).single()
}

/// What a lookup for a session's per-session message/part shard found.
enum SessionShard {
    /// The shard opened and carries the message schema.
    Ready(Connection),
    /// The shard file exists and is a valid `SQLite` database, but holds no
    /// `message` table: the session was aborted before anything was written to
    /// it. Two of 321 local shards and 31 of 7,637 on devbox are in this state.
    /// Falling back to the monolith here would index a phantom zero-message
    /// session, so the caller skips it instead.
    Empty,
    /// No shard on disk, or it could not be read. Use the monolithic connection.
    Absent,
}

/// Open a read-only connection to the per-session message/part shard if it exists.
///
/// The user's `OpenCode` fork shards messages and parts out of the monolithic
/// `opencode.db` into per-session `SQLite` files at `<sessions_dir>/<session_id>.db`.
/// Returns `Absent` if `sessions_dir` is unknown, the shard file is missing, the
/// shard fails to open, or the file isn't a valid `SQLite` database — callers should
/// fall back to the monolithic connection.
fn open_session_shard(sessions_dir: Option<&Path>, session_id: &str) -> SessionShard {
    let Some(dir) = sessions_dir else {
        return SessionShard::Absent;
    };
    let path = dir.join(format!("{session_id}.db"));
    if !path.exists() {
        return SessionShard::Absent;
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
                return SessionShard::Absent;
            }
            // SQLite validates the file header lazily — a non-database file opens
            // successfully but fails on first query. Probing sqlite_master for the
            // `message` table answers both questions in one read: a corrupt shard
            // errors here and falls back to the monolithic connection, while a valid
            // but never-written shard reports no table and is skipped as empty.
            match conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'message')",
                [],
                |row| row.get::<_, bool>(0),
            ) {
                Ok(true) => SessionShard::Ready(conn),
                Ok(false) => SessionShard::Empty,
                Err(err) => {
                    tracing::warn!(
                        path = ?path,
                        error = %err,
                        "OpenCode session shard is not a valid SQLite database"
                    );
                    SessionShard::Absent
                }
            }
        }
        Err(err) => {
            tracing::warn!(
                path = ?path,
                error = %err,
                "failed to open OpenCode session shard"
            );
            SessionShard::Absent
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

/// Open a read-only connection to the monolithic `OpenCode` database.
///
/// Each rayon worker opens its own connection: `NO_MUTEX` is safe because a
/// connection is only ever used by the single thread that created it.
fn open_monolith_ro(db_path: &Path) -> Option<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    match Connection::open_with_flags(db_path, flags) {
        Ok(conn) => {
            if let Err(err) = conn.busy_timeout(Duration::from_secs(5)) {
                tracing::warn!(path = ?db_path, error = %err, "failed to set OpenCode db timeout");
                return None;
            }
            Some(conn)
        }
        Err(err) => {
            tracing::warn!(path = ?db_path, error = %err, "failed to open OpenCode database");
            None
        }
    }
}

/// Read all session metadata rows, optionally filtered to those updated after `since`.
fn collect_session_rows(
    conn: &Connection,
    since: Option<DateTime<Utc>>,
) -> Result<Vec<SessionRow>, rusqlite::Error> {
    let map_row = |row: &rusqlite::Row<'_>| {
        Ok(SessionRow {
            id: row.get::<_, String>(0)?,
            directory: row.get::<_, String>(1)?,
            title: row.get::<_, String>(2)?,
            parent_id: row.get::<_, Option<String>>(3)?,
            time_created: row.get::<_, i64>(4)?,
            time_updated: row.get::<_, i64>(5)?,
        })
    };

    if let Some(ts) = since {
        let mut stmt = conn.prepare(
            "SELECT id, directory, title, parent_id, time_created, time_updated FROM session \
             WHERE time_updated > ?",
        )?;
        let rows = stmt.query_map(params![ts.timestamp_millis()], map_row)?;
        rows.collect()
    } else {
        let mut stmt = conn.prepare(
            "SELECT id, directory, title, parent_id, time_created, time_updated FROM session",
        )?;
        let rows = stmt.query_map([], map_row)?;
        rows.collect()
    }
}

/// Scan `OpenCode` sessions from the monolithic database.
///
/// Reads every session row unless `since` bounds it. Callers on the ~30s ingest path
/// want [`scan_opencode_sessions_incremental`], which also reports whether the store
/// could be read in full.
pub fn scan_opencode_sessions(
    db_path: &Path,
    since: Option<DateTime<Utc>>,
) -> Result<Vec<AgentSession>, SessionError> {
    Ok(scan_opencode_sessions_incremental(db_path, since)?.sessions)
}

/// Scan `OpenCode` sessions, reporting whether the whole store could be read.
///
/// Session rows are read once, then `build_agent_session` runs across a rayon
/// thread pool — sessions are independent and read-only, so a many-core host
/// processes them in parallel rather than serially. Each worker reuses one
/// read-only monolith connection (for sessions without a per-session shard)
/// via `map_init`.
///
/// Three conditions make the scan incomplete, and all three previously presented as
/// an empty store: the monolith would not open, the session query failed, or a
/// session row would not build. A caller advancing a scan cursor must not do so on
/// any of them — see [`ScanOutcome`]. A session skipped as *empty* is expected and
/// leaves the scan complete.
pub fn scan_opencode_sessions_incremental(
    db_path: &Path,
    since: Option<DateTime<Utc>>,
) -> Result<ScanOutcome, SessionError> {
    let Some(conn) = open_monolith_ro(db_path) else {
        return Ok(ScanOutcome {
            sessions: Vec::new(),
            complete: false,
        });
    };

    let sessions_dir_buf = db_path.parent().map(|p| p.join("sessions"));
    let sessions_dir = sessions_dir_buf.as_deref();

    let rows = match collect_session_rows(&conn, since) {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!(path = ?db_path, error = %err, "failed to query OpenCode sessions");
            return Ok(ScanOutcome {
                sessions: Vec::new(),
                complete: false,
            });
        }
    };
    drop(conn);

    let built: Vec<BuiltSession> = rows
        .into_par_iter()
        .map_init(
            || open_monolith_ro(db_path),
            |thread_conn, row| {
                let Some(conn) = thread_conn.as_ref() else {
                    // The row was read but never examined, so the scan did not
                    // cover it.
                    return BuiltSession::UNREADABLE;
                };
                let session_id = row.id.clone();
                match build_agent_session(conn, sessions_dir, row) {
                    Ok(session) => BuiltSession::yielded(session),
                    Err(SessionError::EmptySession) => {
                        tracing::debug!(
                            session_id = %session_id,
                            "skipping empty OpenCode session"
                        );
                        BuiltSession::NOTHING_TO_INDEX
                    }
                    Err(err) => {
                        tracing::warn!(
                            session_id = %session_id,
                            error = %err,
                            "skipping invalid OpenCode session"
                        );
                        BuiltSession::DEFECTIVE
                    }
                }
            },
        )
        .collect();

    let complete = built.iter().all(|row| row.clean);
    let mut sessions: Vec<AgentSession> = built.into_iter().filter_map(|b| b.session).collect();

    sessions.sort_by_key(|e| e.start_time);
    Ok(ScanOutcome { sessions, complete })
}

/// One session row's contribution to a scan.
///
/// `clean` is false only when the row could not be examined or would not build. A
/// row skipped as empty is clean: an aborted session is an expected state, and
/// treating it as a defect would hold the scan cursor still forever.
struct BuiltSession {
    session: Option<AgentSession>,
    clean: bool,
}

impl BuiltSession {
    const NOTHING_TO_INDEX: Self = Self {
        session: None,
        clean: true,
    };
    const DEFECTIVE: Self = Self {
        session: None,
        clean: false,
    };
    const UNREADABLE: Self = Self {
        session: None,
        clean: false,
    };

    const fn yielded(session: AgentSession) -> Self {
        Self {
            session: Some(session),
            clean: true,
        }
    }
}

fn build_agent_session(
    main_conn: &Connection,
    sessions_dir: Option<&Path>,
    session_row: SessionRow,
) -> Result<AgentSession, SessionError> {
    if session_row.id.is_empty() {
        return Err(SessionError::EmptySessionId);
    }

    let shard = open_session_shard(sessions_dir, &session_row.id);
    let stats_conn = match &shard {
        SessionShard::Ready(conn) => conn,
        SessionShard::Empty => return Err(SessionError::EmptySession),
        SessionShard::Absent => main_conn,
    };
    let message_stats = collect_message_stats(stats_conn, &session_row.id)?;
    let tool_call_timestamps = collect_tool_call_timestamps(stats_conn, &session_row.id)?;
    // Derive the tool-call count from the timestamps we already fetched. Only
    // when the timestamp list hit its cap do we run the separate COUNT(*), so
    // the common case avoids a second scan of the session's `part` rows.
    let tool_call_count = if tool_call_timestamps.len() < MAX_TOOL_CALL_TIMESTAMPS {
        i32::try_from(tool_call_timestamps.len()).unwrap_or(i32::MAX)
    } else {
        count_tool_calls(stats_conn, &session_row.id)?
    };
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
    // One pass: join each message to its text parts (if any). The LEFT JOIN
    // keeps messages with no text part (assistant/tool-only messages) so the
    // message counts and `last_message_time` stay correct, collapsing what was
    // previously one `part` query per user message into a single query.
    let mut stmt = conn.prepare_cached(
        "SELECT m.id, m.time_created, \
                CASE WHEN json_valid(m.data) THEN json_extract(m.data, '$.role') END as role, \
                CASE WHEN json_valid(p.data) AND json_extract(p.data, '$.type') = 'text' \
                     THEN json_extract(p.data, '$.text') END as text \
         FROM message m \
         LEFT JOIN part p ON p.message_id = m.id \
             AND json_valid(p.data) AND json_extract(p.data, '$.type') = 'text' \
         WHERE m.session_id = ?1 \
         ORDER BY m.time_created, m.id, p.id",
    )?;

    let mut stats = MessageStats {
        user_message_count: 0,
        assistant_message_count: 0,
        user_prompts: Vec::new(),
        starting_prompt: None,
        user_message_timestamps: Vec::new(),
        last_message_time: None,
    };

    let rows = stmt.query_map([session_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;

    // Rows for the same message are contiguous (ordered by time, id). Buffer
    // each message's text parts, flushing when the message id changes.
    let mut cur_id: Option<String> = None;
    let mut cur_created: i64 = 0;
    let mut cur_role: Option<String> = None;
    let mut cur_texts: Vec<String> = Vec::new();

    for row in rows {
        let (id, created_ms, role, text) = row?;
        if cur_id.as_deref() != Some(id.as_str()) {
            if cur_id.is_some() {
                flush_message(&mut stats, cur_role.as_deref(), cur_created, &cur_texts);
            }
            cur_id = Some(id);
            cur_created = created_ms;
            cur_role = role;
            cur_texts.clear();
        }
        if let Some(text) = text {
            cur_texts.push(text);
        }
    }
    if cur_id.is_some() {
        flush_message(&mut stats, cur_role.as_deref(), cur_created, &cur_texts);
    }

    Ok(stats)
}

/// Fold a single grouped message into the running stats, mirroring the original
/// per-message logic: messages without a valid role are ignored entirely, and
/// harness-injected text is not treated as a user message at all.
fn flush_message(stats: &mut MessageStats, role: Option<&str>, created_ms: i64, texts: &[String]) {
    let Some(role) = role else {
        return;
    };
    stats.last_message_time = Some(created_ms);
    match role {
        "user" => {
            let text = texts.join("\n");
            if crate::injection::is_injected(&text) {
                // Injected text is the harness talking to the agent, not a
                // person: it must not count as a message, a prompt, or a
                // moment of attention. `last_message_time` still advanced —
                // the session was alive, just unattended.
                return;
            }
            stats.user_message_count = stats.user_message_count.saturating_add(1);
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

    /// Given a shard file that was created but never written to (zero bytes),
    /// When it is opened, Then it reports `Empty` — an aborted session, not a
    /// failure worth warning about.
    #[test]
    fn test_zero_byte_shard_is_empty_not_absent() {
        let temp = TempDir::new().unwrap();
        let sessions_dir = temp.path().join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::write(sessions_dir.join("ses_aborted.db"), b"").unwrap();

        let shard = open_session_shard(Some(&sessions_dir), "ses_aborted");
        assert!(matches!(shard, SessionShard::Empty));
    }

    /// Given a shard that is a valid `SQLite` database carrying no tables (the
    /// header-only shape an aborted session leaves behind), When it is opened,
    /// Then it reports `Empty`.
    #[test]
    fn test_header_only_shard_is_empty_not_absent() {
        let temp = TempDir::new().unwrap();
        let sessions_dir = temp.path().join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        let shard_path = sessions_dir.join("ses_headeronly.db");
        // Creating and dropping a connection leaves a valid but table-less file.
        Connection::open(&shard_path)
            .unwrap()
            .execute_batch("PRAGMA user_version = 0;")
            .unwrap();

        let shard = open_session_shard(Some(&sessions_dir), "ses_headeronly");
        assert!(matches!(shard, SessionShard::Empty));
    }

    /// Given a shard file holding bytes that are not a `SQLite` database at all,
    /// When it is opened, Then it reports `Absent` and never `Empty` — a genuine
    /// failure on a non-empty file must keep warning and fall back.
    #[test]
    fn test_corrupt_shard_is_absent_not_empty() {
        let temp = TempDir::new().unwrap();
        let sessions_dir = temp.path().join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::write(sessions_dir.join("ses_corrupt.db"), b"not a sqlite db").unwrap();

        let shard = open_session_shard(Some(&sessions_dir), "ses_corrupt");
        assert!(matches!(shard, SessionShard::Absent));
    }

    /// Given a session whose shard was never written to, When sessions are
    /// scanned, Then that session is skipped while its healthy neighbour is still
    /// returned — an empty shard must not be back-filled from the monolith as a
    /// phantom zero-message session.
    #[test]
    fn test_session_with_empty_shard_is_skipped_not_backfilled() {
        let (temp, db_path) = create_test_db();
        insert_session(
            &db_path,
            "ses_aborted",
            "/home/user/project",
            "",
            None,
            1_700_000_000_000,
            1_700_000_010_000,
        );
        insert_session(
            &db_path,
            "ses_healthy",
            "/home/user/project",
            "",
            None,
            1_700_000_000_000,
            1_700_000_010_000,
        );
        insert_message(&db_path, "msg_u1", "ses_healthy", "user", 1_700_000_001_000);
        insert_part(
            &db_path,
            "prt_u1",
            "msg_u1",
            "ses_healthy",
            "text",
            Some("real work"),
            1_700_000_001_000,
        );

        let sessions_dir = temp.path().join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        fs::write(sessions_dir.join("ses_aborted.db"), b"").unwrap();

        let sessions = scan_opencode_sessions(&db_path, None).unwrap();
        let ids: Vec<&str> = sessions.iter().map(|s| s.session_id.as_str()).collect();
        assert_eq!(ids, vec!["ses_healthy"]);
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

    /// Given a healthy store, When an incremental scan runs, Then it reports itself
    /// complete so the caller may advance its cursor.
    #[test]
    fn opencode_incremental_healthy_store_is_complete() {
        let (_temp, db_path) = create_test_db();
        insert_session(
            &db_path,
            "ses_ok",
            "/home/user/project",
            "",
            None,
            1_700_000_000_000,
            1_700_000_010_000,
        );

        let outcome = scan_opencode_sessions_incremental(&db_path, None).unwrap();

        assert_eq!(outcome.sessions.len(), 1);
        assert!(outcome.complete);
    }

    /// Given a store that cannot be opened, When a scan runs, Then it reports itself
    /// INCOMPLETE rather than empty.
    ///
    /// This is the defect a cursor turns into data loss. `scan_opencode_sessions`
    /// answers an unopenable store with `Ok(vec![])`, which is indistinguishable from
    /// "nothing changed"; advancing the cursor on it would skip that window of
    /// sessions permanently.
    #[test]
    fn opencode_unreadable_store_is_incomplete_not_empty() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("not-a-database.db");
        std::fs::write(&db_path, b"this is not sqlite").unwrap();

        let outcome = scan_opencode_sessions_incremental(&db_path, None).unwrap();

        assert!(outcome.sessions.is_empty());
        assert!(
            !outcome.complete,
            "an unreadable store must hold the cursor back"
        );
    }

    /// Given a store whose `session` table is missing, When a scan runs, Then the
    /// failed query is reported as incomplete rather than as an empty store.
    #[test]
    fn opencode_failed_query_is_incomplete_not_empty() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("no-session-table.db");
        Connection::open(&db_path)
            .unwrap()
            .execute_batch("CREATE TABLE unrelated (id TEXT);")
            .unwrap();

        let outcome = scan_opencode_sessions_incremental(&db_path, None).unwrap();

        assert!(outcome.sessions.is_empty());
        assert!(!outcome.complete);
    }

    /// Given a cursor, When an incremental scan runs, Then only sessions updated
    /// after it are returned, and the scan is still complete.
    #[test]
    fn opencode_incremental_filters_by_since() {
        let (_temp, db_path) = create_test_db();
        insert_session(
            &db_path,
            "ses_before",
            "/home/user/before",
            "",
            None,
            1_700_000_000_000,
            1_700_000_010_000,
        );
        insert_session(
            &db_path,
            "ses_after",
            "/home/user/after",
            "",
            None,
            1_700_000_020_000,
            1_700_000_030_000,
        );
        let since = Utc
            .timestamp_millis_opt(1_700_000_015_000)
            .single()
            .unwrap();

        let outcome = scan_opencode_sessions_incremental(&db_path, Some(since)).unwrap();

        assert_eq!(outcome.sessions.len(), 1);
        assert_eq!(outcome.sessions[0].session_id, "ses_after");
        assert!(outcome.complete);
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

    /// Builds a session whose user messages are exactly `texts`, one per minute.
    fn session_with_user_texts(texts: &[&str]) -> AgentSession {
        let (_temp, db_path) = create_test_db();
        insert_session(
            &db_path,
            "ses_inject",
            "/home/user/project",
            "",
            None,
            1_700_000_000_000,
            1_700_000_600_000,
        );
        for (i, text) in texts.iter().enumerate() {
            let msg_id = format!("msg_u{i}");
            let part_id = format!("prt_u{i}");
            let created_ms = 1_700_000_000_000 + i64::try_from(i).unwrap() * 60_000;
            insert_message(&db_path, &msg_id, "ses_inject", "user", created_ms);
            insert_part(
                &db_path,
                &part_id,
                &msg_id,
                "ses_inject",
                "text",
                Some(text),
                created_ms,
            );
        }
        scan_opencode_sessions(&db_path, None)
            .unwrap()
            .into_iter()
            .find(|s| s.session_id == "ses_inject")
            .expect("session should be scanned")
    }

    #[test]
    fn test_system_reminder_message_produces_no_user_message_timestamp() {
        let session = session_with_user_texts(&[
            "<system-reminder>\n[BACKGROUND TASK COMPLETED]\n**ID:** `bg_f60bcb1c`",
        ]);

        assert!(session.user_message_timestamps.is_empty());
        assert!(session.user_prompts.is_empty());
        assert!(session.starting_prompt.is_none());
        assert_eq!(session.message_count, 0);
    }

    #[test]
    fn test_boulder_continuation_directive_produces_no_user_message_timestamp() {
        let session = session_with_user_texts(&[
            "[SYSTEM DIRECTIVE: OH-MY-OPENCODE - BOULDER CONTINUATION]\n\nContinue the plan.",
        ]);

        assert!(session.user_message_timestamps.is_empty());
        assert!(session.user_prompts.is_empty());
        assert_eq!(session.message_count, 0);
    }

    #[test]
    fn test_skill_instruction_message_produces_a_user_message_timestamp() {
        // Regression guard: real human intent must survive the denylist.
        let session = session_with_user_texts(&[
            "<skill-instruction>\nUse the using-jj skill for version control.",
        ]);

        assert_eq!(session.user_message_timestamps.len(), 1);
        assert_eq!(session.user_prompts.len(), 1);
        assert!(session.starting_prompt.is_some());
    }

    #[test]
    fn test_mode_banner_messages_produce_user_message_timestamps() {
        let session = session_with_user_texts(&[
            "[analyze-mode]\nANALYSIS MODE. Gather context before diving deep.",
            "[CONTEXT]\nWe are mid-refactor of the allocation algorithm.",
            "[search-mode]\nMAXIMIZE SEARCH EFFORT.",
        ]);

        assert_eq!(session.user_message_timestamps.len(), 3);
        assert_eq!(session.user_prompts.len(), 3);
        assert_eq!(session.message_count, 3);
    }

    #[test]
    fn test_session_of_only_injected_messages_has_no_user_message_timestamps() {
        // Such a session carries no human attention at all, which is correct.
        let session = session_with_user_texts(&[
            "<system-reminder>\nbackground task finished",
            "---\n\n[SYSTEM DIRECTIVE: OH-MY-OPENCODE - SINGLE TASK ONLY]\n\nProceed.",
            "[NOTIFICATION from agent (reply-to: ses_abc)]\nstatus update",
            "<local-command-caveat>Caveat: the messages below were generated",
            "This session is being continued from a previous conversation.",
        ]);

        assert!(session.user_message_timestamps.is_empty());
        assert!(session.user_prompts.is_empty());
        assert!(session.starting_prompt.is_none());
        assert_eq!(session.message_count, 0);
    }

    #[test]
    fn test_starting_prompt_is_the_first_human_message_not_the_first_injection() {
        let session = session_with_user_texts(&[
            "<system-reminder>\nbackground task finished",
            "actually fix the allocation bug",
        ]);

        assert_eq!(
            session.starting_prompt.as_deref(),
            Some("actually fix the allocation bug")
        );
        assert_eq!(session.user_message_timestamps.len(), 1);
        assert_eq!(session.message_count, 1);
    }

    #[test]
    fn test_injected_message_still_advances_session_end_time() {
        // An injection proves the session was alive even though nobody was
        // paying attention, so it must not shorten the session's lifetime.
        let session =
            session_with_user_texts(&["real work", "<system-reminder>\nbackground task finished"]);

        assert_eq!(session.user_message_timestamps.len(), 1);
        assert_eq!(session.end_time, unix_ms_to_datetime(1_700_000_600_000));
    }
}

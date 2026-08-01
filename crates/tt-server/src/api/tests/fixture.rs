use anyhow::Result;
use chrono::{DateTime, Duration, TimeZone, Utc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tt_cli::Config;
use tt_core::{AgentSession, EventType, SessionSource, SessionType};
use tt_db::{Database, StoredEvent, Stream};

use crate::{
    ServerEvent,
    api::{ApiState, router},
};

#[path = "fixture/rail.rs"]
mod rail;

pub(super) fn fixture_app() -> Result<(axum::Router, tempfile::NamedTempFile, tempfile::TempDir)> {
    let (app, database_file, todo_store, _events) = fixture_app_with_events()?;
    Ok((app, database_file, todo_store))
}

pub(super) fn fixture_app_with_events() -> Result<(
    axum::Router,
    tempfile::NamedTempFile,
    tempfile::TempDir,
    broadcast::Receiver<ServerEvent>,
)> {
    let database_file = tempfile::NamedTempFile::new()?;
    let todo_store = tempfile::TempDir::new()?;
    let db = Database::open(database_file.path())?;
    let start = timestamp(0);
    insert_stream(&db, "alpha", start)?;
    insert_stream(&db, "beta", timestamp(1))?;
    insert_session(&db, "linked-session", SessionType::Agent)?;
    insert_session(&db, "subagent-session", SessionType::Subagent)?;
    insert_event(
        &db,
        "focus",
        start,
        EventType::TmuxPaneFocus,
        Some("alpha"),
        None,
        None,
        None,
    )?;
    insert_event(
        &db,
        "user-message",
        timestamp(1),
        EventType::UserMessage,
        Some("alpha"),
        Some("human-session"),
        None,
        None,
    )?;
    insert_event(
        &db,
        "session-start",
        timestamp(2),
        EventType::AgentSession,
        Some("alpha"),
        Some("linked-session"),
        Some("started"),
        Some("todo_link"),
    )?;
    insert_event(
        &db,
        "tool-use",
        timestamp(3),
        EventType::AgentToolUse,
        Some("alpha"),
        Some("linked-session"),
        None,
        None,
    )?;
    insert_event(
        &db,
        "subagent-start",
        timestamp(3),
        EventType::AgentSession,
        Some("beta"),
        Some("subagent-session"),
        Some("started"),
        None,
    )?;
    insert_event(
        &db,
        "session-end",
        timestamp(4),
        EventType::AgentSession,
        Some("alpha"),
        Some("linked-session"),
        Some("ended"),
        Some("todo_link"),
    )?;
    rail::populate(&db, todo_store.path())?;
    drop(db);

    let config = Config {
        database_path: database_file.path().to_path_buf(),
        todo_store_path: todo_store.path().to_path_buf(),
        ..Config::default()
    };
    let (events, receiver) = broadcast::channel::<ServerEvent>(1);
    let app = router(ApiState {
        database_path: database_file.path().to_path_buf(),
        config,
        events,
    });
    Ok((app, database_file, todo_store, receiver))
}

pub(super) fn timestamp(minute: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0).unwrap() + Duration::minutes(minute)
}

pub(super) fn insert_stream(db: &Database, id: &str, updated_at: DateTime<Utc>) -> Result<()> {
    db.insert_stream(&Stream {
        id: id.to_owned(),
        name: Some(format!("{id} stream")),
        slug: Some(id.to_owned()),
        description: Some(format!("{id} description")),
        color: Some("#aabbcc".to_owned()),
        created_at: timestamp(0),
        updated_at,
        time_direct_ms: 0,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: false,
    })?;
    Ok(())
}

pub(super) fn insert_session(db: &Database, id: &str, session_type: SessionType) -> Result<()> {
    db.upsert_agent_session(
        &AgentSession {
            session_id: id.to_owned(),
            source: SessionSource::Claude,
            parent_session_id: None,
            session_type,
            project_path: "/test".to_owned(),
            project_name: "test".to_owned(),
            start_time: timestamp(0),
            end_time: Some(timestamp(4)),
            message_count: 0,
            summary: None,
            user_prompts: Vec::new(),
            starting_prompt: None,
            assistant_message_count: 0,
            tool_call_count: 0,
            user_message_timestamps: Vec::new(),
            tool_call_timestamps: Vec::new(),
        },
        None,
    )?;
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "the fixture represents independent StoredEvent fields"
)]
pub(super) fn insert_event(
    db: &Database,
    id: &str,
    timestamp: DateTime<Utc>,
    event_type: EventType,
    stream_id: Option<&str>,
    session_id: Option<&str>,
    action: Option<&str>,
    assignment_source: Option<&str>,
) -> Result<()> {
    db.insert_event(&StoredEvent {
        id: id.to_owned(),
        timestamp,
        event_type,
        source: "test".to_owned(),
        machine_id: None,
        schema_version: 1,
        pane_id: None,
        tmux_session: None,
        window_index: None,
        git_project: None,
        git_workspace: None,
        status: None,
        idle_duration_ms: None,
        window_app_id: None,
        window_title: None,
        action: action.map(str::to_owned),
        cwd: None,
        session_id: session_id.map(str::to_owned),
        stream_id: stream_id.map(str::to_owned),
        assignment_source: assignment_source.map(str::to_owned),
        data: serde_json::json!({}),
    })?;
    Ok(())
}

pub(super) async fn request(app: axum::Router, path: &str) -> Result<String> {
    send_request(
        app,
        RequestSpec {
            method: "GET",
            path,
            body: None,
        },
    )
    .await
}

pub(super) async fn post(app: axum::Router, path: &str) -> Result<String> {
    send_request(
        app,
        RequestSpec {
            method: "POST",
            path,
            body: None,
        },
    )
    .await
}

pub(super) async fn post_json(app: axum::Router, path: &str, body: &str) -> Result<String> {
    send_request(
        app,
        RequestSpec {
            method: "POST",
            path,
            body: Some(body),
        },
    )
    .await
}

struct RequestSpec<'a> {
    method: &'a str,
    path: &'a str,
    body: Option<&'a str>,
}

async fn send_request(app: axum::Router, request: RequestSpec<'_>) -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    tokio::task::yield_now().await;

    let mut stream = TcpStream::connect(address).await?;
    let request = request.body.map_or_else(
        || {
            format!(
                "{} {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                request.method, request.path
            )
        },
        |body| {
            format!(
            "{} {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            request.method,
            request.path,
            body.len()
            )
        },
    );
    stream.write_all(request.as_bytes()).await?;
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;

    server.abort();
    let _ = server.await;
    Ok(response)
}

pub(super) fn response_body(response: &str) -> &str {
    response
        .split_once("\r\n\r\n")
        .expect("HTTP response includes headers and a body")
        .1
}

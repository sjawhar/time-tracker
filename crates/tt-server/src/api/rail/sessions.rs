use std::collections::HashMap;

use anyhow::{Context, Result};
use axum::{Json, extract::State};
use chrono::{Duration, Local, Utc};
use serde::Serialize;

use super::super::{ApiError, ApiState};

#[derive(Serialize)]
pub(super) struct SessionsResponse {
    sessions: Vec<SessionResponse>,
}

#[derive(Serialize)]
struct SessionResponse {
    harness: String,
    session_id: String,
    stream: Option<SessionStreamResponse>,
    machine_label: Option<String>,
    start_time: chrono::DateTime<Utc>,
    duration_ms: i64,
    last_activity: chrono::DateTime<Utc>,
    linked_todo_text: Option<String>,
}

#[derive(Serialize)]
struct SessionStreamResponse {
    name: Option<String>,
    slug: Option<String>,
}

pub(super) async fn handler(
    State(state): State<ApiState>,
) -> Result<Json<SessionsResponse>, ApiError> {
    let now = Utc::now();
    let timeout = Duration::milliseconds(tt_core::AllocationConfig::default().agent_timeout_ms);
    let cutoff = now - timeout;
    let database_path = state.database_path;
    let config = state.config;
    let response = tokio::task::spawn_blocking(move || {
        let db = tt_db::Database::open(&database_path).context("open sessions database")?;
        let active_sessions = db
            .active_agent_sessions(cutoff)
            .context("load active agent sessions")?;
        let machine_labels: HashMap<_, _> = db
            .list_machines()
            .context("load machine labels")?
            .into_iter()
            .map(|machine| (machine.machine_id, machine.label))
            .collect();
        let todo_view =
            tt_cli::commands::todo::top_todo_view(&config, now.with_timezone(&Local).date_naive())
                .context("load todo-session links")?;

        let mut sessions = Vec::with_capacity(active_sessions.len());
        for active in active_sessions {
            let stream = active
                .stream_id
                .as_deref()
                .map(|stream_id| {
                    db.get_stream(stream_id)
                        .context("load active session stream")
                        .map(|stream| {
                            stream.map(|stream| SessionStreamResponse {
                                name: stream.name,
                                slug: stream.slug,
                            })
                        })
                })
                .transpose()?
                .flatten();
            sessions.push(SessionResponse {
                harness: active.session.source.as_str().to_owned(),
                session_id: active.session.session_id.clone(),
                stream,
                machine_label: active
                    .machine_id
                    .as_ref()
                    .and_then(|machine_id| machine_labels.get(machine_id).cloned()),
                start_time: active.session.start_time,
                duration_ms: (active.last_activity - active.session.start_time)
                    .num_milliseconds()
                    .max(0),
                last_activity: active.last_activity,
                linked_todo_text: todo_view
                    .linked_todo_texts_by_session
                    .get(&active.session.session_id)
                    .cloned(),
            });
        }
        Ok(SessionsResponse { sessions })
    })
    .await
    .map_err(|error| {
        ApiError::Sessions(anyhow::Error::new(error).context("sessions task panicked"))
    })?
    .map_err(ApiError::Sessions)?;
    Ok(Json(response))
}

#[derive(serde::Deserialize)]
pub(super) struct LinkSessionRequest {
    todo_id: String,
}

#[derive(Serialize)]
pub(super) struct LinkSessionResponse {
    session_id: String,
    todo_id: String,
    status: &'static str,
}

/// Links an agent session to a todo, applying the todo's stream to the session.
///
/// The same operation as `tt todo link` with an explicit `--session`: a human's own
/// mapping, not an inference, which is why the todo's stream may be propagated.
pub(super) async fn link(
    State(state): State<ApiState>,
    axum::extract::Path(session_id): axum::extract::Path<String>,
    Json(request): Json<LinkSessionRequest>,
) -> Result<Json<LinkSessionResponse>, ApiError> {
    let database_path = state.database_path;
    let config = state.config;
    let events = state.events;
    let todo_id = request.todo_id.clone();
    let session_for_task = session_id.clone();
    let todo_for_task = todo_id.clone();
    tokio::task::spawn_blocking(move || -> Result<(), ApiError> {
        let db = tt_db::Database::open(&database_path)
            .map_err(|error| ApiError::Sessions(anyhow::Error::new(error).context("open db")))?;
        tt_cli::commands::todo::run_link(Some(&db), &config, &todo_for_task, Some(session_for_task))
            // Every failure here names a condition the caller can address (an unknown or
            // ambiguous todo id), so it comes back as the request's fault, not the server's.
            .map_err(|error| ApiError::BadRequest(error.to_string()))
    })
    .await
    .map_err(|error| {
        ApiError::Sessions(anyhow::Error::new(error).context("link session task panicked"))
    })??;
    match events.send(crate::ServerEvent::EventsAppended { count: 1 }) {
        Ok(_) | Err(_) => {}
    }
    Ok(Json(LinkSessionResponse {
        session_id,
        todo_id,
        status: "linked",
    }))
}

/// Removes an agent-session link from a todo, exactly as `tt todo unlink` does.
pub(super) async fn unlink(
    State(state): State<ApiState>,
    axum::extract::Path(session_id): axum::extract::Path<String>,
    Json(request): Json<LinkSessionRequest>,
) -> Result<Json<LinkSessionResponse>, ApiError> {
    let config = state.config;
    let todo_id = request.todo_id.clone();
    let session_for_task = session_id.clone();
    let todo_for_task = todo_id.clone();
    tokio::task::spawn_blocking(move || -> Result<(), ApiError> {
        tt_cli::commands::todo::run_unlink(&config, &todo_for_task, Some(session_for_task))
            .map_err(|error| ApiError::BadRequest(error.to_string()))
    })
    .await
    .map_err(|error| {
        ApiError::Sessions(anyhow::Error::new(error).context("unlink session task panicked"))
    })??;
    Ok(Json(LinkSessionResponse {
        session_id,
        todo_id,
        status: "unlinked",
    }))
}

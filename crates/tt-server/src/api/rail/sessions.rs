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

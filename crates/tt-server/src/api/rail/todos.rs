use anyhow::{Context, Result};
use axum::{Json, extract::State};
use chrono::Local;
use serde::Serialize;
use tt_core::todos::{Priority, PriorityStatus, StreamPriorityLink, Todo};

use super::super::{ApiError, ApiState};

#[derive(Serialize)]
pub(super) struct TodosResponse {
    todos: Vec<TodoResponse>,
}

#[derive(Serialize)]
struct TodoResponse {
    id: String,
    text: String,
    section: &'static str,
    priorities: Vec<TodoPriorityResponse>,
    stream_slug: Option<String>,
    due: Option<chrono::NaiveDate>,
    when: Option<chrono::NaiveDate>,
    linked_agent_count: usize,
}

#[derive(Serialize)]
struct TodoPriorityResponse {
    slug: String,
    value: i32,
}

pub(super) async fn handler(
    State(state): State<ApiState>,
) -> Result<Json<TodosResponse>, ApiError> {
    let config = state.config;
    let today = Local::now().date_naive();
    let response = tokio::task::spawn_blocking(move || {
        let todo_view =
            tt_cli::commands::todo::top_todo_view(&config, today).context("load ranked todos")?;
        Ok(TodosResponse {
            todos: todo_view
                .ranked_todos
                .into_iter()
                .map(|ranked| TodoResponse {
                    id: ranked.todo.id.clone(),
                    text: ranked.todo.text.clone(),
                    section: ranked.section.as_str(),
                    priorities: priority_links(
                        &ranked.todo,
                        &todo_view.priorities,
                        &todo_view.stream_links,
                    ),
                    stream_slug: ranked.todo.stream.clone(),
                    due: ranked.todo.due,
                    when: ranked.todo.when,
                    linked_agent_count: ranked.todo.sessions.len(),
                })
                .collect(),
        })
    })
    .await
    .map_err(|error| ApiError::Todos(anyhow::Error::new(error).context("todos task panicked")))?
    .map_err(ApiError::Todos)?;
    Ok(Json(response))
}

fn priority_links(
    todo: &Todo,
    priorities: &[Priority],
    stream_links: &[StreamPriorityLink],
) -> Vec<TodoPriorityResponse> {
    priorities
        .iter()
        .filter(|priority| priority.status == PriorityStatus::Active)
        .filter(|priority| {
            todo.priority.iter().any(|slug| slug == &priority.slug)
                || todo.stream.as_ref().is_some_and(|stream| {
                    stream_links
                        .iter()
                        .any(|link| link.stream == *stream && link.priority == priority.slug)
                })
        })
        .map(|priority| TodoPriorityResponse {
            slug: priority.slug.clone(),
            value: priority.value,
        })
        .collect()
}

#[derive(serde::Deserialize)]
pub(super) struct SetTodoStreamRequest {
    /// Stream reference (id, slug, or exact name); null clears the link.
    stream: Option<String>,
}

#[derive(Serialize)]
pub(super) struct SetTodoStreamResponse {
    todo_id: String,
    stream_slug: Option<String>,
    status: &'static str,
}

/// Sets or clears the stream a todo serves — the same operation as `tt todo stream`.
///
/// Which stream a todo serves is the human's to name, and this endpoint is that
/// naming with the CLI friction removed: the picker proposes nothing, the person
/// clicking decides. A reference matching no stream is refused, never minted.
///
/// One divergence from the CLI: a chosen stream that carries no slug is given one,
/// derived mechanically from its display name, because `todo.stream` is read as a
/// slug and only ~25 of 2,121 live streams have one — a picker that refuses 99% of
/// its own choices is the CLI friction this endpoint exists to remove. Deriving an
/// identifier from a name the human is looking at is bookkeeping, not attribution.
pub(super) async fn set_stream(
    State(state): State<ApiState>,
    axum::extract::Path(todo_id): axum::extract::Path<String>,
    Json(request): Json<SetTodoStreamRequest>,
) -> Result<Json<SetTodoStreamResponse>, ApiError> {
    let config = state.config;
    let database_path = state.database_path;
    let events = state.events;
    let todo_for_task = todo_id.clone();
    let stream_slug = tokio::task::spawn_blocking(move || -> Result<Option<String>, ApiError> {
        let db = tt_db::Database::open(&database_path)
            .map_err(|error| ApiError::Todos(anyhow::Error::new(error).context("open db")))?;
        let slug = match request.stream.as_deref() {
            None => None,
            Some(reference) => {
                let stream = db
                    .resolve_stream(reference)
                    .map_err(|error| {
                        ApiError::Todos(anyhow::Error::new(error).context("resolve stream"))
                    })?
                    .ok_or_else(|| {
                        ApiError::BadRequest(format!("no stream matching '{reference}'"))
                    })?;
                let slug = match stream.slug {
                    Some(slug) => slug,
                    None => ensure_slug(&db, &stream.id, stream.name.as_deref())?,
                };
                Some(slug)
            }
        };
        tt_cli::commands::todo::run_set_stream(&config, Some(&db), &todo_for_task, slug.as_deref())
            // Remaining failures name conditions the caller can address: an unknown or
            // ambiguous todo id.
            .map_err(|error| ApiError::BadRequest(error.to_string()))?;
        Ok(slug)
    })
    .await
    .map_err(|error| {
        ApiError::Todos(anyhow::Error::new(error).context("set todo stream task panicked"))
    })??;
    match events.send(crate::ServerEvent::EventsAppended { count: 1 }) {
        Ok(_) | Err(_) => {}
    }
    Ok(Json(SetTodoStreamResponse {
        todo_id,
        stream_slug,
        status: "set",
    }))
}

/// Gives a slugless stream a slug derived from its display name.
///
/// Kebab-cased, truncated, and suffixed with a counter when taken — an identifier,
/// never a judgement. A stream with no name at all falls back to its id prefix.
fn ensure_slug(
    db: &tt_db::Database,
    stream_id: &str,
    name: Option<&str>,
) -> Result<String, ApiError> {
    let base: String = name
        .unwrap_or(stream_id)
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .take(48)
        .collect();
    let base = base.trim_end_matches('-').to_string();
    let base = if base.is_empty() {
        stream_id.chars().take(8).collect()
    } else {
        base
    };
    let mut candidate = base.clone();
    for suffix in 2..100 {
        match db.set_stream_slug(stream_id, &candidate) {
            Ok(()) => return Ok(candidate),
            Err(tt_db::DbError::SlugTaken { .. }) => {
                candidate = format!("{base}-{suffix}");
            }
            Err(error) => {
                return Err(ApiError::Todos(
                    anyhow::Error::new(error).context("set generated slug"),
                ));
            }
        }
    }
    Err(ApiError::Todos(anyhow::anyhow!(
        "could not find a free slug for stream {stream_id}"
    )))
}

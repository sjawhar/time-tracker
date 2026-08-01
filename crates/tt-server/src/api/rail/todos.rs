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

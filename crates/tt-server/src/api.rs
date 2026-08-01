use std::path::PathBuf;
use std::time::Duration as StdDuration;

use anyhow::{Context, Result};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::broadcast;

use crate::{ServerEvent, sse};

mod rail;

#[derive(Clone)]
pub struct ApiState {
    pub database_path: PathBuf,
    pub config: tt_cli::Config,
    pub events: broadcast::Sender<ServerEvent>,
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("invalid request: {0}")]
    BadRequest(String),
    #[error("resource not found: {0}")]
    NotFound(String),
    #[error("request conflict: {0}")]
    Conflict(String),
    #[error("status unavailable")]
    Status(#[source] anyhow::Error),
    #[error("timeline unavailable")]
    Timeline(#[source] anyhow::Error),
    #[error("todos unavailable")]
    Todos(#[source] anyhow::Error),
    #[error("sessions unavailable")]
    Sessions(#[source] anyhow::Error),
    #[error("proposals unavailable")]
    Proposals(#[source] anyhow::Error),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message).into_response(),
            Self::NotFound(message) => (StatusCode::NOT_FOUND, message).into_response(),
            Self::Conflict(message) => (StatusCode::CONFLICT, message).into_response(),
            error => {
                tracing::error!(error = ?error, "API request failed");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct TimelineQuery {
    before: Option<String>,
    duration: Option<String>,
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/status", get(status))
        .route("/api/timeline", get(timeline))
        .route("/api/sse", get(sse))
        .merge(rail::router())
        .with_state(state)
        .fallback(crate::web::static_handler)
}

async fn status(State(state): State<ApiState>) -> Result<Json<tt_cli::drift::Verdict>, ApiError> {
    let verdict = tokio::task::spawn_blocking(move || {
        let db = tt_db::Database::open(&state.database_path)?;
        tt_cli::drift::compute_verdict(&db, &state.config, chrono::Utc::now())
            .context("compute status verdict")
    })
    .await
    .map_err(|error| ApiError::Status(anyhow::Error::new(error).context("status task panicked")))?
    .map_err(ApiError::Status)?;
    Ok(Json(verdict))
}

/// Returns `snake_case` timeline JSON: `window` (`start`, exclusive `end` RFC 3339 timestamps),
/// `streams_active` (each with `stream`, interval lists, and event markers), `idle_gaps`, and
/// `db_version`.
async fn timeline(
    State(state): State<ApiState>,
    Query(query): Query<TimelineQuery>,
) -> Result<Json<tt_db::TimelineData>, ApiError> {
    let window = resolve_timeline_window(query, Utc::now())?;
    let idle_threshold = chrono::Duration::minutes(i64::from(state.config.idle_threshold_min));
    let database_path = state.database_path;
    let data = tokio::task::spawn_blocking(move || -> Result<_> {
        let db = tt_db::Database::open(&database_path).context("open timeline database")?;
        tt_db::timeline_for_window(
            &db,
            window,
            &tt_core::AllocationConfig::default(),
            idle_threshold,
        )
        .context("assemble timeline data")
    })
    .await
    .map_err(|error| {
        ApiError::Timeline(anyhow::Error::new(error).context("timeline task panicked"))
    })?
    .map_err(ApiError::Timeline)?;
    Ok(Json(data))
}

fn resolve_timeline_window(
    query: TimelineQuery,
    now: DateTime<Utc>,
) -> Result<tt_db::TimelineWindow, ApiError> {
    let end = query.before.map_or_else(
        || Ok(now),
        |before| {
            DateTime::parse_from_rfc3339(&before)
                .map(|timestamp| timestamp.with_timezone(&Utc))
                .map_err(|_| {
                    ApiError::BadRequest(format!(
                        "invalid before query parameter `{before}`: expected an ISO 8601 timestamp"
                    ))
                })
        },
    )?;
    let duration = query.duration.map_or_else(
        || Ok(StdDuration::from_secs(24 * 60 * 60)),
        |duration| {
            humantime::parse_duration(&duration).map_err(|error| {
                ApiError::BadRequest(format!(
                    "invalid duration query parameter `{duration}`: {error}"
                ))
            })
        },
    )?;
    let duration = Duration::from_std(duration).map_err(|_| {
        ApiError::BadRequest("invalid duration query parameter: duration is too large".to_owned())
    })?;
    let start = end.checked_sub_signed(duration).ok_or_else(|| {
        ApiError::BadRequest(
            "invalid duration query parameter: window start is out of range".to_owned(),
        )
    })?;
    Ok(tt_db::TimelineWindow { start, end })
}

async fn sse(State(state): State<ApiState>) -> impl IntoResponse {
    sse::response(state.events.subscribe())
}

#[cfg(test)]
#[path = "api/tests.rs"]
mod tests;

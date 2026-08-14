//! Write endpoints for correcting attribution from the dashboard.
//!
//! Every write here records a human's verdict, so events land with
//! `assignment_source = 'user'` through the same primitives `tt streams assign`
//! and `tt streams merge` use — the dashboard is a second doorway to the existing
//! correction surface, never a second inference engine. Each mutation notifies the
//! SSE channel so every open tab refetches.

use anyhow::Result;
use axum::{
    Json,
    extract::{Path, State},
};
use serde::{Deserialize, Serialize};
use tt_db::{Database, DbError, MergeMode};

use crate::ServerEvent;

use super::super::{ApiError, ApiState};

#[derive(Deserialize)]
pub(super) struct PatchStreamRequest {
    /// New display name. Whitespace-only is refused; names are not unique.
    name: Option<String>,
    /// New description; an empty string clears it, matching `tt streams describe`.
    description: Option<String>,
    /// New color; an empty string clears it.
    color: Option<String>,
    #[serde(default)]
    add_tags: Vec<String>,
    #[serde(default)]
    remove_tags: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct PatchStreamResponse {
    stream_id: String,
    name: Option<String>,
    description: Option<String>,
    color: Option<String>,
    tags: Vec<String>,
}

#[derive(Deserialize)]
pub(super) struct MergeStreamsRequest {
    /// Source stream references (id, slug, or exact name), merged into the path id.
    sources: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct MergedSourceResponse {
    stream_id: String,
    events_moved: u64,
    user_events_moved: u64,
    tags_moved: u64,
    retired: bool,
}

#[derive(Serialize)]
pub(super) struct MergeStreamsResponse {
    target_stream_id: String,
    sources: Vec<MergedSourceResponse>,
}

#[derive(Deserialize)]
pub(super) struct AssignEventsRequest {
    stream_id: String,
    event_ids: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct AssignEventsResponse {
    stream_id: String,
    events_assigned: u64,
}

pub(super) async fn patch(
    State(state): State<ApiState>,
    Path(stream_id): Path<String>,
    Json(request): Json<PatchStreamRequest>,
) -> Result<Json<PatchStreamResponse>, ApiError> {
    if let Some(name) = &request.name
        && name.trim().is_empty()
    {
        return Err(ApiError::BadRequest(
            "a stream name cannot be empty".to_string(),
        ));
    }
    let database_path = state.database_path;
    let events = state.events;
    let response = tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let db = Database::open(&database_path).map_err(streams_error)?;
        if db.get_stream(&stream_id).map_err(streams_error)?.is_none() {
            return Err(ApiError::NotFound(format!(
                "stream '{stream_id}' does not exist"
            )));
        }
        if let Some(name) = &request.name {
            db.rename_stream(&stream_id, name).map_err(streams_error)?;
        }
        if let Some(description) = &request.description {
            db.set_stream_description(&stream_id, description)
                .map_err(streams_error)?;
        }
        if let Some(color) = &request.color {
            let value = (!color.trim().is_empty()).then_some(color.as_str());
            db.set_stream_color(&stream_id, value)
                .map_err(streams_error)?;
        }
        for tag in &request.add_tags {
            db.add_tag(&stream_id, tag).map_err(streams_error)?;
        }
        for tag in &request.remove_tags {
            db.delete_tag(&stream_id, tag).map_err(streams_error)?;
        }
        let stream = db
            .get_stream(&stream_id)
            .map_err(streams_error)?
            .ok_or_else(|| ApiError::NotFound(format!("stream '{stream_id}' does not exist")))?;
        let tags = db.get_tags(&stream_id).map_err(streams_error)?;
        Ok(PatchStreamResponse {
            stream_id,
            name: stream.name,
            description: stream.description,
            color: stream.color,
            tags,
        })
    })
    .await
    .map_err(|error| {
        ApiError::Streams(anyhow::Error::new(error).context("stream patch task panicked"))
    })??;
    notify_database_change(&events);
    Ok(Json(response))
}

pub(super) async fn merge(
    State(state): State<ApiState>,
    Path(stream_id): Path<String>,
    Json(request): Json<MergeStreamsRequest>,
) -> Result<Json<MergeStreamsResponse>, ApiError> {
    if request.sources.is_empty() {
        return Err(ApiError::BadRequest(
            "a merge needs at least one source stream".to_string(),
        ));
    }
    let database_path = state.database_path;
    let events = state.events;
    let response = tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let db = Database::open(&database_path).map_err(streams_error)?;
        let mut source_ids = Vec::with_capacity(request.sources.len());
        for reference in &request.sources {
            let stream = db
                .resolve_stream(reference)
                .map_err(streams_error)?
                .ok_or_else(|| ApiError::NotFound(format!("no stream matching '{reference}'")))?;
            source_ids.push(stream.id);
        }
        let merged = db
            .merge_streams(&source_ids, &stream_id, MergeMode::Apply)
            .map_err(|error| match error {
                DbError::MergeIntoSelf { stream_id } => ApiError::BadRequest(format!(
                    "stream '{stream_id}' cannot be merged into itself"
                )),
                DbError::MergeTargetNotFound { stream_id } => {
                    ApiError::NotFound(format!("stream '{stream_id}' does not exist"))
                }
                error => streams_error(error),
            })?;
        Ok(MergeStreamsResponse {
            target_stream_id: stream_id,
            sources: merged
                .into_iter()
                .map(|source| MergedSourceResponse {
                    stream_id: source.stream_id,
                    events_moved: source.events_moved,
                    user_events_moved: source.user_events_moved,
                    tags_moved: source.tags_moved,
                    retired: source.retired,
                })
                .collect(),
        })
    })
    .await
    .map_err(|error| {
        ApiError::Streams(anyhow::Error::new(error).context("stream merge task panicked"))
    })??;
    notify_database_change(&events);
    Ok(Json(response))
}

pub(super) async fn assign_events(
    State(state): State<ApiState>,
    Json(request): Json<AssignEventsRequest>,
) -> Result<Json<AssignEventsResponse>, ApiError> {
    if request.event_ids.is_empty() {
        return Err(ApiError::BadRequest(
            "an assignment needs at least one event id".to_string(),
        ));
    }
    let database_path = state.database_path;
    let events = state.events;
    let response = tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let db = Database::open(&database_path).map_err(streams_error)?;
        let stream = db
            .resolve_stream(&request.stream_id)
            .map_err(streams_error)?
            .ok_or_else(|| {
                ApiError::NotFound(format!("no stream matching '{}'", request.stream_id))
            })?;
        // Explicit ids carry the human's verdict, so this is the unguarded pair-wise
        // write `tt streams assign --event` uses — it may overwrite an earlier
        // correction, because a human must be able to change their own mind.
        let pairs: Vec<(String, String)> = request
            .event_ids
            .iter()
            .map(|event_id| (event_id.clone(), stream.id.clone()))
            .collect();
        let assigned = db
            .assign_events_to_stream(&pairs, "user")
            .map_err(streams_error)?;
        Ok(AssignEventsResponse {
            stream_id: stream.id,
            events_assigned: assigned,
        })
    })
    .await
    .map_err(|error| {
        ApiError::Streams(anyhow::Error::new(error).context("event assign task panicked"))
    })??;
    notify_database_change(&events);
    Ok(Json(response))
}

fn streams_error(error: impl Into<anyhow::Error>) -> ApiError {
    ApiError::Streams(error.into().context("write stream"))
}

fn notify_database_change(events: &tokio::sync::broadcast::Sender<ServerEvent>) {
    match events.send(ServerEvent::EventsAppended { count: 1 }) {
        Ok(_) | Err(_) => {}
    }
}

#[derive(Serialize)]
pub(super) struct StreamListEntry {
    id: String,
    name: Option<String>,
    slug: Option<String>,
    last_active: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Serialize)]
pub(super) struct StreamListResponse {
    streams: Vec<StreamListEntry>,
}

/// Lists every stream, most recently active first, for pickers.
///
/// Recency comes from `events` — never from `streams.first_event_at`/`last_event_at`,
/// which nothing writes. A stream with no events sorts last rather than disappearing:
/// a picker's job is to offer every legitimate choice.
pub(super) async fn list(
    State(state): State<ApiState>,
) -> Result<Json<StreamListResponse>, ApiError> {
    let database_path = state.database_path;
    let response = tokio::task::spawn_blocking(move || -> Result<_, ApiError> {
        let db = Database::open(&database_path).map_err(streams_error)?;
        let windows = db.stream_activity_windows().map_err(streams_error)?;
        let mut streams: Vec<StreamListEntry> = db
            .get_streams()
            .map_err(streams_error)?
            .into_iter()
            .map(|stream| StreamListEntry {
                last_active: windows.get(&stream.id).map(|window| window.last),
                id: stream.id,
                name: stream.name,
                slug: stream.slug,
            })
            .collect();
        streams.sort_by_key(|entry| std::cmp::Reverse(entry.last_active));
        Ok(StreamListResponse { streams })
    })
    .await
    .map_err(|error| {
        ApiError::Streams(anyhow::Error::new(error).context("stream list task panicked"))
    })??;
    Ok(Json(response))
}

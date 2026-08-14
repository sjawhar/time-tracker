use axum::{
    Router,
    routing::{get, patch, post},
};

use super::ApiState;

mod proposals;
mod sessions;
mod streams;
mod todos;

pub(super) fn router() -> Router<ApiState> {
    Router::new()
        .route("/api/todos", get(todos::handler))
        .route("/api/todos/{id}/stream", post(todos::set_stream))
        .route("/api/streams", get(streams::list))
        .route("/api/sessions", get(sessions::handler))
        .route("/api/sessions/{id}/link", post(sessions::link))
        .route("/api/sessions/{id}/unlink", post(sessions::unlink))
        .route("/api/proposals", get(proposals::handler))
        .route("/api/proposals/{id}/accept", post(proposals::accept))
        .route("/api/proposals/{id}/reject", post(proposals::reject))
        .route("/api/streams/{id}", patch(streams::patch))
        .route("/api/streams/{id}/merge", post(streams::merge))
        .route("/api/events/assign", post(streams::assign_events))
}

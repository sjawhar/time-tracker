use axum::{
    Router,
    routing::{get, post},
};

use super::ApiState;

mod proposals;
mod sessions;
mod todos;

pub(super) fn router() -> Router<ApiState> {
    Router::new()
        .route("/api/todos", get(todos::handler))
        .route("/api/sessions", get(sessions::handler))
        .route("/api/proposals", get(proposals::handler))
        .route("/api/proposals/{id}/accept", post(proposals::accept))
        .route("/api/proposals/{id}/reject", post(proposals::reject))
}

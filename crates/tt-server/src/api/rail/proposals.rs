use anyhow::{Context, Result, bail};
use axum::{
    Json,
    extract::{Path, State},
};
use serde::{Deserialize, Serialize};
use tt_db::{
    AcceptProposalOutcome, Database, DbError, Proposal, ProposalStatus, RejectProposalOutcome,
};

use crate::ServerEvent;

use super::super::{ApiError, ApiState};

#[derive(Serialize)]
pub(super) struct ProposalsResponse {
    proposals: Vec<ProposalResponse>,
}

#[derive(Serialize)]
struct ProposalResponse {
    id: String,
    created_at: chrono::DateTime<chrono::Utc>,
    target: ProposalTargetResponse,
    confidence: f64,
    reasoning: String,
    scope: ProposalScopeResponse,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ProposalTargetResponse {
    Existing {
        stream_id: String,
        name: Option<String>,
        slug: Option<String>,
    },
    New {
        name: String,
        description: Option<String>,
    },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ProposalScopeResponse {
    Session { count: usize },
    Events { count: usize },
}

#[derive(Deserialize)]
struct ProposedNewStream {
    name: String,
    description: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct RejectProposalRequest {
    stream: Option<String>,
}

#[derive(Serialize)]
pub(super) struct AcceptProposalResponse {
    proposal_id: String,
    status: &'static str,
    stream_id: String,
    created_stream: bool,
    events_assigned: u64,
}

#[derive(Serialize)]
pub(super) struct RejectProposalResponse {
    proposal_id: String,
    status: &'static str,
    stream_id: Option<String>,
    events_assigned: u64,
}

pub(super) async fn handler(
    State(state): State<ApiState>,
) -> Result<Json<ProposalsResponse>, ApiError> {
    let database_path = state.database_path;
    let response = tokio::task::spawn_blocking(move || {
        let db = Database::open(&database_path).context("open proposals database")?;
        let proposals = db
            .get_proposals(Some(ProposalStatus::Pending))
            .context("load pending classifier proposals")?;
        proposals
            .into_iter()
            .map(|proposal| proposal_response(&db, proposal))
            .collect::<Result<Vec<_>>>()
            .map(|proposals| ProposalsResponse { proposals })
    })
    .await
    .map_err(|error| {
        ApiError::Proposals(anyhow::Error::new(error).context("proposals task panicked"))
    })?
    .map_err(ApiError::Proposals)?;
    Ok(Json(response))
}

pub(super) async fn accept(
    State(state): State<ApiState>,
    Path(proposal_id): Path<String>,
) -> Result<Json<AcceptProposalResponse>, ApiError> {
    let database_path = state.database_path;
    let events = state.events;
    let proposal_id_for_task = proposal_id.clone();
    let outcome = tokio::task::spawn_blocking(move || -> Result<AcceptProposalOutcome, DbError> {
        let db = Database::open(&database_path)?;
        db.accept_proposal(&proposal_id_for_task)
    })
    .await
    .map_err(|error| {
        ApiError::Proposals(anyhow::Error::new(error).context("accept proposal task panicked"))
    })?
    .map_err(proposal_write_error)?;
    notify_database_change(&events);

    Ok(Json(AcceptProposalResponse {
        proposal_id,
        status: "accepted",
        stream_id: outcome.stream_id,
        created_stream: outcome.created_stream,
        events_assigned: outcome.events_assigned,
    }))
}

pub(super) async fn reject(
    State(state): State<ApiState>,
    Path(proposal_id): Path<String>,
    body: Option<Json<RejectProposalRequest>>,
) -> Result<Json<RejectProposalResponse>, ApiError> {
    let database_path = state.database_path;
    let events = state.events;
    let target_stream = body.and_then(|Json(body)| body.stream);
    let proposal_id_for_task = proposal_id.clone();
    let outcome = tokio::task::spawn_blocking(move || -> Result<RejectProposalOutcome, DbError> {
        let db = Database::open(&database_path)?;
        db.reject_proposal(&proposal_id_for_task, target_stream.as_deref())
    })
    .await
    .map_err(|error| {
        ApiError::Proposals(anyhow::Error::new(error).context("reject proposal task panicked"))
    })?
    .map_err(proposal_write_error)?;
    notify_database_change(&events);

    Ok(Json(RejectProposalResponse {
        proposal_id,
        status: "rejected",
        stream_id: outcome.stream_id,
        events_assigned: outcome.events_assigned,
    }))
}

fn proposal_write_error(error: DbError) -> ApiError {
    match error {
        DbError::ProposalNotFound { proposal_id } => {
            ApiError::NotFound(format!("proposal '{proposal_id}' does not exist"))
        }
        DbError::ProposalNotPending { proposal_id } => {
            ApiError::Conflict(format!("proposal '{proposal_id}' is not pending"))
        }
        error @ (DbError::InvalidProposalEventIds(_)
        | DbError::InvalidProposalNewStream(_)
        | DbError::InvalidProposalStreamTarget { .. }
        | DbError::InvalidProposalAssignmentTarget { .. }
        | DbError::ProposedStreamNotFound { .. }
        | DbError::RejectTargetStreamNotFound { .. }) => ApiError::BadRequest(error.to_string()),
        error => ApiError::Proposals(anyhow::Error::new(error).context("write proposal")),
    }
}

fn notify_database_change(events: &tokio::sync::broadcast::Sender<ServerEvent>) {
    match events.send(ServerEvent::EventsAppended { count: 1 }) {
        Ok(_) | Err(_) => {}
    }
}

fn proposal_response(db: &Database, proposal: Proposal) -> Result<ProposalResponse> {
    let target = match (&proposal.proposed_stream_id, &proposal.proposed_new_stream) {
        (Some(stream_id), None) => {
            let stream = db
                .get_stream(stream_id)
                .context("load proposed existing stream")?;
            ProposalTargetResponse::Existing {
                stream_id: stream_id.clone(),
                name: stream.as_ref().and_then(|stream| stream.name.clone()),
                slug: stream.and_then(|stream| stream.slug),
            }
        }
        (None, Some(definition)) => {
            let proposed: ProposedNewStream =
                serde_json::from_str(definition).context("parse proposed new stream")?;
            ProposalTargetResponse::New {
                name: proposed.name,
                description: proposed.description,
            }
        }
        (Some(_), Some(_)) => bail!("proposal {} has conflicting stream targets", proposal.id),
        (None, None) => bail!("proposal {} has no proposed stream", proposal.id),
    };
    let scope = match (&proposal.session_id, &proposal.event_ids) {
        (Some(_), None) => ProposalScopeResponse::Session { count: 1 },
        (None, Some(event_ids)) => ProposalScopeResponse::Events {
            count: event_ids.len(),
        },
        (Some(_), Some(_)) => bail!("proposal {} has conflicting assignment scopes", proposal.id),
        (None, None) => bail!("proposal {} has no assignment scope", proposal.id),
    };
    Ok(ProposalResponse {
        id: proposal.id,
        created_at: proposal.created_at,
        target,
        confidence: proposal.confidence,
        reasoning: proposal.reasoning,
        scope,
    })
}

use anyhow::Result;

#[path = "tests/fixture.rs"]
mod fixture;

use fixture::{
    fixture_app, fixture_app_with_events, post, post_json, request, response_body, timestamp,
};
use tt_db::{Database, Proposal, ProposalStatus};

#[tokio::test]
async fn timeline_happy_path_serializes_a_representative_fixture() -> Result<()> {
    let (app, _database_file, _todo_store) = fixture_app()?;

    let response = request(app, "/api/timeline?before=2025-01-15T09:05:00Z&duration=5m").await?;

    assert!(response.starts_with("HTTP/1.1 200"));
    let mut value: serde_json::Value = serde_json::from_str(response_body(&response))?;
    value["db_version"] = serde_json::json!(8);
    let rendered = serde_json::to_string_pretty(&value)?;
    with_snapshots(|| insta::assert_snapshot!(rendered));
    Ok(())
}

#[tokio::test]
async fn timeline_empty_window_returns_an_empty_stream_list() -> Result<()> {
    let (app, _database_file, _todo_store) = fixture_app()?;

    let response = request(app, "/api/timeline?before=2024-01-15T09:05:00Z&duration=5m").await?;

    assert!(response.starts_with("HTTP/1.1 200"));
    let value: serde_json::Value = serde_json::from_str(response_body(&response))?;
    assert_eq!(value["streams_active"], serde_json::json!([]));
    Ok(())
}

#[tokio::test]
async fn timeline_rejects_a_malformed_duration() -> Result<()> {
    let (app, _database_file, _todo_store) = fixture_app()?;

    let response = request(app, "/api/timeline?duration=not-a-duration").await?;

    assert!(response.starts_with("HTTP/1.1 400"));
    assert!(response.contains("invalid duration query parameter"));
    Ok(())
}

#[tokio::test]
async fn timeline_rejects_a_malformed_before_timestamp() -> Result<()> {
    let (app, _database_file, _todo_store) = fixture_app()?;

    let response = request(app, "/api/timeline?before=not-a-timestamp").await?;

    assert!(response.starts_with("HTTP/1.1 400"));
    assert!(response.contains("invalid before query parameter"));
    Ok(())
}

#[tokio::test]
async fn todos_serializes_ranked_todos_with_linked_agent_counts() -> Result<()> {
    let (app, _database_file, _todo_store) = fixture_app()?;

    let response = request(app, "/api/todos").await?;

    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(
        response_body(&response).trim_start().starts_with('{'),
        "GET /api/todos must return JSON"
    );
    let value: serde_json::Value = serde_json::from_str(response_body(&response))?;
    let rendered = serde_json::to_string_pretty(&value)?;
    with_snapshots(|| insta::assert_snapshot!("todos_serializes_ranked_todos", rendered));
    Ok(())
}

#[tokio::test]
async fn sessions_serializes_active_and_quiet_linked_agent_sessions() -> Result<()> {
    let (app, _database_file, _todo_store) = fixture_app()?;

    let response = request(app, "/api/sessions").await?;

    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(
        response_body(&response).trim_start().starts_with('{'),
        "GET /api/sessions must return JSON"
    );
    let mut value: serde_json::Value = serde_json::from_str(response_body(&response))?;
    normalize_session_times(&mut value);
    let rendered = serde_json::to_string_pretty(&value)?;
    with_snapshots(|| insta::assert_snapshot!("sessions_serializes_active_sessions", rendered));
    Ok(())
}

#[tokio::test]
async fn proposals_serializes_existing_and_new_stream_targets() -> Result<()> {
    let (app, _database_file, _todo_store) = fixture_app()?;

    let response = request(app, "/api/proposals").await?;

    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(
        response_body(&response).trim_start().starts_with('{'),
        "GET /api/proposals must return JSON"
    );
    let value: serde_json::Value = serde_json::from_str(response_body(&response))?;
    let rendered = serde_json::to_string_pretty(&value)?;
    with_snapshots(|| insta::assert_snapshot!("proposals_serializes_targets", rendered));
    Ok(())
}

#[tokio::test]
async fn accepts_proposal_when_pending() -> Result<()> {
    let (app, database_file, _todo_store, mut events) = fixture_app_with_events()?;

    let response = post(app, "/api/proposals/proposal-existing/accept").await?;

    assert!(response.starts_with("HTTP/1.1 200"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(response_body(&response))?,
        serde_json::json!({
            "proposal_id": "proposal-existing",
            "status": "accepted",
            "stream_id": "alpha",
            "created_stream": false,
            "events_assigned": 1
        })
    );
    assert_eq!(
        events.recv().await?,
        crate::ServerEvent::EventsAppended { count: 1 }
    );
    let db = Database::open(database_file.path())?;
    assert_eq!(
        db.get_proposals(Some(ProposalStatus::Accepted))?
            .into_iter()
            .map(|proposal| proposal.id)
            .collect::<Vec<_>>(),
        vec!["proposal-existing"]
    );
    let assigned = db.get_events_by_stream("alpha")?;
    assert_eq!(
        assigned
            .into_iter()
            .find(|event| event.id == "active-tool-use")
            .and_then(|event| event.assignment_source),
        Some("user".to_string())
    );
    Ok(())
}

#[tokio::test]
async fn accepts_unknown_proposal_returns_not_found() -> Result<()> {
    let (app, _database_file, _todo_store) = fixture_app()?;

    let response = post(app, "/api/proposals/missing/accept").await?;

    assert!(response.starts_with("HTTP/1.1 404"));
    assert!(response.contains("proposal 'missing' does not exist"));
    Ok(())
}

#[tokio::test]
async fn accepts_proposal_once_when_repeated() -> Result<()> {
    let (app, _database_file, _todo_store) = fixture_app()?;

    let first_response = post(app.clone(), "/api/proposals/proposal-existing/accept").await?;
    let second_response = post(app, "/api/proposals/proposal-existing/accept").await?;

    assert!(first_response.starts_with("HTTP/1.1 200"));
    assert!(second_response.starts_with("HTTP/1.1 409"));
    assert!(second_response.contains("proposal 'proposal-existing' is not pending"));
    Ok(())
}

#[tokio::test]
async fn rejects_proposal_without_target_when_pending() -> Result<()> {
    let (app, database_file, _todo_store, mut events) = fixture_app_with_events()?;
    insert_rejectable_proposal(database_file.path(), "proposal-reject")?;

    let response = post(app, "/api/proposals/proposal-reject/reject").await?;

    assert!(response.starts_with("HTTP/1.1 200"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(response_body(&response))?,
        serde_json::json!({
            "proposal_id": "proposal-reject",
            "status": "rejected",
            "stream_id": null,
            "events_assigned": 0
        })
    );
    assert_eq!(
        events.recv().await?,
        crate::ServerEvent::EventsAppended { count: 1 }
    );
    let db = Database::open(database_file.path())?;
    assert_eq!(db.unassigned_event_ids()?.len(), 1);
    assert!(db.has_rejected_proposal("quiet-session", "beta")?);
    Ok(())
}

#[tokio::test]
async fn rejects_proposal_with_target_when_stream_exists() -> Result<()> {
    let (app, database_file, _todo_store) = fixture_app()?;
    insert_rejectable_proposal(database_file.path(), "proposal-reject")?;

    let response = post_json(
        app,
        "/api/proposals/proposal-reject/reject",
        r#"{"stream":"beta"}"#,
    )
    .await?;

    assert!(response.starts_with("HTTP/1.1 200"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(response_body(&response))?,
        serde_json::json!({
            "proposal_id": "proposal-reject",
            "status": "rejected",
            "stream_id": "beta",
            "events_assigned": 1
        })
    );
    let db = Database::open(database_file.path())?;
    let events = db.get_events_by_stream("beta")?;
    assert_eq!(events.len(), 2);
    assert_eq!(
        events
            .into_iter()
            .find(|event| event.id == "quiet-tool-use")
            .and_then(|event| event.assignment_source),
        Some("user".to_string())
    );
    Ok(())
}

#[tokio::test]
async fn rejects_proposal_with_unknown_target_returns_bad_request() -> Result<()> {
    let (app, database_file, _todo_store) = fixture_app()?;
    insert_rejectable_proposal(database_file.path(), "proposal-reject")?;

    let response = post_json(
        app,
        "/api/proposals/proposal-reject/reject",
        r#"{"stream":"missing"}"#,
    )
    .await?;

    assert!(response.starts_with("HTTP/1.1 400"));
    assert!(response.contains("target stream 'missing' does not exist"));
    let db = Database::open(database_file.path())?;
    assert_eq!(
        db.get_proposals(Some(ProposalStatus::Pending))?
            .into_iter()
            .filter(|proposal| proposal.id == "proposal-reject")
            .count(),
        1
    );
    Ok(())
}

fn insert_rejectable_proposal(database_path: &std::path::Path, proposal_id: &str) -> Result<()> {
    let db = Database::open(database_path)?;
    db.insert_proposal(&Proposal {
        id: proposal_id.to_owned(),
        created_at: timestamp(2),
        session_id: Some("quiet-session".to_owned()),
        event_ids: None,
        proposed_stream_id: Some("beta".to_owned()),
        proposed_new_stream: None,
        confidence: 0.8,
        reasoning: "rejectable proposal".to_owned(),
        status: ProposalStatus::Pending,
        classifier_generation: None,
    })?;
    Ok(())
}

fn with_snapshots(run: impl FnOnce()) {
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path("../snapshots");
    settings.bind(run);
}

fn normalize_session_times(value: &mut serde_json::Value) {
    let Some(sessions) = value["sessions"].as_array_mut() else {
        return;
    };
    for session in sessions {
        session["start_time"] = serde_json::json!("[timestamp]");
        session["last_activity"] = serde_json::json!("[timestamp]");
        session["duration_ms"] = serde_json::json!(123_456);
    }
}

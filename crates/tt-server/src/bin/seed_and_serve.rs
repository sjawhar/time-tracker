use chrono::{Duration, Utc};
use serde_json::json;
use std::path::PathBuf;
use tt_core::EventType;
use tt_db::{Database, StoredEvent, Stream};

// Test-support binary: seeds a throwaway DB at /tmp/tt_seed.db in one of the verdict
// states so the dashboard's visual QA can screenshot each one. The seeding is a single
// linear script of fixture literals; splitting it into helpers would obscure rather
// than clarify what the fixture is.
#[expect(
    clippy::too_many_lines,
    reason = "linear fixture script; splitting it would obscure the fixture"
)]
fn main() -> anyhow::Result<()> {
    let db_path = PathBuf::from("/tmp/tt_seed.db");
    if db_path.exists() {
        std::fs::remove_file(&db_path)?;
    }

    // Copy schema by initializing
    let db = Database::open(&db_path)?;

    let now = Utc::now();

    // Insert streams
    db.insert_stream(&Stream {
        id: "stream-alpha".to_string(),
        name: Some("Alpha work".to_string()),
        slug: Some("alpha".to_string()),
        description: None,
        color: None,
        created_at: now,
        updated_at: now,
        time_direct_ms: 0,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: false,
    })?;

    db.insert_stream(&Stream {
        id: "stream-beta".to_string(),
        name: Some("Beta work".to_string()),
        slug: Some("beta".to_string()),
        description: None,
        color: None,
        created_at: now,
        updated_at: now,
        time_direct_ms: 0,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: false,
    })?;

    // Insert events based on env var
    let state = std::env::var("SEED_STATE").unwrap_or_else(|_| "UNKNOWN".to_string());

    if state == "ALIGNED" {
        db.insert_event(&StoredEvent {
            id: "focus-1".to_string(),
            timestamp: now - Duration::minutes(5),
            event_type: EventType::TmuxPaneFocus,
            source: "test".to_string(),
            machine_id: None,
            schema_version: 1,
            pane_id: None,
            tmux_session: None,
            window_index: None,
            git_project: None,
            git_workspace: None,
            status: None,
            idle_duration_ms: None,
            window_app_id: None,
            window_title: None,
            action: None,
            cwd: None,
            session_id: None,
            stream_id: Some("stream-alpha".to_string()),
            assignment_source: None,
            data: json!({}),
        })?;
        db.insert_event(&StoredEvent {
            id: "focus-2".to_string(),
            timestamp: now - Duration::seconds(10),
            event_type: EventType::TmuxPaneFocus,
            source: "test".to_string(),
            machine_id: None,
            schema_version: 1,
            pane_id: None,
            tmux_session: None,
            window_index: None,
            git_project: None,
            git_workspace: None,
            status: None,
            idle_duration_ms: None,
            window_app_id: None,
            window_title: None,
            action: None,
            cwd: None,
            session_id: None,
            stream_id: Some("stream-alpha".to_string()),
            assignment_source: None,
            data: json!({}),
        })?;
        db.insert_event(&StoredEvent {
            id: "msg-1".to_string(),
            timestamp: now - Duration::minutes(4),
            event_type: EventType::UserMessage,
            source: "test".to_string(),
            machine_id: None,
            schema_version: 1,
            pane_id: None,
            tmux_session: None,
            window_index: None,
            git_project: None,
            git_workspace: None,
            status: None,
            idle_duration_ms: None,
            window_app_id: None,
            window_title: None,
            action: Some("user_message".to_string()),
            cwd: None,
            session_id: Some("ses_123".to_string()),
            stream_id: Some("stream-alpha".to_string()),
            assignment_source: None,
            data: json!({}),
        })?;
        db.insert_event(&StoredEvent {
            id: "sub-1".to_string(),
            timestamp: now - Duration::minutes(3),
            event_type: EventType::AgentSession,
            source: "test".to_string(),
            machine_id: None,
            schema_version: 1,
            pane_id: None,
            tmux_session: None,
            window_index: None,
            git_project: None,
            git_workspace: None,
            status: None,
            idle_duration_ms: None,
            window_app_id: None,
            window_title: None,
            action: Some("started".to_string()),
            cwd: None,
            session_id: Some("ses_123".to_string()),
            stream_id: Some("stream-alpha".to_string()),
            assignment_source: None,
            data: json!({}),
        })?;
        db.insert_event(&StoredEvent {
            id: "start-1".to_string(),
            timestamp: now - Duration::seconds(270),
            event_type: EventType::AgentSession,
            source: "test".to_string(),
            machine_id: None,
            schema_version: 1,
            pane_id: None,
            tmux_session: None,
            window_index: None,
            git_project: None,
            git_workspace: None,
            status: None,
            idle_duration_ms: None,
            window_app_id: None,
            window_title: None,
            action: Some("started".to_string()),
            cwd: None,
            session_id: Some("ses_123".to_string()),
            stream_id: Some("stream-alpha".to_string()),
            assignment_source: None,
            data: json!({}),
        })?;
        db.insert_event(&StoredEvent {
            id: "end-1".to_string(),
            timestamp: now - Duration::minutes(2),
            event_type: EventType::AgentSession,
            source: "test".to_string(),
            machine_id: None,
            schema_version: 1,
            pane_id: None,
            tmux_session: None,
            window_index: None,
            git_project: None,
            git_workspace: None,
            status: None,
            idle_duration_ms: None,
            window_app_id: None,
            window_title: None,
            action: Some("ended".to_string()),
            cwd: None,
            session_id: Some("ses_123".to_string()),
            stream_id: Some("stream-alpha".to_string()),
            assignment_source: None,
            data: json!({}),
        })?;
    } else if state == "DRIFTING" {
        db.insert_event(&StoredEvent {
            id: "focus-1".to_string(),
            timestamp: now - Duration::minutes(15),
            event_type: EventType::TmuxPaneFocus,
            source: "test".to_string(),
            machine_id: None,
            schema_version: 1,
            pane_id: None,
            tmux_session: None,
            window_index: None,
            git_project: None,
            git_workspace: None,
            status: None,
            idle_duration_ms: None,
            window_app_id: None,
            window_title: None,
            action: None,
            cwd: None,
            session_id: None,
            stream_id: Some("stream-beta".to_string()),
            assignment_source: None,
            data: json!({}),
        })?;
        db.insert_event(&StoredEvent {
            id: "focus-2".to_string(),
            timestamp: now - Duration::seconds(10),
            event_type: EventType::TmuxPaneFocus,
            source: "test".to_string(),
            machine_id: None,
            schema_version: 1,
            pane_id: None,
            tmux_session: None,
            window_index: None,
            git_project: None,
            git_workspace: None,
            status: None,
            idle_duration_ms: None,
            window_app_id: None,
            window_title: None,
            action: None,
            cwd: None,
            session_id: None,
            stream_id: Some("stream-beta".to_string()),
            assignment_source: None,
            data: json!({}),
        })?;
    }

    // Set classifier error
    db.record_classifier_unconfigured("{\"error\": {\"message\": \"API rate limit exceeded\"}}")?;

    // Create todo store
    let todo_dir = PathBuf::from("/tmp/tt_seed_todos");
    std::fs::create_dir_all(&todo_dir)?;
    std::fs::write(
        todo_dir.join("priorities.md"),
        "- [ ] High <!-- tt-priority:{\"slug\":\"high\",\"value\":10,\"status\":\"active\"} -->\n",
    )?;
    std::fs::write(
        todo_dir.join("todos.md"),
        "- [ ] Ship alpha <!-- tt-todo:{\"id\":\"td_top000001\",\"priority\":[],\"stream\":\"alpha\",\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n",
    )?;
    std::fs::write(
        todo_dir.join("streams.md"),
        "- alpha <!-- tt-stream:{\"priority\":\"high\"} -->\n",
    )?;

    let seeded_path = db_path.display();
    println!("Seeded DB at {seeded_path}");
    Ok(())
}

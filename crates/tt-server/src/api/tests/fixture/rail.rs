use std::path::Path;

use anyhow::Result;
use chrono::{Duration, Utc};
use tt_core::{AgentSession, EventType, SessionSource, SessionType};
use tt_db::{Database, Proposal, ProposalStatus};

use super::{insert_event, timestamp};

pub(super) fn populate(db: &Database, todo_store: &Path) -> Result<()> {
    std::fs::write(
        todo_store.join("priorities.md"),
        "- [ ] Customer <!-- tt-priority:{\"slug\":\"customer\",\"value\":9,\"status\":\"active\"} -->\n- [ ] Operations <!-- tt-priority:{\"slug\":\"operations\",\"value\":2,\"status\":\"active\"} -->\n",
    )?;
    std::fs::write(todo_store.join("streams.md"), "")?;
    std::fs::write(
        todo_store.join("todos.md"),
        "- [ ] Ship the rail <!-- tt-todo:{\"id\":\"td_rail\",\"priority\":[\"customer\"],\"stream\":\"alpha\",\"when\":null,\"due\":null,\"pin\":false,\"quick\":false,\"sessions\":[\"active-session\",\"quiet-session\"]} -->\n- [ ] Operations follow-up <!-- tt-todo:{\"id\":\"td_ops\",\"priority\":[\"operations\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n",
    )?;
    db.upsert_machine("machine-a", "buildbox", None)?;
    let now = Utc::now();
    insert_open_session(
        db,
        "active-session",
        SessionSource::Claude,
        now - Duration::minutes(4),
        Some("machine-a"),
    )?;
    insert_open_session(
        db,
        "quiet-session",
        SessionSource::OpenCode,
        now - Duration::minutes(28),
        None,
    )?;
    insert_event(
        db,
        "active-tool-use",
        now - Duration::minutes(1),
        EventType::AgentToolUse,
        Some("alpha"),
        Some("active-session"),
        None,
        None,
    )?;
    insert_event(
        db,
        "quiet-tool-use",
        now - Duration::minutes(25),
        EventType::AgentToolUse,
        None,
        Some("quiet-session"),
        None,
        None,
    )?;
    db.insert_proposal(&Proposal {
        id: "proposal-existing".to_owned(),
        created_at: timestamp(0),
        session_id: Some("active-session".to_owned()),
        event_ids: None,
        proposed_stream_id: Some("alpha".to_owned()),
        proposed_new_stream: None,
        confidence: 0.91,
        reasoning: "Recent work belongs with the alpha stream.".to_owned(),
        status: ProposalStatus::Pending,
        classifier_generation: None,
    })?;
    db.insert_proposal(&Proposal {
        id: "proposal-new".to_owned(),
        created_at: timestamp(1),
        session_id: None,
        event_ids: Some(vec!["event-a".to_owned(), "event-b".to_owned()]),
        proposed_stream_id: None,
        proposed_new_stream: Some(
            serde_json::json!({
                "name": "Support handoff",
                "description": "Coordinate the customer support handoff.",
                "tags": ["support"]
            })
            .to_string(),
        ),
        confidence: 0.78,
        reasoning: "The events form a distinct support handoff thread.".to_owned(),
        status: ProposalStatus::Pending,
        classifier_generation: None,
    })?;
    Ok(())
}

fn insert_open_session(
    db: &Database,
    session_id: &str,
    source: SessionSource,
    start_time: chrono::DateTime<Utc>,
    machine_id: Option<&str>,
) -> Result<()> {
    db.upsert_agent_session(
        &AgentSession {
            session_id: session_id.to_owned(),
            source,
            parent_session_id: None,
            session_type: SessionType::Agent,
            project_path: "/test".to_owned(),
            project_name: "test".to_owned(),
            start_time,
            end_time: None,
            message_count: 0,
            summary: None,
            user_prompts: Vec::new(),
            starting_prompt: None,
            assistant_message_count: 0,
            tool_call_count: 1,
            user_message_timestamps: Vec::new(),
            tool_call_timestamps: Vec::new(),
        },
        machine_id,
    )?;
    Ok(())
}

use super::release_junk_attention;
use tt_db::{Database, ReleaseMode, StoredEvent, Stream};

fn junk_stream() -> Stream {
    let now = chrono::Utc::now();
    Stream {
        id: "junk".to_string(),
        created_at: now,
        updated_at: now,
        name: Some("junk: no attributable work".to_string()),
        slug: Some("junk".to_string()),
        description: None,
        color: None,
        time_direct_ms: 0,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: false,
    }
}

fn event(id: &str, event_type: tt_core::EventType, assignment_source: &str) -> StoredEvent {
    StoredEvent {
        id: id.to_string(),
        timestamp: chrono::Utc::now(),
        event_type,
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
        stream_id: Some("junk".to_string()),
        assignment_source: Some(assignment_source.to_string()),
        data: serde_json::json!({}),
    }
}

fn db_with_junk_attention() -> Database {
    let db = Database::open_in_memory().unwrap();
    db.insert_stream(&junk_stream()).unwrap();
    for (id, event_type, assignment_source) in [
        ("user", tt_core::EventType::UserMessage, "junk"),
        (
            "window",
            tt_core::EventType::WindowFocus,
            "session_membership",
        ),
        ("pane", tt_core::EventType::TmuxPaneFocus, "terminal_focus"),
        ("agent-session", tt_core::EventType::AgentSession, "junk"),
        ("agent-tool", tt_core::EventType::AgentToolUse, "junk"),
    ] {
        db.insert_event(&event(id, event_type, assignment_source))
            .unwrap();
    }
    db.insert_event(&event("human", tt_core::EventType::UserMessage, "user"))
        .unwrap();
    db
}

#[test]
fn releases_only_junked_attention_events() {
    // Given: junk holds all three attention-opening types plus agent activity and a human verdict.
    let db = db_with_junk_attention();

    // When: the release command applies its fixed selection.
    release_junk_attention(&db, ReleaseMode::Apply).unwrap();

    // Then: attention is unassigned, agent activity and the human verdict remain on junk.
    let events = db.get_events(None, None).unwrap();
    for id in ["user", "window", "pane"] {
        let event = events.iter().find(|event| event.id == id).unwrap();
        assert_eq!(event.stream_id, None, "{id} stayed on junk");
        assert_eq!(event.assignment_source, None, "{id} kept a source");
    }
    for id in ["agent-session", "agent-tool", "human"] {
        let event = events.iter().find(|event| event.id == id).unwrap();
        assert_eq!(
            event.stream_id.as_deref(),
            Some("junk"),
            "{id} moved off junk"
        );
    }
    let human = events.iter().find(|event| event.id == "human").unwrap();
    assert_eq!(human.assignment_source.as_deref(), Some("user"));
}

#[test]
fn dry_run_releases_nothing() {
    // Given: junked attention awaiting release.
    let db = db_with_junk_attention();
    let version_before = db.get_db_version().unwrap();

    // When: the command previews its fixed selection.
    release_junk_attention(&db, ReleaseMode::DryRun).unwrap();

    // Then: every row and the daemon signal stay unchanged.
    let events = db.get_events(None, None).unwrap();
    assert!(
        events
            .iter()
            .all(|event| event.stream_id.as_deref() == Some("junk"))
    );
    assert_eq!(db.get_db_version().unwrap(), version_before);
}

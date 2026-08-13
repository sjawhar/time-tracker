use chrono::{TimeZone, Utc};
use tt_db::{Database, MergeMode, StoredEvent, Stream};

use super::super::collapse_instance_families;

fn stream(id: &str, name: &str, created_at: chrono::DateTime<Utc>) -> Stream {
    Stream {
        id: id.to_string(),
        created_at,
        updated_at: created_at,
        name: Some(name.to_string()),
        slug: None,
        description: None,
        color: None,
        time_direct_ms: 0,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: false,
    }
}

fn assign(db: &Database, stream_id: &str, event_id: &str, assignment_source: &str) {
    db.insert_event(&StoredEvent {
        id: event_id.to_string(),
        timestamp: Utc::now(),
        event_type: tt_core::EventType::WindowFocus,
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
        stream_id: Some(stream_id.to_string()),
        assignment_source: Some(assignment_source.to_string()),
        data: serde_json::json!({}),
    })
    .unwrap();
}

fn db_with_instance_family() -> Database {
    let db = Database::open_in_memory().unwrap();
    let first = Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();
    let second = Utc.with_ymd_and_hms(2026, 8, 2, 0, 0, 0).unwrap();
    db.insert_stream(&stream(
        "iteration-one",
        "agent-c: reskin implementer (Ralph iteration 1)",
        first,
    ))
    .unwrap();
    db.insert_stream(&stream(
        "iteration-two",
        "agent-c: reskin implementer (Ralph iteration 2)",
        second,
    ))
    .unwrap();
    assign(&db, "iteration-one", "one", "inferred");
    assign(&db, "iteration-two", "two", "inferred");
    assign(&db, "iteration-two", "three", "user");
    db
}

#[test]
fn collapses_an_instance_family_onto_its_most_eventful_member() {
    // Given: two numbered instances of one initiative, with the second holding more events.
    let db = db_with_instance_family();

    // When: the family is collapsed.
    let outcome = collapse_instance_families(&db, MergeMode::Apply).unwrap();

    // Then: the most-eventful member becomes the initiative row and every event moves onto it.
    assert_eq!(outcome.groups.len(), 1);
    assert_eq!(outcome.groups[0].target_id, "iteration-two");
    assert_eq!(
        db.get_stream("iteration-two")
            .unwrap()
            .unwrap()
            .name
            .as_deref(),
        Some("agent-c: reskin implementer")
    );
    assert!(db.get_stream("iteration-one").unwrap().is_none());
    assert_eq!(db.get_events_by_stream("iteration-two").unwrap().len(), 3);
    assert_eq!(db.get_events(None, None).unwrap().len(), 3);
}

#[test]
fn preserves_human_assignments_when_collapsing_an_instance_family() {
    // Given: an instance family with an event a human placed by hand.
    let db = db_with_instance_family();

    // When: its rows are collapsed.
    collapse_instance_families(&db, MergeMode::Apply).unwrap();

    // Then: the human event remains on the selected target rather than being released.
    let events = db.get_events_by_stream("iteration-two").unwrap();
    let user_event = events.iter().find(|event| event.id == "three").unwrap();
    assert_eq!(user_event.assignment_source.as_deref(), Some("user"));
}

#[test]
fn chooses_the_earliest_created_member_when_instance_event_counts_tie() {
    // Given: two equally eventful instances, created on different days.
    let db = Database::open_in_memory().unwrap();
    let first = Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();
    let second = Utc.with_ymd_and_hms(2026, 8, 2, 0, 0, 0).unwrap();
    db.insert_stream(&stream(
        "first",
        "agent-c: reskin tester (Ralph iteration 1)",
        first,
    ))
    .unwrap();
    db.insert_stream(&stream(
        "second",
        "agent-c: reskin tester (Ralph iteration 2)",
        second,
    ))
    .unwrap();
    assign(&db, "first", "one", "inferred");
    assign(&db, "second", "two", "inferred");

    // When: the tied family is collapsed.
    let outcome = collapse_instance_families(&db, MergeMode::Apply).unwrap();

    // Then: creation order selects a stable target.
    assert_eq!(outcome.groups[0].target_id, "first");
    assert!(db.get_stream("second").unwrap().is_none());
}

#[test]
fn dry_run_leaves_instance_families_unchanged() {
    // Given: a family that a real invocation would rename and merge.
    let db = db_with_instance_family();
    let version_before = db.get_db_version().unwrap();

    // When: it is inspected in dry-run mode.
    let outcome = collapse_instance_families(&db, MergeMode::DryRun).unwrap();

    // Then: the proposed group is reported but no row, event, or version changes.
    assert_eq!(outcome.groups.len(), 1);
    assert_eq!(
        db.get_stream("iteration-two")
            .unwrap()
            .unwrap()
            .name
            .as_deref(),
        Some("agent-c: reskin implementer (Ralph iteration 2)")
    );
    assert_eq!(db.get_events_by_stream("iteration-one").unwrap().len(), 1);
    assert_eq!(db.get_events_by_stream("iteration-two").unwrap().len(), 2);
    assert_eq!(db.get_db_version().unwrap(), version_before);
}

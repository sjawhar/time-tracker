use std::collections::HashMap;

use super::*;
use crate::todo_store::load_read_only;

fn stream(id: &str, name: &str, slug: Option<&str>) -> tt_db::Stream {
    let now = chrono::Utc::now();
    tt_db::Stream {
        id: id.to_string(),
        created_at: now,
        updated_at: now,
        name: Some(name.to_string()),
        slug: slug.map(String::from),
        description: None,
        color: None,
        time_direct_ms: 0,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: false,
    }
}

fn event(id: &str, session_id: Option<&str>) -> tt_db::StoredEvent {
    tt_db::StoredEvent {
        id: id.to_string(),
        timestamp: chrono::Utc::now(),
        event_type: tt_core::EventType::AgentToolUse,
        source: "test".to_string(),
        machine_id: None,
        schema_version: 1,
        cwd: Some("/project".to_string()),
        git_project: None,
        git_workspace: None,
        pane_id: None,
        tmux_session: None,
        window_index: None,
        status: None,
        idle_duration_ms: None,
        action: None,
        session_id: session_id.map(String::from),
        stream_id: None,
        assignment_source: None,
        data: serde_json::Value::Null,
        window_app_id: None,
        window_title: None,
    }
}

/// Two streams and a session of two events, plus one loose event.
fn fixture() -> tt_db::Database {
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&stream("right", "webapp: engine refactor", Some("engine")))
        .unwrap();
    db.insert_stream(&stream("wrong", "misc", None)).unwrap();
    db.insert_event(&event("e1", Some("ses-a"))).unwrap();
    db.insert_event(&event("e2", Some("ses-a"))).unwrap();
    db.insert_event(&event("loose", None)).unwrap();
    db
}

fn sources(db: &tt_db::Database, stream_id: &str) -> Vec<(String, Option<String>)> {
    let mut rows: Vec<_> = db
        .get_events_by_stream(stream_id)
        .unwrap()
        .into_iter()
        .map(|event| (event.id, event.assignment_source))
        .collect();
    rows.sort();
    rows
}

#[test]
fn records_a_session_assignment_as_the_human_verdict() {
    // Given: a session the classifier has not reached.
    let db = fixture();

    // When: a human files it.
    let assigned = assign_to(&db, "right", &["ses-a".to_string()], &[]).unwrap();

    // Then: every event of that session is filed, and stamped 'user' — the source no
    // machine writer in the codebase will overwrite.
    assert_eq!(assigned.events_moved, 2);
    assert_eq!(
        sources(&db, "right"),
        [
            ("e1".to_string(), Some("user".to_string())),
            ("e2".to_string(), Some("user".to_string())),
        ]
    );
}

#[test]
fn records_an_explicit_event_assignment_as_the_human_verdict() {
    // Given: a focus event that belongs to no session, so nothing can infer it.
    let db = fixture();

    // When: a human names it directly.
    let assigned = assign_to(&db, "right", &[], &["loose".to_string()]).unwrap();

    // Then: it lands as a user assignment, and the session's events are untouched.
    assert_eq!(assigned.events_moved, 1);
    assert_eq!(
        sources(&db, "right"),
        [("loose".to_string(), Some("user".to_string()))]
    );
}

#[test]
fn resolves_the_target_by_id_slug_or_exact_name() {
    // Given/When/Then: each reference form reaches the same stream.
    for reference in ["right", "engine", "webapp: engine refactor"] {
        let db = fixture();
        let assigned = assign_to(&db, reference, &["ses-a".to_string()], &[]).unwrap();
        assert_eq!(assigned.stream.id, "right", "reference '{reference}'");
        assert_eq!(assigned.events_moved, 2);
    }
}

#[test]
fn lets_a_human_correct_their_own_earlier_correction() {
    // Given: a session a human already filed once, in the wrong place.
    let db = fixture();
    assign_to(&db, "wrong", &["ses-a".to_string()], &[]).unwrap();

    // When: they correct it.
    let assigned = assign_to(&db, "right", &["ses-a".to_string()], &[]).unwrap();

    // Then: the second verdict wins. The 'user' guard exists to stop machines
    // overwriting a human, not to freeze the human's first answer.
    assert_eq!(assigned.events_moved, 2);
    assert!(db.get_events_by_stream("wrong").unwrap().is_empty());
    assert_eq!(db.get_events_by_stream("right").unwrap().len(), 2);
}

#[test]
fn corrects_an_explicit_event_a_previous_correction_claimed() {
    // Given: a loose event a human already filed.
    let db = fixture();
    assign_to(&db, "wrong", &[], &["loose".to_string()]).unwrap();

    // When: they move it.
    let assigned = assign_to(&db, "right", &[], &["loose".to_string()]).unwrap();

    // Then: it moves. The guarded `assign_events_by_ids` would have skipped it.
    assert_eq!(assigned.events_moved, 1);
    assert_eq!(
        sources(&db, "right"),
        [("loose".to_string(), Some("user".to_string()))]
    );
}

#[test]
fn never_creates_the_target_stream() {
    // Given: a reference naming no existing stream.
    let db = fixture();

    // When/Then: the correction is refused rather than minting a container — the exact
    // move that produced 68 misnamed streams.
    let error = assign_to(&db, "some new bucket", &["ses-a".to_string()], &[]).unwrap_err();
    assert!(error.to_string().contains("no stream matching"));
    assert_eq!(db.get_streams().unwrap().len(), 2);
}

#[test]
fn refuses_to_run_without_an_explicit_selection() {
    // Given: a valid target but nothing named.
    let db = fixture();

    // When/Then: refused. There is no "assign everything" form of this command, which
    // is what keeps it a correction surface rather than a second inference engine.
    let error = assign_to(&db, "right", &[], &[]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("at least one --session or --event")
    );
    assert!(db.get_events_by_stream("right").unwrap().is_empty());
}

#[test]
fn marks_the_target_for_recompute_rather_than_refreshing_it() {
    // Given: a stream whose totals are currently believed current.
    let db = fixture();
    assert!(!db.get_stream("right").unwrap().unwrap().needs_recompute);

    // When: a correction moves events onto it.
    assign_to(&db, "right", &["ses-a".to_string()], &[]).unwrap();

    // Then: its times are marked stale. Only `tt recompute` writes them.
    assert!(db.get_stream("right").unwrap().unwrap().needs_recompute);
}

#[test]
fn leaves_the_target_alone_when_nothing_matched() {
    // Given: a session id that names no events.
    let db = fixture();

    // When: it is assigned.
    let assigned = assign_to(&db, "right", &["ses-nothing".to_string()], &[]).unwrap();

    // Then: nothing moved and no recompute was requested.
    assert_eq!(assigned.events_moved, 0);
    assert!(!db.get_stream("right").unwrap().unwrap().needs_recompute);
    assert!(report(&assigned, &[]).contains("No events matched"));
}

fn todo_fixture(todos: &str) -> (tempfile::TempDir, Config) {
    let temp = tempfile::TempDir::new().unwrap();
    let store = temp.path().join("todo-store");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("todos.md"), todos).unwrap();
    let config = Config {
        database_path: temp.path().join("tt.db"),
        todo_store_path: store,
        ..Config::default()
    };
    (temp, config)
}

#[test]
fn backfills_the_slug_onto_streamless_todos_linking_a_moved_session() {
    // Given: three todos — one streamless on the moved session, one already answered,
    // one on a different session.
    let db = fixture();
    let (_temp, config) = todo_fixture(concat!(
        "- [ ] Backfill me <!-- tt-todo:{\"id\":\"td_1\",\"priority\":[],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false,\"sessions\":[\"ses-a\"]} -->\n",
        "- [ ] Already assigned <!-- tt-todo:{\"id\":\"td_2\",\"priority\":[],\"stream\":\"existing\",\"when\":null,\"due\":null,\"pin\":false,\"quick\":false,\"sessions\":[\"ses-a\"]} -->\n",
        "- [ ] Different session <!-- tt-todo:{\"id\":\"td_3\",\"priority\":[],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false,\"sessions\":[\"ses-9\"]} -->\n",
    ));
    let assigned = assign_to(&db, "right", &["ses-a".to_string()], &[]).unwrap();

    // When: the todos are reconciled against that assignment.
    let lines = backfill_linked_todos(&config, &assigned).unwrap();

    // Then: only the streamless todo on the moved session is filled. An existing
    // stream is a human verdict this must not overwrite.
    assert_eq!(lines, ["Backfilled stream 'engine' → td_1"]);
    let loaded = load_read_only(&config).unwrap();
    let todos: HashMap<_, _> = loaded
        .store
        .todos
        .items
        .iter()
        .filter_map(|file_line| {
            let TodoFileItem::Todo(todo) = &file_line.item else {
                return None;
            };
            Some((todo.id.clone(), todo.stream.clone()))
        })
        .collect();
    assert_eq!(todos["td_1"].as_deref(), Some("engine"));
    assert_eq!(todos["td_2"].as_deref(), Some("existing"));
    assert_eq!(todos["td_3"].as_deref(), None);
}

#[test]
fn does_not_touch_the_todo_store_when_the_target_has_no_slug() {
    // Given: a target stream with no slug — there is nothing a todo could reference.
    let db = fixture();
    let (_temp, config) = todo_fixture("");
    let assigned = assign_to(&db, "wrong", &["ses-a".to_string()], &[]).unwrap();

    // When/Then: the store is left alone.
    assert!(
        backfill_linked_todos(&config, &assigned)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn does_not_touch_the_todo_store_when_nothing_moved() {
    // Given: a store path that does not exist and an assignment that matched nothing.
    let temp = tempfile::TempDir::new().unwrap();
    let config = Config {
        database_path: temp.path().join("tt.db"),
        todo_store_path: temp.path().join("missing-store"),
        ..Config::default()
    };
    let db = fixture();
    let assigned = assign_to(&db, "right", &["ses-nothing".to_string()], &[]).unwrap();

    // When/Then: no read and no write.
    assert!(
        backfill_linked_todos(&config, &assigned)
            .unwrap()
            .is_empty()
    );
    assert!(!config.todo_store_path.exists());
}

use super::{format_release, release_pane_focus};
use tt_db::{Database, ReleaseMode, ReleaseOutcome, StoredEvent, Stream};

/// The counts a real run produces on the live corpus, rounded to the measured figures.
const fn sample() -> ReleaseOutcome {
    ReleaseOutcome {
        released: 47_082,
        retained: 0,
        streams_affected: 312,
    }
}

#[test]
fn formats_a_dry_run_report() {
    insta::assert_snapshot!(format_release(sample(), ReleaseMode::DryRun).unwrap());
}

#[test]
fn formats_an_applied_report() {
    insta::assert_snapshot!(format_release(sample(), ReleaseMode::Apply).unwrap());
}

#[test]
fn formats_a_report_with_nothing_to_release() {
    insta::assert_snapshot!(
        format_release(
            ReleaseOutcome {
                released: 0,
                retained: 0,
                streams_affected: 0,
            },
            ReleaseMode::Apply
        )
        .unwrap()
    );
}

/// A report that releases nothing still names what a human assignment held back.
#[test]
fn formats_a_report_whose_only_candidate_a_human_assigned() {
    insta::assert_snapshot!(
        format_release(
            ReleaseOutcome {
                released: 0,
                retained: 4,
                streams_affected: 0,
            },
            ReleaseMode::DryRun
        )
        .unwrap()
    );
}

/// Builds a stream holding one sessionless pane focus the propagator assigned.
fn db_with_propagated_pane() -> Database {
    let db = Database::open_in_memory().unwrap();
    let now = chrono::Utc::now();
    db.insert_stream(&Stream {
        id: "container".to_string(),
        created_at: now,
        updated_at: now,
        name: Some("agent-c: tooling".to_string()),
        slug: None,
        description: None,
        color: None,
        time_direct_ms: 0,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: false,
    })
    .unwrap();
    db.insert_event(&StoredEvent {
        id: "pane-inferred".to_string(),
        timestamp: now,
        event_type: tt_core::EventType::TmuxPaneFocus,
        source: "local.tmux".to_string(),
        machine_id: None,
        schema_version: 1,
        pane_id: Some("%8".to_string()),
        tmux_session: Some("dev".to_string()),
        window_index: Some(1),
        git_project: None,
        git_workspace: None,
        status: None,
        idle_duration_ms: None,
        window_app_id: None,
        window_title: None,
        action: None,
        cwd: Some("/home/sami/Code/time-tracker/default".to_string()),
        session_id: None,
        stream_id: Some("container".to_string()),
        assignment_source: Some("inferred".to_string()),
        data: serde_json::json!({}),
    })
    .unwrap();
    db
}

/// The dry run must leave the database exactly as it found it, end to end.
#[test]
fn a_dry_run_through_the_command_writes_nothing() {
    // Given: a pane focus the propagator assigned.
    let db = db_with_propagated_pane();
    let version_before = db.get_db_version().unwrap();

    // When: the command previews the release.
    release_pane_focus(&db, ReleaseMode::DryRun).unwrap();

    // Then: the assignment stands and the daemon was not signalled.
    let assignment: (Option<String>, Option<String>) = db
        .get_events(None, None)
        .unwrap()
        .into_iter()
        .map(|event| (event.stream_id, event.assignment_source))
        .next()
        .unwrap();
    assert_eq!(
        assignment,
        (Some("container".to_string()), Some("inferred".to_string()))
    );
    assert_eq!(db.get_db_version().unwrap(), version_before);
}

/// Applying through the command releases the same row the preview named.
#[test]
fn applying_through_the_command_releases_the_pane() {
    // Given: a pane focus the propagator assigned.
    let db = db_with_propagated_pane();

    // When: the command applies the release.
    release_pane_focus(&db, ReleaseMode::Apply).unwrap();

    // Then: the event is indistinguishable from one never classified, and still there.
    let events = db.get_events(None, None).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].stream_id, None);
    assert_eq!(events[0].assignment_source, None);
}

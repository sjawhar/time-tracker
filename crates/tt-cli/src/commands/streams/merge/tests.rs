use super::{MergedRow, format_merged, merge_all};
use tt_db::{Database, MergeMode, MergedSource, Proposal, ProposalStatus, StoredEvent, Stream};

fn stream(id: &str, name: &str, slug: Option<&str>) -> Stream {
    let now = chrono::Utc::now();
    Stream {
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

/// Inserts one event per `(id, assignment_source)` pair, all on `stream_id`.
fn assign(db: &Database, stream_id: &str, events: &[(&str, &str)]) {
    for (event_id, source) in events {
        let event = StoredEvent {
            id: (*event_id).to_string(),
            timestamp: chrono::Utc::now(),
            event_type: tt_core::EventType::WindowFocus,
            source: "laptop.cosmic".to_string(),
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
            assignment_source: Some((*source).to_string()),
            data: serde_json::json!({}),
        };
        db.insert_event(&event).unwrap();
    }
}

fn refs(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

/// Files one proposal naming `stream_id`, in the given review state.
fn insert_proposal(db: &Database, id: &str, stream_id: &str, status: ProposalStatus) {
    db.insert_proposal(&Proposal {
        id: id.to_string(),
        created_at: chrono::Utc::now(),
        session_id: None,
        event_ids: Some(vec![format!("{id}-event")]),
        proposed_stream_id: Some(stream_id.to_string()),
        proposed_new_stream: None,
        confidence: 0.6,
        reasoning: "Belongs with the work order.".to_string(),
        status,
        classifier_generation: None,
    })
    .unwrap();
}

/// The stream a proposal currently names.
fn proposal_target(db: &Database, id: &str) -> Option<String> {
    db.get_proposals(None)
        .unwrap()
        .into_iter()
        .find(|proposal| proposal.id == id)
        .and_then(|proposal| proposal.proposed_stream_id)
}

/// Two week buckets of one initiative, plus the renamed row they collapse onto.
fn db_with_weeks() -> Database {
    let db = Database::open_in_memory().unwrap();
    db.insert_stream(&stream("wk1", "workorder-5: IPI envs (Jun14-20)", None))
        .unwrap();
    db.insert_stream(&stream("wk2", "workorder-5: IPI envs (Jun21-27)", None))
        .unwrap();
    db.insert_stream(&stream("wo5", "workorder-5: IPI envs", Some("workorder-5")))
        .unwrap();
    assign(&db, "wk1", &[("e1", "inferred"), ("e2", "user")]);
    assign(&db, "wk2", &[("e3", "terminal_focus")]);
    db
}

#[test]
fn merges_every_source_in_one_invocation() {
    // Given: two week buckets of one initiative and the row they belong on.
    let db = db_with_weeks();

    // When: both are merged into it.
    let (target, merged) =
        merge_all(&db, &refs(&["wk1", "wk2"]), "workorder-5", MergeMode::Apply).unwrap();

    // Then: every event lands on the target and both sources are retired.
    assert_eq!(target, "workorder-5: IPI envs (wo5)");
    assert_eq!(merged.len(), 2);
    assert_eq!(db.get_events_by_stream("wo5").unwrap().len(), 3);
    assert!(db.get_stream("wk1").unwrap().is_none());
    assert!(db.get_stream("wk2").unwrap().is_none());
}

#[test]
fn carries_human_assignments_onto_the_target() {
    // Given: a source holding one event a human classified by hand. A merge corrects
    // which row holds the work, not the human's judgement about what it was.
    let db = db_with_weeks();

    // When: it is merged.
    let (_, merged) = merge_all(&db, &refs(&["wk1"]), "wo5", MergeMode::Apply).unwrap();

    // Then: the human assignment moved and is reported as such.
    assert_eq!(merged[0].source.events_moved, 2);
    assert_eq!(merged[0].source.user_events_moved, 1);
    assert_eq!(db.get_events_by_stream("wo5").unwrap().len(), 2);
}

#[test]
fn moves_tags_without_duplicating_them() {
    // Given: a source and target sharing one tag, with one tag unique to the source.
    let db = db_with_weeks();
    db.add_tag("wk1", "work").unwrap();
    db.add_tag("wk1", "ipi").unwrap();
    db.add_tag("wo5", "work").unwrap();

    // When: the source is merged.
    let (_, merged) = merge_all(&db, &refs(&["wk1"]), "wo5", MergeMode::Apply).unwrap();

    // Then: only the unheld tag counts as moved, and neither is duplicated.
    assert_eq!(merged[0].source.tags_moved, 1);
    assert_eq!(db.get_tags("wo5").unwrap(), vec!["ipi", "work"]);
}

#[test]
fn dry_run_leaves_the_database_untouched() {
    // Given: two sources a real merge would empty and retire.
    let db = db_with_weeks();
    let version_before = db.get_db_version().unwrap();

    // When: the merge runs in dry-run mode.
    let (_, merged) = merge_all(&db, &refs(&["wk1", "wk2"]), "wo5", MergeMode::DryRun).unwrap();

    // Then: the counts are reported and nothing moved.
    assert_eq!(merged[0].source.events_moved, 2);
    assert_eq!(db.get_events_by_stream("wk1").unwrap().len(), 2);
    assert_eq!(db.get_events_by_stream("wk2").unwrap().len(), 1);
    assert!(db.get_events_by_stream("wo5").unwrap().is_empty());
    assert!(db.get_stream("wk1").unwrap().is_some());
    assert_eq!(db.get_db_version().unwrap(), version_before);
}

#[test]
fn refuses_merging_a_stream_into_itself() {
    // Given: a source and a target reference that resolve to the same stream.
    let db = db_with_weeks();

    // When: the two are merged, named by id and by slug.
    let error = merge_all(&db, &refs(&["wo5"]), "workorder-5", MergeMode::Apply).unwrap_err();

    // Then: it is refused, and nothing was written.
    assert!(error.to_string().contains("itself"));
    assert_eq!(db.get_events_by_stream("wk1").unwrap().len(), 2);
}

#[test]
fn a_repeated_source_reference_is_merged_once() {
    // Given: one source named twice, by id and by name.
    let db = db_with_weeks();

    // When: both references are passed together.
    let (_, merged) = merge_all(
        &db,
        &refs(&["wk1", "workorder-5: IPI envs (Jun14-20)"]),
        "wo5",
        MergeMode::Apply,
    )
    .unwrap();

    // Then: it is reported once, so the totals stay honest.
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].source.events_moved, 2);
}

#[test]
fn an_unknown_source_reference_aborts_before_any_write() {
    // Given: a good source reference followed by one that names nothing.
    let db = db_with_weeks();

    // When: both are merged in one call.
    let error = merge_all(&db, &refs(&["wk1", "nope"]), "wo5", MergeMode::Apply).unwrap_err();

    // Then: the call fails and the resolvable source is untouched.
    assert!(error.to_string().contains("nope"));
    assert_eq!(db.get_events_by_stream("wk1").unwrap().len(), 2);
}

#[test]
fn an_unknown_target_reference_aborts_before_any_write() {
    // Given: a target reference naming nothing.
    let db = db_with_weeks();

    // When: a source is merged into it.
    let error = merge_all(&db, &refs(&["wk1"]), "gone", MergeMode::Apply).unwrap_err();

    // Then: the call fails and no event moved.
    assert!(error.to_string().contains("gone"));
    assert_eq!(db.get_events_by_stream("wk1").unwrap().len(), 2);
}

fn sample_report() -> Vec<MergedRow> {
    vec![
        MergedRow {
            name: "workorder-5: IPI envs + wo-005 (Jun14-20)".to_string(),
            id_short: "0f3a91c2".to_string(),
            source: MergedSource {
                stream_id: "0f3a91c2-aaaa-bbbb-cccc-ddddddddddd1".to_string(),
                events_moved: 4347,
                user_events_moved: 12,
                tags_moved: 1,
                proposals_repointed: 3,
                retired: true,
            },
        },
        MergedRow {
            name: "workorder-5: IPI envs + wo-005 (Jun21-27)".to_string(),
            id_short: "a1b2c3d4".to_string(),
            source: MergedSource {
                stream_id: "a1b2c3d4-aaaa-bbbb-cccc-ddddddddddd2".to_string(),
                events_moved: 0,
                user_events_moved: 0,
                tags_moved: 0,
                proposals_repointed: 0,
                retired: false,
            },
        },
    ]
}

#[test]
fn formats_a_dry_run_report() {
    insta::assert_snapshot!(
        format_merged(
            "workorder-5: IPI envs + wo-005 (9c7e5511)",
            &sample_report(),
            MergeMode::DryRun
        )
        .unwrap()
    );
}

#[test]
fn formats_an_applied_report() {
    insta::assert_snapshot!(
        format_merged(
            "workorder-5: IPI envs + wo-005 (9c7e5511)",
            &sample_report(),
            MergeMode::Apply
        )
        .unwrap()
    );
}

#[test]
fn reports_the_pending_proposals_it_re_points_at_the_target() {
    // Given: a week bucket carrying a pending proposal and one a human already decided.
    // A merge says the work belongs on another row, so the pending question follows it;
    // the decided one is a historical record.
    let db = db_with_weeks();
    insert_proposal(&db, "pending-one", "wk1", ProposalStatus::Pending);
    insert_proposal(&db, "already-accepted", "wk1", ProposalStatus::Accepted);

    // When: the bucket is merged into the initiative it belongs to.
    let (_, merged) = merge_all(&db, &refs(&["wk1"]), "wo5", MergeMode::Apply).unwrap();

    // Then: only the pending one moved, and the report says how many.
    assert_eq!(merged[0].source.proposals_repointed, 1);
    assert_eq!(proposal_target(&db, "pending-one").as_deref(), Some("wo5"));
    assert_eq!(
        proposal_target(&db, "already-accepted").as_deref(),
        Some("wk1")
    );
    let report = format_merged("workorder-5: IPI envs (wo5)", &merged, MergeMode::Apply).unwrap();
    assert!(report.contains("1 pending proposal re-pointed"), "{report}");
}

#[test]
fn a_merge_moving_no_proposal_says_nothing_about_proposals() {
    // Given: sources no proposal names.
    let db = db_with_weeks();

    // When: they are merged.
    let (target, merged) = merge_all(&db, &refs(&["wk1", "wk2"]), "wo5", MergeMode::Apply).unwrap();

    // Then: the report says nothing about proposals, because none moved.
    let report = format_merged(&target, &merged, MergeMode::Apply).unwrap();
    assert!(!report.contains("proposal"), "{report}");
}

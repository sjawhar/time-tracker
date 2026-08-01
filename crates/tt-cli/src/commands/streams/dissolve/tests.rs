use super::{Dissolved, StrandedLink, dissolve_all, dissolve_report, format_dissolved};
use crate::Config;
use tt_db::{
    Database, DissolveMode, DissolveOutcome, Proposal, ProposalStatus, StoredEvent,
    StrandedProposal, Stream,
};

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
            window_app_id: Some("com.mitchellh.ghostty".to_string()),
            window_title: Some("tmux attach -t dev".to_string()),
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

/// Files one pending proposal naming `stream_id`, over the given events.
fn insert_pending_proposal(db: &Database, id: &str, stream_id: &str, event_ids: &[&str]) {
    db.insert_proposal(&Proposal {
        id: id.to_string(),
        created_at: chrono::Utc::now(),
        session_id: None,
        event_ids: Some(event_ids.iter().map(|id| (*id).to_string()).collect()),
        proposed_stream_id: Some(stream_id.to_string()),
        proposed_new_stream: None,
        confidence: 0.6,
        reasoning: "Belongs with the stragglers.".to_string(),
        status: ProposalStatus::Pending,
        classifier_generation: None,
    })
    .unwrap();
}

/// Reads a proposal's whole row straight from SQL, to prove nothing rewrote it.
fn proposal_row(db: &Database, id: &str) -> Vec<Option<String>> {
    let proposals = db.get_proposals(None).unwrap();
    let proposal = proposals
        .into_iter()
        .find(|proposal| proposal.id == id)
        .expect("proposal row");
    vec![
        Some(proposal.id),
        proposal.session_id,
        proposal.event_ids.map(|ids| ids.join(",")),
        proposal.proposed_stream_id,
        proposal.proposed_new_stream,
        Some(proposal.confidence.to_string()),
        Some(proposal.reasoning),
        Some(format!("{:?}", proposal.status)),
        proposal
            .classifier_generation
            .map(|value| value.to_string()),
    ]
}

/// Writes `streams.md` into a temp todo store and points a `Config` at it.
fn config_with_stream_links(temp: &tempfile::TempDir, streams_md: &str) -> Config {
    let store = temp.path().join("todo-store");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("streams.md"), streams_md).unwrap();
    Config {
        database_path: temp.path().join("tt.db"),
        todo_store_path: store,
        ..Config::default()
    }
}

#[test]
fn dissolves_every_reference_in_one_invocation() {
    // Given: two catch-all streams, each holding machine-assigned events.
    let db = Database::open_in_memory().unwrap();
    db.insert_stream(&stream("s1", "misc: stragglers", None))
        .unwrap();
    db.insert_stream(&stream("s2", "other: shell / nav", None))
        .unwrap();
    assign(&db, "s1", &[("e1", "inferred"), ("e2", "inferred")]);
    assign(&db, "s2", &[("e3", "terminal_focus")]);

    // When: both are dissolved in one call.
    let dissolved = dissolve_all(&db, &refs(&["s1", "s2"]), DissolveMode::Apply).unwrap();

    // Then: each reports its own release and both streams are retired.
    assert_eq!(dissolved.len(), 2);
    assert_eq!(
        dissolved[0].outcome,
        DissolveOutcome {
            released: 2,
            retained: 0,
            retired: true,
        }
    );
    assert_eq!(
        dissolved[1].outcome,
        DissolveOutcome {
            released: 1,
            retained: 0,
            retired: true,
        }
    );
    assert_eq!(db.unassigned_event_ids().unwrap().len(), 3);
    assert!(db.get_stream("s1").unwrap().is_none());
    assert!(db.get_stream("s2").unwrap().is_none());
}

#[test]
fn keeps_a_stream_holding_user_assignments() {
    // Given: a stream a human has partly classified by hand.
    let db = Database::open_in_memory().unwrap();
    db.insert_stream(&stream("s1", "other: dev-env", None))
        .unwrap();
    assign(&db, "s1", &[("e1", "inferred"), ("e2", "user")]);

    // When: it is dissolved.
    let dissolved = dissolve_all(&db, &refs(&["s1"]), DissolveMode::Apply).unwrap();

    // Then: the human assignment and the stream both survive.
    assert_eq!(
        dissolved[0].outcome,
        DissolveOutcome {
            released: 1,
            retained: 1,
            retired: false,
        }
    );
    assert!(db.get_stream("s1").unwrap().is_some());
    assert_eq!(db.get_events_by_stream("s1").unwrap().len(), 1);
}

#[test]
fn dry_run_leaves_the_database_untouched() {
    // Given: a stream that a real dissolution would empty and retire.
    let db = Database::open_in_memory().unwrap();
    db.insert_stream(&stream("s1", "misc (Jun14-20)", None))
        .unwrap();
    assign(&db, "s1", &[("e1", "inferred")]);
    let version_before = db.get_db_version().unwrap();

    // When: it is dissolved in dry-run mode.
    let dissolved = dissolve_all(&db, &refs(&["s1"]), DissolveMode::DryRun).unwrap();

    // Then: the same counts are reported and nothing changed.
    assert_eq!(
        dissolved[0].outcome,
        DissolveOutcome {
            released: 1,
            retained: 0,
            retired: true,
        }
    );
    assert_eq!(db.get_events_by_stream("s1").unwrap().len(), 1);
    assert!(db.get_stream("s1").unwrap().is_some());
    assert_eq!(db.get_db_version().unwrap(), version_before);
}

#[test]
fn unknown_reference_aborts_before_any_write() {
    // Given: a good reference followed by one that names nothing.
    let db = Database::open_in_memory().unwrap();
    db.insert_stream(&stream("s1", "misc: stragglers", None))
        .unwrap();
    assign(&db, "s1", &[("e1", "inferred")]);

    // When: both are dissolved in one call.
    let error = dissolve_all(&db, &refs(&["s1", "nope"]), DissolveMode::Apply).unwrap_err();

    // Then: the call fails and the resolvable stream is untouched.
    assert!(error.to_string().contains("nope"));
    assert_eq!(db.get_events_by_stream("s1").unwrap().len(), 1);
    assert!(db.get_stream("s1").unwrap().is_some());
}

#[test]
fn resolves_references_by_id_slug_and_name() {
    // Given: three streams, each reachable by a different kind of reference.
    let db = Database::open_in_memory().unwrap();
    db.insert_stream(&stream("s1", "by id", None)).unwrap();
    db.insert_stream(&stream("s2", "by slug", Some("straggler-bucket")))
        .unwrap();
    db.insert_stream(&stream("s3", "ops: devbox nav (Jul5-11)", None))
        .unwrap();

    // When: each is named by a different reference form.
    let dissolved = dissolve_all(
        &db,
        &refs(&["s1", "straggler-bucket", "ops: devbox nav (Jul5-11)"]),
        DissolveMode::Apply,
    )
    .unwrap();

    // Then: all three resolve and retire.
    assert_eq!(dissolved.len(), 3);
    assert!(dissolved.iter().all(|entry| entry.outcome.retired));
}

#[test]
fn a_repeated_reference_is_dissolved_once() {
    // Given: one stream named twice, by id and by name.
    let db = Database::open_in_memory().unwrap();
    db.insert_stream(&stream("s1", "misc: stragglers", None))
        .unwrap();
    assign(&db, "s1", &[("e1", "inferred")]);

    // When: both references are passed together.
    let dissolved =
        dissolve_all(&db, &refs(&["s1", "misc: stragglers"]), DissolveMode::Apply).unwrap();

    // Then: it is reported once, so the totals stay honest.
    assert_eq!(dissolved.len(), 1);
    assert_eq!(dissolved[0].outcome.released, 1);
}

fn sample_report() -> Vec<Dissolved> {
    vec![
        Dissolved {
            name: "misc: stragglers".to_string(),
            id: "0f3a91c2-aaaa-bbbb-cccc-ddddddddddd1".to_string(),
            link_keys: vec!["misc: stragglers".to_string()],
            outcome: DissolveOutcome {
                released: 4812,
                retained: 0,
                retired: true,
            },
        },
        Dissolved {
            name: "other: dev-env (dotfiles/settings/jj/oma)".to_string(),
            id: "a1b2c3d4-aaaa-bbbb-cccc-ddddddddddd2".to_string(),
            link_keys: vec!["other: dev-env (dotfiles/settings/jj/oma)".to_string()],
            outcome: DissolveOutcome {
                released: 120,
                retained: 7,
                retired: false,
            },
        },
    ]
}

fn sample_stranded() -> Vec<StrandedLink> {
    vec![StrandedLink {
        line_number: 25,
        stream: "misc: stragglers".to_string(),
        priority: "ops".to_string(),
    }]
}

fn sample_stranded_proposals() -> Vec<StrandedProposal> {
    vec![
        StrandedProposal {
            proposal_id: "3542bdf0-c7bf-4d7f-a050-2e1e94fb8abe".to_string(),
            stream_id: "0f3a91c2-aaaa-bbbb-cccc-ddddddddddd1".to_string(),
            event_count: 12,
        },
        StrandedProposal {
            proposal_id: "7b19e4aa-1d25-4f60-9c02-8ac0f1f0b3d5".to_string(),
            stream_id: "a1b2c3d4-aaaa-bbbb-cccc-ddddddddddd2".to_string(),
            event_count: 1,
        },
    ]
}

#[test]
fn formats_a_dry_run_report() {
    insta::assert_snapshot!(
        format_dissolved(&sample_report(), &[], &[], DissolveMode::DryRun).unwrap()
    );
}

#[test]
fn formats_an_applied_report() {
    insta::assert_snapshot!(
        format_dissolved(&sample_report(), &[], &[], DissolveMode::Apply).unwrap()
    );
}

#[test]
fn formats_a_dry_run_report_with_stranded_links() {
    insta::assert_snapshot!(
        format_dissolved(
            &sample_report(),
            &sample_stranded(),
            &[],
            DissolveMode::DryRun
        )
        .unwrap()
    );
}

#[test]
fn formats_an_applied_report_with_stranded_links() {
    insta::assert_snapshot!(
        format_dissolved(
            &sample_report(),
            &sample_stranded(),
            &[],
            DissolveMode::Apply
        )
        .unwrap()
    );
}

#[test]
fn formats_a_dry_run_report_with_stranded_links_and_proposals() {
    insta::assert_snapshot!(
        format_dissolved(
            &sample_report(),
            &sample_stranded(),
            &sample_stranded_proposals(),
            DissolveMode::DryRun
        )
        .unwrap()
    );
}

#[test]
fn formats_an_applied_report_with_stranded_links_and_proposals() {
    insta::assert_snapshot!(
        format_dissolved(
            &sample_report(),
            &sample_stranded(),
            &sample_stranded_proposals(),
            DissolveMode::Apply
        )
        .unwrap()
    );
}

#[test]
fn dry_run_reports_a_stream_link_that_would_dangle() {
    // Given: a stream streams.md links by slug, about to be dissolved.
    let db = Database::open_in_memory().unwrap();
    db.insert_stream(&stream(
        "s1",
        "Meetings and coordination",
        Some("meetings-coord"),
    ))
    .unwrap();
    assign(&db, "s1", &[("e1", "inferred")]);
    let temp = tempfile::TempDir::new().unwrap();
    let config = config_with_stream_links(
        &temp,
        "- hawk-platform <!-- tt-stream:{\"priority\":\"sales\"} -->\n\
         - meetings-coord <!-- tt-stream:{\"priority\":\"ops\"} -->\n",
    );

    // When: the dissolution is previewed.
    let report = dissolve_report(
        &db,
        &config,
        &refs(&["meetings-coord"]),
        DissolveMode::DryRun,
    )
    .unwrap();

    // Then: the operator is told which line would dangle, before deciding to apply,
    // and nothing has been written to either the database or the file.
    assert!(report.contains("line 2"), "{report}");
    assert!(report.contains("'meetings-coord'"), "{report}");
    assert!(report.contains("'ops'"), "{report}");
    assert!(!report.contains("hawk-platform"), "{report}");
    assert!(db.get_stream("s1").unwrap().is_some());
    assert_eq!(
        std::fs::read_to_string(config.todo_store_path.join("streams.md")).unwrap(),
        "- hawk-platform <!-- tt-stream:{\"priority\":\"sales\"} -->\n\
         - meetings-coord <!-- tt-stream:{\"priority\":\"ops\"} -->\n"
    );
}

#[test]
fn a_link_naming_a_stream_by_display_name_is_reported_too() {
    // Given: streams.md names this stream the other way it is allowed to — by exact name.
    let db = Database::open_in_memory().unwrap();
    db.insert_stream(&stream("s1", "time-tracker: weekly standups", None))
        .unwrap();
    let temp = tempfile::TempDir::new().unwrap();
    let config = config_with_stream_links(
        &temp,
        "- time-tracker: weekly standups <!-- tt-stream:{\"priority\":\"ops\"} -->\n",
    );

    // When: the stream is dissolved for real.
    let report = dissolve_report(&db, &config, &refs(&["s1"]), DissolveMode::Apply).unwrap();

    // Then: the name form is matched as well as the slug form.
    assert!(report.contains("line 1"), "{report}");
    assert!(
        report.contains("'time-tracker: weekly standups'"),
        "{report}"
    );
}

#[test]
fn an_unlinked_stream_reports_no_stranded_links() {
    // Given: a stream no priority link references.
    let db = Database::open_in_memory().unwrap();
    db.insert_stream(&stream("s1", "misc: stragglers", None))
        .unwrap();
    let temp = tempfile::TempDir::new().unwrap();
    let config = config_with_stream_links(
        &temp,
        "- hawk-platform <!-- tt-stream:{\"priority\":\"sales\"} -->\n",
    );

    // When: it is dissolved.
    let report = dissolve_report(&db, &config, &refs(&["s1"]), DissolveMode::Apply).unwrap();

    // Then: the report says nothing about links, because none were broken.
    assert!(!report.contains("dangl"), "{report}");
}

#[test]
fn dry_run_reports_a_pending_proposal_that_would_be_stranded() {
    // Given: a stream with a pending proposal naming it. `proposed_stream_id` has no
    // foreign key, so retiring the stream leaves a row no reviewer can accept.
    let db = Database::open_in_memory().unwrap();
    db.insert_stream(&stream("s1", "misc: stragglers", None))
        .unwrap();
    assign(&db, "s1", &[("e1", "inferred")]);
    insert_pending_proposal(
        &db,
        "3542bdf0-c7bf-4d7f-a050-2e1e94fb8abe",
        "s1",
        &["e1", "e2"],
    );
    let temp = tempfile::TempDir::new().unwrap();
    let config = config_with_stream_links(&temp, "");

    // When: the dissolution is previewed.
    let report = dissolve_report(&db, &config, &refs(&["s1"]), DissolveMode::DryRun).unwrap();

    // Then: the operator is told what would be stranded, before deciding to apply.
    assert!(
        report.contains("Proposals that would be stranded:"),
        "{report}"
    );
    assert!(
        report.contains("proposal 3542bdf0 \u{2192} stream s1 (2 events)"),
        "{report}"
    );
}

#[test]
fn an_applied_dissolution_names_the_proposal_it_stranded() {
    // Given: the same stream and pending proposal.
    let db = Database::open_in_memory().unwrap();
    db.insert_stream(&stream("s1", "misc: stragglers", None))
        .unwrap();
    assign(&db, "s1", &[("e1", "inferred")]);
    insert_pending_proposal(&db, "3542bdf0-c7bf-4d7f-a050-2e1e94fb8abe", "s1", &["e1"]);
    let before = proposal_row(&db, "3542bdf0-c7bf-4d7f-a050-2e1e94fb8abe");
    let temp = tempfile::TempDir::new().unwrap();
    let config = config_with_stream_links(&temp, "");

    // When: it is dissolved for real.
    let report = dissolve_report(&db, &config, &refs(&["s1"]), DissolveMode::Apply).unwrap();

    // Then: the stranded proposal is named, and left exactly as it was — a dissolution
    // has no stream to re-point it to, and a status the human did not choose is not
    // this command's to write.
    assert!(report.contains("Proposals now stranded:"), "{report}");
    assert!(
        report.contains("proposal 3542bdf0 \u{2192} stream s1 (1 event)"),
        "{report}"
    );
    assert!(db.get_stream("s1").unwrap().is_none());
    assert_eq!(
        proposal_row(&db, "3542bdf0-c7bf-4d7f-a050-2e1e94fb8abe"),
        before
    );
}

#[test]
fn a_dissolution_stranding_no_proposal_says_nothing_about_proposals() {
    // Given: a pending proposal naming a stream this invocation does not touch.
    let db = Database::open_in_memory().unwrap();
    db.insert_stream(&stream("s1", "misc: stragglers", None))
        .unwrap();
    db.insert_stream(&stream("s2", "time-tracker: tooling", None))
        .unwrap();
    assign(&db, "s1", &[("e1", "inferred")]);
    insert_pending_proposal(&db, "elsewhere", "s2", &["e2"]);
    let temp = tempfile::TempDir::new().unwrap();
    let config = config_with_stream_links(&temp, "");

    // When: the other stream is dissolved.
    let report = dissolve_report(&db, &config, &refs(&["s1"]), DissolveMode::Apply).unwrap();

    // Then: the report says nothing about proposals, because none were stranded.
    assert!(!report.contains("Proposals"), "{report}");
}

use insta::assert_snapshot;
use tt_db::{Database, StoredEvent, Stream};

use super::{MisnamedStream, collect_misnamed, format_misnamed, format_misnamed_json};

fn stream(id: &str, name: Option<&str>, direct_ms: i64) -> Stream {
    let now = chrono::Utc::now();
    Stream {
        id: id.to_string(),
        created_at: now,
        updated_at: now,
        name: name.map(String::from),
        slug: None,
        description: None,
        color: None,
        time_direct_ms: direct_ms,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: false,
    }
}

/// Inserts `count` events, all pointing at `stream_id`.
fn assign(db: &Database, stream_id: &str, count: usize) {
    for index in 0..count {
        let event = StoredEvent {
            id: format!("{stream_id}-{index}"),
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
            assignment_source: Some("inferred".to_string()),
            data: serde_json::json!({}),
        };
        db.insert_event(&event).unwrap();
    }
}

#[test]
fn flags_an_activity_type_stream_but_not_real_work_sharing_its_words() {
    // Given: a container naming a posture, and a real initiative whose name merely
    // contains one of the same generic words. A `%nav%` substring rule caught the
    // second during remediation.
    let db = Database::open_in_memory().unwrap();
    db.insert_stream(&stream("s1", Some("other: shell / nav / transitional"), 0))
        .unwrap();
    db.insert_stream(&stream(
        "s2",
        Some("agent-c: calendar navigation debugging"),
        0,
    ))
    .unwrap();

    // When: the misnamed streams are collected.
    let misnamed = collect_misnamed(&db).unwrap();

    // Then: only the posture container is flagged.
    assert_eq!(misnamed.len(), 1);
    assert_eq!(misnamed[0].name, "other: shell / nav / transitional");
    assert_eq!(misnamed[0].reason, "activity_type");
}

#[test]
fn flags_a_date_range_and_a_catch_all_with_their_reasons() {
    // Given: a week bucket over a real initiative, and a leftovers container.
    let db = Database::open_in_memory().unwrap();
    db.insert_stream(&stream(
        "s1",
        Some("workorder-5: IPI envs + wo-005 (Jun14-20)"),
        0,
    ))
    .unwrap();
    db.insert_stream(&stream("s2", Some("misc: stragglers"), 0))
        .unwrap();

    // When: the misnamed streams are collected.
    let misnamed = collect_misnamed(&db).unwrap();

    // Then: both are flagged, each with the reason that decides how to act on it.
    let reasons: Vec<(&str, &str)> = misnamed
        .iter()
        .map(|entry| (entry.name.as_str(), entry.reason))
        .collect();
    assert!(reasons.contains(&("workorder-5: IPI envs + wo-005 (Jun14-20)", "date_range")));
    assert!(reasons.contains(&("misc: stragglers", "catch_all")));
}

#[test]
fn reports_event_counts_and_direct_time_per_stream() {
    // Given: a catch-all holding events and carrying materialized direct time.
    let db = Database::open_in_memory().unwrap();
    db.insert_stream(&stream("s1", Some("misc: stragglers"), 4_500_000))
        .unwrap();
    assign(&db, "s1", 3);

    // When: the misnamed streams are collected.
    let misnamed = collect_misnamed(&db).unwrap();

    // Then: the stake in acting on the row is reported alongside it.
    assert_eq!(misnamed[0].events, 3);
    assert_eq!(misnamed[0].time_direct_ms, 4_500_000);
}

#[test]
fn is_a_report_and_writes_nothing() {
    // Given: a catch-all stream holding events.
    let db = Database::open_in_memory().unwrap();
    db.insert_stream(&stream("s1", Some("misc: stragglers"), 0))
        .unwrap();
    assign(&db, "s1", 2);
    let version_before = db.get_db_version().unwrap();

    // When: the report is collected.
    collect_misnamed(&db).unwrap();

    // Then: the stream, its events, and the change signal are all untouched.
    assert!(db.get_stream("s1").unwrap().is_some());
    assert_eq!(db.get_events_by_stream("s1").unwrap().len(), 2);
    assert_eq!(db.get_db_version().unwrap(), version_before);
}

#[test]
fn skips_streams_with_no_name() {
    // Given: a stream nobody has named — there is no name to judge.
    let db = Database::open_in_memory().unwrap();
    db.insert_stream(&stream("s1", None, 0)).unwrap();

    // When/Then: it is not flagged.
    assert!(collect_misnamed(&db).unwrap().is_empty());
}

#[test]
fn orders_rows_by_the_events_at_stake() {
    // Given: three misnamed streams holding different numbers of events.
    let db = Database::open_in_memory().unwrap();
    for (id, name, events) in [
        ("s1", "misc: stragglers", 1),
        ("s2", "other: shell / nav", 5),
        ("s3", "misc (Jun14-20)", 3),
    ] {
        db.insert_stream(&stream(id, Some(name), 0)).unwrap();
        assign(&db, id, events);
    }

    // When: the misnamed streams are collected.
    let misnamed = collect_misnamed(&db).unwrap();

    // Then: the largest blast radius comes first.
    let events: Vec<u64> = misnamed.iter().map(|entry| entry.events).collect();
    assert_eq!(events, vec![5, 3, 1]);
}

fn sample_report() -> Vec<MisnamedStream> {
    vec![
        MisnamedStream {
            id: "0f3a91c2-aaaa-bbbb-cccc-ddddddddddd1".to_string(),
            id_short: "0f3a91c2".to_string(),
            name: "workorder-5: IPI envs + wo-005 (Jun14-20)".to_string(),
            reason: "date_range",
            events: 4347,
            time_direct_ms: 45_000_000,
        },
        MisnamedStream {
            id: "a1b2c3d4-aaaa-bbbb-cccc-ddddddddddd2".to_string(),
            id_short: "a1b2c3d4".to_string(),
            name: "other: team coordination (Slack, work email)".to_string(),
            reason: "catch_all",
            events: 1204,
            time_direct_ms: 12_600_000,
        },
        MisnamedStream {
            id: "beef1234-aaaa-bbbb-cccc-ddddddddddd3".to_string(),
            id_short: "beef1234".to_string(),
            name: "other: shell / nav / transitional".to_string(),
            reason: "activity_type",
            events: 88,
            time_direct_ms: 0,
        },
    ]
}

#[test]
fn formats_the_misnamed_report() {
    assert_snapshot!(format_misnamed(&sample_report()).unwrap());
}

#[test]
fn formats_an_empty_misnamed_report() {
    assert_snapshot!(format_misnamed(&[]).unwrap());
}

#[test]
fn formats_the_misnamed_report_as_json() {
    assert_snapshot!(format_misnamed_json(&sample_report()).unwrap());
}

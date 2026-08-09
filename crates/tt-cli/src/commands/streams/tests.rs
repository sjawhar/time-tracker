use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use insta::assert_snapshot;
use tt_db::{Database, StoredEvent, Stream};

use super::{format_streams, format_streams_json, get_streams_for_display};

/// Fixed clock for these fixtures: stream times were computed at 09:00 on the
/// 28th and every listing renders at noon on the 29th, so the freshness footer
/// is deterministic.
fn computed_at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2025, 1, 28, 9, 0, 0).unwrap()
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2025, 1, 29, 12, 0, 0).unwrap()
}

/// A stream carrying computed times, with both event-timestamp columns NULL —
/// the shape every stream in the live table has had since the `--apply` engine
/// that used to write them was deleted.
fn make_stream(id: &str, name: Option<&str>, direct_ms: i64, delegated_ms: i64) -> Stream {
    Stream {
        id: id.to_string(),
        name: name.map(String::from),
        slug: None,
        description: None,
        color: None,
        created_at: computed_at(),
        updated_at: computed_at(),
        time_direct_ms: direct_ms,
        time_delegated_ms: delegated_ms,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: false,
    }
}

/// Points one event at `stream_id` at `at`.
///
/// This is what actually puts a stream in the listing's window. The
/// `streams.last_event_at` column reads like it would, and nothing writes it:
/// on the live table 985 of 1,245 streams have it NULL and the newest value in
/// the other 260 is 2026-04-30, 99 days behind the newest event.
fn assign_event(db: &Database, stream_id: &str, at: DateTime<Utc>) {
    let event = StoredEvent {
        id: format!("{stream_id}-{}", at.timestamp_millis()),
        timestamp: at,
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

/// Inserts `stream` and one event pointing at it at `active_at`.
fn insert_active(db: &Database, stream: &Stream, active_at: DateTime<Utc>) {
    db.insert_stream(stream).unwrap();
    assign_event(db, &stream.id, active_at);
}

#[test]
fn test_streams_empty_database() {
    let db = Database::open_in_memory().unwrap();
    let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();

    let entries = get_streams_for_display(&db, today).unwrap();
    assert!(entries.is_empty());

    let output = format_streams(&entries, now()).unwrap();
    insta::with_settings!({snapshot_path => "../snapshots"}, {
        assert_snapshot!(output);
    });
}

#[test]
fn test_streams_single_stream_no_tags() {
    let db = Database::open_in_memory().unwrap();
    let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
    let recent = Utc.with_ymd_and_hms(2025, 1, 28, 12, 0, 0).unwrap();

    let mut stream = make_stream(
        "abc123def456",
        Some("tmux/dev/session-1"),
        8_100_000,
        16_200_000,
    );
    stream.slug = Some("acme-sprint-42".to_owned());
    insert_active(&db, &stream, recent);

    let entries = get_streams_for_display(&db, today).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id_short, "abc123");
    assert!(entries[0].tags.is_empty());

    let output = format_streams(&entries, now()).unwrap();
    assert!(output.contains("acme-sprint-42"));
    insta::with_settings!({snapshot_path => "../snapshots"}, {
        assert_snapshot!(output);
    });
}

#[test]
fn test_streams_multiple_with_tags() {
    let db = Database::open_in_memory().unwrap();
    let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
    let recent = Utc.with_ymd_and_hms(2025, 1, 28, 12, 0, 0).unwrap();

    // Stream 1: higher total time, multiple tags
    let mut stream1 = make_stream(
        "abc123def456",
        Some("tmux/dev/session-1"),
        8_100_000,
        16_200_000,
    );
    stream1.slug = Some("acme-sprint-42".to_owned());
    insert_active(&db, &stream1, recent);
    db.add_tag("abc123def456", "acme-webapp").unwrap();
    db.add_tag("abc123def456", "urgent").unwrap();

    // Stream 2: lower total time, one tag
    let stream2 = make_stream(
        "def456ghi789",
        Some("tmux/dev/session-2"),
        2_700_000,
        4_800_000,
    );
    insert_active(&db, &stream2, recent);
    db.add_tag("def456ghi789", "internal").unwrap();

    // Stream 3: lowest time, no tags
    let stream3 = make_stream(
        "ghi789jkl012",
        Some("tmux/staging/session-1"),
        1_800_000,
        900_000,
    );
    insert_active(&db, &stream3, recent);

    let entries = get_streams_for_display(&db, today).unwrap();

    // Should be sorted by total time descending
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].id_short, "abc123"); // 24.3M ms total
    assert_eq!(entries[1].id_short, "def456"); // 7.5M ms total
    assert_eq!(entries[2].id_short, "ghi789"); // 2.7M ms total

    // Check tags
    assert_eq!(entries[0].tags, vec!["acme-webapp", "urgent"]);
    assert_eq!(entries[1].tags, vec!["internal"]);
    assert!(entries[2].tags.is_empty());

    let output = format_streams(&entries, now()).unwrap();
    insta::with_settings!({snapshot_path => "../snapshots"}, {
        assert_snapshot!(output);
    });
}

#[test]
fn test_streams_zero_time_excluded() {
    let db = Database::open_in_memory().unwrap();
    let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
    let recent = Utc.with_ymd_and_hms(2025, 1, 28, 12, 0, 0).unwrap();

    // Stream with time
    let stream1 = make_stream("abc123def456", Some("has-time"), 3_600_000, 1_800_000);
    insert_active(&db, &stream1, recent);

    // Stream with zero time
    let stream2 = make_stream("def456ghi789", Some("zero-time"), 0, 0);
    insert_active(&db, &stream2, recent);

    let entries = get_streams_for_display(&db, today).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id_short, "abc123");
}

#[test]
fn test_streams_7_day_filtering() {
    let db = Database::open_in_memory().unwrap();
    let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();

    // Stream from 3 days ago (should be included)
    let recent = Utc.with_ymd_and_hms(2025, 1, 26, 12, 0, 0).unwrap();
    let stream1 = make_stream("recent123456", Some("recent-stream"), 3_600_000, 1_800_000);
    insert_active(&db, &stream1, recent);

    // Stream from 10 days ago (should be excluded)
    let old = Utc.with_ymd_and_hms(2025, 1, 19, 12, 0, 0).unwrap();
    let stream2 = make_stream("old123456789", Some("old-stream"), 7_200_000, 3_600_000);
    insert_active(&db, &stream2, old);

    let entries = get_streams_for_display(&db, today).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id_short, "recent");
}

#[test]
fn test_streams_json_output() {
    let db = Database::open_in_memory().unwrap();
    let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
    let recent = Utc.with_ymd_and_hms(2025, 1, 28, 12, 0, 0).unwrap();

    let mut stream = make_stream(
        "abc123def456",
        Some("tmux/dev/session-1"),
        8_100_000,
        16_200_000,
    );
    stream.slug = Some("acme-sprint-42".to_owned());
    insert_active(&db, &stream, recent);
    db.add_tag("abc123def456", "acme-webapp").unwrap();

    let entries = get_streams_for_display(&db, today).unwrap();
    let output = format_streams_json(&entries, today).unwrap();
    assert!(output.contains("\"slug\": \"acme-sprint-42\""));
    insta::with_settings!({snapshot_path => "../snapshots"}, {
        assert_snapshot!(output);
    });
}

#[test]
fn a_stream_with_no_events_is_not_listed() {
    let db = Database::open_in_memory().unwrap();
    let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();

    // Stream nothing ever pointed an event at
    let stream = make_stream("abc123def456", Some("no-events"), 3_600_000, 1_800_000);
    db.insert_stream(&stream).unwrap();

    let entries = get_streams_for_display(&db, today).unwrap();
    assert!(
        entries.is_empty(),
        "streams with no events should be excluded"
    );
}

#[test]
fn test_streams_unnamed_display() {
    let db = Database::open_in_memory().unwrap();
    let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
    let recent = Utc.with_ymd_and_hms(2025, 1, 28, 12, 0, 0).unwrap();

    // Stream without a name
    let stream = make_stream("abc123def456", None, 3_600_000, 1_800_000);
    insert_active(&db, &stream, recent);

    let entries = get_streams_for_display(&db, today).unwrap();
    let output = format_streams(&entries, now()).unwrap();

    assert!(
        output.contains("(unnamed)"),
        "unnamed streams should display as (unnamed)"
    );
}

#[test]
fn test_streams_short_id() {
    let db = Database::open_in_memory().unwrap();
    let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
    let recent = Utc.with_ymd_and_hms(2025, 1, 28, 12, 0, 0).unwrap();

    // Stream with short ID (less than 6 chars)
    let stream = make_stream("abc", Some("short-id-stream"), 3_600_000, 1_800_000);
    insert_active(&db, &stream, recent);

    let entries = get_streams_for_display(&db, today).unwrap();
    assert_eq!(entries[0].id_short, "abc");
}

#[test]
fn test_streams_unicode_name_truncation() {
    let db = Database::open_in_memory().unwrap();
    let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
    let recent = Utc.with_ymd_and_hms(2025, 1, 28, 12, 0, 0).unwrap();

    // Stream with a long Unicode name (Chinese characters, 3 bytes each)
    // 25 characters: should be truncated to 19 + "..."
    let long_name = "这是一个很长的中文名称用来测试截断功能是否正确工作";
    let stream = make_stream("abc123def456", Some(long_name), 3_600_000, 1_800_000);
    insert_active(&db, &stream, recent);

    let entries = get_streams_for_display(&db, today).unwrap();
    // Should not panic, and should produce valid output
    let output = format_streams(&entries, now()).unwrap();
    assert!(
        output.contains("..."),
        "long names should be truncated with ..."
    );
    // Verify truncation uses character count, not byte count
    assert!(
        !output.contains(long_name),
        "the full long name should not appear in truncated output"
    );
}

#[test]
fn junk_is_lifted_out_of_the_listing_but_keeps_its_totals() {
    // Given: the reserved junk stream holding more time than the real work
    // beside it, so it would otherwise head the listing.
    let db = Database::open_in_memory().unwrap();
    let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
    let recent = Utc.with_ymd_and_hms(2025, 1, 28, 12, 0, 0).unwrap();
    let real = make_stream("abc123def456", Some("real work"), 1_800_000, 3_600_000);
    insert_active(&db, &real, recent);
    let mut junk = make_stream(
        tt_db::JUNK_STREAM_SLUG,
        Some("junk: no attributable work"),
        2_700_000,
        54_000_000,
    );
    junk.slug = Some(tt_db::JUNK_STREAM_SLUG.to_string());
    insert_active(&db, &junk, recent);

    // When: the listing is rendered.
    let entries = get_streams_for_display(&db, today).unwrap();
    let output = format_streams(&entries, now()).unwrap();

    // Then: junk takes no row in the table...
    assert!(
        !output.contains("junk: no attributable w"),
        "junk must not be listed as an ordinary stream:\n{output}"
    );
    assert!(output.contains("real work"), "{output}");
    // ...but its totals stay visible, so an over-aggressive junk rule shows up
    // instead of silently shrinking the streams around it.
    assert!(
        output.contains("(junk: 45m direct, 15h 0m delegated"),
        "junk totals must remain reported:\n{output}"
    );
    // And the machine-readable listing keeps it as an ordinary entry.
    let json = format_streams_json(&entries, today).unwrap();
    assert!(json.contains("no attributable work"), "{json}");
}

#[test]
fn a_stream_with_recent_events_is_listed_though_last_event_at_is_null() {
    // Given: a stream worked on yesterday, with `streams.last_event_at` left
    // NULL — which is what every stream minted since the `--apply` engine was
    // deleted looks like, because nothing writes that column.
    let db = Database::open_in_memory().unwrap();
    let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
    let recent = Utc.with_ymd_and_hms(2025, 1, 28, 12, 0, 0).unwrap();
    let stream = make_stream("abc123def456", Some("live work"), 3_600_000, 1_800_000);
    assert!(
        stream.last_event_at.is_none(),
        "the fixture must reproduce the live table's shape"
    );
    db.insert_stream(&stream).unwrap();
    assign_event(&db, "abc123def456", recent);

    // When: the listing is built.
    let entries = get_streams_for_display(&db, today).unwrap();

    // Then: the stream is listed, because activity is read from `events`.
    assert_eq!(
        entries.len(),
        1,
        "a stream with events inside the window must be listed whatever the dead column says"
    );
    assert_eq!(entries[0].id_short, "abc123");
}

#[test]
fn a_stream_whose_events_all_predate_the_window_is_not_listed() {
    // Given: a stream last worked on ten days ago, so its newest event sits
    // outside the seven-day window.
    let db = Database::open_in_memory().unwrap();
    let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
    let old = Utc.with_ymd_and_hms(2025, 1, 19, 12, 0, 0).unwrap();
    let stream = make_stream("old123456789", Some("finished work"), 3_600_000, 1_800_000);
    insert_active(&db, &stream, old);

    // When: the listing is built.
    let entries = get_streams_for_display(&db, today).unwrap();

    // Then: it is left out — reading `events` widens nothing.
    assert!(
        entries.is_empty(),
        "a stream whose newest event predates the window must stay out: {entries:?}"
    );
}

#[test]
fn streams_are_ordered_by_direct_time_not_by_delegated() {
    // Given: a stream with the most attention but modest agent time, listed alongside one
    // with little attention and a great deal of agent time.
    //
    // Sorting by `direct + delegated` is the regression the root AGENTS.md names: delegated
    // routinely exceeds direct by 10-100x, so the sum *is* the delegated ordering. Measured
    // on the live table, `workorder-5: agent-c core` (13h 56m direct, 51h 57m delegated) sat
    // below three streams holding 6h 32m, 4h 48m and 3h 6m of direct time. "Where did my
    // time go" means direct time, so that answers the wrong question.
    let db = Database::open_in_memory().unwrap();
    let attention = make_stream(
        "aaaa1111",
        Some("most attention"),
        14 * 3_600_000,
        52 * 3_600_000,
    );
    let machine = make_stream(
        "bbbb2222",
        Some("most agent time"),
        6 * 3_600_000,
        158 * 3_600_000,
    );
    insert_active(&db, &attention, now());
    insert_active(&db, &machine, now());

    // When: the display list is built.
    let entries = get_streams_for_display(&db, now().date_naive()).unwrap();

    // Then: the one that consumed the most attention leads.
    let order: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(
        order,
        vec!["aaaa1111", "bbbb2222"],
        "direct time is the primary axis of a human-readable view: {entries:?}"
    );
}

#[test]
fn equal_direct_time_falls_back_to_delegated_then_id() {
    // Ties need a stable order or the list reshuffles between runs for no reason.
    let db = Database::open_in_memory().unwrap();
    let more_delegated = make_stream(
        "zzzz9999",
        Some("same direct, more agent"),
        3_600_000,
        90_000_000,
    );
    let less_delegated = make_stream(
        "aaaa0000",
        Some("same direct, less agent"),
        3_600_000,
        10_000_000,
    );
    insert_active(&db, &more_delegated, now());
    insert_active(&db, &less_delegated, now());

    let entries = get_streams_for_display(&db, now().date_naive()).unwrap();

    let order: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(
        order,
        vec!["zzzz9999", "aaaa0000"],
        "equal attention breaks to delegated descending, so the id never decides first"
    );
}

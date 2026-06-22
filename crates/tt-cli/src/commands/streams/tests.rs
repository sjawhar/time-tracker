use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use insta::assert_snapshot;
use tt_db::{Database, Stream};

use super::{format_streams, format_streams_json, get_streams_for_display};

fn make_stream(
    id: &str,
    name: Option<&str>,
    direct_ms: i64,
    delegated_ms: i64,
    last_event_at: Option<DateTime<Utc>>,
) -> Stream {
    let now = Utc::now();
    Stream {
        id: id.to_string(),
        name: name.map(String::from),
        created_at: now,
        updated_at: now,
        time_direct_ms: direct_ms,
        time_delegated_ms: delegated_ms,
        first_event_at: last_event_at,
        last_event_at,
        needs_recompute: false,
    }
}

#[test]
fn test_streams_empty_database() {
    let db = Database::open_in_memory().unwrap();
    let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();

    let entries = get_streams_for_display(&db, today).unwrap();
    assert!(entries.is_empty());

    let output = format_streams(&entries);
    insta::with_settings!({snapshot_path => "../snapshots"}, {
        assert_snapshot!(output);
    });
}

#[test]
fn test_streams_single_stream_no_tags() {
    let db = Database::open_in_memory().unwrap();
    let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
    let recent = Utc.with_ymd_and_hms(2025, 1, 28, 12, 0, 0).unwrap();

    let stream = make_stream(
        "abc123def456",
        Some("tmux/dev/session-1"),
        8_100_000,
        16_200_000,
        Some(recent),
    );
    db.insert_stream(&stream).unwrap();

    let entries = get_streams_for_display(&db, today).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id_short, "abc123");
    assert!(entries[0].tags.is_empty());

    let output = format_streams(&entries);
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
    let stream1 = make_stream(
        "abc123def456",
        Some("tmux/dev/session-1"),
        8_100_000,
        16_200_000,
        Some(recent),
    );
    db.insert_stream(&stream1).unwrap();
    db.add_tag("abc123def456", "acme-webapp").unwrap();
    db.add_tag("abc123def456", "urgent").unwrap();

    // Stream 2: lower total time, one tag
    let stream2 = make_stream(
        "def456ghi789",
        Some("tmux/dev/session-2"),
        2_700_000,
        4_800_000,
        Some(recent),
    );
    db.insert_stream(&stream2).unwrap();
    db.add_tag("def456ghi789", "internal").unwrap();

    // Stream 3: lowest time, no tags
    let stream3 = make_stream(
        "ghi789jkl012",
        Some("tmux/staging/session-1"),
        1_800_000,
        900_000,
        Some(recent),
    );
    db.insert_stream(&stream3).unwrap();

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

    let output = format_streams(&entries);
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
    let stream1 = make_stream(
        "abc123def456",
        Some("has-time"),
        3_600_000,
        1_800_000,
        Some(recent),
    );
    db.insert_stream(&stream1).unwrap();

    // Stream with zero time
    let stream2 = make_stream("def456ghi789", Some("zero-time"), 0, 0, Some(recent));
    db.insert_stream(&stream2).unwrap();

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
    let stream1 = make_stream(
        "recent123456",
        Some("recent-stream"),
        3_600_000,
        1_800_000,
        Some(recent),
    );
    db.insert_stream(&stream1).unwrap();

    // Stream from 10 days ago (should be excluded)
    let old = Utc.with_ymd_and_hms(2025, 1, 19, 12, 0, 0).unwrap();
    let stream2 = make_stream(
        "old123456789",
        Some("old-stream"),
        7_200_000,
        3_600_000,
        Some(old),
    );
    db.insert_stream(&stream2).unwrap();

    let entries = get_streams_for_display(&db, today).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id_short, "recent");
}

#[test]
fn test_streams_json_output() {
    let db = Database::open_in_memory().unwrap();
    let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
    let recent = Utc.with_ymd_and_hms(2025, 1, 28, 12, 0, 0).unwrap();

    let stream = make_stream(
        "abc123def456",
        Some("tmux/dev/session-1"),
        8_100_000,
        16_200_000,
        Some(recent),
    );
    db.insert_stream(&stream).unwrap();
    db.add_tag("abc123def456", "acme-webapp").unwrap();

    let entries = get_streams_for_display(&db, today).unwrap();
    let output = format_streams_json(&entries, today).unwrap();
    insta::with_settings!({snapshot_path => "../snapshots"}, {
        assert_snapshot!(output);
    });
}

#[test]
fn test_streams_no_last_event_at_excluded() {
    let db = Database::open_in_memory().unwrap();
    let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();

    // Stream without last_event_at
    let stream = make_stream(
        "abc123def456",
        Some("no-last-event"),
        3_600_000,
        1_800_000,
        None,
    );
    db.insert_stream(&stream).unwrap();

    let entries = get_streams_for_display(&db, today).unwrap();
    assert!(
        entries.is_empty(),
        "streams without last_event_at should be excluded"
    );
}

#[test]
fn test_streams_unnamed_display() {
    let db = Database::open_in_memory().unwrap();
    let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
    let recent = Utc.with_ymd_and_hms(2025, 1, 28, 12, 0, 0).unwrap();

    // Stream without a name
    let stream = make_stream("abc123def456", None, 3_600_000, 1_800_000, Some(recent));
    db.insert_stream(&stream).unwrap();

    let entries = get_streams_for_display(&db, today).unwrap();
    let output = format_streams(&entries);

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
    let stream = make_stream(
        "abc",
        Some("short-id-stream"),
        3_600_000,
        1_800_000,
        Some(recent),
    );
    db.insert_stream(&stream).unwrap();

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
    let stream = make_stream(
        "abc123def456",
        Some(long_name),
        3_600_000,
        1_800_000,
        Some(recent),
    );
    db.insert_stream(&stream).unwrap();

    let entries = get_streams_for_display(&db, today).unwrap();
    // Should not panic, and should produce valid output
    let output = format_streams(&entries);
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

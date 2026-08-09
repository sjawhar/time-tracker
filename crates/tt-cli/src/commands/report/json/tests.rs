//! Tests for the `--json` report shape.

use chrono::{TimeZone, Utc};
use insta::assert_snapshot;
use serde_json::Value;
use tt_core::session::{SessionSource, SessionType};

use super::super::test_support::{base_report, make_test_session, make_test_stream, tags};
use super::super::{PeriodType, ReportData, ReportStreamTime};
use super::format_report_json;

/// The stream most JSON tests report on: 2h direct, 1h15m delegated.
fn session_one() -> Vec<ReportStreamTime> {
    vec![make_test_stream(
        "abc123def456",
        "tmux/dev/session-1",
        7_200_000,
        4_500_000,
    )]
}

#[test]
fn test_report_json_output() {
    let data = ReportData {
        streams: session_one(),
        total_tracked_ms: 9_000_000, // 2h30m wall clock
        ..base_report(PeriodType::Week)
    };

    let output = format_report_json(&data).unwrap();
    assert_snapshot!(output);
}

#[test]
fn test_report_json_by_tag_aggregates() {
    let data = ReportData {
        streams: vec![
            make_test_stream("abc123def456", "tmux/dev/session-1", 3_600_000, 0),
            make_test_stream("def456ghi789", "tmux/dev/session-2", 1_800_000, 600_000),
        ],
        tags_by_stream: tags(&[("abc123def456", "dev"), ("def456ghi789", "ops")]),
        total_tracked_ms: 6_000_000, // 1h40m wall clock
        ..base_report(PeriodType::Week)
    };

    let output = format_report_json(&data).unwrap();
    assert_snapshot!(output);
}

#[test]
fn test_report_json_multitag_stream_duplicate() {
    let data = ReportData {
        streams: session_one(),
        tags_by_stream: tags(&[
            ("abc123def456", "development"),
            ("abc123def456", "time-tracker"),
        ]),
        total_tracked_ms: 9_000_000, // 2h30m wall clock
        ..base_report(PeriodType::Week)
    };

    let output = format_report_json(&data).unwrap();
    assert_snapshot!(output);
}

#[test]
fn test_report_json_tagged_and_untagged() {
    let data = ReportData {
        streams: vec![
            make_test_stream("abc123def456", "tmux/dev/session-1", 1_200_000, 0),
            make_test_stream("def456ghi789", "tmux/dev/session-2", 600_000, 300_000),
        ],
        tags_by_stream: tags(&[("abc123def456", "dev")]),
        total_tracked_ms: 2_100_000, // 35m wall clock
        ..base_report(PeriodType::Week)
    };

    let output = format_report_json(&data).unwrap();
    assert_snapshot!(output);
}

#[test]
fn test_report_json_with_agent_sessions_summary() {
    let long_prompt = "x".repeat(140);
    let data = ReportData {
        streams: session_one(),
        agent_sessions: vec![
            make_test_session(
                "session-1",
                SessionSource::Claude,
                SessionType::User,
                Utc.with_ymd_and_hms(2025, 1, 28, 10, 0, 0).unwrap(),
                Some(Utc.with_ymd_and_hms(2025, 1, 28, 10, 30, 0).unwrap()),
                Some(&long_prompt),
            ),
            make_test_session(
                "session-2",
                SessionSource::OpenCode,
                SessionType::Subagent,
                Utc.with_ymd_and_hms(2025, 1, 29, 9, 0, 0).unwrap(),
                None,
                Some("Short prompt"),
            ),
        ],
        total_tracked_ms: 9_000_000, // 2h30m wall clock
        ..base_report(PeriodType::Week)
    };

    let output = format_report_json(&data).unwrap();
    assert_snapshot!(output);
}

#[test]
fn test_report_json_top_sessions_sorted() {
    let base_start = Utc.with_ymd_and_hms(2025, 1, 28, 9, 0, 0).unwrap();
    // Six sessions of differing length; only the five longest are reported.
    let scripted = [
        (
            "session-a",
            SessionSource::Claude,
            SessionType::User,
            5,
            "short",
        ),
        (
            "session-b",
            SessionSource::Claude,
            SessionType::User,
            20,
            "longer",
        ),
        (
            "session-c",
            SessionSource::OpenCode,
            SessionType::Subagent,
            15,
            "mid",
        ),
        (
            "session-d",
            SessionSource::Claude,
            SessionType::Subagent,
            30,
            "longest",
        ),
        (
            "session-e",
            SessionSource::OpenCode,
            SessionType::User,
            25,
            "second",
        ),
        (
            "session-f",
            SessionSource::Claude,
            SessionType::User,
            10,
            "cutoff",
        ),
    ];
    let data = ReportData {
        streams: session_one(),
        agent_sessions: scripted
            .into_iter()
            .map(|(id, source, session_type, minutes, prompt)| {
                make_test_session(
                    id,
                    source,
                    session_type,
                    base_start,
                    Some(base_start + chrono::Duration::minutes(minutes)),
                    Some(prompt),
                )
            })
            .collect(),
        total_tracked_ms: 9_000_000, // 2h30m wall clock
        ..base_report(PeriodType::Week)
    };

    let output = format_report_json(&data).unwrap();
    assert_snapshot!(output);
}

#[test]
fn test_report_json_agent_session_counts_match_total() {
    let data = ReportData {
        streams: session_one(),
        agent_sessions: vec![
            make_test_session(
                "session-1",
                SessionSource::Claude,
                SessionType::User,
                Utc.with_ymd_and_hms(2025, 1, 28, 10, 0, 0).unwrap(),
                Some(Utc.with_ymd_and_hms(2025, 1, 28, 10, 30, 0).unwrap()),
                Some("One"),
            ),
            make_test_session(
                "session-2",
                SessionSource::OpenCode,
                SessionType::Subagent,
                Utc.with_ymd_and_hms(2025, 1, 28, 11, 0, 0).unwrap(),
                Some(Utc.with_ymd_and_hms(2025, 1, 28, 11, 20, 0).unwrap()),
                Some("Two"),
            ),
            make_test_session(
                "session-3",
                SessionSource::Claude,
                SessionType::Subagent,
                Utc.with_ymd_and_hms(2025, 1, 28, 12, 0, 0).unwrap(),
                Some(Utc.with_ymd_and_hms(2025, 1, 28, 12, 10, 0).unwrap()),
                Some("Three"),
            ),
        ],
        total_tracked_ms: 9_000_000, // 2h30m wall clock
        ..base_report(PeriodType::Week)
    };

    let output = format_report_json(&data).unwrap();
    let json: Value = serde_json::from_str(&output).unwrap();
    let total = json["agent_sessions"]["total"].as_u64().unwrap();
    let by_source_total: u64 = json["agent_sessions"]["by_source"]
        .as_object()
        .unwrap()
        .values()
        .map(|v| v.as_u64().unwrap())
        .sum();
    let by_type_total: u64 = json["agent_sessions"]["by_type"]
        .as_object()
        .unwrap()
        .values()
        .map(|v| v.as_u64().unwrap())
        .sum();
    assert_eq!(total, by_source_total);
    assert_eq!(total, by_type_total);
    assert_snapshot!(output);
}

#[test]
fn json_stream_order_does_not_depend_on_input_order() {
    // Given: three streams that tie on both time figures, supplied in opposite
    // orders. `data.streams` arrives in `allocate_time`'s HashMap order, which is
    // not reproducible between runs.
    let mut forward = vec![
        make_test_stream("aaa", "a", 0, 600_000),
        make_test_stream("bbb", "b", 0, 600_000),
        make_test_stream("ccc", "c", 0, 600_000),
    ];
    let mut reversed = forward.clone();
    reversed.reverse();

    // When: each ordering is rendered as JSON.
    let render = |streams: Vec<ReportStreamTime>| {
        format_report_json(&ReportData {
            streams,
            total_tracked_ms: 600_000,
            ..base_report(PeriodType::Week)
        })
        .unwrap()
    };
    forward.rotate_left(1);
    let from_forward = render(forward);
    let from_reversed = render(reversed);

    // Then: the JSON is byte-identical — ties are broken by stream id.
    assert_eq!(from_forward, from_reversed);
    let json: Value = serde_json::from_str(&from_forward).unwrap();
    let ids: Vec<&str> = json["streams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["aaa", "bbb", "ccc"]);
    assert_eq!(
        json["untagged"]["streams"].as_array().unwrap().len(),
        3,
        "untagged stream ids must also be ordered"
    );
}

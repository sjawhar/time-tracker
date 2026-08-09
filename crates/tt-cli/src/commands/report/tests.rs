//! Tests for report data generation and period event selection.

use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use tt_core::AllocationConfig;
use tt_core::allocate_time;
use tt_db::{Database, StoredEvent};

use super::test_support::make_agent_event;
use super::{Period, generate_report_data, generate_report_data_for_date, get_period_boundaries};

/// Mirrors how the report selects events for a half-open `[start, end)` period.
fn get_report_period_events(
    db: &Database,
    period_start: DateTime<Utc>,
    period_end: DateTime<Utc>,
) -> Result<Vec<StoredEvent>> {
    let exclusive_end = period_end - chrono::Duration::milliseconds(1);
    if exclusive_end < period_start {
        return Ok(Vec::new());
    }
    db.get_events_in_range(period_start, exclusive_end)
        .context("failed to get events in period")
}

fn empty_stream(id: &str, name: &str, created_at: DateTime<Utc>) -> tt_db::Stream {
    tt_db::Stream {
        id: id.to_string(),
        name: Some(name.to_string()),
        slug: None,
        description: None,
        color: None,
        created_at,
        updated_at: created_at,
        time_direct_ms: 0,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: false,
    }
}

#[test]
fn report_period_event_fetch_is_half_open_at_end_boundary() {
    // Given: one event inside a report period and one exactly at the period end.
    let db = Database::open_in_memory().unwrap();
    let start = Utc.with_ymd_and_hms(2026, 6, 22, 0, 0, 0).unwrap();
    let end = Utc.with_ymd_and_hms(2026, 6, 29, 0, 0, 0).unwrap();
    db.insert_stream(&empty_stream("stream-a", "Stream A", start))
        .unwrap();
    db.insert_stream(&empty_stream("stream-b", "Stream B", start))
        .unwrap();
    let inside = make_agent_event(
        "inside",
        end - chrono::Duration::milliseconds(1),
        tt_core::EventType::TmuxPaneFocus,
        "session",
        "stream-a",
        None,
    );
    let boundary = make_agent_event(
        "boundary",
        end,
        tt_core::EventType::TmuxPaneFocus,
        "session",
        "stream-b",
        None,
    );
    db.insert_events(&[inside, boundary]).unwrap();

    // When: report code fetches events for the half-open period [start, end).
    let events = get_report_period_events(&db, start, end).unwrap();

    // Then: the end-boundary event is excluded from the earlier period.
    let ids = events
        .iter()
        .map(|event| event.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["inside"]);
    let next_period_ids = get_report_period_events(&db, end, end + chrono::Duration::days(7))
        .unwrap()
        .iter()
        .map(|event| event.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(next_period_ids, vec!["boundary"]);
}

#[test]
fn test_zero_time_streams_excluded() {
    let db = Database::open_in_memory().unwrap();

    // Create a stream with zero time (using tt_db::Stream for database insertion)
    let now = Utc::now();
    let mut zero_stream = empty_stream("zero-stream", "empty", now);
    zero_stream.first_event_at = Some(now);
    zero_stream.last_event_at = Some(now);
    db.insert_stream(&zero_stream).unwrap();

    // Generate report - with no events, the allocation returns no time
    let data = generate_report_data(&db, Period::Week, now).unwrap();

    // Zero-time stream should be excluded (no events = no time allocated)
    assert!(
        data.streams.is_empty(),
        "zero-time streams should be excluded"
    );
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "test exercises the core algorithm directly"
)]
fn test_day_report_seeds_cross_boundary_agent_session_starts() {
    let db = Database::open_in_memory().unwrap();
    let reference_date = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
    let (period_start, period_end) = get_period_boundaries(Period::Day, reference_date);
    let session_id = "session-cross-boundary";
    let stream_id = "stream-cross-boundary";
    let stream_created_at = period_start - chrono::Duration::hours(2);

    db.insert_stream(&empty_stream(
        stream_id,
        "cross-boundary stream",
        stream_created_at,
    ))
    .unwrap();

    let session_start = period_start - chrono::Duration::hours(1);
    let first_tool_use = period_start + chrono::Duration::hours(9) + chrono::Duration::minutes(10);
    let second_tool_use = first_tool_use + chrono::Duration::minutes(30);
    let session_end = second_tool_use + chrono::Duration::minutes(20);
    let expected_delegated_ms = (session_end - first_tool_use).num_milliseconds();

    let start_event = make_agent_event(
        "session-start",
        session_start,
        tt_core::EventType::AgentSession,
        session_id,
        stream_id,
        Some("started"),
    );
    let first_tool_event = make_agent_event(
        "tool-use-1",
        first_tool_use,
        tt_core::EventType::AgentToolUse,
        session_id,
        stream_id,
        None,
    );
    let second_tool_event = make_agent_event(
        "tool-use-2",
        second_tool_use,
        tt_core::EventType::AgentToolUse,
        session_id,
        stream_id,
        None,
    );
    let end_event = make_agent_event(
        "session-end",
        session_end,
        tt_core::EventType::AgentSession,
        session_id,
        stream_id,
        Some("ended"),
    );

    for event in [
        &start_event,
        &first_tool_event,
        &second_tool_event,
        &end_event,
    ] {
        db.insert_event(event).unwrap();
    }

    let period_events = db.get_events_in_range(period_start, period_end).unwrap();
    let config = AllocationConfig::default();

    let without_seed = allocate_time(
        &period_events,
        &config,
        Some(period_end),
        &HashMap::new(),
        &HashMap::new(),
    );
    assert_eq!(without_seed.stream_times.len(), 0);

    let mut seeded_events = Vec::with_capacity(period_events.len() + 1);
    seeded_events.push(start_event);
    seeded_events.extend(period_events);
    let with_seed = allocate_time(
        &seeded_events,
        &config,
        Some(period_end),
        &HashMap::new(),
        &HashMap::new(),
    );
    assert_eq!(with_seed.stream_times.len(), 1);
    assert_eq!(with_seed.stream_times[0].stream_id, stream_id);
    assert_eq!(
        with_seed.stream_times[0].time_delegated_ms,
        expected_delegated_ms
    );

    let data = generate_report_data_for_date(
        &db,
        Period::Day,
        period_end + chrono::Duration::hours(1),
        reference_date,
        "Etc/UTC".to_string(),
    )
    .unwrap();

    assert_eq!(data.streams.len(), 1);
    assert_eq!(data.streams[0].id, stream_id);
    assert_eq!(data.streams[0].time_delegated_ms, expected_delegated_ms);
}

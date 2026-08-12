//! Recompute direct/delegated time for streams.
//!
//! Uses the attention allocation algorithm to calculate time based on
//! focus events and agent activity.

use anyhow::{Context, Result};
use chrono::Duration;
use tt_core::AllocationConfig;
use tt_db::{Database, allocate_for_period};

/// Zeroed totals for selected streams that produced no time at all.
///
/// `stream_times` comes from allocating over `events`, so a stream holding none never
/// appears in it and was therefore never written — it kept whatever totals it last had.
/// That is not a stale cache but a wrong one: releasing the cwd propagator's assignments
/// left **138 event-less streams still asserting 248h of direct and 1,633h of delegated
/// time** between them, `cybertasks: DPI sprint` alone claiming 28h direct and 203h
/// delegated while holding zero events. `tt recompute` is the only writer of those two
/// columns, so making them true is precisely its job.
///
/// Zeroing also clears `needs_recompute`, which such a stream could otherwise never shed:
/// it is marked when its events are released, and nothing afterwards can unmark it. That
/// left 45 rows permanently claiming they needed a refresh no walk would ever give them.
fn zeroed_times_for_streams_without_events(
    selected: &[tt_db::Stream],
    computed: &[tt_core::StreamTime],
) -> Vec<tt_core::StreamTime> {
    let with_time: std::collections::HashSet<&str> = computed
        .iter()
        .map(|time| time.stream_id.as_str())
        .collect();
    selected
        .iter()
        .filter(|stream| !with_time.contains(stream.id.as_str()))
        .map(|stream| tt_core::StreamTime {
            stream_id: stream.id.clone(),
            time_direct_ms: 0,
            time_delegated_ms: 0,
            focus_intervals: Vec::new(),
            delegated_intervals: Vec::new(),
        })
        .collect()
}

/// Run time recomputation for streams.
///
/// # Arguments
///
/// * `db` - Database connection
/// * `force` - If true, recompute all streams; otherwise only those needing recomputation
pub fn run(db: &Database, force: bool) -> Result<()> {
    // Get the list of streams to recompute
    let streams = if force {
        db.get_streams().context("failed to get streams")?
    } else {
        db.get_streams_needing_recompute()
            .context("failed to get streams needing recompute")?
    };

    if streams.is_empty() {
        println!("No streams to recompute.");
        return Ok(());
    }

    println!("Recomputing {} stream(s)...", streams.len());

    // Bounds and the split-session warning are aggregates. Loading every event to derive
    // them cost 8.9 GB of RSS on the live corpus, and `allocate_for_period` loads the
    // events it needs itself.
    let Some((earliest, latest)) = db
        .event_time_bounds()
        .context("failed to read event time bounds")?
    else {
        println!("No events to process.");
        return Ok(());
    };

    // Sessions split across streams undercount, so they are reported. Not fatal: only the
    // user can say which stream such a session belongs to.
    for (session_id, stream_ids) in db
        .sessions_spanning_multiple_streams()
        .context("failed to check for sessions split across streams")?
    {
        let shown = &session_id[..session_id.len().min(30)];
        eprintln!(
            "Warning: session {} has events in {} streams: {:?}",
            shown,
            stream_ids.len(),
            stream_ids,
        );
        eprintln!("  Use 'tt streams assign <stream-ref> --session {shown}' to settle it.");
    }

    let config = AllocationConfig::default();
    // `allocate_for_period` uses an exclusive end, so extend it past the final event.
    let result = allocate_for_period(
        db,
        earliest,
        latest + Duration::milliseconds(1),
        None,
        &config,
    )
    .context("failed to allocate time")?;

    tracing::debug!(
        stream_count = result.stream_times.len(),
        total_tracked_ms = result.total_tracked_ms,
        "allocation complete"
    );

    // Filter results to only streams we want to update
    let mut times_to_update: Vec<_> = if force {
        // Update all streams that have time computed
        result.stream_times
    } else {
        // Only update streams that were marked for recomputation
        let stream_ids_to_update: std::collections::HashSet<_> =
            streams.iter().map(|s| s.id.as_str()).collect();
        result
            .stream_times
            .into_iter()
            .filter(|t| stream_ids_to_update.contains(t.stream_id.as_str()))
            .collect()
    };

    let zeroed = zeroed_times_for_streams_without_events(&streams, &times_to_update);
    let zeroed_count = zeroed.len();
    times_to_update.extend(zeroed);

    if times_to_update.is_empty() {
        println!("No time data computed for the selected streams.");
        return Ok(());
    }

    // Update the database
    let updated = db
        .update_stream_times(&times_to_update)
        .context("failed to update stream times")?;

    if zeroed_count > 0 {
        println!(
            "Updated {updated} stream(s); {zeroed_count} of them hold no events and were zeroed."
        );
    } else {
        println!("Updated {updated} stream(s).");
    }

    // Print summary
    for time in &times_to_update {
        let direct_mins = time.time_direct_ms / 60_000;
        let delegated_mins = time.time_delegated_ms / 60_000;
        println!(
            "  {}: direct {}m, delegated {}m",
            time.stream_id, direct_mins, delegated_mins
        );
    }

    let total_mins = result.total_tracked_ms / 60_000;
    println!("\nTotal tracked: {total_mins}m");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use serde_json::json;
    use tt_db::StoredEvent;

    fn make_focus_event(
        id: &str,
        ts: chrono::DateTime<Utc>,
        stream_id: &str,
        cwd: &str,
    ) -> StoredEvent {
        StoredEvent {
            id: id.to_string(),
            timestamp: ts,
            event_type: tt_core::EventType::TmuxPaneFocus,
            source: "remote.tmux".to_string(),
            machine_id: None,
            schema_version: 1,
            pane_id: Some("%1".to_string()),
            tmux_session: None,
            window_index: None,
            git_project: None,
            git_workspace: None,
            status: None,
            idle_duration_ms: None,
            window_app_id: None,
            window_title: None,
            action: None,
            cwd: Some(cwd.to_string()),
            session_id: None,
            stream_id: Some(stream_id.to_string()),
            assignment_source: Some("inferred".to_string()),
            data: json!({}),
        }
    }

    fn make_agent_session_event(
        id: &str,
        ts: chrono::DateTime<Utc>,
        action: &str,
        session_id: &str,
        stream_id: &str,
    ) -> StoredEvent {
        StoredEvent {
            id: id.to_string(),
            timestamp: ts,
            event_type: tt_core::EventType::AgentSession,
            source: "remote.agent".to_string(),
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
            action: Some(action.to_string()),
            cwd: Some("/project".to_string()),
            session_id: Some(session_id.to_string()),
            stream_id: Some(stream_id.to_string()),
            assignment_source: Some("inferred".to_string()),
            data: json!({}),
        }
    }

    fn make_tool_use_event(
        id: &str,
        ts: chrono::DateTime<Utc>,
        session_id: &str,
        stream_id: &str,
    ) -> StoredEvent {
        StoredEvent {
            id: id.to_string(),
            timestamp: ts,
            event_type: tt_core::EventType::AgentToolUse,
            source: "remote.agent".to_string(),
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
            cwd: Some("/project".to_string()),
            session_id: Some(session_id.to_string()),
            stream_id: Some(stream_id.to_string()),
            assignment_source: Some("inferred".to_string()),
            data: json!({}),
        }
    }

    fn ts(minutes: i64) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0).unwrap() + chrono::Duration::minutes(minutes)
    }

    #[test]
    fn test_recompute_with_focus_and_agent() {
        let db = Database::open_in_memory().unwrap();

        // Create a stream
        let now = Utc::now();
        let stream = tt_db::Stream {
            id: "stream-1".to_string(),
            name: Some("test-project".to_string()),
            slug: None,
            description: None,
            color: None,
            created_at: now,
            updated_at: now,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: Some(ts(0)),
            last_event_at: Some(ts(30)),
            needs_recompute: true,
        };
        db.insert_stream(&stream).unwrap();

        // Insert events
        let events = vec![
            make_focus_event("e1", ts(0), "stream-1", "/project"),
            make_agent_session_event("e2", ts(0), "started", "sess1", "stream-1"),
            make_tool_use_event("e3", ts(5), "sess1", "stream-1"),
            make_agent_session_event("e4", ts(30), "ended", "sess1", "stream-1"),
        ];

        for event in &events {
            db.insert_event(event).unwrap();
            // Assign to stream (simulating inference already ran)
            db.assign_event_to_stream(&event.id, "stream-1", "inferred")
                .unwrap();
        }

        // Run recompute
        run(&db, false).unwrap();

        // Verify stream was updated
        let updated_stream = db.get_stream("stream-1").unwrap().unwrap();
        assert!(
            updated_stream.time_direct_ms > 0,
            "direct time should be > 0"
        );
        assert_eq!(
            updated_stream.time_delegated_ms,
            25 * 60 * 1000,
            "delegated time should be 25 minutes"
        );
        assert!(
            !updated_stream.needs_recompute,
            "needs_recompute should be cleared"
        );
    }

    #[test]
    fn test_recompute_no_streams_needing_recompute() {
        let db = Database::open_in_memory().unwrap();

        // Create a stream that doesn't need recomputation
        let now = Utc::now();
        let stream = tt_db::Stream {
            id: "stream-1".to_string(),
            name: Some("test-project".to_string()),
            slug: None,
            description: None,
            color: None,
            created_at: now,
            updated_at: now,
            time_direct_ms: 100,
            time_delegated_ms: 200,
            first_event_at: Some(ts(0)),
            last_event_at: Some(ts(30)),
            needs_recompute: false, // Not marked for recompute
        };
        db.insert_stream(&stream).unwrap();

        // Run recompute (not forced)
        run(&db, false).unwrap();

        // Stream should not be modified
        let unchanged_stream = db.get_stream("stream-1").unwrap().unwrap();
        assert_eq!(unchanged_stream.time_direct_ms, 100);
        assert_eq!(unchanged_stream.time_delegated_ms, 200);
    }

    #[test]
    fn test_recompute_force_all() {
        let db = Database::open_in_memory().unwrap();

        // Create a stream that doesn't need recomputation
        let now = Utc::now();
        let stream = tt_db::Stream {
            id: "stream-1".to_string(),
            name: Some("test-project".to_string()),
            slug: None,
            description: None,
            color: None,
            created_at: now,
            updated_at: now,
            time_direct_ms: 100,
            time_delegated_ms: 200,
            first_event_at: Some(ts(0)),
            last_event_at: Some(ts(30)),
            needs_recompute: false, // Not marked for recompute
        };
        db.insert_stream(&stream).unwrap();

        // Insert events
        let events = vec![
            make_focus_event("e1", ts(0), "stream-1", "/project"),
            make_agent_session_event("e2", ts(0), "started", "sess1", "stream-1"),
            make_tool_use_event("e3", ts(5), "sess1", "stream-1"),
            make_agent_session_event("e4", ts(30), "ended", "sess1", "stream-1"),
        ];

        for event in &events {
            db.insert_event(event).unwrap();
            db.assign_event_to_stream(&event.id, "stream-1", "inferred")
                .unwrap();
        }

        // Run recompute with force
        run(&db, true).unwrap();

        // Stream should be updated even though needs_recompute was false
        let updated_stream = db.get_stream("stream-1").unwrap().unwrap();
        assert!(updated_stream.time_direct_ms > 0);
        assert_eq!(updated_stream.time_delegated_ms, 25 * 60 * 1000);
    }

    fn zt_stream(id: &str) -> tt_db::Stream {
        tt_db::Stream {
            id: id.to_string(),
            name: Some(format!("stream {id}")),
            slug: None,
            description: None,
            color: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            time_direct_ms: 99_999,
            time_delegated_ms: 88_888,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: true,
        }
    }

    fn zt_computed(id: &str, direct_ms: i64) -> tt_core::StreamTime {
        tt_core::StreamTime {
            stream_id: id.to_string(),
            time_direct_ms: direct_ms,
            time_delegated_ms: 0,
            focus_intervals: Vec::new(),
            delegated_intervals: Vec::new(),
        }
    }

    #[test]
    fn streams_needing_recompute_with_no_events_are_left_untouched() {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&zt_stream("stream-1")).unwrap();

        run(&db, false).unwrap();

        let after = db.get_stream("stream-1").unwrap().unwrap();
        assert_eq!(
            after.time_direct_ms, 99_999,
            "no events means no allocation and no write"
        );
        assert!(after.needs_recompute);
    }

    #[test]
    fn a_selected_stream_with_no_events_is_zeroed_not_skipped() {
        // Skipping is what left 138 event-less streams asserting 248h of direct time they
        // no longer hold: allocation only yields rows for streams that have events, so one
        // whose events were released is never written and keeps its last totals forever.
        let selected = vec![zt_stream("has-events"), zt_stream("emptied")];
        let with_time = vec![zt_computed("has-events", 60_000)];

        let zeroed = zeroed_times_for_streams_without_events(&selected, &with_time);

        assert_eq!(zeroed.len(), 1, "only the event-less stream is zeroed");
        assert_eq!(zeroed[0].stream_id, "emptied");
        assert_eq!(zeroed[0].time_direct_ms, 0);
        assert_eq!(zeroed[0].time_delegated_ms, 0);
    }

    #[test]
    fn a_stream_that_produced_time_is_left_to_its_computed_totals() {
        let selected = vec![zt_stream("busy")];
        let with_time = vec![zt_computed("busy", 3_600_000)];

        let zeroed = zeroed_times_for_streams_without_events(&selected, &with_time);

        assert!(
            zeroed.is_empty(),
            "zeroing a stream that has time would erase the recomputation: {zeroed:?}"
        );
    }

    #[test]
    fn nothing_selected_yields_nothing_to_zero() {
        let zeroed = zeroed_times_for_streams_without_events(&[], &[zt_computed("x", 1)]);
        assert!(zeroed.is_empty());
    }
}

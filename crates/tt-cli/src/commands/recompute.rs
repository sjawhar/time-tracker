//! Recompute direct/delegated time for streams.
//!
//! Uses the attention allocation algorithm to calculate time based on
//! focus events and agent activity.

use anyhow::{Context, Result};
use tt_core::{AllocationConfig, allocate_time};
use tt_db::Database;

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

    // Get all events - we need all events to build the focus/agent timelines correctly
    // even if we're only updating specific streams
    let events = db.get_events(None, None).context("failed to get events")?;

    if events.is_empty() {
        println!("No events to process.");
        return Ok(());
    }

    tracing::debug!(event_count = events.len(), "loaded events for allocation");

    // Run the allocation algorithm
    let config = AllocationConfig::default();
    let result = allocate_time(&events, &config, None);

    tracing::debug!(
        stream_count = result.stream_times.len(),
        total_tracked_ms = result.total_tracked_ms,
        "allocation complete"
    );

    // Filter results to only streams we want to update
    let times_to_update: Vec<_> = if force {
        // Update all streams that have time computed
        result.stream_times.clone()
    } else {
        // Only update streams that were marked for recomputation
        let stream_ids_to_update: std::collections::HashSet<_> =
            streams.iter().map(|s| s.id.as_str()).collect();
        result
            .stream_times
            .iter()
            .filter(|t| stream_ids_to_update.contains(t.stream_id.as_str()))
            .cloned()
            .collect()
    };

    if times_to_update.is_empty() {
        println!("No time data computed for the selected streams.");
        return Ok(());
    }

    // Update the database
    let updated = db
        .update_stream_times(&times_to_update)
        .context("failed to update stream times")?;

    println!("Updated {} stream(s).", updated);

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
    println!("\nTotal tracked: {}m", total_mins);

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
            event_type: "tmux_pane_focus".to_string(),
            source: "remote.tmux".to_string(),
            schema_version: 1,
            data: json!({"pane_id": "%1", "cwd": cwd}),
            cwd: Some(cwd.to_string()),
            session_id: None,
            stream_id: Some(stream_id.to_string()),
            assignment_source: Some("inferred".to_string()),
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
            event_type: "agent_session".to_string(),
            source: "remote.agent".to_string(),
            schema_version: 1,
            data: json!({"action": action, "agent": "claude-code"}),
            cwd: Some("/project".to_string()),
            session_id: Some(session_id.to_string()),
            stream_id: Some(stream_id.to_string()),
            assignment_source: Some("inferred".to_string()),
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
            event_type: "agent_tool_use".to_string(),
            source: "remote.agent".to_string(),
            schema_version: 1,
            data: json!({"tool": "Edit"}),
            cwd: Some("/project".to_string()),
            session_id: Some(session_id.to_string()),
            stream_id: Some(stream_id.to_string()),
            assignment_source: Some("inferred".to_string()),
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
}

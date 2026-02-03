//! Stream inference command.
//!
//! Runs the stream inference algorithm on events that don't have stream assignments.

use anyhow::{Context, Result};
use tt_core::{InferenceConfig, infer_streams};
use tt_db::{Database, Stream};

/// Run stream inference on unassigned events.
///
/// If `force` is true, clears all inferred assignments first.
pub fn run(db: &Database, force: bool) -> Result<()> {
    // If force, clear existing inferred assignments and orphaned streams
    if force {
        let cleared = db
            .clear_inferred_assignments()
            .context("failed to clear inferred assignments")?;
        let deleted = db
            .delete_orphaned_streams()
            .context("failed to delete orphaned streams")?;
        println!("Cleared {cleared} inferred assignments, deleted {deleted} orphaned streams");
    }

    // Get only unassigned events for efficiency
    // The inference algorithm will skip events with user assignments or existing stream_ids
    let events = db
        .get_events_without_stream()
        .context("failed to fetch unassigned events")?;

    if events.is_empty() {
        println!("No events to process");
        return Ok(());
    }

    // Run inference
    let config = InferenceConfig::default();
    let result = infer_streams(&events, &config);

    if result.streams.is_empty() {
        println!("No new streams created (all events already assigned)");
        return Ok(());
    }

    // Insert new streams
    for inferred in &result.streams {
        let stream = Stream {
            id: inferred.id.as_str().to_string(),
            name: Some(inferred.name.clone()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: Some(inferred.first_event_at),
            last_event_at: Some(inferred.last_event_at),
            needs_recompute: true, // Time will be computed in a later step
        };
        db.insert_stream(&stream)
            .context("failed to insert stream")?;
    }

    // Assign events to streams
    let assignments: Vec<(String, String)> = result
        .assignments
        .iter()
        .map(|a| (a.event_id.clone(), a.stream_id.as_str().to_string()))
        .collect();

    let assigned_count = db
        .assign_events_to_stream(&assignments, "inferred")
        .context("failed to assign events to streams")?;

    println!(
        "Created {} stream(s), assigned {} event(s)",
        result.streams.len(),
        assigned_count
    );

    // Print summary of created streams using pre-computed counts
    let mut event_counts: std::collections::HashMap<&tt_core::StreamId, usize> =
        std::collections::HashMap::new();
    for assignment in &result.assignments {
        *event_counts.entry(&assignment.stream_id).or_default() += 1;
    }

    for stream in &result.streams {
        let event_count = event_counts.get(&stream.id).copied().unwrap_or(0);
        println!("  {} ({} events)", stream.name, event_count);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use serde_json::json;
    use tt_db::StoredEvent;

    fn make_event(id: &str, timestamp_mins: i64, cwd: Option<&str>) -> StoredEvent {
        let ts = Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0).unwrap()
            + chrono::Duration::minutes(timestamp_mins);
        StoredEvent {
            id: id.to_string(),
            timestamp: ts,
            event_type: "tmux_pane_focus".to_string(),
            source: "remote.tmux".to_string(),
            schema_version: 1,
            data: json!({
                "pane_id": "%1",
                "session_name": "dev"
            }),
            cwd: cwd.map(String::from),
            session_id: None,
            stream_id: None,
            assignment_source: None,
        }
    }

    #[test]
    fn test_infer_creates_streams() {
        let db = Database::open_in_memory().unwrap();

        // Insert events in two different directories
        db.insert_event(&make_event("e1", 0, Some("/project-a")))
            .unwrap();
        db.insert_event(&make_event("e2", 5, Some("/project-a")))
            .unwrap();
        db.insert_event(&make_event("e3", 10, Some("/project-b")))
            .unwrap();

        // Run inference
        run(&db, false).unwrap();

        // Verify streams created
        let streams = db.get_streams().unwrap();
        assert_eq!(streams.len(), 2);

        // Verify events assigned
        let unassigned = db.get_events_without_stream().unwrap();
        assert!(unassigned.is_empty());
    }

    #[test]
    fn test_infer_force_clears_and_reinfers() {
        let db = Database::open_in_memory().unwrap();

        // Insert events
        db.insert_event(&make_event("e1", 0, Some("/project-a")))
            .unwrap();
        db.insert_event(&make_event("e2", 5, Some("/project-a")))
            .unwrap();

        // Run inference
        run(&db, false).unwrap();

        let streams_before = db.get_streams().unwrap();
        assert_eq!(streams_before.len(), 1);

        // Run inference with force - should clear and recreate
        run(&db, true).unwrap();

        // Should have same number of streams (old ones deleted, new ones created)
        let streams_after = db.get_streams().unwrap();
        assert_eq!(streams_after.len(), 1);

        // Stream IDs should be different (new UUIDs)
        assert_ne!(streams_before[0].id, streams_after[0].id);
    }

    #[test]
    fn test_infer_empty_database() {
        let db = Database::open_in_memory().unwrap();
        run(&db, false).unwrap(); // Should not error
    }
}

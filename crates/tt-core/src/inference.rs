//! Stream inference algorithm.
//!
//! Clusters events into coherent work units (streams) based on:
//! 1. Working directory (`cwd`) - Events in the same directory belong to the same stream
//! 2. Temporal proximity - Events separated by >30min gap start a new stream

use chrono::{DateTime, Utc};

use crate::types::{Confidence, StreamId};

/// Configuration for stream inference.
#[derive(Debug, Clone)]
pub struct InferenceConfig {
    /// Gap threshold in milliseconds. Events separated by more than this
    /// start a new stream. Default: 1,800,000 (30 min).
    pub gap_threshold_ms: i64,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            gap_threshold_ms: 1_800_000, // 30 minutes
        }
    }
}

/// An event suitable for inference.
///
/// This trait allows inference to work with different event representations
/// (e.g., `StoredEvent` from tt-db, or test fixtures).
pub trait InferableEvent {
    /// Returns the event's unique ID.
    fn event_id(&self) -> &str;

    /// Returns the event's timestamp.
    fn timestamp(&self) -> DateTime<Utc>;

    /// Returns the event's working directory, if any.
    fn cwd(&self) -> Option<&str>;

    /// Returns the assignment source if already assigned.
    /// Returns `Some("user")` for user assignments, `Some("inferred")` for inferred,
    /// or `None` if not assigned.
    fn assignment_source(&self) -> Option<&str>;

    /// Returns the stream ID if already assigned.
    fn stream_id(&self) -> Option<&str>;

    /// Returns the jj project name, if available.
    ///
    /// This is preferred over the directory basename for stream naming
    /// since it provides a more meaningful project identifier.
    fn jj_project(&self) -> Option<&str>;
}

/// A stream assignment produced by inference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamAssignment {
    /// The event ID being assigned.
    pub event_id: String,

    /// The stream ID to assign to.
    pub stream_id: StreamId,
}

/// A stream produced by inference.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InferredStream {
    /// Unique identifier (UUID).
    pub id: StreamId,

    /// Human-readable name.
    pub name: String,

    /// Session IDs that belong to this stream (for LLM inference).
    #[serde(default)]
    pub session_ids: Vec<String>,

    /// Confidence score for human review. Default is maximum (1.0) for directory-based inference.
    #[serde(default)]
    pub confidence: Confidence,

    /// Timestamp of the first event in this stream.
    pub first_event_at: DateTime<Utc>,

    /// Timestamp of the last event in this stream.
    pub last_event_at: DateTime<Utc>,
}

/// Result of stream inference.
#[derive(Debug)]
pub struct InferenceResult {
    /// New streams created by inference.
    pub streams: Vec<InferredStream>,

    /// Event-to-stream assignments.
    pub assignments: Vec<StreamAssignment>,
}

/// Infer stream assignments for a set of events.
///
/// # Algorithm
///
/// 1. Filter out events that already have user assignments (preserved)
/// 2. Filter out events that already have stream assignments (unless re-inferring)
/// 3. Normalize all cwd paths (trailing slash removal)
/// 4. Group events by normalized cwd (including null-cwd group)
/// 5. Within each cwd group, sort by timestamp
/// 6. Start a new stream when gap > threshold
/// 7. Return (streams, assignments)
///
/// # Arguments
///
/// * `events` - Events to process (must implement `InferableEvent`)
/// * `config` - Inference configuration
///
/// # Returns
///
/// A tuple of (new streams to create, event assignments to make).
pub fn infer_streams<E: InferableEvent>(events: &[E], config: &InferenceConfig) -> InferenceResult {
    use std::collections::HashMap;

    // Group events by normalized cwd
    // Key: normalized cwd (empty string for null-cwd events)
    let mut groups: HashMap<String, Vec<&E>> = HashMap::new();

    for event in events {
        // Skip events with user assignments
        if event.assignment_source() == Some("user") {
            continue;
        }

        // Skip events that already have stream assignments
        if event.stream_id().is_some() {
            continue;
        }

        let key = normalize_path(event.cwd());
        groups.entry(key).or_default().push(event);
    }

    let mut streams = Vec::new();
    let mut assignments = Vec::new();

    // Track name collisions for disambiguation
    let mut name_counts: HashMap<String, u32> = HashMap::new();

    // Helper closure to emit a stream and its assignments
    let emit_stream = |cwd_key: &str,
                       events: &[&E],
                       name_counts: &mut HashMap<String, u32>,
                       streams: &mut Vec<InferredStream>,
                       assignments: &mut Vec<StreamAssignment>| {
        let stream = create_stream(cwd_key, events, name_counts);
        let stream_id = stream.id.clone();
        streams.push(stream);

        for e in events {
            assignments.push(StreamAssignment {
                event_id: e.event_id().to_string(),
                stream_id: stream_id.clone(),
            });
        }
    };

    // Sort groups by key for deterministic output (HashMap iteration order is non-deterministic)
    let mut sorted_groups: Vec<_> = groups.into_iter().collect();
    sorted_groups.sort_by(|(a, _), (b, _)| a.cmp(b));

    for (cwd_key, mut group_events) in sorted_groups {
        // Sort by timestamp
        group_events.sort_by_key(|e| e.timestamp());

        if group_events.is_empty() {
            continue;
        }

        // Split into streams based on temporal gaps
        let mut current_stream_events: Vec<&E> = Vec::new();
        let mut last_timestamp: Option<DateTime<Utc>> = None;

        for event in group_events {
            let should_start_new_stream = last_timestamp.is_none_or(|last| {
                let gap_ms = (event.timestamp() - last).num_milliseconds();
                gap_ms > config.gap_threshold_ms
            });

            if should_start_new_stream && !current_stream_events.is_empty() {
                emit_stream(
                    &cwd_key,
                    &current_stream_events,
                    &mut name_counts,
                    &mut streams,
                    &mut assignments,
                );
                current_stream_events.clear();
            }

            current_stream_events.push(event);
            last_timestamp = Some(event.timestamp());
        }

        // Emit final stream
        if !current_stream_events.is_empty() {
            emit_stream(
                &cwd_key,
                &current_stream_events,
                &mut name_counts,
                &mut streams,
                &mut assignments,
            );
        }
    }

    InferenceResult {
        streams,
        assignments,
    }
}

/// Normalize a path for grouping.
///
/// - Removes trailing slash
/// - Returns empty string for None
fn normalize_path(path: Option<&str>) -> String {
    path.map_or_else(String::new, |p| p.trim_end_matches('/').to_string())
}

/// Generate a stream name from events.
///
/// - Prefers `jj_project` from the first event if available
/// - Falls back to directory basename (e.g., `/home/user/project` → "project")
/// - For null-cwd, returns "Uncategorized"
/// - Handles collisions by tracking counts
fn generate_stream_name<E: InferableEvent>(
    events: &[&E],
    cwd_key: &str,
    name_counts: &mut std::collections::HashMap<String, u32>,
) -> String {
    // Try jj_project from first event
    let base_name = events.first().and_then(|e| e.jj_project()).map_or_else(
        || {
            // Fallback to directory basename
            if cwd_key.is_empty() {
                "Uncategorized".to_string()
            } else {
                std::path::Path::new(cwd_key)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            }
        },
        String::from,
    );

    let count = name_counts.entry(base_name.clone()).or_insert(0);
    *count += 1;

    if *count == 1 {
        base_name
    } else {
        format!("{base_name} ({count})")
    }
}

/// Create a stream from a group of events.
///
/// # Panics
///
/// Panics if `events` is empty.
fn create_stream<E: InferableEvent>(
    cwd_key: &str,
    events: &[&E],
    name_counts: &mut std::collections::HashMap<String, u32>,
) -> InferredStream {
    assert!(!events.is_empty(), "create_stream called with empty events");
    let name = generate_stream_name(events, cwd_key, name_counts);
    let id =
        StreamId::new(uuid::Uuid::new_v4().to_string()).expect("UUID v4 string is never empty");

    // SAFETY: events is guaranteed non-empty by the assert above
    let first_event_at = events.iter().map(|e| e.timestamp()).min().unwrap();
    let last_event_at = events.iter().map(|e| e.timestamp()).max().unwrap();

    InferredStream {
        id,
        name,
        session_ids: Vec::new(),
        confidence: Confidence::MAX,
        first_event_at,
        last_event_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test event implementation for unit tests.
    struct TestEvent {
        id: String,
        timestamp: DateTime<Utc>,
        cwd: Option<String>,
        assignment_source: Option<String>,
        stream_id: Option<String>,
        jj_project: Option<String>,
    }

    impl TestEvent {
        fn new(id: &str, timestamp: DateTime<Utc>, cwd: Option<&str>) -> Self {
            Self {
                id: id.to_string(),
                timestamp,
                cwd: cwd.map(String::from),
                assignment_source: None,
                stream_id: None,
                jj_project: None,
            }
        }

        fn with_user_assignment(mut self) -> Self {
            self.assignment_source = Some("user".to_string());
            self.stream_id = Some("user-stream".to_string());
            self
        }

        fn with_inferred_assignment(mut self, stream_id: &str) -> Self {
            self.assignment_source = Some("inferred".to_string());
            self.stream_id = Some(stream_id.to_string());
            self
        }

        fn with_jj_project(mut self, project: &str) -> Self {
            self.jj_project = Some(project.to_string());
            self
        }
    }

    impl InferableEvent for TestEvent {
        fn event_id(&self) -> &str {
            &self.id
        }

        fn timestamp(&self) -> DateTime<Utc> {
            self.timestamp
        }

        fn cwd(&self) -> Option<&str> {
            self.cwd.as_deref()
        }

        fn assignment_source(&self) -> Option<&str> {
            self.assignment_source.as_deref()
        }

        fn stream_id(&self) -> Option<&str> {
            self.stream_id.as_deref()
        }

        fn jj_project(&self) -> Option<&str> {
            self.jj_project.as_deref()
        }
    }

    fn ts(minutes: i64) -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0).unwrap() + chrono::Duration::minutes(minutes)
    }

    #[test]
    fn test_single_directory_continuous_work() {
        // 5 events in /project-a with 5min gaps
        let events = vec![
            TestEvent::new("e1", ts(0), Some("/project-a")),
            TestEvent::new("e2", ts(5), Some("/project-a")),
            TestEvent::new("e3", ts(10), Some("/project-a")),
            TestEvent::new("e4", ts(15), Some("/project-a")),
            TestEvent::new("e5", ts(20), Some("/project-a")),
        ];

        let config = InferenceConfig::default();
        let result = infer_streams(&events, &config);

        // Expect: 1 stream, all events assigned
        assert_eq!(result.streams.len(), 1);
        assert_eq!(result.assignments.len(), 5);
        assert_eq!(result.streams[0].name, "project-a");
    }

    #[test]
    fn test_gap_creates_new_stream() {
        // Events at t=0, t=5m, t=60m (31min gap after t=5m)
        let events = vec![
            TestEvent::new("e1", ts(0), Some("/project-a")),
            TestEvent::new("e2", ts(5), Some("/project-a")),
            TestEvent::new("e3", ts(60), Some("/project-a")), // 55min gap
        ];

        let config = InferenceConfig::default(); // 30min threshold
        let result = infer_streams(&events, &config);

        // Expect: 2 streams
        assert_eq!(result.streams.len(), 2);
        assert_eq!(result.assignments.len(), 3);

        // First stream: e1, e2
        let stream1_id = &result.streams[0].id;
        let stream1_count = result
            .assignments
            .iter()
            .filter(|a| &a.stream_id == stream1_id)
            .count();
        assert_eq!(stream1_count, 2);

        // Second stream: e3
        let stream2_id = &result.streams[1].id;
        let stream2_count = result
            .assignments
            .iter()
            .filter(|a| &a.stream_id == stream2_id)
            .count();
        assert_eq!(stream2_count, 1);
    }

    #[test]
    fn test_multiple_directories() {
        // Events in /project-a and /project-b interleaved
        let events = vec![
            TestEvent::new("e1", ts(0), Some("/project-a")),
            TestEvent::new("e2", ts(5), Some("/project-b")),
            TestEvent::new("e3", ts(10), Some("/project-a")),
            TestEvent::new("e4", ts(15), Some("/project-b")),
        ];

        let config = InferenceConfig::default();
        let result = infer_streams(&events, &config);

        // Expect: 2 streams (one per directory)
        assert_eq!(result.streams.len(), 2);
        assert_eq!(result.assignments.len(), 4);

        // Each stream should have 2 events
        for stream in &result.streams {
            let count = result
                .assignments
                .iter()
                .filter(|a| a.stream_id == stream.id)
                .count();
            assert_eq!(count, 2);
        }
    }

    #[test]
    fn test_null_cwd_events_with_gaps() {
        // Events without cwd at t=0, t=5m, t=60m
        let events = vec![
            TestEvent::new("e1", ts(0), None),
            TestEvent::new("e2", ts(5), None),
            TestEvent::new("e3", ts(60), None), // 55min gap
        ];

        let config = InferenceConfig::default();
        let result = infer_streams(&events, &config);

        // Expect: 2 "Uncategorized" streams (split by gap)
        assert_eq!(result.streams.len(), 2);

        // Names should be "Uncategorized" and "Uncategorized (2)"
        let names: Vec<_> = result.streams.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Uncategorized"));
        assert!(names.contains(&"Uncategorized (2)"));
    }

    #[test]
    fn test_user_corrections_preserved() {
        // Events with user assignment should not be reassigned
        let events = vec![
            TestEvent::new("e1", ts(0), Some("/project-a")),
            TestEvent::new("e2", ts(5), Some("/project-a")).with_user_assignment(),
            TestEvent::new("e3", ts(10), Some("/project-a")),
        ];

        let config = InferenceConfig::default();
        let result = infer_streams(&events, &config);

        // Expect: 1 stream with only e1 and e3 (e2 has user assignment)
        assert_eq!(result.streams.len(), 1);
        assert_eq!(result.assignments.len(), 2);

        let assigned_ids: Vec<_> = result
            .assignments
            .iter()
            .map(|a| a.event_id.as_str())
            .collect();
        assert!(assigned_ids.contains(&"e1"));
        assert!(!assigned_ids.contains(&"e2")); // User assignment preserved
        assert!(assigned_ids.contains(&"e3"));
    }

    #[test]
    fn test_path_normalization() {
        // Events in /project-a/ and /project-a (trailing slash difference)
        let events = vec![
            TestEvent::new("e1", ts(0), Some("/project-a/")),
            TestEvent::new("e2", ts(5), Some("/project-a")),
        ];

        let config = InferenceConfig::default();
        let result = infer_streams(&events, &config);

        // Expect: Same stream
        assert_eq!(result.streams.len(), 1);
        assert_eq!(result.assignments.len(), 2);
    }

    #[test]
    fn test_stream_name_collisions() {
        // Events in /work/a/project and /work/b/project
        let events = vec![
            TestEvent::new("e1", ts(0), Some("/work/a/project")),
            TestEvent::new("e2", ts(5), Some("/work/b/project")),
        ];

        let config = InferenceConfig::default();
        let result = infer_streams(&events, &config);

        // Expect: 2 streams with distinct names
        assert_eq!(result.streams.len(), 2);

        let names: Vec<_> = result.streams.iter().map(|s| s.name.as_str()).collect();
        // Names should be "project" and "project (2)"
        assert!(names.contains(&"project"));
        assert!(names.contains(&"project (2)"));
    }

    #[test]
    fn test_already_assigned_events_skipped() {
        // Events with existing stream_id (inferred) are skipped
        let events = vec![
            TestEvent::new("e1", ts(0), Some("/project-a")),
            TestEvent::new("e2", ts(5), Some("/project-a"))
                .with_inferred_assignment("existing-stream"),
            TestEvent::new("e3", ts(10), Some("/project-a")),
        ];

        let config = InferenceConfig::default();
        let result = infer_streams(&events, &config);

        // Expect: 1 stream with only e1 and e3 (e2 already assigned)
        assert_eq!(result.streams.len(), 1);
        assert_eq!(result.assignments.len(), 2);

        let assigned_ids: Vec<_> = result
            .assignments
            .iter()
            .map(|a| a.event_id.as_str())
            .collect();
        assert!(assigned_ids.contains(&"e1"));
        assert!(!assigned_ids.contains(&"e2")); // Already assigned
        assert!(assigned_ids.contains(&"e3"));
    }

    #[test]
    fn test_empty_events() {
        let events: Vec<TestEvent> = vec![];
        let config = InferenceConfig::default();
        let result = infer_streams(&events, &config);

        assert!(result.streams.is_empty());
        assert!(result.assignments.is_empty());
    }

    #[test]
    fn test_stream_timestamps() {
        let events = vec![
            TestEvent::new("e1", ts(0), Some("/project-a")),
            TestEvent::new("e2", ts(5), Some("/project-a")),
            TestEvent::new("e3", ts(10), Some("/project-a")),
        ];

        let config = InferenceConfig::default();
        let result = infer_streams(&events, &config);

        assert_eq!(result.streams.len(), 1);
        assert_eq!(result.streams[0].first_event_at, ts(0));
        assert_eq!(result.streams[0].last_event_at, ts(10));
    }

    #[test]
    fn test_jj_project_preferred_over_directory() {
        // Events in /home/user/time-tracker/default with jj_project="time-tracker"
        // Should use "time-tracker" as stream name, not "default"
        let events = vec![
            TestEvent::new("e1", ts(0), Some("/home/user/time-tracker/default"))
                .with_jj_project("time-tracker"),
            TestEvent::new("e2", ts(5), Some("/home/user/time-tracker/default"))
                .with_jj_project("time-tracker"),
        ];

        let config = InferenceConfig::default();
        let result = infer_streams(&events, &config);

        assert_eq!(result.streams.len(), 1);
        assert_eq!(result.streams[0].name, "time-tracker");
    }

    #[test]
    fn test_fallback_to_directory_without_jj_project() {
        // Events without jj_project should fall back to directory basename
        let events = vec![
            TestEvent::new("e1", ts(0), Some("/home/user/my-project")),
            TestEvent::new("e2", ts(5), Some("/home/user/my-project")),
        ];

        let config = InferenceConfig::default();
        let result = infer_streams(&events, &config);

        assert_eq!(result.streams.len(), 1);
        assert_eq!(result.streams[0].name, "my-project");
    }

    #[test]
    fn test_jj_project_same_name_different_directories() {
        // Two different workspaces of the same project should have same jj_project
        // but different cwd paths - they should be in different streams (by cwd)
        // but both named "time-tracker"
        let events = vec![
            TestEvent::new("e1", ts(0), Some("/home/user/time-tracker/default"))
                .with_jj_project("time-tracker"),
            TestEvent::new("e2", ts(5), Some("/home/user/time-tracker/feature"))
                .with_jj_project("time-tracker"),
        ];

        let config = InferenceConfig::default();
        let result = infer_streams(&events, &config);

        // Two streams (different cwd paths)
        assert_eq!(result.streams.len(), 2);

        // Both named "time-tracker" but with collision disambiguation
        let names: Vec<_> = result.streams.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"time-tracker"));
        assert!(names.contains(&"time-tracker (2)"));
    }

    #[test]
    fn test_mixed_jj_project_and_no_jj_project() {
        // Some events have jj_project, some don't
        let events = vec![
            TestEvent::new("e1", ts(0), Some("/home/user/project-a"))
                .with_jj_project("my-jj-project"),
            TestEvent::new("e2", ts(5), Some("/home/user/project-b")),
        ];

        let config = InferenceConfig::default();
        let result = infer_streams(&events, &config);

        assert_eq!(result.streams.len(), 2);

        let stream_a = result.streams.iter().find(|s| {
            result
                .assignments
                .iter()
                .any(|a| a.event_id == "e1" && a.stream_id == s.id)
        });
        let stream_b = result.streams.iter().find(|s| {
            result
                .assignments
                .iter()
                .any(|a| a.event_id == "e2" && a.stream_id == s.id)
        });

        assert_eq!(stream_a.unwrap().name, "my-jj-project");
        assert_eq!(stream_b.unwrap().name, "project-b");
    }
}

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use tt_core::{EventType, SessionType, StreamTime};

use crate::{Database, DbError, Stream, allocate_for_period, timeline_gaps::idle_gaps_for_events};

pub use tt_core::Interval;

/// A half-open time range for timeline data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TimelineWindow {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TimelineData {
    pub window: TimelineWindow,
    pub streams_active: Vec<TimelineStream>,
    pub idle_gaps: Vec<crate::IdleGap>,
    pub db_version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TimelineStream {
    pub stream: Stream,
    pub focus_intervals: Vec<Interval>,
    pub delegated_intervals: Vec<Interval>,
    pub events: Vec<TimelinePoint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelinePointKind {
    UserMessage,
    SubagentStart,
    SessionStart,
    SessionEnd,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TimelinePoint {
    pub timestamp: DateTime<Utc>,
    pub kind: TimelinePointKind,
    pub session_id: String,
    pub todo_linked: Option<bool>,
}

/// Assembles the activity timeline for a half-open window.
pub fn timeline_for_window(
    db: &Database,
    window: TimelineWindow,
    config: &tt_core::AllocationConfig,
    idle_threshold: Duration,
) -> Result<TimelineData, DbError> {
    let allocation = allocate_for_period(db, window.start, window.end, Some(window.end), config)?;
    let (events, session_types) = if window.end > window.start {
        let inclusive_end = window.end - Duration::milliseconds(1);
        let events = db.get_events_in_range(window.start, inclusive_end)?;
        let session_types = db
            .agent_sessions_in_range(window.start, inclusive_end)?
            .into_iter()
            .map(|session| (session.session_id, session.session_type))
            .collect();
        (events, session_types)
    } else {
        (Vec::new(), HashMap::new())
    };
    let mut active_stream_ids = HashSet::new();
    let mut points_by_stream = HashMap::new();
    for event in &events {
        let Some(stream_id) = event.stream_id.as_ref() else {
            continue;
        };
        active_stream_ids.insert(stream_id.as_str());
        if let Some(point) = timeline_point(event, &session_types) {
            points_by_stream
                .entry(stream_id.as_str())
                .or_insert_with(Vec::new)
                .push(point);
        }
    }
    let mut times_by_stream = allocation
        .stream_times
        .into_iter()
        .map(|time| (time.stream_id.clone(), time))
        .collect::<HashMap<_, _>>();
    let streams_active = db
        .get_streams()?
        .into_iter()
        .filter_map(|stream| {
            let time = times_by_stream.remove(&stream.id);
            let events = points_by_stream
                .remove(stream.id.as_str())
                .unwrap_or_default();
            if active_stream_ids.contains(stream.id.as_str()) || time.is_some() {
                let (focus_intervals, delegated_intervals) = intervals_for(time);
                Some(TimelineStream {
                    stream,
                    focus_intervals,
                    delegated_intervals,
                    events,
                })
            } else {
                None
            }
        })
        .collect();
    Ok(TimelineData {
        window,
        streams_active,
        idle_gaps: idle_gaps_for_events(window, &events, idle_threshold),
        db_version: db.get_db_version()?,
    })
}

fn timeline_point(
    event: &crate::StoredEvent,
    session_types: &HashMap<String, SessionType>,
) -> Option<TimelinePoint> {
    let session_id = event.session_id.clone()?;
    let (kind, todo_linked) = match event.event_type {
        EventType::UserMessage => (TimelinePointKind::UserMessage, None),
        EventType::AgentSession => match event.action.as_deref() {
            Some("started") if session_types.get(&session_id) == Some(&SessionType::Subagent) => {
                (TimelinePointKind::SubagentStart, None)
            }
            Some("started") => (
                TimelinePointKind::SessionStart,
                Some(event.assignment_source.as_deref() == Some("todo_link")),
            ),
            Some("ended") => (
                TimelinePointKind::SessionEnd,
                Some(event.assignment_source.as_deref() == Some("todo_link")),
            ),
            _ => return None,
        },
        _ => return None,
    };
    Some(TimelinePoint {
        timestamp: event.timestamp,
        kind,
        session_id,
        todo_linked,
    })
}

fn intervals_for(time: Option<StreamTime>) -> (Vec<Interval>, Vec<Interval>) {
    time.map_or_else(
        || (Vec::new(), Vec::new()),
        |time| (time.focus_intervals, time.delegated_intervals),
    )
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Duration, TimeZone, Utc};
    use serde_json::json;
    use tt_core::{AgentSession, EventType, SessionSource, SessionType};

    use super::{Interval, TimelinePointKind, TimelineWindow, timeline_for_window};
    use crate::{Database, StoredEvent, Stream};

    fn timestamp(minute: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0).unwrap() + Duration::minutes(minute)
    }

    fn insert_stream(db: &Database, id: &str) {
        let now = timestamp(0);
        db.insert_stream(&Stream {
            id: id.to_string(),
            name: Some(id.to_string()),
            slug: None,
            description: None,
            color: None,
            created_at: now,
            updated_at: now,
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: false,
        })
        .unwrap();
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the test fixture mirrors independent StoredEvent fields"
    )]
    fn insert_event(
        db: &Database,
        id: &str,
        at: DateTime<Utc>,
        event_type: EventType,
        stream_id: &str,
        session_id: Option<&str>,
        action: Option<&str>,
        assignment_source: Option<&str>,
    ) {
        db.insert_event(&StoredEvent {
            id: id.to_string(),
            timestamp: at,
            event_type,
            source: "test".to_string(),
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
            action: action.map(String::from),
            cwd: None,
            session_id: session_id.map(String::from),
            stream_id: Some(stream_id.to_string()),
            assignment_source: assignment_source.map(String::from),
            data: json!({}),
        })
        .unwrap();
    }

    fn insert_session(db: &Database, id: &str, end: DateTime<Utc>, session_type: SessionType) {
        db.upsert_agent_session(
            &AgentSession {
                session_id: id.to_string(),
                source: SessionSource::Claude,
                parent_session_id: None,
                session_type,
                project_path: "/test".to_string(),
                project_name: "test".to_string(),
                start_time: timestamp(0),
                end_time: Some(end),
                message_count: 0,
                summary: None,
                user_prompts: Vec::new(),
                starting_prompt: None,
                assistant_message_count: 0,
                tool_call_count: 0,
                user_message_timestamps: Vec::new(),
                tool_call_timestamps: Vec::new(),
            },
            None,
        )
        .unwrap();
    }

    fn window(start: DateTime<Utc>, end: DateTime<Utc>) -> TimelineWindow {
        TimelineWindow { start, end }
    }

    #[test]
    fn timeline_returns_exact_focus_and_delegated_intervals() {
        // Given: direct attention and a separately executing agent in one stream.
        let db = Database::open_in_memory().unwrap();
        let start = timestamp(0);
        let end = timestamp(5);
        insert_stream(&db, "stream-a");
        insert_session(&db, "agent-a", timestamp(4), SessionType::Agent);
        insert_event(
            &db,
            "focus",
            start,
            EventType::TmuxPaneFocus,
            "stream-a",
            None,
            None,
            None,
        );
        insert_event(
            &db,
            "scroll",
            timestamp(2),
            EventType::TmuxScroll,
            "stream-a",
            None,
            None,
            None,
        );
        insert_event(
            &db,
            "agent-start",
            start,
            EventType::AgentSession,
            "stream-a",
            Some("agent-a"),
            Some("started"),
            None,
        );
        insert_event(
            &db,
            "tool-use",
            timestamp(1),
            EventType::AgentToolUse,
            "stream-a",
            Some("agent-a"),
            None,
            None,
        );
        insert_event(
            &db,
            "agent-end",
            timestamp(4),
            EventType::AgentSession,
            "stream-a",
            Some("agent-a"),
            Some("ended"),
            None,
        );

        // When: assembling the half-open timeline window.
        let data = timeline_for_window(
            &db,
            window(start, end),
            &tt_core::AllocationConfig {
                attention_window_ms: 10 * 60 * 1000,
                ..Default::default()
            },
            Duration::minutes(15),
        )
        .unwrap();

        // Then: the allocation engine's direct and delegated intervals are preserved exactly.
        let stream = &data.streams_active[0];
        assert_eq!(
            stream.focus_intervals,
            vec![
                Interval {
                    start,
                    end: timestamp(2)
                },
                Interval {
                    start: timestamp(2),
                    end
                },
            ]
        );
        assert_eq!(
            stream.delegated_intervals,
            vec![Interval {
                start: timestamp(1),
                end: timestamp(4)
            }]
        );
    }

    #[test]
    fn timeline_excludes_events_at_the_exclusive_end() {
        // Given: user messages just inside and exactly at the right boundary.
        let db = Database::open_in_memory().unwrap();
        let start = timestamp(0);
        let end = start + Duration::seconds(5);
        insert_stream(&db, "inside");
        insert_stream(&db, "outside");
        insert_event(
            &db,
            "inside",
            end - Duration::milliseconds(1),
            EventType::UserMessage,
            "inside",
            Some("inside-session"),
            None,
            None,
        );
        insert_event(
            &db,
            "outside",
            end,
            EventType::UserMessage,
            "outside",
            Some("outside-session"),
            None,
            None,
        );

        // When: reading [start, end).
        let data = timeline_for_window(
            &db,
            window(start, end),
            &tt_core::AllocationConfig::default(),
            Duration::minutes(15),
        )
        .unwrap();

        // Then: only the event one millisecond before end contributes activity or a point.
        assert_eq!(data.streams_active.len(), 1);
        assert_eq!(data.streams_active[0].stream.id, "inside");
        assert_eq!(data.streams_active[0].events.len(), 1);
        assert_eq!(
            data.streams_active[0].events[0].session_id,
            "inside-session"
        );
    }

    #[test]
    fn timeline_keeps_agent_only_streams_as_delegated_only() {
        // Given: a stream with agent execution but no direct-attention events.
        let db = Database::open_in_memory().unwrap();
        let start = timestamp(0);
        insert_stream(&db, "agent-only");
        insert_session(&db, "agent-only-session", timestamp(3), SessionType::Agent);
        insert_event(
            &db,
            "agent-start",
            start,
            EventType::AgentSession,
            "agent-only",
            Some("agent-only-session"),
            Some("started"),
            None,
        );
        insert_event(
            &db,
            "tool-use",
            timestamp(1),
            EventType::AgentToolUse,
            "agent-only",
            Some("agent-only-session"),
            None,
            None,
        );
        insert_event(
            &db,
            "agent-end",
            timestamp(3),
            EventType::AgentSession,
            "agent-only",
            Some("agent-only-session"),
            Some("ended"),
            None,
        );

        // When: assembling the timeline.
        let data = timeline_for_window(
            &db,
            window(start, timestamp(5)),
            &tt_core::AllocationConfig::default(),
            Duration::minutes(15),
        )
        .unwrap();

        // Then: the stream is retained with only its delegated interval.
        let stream = &data.streams_active[0];
        assert!(stream.focus_intervals.is_empty());
        assert_eq!(
            stream.delegated_intervals,
            vec![Interval {
                start: timestamp(1),
                end: timestamp(3)
            }]
        );
    }

    #[test]
    fn timeline_marks_todo_linked_session_markers_and_subagent_starts() {
        // Given: linked, unlinked, and subagent session markers in one stream.
        let db = Database::open_in_memory().unwrap();
        let start = timestamp(0);
        insert_stream(&db, "stream-a");
        insert_session(&db, "subagent", timestamp(4), SessionType::Subagent);
        for (id, minute, session_id, action, source) in [
            ("linked-start", 0, "linked", "started", Some("todo_link")),
            ("linked-end", 1, "linked", "ended", Some("todo_link")),
            ("plain-start", 2, "plain", "started", None),
            ("plain-end", 3, "plain", "ended", None),
            ("subagent-start", 4, "subagent", "started", None),
        ] {
            insert_event(
                &db,
                id,
                timestamp(minute),
                EventType::AgentSession,
                "stream-a",
                Some(session_id),
                Some(action),
                source,
            );
        }

        // When: assembling the marker data.
        let data = timeline_for_window(
            &db,
            window(start, timestamp(5)),
            &tt_core::AllocationConfig::default(),
            Duration::minutes(15),
        )
        .unwrap();

        // Then: session markers expose their todo-link state and subagent starts are ticks.
        let points = &data.streams_active[0].events;
        assert_eq!(
            points
                .iter()
                .filter(|point| point.session_id == "linked")
                .map(|point| point.todo_linked)
                .collect::<Vec<_>>(),
            vec![Some(true), Some(true)]
        );
        assert_eq!(
            points
                .iter()
                .filter(|point| point.session_id == "plain")
                .map(|point| point.todo_linked)
                .collect::<Vec<_>>(),
            vec![Some(false), Some(false)]
        );
        let subagent = points
            .iter()
            .find(|point| point.session_id == "subagent")
            .unwrap();
        assert_eq!(subagent.kind, TimelinePointKind::SubagentStart);
        assert_eq!(subagent.todo_linked, None);
    }
}

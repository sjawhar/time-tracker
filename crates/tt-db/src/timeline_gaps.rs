use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use tt_core::EventType;

use crate::StoredEvent;

use super::timeline::TimelineWindow;

/// A cross-stream stretch with no human-input activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct IdleGap {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub duration_minutes: i64,
}

pub(super) fn idle_gaps_for_events(
    window: TimelineWindow,
    events: &[StoredEvent],
    idle_threshold: Duration,
) -> Vec<IdleGap> {
    let mut activity_boundaries = events
        .iter()
        .filter(|event| is_human_input(event.event_type))
        .map(|event| event.timestamp)
        .filter(|timestamp| *timestamp >= window.start && *timestamp < window.end)
        .collect::<Vec<_>>();
    activity_boundaries.sort_unstable();
    activity_boundaries.dedup();
    activity_boundaries.insert(0, window.start);
    activity_boundaries.push(window.end);

    activity_boundaries
        .windows(2)
        .filter_map(|boundaries| {
            let start = boundaries[0];
            let end = boundaries[1];
            let duration = end - start;
            (end > start && duration >= idle_threshold).then_some(IdleGap {
                start,
                end,
                duration_minutes: duration.num_minutes(),
            })
        })
        .collect()
}

const fn is_human_input(event_type: EventType) -> bool {
    matches!(
        event_type,
        EventType::TmuxPaneFocus | EventType::UserMessage
    )
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Duration, TimeZone, Utc};
    use serde_json::json;
    use tt_core::{AllocationConfig, EventType};

    use crate::{Database, StoredEvent, Stream, TimelineWindow, timeline_for_window};

    const IDLE_THRESHOLD: Duration = Duration::minutes(15);

    fn timestamp(seconds: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0).single().unwrap() + Duration::seconds(seconds)
    }

    fn insert_event(db: &Database, id: &str, seconds: i64, event_type: EventType, stream_id: &str) {
        db.insert_event(&StoredEvent {
            id: id.to_string(),
            timestamp: timestamp(seconds),
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
            action: None,
            cwd: None,
            session_id: Some("session".to_string()),
            stream_id: Some(stream_id.to_string()),
            assignment_source: None,
            data: json!({}),
        })
        .unwrap();
    }

    fn insert_stream(db: &Database, id: &str) {
        let created_at = timestamp(0);
        db.insert_stream(&Stream {
            id: id.to_string(),
            name: Some(id.to_string()),
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
        })
        .unwrap();
    }

    fn idle_gaps(
        start_seconds: i64,
        end_seconds: i64,
        events: &[(&str, i64, EventType, &str)],
    ) -> Vec<crate::IdleGap> {
        let db = Database::open_in_memory().unwrap();
        for stream_id in ["stream-a", "stream-b", "agent-stream"] {
            insert_stream(&db, stream_id);
        }
        for (id, seconds, event_type, stream_id) in events {
            insert_event(&db, id, *seconds, *event_type, stream_id);
        }
        timeline_for_window(
            &db,
            TimelineWindow {
                start: timestamp(start_seconds),
                end: timestamp(end_seconds),
            },
            &AllocationConfig::default(),
            IDLE_THRESHOLD,
        )
        .unwrap()
        .idle_gaps
    }

    #[test]
    fn folds_gap_exactly_at_idle_threshold() {
        // Given: a window with no human-input activity for exactly fifteen minutes.
        let gaps = idle_gaps(0, 15 * 60, &[]);

        // When: the idle gap detector processes the window.

        // Then: the threshold-length gap is emitted with a renderer-ready duration.
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].start, timestamp(0));
        assert_eq!(gaps[0].end, timestamp(15 * 60));
        assert_eq!(gaps[0].duration_minutes, 15);
    }

    #[test]
    fn skips_gap_one_second_under_idle_threshold() {
        // Given: a window that is one second shorter than the configured threshold.
        let gaps = idle_gaps(0, 15 * 60 - 1, &[]);

        // When: the idle gap detector processes the window.

        // Then: no fold is produced.
        assert!(gaps.is_empty());
    }

    #[test]
    fn folds_through_agent_only_activity() {
        // Given: an unattended agent tool-use event in the middle of an otherwise idle window.
        let gaps = idle_gaps(
            0,
            30 * 60,
            &[(
                "agent-tool",
                15 * 60,
                EventType::AgentToolUse,
                "agent-stream",
            )],
        );

        // When: the idle gap detector processes the window.

        // Then: agent activity does not interrupt the cross-stream human-input gap.
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].start, timestamp(0));
        assert_eq!(gaps[0].end, timestamp(30 * 60));
    }

    #[test]
    fn user_message_splits_idle_gaps_across_streams() {
        // Given: a user message in one stream and focus activity in another.
        let gaps = idle_gaps(
            0,
            60 * 60,
            &[
                ("message", 20 * 60, EventType::UserMessage, "stream-a"),
                ("focus", 40 * 60, EventType::TmuxPaneFocus, "stream-b"),
            ],
        );

        // When: the idle gap detector processes all streams together.

        // Then: either human-input event ends one gap and starts the next.
        assert_eq!(gaps.len(), 3);
        assert_eq!(gaps[0].start, timestamp(0));
        assert_eq!(gaps[0].end, timestamp(20 * 60));
        assert_eq!(gaps[1].start, timestamp(20 * 60));
        assert_eq!(gaps[1].end, timestamp(40 * 60));
        assert_eq!(gaps[2].start, timestamp(40 * 60));
        assert_eq!(gaps[2].end, timestamp(60 * 60));
    }

    #[test]
    fn clips_gaps_to_both_window_edges() {
        // Given: human-input events before and after, but not inside, the requested window.
        let gaps = idle_gaps(
            10 * 60,
            30 * 60,
            &[
                ("before", 0, EventType::UserMessage, "stream-a"),
                ("after", 40 * 60, EventType::TmuxPaneFocus, "stream-b"),
            ],
        );

        // When: the idle gap detector processes the clipped timeline window.

        // Then: its fold starts and ends exactly at the requested window edges.
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].start, timestamp(10 * 60));
        assert_eq!(gaps[0].end, timestamp(30 * 60));
    }

    #[test]
    fn emitted_gaps_never_overlap() {
        // Given: three human-input points that divide the window into threshold-length gaps.
        let gaps = idle_gaps(
            0,
            60 * 60,
            &[
                ("message", 15 * 60, EventType::UserMessage, "stream-a"),
                ("focus-a", 30 * 60, EventType::TmuxPaneFocus, "stream-b"),
                ("focus-b", 45 * 60, EventType::TmuxPaneFocus, "stream-a"),
            ],
        );

        // When: the idle gap detector returns all folds.

        // Then: adjacent folds may meet but never overlap.
        assert_eq!(gaps.len(), 4);
        assert!(gaps.windows(2).all(|pair| pair[0].end <= pair[1].start));
    }
}

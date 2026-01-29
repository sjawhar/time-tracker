//! Time allocation algorithm.
//!
//! Calculates direct (user focus) and delegated (agent) time for streams
//! based on the attention allocation algorithm in `specs/architecture/overview.md`.
//!
//! # Algorithm Summary
//!
//! 1. Build focus timeline from focus events (`tmux_pane_focus`, `afk_change`, etc.)
//! 2. Build agent activity timeline from `agent_session` and `agent_tool_use` events
//! 3. Iterate through event intervals, attributing time based on state

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};

/// Configuration for time allocation.
#[derive(Debug, Clone)]
pub struct AllocationConfig {
    /// Grace period after last focus event before direct time pauses.
    /// Default: 60000 (1 minute).
    pub attention_window_ms: i64,

    /// If no `agent_tool_use` for this duration after the most recent tool use,
    /// assume session crashed. Session ends at last tool use timestamp.
    /// Default: 1800000 (30 minutes).
    pub agent_timeout_ms: i64,
}

impl Default for AllocationConfig {
    fn default() -> Self {
        Self {
            attention_window_ms: 60_000, // 1 minute
            agent_timeout_ms: 1_800_000, // 30 minutes
        }
    }
}

/// Computed time for a single stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamTime {
    /// The stream ID.
    pub stream_id: String,

    /// Total human attention time in milliseconds.
    pub time_direct_ms: i64,

    /// Total agent execution time in milliseconds.
    pub time_delegated_ms: i64,
}

/// Result of time allocation calculation.
#[derive(Debug, Clone)]
pub struct AllocationResult {
    /// Time computed per stream.
    pub stream_times: Vec<StreamTime>,

    /// Total wall-clock time with any activity (union of intervals, not sum).
    pub total_tracked_ms: i64,
}

/// An event suitable for time allocation.
///
/// This trait allows allocation to work with different event representations
/// (e.g., `StoredEvent` from tt-db, or test fixtures).
pub trait AllocatableEvent {
    /// Returns the event's timestamp.
    fn timestamp(&self) -> DateTime<Utc>;

    /// Returns the event's type (e.g., "`tmux_pane_focus`", "`agent_session`").
    fn event_type(&self) -> &str;

    /// Returns the stream ID if assigned.
    fn stream_id(&self) -> Option<&str>;

    /// Returns the agent session ID if applicable.
    fn session_id(&self) -> Option<&str>;

    /// Returns the event's data payload.
    fn data(&self) -> &serde_json::Value;
}

/// Current focus state.
#[derive(Debug, Clone)]
enum FocusState {
    /// User is focused on a stream.
    Focused {
        stream_id: String,
        /// When focus started or last activity occurred
        focus_start: DateTime<Utc>,
    },
    /// No active focus (AFK or no focus events yet).
    Unfocused,
}

/// Tracked agent session state.
#[derive(Debug, Clone)]
struct AgentSession {
    /// Which stream this agent is working in.
    stream_id: String,

    /// When the first tool use occurred (None = no tool use yet).
    first_tool_use_at: Option<DateTime<Utc>>,

    /// When the last tool use occurred.
    last_tool_use_at: Option<DateTime<Utc>>,

    /// Whether the session has ended.
    ended: bool,
}

/// An activity interval for tracking total time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Interval {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
}

impl Interval {
    fn duration_ms(&self) -> i64 {
        (self.end - self.start).num_milliseconds()
    }
}

/// Calculate time allocation for a time range.
///
/// Events must be sorted by timestamp ascending.
/// Events with `stream_id = None` are excluded from direct time attribution
/// (but may still contribute to agent tracking if they have `session_id`).
///
/// # Arguments
///
/// * `events` - Events to process (must implement `AllocatableEvent`)
/// * `config` - Allocation configuration
/// * `period_end` - Where to close open intervals. If None, uses last event + `attention_window`
///
/// # Returns
///
/// Computed time per stream and total tracked time.
pub fn allocate_time<E: AllocatableEvent>(
    events: &[E],
    config: &AllocationConfig,
    period_end: Option<DateTime<Utc>>,
) -> AllocationResult {
    let mut focus_state = FocusState::Unfocused;
    let mut agent_sessions: HashMap<String, AgentSession> = HashMap::new();
    let mut stream_times: HashMap<String, (i64, i64)> = HashMap::new(); // (direct_ms, delegated_ms)
    let mut activity_intervals: Vec<Interval> = Vec::new();
    let mut last_event_time: Option<DateTime<Utc>> = None;

    // Helper to add direct time
    let add_direct = |stream_id: &str,
                      start: DateTime<Utc>,
                      end: DateTime<Utc>,
                      intervals: &mut Vec<Interval>,
                      times: &mut HashMap<String, (i64, i64)>| {
        if end > start {
            let duration_ms = (end - start).num_milliseconds();
            let (direct, _) = times.entry(stream_id.to_string()).or_insert((0, 0));
            *direct += duration_ms;
            intervals.push(Interval { start, end });
        }
    };

    // Helper to add delegated time
    let add_delegated = |stream_id: &str,
                         start: DateTime<Utc>,
                         end: DateTime<Utc>,
                         intervals: &mut Vec<Interval>,
                         times: &mut HashMap<String, (i64, i64)>| {
        if end > start {
            let duration_ms = (end - start).num_milliseconds();
            let (_, delegated) = times.entry(stream_id.to_string()).or_insert((0, 0));
            *delegated += duration_ms;
            intervals.push(Interval { start, end });
        }
    };

    for event in events {
        let event_time = event.timestamp();
        let event_type = event.event_type();
        let data = event.data();

        // Check for agent timeouts before processing this event
        // Collect attributions first to avoid borrow issues
        let timeout_attributions: Vec<_> = agent_sessions
            .iter()
            .filter(|(_, session)| !session.ended && session.last_tool_use_at.is_some())
            .filter_map(|(session_id, session)| {
                let last_tool = session.last_tool_use_at.unwrap();
                let timeout_at = last_tool + Duration::milliseconds(config.agent_timeout_ms);
                if event_time > timeout_at {
                    // Session timed out - attribute time from first to last tool use + timeout
                    // Actually per spec: session ends at last_tool_use timestamp
                    // But delegated time runs from first_tool_use to timeout_at
                    if let Some(first_tool) = session.first_tool_use_at {
                        return Some((
                            session_id.clone(),
                            session.stream_id.clone(),
                            first_tool,
                            timeout_at,
                        ));
                    }
                }
                None
            })
            .collect();

        for (session_id, stream_id, first_tool, timeout_at) in timeout_attributions {
            // Attribute delegated time from first tool use to timeout
            add_delegated(
                &stream_id,
                first_tool,
                timeout_at,
                &mut activity_intervals,
                &mut stream_times,
            );
            // Mark session as ended
            if let Some(session) = agent_sessions.get_mut(&session_id) {
                session.ended = true;
            }
        }

        match event_type {
            "tmux_pane_focus" => {
                if let Some(stream_id) = event.stream_id() {
                    // Close previous focus interval
                    if let FocusState::Focused {
                        stream_id: prev_stream,
                        focus_start,
                    } = &focus_state
                    {
                        // Attribute time up to this event
                        add_direct(
                            prev_stream,
                            *focus_start,
                            event_time,
                            &mut activity_intervals,
                            &mut stream_times,
                        );
                    }

                    // Start new focus
                    focus_state = FocusState::Focused {
                        stream_id: stream_id.to_string(),
                        focus_start: event_time,
                    };
                }
            }

            "afk_change" => {
                let status = data.get("status").and_then(|v| v.as_str()).unwrap_or("");
                if status == "idle" {
                    // AFK - close focus
                    if let FocusState::Focused {
                        stream_id,
                        focus_start,
                    } = &focus_state
                    {
                        add_direct(
                            stream_id,
                            *focus_start,
                            event_time,
                            &mut activity_intervals,
                            &mut stream_times,
                        );
                    }
                    focus_state = FocusState::Unfocused;
                }
                // Note: "active" does NOT restore focus - wait for next focus event
            }

            "tmux_scroll" | "user_message" => {
                // These confirm focus and reset attention window, but only if
                // the event is for the currently focused stream
                if let FocusState::Focused {
                    stream_id: focused_stream,
                    focus_start,
                } = &focus_state
                {
                    // Only reset attention window if this event is for the focused stream
                    let event_stream = event.stream_id();
                    if event_stream == Some(focused_stream.as_str()) {
                        if event_time > *focus_start {
                            add_direct(
                                focused_stream,
                                *focus_start,
                                event_time,
                                &mut activity_intervals,
                                &mut stream_times,
                            );
                        }
                        focus_state = FocusState::Focused {
                            stream_id: focused_stream.clone(),
                            focus_start: event_time,
                        };
                    }
                    // If event is for a different stream, ignore it - doesn't affect focus state
                }
            }

            "agent_session" => {
                let action = data.get("action").and_then(|v| v.as_str()).unwrap_or("");
                let session_id = event.session_id().unwrap_or("");

                match action {
                    "started" => {
                        if let Some(stream_id) = event.stream_id() {
                            agent_sessions.insert(
                                session_id.to_string(),
                                AgentSession {
                                    stream_id: stream_id.to_string(),
                                    first_tool_use_at: None,
                                    last_tool_use_at: None,
                                    ended: false,
                                },
                            );
                        }
                    }
                    "ended" => {
                        // Close the session
                        if let Some(session) = agent_sessions.get(session_id) {
                            if !session.ended {
                                if let Some(first_tool) = session.first_tool_use_at {
                                    // Attribute from first tool use to end
                                    add_delegated(
                                        &session.stream_id.clone(),
                                        first_tool,
                                        event_time,
                                        &mut activity_intervals,
                                        &mut stream_times,
                                    );
                                }
                            }
                        }
                        if let Some(session) = agent_sessions.get_mut(session_id) {
                            session.ended = true;
                        }
                    }
                    _ => {}
                }
            }

            "agent_tool_use" => {
                let session_id = event.session_id().unwrap_or("");
                if let Some(session) = agent_sessions.get_mut(session_id) {
                    if !session.ended {
                        if session.first_tool_use_at.is_none() {
                            // First tool use - delegated time starts here
                            session.first_tool_use_at = Some(event_time);
                        }
                        session.last_tool_use_at = Some(event_time);
                    }
                }
            }

            _ => {}
        }

        last_event_time = Some(event_time);
    }

    // Finalize: close open intervals
    let end_time = period_end.or(last_event_time);

    if let Some(end) = end_time {
        // Close focus - cap at attention window
        if let FocusState::Focused {
            stream_id,
            focus_start,
        } = &focus_state
        {
            let window_end = *focus_start + Duration::milliseconds(config.attention_window_ms);
            let actual_end = if let Some(pe) = period_end {
                pe.min(window_end)
            } else {
                window_end
            };
            if actual_end > *focus_start {
                add_direct(
                    stream_id,
                    *focus_start,
                    actual_end,
                    &mut activity_intervals,
                    &mut stream_times,
                );
            }
        }

        // Close active agent sessions
        let final_attributions: Vec<_> = agent_sessions
            .values()
            .filter(|session| !session.ended && session.first_tool_use_at.is_some())
            .map(|session| {
                let first_tool = session.first_tool_use_at.unwrap();
                let last_tool = session.last_tool_use_at.unwrap_or(first_tool);

                // Check for timeout
                let timeout_at = last_tool + Duration::milliseconds(config.agent_timeout_ms);
                let session_end = if end > timeout_at { timeout_at } else { end };

                (session.stream_id.clone(), first_tool, session_end)
            })
            .collect();

        for (stream_id, first_tool, session_end) in final_attributions {
            if session_end > first_tool {
                add_delegated(
                    &stream_id,
                    first_tool,
                    session_end,
                    &mut activity_intervals,
                    &mut stream_times,
                );
            }
        }
    }

    // Calculate total tracked time from interval union
    let total_tracked_ms = calculate_total_tracked(&activity_intervals);

    let stream_times_vec = stream_times
        .into_iter()
        .map(|(stream_id, (direct, delegated))| StreamTime {
            stream_id,
            time_direct_ms: direct,
            time_delegated_ms: delegated,
        })
        .collect();

    AllocationResult {
        stream_times: stream_times_vec,
        total_tracked_ms,
    }
}

/// Calculate total tracked time from interval union.
fn calculate_total_tracked(intervals: &[Interval]) -> i64 {
    if intervals.is_empty() {
        return 0;
    }

    // Sort intervals by start time
    let mut sorted: Vec<Interval> = intervals.to_vec();
    sorted.sort_by_key(|i| i.start);

    // Merge overlapping intervals
    let mut merged: Vec<Interval> = Vec::new();
    for interval in sorted {
        if let Some(last) = merged.last_mut() {
            if interval.start <= last.end {
                last.end = last.end.max(interval.end);
            } else {
                merged.push(interval);
            }
        } else {
            merged.push(interval);
        }
    }

    merged.iter().map(Interval::duration_ms).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    /// Test event implementation.
    struct TestEvent {
        timestamp: DateTime<Utc>,
        event_type: String,
        stream_id: Option<String>,
        session_id: Option<String>,
        data: serde_json::Value,
    }

    impl TestEvent {
        fn tmux_focus(ts: DateTime<Utc>, stream_id: &str) -> Self {
            Self {
                timestamp: ts,
                event_type: "tmux_pane_focus".to_string(),
                stream_id: Some(stream_id.to_string()),
                session_id: None,
                data: json!({"pane_id": "%1", "cwd": "/test"}),
            }
        }

        fn afk_change(ts: DateTime<Utc>, status: &str) -> Self {
            Self {
                timestamp: ts,
                event_type: "afk_change".to_string(),
                stream_id: None,
                session_id: None,
                data: json!({"status": status}),
            }
        }

        fn tmux_scroll(ts: DateTime<Utc>, stream_id: &str) -> Self {
            Self {
                timestamp: ts,
                event_type: "tmux_scroll".to_string(),
                stream_id: Some(stream_id.to_string()),
                session_id: None,
                data: json!({"direction": "up"}),
            }
        }

        fn agent_session(
            ts: DateTime<Utc>,
            action: &str,
            session_id: &str,
            stream_id: Option<&str>,
        ) -> Self {
            Self {
                timestamp: ts,
                event_type: "agent_session".to_string(),
                stream_id: stream_id.map(String::from),
                session_id: Some(session_id.to_string()),
                data: json!({"action": action, "agent": "claude-code"}),
            }
        }

        fn agent_tool_use(ts: DateTime<Utc>, session_id: &str, stream_id: &str) -> Self {
            Self {
                timestamp: ts,
                event_type: "agent_tool_use".to_string(),
                stream_id: Some(stream_id.to_string()),
                session_id: Some(session_id.to_string()),
                data: json!({"tool": "Edit"}),
            }
        }

        fn user_message(ts: DateTime<Utc>, session_id: &str, stream_id: &str) -> Self {
            Self {
                timestamp: ts,
                event_type: "user_message".to_string(),
                stream_id: Some(stream_id.to_string()),
                session_id: Some(session_id.to_string()),
                data: json!({"length": 100}),
            }
        }
    }

    impl AllocatableEvent for TestEvent {
        fn timestamp(&self) -> DateTime<Utc> {
            self.timestamp
        }

        fn event_type(&self) -> &str {
            &self.event_type
        }

        fn stream_id(&self) -> Option<&str> {
            self.stream_id.as_deref()
        }

        fn session_id(&self) -> Option<&str> {
            self.session_id.as_deref()
        }

        fn data(&self) -> &serde_json::Value {
            &self.data
        }
    }

    fn ts(minutes: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0).unwrap() + Duration::minutes(minutes)
    }

    fn get_stream_time<'a>(
        result: &'a AllocationResult,
        stream_id: &str,
    ) -> Option<&'a StreamTime> {
        result
            .stream_times
            .iter()
            .find(|s| s.stream_id == stream_id)
    }

    // Test 1: Single stream, continuous focus
    #[test]
    fn test_single_stream_continuous_focus() {
        let events = vec![
            TestEvent::tmux_focus(ts(0), "A"),
            TestEvent::tmux_scroll(ts(5), "A"),
            TestEvent::tmux_scroll(ts(10), "A"),
        ];

        let config = AllocationConfig::default();
        // Set period_end to cap the final attention window
        let result = allocate_time(&events, &config, Some(ts(11)));

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // Direct time: 0->5 (focus to scroll) + 5->10 (scroll to scroll) + 10->11 (scroll to period_end, capped by attention window)
        // Total: 5 + 5 + 1 = 11 minutes
        assert_eq!(stream_a.time_direct_ms, 11 * 60 * 1000);
    }

    // Test 2: Focus switches between streams
    #[test]
    fn test_focus_switches_between_streams() {
        let events = vec![
            TestEvent::tmux_focus(ts(0), "A"),
            TestEvent::tmux_focus(ts(10), "B"),
        ];

        let config = AllocationConfig::default();
        let result = allocate_time(&events, &config, Some(ts(20)));

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        let stream_b = get_stream_time(&result, "B").expect("Stream B should exist");

        // Stream A: 0 to 10 = 10 minutes
        assert_eq!(stream_a.time_direct_ms, 10 * 60 * 1000);
        // Stream B: 10 to min(20, 10+1) = 10 to 11 = 1 minute (attention window)
        assert_eq!(stream_b.time_direct_ms, 1 * 60 * 1000);
    }

    // Test 3: AFK pauses direct time
    #[test]
    fn test_afk_pauses_direct_time() {
        let events = vec![
            TestEvent::tmux_focus(ts(0), "A"),
            TestEvent::afk_change(ts(10), "idle"),
            TestEvent::afk_change(ts(15), "active"),
        ];

        let config = AllocationConfig::default();
        let result = allocate_time(&events, &config, Some(ts(20)));

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // Direct time: 0-10 only (AFK pauses at 10, active at 15 doesn't restore)
        assert_eq!(stream_a.time_direct_ms, 10 * 60 * 1000);
    }

    // Test 4: AFK active doesn't restore focus
    #[test]
    fn test_afk_active_does_not_restore_focus() {
        let events = vec![
            TestEvent::tmux_focus(ts(0), "A"),
            TestEvent::afk_change(ts(5), "idle"),
            TestEvent::afk_change(ts(10), "active"),
            // No focus event after active
        ];

        let config = AllocationConfig::default();
        let result = allocate_time(&events, &config, Some(ts(20)));

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // Direct time: only 0-5 (AFK at 5)
        assert_eq!(stream_a.time_direct_ms, 5 * 60 * 1000);
    }

    // Test 5: Single agent session
    #[test]
    fn test_single_agent_session() {
        let events = vec![
            TestEvent::agent_session(ts(0), "started", "sess1", Some("A")),
            TestEvent::agent_tool_use(ts(5), "sess1", "A"),
            TestEvent::agent_session(ts(30), "ended", "sess1", Some("A")),
        ];

        let config = AllocationConfig::default();
        let result = allocate_time(&events, &config, Some(ts(30)));

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // Delegated: from first tool use (5) to end (30) = 25 minutes
        assert_eq!(stream_a.time_delegated_ms, 25 * 60 * 1000);
        assert_eq!(stream_a.time_direct_ms, 0);
    }

    // Test 6: Agent session with no tool use
    #[test]
    fn test_agent_session_no_tool_use() {
        let events = vec![
            TestEvent::agent_session(ts(0), "started", "sess1", Some("A")),
            TestEvent::agent_session(ts(30), "ended", "sess1", Some("A")),
        ];

        let config = AllocationConfig::default();
        let result = allocate_time(&events, &config, Some(ts(30)));

        // No tool use = no delegated time
        let stream_a = get_stream_time(&result, "A");
        assert!(stream_a.is_none() || stream_a.unwrap().time_delegated_ms == 0);
    }

    // Test 7: Agent timeout (crashed session)
    #[test]
    fn test_agent_timeout() {
        let events = vec![
            TestEvent::agent_session(ts(0), "started", "sess1", Some("A")),
            TestEvent::agent_tool_use(ts(5), "sess1", "A"),
            // No end event, next event at 60min (35min after last tool use)
            TestEvent::tmux_focus(ts(60), "B"),
        ];

        let mut config = AllocationConfig::default();
        config.agent_timeout_ms = 30 * 60 * 1000; // 30 minutes

        let result = allocate_time(&events, &config, Some(ts(60)));

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // Session times out at 5 + 30 = 35 minutes
        // Delegated: from 5 to 35 = 30 minutes
        assert_eq!(stream_a.time_delegated_ms, 30 * 60 * 1000);
    }

    // Test 8: Concurrent agents in different streams
    #[test]
    fn test_concurrent_agents() {
        let events = vec![
            TestEvent::agent_session(ts(0), "started", "sess1", Some("A")),
            TestEvent::agent_session(ts(0), "started", "sess2", Some("B")),
            TestEvent::agent_tool_use(ts(5), "sess1", "A"),
            TestEvent::agent_tool_use(ts(5), "sess2", "B"),
            TestEvent::agent_session(ts(30), "ended", "sess1", Some("A")),
            TestEvent::agent_session(ts(30), "ended", "sess2", Some("B")),
        ];

        let config = AllocationConfig::default();
        let result = allocate_time(&events, &config, Some(ts(30)));

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        let stream_b = get_stream_time(&result, "B").expect("Stream B should exist");

        // Both agents: 5 to 30 = 25 minutes each
        assert_eq!(stream_a.time_delegated_ms, 25 * 60 * 1000);
        assert_eq!(stream_b.time_delegated_ms, 25 * 60 * 1000);
    }

    // Test 9: User focused while agent works
    #[test]
    fn test_user_focused_while_agent_works() {
        let events = vec![
            TestEvent::tmux_focus(ts(0), "A"),
            TestEvent::agent_session(ts(0), "started", "sess1", Some("A")),
            TestEvent::agent_tool_use(ts(5), "sess1", "A"),
            TestEvent::agent_session(ts(30), "ended", "sess1", Some("A")),
        ];

        let config = AllocationConfig::default();
        let result = allocate_time(&events, &config, Some(ts(30)));

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");

        // Focus: from 0 to min(30, 0+1) = 1 minute (attention window)
        // Delegated: 5 to 30 = 25 minutes
        assert_eq!(stream_a.time_direct_ms, 1 * 60 * 1000);
        assert_eq!(stream_a.time_delegated_ms, 25 * 60 * 1000);
    }

    // Test 10: Attention window expiry
    #[test]
    fn test_attention_window_expiry() {
        let events = vec![
            TestEvent::tmux_focus(ts(0), "A"),
            // No further events
        ];

        let config = AllocationConfig {
            attention_window_ms: 60_000, // 1 minute
            agent_timeout_ms: 30 * 60 * 1000,
        };
        let result = allocate_time(&events, &config, Some(ts(10)));

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // Direct time caps at attention window: 1 minute
        assert_eq!(stream_a.time_direct_ms, 1 * 60 * 1000);
    }

    // Test 11: Scroll resets attention window
    #[test]
    fn test_scroll_resets_attention_window() {
        let events = vec![
            TestEvent::tmux_focus(ts(0), "A"),
            TestEvent::tmux_scroll(ts(0) + Duration::seconds(30), "A"), // 30 seconds later
        ];

        let config = AllocationConfig {
            attention_window_ms: 60_000, // 1 minute
            agent_timeout_ms: 30 * 60 * 1000,
        };
        let result = allocate_time(&events, &config, Some(ts(10)));

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // Focus at 0, scroll at 0:30, attention window from scroll = 1:30
        // Total: 1 minute 30 seconds
        assert_eq!(stream_a.time_direct_ms, 90 * 1000);
    }

    // Test 12: Events in unfocused streams
    #[test]
    fn test_events_in_unfocused_streams() {
        let events = vec![
            TestEvent::tmux_focus(ts(0), "A"),
            // Some activity in B while focused on A (no agent)
            TestEvent::tmux_scroll(ts(5), "B"), // This scroll doesn't affect focus since focus is on A
        ];

        let config = AllocationConfig::default();
        let result = allocate_time(&events, &config, Some(ts(10)));

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // Focus is on A the whole time. The scroll in B doesn't change focus state.
        // Scroll events only reset attention window if we're already focused on that stream.
        // Direct time for A: from 0 to min(10, 0+1) = 1 minute
        assert_eq!(stream_a.time_direct_ms, 1 * 60 * 1000);

        // Stream B gets no direct time (no focus on B)
        let stream_b = get_stream_time(&result, "B");
        assert!(stream_b.is_none() || stream_b.unwrap().time_direct_ms == 0);
    }

    // Test 13: Events with stream_id = null excluded
    #[test]
    fn test_events_without_stream_excluded() {
        let events = vec![TestEvent {
            timestamp: ts(0),
            event_type: "tmux_pane_focus".to_string(),
            stream_id: None, // Not assigned to any stream
            session_id: None,
            data: json!({"pane_id": "%1"}),
        }];

        let config = AllocationConfig::default();
        let result = allocate_time(&events, &config, Some(ts(10)));

        // No streams should have time
        assert!(
            result.stream_times.is_empty()
                || result
                    .stream_times
                    .iter()
                    .all(|s| s.time_direct_ms == 0 && s.time_delegated_ms == 0)
        );
    }

    // Test 14: Combined focus + agent + AFK
    #[test]
    fn test_combined_focus_agent_afk() {
        let events = vec![
            TestEvent::tmux_focus(ts(0), "A"),
            TestEvent::agent_session(ts(0), "started", "sess1", Some("A")),
            TestEvent::agent_tool_use(ts(5), "sess1", "A"),
            TestEvent::afk_change(ts(10), "idle"),
            TestEvent::agent_session(ts(30), "ended", "sess1", Some("A")),
        ];

        let config = AllocationConfig::default();
        let result = allocate_time(&events, &config, Some(ts(30)));

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");

        // Direct: from 0 to 10 (AFK pauses) = 10 minutes
        // Delegated: from 5 to 30 = 25 minutes
        assert_eq!(stream_a.time_direct_ms, 10 * 60 * 1000);
        assert_eq!(stream_a.time_delegated_ms, 25 * 60 * 1000);
    }

    // Test 15: Total tracked time (interval union)
    #[test]
    fn test_total_tracked_time_union() {
        let events = vec![
            TestEvent::tmux_focus(ts(0), "A"),
            TestEvent::agent_session(ts(0), "started", "sess1", Some("A")),
            TestEvent::agent_tool_use(ts(5), "sess1", "A"),
            TestEvent::afk_change(ts(10), "idle"),
            TestEvent::agent_session(ts(20), "ended", "sess1", Some("A")),
        ];

        let config = AllocationConfig::default();
        let result = allocate_time(&events, &config, Some(ts(20)));

        // Direct: [0, 10) = 10 min
        // Delegated: [5, 20) = 15 min
        // Union: [0, 20) = 20 min
        assert_eq!(result.total_tracked_ms, 20 * 60 * 1000);
    }

    // Test: Multiple tool uses in one session
    #[test]
    fn test_multiple_tool_uses() {
        let events = vec![
            TestEvent::agent_session(ts(0), "started", "sess1", Some("A")),
            TestEvent::agent_tool_use(ts(5), "sess1", "A"),
            TestEvent::agent_tool_use(ts(10), "sess1", "A"),
            TestEvent::agent_tool_use(ts(15), "sess1", "A"),
            TestEvent::agent_session(ts(20), "ended", "sess1", Some("A")),
        ];

        let config = AllocationConfig::default();
        let result = allocate_time(&events, &config, Some(ts(20)));

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // Delegated: from first tool (5) to end (20) = 15 minutes
        assert_eq!(stream_a.time_delegated_ms, 15 * 60 * 1000);
    }

    // Test: User message resets attention window
    #[test]
    fn test_user_message_resets_attention() {
        let events = vec![
            TestEvent::tmux_focus(ts(0), "A"),
            TestEvent::agent_session(ts(0), "started", "sess1", Some("A")),
            TestEvent::user_message(ts(0) + Duration::seconds(30), "sess1", "A"),
        ];

        let config = AllocationConfig {
            attention_window_ms: 60_000,
            agent_timeout_ms: 30 * 60 * 1000,
        };
        let result = allocate_time(&events, &config, Some(ts(5)));

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // Focus at 0, user_message at 30s, attention window extends to 1:30
        // But period_end is at 5 min, so finalize uses min(5, 0:30 + 1:00) = 1:30
        assert_eq!(stream_a.time_direct_ms, 90 * 1000);
    }

    // Test: Empty events
    #[test]
    fn test_empty_events() {
        let events: Vec<TestEvent> = vec![];
        let config = AllocationConfig::default();
        let result = allocate_time(&events, &config, None);

        assert!(result.stream_times.is_empty());
        assert_eq!(result.total_tracked_ms, 0);
    }
}

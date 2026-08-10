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

use crate::{EventType, SessionType};

/// Synthetic stream id for activity not assigned to any real stream. It is removed
/// from `stream_times` before returning (surfaced via `AllocationResult::unassigned_*_ms`),
/// so it never leaks into a real stream id (and thus never reaches the DB on recompute).
const UNASSIGNED_STREAM_ID: &str = "(unassigned)";

/// Configuration for time allocation.
#[derive(Debug, Clone)]
pub struct AllocationConfig {
    /// Grace period after last focus event before direct time pauses.
    /// Default: 300000 (5 minutes).
    pub attention_window_ms: i64,

    /// If no `agent_tool_use` for this duration after the most recent tool use,
    /// assume session crashed. Session ends at last tool use timestamp.
    /// Default: 1800000 (30 minutes).
    pub agent_timeout_ms: i64,
}

impl Default for AllocationConfig {
    fn default() -> Self {
        Self {
            attention_window_ms: 300_000, // 5 minutes
            agent_timeout_ms: 1_800_000,  // 30 minutes
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

    /// Direct-attention intervals used to compute `time_direct_ms`.
    pub focus_intervals: Vec<Interval>,

    /// Agent-execution intervals used to compute `time_delegated_ms`.
    pub delegated_intervals: Vec<Interval>,
}

/// Result of time allocation calculation.
#[derive(Debug, Clone)]
pub struct AllocationResult {
    /// Time computed per stream.
    pub stream_times: Vec<StreamTime>,

    /// Total wall-clock time with any activity (union of intervals, not sum).
    pub total_tracked_ms: i64,

    /// Human attention time on events not assigned to any stream.
    pub unassigned_direct_ms: i64,

    /// Agent execution time on events not assigned to any stream.
    pub unassigned_delegated_ms: i64,
}

/// An event suitable for time allocation.
///
/// This trait allows allocation to work with different event representations
/// (e.g., `StoredEvent` from tt-db, or test fixtures).
pub trait AllocatableEvent {
    /// Returns the event's timestamp.
    fn timestamp(&self) -> DateTime<Utc>;

    /// Returns the event's type.
    fn event_type(&self) -> EventType;

    /// Returns the stream ID if assigned.
    fn stream_id(&self) -> Option<&str>;

    /// Returns the agent session ID if applicable.
    fn session_id(&self) -> Option<&str>;

    /// Returns the action for `agent_session` events (e.g., "started", "ended").
    fn action(&self) -> Option<&str>;

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

/// Current window focus state.
#[derive(Debug, Clone, Default)]
struct WindowFocusState {
    /// Currently focused application name (lowercase).
    app: Option<String>,
    /// Stream associated with window focus event.
    stream_id: Option<String>,
}

/// Current browser tab focus state.
#[derive(Debug, Clone, Default)]
struct BrowserFocusState {
    /// Stream associated with the currently focused browser tab.
    stream_id: Option<String>,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Interval {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl Interval {
    fn duration_ms(&self) -> i64 {
        (self.end - self.start).num_milliseconds()
    }
}

/// Per-stream accumulator.
///
/// Direct time is derived from `focus_intervals` as a union (attention cannot be in two
/// places at once), so there is no running direct total. Delegated time is a running sum:
/// parallel agents legitimately exceed wall clock (`specs/design/core-concepts.md:113`).
#[derive(Default)]
struct StreamAllocation {
    delegated_ms: i64,
    focus_intervals: Vec<Interval>,
    delegated_intervals: Vec<Interval>,
}

/// The allocation pass, fed one event at a time.
///
/// [`allocate_time`] has always been a single forward walk over timestamp-sorted events
/// with its state in maps — it never indexed the slice or looked backwards. Taking
/// `&[E]` was therefore a property of the signature and not of the algorithm, and it was
/// an expensive one: `tt recompute` held all 2,738,805 events to run it, peaking at
/// 8.9 GB where the equivalent `SQLite` aggregate uses 6.4 MB.
///
/// Splitting the walk from its input lets a caller stream rows straight from `SQLite`.
/// Memory becomes a function of concurrently-open sessions, streams, and recorded
/// intervals rather than of history length.
///
/// Events must still arrive in ascending timestamp order. That requirement is the
/// algorithm's, not the container's, and pushing them out of order silently produces
/// wrong time exactly as passing an unsorted slice always did.
///
/// # Examples
///
/// ```
/// use std::collections::HashMap;
/// use tt_core::{AllocationConfig, Allocator};
///
/// let config = AllocationConfig::default();
/// let session_end_times = HashMap::new();
/// let session_types = HashMap::new();
/// let result = Allocator::new(&config, None, &session_end_times, &session_types).finish();
///
/// assert_eq!(result.total_tracked_ms, 0);
/// ```
pub struct Allocator<'a> {
    config: &'a AllocationConfig,
    period_end: Option<DateTime<Utc>>,
    session_end_times: &'a HashMap<String, DateTime<Utc>>,
    session_types: &'a HashMap<String, SessionType>,
    focus_state: FocusState,
    window_focus_state: WindowFocusState,
    browser_focus_state: BrowserFocusState,
    tmux_focus_stream_id: Option<String>,
    agent_sessions: HashMap<String, AgentSession>,
    stream_times: HashMap<String, StreamAllocation>,
    activity_intervals: Vec<Interval>,
    last_event_time: Option<DateTime<Utc>>,
}

impl<'a> Allocator<'a> {
    /// Starts a pass with no events seen yet.
    #[must_use]
    pub fn new(
        config: &'a AllocationConfig,
        period_end: Option<DateTime<Utc>>,
        session_end_times: &'a HashMap<String, DateTime<Utc>>,
        session_types: &'a HashMap<String, SessionType>,
    ) -> Self {
        Self {
            config,
            period_end,
            session_end_times,
            session_types,
            focus_state: FocusState::Unfocused,
            window_focus_state: WindowFocusState::default(),
            browser_focus_state: BrowserFocusState::default(),
            tmux_focus_stream_id: None,
            agent_sessions: HashMap::new(),
            stream_times: HashMap::new(),
            activity_intervals: Vec::new(),
            last_event_time: None,
        }
    }

    /// Helper to add direct time. Only the interval is recorded; the total is unioned at
    /// the end so overlapping observations (e.g. two machines) are not double-counted.
    fn add_direct(
        stream_id: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        intervals: &mut Vec<Interval>,
        times: &mut HashMap<String, StreamAllocation>,
    ) {
        if end > start {
            let interval = Interval { start, end };
            let allocation = times.entry(stream_id.to_string()).or_default();
            allocation.focus_intervals.push(interval);
            intervals.push(interval);
        }
    }

    /// Helper to add delegated time.
    fn add_delegated(
        stream_id: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        intervals: &mut Vec<Interval>,
        times: &mut HashMap<String, StreamAllocation>,
    ) {
        if end > start {
            let duration_ms = (end - start).num_milliseconds();
            let interval = Interval { start, end };
            let allocation = times.entry(stream_id.to_string()).or_default();
            allocation.delegated_ms += duration_ms;
            allocation.delegated_intervals.push(interval);
            intervals.push(interval);
        }
    }

    /// Processes one timestamp-ordered event.
    #[expect(
        clippy::too_many_lines,
        reason = "push mechanically preserves the existing single-pass allocation rules"
    )]
    pub fn push<E: AllocatableEvent>(&mut self, event: &E) {
        let event_time = event.timestamp();
        let event_type = event.event_type();
        let data = event.data();

        // Check for agent timeouts before processing this event.
        // If a session has a known end_time (from agent_sessions table), use it.
        // Otherwise, fall back to the timeout heuristic.
        let timeout_attributions: Vec<_> = self
            .agent_sessions
            .iter()
            .filter(|(_, session)| !session.ended)
            .filter_map(|(session_id, session)| {
                let last_tool = session.last_tool_use_at?;
                let first_tool = session.first_tool_use_at?;

                // Use known end_time if available, otherwise timeout heuristic
                if let Some(&known_end) = self.session_end_times.get(session_id) {
                    if event_time > known_end {
                        Some((
                            session_id.clone(),
                            session.stream_id.clone(),
                            first_tool,
                            known_end,
                        ))
                    } else {
                        None
                    }
                } else {
                    let timeout_at =
                        last_tool + Duration::milliseconds(self.config.agent_timeout_ms);
                    if event_time > timeout_at {
                        Some((
                            session_id.clone(),
                            session.stream_id.clone(),
                            first_tool,
                            timeout_at,
                        ))
                    } else {
                        None
                    }
                }
            })
            .collect();

        for (session_id, stream_id, first_tool, timeout_at) in timeout_attributions {
            // Attribute delegated time from first tool use to timeout
            Self::add_delegated(
                &stream_id,
                first_tool,
                timeout_at,
                &mut self.activity_intervals,
                &mut self.stream_times,
            );
            // Mark session as ended
            if let Some(session) = self.agent_sessions.get_mut(&session_id) {
                session.ended = true;
            }
        }

        match event_type {
            EventType::TmuxPaneFocus => {
                let stream_id = event.stream_id().unwrap_or(UNASSIGNED_STREAM_ID);
                {
                    // Close previous focus interval using resolved stream
                    if let FocusState::Focused { focus_start, .. } = &self.focus_state {
                        let resolved = resolve_focus_stream(
                            &self.window_focus_state,
                            self.tmux_focus_stream_id.as_deref(),
                            self.browser_focus_state.stream_id.as_deref(),
                        );
                        if let Some(resolved_stream) = &resolved {
                            let max_end = *focus_start
                                + Duration::milliseconds(self.config.attention_window_ms);
                            let actual_end = event_time.min(max_end);
                            Self::add_direct(
                                resolved_stream,
                                *focus_start,
                                actual_end,
                                &mut self.activity_intervals,
                                &mut self.stream_times,
                            );
                        }
                    }

                    self.tmux_focus_stream_id = Some(stream_id.to_string());
                    self.window_focus_state.app = None;
                    self.window_focus_state.stream_id = None;
                    self.focus_state = FocusState::Focused {
                        stream_id: stream_id.to_string(),
                        focus_start: event_time,
                    };
                }
            }

            EventType::AfkChange => {
                let status = data.get("status").and_then(|v| v.as_str()).unwrap_or("");
                if status == "idle" {
                    // Check for retroactive idle duration
                    let idle_start = data
                        .get("idle_duration_ms")
                        .and_then(serde_json::Value::as_i64)
                        .filter(|&ms| ms > 0)
                        .map_or(event_time, |ms| event_time - Duration::milliseconds(ms));

                    // Close focus at idle_start, not event_time
                    if let FocusState::Focused { focus_start, .. } = &self.focus_state {
                        let end_time = idle_start.max(*focus_start); // Don't go before focus started
                        if end_time > *focus_start {
                            let resolved = resolve_focus_stream(
                                &self.window_focus_state,
                                self.tmux_focus_stream_id.as_deref(),
                                self.browser_focus_state.stream_id.as_deref(),
                            );
                            if let Some(resolved_stream) = &resolved {
                                let max_end = *focus_start
                                    + Duration::milliseconds(self.config.attention_window_ms);
                                let actual_end = end_time.min(max_end);
                                Self::add_direct(
                                    resolved_stream,
                                    *focus_start,
                                    actual_end, // Use calculated idle_start, not event_time
                                    &mut self.activity_intervals,
                                    &mut self.stream_times,
                                );
                            }
                        }
                    }
                    self.focus_state = FocusState::Unfocused;
                }
                // Note: "active" does NOT restore focus - wait for next focus event
            }

            EventType::TmuxScroll => {
                // Scroll confirms focus and resets attention window, but only if
                // the event is for the currently focused stream (using resolved stream)
                if let FocusState::Focused {
                    stream_id: focused_stream,
                    focus_start,
                } = &self.focus_state
                {
                    // Resolve which stream should actually get the time
                    let resolved = resolve_focus_stream(
                        &self.window_focus_state,
                        self.tmux_focus_stream_id.as_deref(),
                        self.browser_focus_state.stream_id.as_deref(),
                    );
                    // Reset the attention window if this scroll belongs to the
                    // focused pane. The tmux hook emits scroll events with no stream
                    // of their own, so an unassigned scroll counts toward the
                    // currently focused stream; a scroll tagged to a DIFFERENT stream
                    // is ignored.
                    let event_stream = event.stream_id();
                    if let Some(resolved_stream) = &resolved {
                        if event_stream.is_none() || event_stream == Some(resolved_stream.as_str())
                        {
                            if event_time > *focus_start {
                                let max_end = *focus_start
                                    + Duration::milliseconds(self.config.attention_window_ms);
                                let actual_end = event_time.min(max_end);
                                Self::add_direct(
                                    resolved_stream,
                                    *focus_start,
                                    actual_end,
                                    &mut self.activity_intervals,
                                    &mut self.stream_times,
                                );
                            }
                            self.focus_state = FocusState::Focused {
                                stream_id: focused_stream.clone(),
                                focus_start: event_time,
                            };
                        }
                    }
                    // If event is for a different stream, ignore it - doesn't affect focus state
                }
            }

            EventType::UserMessage => {
                // User messages represent active work — sending a message to an
                // agent IS direct work. Establish focus on the message's stream,
                // just like switching to a tmux pane. Exception: user_message events
                // emitted by a subagent (or any non-User session type) reflect the
                // parent agent's delegation, not human attention, so they are skipped.
                let is_subagent_message = event
                    .session_id()
                    .and_then(|session_id| self.session_types.get(session_id))
                    .is_some_and(|session_type| *session_type != SessionType::User);
                if is_subagent_message {
                    return;
                }
                let stream_id = event.stream_id().unwrap_or(UNASSIGNED_STREAM_ID);
                {
                    // Close previous focus interval
                    if let FocusState::Focused { focus_start, .. } = &self.focus_state {
                        let resolved = resolve_focus_stream(
                            &self.window_focus_state,
                            self.tmux_focus_stream_id.as_deref(),
                            self.browser_focus_state.stream_id.as_deref(),
                        );
                        if let Some(resolved_stream) = &resolved {
                            let max_end = *focus_start
                                + Duration::milliseconds(self.config.attention_window_ms);
                            let actual_end = event_time.min(max_end);
                            Self::add_direct(
                                resolved_stream,
                                *focus_start,
                                actual_end,
                                &mut self.activity_intervals,
                                &mut self.stream_times,
                            );
                        }
                    }

                    self.tmux_focus_stream_id = Some(stream_id.to_string());
                    self.window_focus_state.app = None;
                    self.window_focus_state.stream_id = None;
                    self.focus_state = FocusState::Focused {
                        stream_id: stream_id.to_string(),
                        focus_start: event_time,
                    };
                }
            }

            EventType::AgentSession => {
                let action = event.action().unwrap_or("");
                let session_id = event.session_id().unwrap_or("");

                match action {
                    "started" => {
                        let stream_id = event.stream_id().unwrap_or(UNASSIGNED_STREAM_ID);
                        // One agent session is recorded several times: the machine holding
                        // the transcript derives it (`source = opencode`, carrying the
                        // stream once classified), and every machine that syncs it in adds
                        // its own copy (`source = remote.agent`, carrying no stream, because
                        // attribution happens locally after the sync). Their ids differ in
                        // shape, so `INSERT OR IGNORE` cannot collapse them, and all of them
                        // share the session's start timestamp.
                        //
                        // Re-inserting on the later copy therefore discarded the attribution
                        // the earlier one carried, and the session's whole delegated span
                        // fell to unassigned. Live, 12,474 sessions are shadowed this way,
                        // leaving 287 streams reporting zero delegated time while holding
                        // 295,361 agent_tool_use events between them.
                        //
                        // A duplicate must not take information away. An unassigned copy
                        // never overwrites a stream already known for the session; anything
                        // that does name a stream still updates it, so a genuine
                        // re-attribution is unaffected.
                        let shadows_known_stream = stream_id == UNASSIGNED_STREAM_ID
                            && self
                                .agent_sessions
                                .get(session_id)
                                .is_some_and(|tracked| tracked.stream_id != UNASSIGNED_STREAM_ID);
                        if !shadows_known_stream {
                            self.agent_sessions.insert(
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
                        if let Some(session) = self.agent_sessions.get(session_id) {
                            if !session.ended {
                                if let Some(first_tool) = session.first_tool_use_at {
                                    // Attribute from first tool use to end
                                    Self::add_delegated(
                                        &session.stream_id.clone(),
                                        first_tool,
                                        event_time,
                                        &mut self.activity_intervals,
                                        &mut self.stream_times,
                                    );
                                }
                            }
                        }
                        if let Some(session) = self.agent_sessions.get_mut(session_id) {
                            session.ended = true;
                        }
                    }
                    _ => {}
                }
            }

            EventType::AgentToolUse => {
                let session_id = event.session_id().unwrap_or("");
                if let Some(session) = self.agent_sessions.get_mut(session_id) {
                    if !session.ended {
                        if session.first_tool_use_at.is_none() {
                            // First tool use - delegated time starts here
                            session.first_tool_use_at = Some(event_time);
                        }
                        session.last_tool_use_at = Some(event_time);
                    }
                }
            }

            EventType::WindowFocus => {
                let app = data
                    .get("app")
                    .and_then(|v| v.as_str())
                    .map(str::to_ascii_lowercase);

                if let FocusState::Focused { focus_start, .. } = &self.focus_state {
                    let resolved = resolve_focus_stream(
                        &self.window_focus_state,
                        self.tmux_focus_stream_id.as_deref(),
                        self.browser_focus_state.stream_id.as_deref(),
                    );
                    if let Some(resolved_stream) = &resolved {
                        let max_end =
                            *focus_start + Duration::milliseconds(self.config.attention_window_ms);
                        let actual_end = event_time.min(max_end);
                        Self::add_direct(
                            resolved_stream,
                            *focus_start,
                            actual_end,
                            &mut self.activity_intervals,
                            &mut self.stream_times,
                        );
                    }
                }

                self.window_focus_state.app = app;
                self.window_focus_state.stream_id = event.stream_id().map(String::from);

                if let Some(stream_id) = resolve_focus_stream(
                    &self.window_focus_state,
                    self.tmux_focus_stream_id.as_deref(),
                    self.browser_focus_state.stream_id.as_deref(),
                ) {
                    self.focus_state = FocusState::Focused {
                        stream_id,
                        focus_start: event_time,
                    };
                } else {
                    self.focus_state = FocusState::Unfocused;
                }
            }

            EventType::BrowserTab => {
                // If we're in a browser app and have focus, update focus state
                if self
                    .window_focus_state
                    .app
                    .as_ref()
                    .is_some_and(|app| is_browser_app(app))
                {
                    let stream_id = event.stream_id().unwrap_or(UNASSIGNED_STREAM_ID);
                    {
                        // Close previous focus interval
                        if let FocusState::Focused { focus_start, .. } = &self.focus_state {
                            let resolved = resolve_focus_stream(
                                &self.window_focus_state,
                                self.tmux_focus_stream_id.as_deref(),
                                self.browser_focus_state.stream_id.as_deref(),
                            );
                            if let Some(resolved_stream) = &resolved {
                                let max_end = *focus_start
                                    + Duration::milliseconds(self.config.attention_window_ms);
                                let actual_end = event_time.min(max_end);
                                Self::add_direct(
                                    resolved_stream,
                                    *focus_start,
                                    actual_end,
                                    &mut self.activity_intervals,
                                    &mut self.stream_times,
                                );
                            }
                        }

                        self.focus_state = FocusState::Focused {
                            stream_id: stream_id.to_string(),
                            focus_start: event_time,
                        };
                    }
                }

                self.browser_focus_state.stream_id = Some(
                    event
                        .stream_id()
                        .unwrap_or(UNASSIGNED_STREAM_ID)
                        .to_string(),
                );
            }
        }

        self.last_event_time = Some(event_time);
    }

    /// Finalizes allocation after every event has been processed.
    #[must_use]
    pub fn finish(mut self) -> AllocationResult {
        // Finalize: close open intervals
        let end_time = self.period_end.or(self.last_event_time);

        if let Some(end) = end_time {
            // Close focus - cap at attention window, using resolved stream
            if let FocusState::Focused { focus_start, .. } = &self.focus_state {
                let resolved = resolve_focus_stream(
                    &self.window_focus_state,
                    self.tmux_focus_stream_id.as_deref(),
                    self.browser_focus_state.stream_id.as_deref(),
                );
                if let Some(resolved_stream) = &resolved {
                    let window_end =
                        *focus_start + Duration::milliseconds(self.config.attention_window_ms);
                    let actual_end = self.period_end.map_or(window_end, |pe| pe.min(window_end));
                    if actual_end > *focus_start {
                        Self::add_direct(
                            resolved_stream,
                            *focus_start,
                            actual_end,
                            &mut self.activity_intervals,
                            &mut self.stream_times,
                        );
                    }
                }
            }

            // Close active agent sessions.
            // Use known end_time when available, otherwise timeout heuristic.
            let final_attributions: Vec<_> = self
                .agent_sessions
                .iter()
                .filter(|(_, session)| !session.ended)
                .filter_map(|(session_id, session)| {
                    let first_tool = session.first_tool_use_at?;
                    let last_tool = session.last_tool_use_at.unwrap_or(first_tool);

                    let session_end =
                        if let Some(&known_end) = self.session_end_times.get(session_id) {
                            // Use known end_time, capped at period end
                            known_end.min(end)
                        } else {
                            // Timeout heuristic: last_tool + timeout, capped at period end
                            let timeout_at =
                                last_tool + Duration::milliseconds(self.config.agent_timeout_ms);
                            if end > timeout_at { timeout_at } else { end }
                        };

                    Some((session.stream_id.clone(), first_tool, session_end))
                })
                .collect();

            for (stream_id, first_tool, session_end) in final_attributions {
                if session_end > first_tool {
                    Self::add_delegated(
                        &stream_id,
                        first_tool,
                        session_end,
                        &mut self.activity_intervals,
                        &mut self.stream_times,
                    );
                }
            }
        }

        // Calculate total tracked time from interval union
        let total_tracked_ms = union_duration_ms(&self.activity_intervals);

        let unassigned = self
            .stream_times
            .remove(UNASSIGNED_STREAM_ID)
            .unwrap_or_default();

        let stream_times_vec = self
            .stream_times
            .into_iter()
            .map(|(stream_id, allocation)| StreamTime {
                stream_id,
                time_direct_ms: union_duration_ms(&allocation.focus_intervals),
                time_delegated_ms: allocation.delegated_ms,
                focus_intervals: allocation.focus_intervals,
                delegated_intervals: allocation.delegated_intervals,
            })
            .collect();

        AllocationResult {
            stream_times: stream_times_vec,
            total_tracked_ms,
            unassigned_direct_ms: union_duration_ms(&unassigned.focus_intervals),
            unassigned_delegated_ms: unassigned.delegated_ms,
        }
    }
}

/// Calculate time allocation for a time range.
///
/// Events must be sorted by timestamp ascending.
/// Events with `stream_id = None` are attributed to a synthetic unassigned bucket
/// (surfaced via `unassigned_direct_ms` / `unassigned_delegated_ms`) instead of being
/// silently dropped. The sentinel is removed from `stream_times` before returning.
///
/// # Arguments
///
/// * `events` - Events to process (must implement `AllocatableEvent`)
/// * `config` - Allocation configuration
/// * `period_end` - Where to close open intervals. If None, uses last event + `attention_window`
/// * `session_end_times` - Known end times for agent sessions (from `agent_sessions` table).
///   When a session has a known `end_time`, the algorithm uses it instead of the timeout heuristic.
/// * `session_types` - Known session types (from `agent_sessions` table). `UserMessage` events
///   originating from non-`User` sessions (e.g. subagents) are skipped so they do not establish
///   focus.
///
/// # Returns
///
/// Computed time per stream and total tracked time.
#[expect(
    clippy::implicit_hasher,
    reason = "callers hold the maps tt-db builds with the default hasher"
)]
#[expect(
    clippy::disallowed_methods,
    reason = "allocate_time is the in-crate wrapper over the incremental pass"
)]
pub fn allocate_time<E: AllocatableEvent>(
    events: &[E],
    config: &AllocationConfig,
    period_end: Option<DateTime<Utc>>,
    session_end_times: &HashMap<String, DateTime<Utc>>,
    session_types: &HashMap<String, SessionType>,
) -> AllocationResult {
    let mut allocator = Allocator::new(config, period_end, session_end_times, session_types);
    for event in events {
        allocator.push(event);
    }
    allocator.finish()
}

/// Total duration covered by the union of `intervals`, merging any overlap.
fn union_duration_ms(intervals: &[Interval]) -> i64 {
    if intervals.is_empty() {
        return 0;
    }

    // Filter out invalid intervals (where end <= start) and sort by start time
    let mut sorted: Vec<Interval> = intervals
        .iter()
        .filter(|i| i.end > i.start)
        .copied()
        .collect();
    if sorted.is_empty() {
        return 0;
    }
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

/// Returns true if the app name indicates a terminal application.
fn is_terminal_app(app: &str) -> bool {
    let app_lower = app.to_ascii_lowercase();
    app_lower.contains("terminal")
        || app_lower.contains("iterm")
        || app_lower.contains("alacritty")
        || app_lower.contains("wezterm")
        || app_lower.contains("kitty")
        || app_lower.contains("konsole")
        || app_lower.contains("gnome-terminal")
        || app_lower.contains("ghostty")
}

/// Returns true if the app name indicates a browser application.
fn is_browser_app(app: &str) -> bool {
    let app_lower = app.to_ascii_lowercase();
    app_lower.contains("chrome")
        || app_lower.contains("firefox")
        || app_lower.contains("safari")
        || app_lower.contains("edge")
        || app_lower.contains("brave")
        || app_lower.contains("arc")
}

/// Resolves which stream should receive direct time based on focus hierarchy.
///
/// Hierarchy:
/// - If window is a terminal app -> use tmux focus stream
/// - If window is a browser app -> browser tab stream, else the window's own stream,
///   else the UNASSIGNED bucket (active GUI time is never dropped to nothing)
/// - Otherwise (non-terminal GUI) -> the window's own stream, else UNASSIGNED
fn resolve_focus_stream(
    window_state: &WindowFocusState,
    tmux_stream_id: Option<&str>,
    browser_stream_id: Option<&str>,
) -> Option<String> {
    match &window_state.app {
        Some(app) if is_terminal_app(app) => tmux_stream_id.map(String::from),
        Some(app) if is_browser_app(app) => Some(
            browser_stream_id
                .or(window_state.stream_id.as_deref())
                .unwrap_or(UNASSIGNED_STREAM_ID)
                .to_string(),
        ),
        Some(_) => Some(
            window_state
                .stream_id
                .as_deref()
                .unwrap_or(UNASSIGNED_STREAM_ID)
                .to_string(),
        ),
        None => tmux_stream_id.map(String::from), // Fallback to tmux if no window info
    }
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "tt-core tests exercise the core algorithm directly"
)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    #[test]
    fn incremental_allocation_matches_the_hardcoded_oracle() {
        // Given: a corpus exercising focus, agent work, and an unassigned stretch.
        let events = vec![
            TestEvent::tmux_focus(ts(0), "stream-a"),
            TestEvent::agent_session(ts(1), "started", "session-1", Some("stream-a")),
            TestEvent::agent_tool_use(ts(2), "session-1", "stream-a"),
            TestEvent::agent_tool_use(ts(30), "session-1", "stream-a"),
            TestEvent::tmux_focus_unassigned(ts(60)),
            TestEvent::agent_session(ts(61), "ended", "session-1", Some("stream-a")),
        ];
        let config = test_config();
        let end_times = HashMap::new();
        let types = HashMap::new();

        // When: the incremental path runs.
        let mut allocator = Allocator::new(&config, None, &end_times, &types);
        for event in &events {
            allocator.push(event);
        }
        let from_pushes = allocator.finish();

        // Then: focus intervals union while delegated intervals sum.
        assert_eq!(from_pushes.total_tracked_ms, 3_600_000);
        assert_eq!(from_pushes.unassigned_direct_ms, 60_000);
        assert_eq!(from_pushes.unassigned_delegated_ms, 0);
        assert_eq!(
            from_pushes.stream_times,
            vec![StreamTime {
                stream_id: "stream-a".to_string(),
                time_direct_ms: 60_000,
                time_delegated_ms: 3_480_000,
                focus_intervals: vec![Interval {
                    start: ts(0),
                    end: ts(1),
                }],
                delegated_intervals: vec![Interval {
                    start: ts(2),
                    end: ts(60),
                }],
            }]
        );
    }

    fn test_config() -> AllocationConfig {
        AllocationConfig {
            attention_window_ms: 60_000,
            agent_timeout_ms: 1_800_000,
        }
    }

    /// Test event implementation.
    struct TestEvent {
        timestamp: DateTime<Utc>,
        event_type: EventType,
        stream_id: Option<String>,
        session_id: Option<String>,
        action: Option<String>,
        data: serde_json::Value,
    }

    impl TestEvent {
        fn tmux_focus(ts: DateTime<Utc>, stream_id: &str) -> Self {
            Self {
                timestamp: ts,
                event_type: EventType::TmuxPaneFocus,
                stream_id: Some(stream_id.to_string()),
                session_id: None,
                action: None,
                data: json!({"pane_id": "%1", "cwd": "/test"}),
            }
        }

        fn tmux_focus_unassigned(ts: DateTime<Utc>) -> Self {
            Self {
                timestamp: ts,
                event_type: EventType::TmuxPaneFocus,
                stream_id: None,
                session_id: None,
                action: None,
                data: json!({"pane_id": "%1", "cwd": "/test"}),
            }
        }

        fn afk_change(ts: DateTime<Utc>, status: &str) -> Self {
            Self {
                timestamp: ts,
                event_type: EventType::AfkChange,
                stream_id: None,
                session_id: None,
                action: None,
                data: json!({"status": status}),
            }
        }

        fn tmux_scroll(ts: DateTime<Utc>, stream_id: &str) -> Self {
            Self {
                timestamp: ts,
                event_type: EventType::TmuxScroll,
                stream_id: Some(stream_id.to_string()),
                session_id: None,
                action: None,
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
                event_type: EventType::AgentSession,
                stream_id: stream_id.map(String::from),
                session_id: Some(session_id.to_string()),
                action: Some(action.to_string()),
                data: json!({"agent": "claude-code"}),
            }
        }

        fn agent_tool_use(ts: DateTime<Utc>, session_id: &str, stream_id: &str) -> Self {
            Self {
                timestamp: ts,
                event_type: EventType::AgentToolUse,
                stream_id: Some(stream_id.to_string()),
                session_id: Some(session_id.to_string()),
                action: None,
                data: json!({"tool": "Edit"}),
            }
        }

        fn user_message(ts: DateTime<Utc>, session_id: &str, stream_id: &str) -> Self {
            Self {
                timestamp: ts,
                event_type: EventType::UserMessage,
                stream_id: Some(stream_id.to_string()),
                session_id: Some(session_id.to_string()),
                action: None,
                data: json!({"length": 100}),
            }
        }

        fn window_focus(ts: DateTime<Utc>, app: &str, stream_id: Option<&str>) -> Self {
            Self {
                timestamp: ts,
                event_type: EventType::WindowFocus,
                stream_id: stream_id.map(String::from),
                session_id: None,
                action: None,
                data: json!({"app": app, "title": "test window"}),
            }
        }

        fn browser_tab(ts: DateTime<Utc>, stream_id: &str) -> Self {
            Self {
                timestamp: ts,
                event_type: EventType::BrowserTab,
                stream_id: Some(stream_id.to_string()),
                session_id: None,
                action: None,
                data: json!({"url": "https://example.com", "title": "Test Page"}),
            }
        }

        fn afk_with_duration(ts: DateTime<Utc>, status: &str, idle_duration_ms: i64) -> Self {
            Self {
                timestamp: ts,
                event_type: EventType::AfkChange,
                stream_id: None,
                session_id: None,
                action: None,
                data: json!({"status": status, "idle_duration_ms": idle_duration_ms}),
            }
        }
    }

    impl AllocatableEvent for TestEvent {
        fn timestamp(&self) -> DateTime<Utc> {
            self.timestamp
        }

        fn event_type(&self) -> EventType {
            self.event_type
        }

        fn stream_id(&self) -> Option<&str> {
            self.stream_id.as_deref()
        }

        fn session_id(&self) -> Option<&str> {
            self.session_id.as_deref()
        }

        fn action(&self) -> Option<&str> {
            self.action.as_deref()
        }

        fn data(&self) -> &serde_json::Value {
            &self.data
        }
    }

    fn ts(minutes: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2025, 1, 15, 9, 0, 0)
            .single()
            .expect("valid test timestamp")
            + Duration::minutes(minutes)
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

        let config = test_config();
        // Set period_end to cap the final attention window
        let result = allocate_time(
            &events,
            &config,
            Some(ts(11)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // Direct time capped per attention window:
        // 0->1 (focus to scroll), 5->6 (scroll to scroll), 10->11 (scroll to period_end)
        // Total: 3 minutes
        assert_eq!(stream_a.time_direct_ms, 3 * 60 * 1000);
    }

    // Test 2: Focus switches between streams
    #[test]
    fn test_focus_switches_between_streams() {
        let events = vec![
            TestEvent::tmux_focus(ts(0), "A"),
            TestEvent::tmux_focus(ts(10), "B"),
        ];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(20)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        let stream_b = get_stream_time(&result, "B").expect("Stream B should exist");

        // Stream A: 0 to min(10, 0+1) = 1 minute (attention window)
        assert_eq!(stream_a.time_direct_ms, 60 * 1000);
        // Stream B: 10 to min(20, 10+1) = 10 to 11 = 1 minute (attention window)
        assert_eq!(stream_b.time_direct_ms, 60 * 1000);
    }

    // Test 3: AFK pauses direct time
    #[test]
    fn test_afk_pauses_direct_time() {
        let events = vec![
            TestEvent::tmux_focus(ts(0), "A"),
            TestEvent::afk_change(ts(10), "idle"),
            TestEvent::afk_change(ts(15), "active"),
        ];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(20)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // Direct time capped at attention window before AFK: 1 minute
        assert_eq!(stream_a.time_direct_ms, 60 * 1000);
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

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(20)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // Direct time capped at attention window: 1 minute
        assert_eq!(stream_a.time_direct_ms, 60 * 1000);
    }

    // Test 5: Single agent session
    #[test]
    fn test_single_agent_session() {
        let events = vec![
            TestEvent::agent_session(ts(0), "started", "sess1", Some("A")),
            TestEvent::agent_tool_use(ts(5), "sess1", "A"),
            TestEvent::agent_session(ts(30), "ended", "sess1", Some("A")),
        ];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(30)),
            &HashMap::new(),
            &HashMap::new(),
        );

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

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(30)),
            &HashMap::new(),
            &HashMap::new(),
        );

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

        let config = AllocationConfig {
            agent_timeout_ms: 30 * 60 * 1000, // 30 minutes
            ..Default::default()
        };

        let result = allocate_time(
            &events,
            &config,
            Some(ts(60)),
            &HashMap::new(),
            &HashMap::new(),
        );

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

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(30)),
            &HashMap::new(),
            &HashMap::new(),
        );

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

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(30)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");

        // Focus: from 0 to min(30, 0+1) = 1 minute (attention window)
        // Delegated: 5 to 30 = 25 minutes
        assert_eq!(stream_a.time_direct_ms, 60 * 1000);
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
        let result = allocate_time(
            &events,
            &config,
            Some(ts(10)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // Direct time caps at attention window: 1 minute
        assert_eq!(stream_a.time_direct_ms, 60 * 1000);
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
        let result = allocate_time(
            &events,
            &config,
            Some(ts(10)),
            &HashMap::new(),
            &HashMap::new(),
        );

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

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(10)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // Focus is on A the whole time. The scroll in B doesn't change focus state.
        // Scroll events only reset attention window if we're already focused on that stream.
        // Direct time for A: from 0 to min(10, 0+1) = 1 minute
        assert_eq!(stream_a.time_direct_ms, 60 * 1000);

        // Stream B gets no direct time (no focus on B)
        let stream_b = get_stream_time(&result, "B");
        assert!(stream_b.is_none() || stream_b.unwrap().time_direct_ms == 0);
    }

    // Test 13: Switching focus to an unassigned pane must stop crediting the previously
    // focused real stream; the remaining time goes to the unassigned bucket instead of
    // being silently dropped (regression guard for the silent-drop bug).
    #[test]
    fn test_focus_switch_to_unassigned_stops_crediting_real_stream() {
        let events = vec![
            TestEvent::tmux_focus(ts(0), "A"),
            TestEvent {
                timestamp: ts(0) + Duration::seconds(30),
                event_type: EventType::TmuxPaneFocus,
                stream_id: None,
                session_id: None,
                action: None,
                data: json!({"pane_id": "%2", "cwd": "/test"}),
            },
        ];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(5)),
            &HashMap::new(),
            &HashMap::new(),
        );

        // A focused at 0; the unassigned focus at 0:30 closes A's interval at 0:30 = 30s.
        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        assert_eq!(stream_a.time_direct_ms, 30_000);
        // Unassigned focus from 0:30, capped at the 60s attention window = 60s.
        assert_eq!(result.unassigned_direct_ms, 60_000);
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

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(30)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");

        // Direct: from 0 to min(10, 0+1) = 1 minute (attention window)
        // Delegated: from 5 to 30 = 25 minutes
        assert_eq!(stream_a.time_direct_ms, 60 * 1000);
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

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(20)),
            &HashMap::new(),
            &HashMap::new(),
        );

        // Direct: [0, 1) = 1 min (attention window)
        // Delegated: [5, 20) = 15 min
        // Union: [0, 1) + [5, 20) = 16 min
        assert_eq!(result.total_tracked_ms, 16 * 60 * 1000);
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

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(20)),
            &HashMap::new(),
            &HashMap::new(),
        );

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
        let result = allocate_time(
            &events,
            &config,
            Some(ts(5)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // Focus at 0, user_message at 30s, attention window extends to 1:30
        // But period_end is at 5 min, so finalize uses min(5, 0:30 + 1:00) = 1:30
        assert_eq!(stream_a.time_direct_ms, 90 * 1000);
    }

    // Test: Empty events
    #[test]
    fn test_empty_events() {
        let events: Vec<TestEvent> = vec![];
        let config = test_config();
        let result = allocate_time(&events, &config, None, &HashMap::new(), &HashMap::new());

        assert!(result.stream_times.is_empty());
        assert_eq!(result.total_tracked_ms, 0);
    }

    #[test]
    fn test_window_focus_sets_active_window() {
        let events = vec![
            TestEvent::window_focus(ts(0), "Terminal", Some("A")),
            TestEvent::tmux_focus(ts(0), "A"),
            TestEvent::tmux_focus(ts(10), "A"), // Activity to close interval
        ];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(10)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // Window focus + tmux focus on same stream = 1 minute (attention window)
        assert_eq!(stream_a.time_direct_ms, 60 * 1000);
    }

    #[test]
    fn test_ghostty_window_focus_resolves_to_tmux_stream() {
        // Regression: the user's terminal app id is "com.mitchellh.ghostty". Its
        // watcher window-focus events (which carry no stream of their own) must credit
        // the active tmux stream, not leak to the UNASSIGNED bucket.
        let events = vec![
            TestEvent::tmux_focus(ts(0), "A"),
            TestEvent::window_focus(ts(2), "com.mitchellh.ghostty", None),
            TestEvent::tmux_focus(ts(10), "A"),
        ];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(10)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        assert!(stream_a.time_direct_ms > 0);
        assert_eq!(result.unassigned_direct_ms, 0);
    }

    #[test]
    fn test_unassigned_scroll_refreshes_focused_tmux_stream() {
        // Regression: the tmux hook emits tmux_scroll events with no stream of their
        // own. Such a scroll must refresh the attention window on the currently
        // focused tmux stream (heads-down scrolling stays "active"), instead of being
        // ignored because its stream != the focused stream.
        let events = vec![
            TestEvent::tmux_focus(ts(0), "A"),
            TestEvent {
                timestamp: ts(4),
                event_type: EventType::TmuxScroll,
                stream_id: None,
                session_id: None,
                action: None,
                data: json!({"direction": "up"}),
            },
        ];

        let config = AllocationConfig {
            attention_window_ms: 5 * 60 * 1000,
            agent_timeout_ms: 30 * 60 * 1000,
        };
        let result = allocate_time(
            &events,
            &config,
            Some(ts(8)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // Scroll at 4min resets the window, so A accrues 0->4 plus 4->8 = 8 min;
        // without the reset it would cap at the single 0->5 window (5 min).
        assert!(stream_a.time_direct_ms > 5 * 60 * 1000);
    }

    #[test]
    fn test_window_focus_closes_prior_interval_before_updating_window_state() {
        let events = vec![
            TestEvent::tmux_focus(ts(0), "A"),
            TestEvent::window_focus(ts(10), "slack", Some("S")),
        ];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(11)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        let stream_s = get_stream_time(&result, "S").expect("Stream S should exist");

        assert_eq!(stream_a.time_direct_ms, 60_000);
        assert_eq!(stream_s.time_direct_ms, 60_000);
    }

    #[test]
    fn test_tmux_focus_after_gui_window_does_not_use_stale_window_stream_on_finalize() {
        let events = vec![
            TestEvent::window_focus(ts(0), "slack", Some("S")),
            TestEvent::tmux_focus(ts(5), "A"),
        ];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(10)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        let stream_s = get_stream_time(&result, "S").expect("Stream S should exist");

        assert_eq!(stream_a.time_direct_ms, 60_000);
        assert_eq!(stream_s.time_direct_ms, 60_000);
    }

    #[test]
    fn test_window_focus_accrues_direct_time_for_gui_app() {
        let events = vec![TestEvent::window_focus(ts(0), "slack", Some("S"))];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(1)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream_s = get_stream_time(&result, "S").expect("Stream S should exist");
        assert_eq!(stream_s.time_direct_ms, 60_000);
    }

    #[test]
    fn test_window_focus_browser_without_tab_falls_back_to_window_stream() {
        let events = vec![TestEvent::window_focus(ts(0), "firefox", Some("P"))];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(1)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream_p = get_stream_time(&result, "P").expect("Stream P should exist");
        assert_eq!(stream_p.time_direct_ms, 60_000);
    }

    #[test]
    fn test_browser_tab_tracks_stream() {
        let events = vec![
            TestEvent::browser_tab(ts(0), "B"),
            TestEvent::browser_tab(ts(10), "B"), // Activity to close interval
        ];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(10)),
            &HashMap::new(),
            &HashMap::new(),
        );

        // Browser tab alone doesn't grant direct time without window focus
        // This test verifies the event is parsed without error
        assert!(
            result.stream_times.is_empty()
                || get_stream_time(&result, "B").map_or(0, |s| s.time_direct_ms) == 0
        );
    }

    #[test]
    fn test_focus_hierarchy_terminal_uses_tmux_stream() {
        let events = vec![
            TestEvent::window_focus(ts(0), "Terminal", None), // Window focus, no stream
            TestEvent::tmux_focus(ts(0), "A"),                // Tmux focus on A
            TestEvent::tmux_scroll(ts(5), "A"),               // Activity
        ];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(6)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // Terminal window focus + tmux focus = time goes to tmux stream A, capped per window
        assert_eq!(stream_a.time_direct_ms, 2 * 60 * 1000);
    }

    #[test]
    fn test_focus_hierarchy_browser_uses_browser_stream() {
        let events = vec![
            TestEvent::window_focus(ts(0), "Chrome", None),
            TestEvent::browser_tab(ts(0), "B"),
            TestEvent::browser_tab(ts(5), "B"), // Activity
        ];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(6)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream_b = get_stream_time(&result, "B").expect("Stream B should exist");
        // Browser window focus + browser tab = time goes to browser stream B, capped per window
        assert_eq!(stream_b.time_direct_ms, 2 * 60 * 1000);
    }

    #[test]
    fn test_afk_idle_duration_retroactive() {
        // AFK event at 5 min reports user was idle for 3 minutes (since 2 min)
        let events = vec![
            TestEvent::tmux_focus(ts(0), "A"),
            TestEvent::afk_with_duration(ts(5), "idle", 180_000), // idle_duration_ms = 3 min
        ];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(5)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // Direct time capped at attention window: 1 minute
        assert_eq!(stream_a.time_direct_ms, 60 * 1000);
    }

    #[test]
    fn test_focus_switch_caps_gap_at_attention_window() {
        let events = vec![
            TestEvent::tmux_focus(ts(0), "stream-a"),
            TestEvent::tmux_focus(ts(120), "stream-b"), // 2 hours later
        ];
        let config = AllocationConfig {
            attention_window_ms: 60_000,
            ..Default::default()
        };
        let result = allocate_time(
            &events,
            &config,
            Some(ts(121)),
            &HashMap::new(),
            &HashMap::new(),
        );
        let stream_a = get_stream_time(&result, "stream-a").expect("stream-a should exist");
        assert_eq!(
            stream_a.time_direct_ms, 60_000,
            "stream-a should be capped to 1 min, not 120 min"
        );
        let stream_b = get_stream_time(&result, "stream-b").expect("stream-b should exist");
        assert_eq!(
            stream_b.time_direct_ms, 60_000,
            "stream-b capped at finalization"
        );
    }

    #[test]
    fn test_scroll_caps_gap_at_attention_window() {
        let events = vec![
            TestEvent::tmux_focus(ts(0), "stream-a"),
            TestEvent::tmux_scroll(ts(120), "stream-a"), // 2 hours later
        ];
        let config = AllocationConfig {
            attention_window_ms: 60_000,
            ..Default::default()
        };
        let result = allocate_time(
            &events,
            &config,
            Some(ts(121)),
            &HashMap::new(),
            &HashMap::new(),
        );
        let stream_a = get_stream_time(&result, "stream-a").expect("stream-a should exist");
        // First interval 0→1min capped, scroll resets window, second interval 120→121min = 60s
        assert_eq!(
            stream_a.time_direct_ms, 120_000,
            "60s capped + 60s finalization = 120s"
        );
    }

    #[test]
    fn test_afk_caps_gap_at_attention_window() {
        let events = vec![
            TestEvent::tmux_focus(ts(0), "stream-a"),
            TestEvent::afk_change(ts(120), "idle"), // 2 hours later
        ];
        let config = AllocationConfig {
            attention_window_ms: 60_000,
            ..Default::default()
        };
        let result = allocate_time(
            &events,
            &config,
            Some(ts(121)),
            &HashMap::new(),
            &HashMap::new(),
        );
        let stream_a = get_stream_time(&result, "stream-a").expect("stream-a should exist");
        assert_eq!(
            stream_a.time_direct_ms, 60_000,
            "should be capped to 1 min, not 120 min"
        );
    }

    #[test]
    fn test_browser_tab_switch_caps_gap_at_attention_window() {
        let events = vec![
            TestEvent::window_focus(ts(0), "firefox", Some("stream-a")),
            TestEvent::browser_tab(ts(0), "stream-a"),
            TestEvent::browser_tab(ts(120), "stream-b"), // 2 hours later
        ];
        let config = AllocationConfig {
            attention_window_ms: 60_000,
            ..Default::default()
        };
        let result = allocate_time(
            &events,
            &config,
            Some(ts(121)),
            &HashMap::new(),
            &HashMap::new(),
        );
        let stream_a = get_stream_time(&result, "stream-a").expect("stream-a should exist");
        assert_eq!(
            stream_a.time_direct_ms, 60_000,
            "previous tab gets capped 60s"
        );
        let stream_b = get_stream_time(&result, "stream-b").expect("stream-b should exist");
        assert_eq!(
            stream_b.time_direct_ms, 60_000,
            "new tab gets finalization 60s"
        );
    }

    #[test]
    fn test_scroll_within_window_not_capped() {
        let events = vec![
            TestEvent::tmux_focus(ts(0), "stream-a"),
            TestEvent::tmux_scroll(ts(0) + Duration::seconds(30), "stream-a"), // within window
            TestEvent::tmux_focus(ts(120), "stream-b"),
        ];
        let config = AllocationConfig {
            attention_window_ms: 60_000,
            ..Default::default()
        };
        let result = allocate_time(
            &events,
            &config,
            Some(ts(121)),
            &HashMap::new(),
            &HashMap::new(),
        );
        let stream_a = get_stream_time(&result, "stream-a").expect("stream-a should exist");
        // First interval: 0→30s (NOT capped, within window)
        // Second interval: 30s→30s+60s=90s (capped at attention window from scroll to next focus switch at ts(120))
        assert_eq!(
            stream_a.time_direct_ms, 90_000,
            "30s uncapped + 60s capped = 90s"
        );
    }

    #[test]
    fn test_multiple_focus_switches_with_gaps_all_capped() {
        let events = vec![
            TestEvent::tmux_focus(ts(0), "stream-a"),
            TestEvent::tmux_focus(ts(120), "stream-b"), // 2h gap
            TestEvent::tmux_focus(ts(240), "stream-c"), // another 2h gap
        ];
        let config = AllocationConfig {
            attention_window_ms: 60_000,
            ..Default::default()
        };
        let result = allocate_time(
            &events,
            &config,
            Some(ts(241)),
            &HashMap::new(),
            &HashMap::new(),
        );
        let stream_a = get_stream_time(&result, "stream-a").expect("stream-a should exist");
        let stream_b = get_stream_time(&result, "stream-b").expect("stream-b should exist");
        let stream_c = get_stream_time(&result, "stream-c").expect("stream-c should exist");
        assert_eq!(stream_a.time_direct_ms, 60_000, "stream-a capped");
        assert_eq!(stream_b.time_direct_ms, 60_000, "stream-b capped");
        assert_eq!(
            stream_c.time_direct_ms, 60_000,
            "stream-c capped at finalization"
        );
    }

    #[test]
    fn test_afk_retroactive_duration_caps_at_attention_window() {
        let events = vec![
            TestEvent::tmux_focus(ts(0), "stream-a"),
            TestEvent::afk_with_duration(ts(120), "idle", 30 * 60 * 1000), // idle 30min retroactively
                                                                           // idle_start = ts(120) - 30min = ts(90)
        ];
        let config = AllocationConfig {
            attention_window_ms: 60_000,
            ..Default::default()
        };
        let result = allocate_time(
            &events,
            &config,
            Some(ts(121)),
            &HashMap::new(),
            &HashMap::new(),
        );
        let stream_a = get_stream_time(&result, "stream-a").expect("stream-a should exist");
        // idle_start = ts(90), end_time = max(ts(90), ts(0)) = ts(90)
        // But capped: max_end = ts(0) + 60s = ts(1), capped_end = min(ts(90), ts(1)) = ts(1)
        assert_eq!(
            stream_a.time_direct_ms, 60_000,
            "capped at attention window"
        );
    }

    // Test: User message establishes focus when unfocused (no prior tmux focus)
    #[test]
    fn test_user_message_establishes_focus_when_unfocused() {
        let events = vec![
            TestEvent::agent_session(ts(0), "started", "sess1", Some("A")),
            TestEvent::user_message(ts(1), "sess1", "A"),
            TestEvent::user_message(ts(5), "sess1", "A"),
        ];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(6)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // First user_message at ts(1) establishes focus on A.
        // Second user_message at ts(5) closes interval [1, min(5, 1+1min)] = [1, 2] = 1 min.
        // Finalize: min(ts(6), ts(5) + 1min) = ts(6) → [5, 6] = 1 min.
        // Total: 1 + 1 = 2 min.
        assert_eq!(stream_a.time_direct_ms, 2 * 60 * 1000);
    }

    // Test: User message switches focus from one stream to another
    #[test]
    fn test_user_message_switches_focus_between_streams() {
        let events = vec![
            TestEvent::tmux_focus(ts(0), "A"),
            TestEvent::user_message(ts(3), "sess1", "B"),
        ];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(5)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        let stream_b = get_stream_time(&result, "B").expect("Stream B should exist");
        // Focus on A from ts(0), capped at attention window: min(ts(3), ts(0)+1min) = ts(1)
        // So A gets [0, 1] = 1 min.
        assert_eq!(stream_a.time_direct_ms, 60_000);
        // UserMessage at ts(3) establishes focus on B.
        // Finalize: min(ts(5), ts(3)+1min) = ts(4) → [3, 4] = 1 min.
        assert_eq!(stream_b.time_direct_ms, 60_000);
    }

    // Test: Sequence of user messages accumulates direct time
    #[test]
    fn test_user_message_sequence_accumulates_direct_time() {
        let events = vec![
            TestEvent::user_message(ts(0), "sess1", "A"),
            TestEvent::user_message(ts(2), "sess1", "A"),
            TestEvent::user_message(ts(4), "sess1", "A"),
            TestEvent::user_message(ts(6), "sess1", "A"),
        ];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(7)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // msg@0 establishes focus
        // msg@2 closes [0,1min]=1min (capped), reopens at 2
        // msg@4 closes [2,3min]=1min (capped), reopens at 4
        // msg@6 closes [4,5min]=1min (capped), reopens at 6
        // Finalize: min(7, 6+1min)=7 → [6,7]=1min
        // Total: 4 * 1min = 4 min
        assert_eq!(stream_a.time_direct_ms, 4 * 60 * 1000);
    }

    // Test: User message followed by tmux focus restores pane-based tracking
    #[test]
    fn test_user_message_then_tmux_focus_switches_back() {
        let events = vec![
            TestEvent::tmux_focus(ts(0), "A"),
            TestEvent::user_message(ts(2), "sess1", "B"),
            TestEvent::tmux_focus(ts(4), "A"),
        ];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(5)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        let stream_b = get_stream_time(&result, "B").expect("Stream B should exist");
        // A: focus [0, min(2, 0+1min)] = [0, 1] = 1min
        // B: user_message [2, min(4, 2+1min)] = [2, 3] = 1min
        // A: tmux_focus [4, min(5, 4+1min)] = [4, 5] = 1min
        assert_eq!(stream_a.time_direct_ms, 2 * 60_000);
        assert_eq!(stream_b.time_direct_ms, 60_000);
    }

    // Test: UserMessage from a subagent session is not treated as human attention.
    // Without this filter, a parent agent dispatching a Task subagent would have
    // the subagent's prompt counted as direct user time on the subagent's stream.
    #[test]
    fn test_user_message_from_subagent_session_does_not_establish_focus() {
        let events = vec![
            TestEvent::user_message(ts(1), "sess1", "A"),
            TestEvent::user_message(ts(2), "sess1", "A"),
        ];
        let session_types = HashMap::from([(String::from("sess1"), SessionType::Subagent)]);

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(5)),
            &HashMap::new(),
            &session_types,
        );

        // The subagent's user_message events are skipped, so no focus is established.
        assert!(result.stream_times.is_empty());
    }

    // Test: UserMessage from a User session still establishes focus when session_types
    // is populated. Guards against the subagent filter accidentally suppressing real work.
    #[test]
    fn test_user_message_from_user_session_still_establishes_focus() {
        let events = vec![
            TestEvent::user_message(ts(1), "sess1", "A"),
            TestEvent::user_message(ts(2), "sess1", "A"),
        ];
        let session_types = HashMap::from([(String::from("sess1"), SessionType::User)]);

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(5)),
            &HashMap::new(),
            &session_types,
        );

        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // msg@1 establishes focus; msg@2 closes [1, min(2, 1+1min)]=[1,2]=1min, reopens at 2;
        // finalize closes [2, min(5, 2+1min)]=[2,3]=1min. Total: 2min.
        assert_eq!(stream_a.time_direct_ms, 2 * 60_000);
    }

    // Regression guard for the silent-drop bug: events with NO stream assignment used to
    // vanish from time attribution entirely. They must now accrue to a synthetic
    // "unassigned" bucket, surfaced via AllocationResult.unassigned_{direct,delegated}_ms,
    // WITHOUT leaking a fake stream id into stream_times.
    #[test]
    fn test_unassigned_agent_session_accrues_delegated_time() {
        let events = vec![
            TestEvent::agent_session(ts(0), "started", "sess1", None),
            TestEvent::agent_tool_use(ts(1), "sess1", "ignored"),
            TestEvent::agent_tool_use(ts(2), "sess1", "ignored"),
            TestEvent::agent_session(ts(3), "ended", "sess1", None),
        ];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(5)),
            &HashMap::new(),
            &HashMap::new(),
        );

        // Delegated time from first tool use (1) to end (3) = 2 min, credited to unassigned.
        assert_eq!(result.unassigned_delegated_ms, 2 * 60_000);
        // The synthetic bucket must NOT leak into stream_times as a real stream.
        assert!(result.stream_times.is_empty());
    }

    #[test]
    fn an_assigned_agent_session_credits_delegated_time_to_its_stream() {
        // Reproduces the exact shape of a live stream that reports zero delegated time
        // while holding 76 agent_tool_use events: a session started, tool uses, ended, with
        // every event carrying the same stream_id. 287 streams and 295,361 tool-use events
        // sit in this state on the live corpus, so if the allocator drops them here the
        // leverage figure is understated at scale.
        let events = vec![
            TestEvent::agent_session(ts(0), "started", "sess1", Some("stream-a")),
            TestEvent::agent_tool_use(ts(1), "sess1", "stream-a"),
            TestEvent::agent_tool_use(ts(2), "sess1", "stream-a"),
            TestEvent::agent_session(ts(3), "ended", "sess1", Some("stream-a")),
        ];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(5)),
            &HashMap::new(),
            &HashMap::new(),
        );

        // Delegated runs first tool use (1) to end (3) = 2 min, credited to the stream.
        let stream = result
            .stream_times
            .iter()
            .find(|time| time.stream_id == "stream-a")
            .expect("an assigned session must credit its stream, not vanish");
        assert_eq!(
            stream.time_delegated_ms,
            2 * 60_000,
            "delegated time must land on the stream the session's events name"
        );
        assert_eq!(
            result.unassigned_delegated_ms, 0,
            "an assigned session must not be credited to the unassigned bucket"
        );
    }

    #[test]
    fn a_synced_duplicate_session_start_does_not_erase_the_stream() {
        // The exact live shape: the machine that holds the transcript derives the session
        // with its stream, and two machines that synced it in each add an unassigned copy at
        // the identical timestamp. Their ids differ, so INSERT OR IGNORE keeps all three.
        let events = vec![
            TestEvent::agent_session(ts(0), "started", "sess1", Some("stream-a")),
            TestEvent::agent_session(ts(0), "started", "sess1", None),
            TestEvent::agent_session(ts(0), "started", "sess1", None),
            TestEvent::agent_tool_use(ts(1), "sess1", "stream-a"),
            TestEvent::agent_tool_use(ts(2), "sess1", "stream-a"),
            TestEvent::agent_session(ts(3), "ended", "sess1", Some("stream-a")),
        ];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(5)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream = result
            .stream_times
            .iter()
            .find(|time| time.stream_id == "stream-a")
            .expect("an unassigned duplicate must not take the stream away");
        assert_eq!(stream.time_delegated_ms, 2 * 60_000);
        assert_eq!(
            result.unassigned_delegated_ms, 0,
            "the session's span must not fall to unassigned because a copy lacked a stream"
        );
    }

    #[test]
    fn a_later_start_naming_a_stream_still_updates_an_unassigned_session() {
        // The guard is one-directional: it stops an unassigned copy from erasing a known
        // stream, and must not stop a copy that actually carries one from supplying it.
        let events = vec![
            TestEvent::agent_session(ts(0), "started", "sess1", None),
            TestEvent::agent_session(ts(0), "started", "sess1", Some("stream-a")),
            TestEvent::agent_tool_use(ts(1), "sess1", "stream-a"),
            TestEvent::agent_session(ts(3), "ended", "sess1", Some("stream-a")),
        ];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(5)),
            &HashMap::new(),
            &HashMap::new(),
        );

        let stream = result
            .stream_times
            .iter()
            .find(|time| time.stream_id == "stream-a")
            .expect("a copy naming a stream must supply it");
        assert_eq!(stream.time_delegated_ms, 2 * 60_000);
    }

    #[test]
    fn a_genuinely_unassigned_session_still_accrues_to_unassigned() {
        // With no copy naming a stream, nothing is invented: the span stays unassigned.
        let events = vec![
            TestEvent::agent_session(ts(0), "started", "sess1", None),
            TestEvent::agent_session(ts(0), "started", "sess1", None),
            TestEvent::agent_tool_use(ts(1), "sess1", "ignored"),
            TestEvent::agent_session(ts(3), "ended", "sess1", None),
        ];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(5)),
            &HashMap::new(),
            &HashMap::new(),
        );

        assert_eq!(result.unassigned_delegated_ms, 2 * 60_000);
        assert!(result.stream_times.is_empty());
    }

    #[test]
    fn a_sessions_delegated_span_never_outruns_the_end_event_that_closed_it() {
        // Guards against inflating delegated time, which is easy to ship unnoticed because
        // direct time is unaffected and only direct time gets checked by habit.
        //
        // Deferring to `agent_sessions.end_time` when an `ended` event looks premature was
        // tried and reverted: sessions stay open for months (one spans 2,189 hours against
        // 55 hours of tool use), so running the interval to the authoritative end took the
        // corpus from 14,648h to 80,161h of delegated time and a single week from 428h to
        // 2,116h. A correct version must cap at `min(end, last_tool + agent_timeout_ms)`;
        // see the AGENTS.md entry before attempting it again.
        let mut ends = HashMap::new();
        // The authoritative record claims the session ran for hours after the work stopped.
        ends.insert("sess1".to_string(), ts(600));
        let events = vec![
            TestEvent::agent_session(ts(0), "started", "sess1", Some("stream-a")),
            TestEvent::agent_tool_use(ts(1), "sess1", "stream-a"),
            TestEvent::agent_tool_use(ts(2), "sess1", "stream-a"),
            TestEvent::agent_session(ts(3), "ended", "sess1", Some("stream-a")),
        ];

        let config = test_config();
        let result = allocate_time(&events, &config, Some(ts(700)), &ends, &HashMap::new());

        let stream = result
            .stream_times
            .iter()
            .find(|time| time.stream_id == "stream-a")
            .expect("the stream accrues its span");
        assert_eq!(
            stream.time_delegated_ms,
            2 * 60_000,
            "delegated must run first tool use to the closing event, not to a far-off \
             authoritative end: got {}ms",
            stream.time_delegated_ms
        );
    }

    #[test]
    fn test_unassigned_focus_accrues_direct_time() {
        let events = vec![TestEvent {
            timestamp: ts(0),
            event_type: EventType::TmuxPaneFocus,
            stream_id: None,
            session_id: None,
            action: None,
            data: json!({"pane_id": "%1", "cwd": "/test"}),
        }];

        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(5)),
            &HashMap::new(),
            &HashMap::new(),
        );

        // Focus with no stream accrues direct time to the unassigned bucket (1min window),
        // and must not appear as a real stream.
        assert_eq!(result.unassigned_direct_ms, 60_000);
        assert!(result.stream_times.is_empty());
    }

    #[test]
    fn test_browser_focus_without_stream_accrues_unassigned_direct() {
        // Given: focus on a browser window with no resolvable stream (no browser_tab,
        // no window stream) — e.g. reading a doc in Brave.
        let events = vec![TestEvent::window_focus(ts(0), "brave-browser", None)];

        // When: time is allocated over a 5-minute period.
        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(5)),
            &HashMap::new(),
            &HashMap::new(),
        );

        // Then: the active browser time accrues to the unassigned bucket (1-min window),
        // not zero, and does not appear as a real stream.
        assert_eq!(result.unassigned_direct_ms, 60_000);
        assert!(result.stream_times.is_empty());
    }

    #[test]
    fn test_gui_focus_without_stream_accrues_unassigned_direct() {
        // Given: focus on a non-terminal GUI window (Slack) with no stream.
        let events = vec![TestEvent::window_focus(ts(0), "slack", None)];

        // When: time is allocated over a 5-minute period.
        let config = test_config();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(5)),
            &HashMap::new(),
            &HashMap::new(),
        );

        // Then: the active GUI time accrues to the unassigned bucket, not zero.
        assert_eq!(result.unassigned_direct_ms, 60_000);
        assert!(result.stream_times.is_empty());
    }

    // Two machines observing the same work overlap their focus intervals. Direct
    // attention is a union, never a sum — see `specs/design/core-concepts.md:112`.
    #[test]
    fn test_direct_time_is_union_of_overlapping_focus_intervals() {
        // Given: one stream watched by two machines. The laptop reports GUI focus at
        // 09:10 while the devbox reports tmux focus stamped 09:02 — clock skew between
        // the two machines puts the devbox event out of monotonic order in the merged
        // stream, so the attention window reopens over ground already credited.
        let events = vec![
            TestEvent::tmux_focus(ts(0), "A"),
            TestEvent::window_focus(ts(10), "ghostty", None),
            TestEvent::tmux_focus(ts(2), "A"),
            TestEvent::window_focus(ts(10), "ghostty", None),
        ];

        // When: time is allocated with the production 5-minute attention window.
        let config = AllocationConfig::default();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(10)),
            &HashMap::new(),
            &HashMap::new(),
        );

        // Then: stream A holds two overlapping focus intervals, [09:00, 09:05) and
        // [09:02, 09:07), and its direct time is their union (7 min) — not their sum
        // (10 min), which would double-count the contended 09:02-09:05 stretch.
        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        let naive_sum: i64 = stream_a
            .focus_intervals
            .iter()
            .map(Interval::duration_ms)
            .sum();
        assert_eq!(naive_sum, 10 * 60 * 1000, "fixture must produce an overlap");
        assert_eq!(stream_a.time_direct_ms, 7 * 60 * 1000);
    }

    // Invariant from `specs/design/core-concepts.md:112`: "Total direct time across all
    // streams <= wall clock time (no double-counting attention)".
    #[test]
    fn test_total_direct_time_never_exceeds_wall_clock() {
        // Given: contended focus on the unassigned bucket (two machines reporting the
        // same unclassified attention) followed by focus on a real stream, over a
        // 7-minute period.
        let events = vec![
            TestEvent::tmux_focus_unassigned(ts(0)),
            TestEvent::tmux_focus_unassigned(ts(6)),
            TestEvent::tmux_focus_unassigned(ts(2)),
            TestEvent::tmux_focus(ts(6), "A"),
        ];

        // When: time is allocated with the production 5-minute attention window.
        let config = AllocationConfig::default();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(7)),
            &HashMap::new(),
            &HashMap::new(),
        );

        // Then: every direct bucket — real streams and unassigned alike — sums to no
        // more than the wall-clock span of the period.
        let wall_clock_ms = (ts(7) - ts(0)).num_milliseconds();
        let total_direct_ms: i64 = result
            .stream_times
            .iter()
            .map(|s| s.time_direct_ms)
            .sum::<i64>()
            + result.unassigned_direct_ms;
        assert!(
            total_direct_ms <= wall_clock_ms,
            "total direct {total_direct_ms}ms exceeds wall clock {wall_clock_ms}ms"
        );
        // Unassigned attention is still attention: [09:00, 09:06) unioned, not 9 min summed.
        assert_eq!(result.unassigned_direct_ms, 6 * 60 * 1000);
    }

    // Invariant from `specs/design/core-concepts.md:113`: "Total delegated time can
    // exceed wall clock time (parallel execution)". Delegated time measures leverage,
    // so parallel agents MUST sum — do not "fix" this into a union.
    #[test]
    fn test_parallel_agent_sessions_sum_delegated_beyond_wall_clock() {
        // Given: two agent sessions running concurrently on the same stream for the
        // whole 31-minute period.
        let events = vec![
            TestEvent::agent_session(ts(0), "started", "sess1", Some("A")),
            TestEvent::agent_session(ts(0), "started", "sess2", Some("A")),
            TestEvent::agent_tool_use(ts(1), "sess1", "A"),
            TestEvent::agent_tool_use(ts(1), "sess2", "A"),
            TestEvent::agent_session(ts(31), "ended", "sess1", Some("A")),
            TestEvent::agent_session(ts(31), "ended", "sess2", Some("A")),
        ];

        // When: time is allocated over that period.
        let config = AllocationConfig::default();
        let result = allocate_time(
            &events,
            &config,
            Some(ts(31)),
            &HashMap::new(),
            &HashMap::new(),
        );

        // Then: both sessions are credited in full — 2 x 30 min of machine time against
        // 31 min of wall clock.
        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        let wall_clock_ms = (ts(31) - ts(0)).num_milliseconds();
        assert_eq!(stream_a.time_delegated_ms, 60 * 60 * 1000);
        assert!(
            stream_a.time_delegated_ms > wall_clock_ms,
            "parallel delegation must be able to exceed wall clock"
        );
    }

    // Injected text must not manufacture human attention.
    //
    // Composition test: extraction drops injected messages, so allocation never
    // sees them and they cannot open an attention window. Real messages still do.
    // This is the regression guard for a day that reported 18h of direct time
    // because overnight harness injections kept re-opening the window.
    #[test]
    fn test_injected_user_messages_do_not_extend_the_attention_window() {
        let config = test_config(); // 60s attention window
        let transcript = [
            (0, "start the refactor"),
            (10, "<system-reminder>\n[BACKGROUND TASK COMPLETED]"),
            (20, "[analyze-mode]\nkeep going"),
        ];

        let to_events = |texts: &[(i64, &str)]| -> Vec<TestEvent> {
            texts
                .iter()
                .map(|(minute, _)| TestEvent::user_message(ts(*minute), "sess1", "A"))
                .collect()
        };

        let human: Vec<(i64, &str)> = transcript
            .iter()
            .filter(|(_, text)| !crate::injection::is_injected(text))
            .copied()
            .collect();
        let result = allocate_time(
            &to_events(&human),
            &config,
            Some(ts(30)),
            &HashMap::new(),
            &HashMap::new(),
        );
        let stream_a = get_stream_time(&result, "A").expect("Stream A should exist");
        // Two human messages, each opening one 60s window. The injection at
        // minute 10 contributes nothing.
        assert_eq!(stream_a.time_direct_ms, 2 * 60 * 1000);

        // Control: treating the injection as human input manufactures a third
        // window out of nothing. This is the bug being fixed.
        let unfiltered = allocate_time(
            &to_events(&transcript),
            &config,
            Some(ts(30)),
            &HashMap::new(),
            &HashMap::new(),
        );
        let unfiltered_a = get_stream_time(&unfiltered, "A").expect("Stream A should exist");
        assert_eq!(
            unfiltered_a.time_direct_ms,
            3 * 60 * 1000,
            "control: an unfiltered injection fabricates an extra attention window"
        );
    }
}

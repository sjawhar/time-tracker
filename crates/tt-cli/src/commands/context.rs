//! Context export for stream inference.
//!
//! Exports layered context (events, agents, streams, gaps) for use by humans
//! or LLMs when making stream assignment decisions.

use anyhow::Context;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use tt_db::Database;

use super::util::parse_datetime;

/// Output structure for context export.
#[derive(Debug, Serialize)]
pub struct ContextOutput {
    pub time_range: TimeRange,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<EventExport>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agents: Option<Vec<AgentExport>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub streams: Option<Vec<StreamExport>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gaps: Option<Vec<GapExport>>,
}

/// Time range for the context export.
#[derive(Debug, Serialize)]
pub struct TimeRange {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// Event information for context export.
#[derive(Debug, Serialize)]
pub struct EventExport {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    #[serde(rename = "type")]
    pub event_type: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmux_session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_id: Option<String>,
}

/// Agent session information for context export.
#[derive(Debug, Serialize)]
pub struct AgentExport {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    pub session_type: tt_core::SessionType,
    pub source: tt_core::SessionSource,
    pub project_path: String,
    pub project_name: String,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starting_prompt: Option<String>,
    pub user_prompts: Vec<String>,
    pub user_prompt_count: i32,
    pub assistant_message_count: i32,
    pub tool_call_count: i32,
}

/// Stream information for context export.
#[derive(Debug, Serialize)]
pub struct StreamExport {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub time_direct_ms: i64,
    pub time_delegated_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_event_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_event_at: Option<DateTime<Utc>>,
}

/// Gap between user input events.
#[derive(Debug, Serialize)]
pub struct GapExport {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub duration_minutes: i64,
    pub before_event_type: String,
    pub after_event_type: String,
}

/// Export events from the database within the given time range.
fn export_events(
    db: &Database,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> anyhow::Result<Vec<EventExport>> {
    Ok(db
        .get_events_in_range(start, end)?
        .into_iter()
        .map(|e| EventExport {
            id: e.id,
            timestamp: e.timestamp,
            event_type: e.event_type.to_string(),
            source: e.source,
            cwd: e.cwd,
            git_project: e.git_project,
            git_workspace: e.git_workspace,
            session_id: e.session_id,
            tmux_session: e.tmux_session,
            pane_id: e.pane_id,
            machine_id: e.machine_id,
            stream_id: e.stream_id,
        })
        .collect())
}

/// Export agent sessions from the database within the given time range.
fn export_agents(
    db: &Database,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> anyhow::Result<Vec<AgentExport>> {
    Ok(db
        .agent_sessions_in_range(start, end)?
        .into_iter()
        .map(|s| AgentExport {
            user_prompt_count: i32::try_from(s.user_prompts.len()).unwrap_or(i32::MAX),
            session_id: s.session_id,
            parent_session_id: s.parent_session_id,
            session_type: s.session_type,
            source: s.source,
            project_path: s.project_path,
            project_name: s.project_name,
            start_time: s.start_time,
            end_time: s.end_time,
            summary: s.summary,
            starting_prompt: s.starting_prompt,
            user_prompts: s.user_prompts,
            assistant_message_count: s.assistant_message_count,
            tool_call_count: s.tool_call_count,
        })
        .collect())
}

/// Export streams from the database within the given time range.
fn export_streams(
    db: &Database,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> anyhow::Result<Vec<StreamExport>> {
    Ok(db
        .streams_in_range(start, end)?
        .into_iter()
        .map(|s| StreamExport {
            id: s.id,
            name: s.name,
            time_direct_ms: s.time_direct_ms,
            time_delegated_ms: s.time_delegated_ms,
            first_event_at: s.first_event_at,
            last_event_at: s.last_event_at,
        })
        .collect())
}

/// Check if an event type represents direct user activity.
const fn is_user_event(event_type: tt_core::EventType) -> bool {
    matches!(
        event_type,
        tt_core::EventType::UserMessage
            | tt_core::EventType::TmuxPaneFocus
            | tt_core::EventType::TmuxScroll
            | tt_core::EventType::WindowFocus
            | tt_core::EventType::BrowserTab
    )
}

/// Export gaps (periods of inactivity) between user events.
fn export_gaps(
    db: &Database,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    threshold_minutes: u32,
) -> anyhow::Result<Vec<GapExport>> {
    let events = db.get_events_in_range(start, end)?;

    // Filter to user events only
    let user_events: Vec<_> = events
        .iter()
        .filter(|e| is_user_event(e.event_type))
        .collect();

    if user_events.len() < 2 {
        return Ok(vec![]);
    }

    let threshold_ms = i64::from(threshold_minutes) * 60 * 1000;
    let mut gaps = Vec::new();

    for window in user_events.windows(2) {
        let before = window[0];
        let after = window[1];
        let gap_ms = (after.timestamp - before.timestamp).num_milliseconds();

        if gap_ms >= threshold_ms {
            gaps.push(GapExport {
                start: before.timestamp,
                end: after.timestamp,
                duration_minutes: gap_ms / 60_000,
                before_event_type: before.event_type.to_string(),
                after_event_type: after.event_type.to_string(),
            });
        }
    }

    Ok(gaps)
}

/// Run the context export command.
///
/// Exports layered context to stdout as JSON, filtered by flags and time range.
#[expect(clippy::fn_params_excessive_bools, reason = "CLI flag passthrough")]
#[expect(clippy::too_many_arguments, reason = "CLI flag passthrough")]
pub fn run(
    db: &Database,
    events: bool,
    agents: bool,
    streams: bool,
    gaps: bool,
    gap_threshold: u32,
    start: Option<String>,
    end: Option<String>,
    unclassified: bool,
    summary: bool,
) -> anyhow::Result<()> {
    eprintln!("Warning: `tt context` is deprecated. Use `tt classify` instead.");
    eprintln!("  tt classify --json            (replaces tt context --events --agents)");
    eprintln!("  tt classify --unclassified    (replaces tt context --unclassified)");
    eprintln!("  tt classify --gaps            (replaces tt context --gaps)");
    eprintln!();

    // Parse end time (default to now)
    let end_time = end
        .map(|s| parse_datetime(&s))
        .transpose()?
        .unwrap_or_else(Utc::now);

    // Parse start time (default to 24 hours before end)
    let start_time = start
        .map(|s| parse_datetime(&s))
        .transpose()?
        .unwrap_or_else(|| end_time - Duration::hours(24));

    // Validate time range
    if start_time > end_time {
        anyhow::bail!(
            "start time ({}) is after end time ({})",
            start_time.format("%Y-%m-%dT%H:%M:%SZ"),
            end_time.format("%Y-%m-%dT%H:%M:%SZ")
        );
    }

    let mut output = ContextOutput {
        time_range: TimeRange {
            start: start_time,
            end: end_time,
        },
        events: events
            .then(|| export_events(db, start_time, end_time))
            .transpose()?,
        agents: agents
            .then(|| export_agents(db, start_time, end_time))
            .transpose()?,
        streams: streams
            .then(|| export_streams(db, start_time, end_time))
            .transpose()?,
        gaps: gaps
            .then(|| export_gaps(db, start_time, end_time, gap_threshold))
            .transpose()?,
    };

    // Apply --unclassified filter: remove events/agents that already have a stream_id
    if unclassified {
        if let Some(ref mut evts) = output.events {
            evts.retain(|e| e.stream_id.is_none());
        }
        // For agents, filter to sessions whose events are unassigned
        // (agent exports don't have stream_id directly, so this is a best-effort filter)
    }

    // Apply --summary filter: truncate to compact representations
    if summary {
        if let Some(ref mut agents_list) = output.agents {
            for agent in agents_list.iter_mut() {
                // Truncate summary and prompts for compact output
                if let Some(ref mut s) = agent.summary {
                    s.truncate(120);
                }
                agent.user_prompts.truncate(1);
            }
        }
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&output).context("failed to serialize context")?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a basic `StoredEvent` for tests.
    fn make_test_event(
        id: &str,
        timestamp: chrono::DateTime<Utc>,
        event_type: tt_core::EventType,
        source: &str,
    ) -> tt_db::StoredEvent {
        tt_db::StoredEvent {
            id: id.to_string(),
            timestamp,
            event_type,
            source: source.to_string(),
            machine_id: None,
            schema_version: 1,
            pane_id: None,
            tmux_session: None,
            window_index: None,
            git_project: None,
            git_workspace: None,
            status: None,
            idle_duration_ms: None,
            action: None,
            cwd: None,
            session_id: None,
            stream_id: None,
            assignment_source: None,
            data: serde_json::json!({}),
        }
    }

    #[test]
    fn test_context_output_serializes_without_optional_fields() {
        let output = ContextOutput {
            time_range: TimeRange {
                start: chrono::DateTime::parse_from_rfc3339("2026-01-15T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                end: chrono::DateTime::parse_from_rfc3339("2026-01-15T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            },
            events: None,
            agents: None,
            streams: None,
            gaps: None,
        };

        let json = serde_json::to_string(&output).unwrap();
        // Should not contain "events", "agents", "streams", or "gaps" when None
        assert!(!json.contains("\"events\""));
        assert!(!json.contains("\"agents\""));
        assert!(!json.contains("\"streams\""));
        assert!(!json.contains("\"gaps\""));
        // Should contain time_range
        assert!(json.contains("\"time_range\""));
    }

    #[test]
    fn test_context_output_serializes_with_events() {
        let output = ContextOutput {
            time_range: TimeRange {
                start: chrono::DateTime::parse_from_rfc3339("2026-01-15T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                end: chrono::DateTime::parse_from_rfc3339("2026-01-15T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            },
            events: Some(vec![EventExport {
                id: "event-1".to_string(),
                timestamp: chrono::DateTime::parse_from_rfc3339("2026-01-15T11:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                event_type: "tmux_pane_focus".to_string(),
                source: "remote.tmux".to_string(),
                cwd: Some("/home/user/project".to_string()),
                git_project: Some("project".to_string()),
                git_workspace: Some("default".to_string()),
                session_id: None,
                tmux_session: Some("main".to_string()),
                pane_id: Some("%0".to_string()),
                machine_id: None,
                stream_id: None,
            }]),
            agents: None,
            streams: None,
            gaps: None,
        };

        let json = serde_json::to_string(&output).unwrap();
        assert!(json.contains("\"events\""));
        assert!(json.contains("\"event-1\""));
        assert!(json.contains("\"tmux_pane_focus\""));
    }

    #[test]
    fn test_event_export_skips_none_optional_fields() {
        let event = EventExport {
            id: "event-1".to_string(),
            timestamp: chrono::DateTime::parse_from_rfc3339("2026-01-15T11:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            event_type: "test".to_string(),
            source: "test".to_string(),
            cwd: None,
            git_project: None,
            git_workspace: None,
            session_id: None,
            tmux_session: None,
            pane_id: None,
            machine_id: None,
            stream_id: None,
        };

        let json = serde_json::to_string(&event).unwrap();
        // Optional fields should not be serialized when None
        assert!(!json.contains("\"cwd\""));
        assert!(!json.contains("\"session_id\""));
        assert!(!json.contains("\"tmux_session\""));
        assert!(!json.contains("\"pane_id\""));
        assert!(!json.contains("\"machine_id\""));
        assert!(!json.contains("\"stream_id\""));
    }

    #[test]
    fn test_agent_export_serialization() {
        let agent = AgentExport {
            session_id: "session-123".to_string(),
            parent_session_id: None,
            session_type: tt_core::SessionType::User,
            source: tt_core::SessionSource::Claude,
            project_path: "/home/user/project".to_string(),
            project_name: "project".to_string(),
            start_time: chrono::DateTime::parse_from_rfc3339("2026-01-15T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            end_time: Some(
                chrono::DateTime::parse_from_rfc3339("2026-01-15T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            summary: Some("Implemented feature X".to_string()),
            starting_prompt: Some("Implement feature X".to_string()),
            user_prompts: vec!["Implement feature X".to_string(), "Add tests".to_string()],
            user_prompt_count: 2,
            assistant_message_count: 3,
            tool_call_count: 10,
        };

        let json = serde_json::to_string(&agent).unwrap();
        assert!(json.contains("\"session_id\""));
        assert!(json.contains("\"session_type\""));
        assert!(json.contains("\"project_path\""));
        assert!(json.contains("\"user_prompts\""));
        assert!(json.contains("\"tool_call_count\""));
        assert!(json.contains("\"source\":\"claude\""));
        assert!(json.contains("\"session_type\":\"user\""));
        // parent_session_id should be skipped when None
        assert!(!json.contains("\"parent_session_id\""));
    }

    #[test]
    fn test_agent_export_with_parent_session_id() {
        let agent = AgentExport {
            session_id: "agent-a913a65".to_string(),
            parent_session_id: Some("d66718b7-3b37-47c8-b3a6-f01b637d8c13".to_string()),
            session_type: tt_core::SessionType::Subagent,
            source: tt_core::SessionSource::Claude,
            project_path: "/home/user/project".to_string(),
            project_name: "project".to_string(),
            start_time: chrono::DateTime::parse_from_rfc3339("2026-01-15T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            end_time: None,
            summary: None,
            starting_prompt: None,
            user_prompts: vec![],
            user_prompt_count: 0,
            assistant_message_count: 0,
            tool_call_count: 0,
        };

        let json = serde_json::to_string(&agent).unwrap();
        assert!(json.contains("\"parent_session_id\""));
        assert!(json.contains("d66718b7-3b37-47c8-b3a6-f01b637d8c13"));
        assert!(json.contains("\"session_type\":\"subagent\""));
    }

    #[test]
    fn test_export_agents_includes_parent_session_id() {
        let db = tt_db::Database::open_in_memory().unwrap();

        // Insert a parent session
        let parent = tt_core::session::AgentSession {
            session_id: "parent-session-id".to_string(),
            source: tt_core::session::SessionSource::default(),
            parent_session_id: None,
            session_type: tt_core::session::SessionType::User,
            project_path: "/home/user/project".to_string(),
            project_name: "project".to_string(),
            start_time: chrono::DateTime::parse_from_rfc3339("2026-01-15T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            end_time: Some(
                chrono::DateTime::parse_from_rfc3339("2026-01-15T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            message_count: 3,
            summary: None,
            user_prompts: vec![],
            starting_prompt: None,
            assistant_message_count: 1,
            tool_call_count: 0,
            user_message_timestamps: Vec::new(),
            tool_call_timestamps: Vec::new(),
        };
        db.upsert_agent_session(&parent, None).unwrap();

        // Insert a subagent session with parent
        let child = tt_core::session::AgentSession {
            session_id: "agent-a913a65".to_string(),
            source: tt_core::session::SessionSource::default(),
            parent_session_id: Some("parent-session-id".to_string()),
            session_type: tt_core::session::SessionType::Subagent,
            project_path: "/home/user/project".to_string(),
            project_name: "project".to_string(),
            start_time: chrono::DateTime::parse_from_rfc3339("2026-01-15T10:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
            end_time: Some(
                chrono::DateTime::parse_from_rfc3339("2026-01-15T11:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            message_count: 2,
            summary: None,
            user_prompts: vec![],
            starting_prompt: None,
            assistant_message_count: 1,
            tool_call_count: 5,
            user_message_timestamps: Vec::new(),
            tool_call_timestamps: Vec::new(),
        };
        db.upsert_agent_session(&child, None).unwrap();

        let start = chrono::DateTime::parse_from_rfc3339("2026-01-15T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let end = chrono::DateTime::parse_from_rfc3339("2026-01-15T13:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let exports = export_agents(&db, start, end).unwrap();

        assert_eq!(exports.len(), 2);

        let parent_export = exports
            .iter()
            .find(|e| e.session_id == "parent-session-id")
            .unwrap();
        assert!(parent_export.parent_session_id.is_none());
        assert_eq!(parent_export.session_type, tt_core::SessionType::User);

        let child_export = exports
            .iter()
            .find(|e| e.session_id == "agent-a913a65")
            .unwrap();
        assert_eq!(
            child_export.parent_session_id.as_deref(),
            Some("parent-session-id")
        );
        assert_eq!(child_export.session_type, tt_core::SessionType::Subagent);
    }

    #[test]
    fn test_export_agents_preserves_source_field() {
        let db = tt_db::Database::open_in_memory().unwrap();

        // Insert a Claude session
        let claude_session = tt_core::session::AgentSession {
            session_id: "claude-session-1".to_string(),
            source: tt_core::session::SessionSource::Claude,
            parent_session_id: None,
            session_type: tt_core::session::SessionType::User,
            project_path: "/home/user/project".to_string(),
            project_name: "project".to_string(),
            start_time: chrono::DateTime::parse_from_rfc3339("2026-01-15T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            end_time: None,
            message_count: 1,
            summary: None,
            user_prompts: vec![],
            starting_prompt: None,
            assistant_message_count: 0,
            tool_call_count: 0,
            user_message_timestamps: Vec::new(),
            tool_call_timestamps: Vec::new(),
        };
        db.upsert_agent_session(&claude_session, None).unwrap();

        // Insert an OpenCode session
        let opencode_session = tt_core::session::AgentSession {
            session_id: "ses_opencode_1".to_string(),
            source: tt_core::session::SessionSource::OpenCode,
            parent_session_id: None,
            session_type: tt_core::session::SessionType::User,
            project_path: "/home/user/project".to_string(),
            project_name: "project".to_string(),
            start_time: chrono::DateTime::parse_from_rfc3339("2026-01-15T11:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            end_time: None,
            message_count: 1,
            summary: None,
            user_prompts: vec![],
            starting_prompt: None,
            assistant_message_count: 0,
            tool_call_count: 0,
            user_message_timestamps: Vec::new(),
            tool_call_timestamps: Vec::new(),
        };
        db.upsert_agent_session(&opencode_session, None).unwrap();

        let start = chrono::DateTime::parse_from_rfc3339("2026-01-15T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let end = chrono::DateTime::parse_from_rfc3339("2026-01-15T13:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let exports = export_agents(&db, start, end).unwrap();
        assert_eq!(exports.len(), 2);

        let claude_export = exports
            .iter()
            .find(|e| e.session_id == "claude-session-1")
            .unwrap();
        assert_eq!(claude_export.source, tt_core::SessionSource::Claude);

        let opencode_export = exports
            .iter()
            .find(|e| e.session_id == "ses_opencode_1")
            .unwrap();
        assert_eq!(opencode_export.source, tt_core::SessionSource::OpenCode);

        // Verify JSON serialization uses consistent values
        let json = serde_json::to_string(&opencode_export).unwrap();
        assert!(json.contains("\"source\":\"opencode\""));
    }
    #[test]
    fn test_stream_export_serialization() {
        let stream = StreamExport {
            id: "stream-123".to_string(),
            name: Some("time-tracker".to_string()),
            time_direct_ms: 3_600_000,
            time_delegated_ms: 1_800_000,
            first_event_at: Some(
                chrono::DateTime::parse_from_rfc3339("2026-01-15T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            last_event_at: Some(
                chrono::DateTime::parse_from_rfc3339("2026-01-15T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
        };

        let json = serde_json::to_string(&stream).unwrap();
        assert!(json.contains("\"stream-123\""));
        assert!(json.contains("\"time-tracker\""));
        assert!(json.contains("\"time_direct_ms\""));
        assert!(json.contains("3600000"));
    }

    #[test]
    fn test_gap_export_serialization() {
        let gap = GapExport {
            start: chrono::DateTime::parse_from_rfc3339("2026-01-15T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            end: chrono::DateTime::parse_from_rfc3339("2026-01-15T10:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
            duration_minutes: 30,
            before_event_type: "tmux_pane_focus".to_string(),
            after_event_type: "tmux_pane_focus".to_string(),
        };

        let json = serde_json::to_string(&gap).unwrap();
        assert!(json.contains("\"duration_minutes\""));
        assert!(json.contains("30"));
        assert!(json.contains("\"before_event_type\""));
        assert!(json.contains("\"after_event_type\""));
    }

    #[test]
    fn test_run_outputs_json() {
        let db = tt_db::Database::open_in_memory().unwrap();

        // Run should succeed with empty database
        let result = run(&db, false, false, false, false, 5, None, None, false, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_with_events_flag() {
        let db = tt_db::Database::open_in_memory().unwrap();

        // Should succeed with events flag
        let result = run(&db, true, false, false, false, 5, None, None, false, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_with_all_flags() {
        let db = tt_db::Database::open_in_memory().unwrap();

        // Should succeed with all flags enabled
        let result = run(&db, true, true, true, true, 5, None, None, false, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_with_iso8601_times() {
        let db = tt_db::Database::open_in_memory().unwrap();

        let result = run(
            &db,
            true,
            false,
            false,
            false,
            5,
            Some("2026-01-15T10:00:00Z".to_string()),
            Some("2026-01-15T12:00:00Z".to_string()),
            false,
            false,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_with_relative_times() {
        let db = tt_db::Database::open_in_memory().unwrap();

        let result = run(
            &db,
            true,
            false,
            false,
            false,
            5,
            Some("2 hours ago".to_string()),
            None,
            false,
            false,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_with_invalid_start_time() {
        let db = tt_db::Database::open_in_memory().unwrap();

        let result = run(
            &db,
            false,
            false,
            false,
            false,
            5,
            Some("invalid-time".to_string()),
            None,
            false,
            false,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_run_with_invalid_end_time() {
        let db = tt_db::Database::open_in_memory().unwrap();

        let result = run(
            &db,
            false,
            false,
            false,
            false,
            5,
            None,
            Some("not-a-date".to_string()),
            false,
            false,
        );
        assert!(result.is_err());
    }

    // Tests for parse_datetime
    #[test]
    fn test_parse_datetime_iso8601() {
        use chrono::TimeZone;

        let dt = parse_datetime("2026-01-15T10:30:00Z").unwrap();
        assert_eq!(dt, Utc.with_ymd_and_hms(2026, 1, 15, 10, 30, 0).unwrap());
    }

    #[test]
    fn test_parse_datetime_relative_hours() {
        let before = Utc::now();
        let dt = parse_datetime("2 hours ago").unwrap();
        let after = Utc::now();

        // Should be approximately 2 hours ago
        let expected = before - Duration::hours(2);
        assert!(dt >= expected - Duration::seconds(1));
        assert!(dt <= after - Duration::hours(2) + Duration::seconds(1));
    }

    #[test]
    fn test_parse_datetime_relative_minutes() {
        let before = Utc::now();
        let dt = parse_datetime("30 minutes ago").unwrap();
        let after = Utc::now();

        let expected = before - Duration::minutes(30);
        assert!(dt >= expected - Duration::seconds(1));
        assert!(dt <= after - Duration::minutes(30) + Duration::seconds(1));
    }

    #[test]
    fn test_parse_datetime_relative_days() {
        let before = Utc::now();
        let dt = parse_datetime("1 day ago").unwrap();
        let after = Utc::now();

        let expected = before - Duration::days(1);
        assert!(dt >= expected - Duration::seconds(1));
        assert!(dt <= after - Duration::days(1) + Duration::seconds(1));
    }

    #[test]
    fn test_parse_datetime_relative_weeks() {
        let before = Utc::now();
        let dt = parse_datetime("1 week ago").unwrap();
        let after = Utc::now();

        let expected = before - Duration::weeks(1);
        assert!(dt >= expected - Duration::seconds(1));
        assert!(dt <= after - Duration::weeks(1) + Duration::seconds(1));
    }

    #[test]
    fn test_parse_datetime_plural_forms() {
        // Both singular and plural should work
        assert!(parse_datetime("1 hour ago").is_ok());
        assert!(parse_datetime("2 hours ago").is_ok());
        assert!(parse_datetime("1 minute ago").is_ok());
        assert!(parse_datetime("5 minutes ago").is_ok());
    }

    #[test]
    fn test_parse_datetime_invalid() {
        assert!(parse_datetime("invalid").is_err());
        assert!(parse_datetime("ago").is_err());
        assert!(parse_datetime("2 years ago").is_err()); // years not supported
        assert!(parse_datetime("-1 hours ago").is_err()); // negative not supported
    }

    #[test]
    fn test_export_events_extracts_fields_from_stored_event() {
        let db = tt_db::Database::open_in_memory().unwrap();

        // Create a test event with explicit fields
        let mut event = make_test_event(
            "test-event-123",
            chrono::DateTime::parse_from_rfc3339("2026-01-15T11:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            tt_core::EventType::TmuxPaneFocus,
            "remote.tmux",
        );
        event.session_id = Some("session-abc".to_string());
        event.tmux_session = Some("main".to_string());
        event.pane_id = Some("%0".to_string());
        event.cwd = Some("/home/user/project".to_string());

        db.insert_event(&event).unwrap();

        // Create the stream first (foreign key constraint)
        let stream = tt_db::Stream {
            id: "stream-xyz".to_string(),
            name: Some("test-stream".to_string()),
            created_at: chrono::DateTime::parse_from_rfc3339("2026-01-15T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            updated_at: chrono::DateTime::parse_from_rfc3339("2026-01-15T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            time_direct_ms: 0,
            time_delegated_ms: 0,
            first_event_at: None,
            last_event_at: None,
            needs_recompute: false,
        };
        db.insert_stream(&stream).unwrap();

        // Assign event to stream (insert_event doesn't persist stream_id)
        db.assign_event_to_stream("test-event-123", "stream-xyz", "inferred")
            .unwrap();

        // Query the events
        let start = chrono::DateTime::parse_from_rfc3339("2026-01-15T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let end = chrono::DateTime::parse_from_rfc3339("2026-01-15T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let exports = export_events(&db, start, end).unwrap();

        assert_eq!(exports.len(), 1);
        let export = &exports[0];
        assert_eq!(export.id, "test-event-123");
        assert_eq!(export.event_type, "tmux_pane_focus");
        assert_eq!(export.source, "remote.tmux");
        assert_eq!(export.cwd, Some("/home/user/project".to_string()));
        assert_eq!(export.session_id, Some("session-abc".to_string()));
        assert_eq!(export.tmux_session, Some("main".to_string()));
        assert_eq!(export.pane_id, Some("%0".to_string()));
        assert_eq!(export.stream_id, Some("stream-xyz".to_string()));
    }

    #[test]
    fn test_export_events_extracts_session_name_as_tmux_session() {
        let db = tt_db::Database::open_in_memory().unwrap();

        // Create a test event with tmux_session field
        let mut event = make_test_event(
            "test-event-456",
            chrono::DateTime::parse_from_rfc3339("2026-01-15T11:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            tt_core::EventType::TmuxPaneFocus,
            "remote.tmux",
        );
        event.tmux_session = Some("dev".to_string());

        db.insert_event(&event).unwrap();

        let start = chrono::DateTime::parse_from_rfc3339("2026-01-15T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let end = chrono::DateTime::parse_from_rfc3339("2026-01-15T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let exports = export_events(&db, start, end).unwrap();

        assert_eq!(exports.len(), 1);
        // session_name should be extracted as tmux_session
        assert_eq!(exports[0].tmux_session, Some("dev".to_string()));
    }

    #[test]
    fn test_export_events_empty_range() {
        let db = tt_db::Database::open_in_memory().unwrap();

        let start = chrono::DateTime::parse_from_rfc3339("2026-01-15T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let end = chrono::DateTime::parse_from_rfc3339("2026-01-15T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let exports = export_events(&db, start, end).unwrap();
        assert!(exports.is_empty());
    }

    #[test]
    fn test_export_streams_returns_stream_exports() {
        let db = tt_db::Database::open_in_memory().unwrap();

        // Insert a test stream
        let stream = tt_db::Stream {
            id: "stream-abc".to_string(),
            name: Some("time-tracker".to_string()),
            created_at: chrono::DateTime::parse_from_rfc3339("2026-01-15T09:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            updated_at: chrono::DateTime::parse_from_rfc3339("2026-01-15T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            time_direct_ms: 3_600_000,
            time_delegated_ms: 1_800_000,
            first_event_at: Some(
                chrono::DateTime::parse_from_rfc3339("2026-01-15T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            last_event_at: Some(
                chrono::DateTime::parse_from_rfc3339("2026-01-15T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            needs_recompute: false,
        };
        db.insert_stream(&stream).unwrap();

        // Query streams in a range that includes the test stream
        let start = chrono::DateTime::parse_from_rfc3339("2026-01-15T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let end = chrono::DateTime::parse_from_rfc3339("2026-01-15T13:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let exports = export_streams(&db, start, end).unwrap();

        assert_eq!(exports.len(), 1);
        let export = &exports[0];
        assert_eq!(export.id, "stream-abc");
        assert_eq!(export.name, Some("time-tracker".to_string()));
        assert_eq!(export.time_direct_ms, 3_600_000);
        assert_eq!(export.time_delegated_ms, 1_800_000);
        assert_eq!(export.first_event_at, stream.first_event_at);
        assert_eq!(export.last_event_at, stream.last_event_at);
    }

    #[test]
    fn test_export_streams_empty_when_no_streams() {
        let db = tt_db::Database::open_in_memory().unwrap();

        let start = chrono::DateTime::parse_from_rfc3339("2026-01-15T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let end = chrono::DateTime::parse_from_rfc3339("2026-01-15T13:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let exports = export_streams(&db, start, end).unwrap();
        assert!(exports.is_empty());
    }

    #[test]
    fn test_export_agents_returns_sessions_in_range() {
        let db = tt_db::Database::open_in_memory().unwrap();

        // Insert a test agent session
        let session = tt_core::session::AgentSession {
            session_id: "test-session-123".to_string(),
            source: tt_core::session::SessionSource::default(),
            parent_session_id: None,
            session_type: tt_core::session::SessionType::default(),
            project_path: "/home/user/my-project".to_string(),
            project_name: "my-project".to_string(),
            start_time: chrono::DateTime::parse_from_rfc3339("2026-01-15T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            end_time: Some(
                chrono::DateTime::parse_from_rfc3339("2026-01-15T12:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            ),
            message_count: 5,
            summary: Some("Implemented feature X".to_string()),
            user_prompts: vec!["Implement feature X".to_string(), "Add tests".to_string()],
            starting_prompt: Some("Implement feature X".to_string()),
            assistant_message_count: 4,
            tool_call_count: 15,
            user_message_timestamps: Vec::new(),
            tool_call_timestamps: Vec::new(),
        };
        db.upsert_agent_session(&session, None).unwrap();

        // Query for sessions in range
        let start = chrono::DateTime::parse_from_rfc3339("2026-01-15T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let end = chrono::DateTime::parse_from_rfc3339("2026-01-15T13:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let exports = export_agents(&db, start, end).unwrap();

        assert_eq!(exports.len(), 1);
        let agent = &exports[0];
        assert_eq!(agent.session_id, "test-session-123");
        assert_eq!(agent.project_path, "/home/user/my-project");
        assert_eq!(agent.project_name, "my-project");
        assert_eq!(agent.summary.as_deref(), Some("Implemented feature X"));
        assert_eq!(
            agent.starting_prompt.as_deref(),
            Some("Implement feature X")
        );
        assert_eq!(agent.user_prompts, vec!["Implement feature X", "Add tests"]);
        assert_eq!(agent.user_prompt_count, 2); // Matches user_prompts.len(), not message_count
        assert_eq!(agent.assistant_message_count, 4);
        assert_eq!(agent.tool_call_count, 15);
    }

    #[test]
    fn test_export_agents_empty_when_no_sessions() {
        let db = tt_db::Database::open_in_memory().unwrap();

        let start = chrono::DateTime::parse_from_rfc3339("2026-01-15T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let end = chrono::DateTime::parse_from_rfc3339("2026-01-15T13:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let exports = export_agents(&db, start, end).unwrap();

        assert!(exports.is_empty());
    }

    // Tests for is_user_event
    #[test]
    fn test_is_user_event_recognizes_user_events() {
        assert!(is_user_event(tt_core::EventType::UserMessage));
        assert!(is_user_event(tt_core::EventType::TmuxPaneFocus));
        assert!(is_user_event(tt_core::EventType::TmuxScroll));
        assert!(is_user_event(tt_core::EventType::WindowFocus));
        assert!(is_user_event(tt_core::EventType::BrowserTab));
    }

    #[test]
    fn test_is_user_event_rejects_non_user_events() {
        assert!(!is_user_event(tt_core::EventType::AgentToolUse));
        assert!(!is_user_event(tt_core::EventType::AgentSession));
    }

    // Tests for export_gaps
    #[test]
    fn test_export_gaps_finds_gaps_exceeding_threshold() {
        use chrono::TimeZone;

        let db = tt_db::Database::open_in_memory().unwrap();

        // Create events with a 10-minute gap
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 15, 10, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 1, 15, 10, 10, 0).unwrap(); // 10 min later

        let event1 = make_test_event("e1", ts1, tt_core::EventType::TmuxPaneFocus, "remote.tmux");
        let event2 = make_test_event("e2", ts2, tt_core::EventType::UserMessage, "remote.agent");

        db.insert_event(&event1).unwrap();
        db.insert_event(&event2).unwrap();

        // With 5-minute threshold, should find the 10-minute gap
        let start = Utc.with_ymd_and_hms(2026, 1, 15, 9, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 1, 15, 11, 0, 0).unwrap();
        let gaps = export_gaps(&db, start, end, 5).unwrap();

        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].duration_minutes, 10);
        assert_eq!(gaps[0].before_event_type, "tmux_pane_focus");
        assert_eq!(gaps[0].after_event_type, "user_message");
    }

    #[test]
    fn test_export_gaps_ignores_gaps_below_threshold() {
        use chrono::TimeZone;

        let db = tt_db::Database::open_in_memory().unwrap();

        // Create events with a 3-minute gap
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 15, 10, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 1, 15, 10, 3, 0).unwrap(); // 3 min later

        let event1 = make_test_event("e1", ts1, tt_core::EventType::TmuxPaneFocus, "remote.tmux");
        let event2 = make_test_event("e2", ts2, tt_core::EventType::UserMessage, "remote.agent");

        db.insert_event(&event1).unwrap();
        db.insert_event(&event2).unwrap();

        // With 5-minute threshold, should NOT find the 3-minute gap
        let start = Utc.with_ymd_and_hms(2026, 1, 15, 9, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 1, 15, 11, 0, 0).unwrap();
        let gaps = export_gaps(&db, start, end, 5).unwrap();

        assert!(gaps.is_empty());
    }

    #[test]
    fn test_export_gaps_empty_or_single_event_returns_empty() {
        use chrono::TimeZone;

        let db = tt_db::Database::open_in_memory().unwrap();

        let start = Utc.with_ymd_and_hms(2026, 1, 15, 9, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 1, 15, 11, 0, 0).unwrap();

        // Empty database - no gaps
        let gaps = export_gaps(&db, start, end, 5).unwrap();
        assert!(gaps.is_empty());

        // Single user event - no gaps (need at least 2 to form a gap)
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 15, 10, 0, 0).unwrap();
        let event1 = make_test_event("e1", ts1, tt_core::EventType::TmuxPaneFocus, "remote.tmux");
        db.insert_event(&event1).unwrap();

        let gaps = export_gaps(&db, start, end, 5).unwrap();
        assert!(gaps.is_empty());
    }

    #[test]
    fn test_export_gaps_ignores_non_user_events() {
        use chrono::TimeZone;

        let db = tt_db::Database::open_in_memory().unwrap();

        // User event, then non-user event, then user event with 20 min gap
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 15, 10, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 1, 15, 10, 5, 0).unwrap(); // non-user event
        let ts3 = Utc.with_ymd_and_hms(2026, 1, 15, 10, 20, 0).unwrap(); // 20 min after first

        let event1 = make_test_event("e1", ts1, tt_core::EventType::TmuxPaneFocus, "remote.tmux");
        let event2 = make_test_event("e2", ts2, tt_core::EventType::AgentToolUse, "remote.agent"); // NOT a user event
        let event3 = make_test_event("e3", ts3, tt_core::EventType::WindowFocus, "remote.window");

        db.insert_event(&event1).unwrap();
        db.insert_event(&event2).unwrap();
        db.insert_event(&event3).unwrap();

        // Should find 20 min gap between user events (ignoring the agent_tool_use)
        let start = Utc.with_ymd_and_hms(2026, 1, 15, 9, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 1, 15, 11, 0, 0).unwrap();
        let gaps = export_gaps(&db, start, end, 5).unwrap();

        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].duration_minutes, 20);
        assert_eq!(gaps[0].before_event_type, "tmux_pane_focus");
        assert_eq!(gaps[0].after_event_type, "window_focus");
    }

    #[test]
    fn test_export_events_without_stream_assignment() {
        let db = tt_db::Database::open_in_memory().unwrap();

        // Create event without stream_id
        let mut event = make_test_event(
            "unassigned-event",
            chrono::DateTime::parse_from_rfc3339("2026-01-15T11:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            tt_core::EventType::TmuxPaneFocus,
            "remote.tmux",
        );
        event.pane_id = Some("%1".to_string());
        event.cwd = Some("/home/user/project".to_string());

        db.insert_event(&event).unwrap();

        let start = chrono::DateTime::parse_from_rfc3339("2026-01-15T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let end = chrono::DateTime::parse_from_rfc3339("2026-01-15T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let exports = export_events(&db, start, end).unwrap();

        assert_eq!(exports.len(), 1);
        assert_eq!(exports[0].stream_id, None);
    }

    #[test]
    fn test_export_gaps_exact_threshold() {
        use chrono::TimeZone;

        let db = tt_db::Database::open_in_memory().unwrap();

        // Create events with exactly 5-minute gap
        let ts1 = Utc.with_ymd_and_hms(2026, 1, 15, 10, 0, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 1, 15, 10, 5, 0).unwrap(); // Exactly 5 min

        let event1 = make_test_event("e1", ts1, tt_core::EventType::TmuxPaneFocus, "remote.tmux");
        let event2 = make_test_event("e2", ts2, tt_core::EventType::UserMessage, "remote.agent");

        db.insert_event(&event1).unwrap();
        db.insert_event(&event2).unwrap();

        // Test with threshold of 5 minutes - should find the gap (>=)
        let start = Utc.with_ymd_and_hms(2026, 1, 15, 9, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 1, 15, 11, 0, 0).unwrap();
        let gaps = export_gaps(&db, start, end, 5).unwrap();

        assert_eq!(
            gaps.len(),
            1,
            "5-minute gap should be found with 5-minute threshold"
        );
        assert_eq!(gaps[0].duration_minutes, 5);
    }

    #[test]
    fn test_parse_datetime_with_timezone_offset() {
        // Test ISO 8601 with timezone offset (not just Z)
        let dt = parse_datetime("2026-01-15T10:30:00+05:00").unwrap();
        // Should be converted to UTC
        // Should be converted to UTC (10:30+05:00 = 05:30 UTC)
        assert!(
            dt.to_string().contains("05:30"),
            "Expected UTC conversion: {dt}"
        );
    }

    #[test]
    fn test_parse_datetime_zero_relative_time() {
        // Edge case: 0 units ago
        let result = parse_datetime("0 hours ago");
        assert!(result.is_ok(), "Should accept 0 hours ago");
    }

    #[test]
    fn test_is_user_event_case_sensitivity() {
        assert!(is_user_event(tt_core::EventType::UserMessage));
        assert!(is_user_event(tt_core::EventType::TmuxPaneFocus));
        assert!(!is_user_event(tt_core::EventType::AgentToolUse));
    }

    #[test]
    fn test_export_gaps_multiple_gaps() {
        use chrono::TimeZone;

        let db = tt_db::Database::open_in_memory().unwrap();

        // Create events with multiple gaps exceeding threshold
        let times = [
            Utc.with_ymd_and_hms(2026, 1, 15, 10, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 1, 15, 10, 15, 0).unwrap(), // 15 min gap
            Utc.with_ymd_and_hms(2026, 1, 15, 10, 25, 0).unwrap(), // 10 min gap
            Utc.with_ymd_and_hms(2026, 1, 15, 10, 30, 0).unwrap(), // 5 min gap
        ];

        for (i, ts) in times.iter().enumerate() {
            let event = make_test_event(
                &format!("e{i}"),
                *ts,
                tt_core::EventType::TmuxPaneFocus,
                "remote.tmux",
            );
            db.insert_event(&event).unwrap();
        }

        // With 10-minute threshold, should find first 2 gaps
        let start = Utc.with_ymd_and_hms(2026, 1, 15, 9, 0, 0).unwrap();
        let end = Utc.with_ymd_and_hms(2026, 1, 15, 11, 0, 0).unwrap();
        let gaps = export_gaps(&db, start, end, 10).unwrap();

        assert_eq!(gaps.len(), 2, "Should find 2 gaps >= 10 minutes");
        assert_eq!(gaps[0].duration_minutes, 15);
        assert_eq!(gaps[1].duration_minutes, 10);
    }
}

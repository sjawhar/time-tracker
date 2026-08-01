//! Fixture builders shared by the report module's test files.

use std::collections::HashMap;

use chrono::{DateTime, TimeZone, Utc};
use serde_json::json;
use tt_core::session::{AgentSession, SessionSource, SessionType};
use tt_db::WindowedAgentSession;

use super::{PeriodType, ReportData, ReportStreamTime};

/// A report with no activity, to be filled in with struct update syntax.
pub fn base_report(period_type: PeriodType) -> ReportData {
    let (period_start, period_end) = match period_type {
        PeriodType::Week => (
            Utc.with_ymd_and_hms(2025, 1, 27, 8, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2025, 2, 3, 8, 0, 0).unwrap(),
        ),
        PeriodType::Day => (
            Utc.with_ymd_and_hms(2025, 1, 29, 8, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2025, 1, 30, 8, 0, 0).unwrap(),
        ),
    };
    ReportData {
        generated_at: Utc.with_ymd_and_hms(2025, 1, 29, 16, 0, 0).unwrap(),
        period_start,
        period_end,
        period_type,
        timezone: "Etc/UTC".to_string(),
        streams: vec![],
        tags_by_stream: HashMap::new(),
        agent_sessions: vec![],
        total_tracked_ms: 0,
        unassigned_direct_ms: 0,
        unassigned_delegated_ms: 0,
        junk_stream_id: Some(tt_db::JUNK_STREAM_SLUG.to_string()),
    }
}

/// Builds a stream-id-to-tags map from `(stream_id, tag)` pairs.
pub fn tags(pairs: &[(&str, &str)]) -> HashMap<String, Vec<String>> {
    let mut tags_by_stream: HashMap<String, Vec<String>> = HashMap::new();
    for (stream_id, tag) in pairs {
        tags_by_stream
            .entry((*stream_id).to_string())
            .or_default()
            .push((*tag).to_string());
    }
    tags_by_stream
}

pub fn make_test_stream(
    id: &str,
    name: &str,
    direct_ms: i64,
    delegated_ms: i64,
) -> ReportStreamTime {
    ReportStreamTime {
        id: id.to_string(),
        name: Some(name.to_string()),
        time_direct_ms: direct_ms,
        time_delegated_ms: delegated_ms,
    }
}

/// A session active for its whole nominal span, which is how most sessions look.
///
/// `end_time` is retained because callers still assert on session metadata, but
/// the report reads only the activity bounds — see [`windowed_session`] for a
/// session whose activity is narrower than its span.
pub fn make_test_session(
    session_id: &str,
    source: SessionSource,
    session_type: SessionType,
    start_time: DateTime<Utc>,
    end_time: Option<DateTime<Utc>>,
    starting_prompt: Option<&str>,
) -> WindowedAgentSession {
    let session = agent_session(
        session_id,
        source,
        session_type,
        start_time,
        end_time,
        starting_prompt,
    );
    // Absent a recorded end, the session is treated as a single instant of
    // activity: nothing observed it running any longer than that.
    let last_activity = end_time.unwrap_or(start_time);
    WindowedAgentSession {
        session,
        first_activity: start_time,
        last_activity,
    }
}

/// A session whose observed activity is narrower than its nominal span.
pub fn windowed_session(
    session_id: &str,
    start_time: DateTime<Utc>,
    end_time: Option<DateTime<Utc>>,
    first_activity: DateTime<Utc>,
    last_activity: DateTime<Utc>,
) -> WindowedAgentSession {
    WindowedAgentSession {
        session: agent_session(
            session_id,
            SessionSource::OpenCode,
            SessionType::User,
            start_time,
            end_time,
            Some("long-lived session"),
        ),
        first_activity,
        last_activity,
    }
}

fn agent_session(
    session_id: &str,
    source: SessionSource,
    session_type: SessionType,
    start_time: DateTime<Utc>,
    end_time: Option<DateTime<Utc>>,
    starting_prompt: Option<&str>,
) -> AgentSession {
    AgentSession {
        session_id: session_id.to_string(),
        source,
        parent_session_id: None,
        session_type,
        project_path: "/home/sami/time-tracker/default".to_string(),
        project_name: "time-tracker".to_string(),
        start_time,
        end_time,
        message_count: 3,
        summary: None,
        user_prompts: Vec::new(),
        starting_prompt: starting_prompt.map(ToString::to_string),
        assistant_message_count: 1,
        tool_call_count: 0,
        user_message_timestamps: Vec::new(),
        tool_call_timestamps: Vec::new(),
    }
}

pub fn make_agent_event(
    id: &str,
    timestamp: DateTime<Utc>,
    event_type: tt_core::EventType,
    session_id: &str,
    stream_id: &str,
    action: Option<&str>,
) -> tt_db::StoredEvent {
    tt_db::StoredEvent {
        id: id.to_string(),
        timestamp,
        event_type,
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
        action: action.map(ToString::to_string),
        cwd: Some("/home/sami/time-tracker/default".to_string()),
        session_id: Some(session_id.to_string()),
        stream_id: Some(stream_id.to_string()),
        assignment_source: None,
        data: json!({}),
    }
}

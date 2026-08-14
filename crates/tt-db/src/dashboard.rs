use chrono::{DateTime, Utc};
use rusqlite::params;
use tt_core::AgentSession;

use super::{Database, DbError, format_timestamp};

/// An agent session whose latest agent activity is within a requested window.
#[derive(Debug, Clone)]
pub struct ActiveAgentSession {
    pub session: AgentSession,
    pub machine_id: Option<String>,
    pub stream_id: Option<String>,
    pub last_activity: DateTime<Utc>,
}

impl Database {
    /// Lists agent sessions with agent activity at or after `since`.
    ///
    /// Liveness is defined by recent activity alone — deliberately not by
    /// `end_time`, which the `OpenCode` extractor rewrites to the newest message
    /// time on every scan, so a *running* session's `end_time` is always set and
    /// always roughly now. Filtering on `end_time IS NULL` returned an empty list
    /// while 16 sessions were making tool calls, which is how the dashboard's
    /// sessions card read "No active sessions" against a machine visibly running
    /// agents. The window (the allocation `agent_timeout`) is the same liveness
    /// rule allocation itself uses for an unclosed session.
    pub fn active_agent_sessions(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<ActiveAgentSession>, DbError> {
        let activities = {
            let mut statement = self.conn.prepare(
                "SELECT activity.session_id, activity.last_activity,
                        (SELECT stream_id FROM events
                         WHERE session_id = activity.session_id AND stream_id IS NOT NULL
                         ORDER BY timestamp DESC LIMIT 1)
                 FROM (
                     SELECT session_id, MAX(timestamp) AS last_activity
                     FROM events
                     WHERE session_id IS NOT NULL
                       AND type IN ('agent_session', 'agent_tool_use')
                       AND timestamp >= ?1
                     GROUP BY session_id
                 ) AS activity
                 ORDER BY activity.last_activity DESC",
            )?;
            let rows = statement.query_map(params![format_timestamp(since)], |row| {
                let last_activity: String = row.get(1)?;
                let last_activity = DateTime::parse_from_rfc3339(&last_activity)
                    .map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?
                    .with_timezone(&Utc);
                Ok((row.get::<_, String>(0)?, last_activity, row.get(2)?))
            })?;
            rows.collect::<Result<Vec<(String, DateTime<Utc>, Option<String>)>, _>>()?
        };

        let mut sessions = Vec::with_capacity(activities.len());
        for (session_id, last_activity, stream_id) in activities {
            let Some((session, machine_id)) = self.get_agent_session(&session_id)? else {
                continue;
            };
            sessions.push(ActiveAgentSession {
                session,
                machine_id,
                stream_id,
                last_activity,
            });
        }
        Ok(sessions)
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use tt_core::EventType;
    use tt_core::session::{AgentSession, SessionSource, SessionType};

    use super::*;
    use crate::StoredEvent;

    fn session_with_end_time(id: &str, end: DateTime<Utc>) -> AgentSession {
        AgentSession {
            session_id: id.to_owned(),
            source: SessionSource::OpenCode,
            parent_session_id: None,
            session_type: SessionType::User,
            project_path: "/home/x/p".to_owned(),
            project_name: "p".to_owned(),
            start_time: end - Duration::hours(1),
            end_time: Some(end),
            message_count: 5,
            summary: None,
            user_prompts: Vec::new(),
            starting_prompt: None,
            assistant_message_count: 3,
            tool_call_count: 7,
            user_message_timestamps: Vec::new(),
            tool_call_timestamps: Vec::new(),
        }
    }

    fn tool_use(id: &str, session_id: &str, timestamp: DateTime<Utc>) -> StoredEvent {
        StoredEvent {
            id: id.to_owned(),
            timestamp,
            event_type: EventType::AgentToolUse,
            source: "opencode".to_owned(),
            machine_id: None,
            schema_version: 1,
            data: serde_json::Value::Null,
            pane_id: None,
            tmux_session: None,
            window_index: None,
            cwd: None,
            git_project: None,
            git_workspace: None,
            status: None,
            idle_duration_ms: None,
            action: None,
            session_id: Some(session_id.to_owned()),
            stream_id: None,
            assignment_source: None,
            window_app_id: None,
            window_title: None,
        }
    }

    #[test]
    fn a_running_session_with_a_rewritten_end_time_is_still_active() {
        // Given: a session whose extractor rewrote `end_time` to the newest message
        // time on its last scan — the shape every RUNNING `OpenCode` session has —
        // with a tool call two minutes ago. Filtering on `end_time IS NULL` read
        // "No active sessions" against a machine running 16 agents.
        let db = Database::open_in_memory().unwrap();
        let now = Utc::now();
        db.upsert_agent_session(&session_with_end_time("ses_live", now), None)
            .unwrap();
        db.insert_event(&tool_use(
            "evt-live",
            "ses_live",
            now - Duration::minutes(2),
        ))
        .unwrap();
        // And: a session whose last activity predates the window entirely.
        db.upsert_agent_session(
            &session_with_end_time("ses_stale", now - Duration::hours(2)),
            None,
        )
        .unwrap();
        db.insert_event(&tool_use(
            "evt-stale",
            "ses_stale",
            now - Duration::hours(2),
        ))
        .unwrap();

        // When: asking for sessions active in the last 30 minutes.
        let active = db
            .active_agent_sessions(now - Duration::minutes(30))
            .unwrap();

        // Then: recent activity decides, not `end_time`.
        let ids: Vec<_> = active
            .iter()
            .map(|active| active.session.session_id.as_str())
            .collect();
        assert_eq!(ids, vec!["ses_live"]);
    }
}

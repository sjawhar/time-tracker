use chrono::{DateTime, Utc};
use rusqlite::params;
use tt_core::AgentSession;

use super::{Database, DbError, format_timestamp};

/// An unended agent session whose latest agent activity is within a requested window.
#[derive(Debug, Clone)]
pub struct ActiveAgentSession {
    pub session: AgentSession,
    pub machine_id: Option<String>,
    pub stream_id: Option<String>,
    pub last_activity: DateTime<Utc>,
}

impl Database {
    /// Lists unended agent sessions with agent activity at or after `since`.
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
            if session.end_time.is_none() {
                sessions.push(ActiveAgentSession {
                    session,
                    machine_id,
                    stream_id,
                    last_activity,
                });
            }
        }
        Ok(sessions)
    }
}

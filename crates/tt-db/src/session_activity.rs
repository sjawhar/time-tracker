//! Agent-session listing for a reporting window, scoped by observed activity.
//!
//! An `agent_sessions` row is not a trustworthy description of when its session
//! was alive. A fifth of the rows carry no `end_time` at all — the harness
//! records thousands more `started` markers than `ended` ones — and an
//! unterminated session read as "still running" overlaps every window after it.
//! That is how a report for a single day came to list 10,385 sessions and clamp
//! the top five to `24h 0m`, the window's own length: a session that opened in
//! April was still being counted into July.
//!
//! Bounding the nominal span by the last observed activity would fix only part
//! of it. Measured on real data, 642 rows carry a genuine multi-day span whose
//! *activity* is spread just as widely, so they would keep reporting the window
//! length however the span were capped.
//!
//! So a session is described here by the activity it actually emitted inside the
//! window, and its nominal span is never consulted. One rule covers both
//! populations and introduces no tunable: a session is listed only if it emitted
//! an event in the window, and its duration is the distance between its first
//! and last event *in that window*. The `24h 0m` artifact becomes structurally
//! impossible unless the session really was active at both ends of the day.

use chrono::{DateTime, Utc};
use rusqlite::params;
use tt_core::session::AgentSession;

use super::{AGENT_SESSION_COLUMNS, Database, DbError, format_timestamp};

#[cfg(test)]
mod tests;

/// Column index of `first_activity` in [`Database::agent_sessions_active_in_range`]'s
/// projection, which appends it after [`AGENT_SESSION_COLUMNS`].
const FIRST_ACTIVITY_COLUMN: usize = 15;
const LAST_ACTIVITY_COLUMN: usize = 16;

/// An agent session together with the activity it emitted inside one window.
#[derive(Debug, Clone)]
pub struct WindowedAgentSession {
    pub session: AgentSession,
    /// Timestamp of the first event this session emitted inside the window.
    pub first_activity: DateTime<Utc>,
    /// Timestamp of the last event this session emitted inside the window.
    pub last_activity: DateTime<Utc>,
}

impl WindowedAgentSession {
    /// How long the session was observably active inside the window.
    ///
    /// This can equal the window's length only if the session really did emit
    /// events at both ends of it — the property a window-clamped nominal span
    /// could not hold, and the reason that span reported every long-lived
    /// session as having filled the whole day.
    pub fn active_ms(&self) -> i64 {
        (self.last_activity - self.first_activity)
            .num_milliseconds()
            .max(0)
    }
}

/// Parses one `events.timestamp` column, which is always written RFC3339.
fn activity_column(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<DateTime<Utc>> {
    let raw: String = row.get(index)?;
    DateTime::parse_from_rfc3339(&raw)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

impl Database {
    /// Lists the agent sessions that emitted at least one event in `[start, end)`.
    ///
    /// **`end` is exclusive**, matching [`allocate_for_period`](super::allocate_for_period):
    /// an event at exactly `end` belongs to the next window.
    ///
    /// A session that merely *spans* the window without emitting anything inside
    /// it is absent, because it contributed nothing to the window. See the module
    /// docs for why the nominal span is not consulted at all.
    ///
    /// Any event carrying a `session_id` counts as activity. Only `agent_session`,
    /// `agent_tool_use` and `user_message` ever do, and all three are activity, so
    /// naming them would add a list to keep in step with no change in meaning.
    ///
    /// Sessions are ordered by `start_time` ascending. A session whose
    /// `agent_sessions` row is malformed is skipped with a warning rather than
    /// failing the listing, so one bad row cannot blank the whole section.
    pub fn agent_sessions_active_in_range(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<WindowedAgentSession>, DbError> {
        // `activity.session_id` is aliased so the unqualified names in
        // AGENT_SESSION_COLUMNS stay unambiguous across the join.
        let mut statement = self.conn.prepare(&format!(
            "SELECT {AGENT_SESSION_COLUMNS}, activity.first_activity, activity.last_activity
             FROM (
                 SELECT session_id AS activity_session_id,
                        MIN(timestamp) AS first_activity,
                        MAX(timestamp) AS last_activity
                 FROM events
                 WHERE session_id IS NOT NULL
                   AND timestamp >= ?1
                   AND timestamp < ?2
                 GROUP BY session_id
             ) AS activity
             JOIN agent_sessions ON agent_sessions.session_id = activity.activity_session_id
             ORDER BY agent_sessions.start_time"
        ))?;

        let mut rows = statement.query(params![format_timestamp(start), format_timestamp(end)])?;
        let mut sessions = Vec::new();
        while let Some(row) = rows.next()? {
            let parsed = Self::row_to_agent_session(row).and_then(|(session, _machine_id)| {
                Ok(WindowedAgentSession {
                    session,
                    first_activity: activity_column(row, FIRST_ACTIVITY_COLUMN)?,
                    last_activity: activity_column(row, LAST_ACTIVITY_COLUMN)?,
                })
            });
            match parsed {
                Ok(session) => sessions.push(session),
                Err(error) => {
                    tracing::warn!(%error, "skipping agent session with malformed columns");
                }
            }
        }
        Ok(sessions)
    }
}

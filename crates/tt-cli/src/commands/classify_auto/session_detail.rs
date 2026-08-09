//! The classifier's read access to a session, over `tt_db::Database`.
//!
//! `tt-llm` declares [`SessionDetail`] but cannot implement it — it depends on
//! rig-core and nothing internal, so it cannot reach the database. This is the other
//! half: the crate that already owns a `Database` supplies the answers, and the crate
//! that talks to the model stays free of internal dependencies.
//!
//! The connection lives behind a `Mutex` for one reason only: rig requires a tool to
//! be `Sync`, and `rusqlite::Connection` is `Send` but not `Sync`. This is *not* the
//! `Arc<Mutex<Database>>` the root `AGENTS.md` rules out for the daemon — that ban is
//! about sharing one connection across async tasks that hold it over `.await` points.
//! Here every method is sync, takes the lock and drops it before returning, and the
//! connection belongs to the classifier alone.

use std::path::Path;
use std::sync::{Arc, Mutex};

use tt_core::injection::is_injected;
use tt_db::Database;
use tt_llm::{MessagePage, SessionDetail, SessionDetailError, SessionOverview, SessionTools};

/// The injection denylist the classifier's fetches are filtered through.
///
/// Handing this to `tt_llm::SessionTools::new` is what keeps harness text out of the
/// model's view. It is the same arbiter every other user-message path uses; see
/// "Injected text is not attention" in the root `AGENTS.md`.
pub const INJECTION_FILTER: fn(&str) -> bool = is_injected;

/// The classifier's session access for a database, ready for `with_session_detail`.
///
/// Binding the provider to [`INJECTION_FILTER`] here, in the one place that builds it,
/// is what keeps the filter from being an optional step a caller can skip.
///
/// # Errors
/// When the database cannot be opened.
pub fn session_tools(path: &Path) -> Result<Arc<SessionTools>, tt_db::DbError> {
    Ok(SessionTools::new(
        Arc::new(DbSessionDetail::open(path)?) as Arc<dyn SessionDetail>,
        INJECTION_FILTER,
    ))
}

/// Session reads for a classifier, backed by its own database connection.
pub struct DbSessionDetail {
    db: Mutex<Database>,
}

impl DbSessionDetail {
    /// Opens a connection dedicated to answering the classifier's fetches.
    ///
    /// # Errors
    /// When the database cannot be opened.
    pub fn open(path: &Path) -> Result<Self, tt_db::DbError> {
        Ok(Self {
            db: Mutex::new(Database::open(path)?),
        })
    }

    /// Wraps an already-open connection, for tests.
    #[must_use]
    pub const fn from_database(db: Database) -> Self {
        Self { db: Mutex::new(db) }
    }

    fn session(
        &self,
        session_id: &str,
    ) -> Result<tt_core::session::AgentSession, SessionDetailError> {
        let db = self
            .db
            .lock()
            .map_err(|error| SessionDetailError::Backend(error.to_string()))?;
        db.get_agent_session(session_id)
            .map_err(|error| SessionDetailError::Backend(error.to_string()))?
            .map(|(session, _machine)| session)
            .ok_or_else(|| SessionDetailError::NotFound(session_id.to_owned()))
    }
}

impl SessionDetail for DbSessionDetail {
    fn overview(&self, session_id: &str) -> Result<SessionOverview, SessionDetailError> {
        let (session, machine) = {
            let db = self
                .db
                .lock()
                .map_err(|error| SessionDetailError::Backend(error.to_string()))?;
            db.get_agent_session(session_id)
                .map_err(|error| SessionDetailError::Backend(error.to_string()))?
                .ok_or_else(|| SessionDetailError::NotFound(session_id.to_owned()))?
        };
        Ok(SessionOverview {
            summary: session.summary,
            source: Some(session.source.as_str().to_owned()),
            project_path: Some(session.project_path),
            machine,
            started_at: Some(session.start_time),
            ended_at: session.end_time,
            message_count: i64::from(session.message_count),
            assistant_message_count: i64::from(session.assistant_message_count),
            tool_call_count: i64::from(session.tool_call_count),
        })
    }

    fn messages(
        &self,
        session_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<MessagePage, SessionDetailError> {
        let session = self.session(session_id)?;
        Ok(MessagePage {
            messages: session
                .user_prompts
                .iter()
                .skip(offset)
                .take(limit)
                .cloned()
                .collect(),
            offset,
            total: session.user_prompts.len(),
        })
    }
}

#[cfg(test)]
mod tests;

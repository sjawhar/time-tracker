//! What the classifier may pull from a session it is judging.
//!
//! This crate must stay free of internal dependencies (see the dependency graph in
//! the root `AGENTS.md`), so it cannot reach `tt-db` to read a session itself. It
//! declares *what* it may ask for here and lets the binary crates supply the answer:
//! `tt-cli` implements this over `tt_db::Database`, and tests implement it over a
//! literal.
//!
//! The method set is deliberately small, because each method had to earn its place
//! against what the default payload already carries and against what the schema can
//! actually serve. Two did:
//!
//! - [`SessionDetail::overview`] — the session's summary, timing and counts. The
//!   summary is the single highest-value fetch: it is present on 4,619 of the 4,721
//!   unclassified user sessions and on 2,352 of the 2,398 whose prompt runs under 50
//!   characters, and [`crate::ClassificationInput`] has no field for it, so today it
//!   never reaches the model at all.
//! - [`SessionDetail::messages`] — the stored user messages at full length. The
//!   default payload truncates each to 500 characters while the extractors store up
//!   to 2,000 bytes; 2,081 of those 4,721 sessions hold more text than the default
//!   shows.
//!
//! Three candidates were measured and rejected rather than stubbed:
//!
//! - **Tool-call names and the paths they touched.** Not available. The
//!   `tool_call_timestamps` on `tt_core::session::AgentSession` is an in-memory field
//!   that is never persisted, `agent_sessions` stores only a `tool_call_count`, and
//!   the `events` table has no payload column — an `agent_tool_use` row records that
//!   a call happened, never which tool or which file.
//! - **The session's distinct working directories.** Real but empty: 51,989 sessions
//!   have exactly one, 21 have more, and that one is already in the payload as `cwd`.
//! - **Temporally neighbouring sessions.** Only 19 of 400 sampled thin-prompt
//!   sessions had a *classified* neighbour within half an hour, because the backlog
//!   drains in bulk and the neighbours are unclassified too. Offering unclassified
//!   neighbours instead would bleed one session's subject onto another.

use chrono::{DateTime, Utc};

/// Why a detail lookup could not be answered.
#[derive(Debug, thiserror::Error)]
pub enum SessionDetailError {
    /// No session is indexed under this id.
    #[error("session {0} is not indexed")]
    NotFound(String),
    /// The backing store refused the read.
    #[error("session detail lookup failed: {0}")]
    Backend(String),
}

/// A session's shape: what the payload omits about the work itself.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionOverview {
    /// The extractor's own summary of the session, when its source writes one.
    ///
    /// `OpenCode` always does; Claude never does. A classifier must therefore treat an
    /// absent summary as normal and fall back to the messages, never as an error.
    pub summary: Option<String>,
    /// Which extractor indexed the session (`claude`, `opencode`).
    pub source: Option<String>,
    /// Directory the session ran in.
    pub project_path: Option<String>,
    /// Machine the session ran on.
    pub machine: Option<String>,
    /// When the session opened.
    pub started_at: Option<DateTime<Utc>>,
    /// When the session last showed activity.
    pub ended_at: Option<DateTime<Utc>>,
    /// Messages of every role.
    pub message_count: i64,
    /// Assistant replies alone.
    pub assistant_message_count: i64,
    /// Tool invocations the session made. Separates a typo from a day's build.
    pub tool_call_count: i64,
}

/// A slice of a session's stored user messages.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MessagePage {
    /// The messages in this slice, **unfiltered**.
    ///
    /// Implementors return whatever they stored and do no injection filtering of
    /// their own: [`crate::SessionTools`] applies the caller-supplied predicate to
    /// every message before rendering, so the filter cannot be forgotten by a new
    /// implementor.
    pub messages: Vec<String>,
    /// Index of the first message in this slice.
    pub offset: usize,
    /// How many stored messages exist in total, so a reader can tell whether asking
    /// for another page would return anything.
    pub total: usize,
}

/// Read-only access to one session, for a classifier deciding how to attribute it.
///
/// Sync on purpose. The `Classifier` trait is sync and `RigClassifier` drives async
/// rig on its own runtime, so an async method here would force every caller async for
/// no gain — and `tt-db` is sync regardless.
pub trait SessionDetail: Send + Sync {
    /// The session's summary, timing and counts.
    ///
    /// # Errors
    /// [`SessionDetailError::NotFound`] when no session carries that id;
    /// [`SessionDetailError::Backend`] when the store refuses the read.
    fn overview(&self, session_id: &str) -> Result<SessionOverview, SessionDetailError>;

    /// Up to `limit` stored user messages starting at `offset`, at full stored length.
    ///
    /// # Errors
    /// [`SessionDetailError::NotFound`] when no session carries that id;
    /// [`SessionDetailError::Backend`] when the store refuses the read.
    fn messages(
        &self,
        session_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<MessagePage, SessionDetailError>;
}

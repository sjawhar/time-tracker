//! The events one verdict applies to, and how a verdict reaches them.
//!
//! Sessions, rechecked sessions and bare window runs each answer the same four
//! questions differently — which rows a stream lands on, whether a human already
//! rejected this answer, whether one is already waiting on this target, and what scope
//! a proposal records — so the target owns that dispatch and the resolver is left
//! holding only policy.

use anyhow::{Context, Result};
use tt_db::Database;

/// The events one verdict applies to.
#[derive(Clone, Copy)]
pub(super) enum AssignmentTarget<'a> {
    /// A session reaching the classifier for the first time.
    Session {
        session_id: &'a str,
        prompt_count: u32,
    },
    /// A session revisited after it gained prompts.
    Recheck {
        session_id: &'a str,
        prompt_count: u32,
    },
    /// A run of window-focus events belonging to no session.
    Events(&'a [String]),
}

impl AssignmentTarget<'_> {
    /// Names the candidate in a log line.
    pub(super) fn label(&self) -> &str {
        match self {
            Self::Session { session_id, .. } | Self::Recheck { session_id, .. } => session_id,
            Self::Events(event_ids) => event_ids.first().map_or("window-run", String::as_str),
        }
    }

    /// Writes `stream_id` onto the events this target names, returning the count.
    ///
    /// A first pass claims only unassigned events; a recheck moves the ones the
    /// classifier itself placed. Neither touches an event a human, a todo link or the
    /// terminal-focus pass already spoke for.
    pub(super) fn assign(&self, db: &Database, stream_id: &str, source: &str) -> Result<u64> {
        let assigned = match self {
            Self::Session { session_id, .. } => db
                .assign_unassigned_events_by_session_id(session_id, stream_id, source)
                .context("assign unclassified session events")?,
            Self::Recheck { session_id, .. } => db
                .reassign_inferred_events_by_session_id(session_id, stream_id, source)
                .context("reassign inferred session events")?,
            Self::Events(event_ids) => db
                .assign_unassigned_events_by_ids(event_ids, stream_id, source)
                .context("assign unclassified window events")?,
        };
        self.record(db, assigned)?;
        Ok(assigned)
    }

    /// Notes how much of the session the classifier had seen, so a later pass knows
    /// when the session has gained enough prompts to be worth revisiting.
    fn record(&self, db: &Database, assigned: u64) -> Result<()> {
        let (Self::Session {
            session_id,
            prompt_count,
        }
        | Self::Recheck {
            session_id,
            prompt_count,
        }) = self
        else {
            return Ok(());
        };
        if assigned > 0 {
            db.record_classification(session_id, *prompt_count)
                .context("record how much of the session was classified")?;
        }
        Ok(())
    }

    /// Did a human already reject `stream_id` for this target?
    pub(super) fn was_rejected(&self, db: &Database, stream_id: &str) -> Result<bool> {
        match self {
            Self::Session { session_id, .. } | Self::Recheck { session_id, .. } => db
                .has_rejected_proposal(session_id, stream_id)
                .context("check rejected stream proposal"),
            Self::Events(_) => Ok(false),
        }
    }

    /// Did a human already reject minting a stream for this target?
    pub(super) fn was_new_stream_rejected(&self, db: &Database) -> Result<bool> {
        match self {
            Self::Session { session_id, .. } | Self::Recheck { session_id, .. } => db
                .has_rejected_new_stream_proposal(session_id)
                .context("check rejected new-stream proposal"),
            Self::Events(_) => Ok(false),
        }
    }

    /// Is an answer for this target already waiting on a human?
    ///
    /// The duplicate guard. Selection used to answer this question by excluding the
    /// candidate outright, which also froze it out of every later pass; asking here
    /// instead keeps the guard and drops the freeze. A proposal naming a dissolved
    /// stream counts for nothing on either side of the lookup, because no reviewer can
    /// act on it.
    pub(super) fn has_pending_proposal(&self, db: &Database) -> Result<bool> {
        match self {
            Self::Session { session_id, .. } | Self::Recheck { session_id, .. } => db
                .has_pending_proposal_for_session(session_id)
                .context("check pending session proposal"),
            Self::Events(event_ids) => Ok(db
                .get_pending_proposal_for_events(event_ids)
                .context("check pending window proposal")?
                .is_some()),
        }
    }

    /// Records which classifier last answered the questions waiting on this target.
    ///
    /// Paired with [`Self::has_pending_proposal`] rather than with an assignment, and
    /// that is the difference from [`Self::supersede_pending_proposals`]: superseding
    /// says the question is spent because the answer landed, while this says only that
    /// this classifier has now looked and produced nothing stronger. The proposal keeps
    /// its status and its content either way.
    ///
    /// Uniform across scopes on purpose, even though only the window-run gate reads it
    /// back. The column has one meaning — the newest generation that answered this
    /// question — and a stamp written for one scope and not the other would give it two.
    pub(super) fn stamp_pending_proposals(&self, db: &Database, generation: u32) -> Result<u64> {
        match self {
            Self::Session { session_id, .. } | Self::Recheck { session_id, .. } => db
                .stamp_pending_proposals_for_session(session_id, generation)
                .context("stamp the pending session proposal with this classifier"),
            Self::Events(event_ids) => db
                .stamp_pending_proposals_for_events(event_ids, generation)
                .context("stamp the pending window proposal with this classifier"),
        }
    }

    /// Retires the proposals waiting on this target, a verdict having answered them.
    ///
    /// Always paired with an assignment, never run on its own: the question the proposal
    /// asked is spent only because the answer has landed on the events.
    pub(super) fn supersede_pending_proposals(&self, db: &Database) -> Result<u64> {
        match self {
            Self::Session { session_id, .. } | Self::Recheck { session_id, .. } => db
                .supersede_pending_proposals_for_session(session_id)
                .context("supersede answered session proposals"),
            Self::Events(event_ids) => db
                .supersede_pending_proposals_for_events(event_ids)
                .context("supersede answered window proposals"),
        }
    }

    /// Splits into the `(session, events)` scope a proposal records.
    pub(super) fn proposal_scope(&self) -> (Option<String>, Option<Vec<String>>) {
        match self {
            Self::Session { session_id, .. } | Self::Recheck { session_id, .. } => {
                (Some((*session_id).to_string()), None)
            }
            Self::Events(event_ids) => (None, Some(event_ids.to_vec())),
        }
    }
}

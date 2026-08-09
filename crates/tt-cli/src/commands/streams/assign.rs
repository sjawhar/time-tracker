//! `tt streams assign <ref> --session <id>... --event <id>...` — record a human's
//! verdict that specific work belongs to a specific stream.
//!
//! This is the correction surface, and it is deliberately the narrow opposite of the
//! bulk-inference blob it replaces. It names events one way only — by session id or by
//! event id, both of which the human must supply — so there is no rule here that could
//! sweep up work nobody looked at. No cwd patterns, no time ranges, no stream creation:
//! a target that does not already exist is an error, not an invitation to mint one.
//!
//! Everything it writes is `assignment_source = "user"`, which every machine writer in
//! the codebase refuses to overwrite. That is why the surface has to stay this small —
//! a wrong `user` row is the one kind of mistake the classifier cannot later repair.
//!
//! Which work belongs where is a human judgement, so this command holds no list of its
//! own and infers nothing.

use std::collections::HashSet;
use std::fmt::Write;

use anyhow::{Context, Result, bail};
use tt_core::todos::TodoFileItem;
use tt_db::Database;

use super::format_stream_label;
use crate::Config;
use crate::todo_store::{load_mutating, write_todos};

#[cfg(test)]
mod tests;

/// The outcome of one correction.
#[derive(Debug)]
struct Assigned {
    stream: tt_db::Stream,
    events_moved: u64,
    /// Session ids that moved, for todos that reference them.
    sessions_moved: Vec<String>,
}

/// Applies a human's assignment to the named sessions and events.
fn assign_to(
    db: &Database,
    stream_ref: &str,
    sessions: &[String],
    events: &[String],
) -> Result<Assigned> {
    if sessions.is_empty() && events.is_empty() {
        bail!("name at least one --session or --event to assign");
    }
    let Some(stream) = db
        .resolve_stream(stream_ref)
        .with_context(|| format!("failed to resolve stream '{stream_ref}'"))?
    else {
        bail!(
            "no stream matching '{stream_ref}' (tried id, slug, exact name); \
             create it with 'tt streams create' first"
        );
    };

    let mut events_moved = 0u64;
    let mut sessions_moved = Vec::new();
    for session_id in sessions {
        let moved = db
            .reassign_session_as_user(session_id, &stream.id)
            .with_context(|| format!("failed to assign session {session_id}"))?;
        if moved > 0 {
            events_moved += moved;
            sessions_moved.push(session_id.clone());
        }
    }

    if !events.is_empty() {
        // Explicit ids carry the human's verdict too, so this uses the unguarded
        // pair-wise write rather than `assign_events_by_ids`, which would skip rows a
        // previous correction already claimed.
        let pairs: Vec<(String, String)> = events
            .iter()
            .map(|event_id| (event_id.clone(), stream.id.clone()))
            .collect();
        events_moved += db
            .assign_events_to_stream(&pairs, "user")
            .with_context(|| format!("failed to assign {} event(s)", events.len()))?;
    }

    if events_moved > 0 {
        // A correction invalidates the stream's totals rather than refreshing them —
        // only `tt recompute` writes those. Same contract as `tt streams merge`.
        db.mark_streams_for_recompute(&[stream.id.as_str()])
            .with_context(|| format!("failed to mark stream {} for recompute", stream.id))?;
    }

    Ok(Assigned {
        stream,
        events_moved,
        sessions_moved,
    })
}

/// Fills in the stream slug on todos that link a session which just moved.
///
/// A todo naming a session is a human's own mapping, so a human's assignment of that
/// session answers the todo's stream too. Only untouched todos are filled — an existing
/// `stream` is a verdict this must not overwrite.
fn backfill_linked_todos(config: &Config, assigned: &Assigned) -> Result<Vec<String>> {
    let Some(slug) = assigned.stream.slug.as_ref() else {
        return Ok(Vec::new());
    };
    if assigned.sessions_moved.is_empty() {
        return Ok(Vec::new());
    }
    let moved: HashSet<&str> = assigned.sessions_moved.iter().map(String::as_str).collect();

    let mut loaded = load_mutating(config)?;
    let mut lines = Vec::new();
    for file_line in &mut loaded.store.todos.items {
        let TodoFileItem::Todo(todo) = &mut file_line.item else {
            continue;
        };
        if todo.stream.is_some() || todo.done {
            continue;
        }
        if !todo
            .sessions
            .iter()
            .any(|session| moved.contains(session.as_str()))
        {
            continue;
        }
        todo.stream = Some(slug.clone());
        lines.push(format!("Backfilled stream '{slug}' → {}", todo.id));
    }

    if !lines.is_empty() {
        write_todos(config, &loaded.store.todos)?;
    }
    Ok(lines)
}

/// Renders the confirmation for one correction.
fn report(assigned: &Assigned, backfilled: &[String]) -> String {
    let label = format_stream_label(assigned.stream.name.as_deref(), &assigned.stream.id);
    if assigned.events_moved == 0 {
        return format!("No events matched. {label} is unchanged.\n");
    }

    let mut message = format!(
        "Assigned {} event(s) to {label} as a user assignment.\n",
        assigned.events_moved
    );
    for line in backfilled {
        let _ = writeln!(message, "{line}");
    }
    let _ = write!(
        message,
        "Stream times are stale until 'tt recompute'.\n\
         A user assignment is never overwritten by the classifier.\n"
    );
    message
}

/// Runs the assign command.
pub fn assign(
    db: &Database,
    config: &Config,
    stream_ref: &str,
    sessions: &[String],
    events: &[String],
) -> Result<()> {
    let assigned = assign_to(db, stream_ref, sessions, events)?;
    let backfilled = backfill_linked_todos(config, &assigned)?;
    print!("{}", report(&assigned, &backfilled));
    Ok(())
}

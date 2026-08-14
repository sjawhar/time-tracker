//! Releases junk attribution from sessions that outgrew the junk rule.
//!
//! `tt_core::is_structurally_junk` is `tool_call_count = 0 AND message_count <= 2`, and it
//! is evaluated once. Nothing revisits the verdict: `get_recheck_candidates` grants each
//! session exactly one re-check, and once a session's events carry the junk stream they are
//! no longer `NULL`, so `unclassified_user_sessions` never selects it again. A session that
//! opens with `Hello` and goes on to make 17 tool calls therefore stays filed as "no
//! attributable work" permanently.
//!
//! Measured live before this existed: **4,387 junked sessions violate the rule**, holding
//! **125,968 events** between them. Their unassigned *attention* is small — 1,036 events,
//! 14 of them inside the current week — so this repairs stream and delegated attribution
//! rather than the direct-time headline, and must not be presented as moving the latter.

use std::fmt::Write;

use anyhow::Result;
use tt_db::{Database, ReleaseMode, ReleaseOutcome};

fn format_release(outcome: ReleaseOutcome, mode: ReleaseMode) -> Result<String> {
    let mut output = String::new();
    match mode {
        ReleaseMode::DryRun => {
            writeln!(output, "RELEASE OUTGROWN JUNK (dry run — nothing written)")?;
        }
        ReleaseMode::Apply => writeln!(output, "RELEASE OUTGROWN JUNK")?,
    }
    writeln!(output)?;
    let release_label = match mode {
        ReleaseMode::DryRun => "Would release",
        ReleaseMode::Apply => "Released",
    };
    writeln!(output, "{release_label:<18}{:>8}", outcome.released)?;
    writeln!(
        output,
        "{:<18}{:>8}  (assigned by a human — never touched)",
        "Retained", outcome.retained
    )?;
    // Sessions rather than streams: everything selected sits on the one junk stream, so a
    // stream count would always be 1. How many sessions return to the classifier is the
    // figure that describes the blast radius.
    writeln!(
        output,
        "{:<18}{:>8}  (returned to the classifier)",
        "Sessions", outcome.streams_affected
    )?;
    writeln!(output)?;
    if outcome.released == 0 {
        writeln!(output, "Nothing to release.")?;
    } else {
        writeln!(
            output,
            "No event row was deleted, and the junk stream is not retired — it still holds"
        )?;
        writeln!(output, "the sessions that never outgrew the rule.")?;
        match mode {
            ReleaseMode::DryRun => {
                writeln!(
                    output,
                    "Nothing was written. Re-run without --dry-run to apply."
                )?;
            }
            ReleaseMode::Apply => {
                writeln!(
                    output,
                    "Released events await re-attribution; their classification record was"
                )?;
                writeln!(output, "forgotten so the classifier will re-reach them.")?;
            }
        }
    }
    Ok(output)
}

pub fn release_outgrown_junk(db: &Database, mode: ReleaseMode) -> Result<()> {
    let outcome = db.release_outgrown_junk(mode)?;
    print!("{}", format_release(outcome, mode)?);
    Ok(())
}

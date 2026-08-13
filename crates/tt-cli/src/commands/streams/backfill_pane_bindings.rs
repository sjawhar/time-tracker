//! `tt streams backfill-pane-bindings` — restore durable pane identities to past focus events.

use std::fmt::Write;

use anyhow::Result;
use tt_db::{Database, PaneSessionBindingBackfillOutcome, ReleaseMode};

fn format_backfill(
    outcome: PaneSessionBindingBackfillOutcome,
    mode: ReleaseMode,
) -> Result<String> {
    let mut output = String::new();
    match mode {
        ReleaseMode::DryRun => {
            writeln!(output, "BACKFILL PANE BINDINGS (dry run — nothing written)")?;
        }
        ReleaseMode::Apply => writeln!(output, "BACKFILL PANE BINDINGS")?,
    }
    writeln!(output)?;
    writeln!(
        output,
        "Reuses only a recent session identity observed in the same pane; no stream is inferred."
    )?;
    writeln!(output)?;
    let bound_label = match mode {
        ReleaseMode::DryRun => "Would bind",
        ReleaseMode::Apply => "Bound",
    };
    writeln!(output, "{bound_label:<18}{:>8}", outcome.bound)?;
    writeln!(
        output,
        "{:<18}{:>8}  (assigned by a human — never touched)",
        "Retained", outcome.retained
    )?;
    writeln!(output)?;
    writeln!(output, "No event row was deleted.")?;
    match mode {
        ReleaseMode::DryRun => writeln!(
            output,
            "Nothing was written. Re-run without --dry-run to apply."
        )?,
        ReleaseMode::Apply => writeln!(
            output,
            "Bound events await the existing session-membership attribution pass. \
             Run 'tt ingest sessions' to claim already-classified sessions."
        )?,
    }
    Ok(output)
}

/// Runs the durable pane identity backfill command.
pub fn backfill_pane_session_bindings(db: &Database, mode: ReleaseMode) -> Result<()> {
    let outcome = db.backfill_pane_session_bindings(mode)?;
    print!("{}", format_backfill(outcome, mode)?);
    Ok(())
}

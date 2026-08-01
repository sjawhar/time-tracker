//! `tt streams release-pane-focus` — release attribution no pane could have earned.
//!
//! The cwd propagator was deleted and 777,583 of its assignments were released, but
//! it wrote `assignment_source = 'inferred'` — the classifier's own value — so a
//! source-based sweep could not tell its rows from real classifications. One
//! population survived that cleanup: `tmux_pane_focus` events with no `session_id`.
//!
//! Such an event exposes nothing any writer in this tree reads. It carries no window
//! title (0 of 102,748) and no `window_app_id`, so the window-run classifier has no
//! evidence — and never sees one anyway, because the query behind it selects
//! `type = 'window_focus'`. Without a session id every session-keyed writer filters it
//! out. The `terminal_focus` and `artifact_reference` passes are `window_focus`-only.
//! So a stream on one of these rows was put there by the propagator, and it is still
//! inflating the direct time reported for whatever container it names.
//!
//! This is the undo, and it is a command rather than a primitive for the same reason
//! `tt streams dissolve` is: bulk release needs the same deliberateness as bulk
//! inference. The selection rule lives in `tt_db::release_unattributable_pane_focus`,
//! hardcoded, so nothing here can widen it.
//!
//! Streams are never retired. These events were filed under containers that may hold
//! legitimate work too, and judging a stream stays `tt streams dissolve`.

use std::fmt::Write;

use anyhow::Result;
use tt_db::{Database, ReleaseMode, ReleaseOutcome};

/// Why a release found nothing, told apart by whether the human guard held rows back.
///
/// A run that retained rows did have candidates, so reporting that everything was
/// legitimately placed would credit the classifier with the guard's work.
const fn nothing_to_release(retained: u64) -> &'static str {
    if retained > 0 {
        "Nothing to release — every remaining candidate carries a human's assignment."
    } else {
        "Nothing to release — every pane focus is unattributed or legitimately placed."
    }
}

/// Renders what the release did, or would do.
fn format_release(outcome: ReleaseOutcome, mode: ReleaseMode) -> Result<String> {
    let mut output = String::new();

    match mode {
        ReleaseMode::DryRun => writeln!(output, "RELEASE PANE FOCUS (dry run — nothing written)")?,
        ReleaseMode::Apply => writeln!(output, "RELEASE PANE FOCUS")?,
    }
    writeln!(output)?;
    writeln!(
        output,
        "Sessionless tmux pane focus, which no writer in this tree could have attributed."
    )?;
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
    writeln!(
        output,
        "{:<18}{:>8}",
        "Streams affected", outcome.streams_affected
    )?;
    writeln!(output)?;

    if outcome.released == 0 {
        writeln!(output, "{}", nothing_to_release(outcome.retained))?;
        return Ok(output);
    }

    writeln!(output, "No event row was deleted.")?;
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
                "Released events await re-attribution. \
                 Run 'tt recompute' to refresh stream totals."
            )?;
        }
    }

    Ok(output)
}

/// Runs the release command.
pub fn release_pane_focus(db: &Database, mode: ReleaseMode) -> Result<()> {
    let outcome = db.release_unattributable_pane_focus(mode)?;
    print!("{}", format_release(outcome, mode)?);
    Ok(())
}

#[cfg(test)]
mod tests;

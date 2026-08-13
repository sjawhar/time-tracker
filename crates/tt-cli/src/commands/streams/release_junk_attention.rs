use std::fmt::Write;

use anyhow::Result;
use tt_db::{Database, ReleaseMode, ReleaseOutcome};

fn format_release(outcome: ReleaseOutcome, mode: ReleaseMode) -> Result<String> {
    let mut output = String::new();
    match mode {
        ReleaseMode::DryRun => {
            writeln!(output, "RELEASE JUNK ATTENTION (dry run — nothing written)")?;
        }
        ReleaseMode::Apply => writeln!(output, "RELEASE JUNK ATTENTION")?,
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
    writeln!(
        output,
        "{:<18}{:>8}",
        "Streams affected", outcome.streams_affected
    )?;
    writeln!(output)?;
    if outcome.released == 0 {
        writeln!(output, "Nothing to release.")?;
    } else {
        writeln!(output, "No event row was deleted.")?;
        match mode {
            ReleaseMode::DryRun => {
                writeln!(
                    output,
                    "Nothing was written. Re-run without --dry-run to apply."
                )?;
            }
            ReleaseMode::Apply => {
                writeln!(output, "Released events await re-attribution.")?;
            }
        }
    }
    Ok(output)
}

pub fn release_junk_attention(db: &Database, mode: ReleaseMode) -> Result<()> {
    let outcome = db.release_junk_attention(mode)?;
    print!("{}", format_release(outcome, mode)?);
    Ok(())
}

#[cfg(test)]
mod tests;

//! `tt streams merge <from>... --into <to>` — collapse streams onto one row.
//!
//! The counterpart to dissolution. Dissolving says *this was never work*; merging
//! says *this was work, in a stream that already exists*. It is what repairs a real
//! initiative that was minted once per week: strip the week suffixes with
//! `tt streams rename`, then collapse the rows that now share a name here.
//!
//! Events a human assigned move too, keeping their `assignment_source`. A merge
//! changes which row holds the work, never the human's judgement about what the
//! work was, so releasing those events the way dissolution does would discard a
//! verdict that is still correct.
//!
//! Which streams belong together is a human judgement made per invocation, so the
//! command takes references and holds no opinion of its own.

use std::collections::HashSet;
use std::fmt::Write;

use anyhow::{Context, Result, bail};
use tt_db::{Database, MergeMode, MergedSource};

use super::format_stream_label;
use crate::commands::util::plural;

/// One source stream's contribution, labelled for the report.
#[derive(Debug)]
struct MergedRow {
    name: String,
    id_short: String,
    source: MergedSource,
}

/// Resolves a reference or fails naming it.
fn resolve(db: &Database, stream_ref: &str) -> Result<tt_db::Stream> {
    db.resolve_stream(stream_ref)
        .with_context(|| format!("failed to resolve stream '{stream_ref}'"))?
        .with_context(|| format!("no stream matching '{stream_ref}' (tried id, slug, exact name)"))
}

/// Resolves every reference up front, then merges them all in one transaction.
///
/// Resolution comes first so an unusable reference aborts the invocation instead of
/// merging whichever streams happened to be named ahead of it. References landing
/// on the same stream collapse to one entry, which keeps the reported totals
/// honest. Returns the target's label alongside the per-source outcomes.
fn merge_all(
    db: &Database,
    from_refs: &[String],
    into_ref: &str,
    mode: MergeMode,
) -> Result<(String, Vec<MergedRow>)> {
    let target = resolve(db, into_ref)?;

    let mut seen = HashSet::new();
    let mut sources = Vec::new();
    for from_ref in from_refs {
        let source = resolve(db, from_ref)?;
        if source.id == target.id {
            bail!(
                "'{from_ref}' and '{into_ref}' are the same stream — cannot merge it into itself"
            );
        }
        if seen.insert(source.id.clone()) {
            sources.push(source);
        }
    }

    let from_ids: Vec<String> = sources.iter().map(|source| source.id.clone()).collect();
    let merged = db
        .merge_streams(&from_ids, &target.id, mode)
        .with_context(|| format!("failed to merge into stream {}", target.id))?;

    let rows = sources
        .into_iter()
        .zip(merged)
        .map(|(stream, source)| MergedRow {
            name: stream.name.unwrap_or_else(|| "(unnamed)".to_string()),
            id_short: stream.id.chars().take(8).collect(),
            source,
        })
        .collect();
    Ok((
        format_stream_label(target.name.as_deref(), &target.id),
        rows,
    ))
}

/// What became of the source stream row.
const fn fate(retired: bool, mode: MergeMode) -> &'static str {
    match (mode, retired) {
        (MergeMode::DryRun, true) => "would be retired",
        (MergeMode::DryRun, false) => "would be kept",
        (MergeMode::Apply, true) => "retired",
        (MergeMode::Apply, false) => "kept",
    }
}

/// Renders one row per source stream, then the invocation's totals.
fn format_merged(target: &str, rows: &[MergedRow], mode: MergeMode) -> Result<String> {
    let mut output = String::new();
    match mode {
        MergeMode::DryRun => writeln!(output, "MERGE (dry run — nothing written)")?,
        MergeMode::Apply => writeln!(output, "MERGE")?,
    }
    writeln!(output)?;
    writeln!(output, "Into: {target}")?;
    writeln!(output)?;
    writeln!(
        output,
        "{:<42}  {:<8}  {:>7}  {:>5}  {:>5}  Outcome",
        "Source", "ID", "Events", "Human", "Tags"
    )?;
    writeln!(
        output,
        "──────────────────────────────────────────  ────────  ───────  \
         ─────  ─────  ────────────────"
    )?;

    for row in rows {
        // Truncate by characters, not bytes, to avoid panics on multi-byte UTF-8.
        let name = if row.name.chars().count() > 42 {
            format!("{}...", row.name.chars().take(39).collect::<String>())
        } else {
            row.name.clone()
        };
        writeln!(
            output,
            "{name:<42}  {:<8}  {:>7}  {:>5}  {:>5}  {}",
            row.id_short,
            row.source.events_moved,
            row.source.user_events_moved,
            row.source.tags_moved,
            fate(row.source.retired, mode)
        )?;
    }

    let events: u64 = rows.iter().map(|row| row.source.events_moved).sum();
    let human: u64 = rows.iter().map(|row| row.source.user_events_moved).sum();
    let tags: u64 = rows.iter().map(|row| row.source.tags_moved).sum();
    let retired = rows.iter().filter(|row| row.source.retired).count();

    writeln!(output)?;
    let move_verb = match mode {
        MergeMode::DryRun => "would move",
        MergeMode::Apply => "moved",
    };
    writeln!(
        output,
        "Total: {} {move_verb} ({human} assigned by hand), {}, \
         {retired} of {} streams {}.",
        plural(events, "event"),
        plural(tags, "tag"),
        rows.len(),
        fate(true, mode)
    )?;
    output.push_str(&format_repointed(rows, mode)?);
    match mode {
        MergeMode::DryRun => writeln!(
            output,
            "Nothing was written. Re-run without --dry-run to apply."
        )?,
        MergeMode::Apply => writeln!(
            output,
            "The target's totals are now stale. Run 'tt recompute' to refresh them."
        )?,
    }
    Ok(output)
}

/// Renders the pending proposals this merge re-points at the target.
///
/// Empty when none moved, so a merge that touches no proposal says nothing about
/// proposals at all — the same shape as the stranded-link block in `dissolve`.
fn format_repointed(rows: &[MergedRow], mode: MergeMode) -> Result<String> {
    let repointed: u64 = rows.iter().map(|row| row.source.proposals_repointed).sum();
    if repointed == 0 {
        return Ok(String::new());
    }
    let mut output = String::new();
    writeln!(output)?;
    let move_verb = match mode {
        MergeMode::DryRun => "would be re-pointed",
        MergeMode::Apply => "re-pointed",
    };
    writeln!(
        output,
        "{} {move_verb} at the target, so accepting one lands its events there.",
        plural(repointed, "pending proposal")
    )?;
    writeln!(
        output,
        "Proposals a human already decided are historical records and are left alone."
    )?;
    Ok(output)
}

/// Runs the merge command.
pub fn merge(db: &Database, from_refs: &[String], into_ref: &str, mode: MergeMode) -> Result<()> {
    let (target, rows) = merge_all(db, from_refs, into_ref, mode)?;
    print!("{}", format_merged(&target, &rows, mode)?);
    Ok(())
}

#[cfg(test)]
mod tests;

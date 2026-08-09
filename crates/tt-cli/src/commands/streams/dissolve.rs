//! `tt streams dissolve` — release a stream's events back to unassigned.
//!
//! Dissolution is the undo for a container that should never have been minted:
//! an activity type, a date range, a catch-all. The events it swallowed are
//! returned to the unassigned pool, where the terminal-focus pass and the
//! classifier can reach them — both only ever read `stream_id IS NULL`.
//!
//! Deciding which streams deserve this is a human judgement made per
//! invocation, so the command takes references and holds no opinion of its own.
//!
//! Retiring a stream can strand references held outside the database. `streams.md`
//! maps streams to priorities by slug or by exact name, and it is hand-edited, so
//! dissolution silently leaves those lines pointing at nothing. The report names
//! every link it breaks — in dry-run too, because that is where the operator decides.
//! It never edits the file: the mapping is the user's record, and only the user can
//! say whether a line is stale.
//!
//! Pending proposals are the other reference held outside the foreign key:
//! `proposals.proposed_stream_id` has none, so a retired stream leaves rows no reviewer
//! can accept. They are named for the same reason, and left exactly as they are —
//! dissolution says the work never happened, so there is no stream to re-point them to.

use std::collections::HashSet;
use std::fmt::Write;

use anyhow::{Context, Result, bail};
use tt_core::todos::StreamFileItem;
use tt_db::{Database, DissolveMode, DissolveOutcome, StrandedProposal};

use crate::Config;
use crate::commands::util::plural;
use crate::todo_store::load_read_only;

/// One stream's dissolution, labelled for the report.
#[derive(Debug)]
struct Dissolved {
    name: String,
    /// The stream's full id: rendered truncated, and matched whole against the
    /// `proposed_stream_id` of the pending proposals this dissolution strands.
    id: String,
    /// The forms a `streams.md` line may use to name this stream: its slug and its
    /// display name. The file uses both, so a link check must try both.
    link_keys: Vec<String>,
    outcome: DissolveOutcome,
}

/// One `streams.md` link left pointing at a stream this invocation retires.
#[derive(Debug)]
struct StrandedLink {
    line_number: usize,
    stream: String,
    priority: String,
}

/// Resolves every reference up front, then dissolves each stream in turn.
///
/// Resolution comes first so an unusable reference aborts the invocation
/// instead of dissolving whichever streams happened to be named ahead of it.
/// References that land on the same stream collapse to one entry, which keeps
/// the reported totals honest.
fn dissolve_all(
    db: &Database,
    stream_refs: &[String],
    mode: DissolveMode,
) -> Result<Vec<Dissolved>> {
    let mut seen = HashSet::new();
    let mut targets = Vec::new();
    for stream_ref in stream_refs {
        let Some(stream) = db
            .resolve_stream(stream_ref)
            .with_context(|| format!("failed to resolve stream '{stream_ref}'"))?
        else {
            bail!("no stream matching '{stream_ref}' (tried id, slug, exact name)");
        };
        if seen.insert(stream.id.clone()) {
            targets.push(stream);
        }
    }

    targets
        .into_iter()
        .map(|stream| {
            let outcome = db
                .dissolve_stream(&stream.id, mode)
                .with_context(|| format!("failed to dissolve stream {}", stream.id))?;
            Ok(Dissolved {
                link_keys: stream.slug.into_iter().chain(stream.name.clone()).collect(),
                name: stream.name.unwrap_or_else(|| "(unnamed)".to_string()),
                id: stream.id,
                outcome,
            })
        })
        .collect()
}

/// What became of the stream row itself.
const fn fate(retired: bool, mode: DissolveMode) -> &'static str {
    match (mode, retired) {
        (DissolveMode::DryRun, true) => "would be retired",
        (DissolveMode::DryRun, false) => "would be kept",
        (DissolveMode::Apply, true) => "retired",
        (DissolveMode::Apply, false) => "kept",
    }
}

/// The leading 8 characters of an id, the form every report and `tt proposals` use.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Renders one row per dissolved stream, then the invocation's totals.
fn format_dissolved(
    entries: &[Dissolved],
    stranded: &[StrandedLink],
    proposals: &[StrandedProposal],
    mode: DissolveMode,
) -> Result<String> {
    let mut output = String::new();

    match mode {
        DissolveMode::DryRun => writeln!(output, "DISSOLVE (dry run — nothing written)")?,
        DissolveMode::Apply => writeln!(output, "DISSOLVE")?,
    }
    writeln!(output)?;
    writeln!(
        output,
        "{:<30}  {:<8}  {:>8}  {:>8}  Outcome",
        "Name", "ID", "Released", "Retained"
    )?;
    writeln!(
        output,
        "──────────────────────────────  ────────  ────────  ────────  ────────────────"
    )?;

    for entry in entries {
        // Truncate by characters, not bytes, to avoid panics on multi-byte UTF-8
        let name = if entry.name.chars().count() > 30 {
            format!("{}...", entry.name.chars().take(27).collect::<String>())
        } else {
            entry.name.clone()
        };
        writeln!(
            output,
            "{name:<30}  {:<8}  {:>8}  {:>8}  {}",
            short_id(&entry.id),
            entry.outcome.released,
            entry.outcome.retained,
            fate(entry.outcome.retired, mode)
        )?;
    }

    let released: u64 = entries.iter().map(|entry| entry.outcome.released).sum();
    let retained: u64 = entries.iter().map(|entry| entry.outcome.retained).sum();
    let retired = entries.iter().filter(|entry| entry.outcome.retired).count();

    writeln!(output)?;
    let release_verb = match mode {
        DissolveMode::DryRun => "would be released",
        DissolveMode::Apply => "released",
    };
    writeln!(
        output,
        "Total: {released} events {release_verb}, {retained} retained, \
         {retired} of {} streams {}.",
        entries.len(),
        fate(true, mode)
    )?;
    output.push_str(&format_stranded(stranded, mode)?);
    output.push_str(&format_stranded_proposals(proposals, mode)?);
    match mode {
        DissolveMode::DryRun => writeln!(
            output,
            "Nothing was written. Re-run without --dry-run to apply."
        )?,
        DissolveMode::Apply => writeln!(
            output,
            "Released events await re-attribution. Run 'tt recompute' to refresh stream totals."
        )?,
    }

    Ok(output)
}

/// Renders the `streams.md` links this invocation leaves pointing at nothing.
///
/// Empty when nothing is stranded, so a dissolution that breaks no link says nothing
/// about links at all.
fn format_stranded(stranded: &[StrandedLink], mode: DissolveMode) -> Result<String> {
    if stranded.is_empty() {
        return Ok(String::new());
    }
    let mut output = String::new();
    writeln!(output)?;
    match mode {
        DissolveMode::DryRun => writeln!(output, "Priority links that would dangle:")?,
        DissolveMode::Apply => writeln!(output, "Priority links now dangling:")?,
    }
    for link in stranded {
        writeln!(
            output,
            "  streams.md line {}: '{}' → priority '{}'",
            link.line_number, link.stream, link.priority
        )?;
    }
    writeln!(
        output,
        "Drift skips a dangling link and names it, so the verdict still computes."
    )?;
    writeln!(
        output,
        "Nothing was removed from streams.md — that mapping is yours to edit."
    )?;
    Ok(output)
}

/// Renders the pending proposals this invocation leaves naming a stream that is gone.
///
/// Empty when nothing is stranded, so a dissolution that strands no proposal says
/// nothing about proposals at all.
fn format_stranded_proposals(proposals: &[StrandedProposal], mode: DissolveMode) -> Result<String> {
    if proposals.is_empty() {
        return Ok(String::new());
    }
    let mut output = String::new();
    writeln!(output)?;
    match mode {
        DissolveMode::DryRun => writeln!(output, "Proposals that would be stranded:")?,
        DissolveMode::Apply => writeln!(output, "Proposals now stranded:")?,
    }
    for proposal in proposals {
        writeln!(
            output,
            "  proposal {} → stream {} ({})",
            short_id(&proposal.proposal_id),
            short_id(&proposal.stream_id),
            plural(proposal.event_count as u64, "event")
        )?;
    }
    writeln!(
        output,
        "'tt proposals ls' marks these (gone): they suppress nothing, and no reviewer \
         can accept one."
    )?;
    writeln!(
        output,
        "Nothing about them was changed — a dissolution has no stream to re-point a \
         proposal to."
    )?;
    Ok(output)
}

/// Reads `streams.md` and pairs each priority link with its 1-based line number.
///
/// `parse_streams` yields one item per source line, raw lines included, so the index
/// is the line number the operator will look for.
fn load_stream_links(config: &Config) -> Result<Vec<StrandedLink>> {
    let loaded = load_read_only(config)
        .context("failed to read the todo store to check for stream priority links")?;
    Ok(loaded
        .store
        .streams
        .items
        .iter()
        .enumerate()
        .filter_map(|(index, line)| match &line.item {
            StreamFileItem::Link(link) => Some(StrandedLink {
                line_number: index + 1,
                stream: link.stream.clone(),
                priority: link.priority.clone(),
            }),
            StreamFileItem::Raw(_) => None,
        })
        .collect())
}

/// Keeps the links naming one of the dissolved streams, in file order.
fn stranded_links(links: Vec<StrandedLink>, dissolved: &[Dissolved]) -> Vec<StrandedLink> {
    let keys: HashSet<&str> = dissolved
        .iter()
        .flat_map(|entry| entry.link_keys.iter().map(String::as_str))
        .collect();
    links
        .into_iter()
        .filter(|link| keys.contains(link.stream.as_str()))
        .collect()
}

/// Loads the pending proposals this invocation leaves naming a stream that is gone.
fn stranded_proposals(db: &Database, dissolved: &[Dissolved]) -> Result<Vec<StrandedProposal>> {
    let stream_ids: Vec<String> = dissolved.iter().map(|entry| entry.id.clone()).collect();
    db.pending_proposals_for_streams(&stream_ids)
        .context("failed to load the pending proposals naming the dissolved streams")
}

/// Dissolves the named streams and renders the report.
///
/// `streams.md` is read **before** anything is written, so an unreadable todo store
/// aborts the invocation instead of leaving a dissolution the operator was never told
/// about — the same discipline that resolves every stream reference up front.
fn dissolve_report(
    db: &Database,
    config: &Config,
    stream_refs: &[String],
    mode: DissolveMode,
) -> Result<String> {
    let links = load_stream_links(config)?;
    let dissolved = dissolve_all(db, stream_refs, mode)?;
    let stranded = stranded_links(links, &dissolved);
    let proposals = stranded_proposals(db, &dissolved)?;
    format_dissolved(&dissolved, &stranded, &proposals, mode)
}

/// Runs the dissolve command.
pub fn dissolve(
    db: &Database,
    config: &Config,
    stream_refs: &[String],
    mode: DissolveMode,
) -> Result<()> {
    print!("{}", dissolve_report(db, config, stream_refs, mode)?);
    Ok(())
}

#[cfg(test)]
mod tests;

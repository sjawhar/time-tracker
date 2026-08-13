//! `tt streams collapse-instances` — merge numbered execution instances by initiative.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use anyhow::{Context, Result};
use tt_core::{normalize_stream_name, strip_trailing_instance_qualifier};
use tt_db::{Database, MergeMode, MergedSource, Stream};

use crate::commands::util::plural;

/// One family collapsed onto its deterministically selected target.
#[derive(Debug)]
pub struct CollapsedInstanceFamily {
    pub target_id: String,
    target_name: String,
    member_count: usize,
    renamed: bool,
    sources: Vec<MergedSource>,
}

/// The complete result of one collapse invocation.
#[derive(Debug)]
pub struct CollapseInstanceFamiliesOutcome {
    pub groups: Vec<CollapsedInstanceFamily>,
    skipped: Vec<String>,
}

struct FamilyMember {
    stream: Stream,
    events: u64,
}

struct PlannedFamily {
    target_name: String,
    members: Vec<FamilyMember>,
}

fn instance_families(db: &Database) -> Result<(Vec<PlannedFamily>, Vec<String>)> {
    let streams = db.get_streams().context("failed to load streams")?;
    let named: Vec<(String, String)> = streams
        .iter()
        .filter_map(|stream| {
            stream
                .name
                .as_deref()
                .map(|name| (stream.id.clone(), normalize_stream_name(name)))
        })
        .collect();
    let streams_by_id: BTreeMap<String, Stream> = streams
        .into_iter()
        .map(|stream| (stream.id.clone(), stream))
        .collect();
    let mut families: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut skipped = Vec::new();

    for (id, name) in &named {
        if let Some(base) = strip_trailing_instance_qualifier(name) {
            let base = normalize_stream_name(base);
            if base.is_empty() {
                skipped.push(format!(
                    "{name} ({id}) has no initiative name before its qualifier"
                ));
            } else {
                families.entry(base).or_default().insert(id.clone());
            }
        }
    }

    let mut grouped = Vec::new();
    for (base, mut ids) in families {
        ids.extend(
            named
                .iter()
                .filter(|(_, name)| name == &base)
                .map(|(id, _)| id.clone()),
        );
        if ids.len() < 2 {
            continue;
        }
        let mut members = Vec::with_capacity(ids.len());
        for id in ids {
            let stream = streams_by_id
                .get(&id)
                .with_context(|| {
                    format!("stream {id} disappeared while planning instance families")
                })?
                .clone();
            let events = db
                .count_events_by_stream(&id)
                .with_context(|| format!("failed to count events for stream {id}"))?;
            members.push(FamilyMember { stream, events });
        }
        members.sort_by(|left, right| {
            right
                .events
                .cmp(&left.events)
                .then_with(|| left.stream.created_at.cmp(&right.stream.created_at))
                .then_with(|| left.stream.id.cmp(&right.stream.id))
        });
        grouped.push(PlannedFamily {
            target_name: base,
            members,
        });
    }
    Ok((grouped, skipped))
}

fn collapse_all(db: &Database, mode: MergeMode) -> Result<CollapseInstanceFamiliesOutcome> {
    let (families, skipped) = instance_families(db)?;
    let mut groups = Vec::with_capacity(families.len());

    for PlannedFamily {
        target_name,
        members,
    } in families
    {
        let target = &members[0].stream;
        let source_ids: Vec<String> = members
            .iter()
            .skip(1)
            .map(|member| member.stream.id.clone())
            .collect();
        let renamed = target.name.as_deref() != Some(target_name.as_str());
        if matches!(mode, MergeMode::Apply) && renamed {
            db.rename_stream(&target.id, &target_name)
                .with_context(|| format!("failed to rename stream {}", target.id))?;
        }
        let sources = db
            .merge_streams(&source_ids, &target.id, mode)
            .with_context(|| format!("failed to merge into stream {}", target.id))?;
        groups.push(CollapsedInstanceFamily {
            target_id: target.id.clone(),
            target_name,
            member_count: members.len(),
            renamed,
            sources,
        });
    }

    Ok(CollapseInstanceFamiliesOutcome { groups, skipped })
}

fn format_outcome(outcome: &CollapseInstanceFamiliesOutcome, mode: MergeMode) -> Result<String> {
    let mut output = String::new();
    match mode {
        MergeMode::DryRun => writeln!(output, "COLLAPSE INSTANCES (dry run — nothing written)")?,
        MergeMode::Apply => writeln!(output, "COLLAPSE INSTANCES")?,
    }
    writeln!(output)?;

    if outcome.groups.is_empty() {
        writeln!(output, "No unambiguous instance families found.")?;
    } else {
        writeln!(
            output,
            "{:<44}  {:<8}  {:>8}  {:>8}  {:>6}",
            "Initiative", "Target", "Members", "Moved", "Human"
        )?;
        writeln!(
            output,
            "────────────────────────────────────────────  ────────  ────────  ────────  ──────"
        )?;
        for group in &outcome.groups {
            let events: u64 = group.sources.iter().map(|source| source.events_moved).sum();
            let human: u64 = group
                .sources
                .iter()
                .map(|source| source.user_events_moved)
                .sum();
            let name = if group.target_name.chars().count() > 44 {
                format!(
                    "{}...",
                    group.target_name.chars().take(41).collect::<String>()
                )
            } else {
                group.target_name.clone()
            };
            writeln!(
                output,
                "{name:<44}  {:<8}  {:>8}  {:>8}  {:>6}",
                group.target_id.chars().take(8).collect::<String>(),
                group.member_count,
                events,
                human,
            )?;
        }
        let groups = outcome.groups.len();
        let streams: usize = outcome.groups.iter().map(|group| group.member_count).sum();
        let events: u64 = outcome
            .groups
            .iter()
            .flat_map(|group| &group.sources)
            .map(|source| source.events_moved)
            .sum();
        let renamed = outcome.groups.iter().filter(|group| group.renamed).count();
        writeln!(output)?;
        writeln!(
            output,
            "Total: {}, {streams} streams, {} moved, {renamed} targets renamed.",
            plural(groups as u64, "initiative"),
            plural(events, "event"),
        )?;
    }

    if !outcome.skipped.is_empty() {
        writeln!(output)?;
        writeln!(output, "Skipped:")?;
        for reason in &outcome.skipped {
            writeln!(output, "- {reason}")?;
        }
    }
    match mode {
        MergeMode::DryRun => writeln!(
            output,
            "Nothing was written. Re-run without --dry-run to apply."
        )?,
        MergeMode::Apply => writeln!(
            output,
            "No event was deleted. Run 'tt recompute' to refresh target totals."
        )?,
    }
    Ok(output)
}

/// Collapses every unambiguous family of numbered stream instances.
pub fn collapse_instance_families(
    db: &Database,
    mode: MergeMode,
) -> Result<CollapseInstanceFamiliesOutcome> {
    let outcome = collapse_all(db, mode)?;
    print!("{}", format_outcome(&outcome, mode)?);
    Ok(outcome)
}

#[cfg(test)]
mod tests;

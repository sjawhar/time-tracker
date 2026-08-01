use std::collections::{BTreeMap, HashSet};
use std::fmt::Write;

use anyhow::{Context, Result};
use chrono::{Local, Utc};
use tt_core::todos::{DriftReport, StreamPriorityLink, StreamTimeInput, compute_drift};
use tt_db::Database;

use crate::Config;
use crate::commands::report::{self, Period};
use crate::commands::todo::view::{priority_items, stream_links};
use crate::todo_store::load_read_only;

pub fn run(db: &Database, config: &Config, period: Period, json: bool) -> Result<()> {
    let generated_at = Utc::now();
    let reference_date = generated_at.with_timezone(&Local).date_naive();
    let timezone = iana_time_zone::get_timezone().unwrap_or_else(|_| "Etc/UTC".to_string());
    let report_data =
        report::generate_report_data_for_date(db, period, generated_at, reference_date, timezone)
            .context("failed to generate report data for todo drift")?;
    let loaded = load_read_only(config)?;
    let priorities = priority_items(&loaded);
    let links = stream_links(&loaded);
    let mut warnings = duplicate_stream_key_warnings(db)?;
    let (links, mut link_warnings) = resolve_stream_links(db, links)?;
    warnings.append(&mut link_warnings);
    let stream_times = stream_times_with_idle_named_streams(db, &report_data.streams)?;
    let drift =
        compute_drift(&priorities, &links, &stream_times).context("failed to compute drift")?;
    // A link naming a dissolved stream is skipped rather than fatal, so it has to be said
    // out loud here or the report quietly omits a priority's only link.
    warnings.extend(drift.dangling_stream_links.iter().map(|stream| {
        format!("streams.md reference '{stream}' names no stream; skipped (remove the line if it is stale)")
    }));
    if json {
        for warning in &warnings {
            eprintln!("{}", warning_line(warning));
        }
        println!("{}", serde_json::to_string_pretty(&drift)?);
    } else {
        print!("{}", render_human(&drift, &warnings)?);
    }
    Ok(())
}

fn render_human(drift: &DriftReport, warnings: &[String]) -> Result<String> {
    let mut output = String::new();
    writeln!(output, "TODO DRIFT").context("failed to format drift header")?;
    output.push_str(&render_warnings(warnings)?);
    writeln!(output).context("failed to format drift spacer")?;
    // No percentage of `direct + delegated`. Direct is wall-clock hours and delegated is
    // machine-hours summed across parallel agents, so their sum has no denominator and a
    // share of it silently equates one human hour with one agent hour. This table used to
    // print `Direct+Del` and `All time` columns doing exactly that -- `wo-005` read 6.0% of
    // attention and 0.5% of "all time", two numbers that cannot both be a proportion of
    // effort. `tt report`'s SUMMARY already reports the two separately plus the ratio;
    // Delegated and Leverage carry the same information here without the false sum.
    writeln!(
        output,
        "{:<18} {:>10} {:>10} {:>12} {:>12} {:>10}",
        "Priority", "Importance", "Direct", "Direct time", "Delegated", "Leverage"
    )
    .context("failed to format drift table header")?;
    for priority in &drift.priorities {
        writeln!(
            output,
            "{:<18} {:>9.1}% {:>9.1}% {:>12} {:>12} {:>10}",
            priority.priority_slug,
            percentage(priority.importance_share),
            percentage(priority.direct_share),
            report::format_duration(priority.direct_ms),
            report::format_duration(delegated_ms(
                priority.direct_ms,
                priority.direct_plus_delegated_ms
            )),
            leverage(priority.direct_ms, priority.direct_plus_delegated_ms)
        )
        .context("failed to format priority drift row")?;
    }
    writeln!(
        output,
        "{:<18} {:>10} {:>9.1}% {:>12} {:>12} {:>10}",
        "unattributed",
        "-",
        percentage(drift.unattributed.direct_share),
        report::format_duration(drift.unattributed.direct_ms),
        report::format_duration(delegated_ms(
            drift.unattributed.direct_ms,
            drift.unattributed.direct_plus_delegated_ms
        )),
        leverage(
            drift.unattributed.direct_ms,
            drift.unattributed.direct_plus_delegated_ms
        )
    )
    .context("failed to format unattributed drift row")?;
    Ok(output)
}

/// Recovers delegated time from the stored sum, which is what the report carries.
fn delegated_ms(direct_ms: i64, direct_plus_delegated_ms: i64) -> i64 {
    (direct_plus_delegated_ms - direct_ms).max(0)
}

/// Delegated over direct, the only honest way to relate the two.
///
/// Defers to `report::format_leverage`, which `tt report`'s SUMMARY already uses, so the
/// two surfaces cannot drift on how the ratio is computed or how the undefined case reads.
/// A priority with neither figure shows `-` rather than `n/a`: nothing happened at all,
/// which is different from agent time the user never attended.
fn leverage(direct_ms: i64, direct_plus_delegated_ms: i64) -> String {
    let delegated = delegated_ms(direct_ms, direct_plus_delegated_ms);
    if direct_ms <= 0 && delegated == 0 {
        return "-".to_string();
    }
    report::format_leverage(direct_ms, delegated)
}

fn render_warnings(warnings: &[String]) -> Result<String> {
    let mut output = String::new();
    for warning in warnings {
        writeln!(output, "{}", warning_line(warning)).context("failed to format drift warning")?;
    }
    Ok(output)
}

fn warning_line(warning: &str) -> String {
    format!("WARNING: {warning}")
}

fn duplicate_stream_key_warnings(db: &Database) -> Result<Vec<String>> {
    let streams = db
        .get_streams()
        .context("failed to get streams for todo drift duplicate-key warnings")?;
    let mut counts_by_key = BTreeMap::new();
    for stream in streams {
        if let Some(key) = stream.slug.or(stream.name) {
            *counts_by_key.entry(key).or_insert(0usize) += 1;
        }
    }
    Ok(counts_by_key
        .into_iter()
        .filter_map(|(key, count)| {
            (count > 1).then_some(format!(
                "DB stream key '{key}' appears more than once; times were combined"
            ))
        })
        .collect())
}

/// Resolves streams.md references to the key used by drift calculations.
///
/// A stream's key is its slug when present, otherwise its display name. Legacy display-name
/// references to one slugged stream are rewritten and reported; unknown or ambiguous references
/// are preserved so `compute_drift` continues to report them as errors.
fn resolve_stream_links(
    db: &Database,
    links: Vec<StreamPriorityLink>,
) -> Result<(Vec<StreamPriorityLink>, Vec<String>)> {
    let streams = db
        .get_streams()
        .context("failed to get streams for todo drift link resolution")?;
    let known_keys: HashSet<String> = streams
        .iter()
        .filter_map(|stream| stream.slug.clone().or_else(|| stream.name.clone()))
        .collect();
    let mut resolved_links = Vec::with_capacity(links.len());
    let mut warnings = Vec::new();

    for link in links {
        if known_keys.contains(&link.stream) {
            resolved_links.push(link);
            continue;
        }

        let matching_names = streams
            .iter()
            .filter(|stream| stream.name.as_deref() == Some(link.stream.as_str()))
            .collect::<Vec<_>>();
        if let [stream] = matching_names.as_slice()
            && let Some(slug) = &stream.slug
        {
            warnings.push(format!(
                "streams.md reference '{}' matches by name; update to slug '{slug}'",
                link.stream
            ));
            resolved_links.push(StreamPriorityLink {
                stream: slug.clone(),
                priority: link.priority,
            });
            continue;
        }

        resolved_links.push(link);
    }

    Ok((resolved_links, warnings))
}

fn percentage(share: f64) -> f64 {
    share * 100.0
}

fn stream_times_with_idle_named_streams(
    db: &Database,
    report_streams: &[report::ReportStreamTime],
) -> Result<Vec<StreamTimeInput>> {
    let stream_keys_by_id = db
        .get_streams()
        .context("failed to get streams for todo drift")?
        .into_iter()
        .filter_map(|stream| {
            stream
                .slug
                .or(stream.name)
                .map(|stream_key| (stream.id, stream_key))
        })
        .collect::<BTreeMap<_, _>>();
    let mut stream_times = stream_keys_by_id
        .values()
        .cloned()
        .map(|stream_name| StreamTimeInput {
            stream_name,
            direct_ms: 0,
            delegated_ms: 0,
        })
        .collect::<Vec<_>>();
    stream_times.extend(report_streams.iter().map(|stream| {
        StreamTimeInput {
            stream_name: stream_keys_by_id
                .get(&stream.id)
                .cloned()
                .unwrap_or_else(|| stream.name.clone().unwrap_or_else(|| stream.id.clone())),
            direct_ms: stream.time_direct_ms,
            delegated_ms: stream.time_delegated_ms,
        }
    }));
    Ok(stream_times)
}

#[cfg(test)]
#[path = "drift_tests.rs"]
mod tests;

use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::{Context, Result};
use chrono::{Local, Utc};
use tt_core::todos::{DriftReport, StreamTimeInput, compute_drift};
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
    let warnings = duplicate_named_stream_warnings(db)?;
    let stream_times = stream_times_with_idle_named_streams(db, &report_data.streams)?;
    let drift =
        compute_drift(&priorities, &links, &stream_times).context("failed to compute drift")?;
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
    writeln!(
        output,
        "{:<18} {:>10} {:>12} {:>12} {:>12} {:>12}",
        "Priority", "Importance", "Direct", "Direct+Del", "Direct time", "All time"
    )
    .context("failed to format drift table header")?;
    for priority in &drift.priorities {
        writeln!(
            output,
            "{:<18} {:>9.1}% {:>11.1}% {:>11.1}% {:>12} {:>12}",
            priority.priority_slug,
            percentage(priority.importance_share),
            percentage(priority.direct_share),
            percentage(priority.direct_plus_delegated_share),
            report::format_duration(priority.direct_ms),
            report::format_duration(priority.direct_plus_delegated_ms)
        )
        .context("failed to format priority drift row")?;
    }
    writeln!(
        output,
        "{:<18} {:>10} {:>11.1}% {:>11.1}% {:>12} {:>12}",
        "unattributed",
        "-",
        percentage(drift.unattributed.direct_share),
        percentage(drift.unattributed.direct_plus_delegated_share),
        report::format_duration(drift.unattributed.direct_ms),
        report::format_duration(drift.unattributed.direct_plus_delegated_ms)
    )
    .context("failed to format unattributed drift row")?;
    Ok(output)
}

fn render_warnings(warnings: &[String]) -> Result<String> {
    let mut output = String::new();
    for warning in warnings {
        writeln!(output, "{}", warning_line(warning)).context("failed to format drift warning")?;
    }
    Ok(output)
}

fn warning_line(stream_name: &str) -> String {
    format!("WARNING: DB stream name '{stream_name}' appears more than once; times were combined")
}

fn duplicate_named_stream_warnings(db: &Database) -> Result<Vec<String>> {
    let streams = db
        .get_streams()
        .context("failed to get streams for todo drift duplicate-name warnings")?;
    let mut counts_by_name = BTreeMap::new();
    for stream in streams {
        if let Some(name) = stream.name {
            *counts_by_name.entry(name).or_insert(0usize) += 1;
        }
    }
    Ok(counts_by_name
        .into_iter()
        .filter_map(|(name, count)| (count > 1).then_some(name))
        .collect())
}

fn percentage(share: f64) -> f64 {
    share * 100.0
}

fn stream_times_with_idle_named_streams(
    db: &Database,
    report_streams: &[report::ReportStreamTime],
) -> Result<Vec<StreamTimeInput>> {
    let mut stream_times = db
        .get_streams()
        .context("failed to get streams for todo drift")?
        .into_iter()
        .filter_map(|stream| {
            stream.name.map(|stream_name| StreamTimeInput {
                stream_name,
                direct_ms: 0,
                delegated_ms: 0,
            })
        })
        .collect::<Vec<_>>();
    stream_times.extend(report_streams.iter().map(|stream| StreamTimeInput {
        stream_name: stream.name.clone().unwrap_or_else(|| stream.id.clone()),
        direct_ms: stream.time_direct_ms,
        delegated_ms: stream.time_delegated_ms,
    }));
    Ok(stream_times)
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use tt_core::todos::{Priority, PriorityStatus, StreamPriorityLink, compute_drift};
    use tt_db::Stream;

    use super::*;

    #[test]
    fn duplicate_named_db_streams_warn_and_keep_combined_time() {
        // Given: the DB has two streams with the same display name and the period report has time
        // for both stream IDs under that shared name.
        let db = Database::open_in_memory().unwrap();
        let created_at = Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap();
        for id in ["stream-a", "stream-b"] {
            db.insert_stream(&Stream {
                id: id.to_string(),
                name: Some("Shared stream".to_string()),
                created_at,
                updated_at: created_at,
                time_direct_ms: 0,
                time_delegated_ms: 0,
                first_event_at: None,
                last_event_at: None,
                needs_recompute: false,
            })
            .unwrap();
        }
        let report_streams = vec![
            report::ReportStreamTime {
                id: "stream-a".to_string(),
                name: Some("Shared stream".to_string()),
                time_direct_ms: 60_000,
                time_delegated_ms: 0,
            },
            report::ReportStreamTime {
                id: "stream-b".to_string(),
                name: Some("Shared stream".to_string()),
                time_direct_ms: 120_000,
                time_delegated_ms: 0,
            },
        ];

        // When: stream times and warnings are built for drift.
        let stream_times = stream_times_with_idle_named_streams(&db, &report_streams).unwrap();
        let warnings = duplicate_named_stream_warnings(&db).unwrap();
        let drift = compute_drift(
            &[Priority {
                title: "IPI launch".to_string(),
                slug: "ipi".to_string(),
                value: 9,
                status: PriorityStatus::Active,
            }],
            &[StreamPriorityLink {
                stream: "Shared stream".to_string(),
                priority: "ipi".to_string(),
            }],
            &stream_times,
        )
        .unwrap();

        // Then: drift does not error, combines both streams' time, and warns about the shared name.
        assert_eq!(drift.priorities[0].direct_ms, 180_000);
        assert_eq!(warnings, vec!["Shared stream".to_string()]);
        let rendered = render_warnings(&warnings).unwrap();
        assert!(rendered.contains(
            "WARNING: DB stream name 'Shared stream' appears more than once; times were combined"
        ));
    }
}

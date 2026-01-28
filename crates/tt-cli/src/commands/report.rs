//! Report command for generating weekly summaries.

use std::collections::HashMap;
use std::io::Write;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Datelike, Duration, FixedOffset, Local, SecondsFormat, TimeZone, Utc};
use clap::Args;
use tt_db::{Database, StreamRecord, TimeTotals};

use crate::Config;

#[derive(Debug, Args)]
pub struct ReportArgs {
    /// Generate report for the current week.
    #[arg(long)]
    pub week: bool,
}

pub fn run<W: Write>(writer: &mut W, args: &ReportArgs, config: &Config) -> Result<()> {
    if !args.week {
        bail!("only --week is supported");
    }

    let db = Database::open(&config.database_path)
        .with_context(|| format!("failed to open {}", config.database_path.display()))?;
    let now = Local::now().fixed_offset();
    run_week_report(writer, &db, now)
}

fn run_week_report<W: Write>(
    writer: &mut W,
    db: &Database,
    now: DateTime<FixedOffset>,
) -> Result<()> {
    let (week_start, week_end_date) = week_bounds(now);
    let week_start_utc = week_start.with_timezone(&Utc);
    let week_end_utc = now.with_timezone(&Utc);
    let week_events = db.list_events_in_range(week_start_utc, week_end_utc)?;
    let week_totals = db.allocate_time_for_events(week_events)?;
    let summary_totals = sum_totals(&week_totals);
    let unassigned_totals = week_totals.get(&None).copied().unwrap_or_default();
    let stream_map = stream_name_map(db.list_streams()?);
    let tags_map = db.list_stream_tags()?;
    let stream_rows = build_stream_rows(&week_totals, &stream_map, &tags_map);
    let daily_totals = build_daily_totals(db, week_start, now)?;

    writeln!(writer, "TIME REPORT (WEEK)")?;
    writeln!(
        writer,
        "Week: {}..{} (Mon-Sun)",
        week_start.format("%Y-%m-%d"),
        week_end_date.format("%Y-%m-%d")
    )?;
    writeln!(
        writer,
        "Generated: {}",
        now.to_rfc3339_opts(SecondsFormat::Secs, false)
    )?;
    writeln!(writer)?;

    let total_ms = summary_totals.direct_ms + summary_totals.delegated_ms;
    writeln!(writer, "SUMMARY")?;
    writeln!(writer, "Total tracked: {}", format_duration_ms(total_ms))?;
    writeln!(
        writer,
        "Direct time:   {} ({:>3}%)",
        format_duration_ms(summary_totals.direct_ms),
        percent(summary_totals.direct_ms, total_ms)
    )?;
    writeln!(
        writer,
        "Delegated:     {} ({:>3}%)",
        format_duration_ms(summary_totals.delegated_ms),
        percent(summary_totals.delegated_ms, total_ms)
    )?;
    if unassigned_totals.direct_ms + unassigned_totals.delegated_ms > 0 {
        let unassigned_ms = unassigned_totals.direct_ms + unassigned_totals.delegated_ms;
        writeln!(
            writer,
            "Unassigned:    {} ({:>3}%)",
            format_duration_ms(unassigned_ms),
            percent(unassigned_ms, total_ms)
        )?;
    }
    writeln!(writer)?;

    writeln!(writer, "DAILY TOTALS")?;
    let daily_width = daily_totals
        .iter()
        .map(|day| day.total.len())
        .max()
        .unwrap_or(0)
        .max("0h 00m".len());
    for day in daily_totals {
        writeln!(
            writer,
            "{}  {:>width$}  (D {} / A {})",
            day.label,
            day.total,
            day.direct,
            day.delegated,
            width = daily_width
        )?;
    }
    writeln!(writer)?;

    writeln!(writer, "BY STREAM")?;
    let stream_width = stream_rows
        .iter()
        .map(|row| row.name.len())
        .max()
        .unwrap_or(0)
        .max("Stream".len())
        .max(30);
    let direct_width = "Direct".len().max(7);
    let delegated_width = "Delegated".len().max(9);
    let total_width = "Total".len().max(5);
    writeln!(
        writer,
        "{:<stream_width$}  {:>direct_width$}   {:>delegated_width$}   {:>total_width$}  Tags",
        "Stream",
        "Direct",
        "Delegated",
        "Total",
        stream_width = stream_width,
        direct_width = direct_width,
        delegated_width = delegated_width,
        total_width = total_width
    )?;
    for row in stream_rows {
        writeln!(
            writer,
            "{:<stream_width$}  {:>direct_width$}   {:>delegated_width$}   {:>total_width$}  {}",
            row.name,
            row.direct,
            row.delegated,
            row.total,
            row.tags,
            stream_width = stream_width,
            direct_width = direct_width,
            delegated_width = delegated_width,
            total_width = total_width
        )?;
    }

    Ok(())
}

fn week_bounds(now: DateTime<FixedOffset>) -> (DateTime<FixedOffset>, chrono::NaiveDate) {
    let offset = *now.offset();
    let days_from_monday = now.weekday().num_days_from_monday() as i64;
    let week_start_date = now.date_naive() - Duration::days(days_from_monday);
    let week_start = offset
        .from_local_datetime(&week_start_date.and_hms_opt(0, 0, 0).unwrap())
        .single()
        .expect("valid week start");
    let week_end_date = week_start_date + Duration::days(6);
    (week_start, week_end_date)
}

fn build_daily_totals(
    db: &Database,
    week_start: DateTime<FixedOffset>,
    now: DateTime<FixedOffset>,
) -> Result<Vec<DailyTotal>> {
    let mut totals = Vec::with_capacity(7);
    for day_offset in 0..7 {
        let day_start = week_start + Duration::days(day_offset);
        let day_label = day_start.format("%a %m-%d").to_string();
        let day_date = day_start.date_naive();
        if day_date > now.date_naive() {
            totals.push(DailyTotal::empty(day_label));
            continue;
        }

        let day_end = day_start + Duration::days(1);
        let day_end = if day_end > now { now } else { day_end };
        let events =
            db.list_events_in_range(day_start.with_timezone(&Utc), day_end.with_timezone(&Utc))?;
        let day_totals = db.allocate_time_for_events(events)?;
        let summed = sum_totals(&day_totals);
        totals.push(DailyTotal::new(day_label, summed));
    }

    Ok(totals)
}

fn stream_name_map(streams: Vec<StreamRecord>) -> HashMap<String, String> {
    streams
        .into_iter()
        .map(|stream| {
            let name = stream.name.unwrap_or_else(|| stream.id.clone());
            (stream.id, name)
        })
        .collect()
}

fn build_stream_rows(
    totals: &HashMap<Option<String>, TimeTotals>,
    stream_map: &HashMap<String, String>,
    tags_map: &HashMap<String, Vec<String>>,
) -> Vec<StreamRow> {
    let mut rows: Vec<StreamRow> = totals
        .iter()
        .filter_map(|(stream_id, totals)| {
            let total_ms = totals.direct_ms + totals.delegated_ms;
            if total_ms <= 0 {
                return None;
            }
            let (name, tags) = match stream_id.as_deref() {
                Some(id) => (
                    stream_map
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| id.to_string()),
                    tags_map
                        .get(id)
                        .map(|tags| tags.join(", "))
                        .filter(|tags| !tags.is_empty())
                        .unwrap_or_else(|| "-".to_string()),
                ),
                None => ("Uncategorized".to_string(), "-".to_string()),
            };
            Some(StreamRow {
                name,
                direct: format_duration_ms(totals.direct_ms),
                delegated: format_duration_ms(totals.delegated_ms),
                total: format_duration_ms(total_ms),
                tags,
                total_ms,
            })
        })
        .collect();

    rows.sort_by(|a, b| {
        b.total_ms
            .cmp(&a.total_ms)
            .then_with(|| a.name.cmp(&b.name))
    });

    rows
}

fn sum_totals(totals: &HashMap<Option<String>, TimeTotals>) -> TimeTotals {
    let mut summed = TimeTotals::default();
    for entry in totals.values() {
        summed.direct_ms += entry.direct_ms;
        summed.delegated_ms += entry.delegated_ms;
    }
    summed
}

fn format_duration_ms(ms: i64) -> String {
    let ms = ms.max(0);
    let minutes = (ms as f64 / 60_000.0).round() as i64;
    let hours = minutes / 60;
    let minutes = minutes % 60;
    format!("{hours}h {minutes:02}m")
}

fn percent(part: i64, total: i64) -> i64 {
    if total <= 0 {
        0
    } else {
        ((part as f64 / total as f64) * 100.0).round() as i64
    }
}

#[derive(Debug)]
struct DailyTotal {
    label: String,
    total: String,
    direct: String,
    delegated: String,
}

impl DailyTotal {
    fn new(label: String, totals: TimeTotals) -> Self {
        Self {
            label,
            total: format_duration_ms(totals.direct_ms + totals.delegated_ms),
            direct: format_duration_ms(totals.direct_ms),
            delegated: format_duration_ms(totals.delegated_ms),
        }
    }

    fn empty(label: String) -> Self {
        Self {
            label,
            total: "0h 00m".to_string(),
            direct: "0h 00m".to_string(),
            delegated: "0h 00m".to_string(),
        }
    }
}

#[derive(Debug)]
struct StreamRow {
    name: String,
    direct: String,
    delegated: String,
    total: String,
    tags: String,
    total_ms: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::{FixedOffset, TimeZone};
    use insta::assert_snapshot;
    use tt_db::{Database, EventRecord};

    fn event(id: &str, timestamp: &str, kind: &str, cwd: Option<&str>) -> EventRecord {
        EventRecord {
            id: id.to_string(),
            timestamp: timestamp.to_string(),
            kind: kind.to_string(),
            source: "remote.tmux".to_string(),
            schema_version: 1,
            data: r#"{"pane_id":"%1"}"#.to_string(),
            cwd: cwd.map(str::to_string),
            session_id: None,
            stream_id: None,
            assignment_source: None,
        }
    }

    #[test]
    fn report_week_outputs_summary_and_breakdowns() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("tt.db");
        let mut db = Database::open(&db_path).unwrap();

        db.insert_events(&[
            event(
                "event-1",
                "2026-01-26T09:00:00Z",
                "tmux_pane_focus",
                Some("acme-webapp"),
            ),
            event(
                "event-2",
                "2026-01-26T09:01:00Z",
                "tmux_pane_focus",
                Some("acme-webapp"),
            ),
            event(
                "event-3",
                "2026-01-26T09:02:00Z",
                "tmux_pane_focus",
                Some("acme-webapp"),
            ),
            event(
                "event-4",
                "2026-01-26T09:03:00Z",
                "tmux_pane_focus",
                Some("acme-webapp"),
            ),
            EventRecord {
                id: "event-5".to_string(),
                timestamp: "2026-01-27T10:00:00Z".to_string(),
                kind: "agent_tool_use".to_string(),
                source: "remote.agent".to_string(),
                schema_version: 1,
                data: "{}".to_string(),
                cwd: Some("infra-docs".to_string()),
                session_id: Some("sess-1".to_string()),
                stream_id: None,
                assignment_source: None,
            },
            event(
                "event-6",
                "2026-01-27T10:02:00Z",
                "tmux_pane_focus",
                Some("infra-docs"),
            ),
            event(
                "event-7",
                "2026-01-27T10:03:00Z",
                "tmux_pane_focus",
                Some("infra-docs"),
            ),
        ])
        .unwrap();

        db.infer_streams(1_800_000).unwrap();
        let streams = db.list_streams().unwrap();
        let mut ids_by_name = std::collections::HashMap::new();
        for stream in streams {
            if let Some(name) = stream.name {
                ids_by_name.insert(name, stream.id);
            }
        }
        let acme_id = ids_by_name.get("acme-webapp").unwrap();
        let infra_id = ids_by_name.get("infra-docs").unwrap();
        db.add_stream_tag(acme_id, "client:acme").unwrap();
        db.add_stream_tag(acme_id, "backend").unwrap();
        db.add_stream_tag(infra_id, "writing").unwrap();
        let now = FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(2026, 1, 28, 9, 42, 10)
            .unwrap();
        let mut output = Vec::new();

        run_week_report(&mut output, &db, now).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert_snapshot!(output);
    }
}

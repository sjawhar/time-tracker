//! Human-readable rendering of report data.
//!
//! Direct time — the user's own attention — is the primary axis: every section
//! is ordered by it and every bar is scaled to it, because "where did my time
//! go" always means direct time. Delegated agent time rides along as a trailing
//! `+` figure so leverage stays visible without competing for the reader's eye.
//! See "The point: leverage" in `AGENTS.md`.

use std::fmt::Write;

use chrono::{Datelike, Duration, Local};

use super::format::{format_duration, format_leverage};
use super::sessions::{build_agent_session_summary, write_agent_session_summary};
use super::tags::{build_tag_times, is_tagged, untagged_totals};
use super::{PeriodType, ReportData, ReportStreamTime};

mod table;
pub use table::short_id;

#[cfg(test)]
mod tests;

/// Formats the human-readable report output.
pub fn format_report(data: &ReportData) -> String {
    let mut output = String::new();
    writeln!(output, "TIME REPORT: {}", format_period_description(data)).unwrap();

    let agent_sessions = build_agent_session_summary(&data.agent_sessions);

    if data.streams.is_empty()
        && data.unassigned_direct_ms == 0
        && data.unassigned_delegated_ms == 0
    {
        let spans_one_day = (data.period_end.with_timezone(&Local) - Duration::days(1))
            .date_naive()
            <= data.period_start.with_timezone(&Local).date_naive();
        write_empty_period(&mut output, data.period_type, spans_one_day);
        write_agent_session_summary(&mut output, &agent_sessions);
        return output;
    }

    write_by_stream(&mut output, data);
    write_by_tag(&mut output, data);
    write_agent_session_summary(&mut output, &agent_sessions);
    write_summary(&mut output, data);

    output
}

/// Formats the period description for the report header.
///
/// The span is read from the bounds rather than trusted to `period_type`, because
/// `Period::Custom` maps to `PeriodType::Day` however long it is. A 7-day `--start/--end`
/// range therefore printed "Monday, Jul 13, 2026" above 74h 20m of direct time, inviting
/// the reading that 74 hours happened on one Monday -- the same numbers that `--week`
/// correctly headed "Week of Jul 13, 2026". The header frames every figure beneath it, so
/// it has to name the period it actually covers.
fn format_period_description(report_data: &ReportData) -> String {
    // Convert the bounds from UTC to local for display. `period_end` is exclusive, so the
    // last day a reader sees is the day before it.
    let start_date = report_data.period_start.with_timezone(&Local).date_naive();
    let last_date = (report_data.period_end.with_timezone(&Local) - Duration::days(1)).date_naive();

    if matches!(report_data.period_type, PeriodType::Week) {
        return format!("Week of {}", start_date.format("%b %-d, %Y"));
    }
    if last_date <= start_date {
        return format!("{}", start_date.format("%A, %b %-d, %Y"));
    }
    if start_date.year() == last_date.year() {
        return format!(
            "{} – {}",
            start_date.format("%b %-d"),
            last_date.format("%b %-d, %Y")
        );
    }
    format!(
        "{} – {}",
        start_date.format("%b %-d, %Y"),
        last_date.format("%b %-d, %Y")
    )
}

fn write_empty_period(output: &mut String, period_type: PeriodType, spans_one_day: bool) {
    // "day" is only true of a single-day period; a custom range carries PeriodType::Day
    // however long it is, so this said "No events recorded this day" for a whole week.
    let period_word = match period_type {
        PeriodType::Week => "week",
        PeriodType::Day if spans_one_day => "day",
        PeriodType::Day => "period",
    };
    writeln!(output).unwrap();
    writeln!(output, "No events recorded this {period_word}.").unwrap();
    writeln!(output).unwrap();
    writeln!(output, "Hint: Run 'tt status' to check tracking health.").unwrap();
}

/// Splits the reserved junk stream out of the rows that get ranked.
///
/// Junk is by definition not attributable work, so ranking it beside real
/// streams answers "where did my attention go" with noise. It is still reported
/// on a line of its own rather than dropped: junk is *routed* instead of deleted
/// precisely so a rule that starts eating real work stays detectable, and a
/// silently omitted total is the failure that design warns about. See
/// `specs/design/2026-08-02-classifier-work-selection-design.md`.
fn split_junk(data: &ReportData) -> (Vec<&ReportStreamTime>, Option<&ReportStreamTime>) {
    let junk_id = data.junk_stream_id.as_deref();
    let mut ranked = Vec::with_capacity(data.streams.len());
    let mut junk = None;
    for stream in &data.streams {
        if Some(stream.id.as_str()) == junk_id {
            junk = Some(stream);
        } else {
            ranked.push(stream);
        }
    }
    (ranked, junk)
}

/// Writes BY STREAM: the answer to "where did my attention go", finest grained
/// and ordered by direct time.
fn write_by_stream(output: &mut String, data: &ReportData) {
    let (mut rows, junk) = split_junk(data);
    rows.sort_by(|a, b| {
        b.time_direct_ms
            .cmp(&a.time_direct_ms)
            .then_with(|| b.time_delegated_ms.cmp(&a.time_delegated_ms))
            .then_with(|| a.id.cmp(&b.id))
    });
    let (attended, unattended) = rows.split_at(rows.partition_point(|s| s.time_direct_ms > 0));
    // Scaled to the largest *stream*, deliberately excluding `(unassigned)`. The bars
    // exist so streams can be compared with each other, and unassigned is the absence of
    // a stream rather than one more of them. Including it collapsed every real row to the
    // 1-block minimum as soon as unassigned exceeded the top stream by ~10x -- which is
    // now the ordinary state, not an edge case: releasing the cwd propagator's 47,082
    // assignments left the week of 2026-08-03 at 47h 26m unassigned against a top stream
    // of 4h 17m, so BY STREAM rendered `█░░░░░░░░░` on every line and compared nothing.
    //
    // `progress_bar` clamps a value above its max, so the unassigned row still draws a
    // full bar; its figure is printed beside it, and the SUMMARY block carries the totals.
    // With no attended stream there is nothing to compare against, so the unassigned row
    // becomes the scale rather than leaving it at 0 -- `progress_bar` renders an empty bar
    // for a zero max, which would draw the only row carrying time as if it carried none.
    let max_direct = match attended.first().map_or(0, |s| s.time_direct_ms) {
        0 => data.unassigned_direct_ms,
        largest => largest,
    };

    writeln!(output).unwrap();
    writeln!(output, "BY STREAM").unwrap();
    writeln!(output, "─────────").unwrap();
    table::write_header(output);

    for stream in attended {
        let label = table::stream_label(&stream.id, stream.name.as_deref().unwrap_or("(unnamed)"));
        let row = table::row(
            &label,
            stream.time_direct_ms,
            stream.time_delegated_ms,
            max_direct,
        );
        writeln!(output, "{row}").unwrap();
    }

    if !unattended.is_empty() {
        let delegated_ms = unattended.iter().map(|s| s.time_delegated_ms).sum();
        table::write_no_direct_tail(output, "stream", unattended.len(), delegated_ms);
    }

    write_unassigned(output, data, max_direct);
    write_junk(output, junk);
    write_stream_tip(output, data, attended, !unattended.is_empty());
}

/// The reserved junk stream's totals, on one line and outside the ranking.
///
/// Deliberately rendered without a bar: a bar is a position in the ranking, and
/// junk has none. The figures are still here so an over-aggressive junk rule
/// shows up as junk time growing against the streams it ate.
fn write_junk(output: &mut String, junk: Option<&ReportStreamTime>) {
    let Some(junk) = junk else {
        return;
    };
    writeln!(
        output,
        "  (junk: {} direct, {} delegated — not ranked; 'tt streams dissolve junk' releases it)",
        table::duration_cell(junk.time_direct_ms),
        table::duration_cell(junk.time_delegated_ms),
    )
    .unwrap();
}

/// Activity attributed to no stream at all.
///
/// Kept as its own visible row rather than folded away: unassigned *direct*
/// time is the signal that classification has fallen behind.
fn write_unassigned(output: &mut String, data: &ReportData, max_direct: i64) {
    if data.unassigned_direct_ms == 0 && data.unassigned_delegated_ms == 0 {
        return;
    }
    let row = table::row(
        "(unassigned)",
        data.unassigned_direct_ms,
        data.unassigned_delegated_ms,
        max_direct,
    );
    writeln!(output, "{row}").unwrap();
    writeln!(
        output,
        "  Not assigned to any stream. Run 'tt classify' to attribute this time."
    )
    .unwrap();
}

fn write_stream_tip(
    output: &mut String,
    data: &ReportData,
    listed: &[&ReportStreamTime],
    has_hidden_rows: bool,
) {
    let tip = if has_hidden_rows {
        Some("Run 'tt streams list' to see all".to_string())
    } else {
        // Only untagged streams are worth suggesting: tagging an already-tagged
        // stream is a no-op.
        listed
            .iter()
            .find(|stream| !is_tagged(&data.tags_by_stream, &stream.id))
            .map(|stream| format!("Run 'tt tag {} <project>' to assign", short_id(&stream.id)))
    };
    if let Some(tip) = tip {
        writeln!(output).unwrap();
        writeln!(output, "  Tip: {tip}").unwrap();
    }
}

/// Writes BY TAG: the same direct time rolled up along the tag dimensions.
///
/// Junk is lifted out here too, so the two sections roll up the same population.
/// Its totals are reported once, on the junk line in BY STREAM.
fn write_by_tag(output: &mut String, data: &ReportData) {
    let (ranked, _junk) = split_junk(data);
    let mut tags = build_tag_times(&ranked, &data.tags_by_stream);
    tags.sort_by(|a, b| {
        b.time_direct_ms
            .cmp(&a.time_direct_ms)
            .then_with(|| b.time_delegated_ms.cmp(&a.time_delegated_ms))
            .then_with(|| a.tag.cmp(&b.tag))
    });
    let (untagged_direct_ms, untagged_delegated_ms) =
        untagged_totals(&ranked, &data.tags_by_stream);
    let has_untagged = untagged_direct_ms > 0 || untagged_delegated_ms > 0;
    // Same rule as BY STREAM above: scaled to the largest *tag*, excluding `(untagged)`,
    // so the bars compare tags with each other instead of all collapsing against a large
    // untagged remainder. `progress_bar` clamps, so untagged still draws a full bar.
    let max_direct = match tags.first().map_or(0, |t| t.time_direct_ms) {
        0 => untagged_direct_ms,
        largest => largest,
    };

    writeln!(output).unwrap();
    writeln!(output, "BY TAG").unwrap();
    writeln!(output, "──────").unwrap();

    if tags.is_empty() {
        writeln!(output, "(no tagged streams)").unwrap();
        if !has_untagged {
            return;
        }
    }

    table::write_header(output);
    let (attended, unattended) = tags.split_at(tags.partition_point(|t| t.time_direct_ms > 0));
    for entry in attended {
        let label = table::truncated_tag(&entry.tag);
        let row = table::row(
            &label,
            entry.time_direct_ms,
            entry.time_delegated_ms,
            max_direct,
        );
        writeln!(output, "{row}").unwrap();
    }

    if !unattended.is_empty() {
        let delegated_ms = unattended.iter().map(|t| t.time_delegated_ms).sum();
        table::write_no_direct_tail(output, "tag", unattended.len(), delegated_ms);
    }

    if has_untagged {
        let row = table::row(
            "(untagged)",
            untagged_direct_ms,
            untagged_delegated_ms,
            max_direct,
        );
        writeln!(output, "{row}").unwrap();
    }
}

fn write_summary(output: &mut String, data: &ReportData) {
    let total_direct = data.total_direct_ms();
    let total_delegated = data.total_delegated_ms();

    writeln!(output).unwrap();
    writeln!(output, "SUMMARY").unwrap();
    writeln!(output, "───────").unwrap();
    writeln!(
        output,
        "Wall clock:      {}",
        format_duration(data.total_tracked_ms)
    )
    .unwrap();
    writeln!(output, "Direct time:     {}", format_duration(total_direct)).unwrap();
    writeln!(
        output,
        "Delegated time:  {}",
        format_duration(total_delegated)
    )
    .unwrap();
    writeln!(
        output,
        "Leverage:        {}",
        format_leverage(total_direct, total_delegated)
    )
    .unwrap();
}

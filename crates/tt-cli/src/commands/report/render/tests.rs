//! Tests for the human-readable report rendering.
//!
//! The invariant under test throughout: direct time is the primary axis. Every
//! section orders by it, every bar scales to it, and delegated time never takes
//! the lead.

use chrono::{TimeZone, Utc};
use insta::assert_snapshot;

use super::super::test_support::{base_report, make_test_stream, tags, windowed_session};
use super::super::{PeriodType, ReportData, ReportStreamTime};
use super::format_report;

/// Slices the rendered report from one section header up to the next.
fn section<'a>(output: &'a str, header: &str, next_header: &str) -> &'a str {
    let start = output
        .find(header)
        .unwrap_or_else(|| panic!("report has no {header} section:\n{output}"));
    let rest = &output[start..];
    let end = rest.find(next_header).unwrap_or(rest.len());
    &rest[..end]
}

/// Returns the first rendered line containing `needle`.
fn line_containing<'a>(output: &'a str, needle: &str) -> &'a str {
    output
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no line contains {needle:?}:\n{output}"))
}

/// Two streams whose direct ordering is the reverse of their combined ordering.
fn attention_vs_delegation() -> Vec<ReportStreamTime> {
    vec![
        make_test_stream("aaaaaa111111", "attention-heavy", 3_600_000, 600_000),
        make_test_stream("bbbbbb222222", "delegation-heavy", 600_000, 360_000_000),
    ]
}

#[test]
fn by_stream_section_orders_by_direct_time_not_delegated() {
    // Given: one stream the user actually watched, and one that ran mostly
    // unattended across many parallel agents.
    let data = ReportData {
        streams: attention_vs_delegation(),
        total_tracked_ms: 4_200_000,
        ..base_report(PeriodType::Day)
    };

    // When: the human-readable report is rendered.
    let output = format_report(&data);
    let by_stream = section(&output, "BY STREAM", "BY TAG");

    // Then: the stream that consumed attention is listed first.
    let attention = by_stream
        .find("attention-heavy")
        .expect("attention-heavy stream missing from BY STREAM");
    let delegation = by_stream
        .find("delegation-heavy")
        .expect("delegation-heavy stream missing from BY STREAM");
    assert!(
        attention < delegation,
        "BY STREAM must order by direct time:\n{by_stream}"
    );
}

#[test]
fn by_tag_section_orders_by_direct_time_not_combined() {
    // Given: two tags whose direct ordering is the reverse of their combined ordering.
    let data = ReportData {
        streams: attention_vs_delegation(),
        tags_by_stream: tags(&[("aaaaaa111111", "focus"), ("bbbbbb222222", "swarm")]),
        total_tracked_ms: 4_200_000,
        ..base_report(PeriodType::Day)
    };

    // When: the human-readable report is rendered.
    let output = format_report(&data);
    let by_tag = section(&output, "BY TAG", "AGENT SESSIONS");

    // Then: the tag holding more attention leads, even though it holds far less
    // delegated time.
    let focus = by_tag.find("focus").expect("focus tag missing from BY TAG");
    let swarm = by_tag.find("swarm").expect("swarm tag missing from BY TAG");
    assert!(focus < swarm, "BY TAG must order by direct time:\n{by_tag}");
}

#[test]
fn by_tag_bars_scale_to_direct_time_not_combined() {
    // Given: the same reversal of direct and combined ordering.
    let data = ReportData {
        streams: attention_vs_delegation(),
        tags_by_stream: tags(&[("aaaaaa111111", "focus"), ("bbbbbb222222", "swarm")]),
        total_tracked_ms: 4_200_000,
        ..base_report(PeriodType::Day)
    };

    // When: the human-readable report is rendered.
    let output = format_report(&data);

    // Then: the tag holding the most attention gets the full bar, and the
    // 100-hour-delegated tag gets one sized to its 10 minutes of attention.
    assert!(
        line_containing(&output, "focus").contains("██████████"),
        "focus should own the full bar:\n{output}"
    );
    assert!(
        line_containing(&output, "swarm").contains("██░░░░░░░░"),
        "swarm's bar must track its 10m of direct time:\n{output}"
    );
}

#[test]
fn stream_direct_time_is_rendered_per_stream() {
    // Given: two tagged streams, so no aggregate row happens to equal either
    // stream's own direct time.
    let data = ReportData {
        streams: vec![
            make_test_stream("aaaaaa111111", "alpha", 6_420_000, 10_800_000),
            make_test_stream("bbbbbb222222", "beta", 1_380_000, 1_800_000),
        ],
        tags_by_stream: tags(&[("aaaaaa111111", "proj-a"), ("bbbbbb222222", "proj-b")]),
        total_tracked_ms: 7_800_000,
        ..base_report(PeriodType::Day)
    };

    // When: the human-readable report is rendered.
    let output = format_report(&data);

    // Then: each stream's own direct time is on its own row.
    assert!(
        line_containing(&output, "alpha").contains("1h 47m"),
        "alpha's direct time is missing from its row:\n{output}"
    );
    assert!(
        line_containing(&output, "beta").contains("23m"),
        "beta's direct time is missing from its row:\n{output}"
    );
}

#[test]
fn streams_and_tags_without_direct_time_collapse_into_a_tail() {
    // Given: one attended stream and two that only ran agents.
    let data = ReportData {
        streams: vec![
            make_test_stream("aaaaaa111111", "attended", 3_600_000, 600_000),
            make_test_stream("bbbbbb222222", "overnight-a", 0, 36_000_000),
            make_test_stream("cccccc333333", "overnight-b", 0, 7_200_000),
        ],
        tags_by_stream: tags(&[
            ("aaaaaa111111", "focus"),
            ("bbbbbb222222", "swarm"),
            ("cccccc333333", "swarm"),
        ]),
        total_tracked_ms: 3_600_000,
        ..base_report(PeriodType::Day)
    };

    // When: the human-readable report is rendered.
    let output = format_report(&data);
    let by_stream = section(&output, "BY STREAM", "BY TAG");

    // Then: the unattended streams are summarised on one line instead of
    // pushing the attended one down the page, and their delegated time survives.
    assert!(
        by_stream.contains("  (+ 2 streams with no direct time, 12h 0m delegated)"),
        "missing zero-direct tail:\n{by_stream}"
    );
    assert!(!by_stream.contains("overnight-a"), "{by_stream}");
    let by_tag = section(&output, "BY TAG", "AGENT SESSIONS");
    assert!(
        by_tag.contains("  (+ 1 tag with no direct time, 12h 0m delegated)"),
        "missing zero-direct tag tail:\n{by_tag}"
    );
}

#[test]
fn unassigned_activity_reports_its_direct_time() {
    // Given: a period where some attention landed on activity no stream claims.
    let data = ReportData {
        streams: vec![make_test_stream(
            "aaaaaa111111",
            "attended",
            3_600_000,
            600_000,
        )],
        total_tracked_ms: 5_400_000,
        unassigned_direct_ms: 1_800_000,
        unassigned_delegated_ms: 71_400_000,
        ..base_report(PeriodType::Day)
    };

    // When: the human-readable report is rendered.
    let output = format_report(&data);
    let unassigned = line_containing(&output, "(unassigned)");

    // Then: unattributed direct time is the headline figure on its row, because
    // it is the signal that classification has fallen behind.
    assert!(unassigned.contains("30m"), "{unassigned}");
    assert!(unassigned.contains("+19h 50m"), "{unassigned}");
    assert!(
        output.contains("Run 'tt classify' to attribute this time."),
        "{output}"
    );
}

#[test]
fn test_report_empty_period() {
    let data = base_report(PeriodType::Week);

    let output = format_report(&data);
    assert_snapshot!(output);
}

#[test]
fn test_report_all_untagged() {
    let data = ReportData {
        streams: vec![
            // 2h direct, 1h15m delegated
            make_test_stream("abc123def456", "tmux/dev/session-1", 7_200_000, 4_500_000),
            // 45m direct, 30m delegated
            make_test_stream("def456ghi789", "tmux/dev/session-2", 2_700_000, 1_800_000),
        ],
        total_tracked_ms: 10_800_000, // 3h wall clock
        ..base_report(PeriodType::Week)
    };

    let output = format_report(&data);
    assert_snapshot!(output);
}

#[test]
fn test_report_single_stream() {
    let data = ReportData {
        streams: vec![make_test_stream(
            "abc123def456",
            "tmux/dev/session-1",
            3_600_000, // 1h direct
            4_500_000, // 1h15m delegated
        )],
        total_tracked_ms: 5_400_000, // 1h30m wall clock
        ..base_report(PeriodType::Day)
    };

    let output = format_report(&data);
    assert_snapshot!(output);
}

#[test]
fn report_lists_every_stream_that_consumed_attention() {
    // Eight streams with descending direct time: none is hidden, because each
    // one answers "where did my attention go".
    let streams: Vec<ReportStreamTime> = (0..8)
        .map(|i| {
            make_test_stream(
                &format!("stream{i:02}abcdef"),
                &format!("tmux/dev/session-{i}"),
                3_600_000 - i64::from(i * 300_000),
                1_800_000 - i64::from(i * 100_000),
            )
        })
        .collect();

    let data = ReportData {
        streams,
        total_tracked_ms: 21_600_000, // 6h wall clock
        ..base_report(PeriodType::Week)
    };

    let output = format_report(&data);
    assert_snapshot!(output);
}

#[test]
fn test_summary_reports_wall_clock_and_leverage_without_percentages() {
    // Given: a day whose delegated time far exceeds the wall clock the user was
    // actually present for — the parallel-agent case leverage is meant to describe.
    let data = ReportData {
        streams: vec![make_test_stream(
            "abc123def456",
            "tmux/dev/session-1",
            1_200_000,  // 20m direct
            12_000_000, // 3h20m delegated across parallel agents
        )],
        total_tracked_ms: 1_800_000, // 30m wall clock
        ..base_report(PeriodType::Day)
    };

    // When: the human-readable report is rendered.
    let output = format_report(&data);
    let summary_section = output.split("SUMMARY").nth(1).unwrap_or("");

    // Then: the summary reports the wall-clock union and the delegation ratio,
    // and never a percentage of a direct+delegated sum.
    assert!(summary_section.contains("Wall clock:      30m"));
    assert!(summary_section.contains("Direct time:     20m"));
    assert!(summary_section.contains("Delegated time:  3h 20m"));
    assert!(summary_section.contains("Leverage:        10.0x"));
    assert!(
        !summary_section.contains('%'),
        "summary must not report percentages: {summary_section}"
    );
}

#[test]
fn test_summary_leverage_is_not_available_without_direct_time() {
    // Given: a period with delegated time but no human attention at all.
    let data = ReportData {
        streams: vec![make_test_stream(
            "abc123def456",
            "tmux/dev/session-1",
            0,
            600_000, // 10m delegated
        )],
        total_tracked_ms: 600_000,
        ..base_report(PeriodType::Day)
    };

    // When: the human-readable report is rendered.
    let output = format_report(&data);

    // Then: leverage is reported as unavailable rather than dividing by zero.
    assert!(output.contains("Leverage:        n/a"));
}

/// A period's streams plus the reserved junk stream, which holds more direct
/// time than any of them — so any leak into the ranking is unmissable.
fn real_work_and_junk() -> Vec<ReportStreamTime> {
    vec![
        make_test_stream("aaaaaa111111", "real work", 1_800_000, 3_600_000),
        make_test_stream("junk", "junk: no attributable work", 2_700_000, 54_000_000),
    ]
}

#[test]
fn junk_is_excluded_from_the_stream_ranking() {
    // Given: a junk stream holding more direct time than the real work beside it.
    let data = ReportData {
        streams: real_work_and_junk(),
        total_tracked_ms: 4_500_000,
        ..base_report(PeriodType::Day)
    };

    // When: the human-readable report is rendered.
    let output = format_report(&data);
    let by_stream = section(&output, "BY STREAM", "BY TAG");

    // Then: junk takes no row in the ranking, and does not scale the bars — the
    // real work still owns the full bar despite holding less direct time.
    assert!(
        !by_stream.contains("junk: no attributable work"),
        "junk must not be ranked as an ordinary stream:\n{by_stream}"
    );
    assert!(
        line_containing(&output, "real work").contains("██████████"),
        "junk must not scale the ranking's bars:\n{by_stream}"
    );
}

#[test]
fn junk_totals_stay_visible_on_their_own_line() {
    // Given: the same period.
    let data = ReportData {
        streams: real_work_and_junk(),
        total_tracked_ms: 4_500_000,
        ..base_report(PeriodType::Day)
    };

    // When: the human-readable report is rendered.
    let output = format_report(&data);

    // Then: junk's totals are reported on one line, so a junk rule that starts
    // eating real work is detectable rather than silent. Dropping this line is
    // the failure the design explicitly warns about.
    let junk_line = line_containing(&output, "(junk:");
    assert!(junk_line.contains("45m direct"), "{junk_line}");
    assert!(junk_line.contains("15h 0m delegated"), "{junk_line}");
    assert!(junk_line.contains("not ranked"), "{junk_line}");
}

#[test]
fn junk_is_excluded_from_tag_rollups_but_still_counted_in_the_summary() {
    // Given: junk untagged beside a tagged stream of real work.
    let data = ReportData {
        streams: real_work_and_junk(),
        tags_by_stream: tags(&[("aaaaaa111111", "project:real")]),
        total_tracked_ms: 4_500_000,
        ..base_report(PeriodType::Day)
    };

    // When: the human-readable report is rendered.
    let output = format_report(&data);
    let by_tag = section(&output, "BY TAG", "AGENT SESSIONS");
    let summary = output.split("SUMMARY").nth(1).unwrap_or_default();

    // Then: BY TAG rolls up the same population as BY STREAM, so junk does not
    // reappear as untagged time...
    assert!(
        !by_tag.contains("(untagged)"),
        "junk must not resurface as untagged time:\n{by_tag}"
    );
    // ...while the SUMMARY still counts it, because junk time is real time and
    // the wall-clock and leverage figures must stay reconcilable.
    assert!(summary.contains("Direct time:     1h 15m"), "{summary}");
    assert!(summary.contains("Delegated time:  16h 0m"), "{summary}");
}

#[test]
fn agent_session_duration_reports_observed_activity_not_the_period_length() {
    // Given: a session whose nominal span brackets the whole reporting day but
    // which was only observably active for twelve minutes of it. Read from the
    // nominal span, this session reported the window's own length — the artifact
    // that filled a day's "Top sessions" with identical `24h 0m` rows.
    let period_start = base_report(PeriodType::Day).period_start;
    let data = ReportData {
        streams: vec![make_test_stream("aaaaaa111111", "real work", 1_800_000, 0)],
        agent_sessions: vec![windowed_session(
            "ses-long-lived",
            period_start - chrono::Duration::days(14),
            Some(period_start + chrono::Duration::days(14)),
            period_start + chrono::Duration::hours(4),
            period_start + chrono::Duration::hours(4) + chrono::Duration::minutes(12),
        )],
        total_tracked_ms: 1_800_000,
        ..base_report(PeriodType::Day)
    };

    // When: the human-readable report is rendered.
    let output = format_report(&data);
    let sessions = section(&output, "AGENT SESSIONS", "SUMMARY");

    // Then: the row reports the twelve minutes it was active, and no row claims
    // the window's length.
    assert!(sessions.contains("Total sessions: 1"), "{sessions}");
    assert!(
        line_containing(sessions, "ses-lo").contains("12m"),
        "{sessions}"
    );
    assert!(
        !sessions.contains("24h 0m"),
        "no session may report the window length it merely spanned:\n{sessions}"
    );

    // And: the section says which quantity that figure is. It is `active_ms`, so it is
    // delegated time and sums across parallel subagents -- a live session read 106h 31m in
    // a 168h week. Every other section labels Direct and Delegated explicitly; this column
    // carried no label, leaving a reader to assume attention.
    assert!(
        sessions.contains("delegated"),
        "the sessions figure must name itself as delegated, not leave it to be assumed:\n{sessions}"
    );
}

#[test]
fn a_large_unassigned_remainder_does_not_flatten_every_stream_bar() {
    // Given: unassigned direct time an order of magnitude above the largest stream.
    //
    // This is the ordinary state, not an edge case. Releasing the cwd propagator's 47,082
    // assignments left the week of 2026-08-03 with 47h 26m unassigned against a top stream
    // of 4h 17m, and because the bar scale took max(top stream, unassigned), every stream
    // rendered at the 1-block minimum: `█░░░░░░░░░` on every line, comparing nothing. The
    // bars exist so streams can be compared with each other, and unassigned is the absence
    // of a stream rather than one more of them.
    let data = ReportData {
        streams: vec![
            make_test_stream("aaaa1111bbbb", "tmux/dev/a", 4 * 3_600_000, 0),
            make_test_stream("cccc2222dddd", "tmux/dev/b", 2 * 3_600_000, 0),
        ],
        unassigned_direct_ms: 47 * 3_600_000,
        total_tracked_ms: 53 * 3_600_000,
        ..base_report(PeriodType::Week)
    };

    // When: the human-readable report is rendered.
    let output = format_report(&data);
    let by_stream = output
        .split("BY STREAM")
        .nth(1)
        .and_then(|section| section.split("BY TAG").next())
        .expect("a BY STREAM section");

    // Then: the largest stream fills its bar and the half-size one is visibly half.
    let top = by_stream
        .lines()
        .find(|line| line.contains("4h 0m"))
        .expect("the top stream row");
    assert!(
        top.contains("██████████"),
        "the largest stream must fill its bar: {top}"
    );
    let half = by_stream
        .lines()
        .find(|line| line.contains("2h 0m"))
        .expect("the half-size stream row");
    assert!(
        half.contains("█████░░░░░"),
        "half the direct time must draw half a bar: {half}"
    );

    // And: the unassigned row still draws a full bar. `progress_bar` clamps a value above
    // its max, so excluding unassigned from the scale does not understate it visually, and
    // its figure is printed beside it either way.
    let unassigned = by_stream
        .lines()
        .find(|line| line.starts_with("(unassigned)"))
        .expect("the unassigned row");
    assert!(
        unassigned.contains("██████████"),
        "unassigned must still read as the largest thing on the page: {unassigned}"
    );
}

#[test]
fn a_multi_day_custom_range_names_its_span_not_a_single_day() {
    // `Period::Custom` maps to `PeriodType::Day` however long it is, so a 7-day
    // `--start/--end` range printed "Monday, Jul 13, 2026" above 74h 20m of direct time --
    // inviting the reading that 74 hours happened on one Monday. The header frames every
    // figure beneath it, so it must name the period it actually covers.
    let start = Utc.with_ymd_and_hms(2026, 7, 13, 0, 0, 0).single().unwrap();
    let data = ReportData {
        streams: vec![make_test_stream("aaaa1111bbbb", "work", 3_600_000, 0)],
        period_start: start,
        period_end: start + chrono::Duration::days(7),
        total_tracked_ms: 3_600_000,
        ..base_report(PeriodType::Day)
    };

    let header = format_report(&data).lines().next().unwrap().to_string();

    // Asserted on shape, not on exact dates: the header renders in local time, so the
    // calendar days depend on the machine's zone, while the property that matters does not.
    for weekday in [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ] {
        assert!(
            !header.contains(weekday),
            "a week-long range must not be titled as one weekday: {header}"
        );
    }
    assert!(
        header.contains('\u{2013}'),
        "a multi-day range must name both ends: {header}"
    );
}

#[test]
fn a_single_day_range_still_names_its_weekday() {
    let start = Utc.with_ymd_and_hms(2026, 7, 20, 0, 0, 0).single().unwrap();
    let data = ReportData {
        streams: vec![make_test_stream("aaaa1111bbbb", "work", 3_600_000, 0)],
        period_start: start,
        period_end: start + chrono::Duration::days(1),
        total_tracked_ms: 3_600_000,
        ..base_report(PeriodType::Day)
    };

    let header = format_report(&data).lines().next().unwrap().to_string();

    // A single day still names its weekday; which weekday depends on the local zone, so
    // this asserts the shape rather than the date.
    assert!(
        !header.contains('\u{2013}'),
        "a single day must not render as a range: {header}"
    );
    assert!(
        [
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
            "Sunday"
        ]
        .iter()
        .any(|weekday| header.contains(weekday)),
        "a single day names its weekday: {header}"
    );
}

#[test]
fn an_empty_multi_day_range_does_not_call_itself_a_day() {
    let start = Utc.with_ymd_and_hms(2027, 1, 4, 0, 0, 0).single().unwrap();
    let data = ReportData {
        streams: Vec::new(),
        period_start: start,
        period_end: start + chrono::Duration::days(7),
        total_tracked_ms: 0,
        ..base_report(PeriodType::Day)
    };

    let output = format_report(&data);

    assert!(
        !output.contains("recorded this day"),
        "a week-long range is not a day: {output}"
    );
    assert!(output.contains("recorded this period"), "{output}");
}

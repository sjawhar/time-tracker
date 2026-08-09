//! Shared row layout for the report's direct-time tables.
//!
//! Every section uses the same columns so a reader can compare figures
//! vertically across sections: the label, then direct time, then a bar scaled
//! to direct time, then delegated time as a trailing `+` figure.

use std::fmt::Write;

use super::super::format::{format_duration, progress_bar, truncate_cell};

/// `<label:46><direct:>8>  <bar:10>  <delegated:>10>` — 78 columns, so rows fit
/// an 80-column terminal without wrapping.
const LABEL_WIDTH: usize = 46;
const DIRECT_WIDTH: usize = 8;
const DELEGATED_WIDTH: usize = 10;
/// Mirrors the fixed width of [`progress_bar`]'s output.
const BAR_WIDTH: usize = 10;
/// Stream rows spend their first columns on the short stream id, then two spaces.
const ID_WIDTH: usize = 6;

/// The id prefix used to address a row from other commands (`tt tag <id> …`).
pub fn short_id(id: &str) -> String {
    id.chars().take(ID_WIDTH).collect()
}

/// Builds a stream row's label: short id, then the name truncated to fit.
///
/// The two spare columns keep the longest name off the direct-time figure.
pub fn stream_label(id: &str, name: &str) -> String {
    let name = truncate_cell(name, LABEL_WIDTH - ID_WIDTH - 4);
    format!("{:<id_width$}  {name}", short_id(id), id_width = ID_WIDTH)
}

/// Truncates a tag label to fit the label column.
///
/// The two spare columns keep the longest tag off the direct-time figure.
pub fn truncated_tag(tag: &str) -> String {
    truncate_cell(tag, LABEL_WIDTH - 2)
}

/// Formats a row figure, distinguishing "less than a minute" from "none".
///
/// [`format_duration`] floors to whole minutes, so a stream with forty seconds
/// of attention would print `0m` beside a filled bar cell and read as a
/// rendering bug. `<1m` keeps the row honest without changing the duration
/// format that `tt streams` and `tt status` share.
pub fn duration_cell(ms: i64) -> String {
    if ms > 0 && ms < 60_000 {
        return "<1m".to_string();
    }
    format_duration(ms)
}

/// Renders one table row with direct time as the headline figure.
///
/// Delegated time is omitted entirely when it is zero: an empty column is the
/// strongest available signal that it is secondary to what the row is about.
pub fn row(label: &str, direct_ms: i64, delegated_ms: i64, max_direct_ms: i64) -> String {
    let delegated = if delegated_ms > 0 {
        format!("+{}", duration_cell(delegated_ms))
    } else {
        String::new()
    };
    let rendered = format!(
        "{label:<label_width$}{direct:>direct_width$}  {bar}  {delegated:>delegated_width$}",
        direct = duration_cell(direct_ms),
        bar = progress_bar(direct_ms, max_direct_ms),
        label_width = LABEL_WIDTH,
        direct_width = DIRECT_WIDTH,
        delegated_width = DELEGATED_WIDTH,
    );
    rendered.trim_end().to_string()
}

/// Writes the column labels that name the two figures on every row.
pub fn write_header(output: &mut String) {
    writeln!(
        output,
        "{:>direct_col$}{:>delegated_col$}",
        "Direct",
        "Delegated",
        direct_col = LABEL_WIDTH + DIRECT_WIDTH,
        delegated_col = BAR_WIDTH + 4 + DELEGATED_WIDTH,
    )
    .unwrap();
}

/// Collapses every row with no direct time into a single line.
///
/// They are omitted individually rather than deleted: the delegated time is
/// real work that the SUMMARY still counts, so dropping it would hide the
/// leverage signal. But they answer "where did my attention go" with silence,
/// so they must not crowd out the rows that do answer it.
pub fn write_no_direct_tail(output: &mut String, noun: &str, count: usize, delegated_ms: i64) {
    let plural = if count == 1 { "" } else { "s" };
    let delegated = duration_cell(delegated_ms);
    writeln!(
        output,
        "  (+ {count} {noun}{plural} with no direct time, {delegated} delegated)"
    )
    .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_puts_direct_time_before_the_bar_and_omits_zero_delegated() {
        let rendered = row("legion: dev", 3_600_000, 0, 3_600_000);
        assert!(rendered.starts_with("legion: dev"), "{rendered}");
        assert!(rendered.ends_with("1h 0m  ██████████"), "{rendered}");
        assert!(!rendered.contains('+'), "{rendered}");
    }

    #[test]
    fn row_trails_nonzero_delegated_with_a_plus() {
        let rendered = row("legion: dev", 600_000, 79_200_000, 3_600_000);
        assert!(rendered.ends_with("+22h 0m"), "{rendered}");
        // The bar tracks direct time, so a 22-hour delegated figure buys 2 cells.
        assert!(rendered.contains("██░░░░░░░░"), "{rendered}");
    }

    #[test]
    fn row_marks_sub_minute_figures_rather_than_flooring_them_to_zero() {
        // 40s of attention and 30s of agent time: real, but below the minute
        // that `format_duration` floors to.
        let rendered = row("legion: dev", 40_000, 30_000, 3_600_000);
        assert!(rendered.contains("<1m"), "{rendered}");
        assert!(rendered.ends_with("+<1m"), "{rendered}");
        assert!(!rendered.contains("0m"), "{rendered}");
    }

    #[test]
    fn row_still_prints_zero_for_genuinely_no_direct_time() {
        let rendered = row("(unassigned)", 0, 71_400_000, 3_600_000);
        assert!(rendered.contains("0m  ░░░░░░░░░░"), "{rendered}");
        assert!(!rendered.contains("<1m"), "{rendered}");
    }

    #[test]
    fn header_labels_align_with_the_row_columns() {
        let mut output = String::new();
        write_header(&mut output);
        let header = output.trim_end_matches('\n');
        let rendered = row("x", 3_600_000, 79_200_000, 3_600_000);

        // "Direct" shares a right edge with the direct figure.
        assert_eq!(
            header.find("Direct").map(|i| i + "Direct".len()),
            rendered.find("1h 0m").map(|i| i + "1h 0m".len())
        );
        // "Delegated" shares a right edge with the delegated figure: both end the row.
        assert_eq!(header.chars().count(), rendered.chars().count());
        assert!(header.ends_with("Delegated"), "{header}");
        assert!(rendered.ends_with("+22h 0m"), "{rendered}");
    }

    #[test]
    fn stream_label_pads_short_ids_and_truncates_long_names() {
        assert_eq!(stream_label("abc", "short"), "abc     short");
        let label = stream_label(
            "abc123def456",
            "workorder-5: Anthropic cross-model eval - runs/scoring/flow",
        );
        assert_eq!(label, "abc123  workorder-5: Anthropic cross-model…");
    }

    #[test]
    fn no_direct_tail_is_singular_for_one_row() {
        let mut output = String::new();
        write_no_direct_tail(&mut output, "stream", 1, 600_000);
        assert_eq!(
            output,
            "  (+ 1 stream with no direct time, 10m delegated)\n"
        );
    }

    #[test]
    fn no_direct_tail_is_plural_for_several_rows() {
        let mut output = String::new();
        write_no_direct_tail(&mut output, "tag", 3, 5_400_000);
        assert_eq!(
            output,
            "  (+ 3 tags with no direct time, 1h 30m delegated)\n"
        );
    }
}

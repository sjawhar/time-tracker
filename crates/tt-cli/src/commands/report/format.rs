//! Duration, ratio, bar, and table-cell formatting primitives shared by the
//! report renderer and by other commands that print the same figures.

/// Formats milliseconds as duration string.
/// Returns "Xh Ym" if >= 1 hour, "Xm" if < 1 hour.
/// Negative durations are treated as 0m (defensive).
pub fn format_duration(ms: i64) -> String {
    if ms < 0 {
        return "0m".to_string();
    }
    let total_minutes = ms / 60_000;
    let hours = total_minutes / 60;
    let minutes = total_minutes % 60;

    if hours >= 1 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

/// Formats the delegation ratio (delegated ÷ direct) from `specs/design/core-concepts.md:102`.
///
/// Returns "n/a" when there is no direct time to divide by.
pub fn format_leverage(direct_ms: i64, delegated_ms: i64) -> String {
    if direct_ms <= 0 {
        return "n/a".to_string();
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "durations are far below the f64 integer-precision limit"
    )]
    let ratio = delegated_ms as f64 / direct_ms as f64;
    format!("{ratio:.1}x")
}

/// Generates a 10-character progress bar.
/// Values <5% of max get a single block for visibility.
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "bar width is 10 cells; durations are far below f64 precision limits"
)]
pub fn progress_bar(value: i64, max: i64) -> String {
    // Sections with nothing to scale against (every row at zero) render an empty bar
    if max == 0 {
        return "░░░░░░░░░░".to_string();
    }

    let ratio = value as f64 / max as f64;
    let filled = if ratio < 0.05 && value > 0 {
        1 // Minimum 1 for visibility (spec: <5% gets single block)
    } else {
        // Clamp to 10: this is reached by design, not defensively. BY STREAM and BY TAG
        // scale to the largest stream/tag and exclude the `(unassigned)`/`(untagged)`
        // rows, so those rows legitimately exceed the max and draw a full bar.
        (ratio * 10.0).round().min(10.0) as usize
    };

    let empty = 10 - filled;
    format!("{}{}", "█".repeat(filled), "░".repeat(empty))
}

/// Truncates a table label to `width`, marking elision with `…`.
///
/// Counts characters rather than display cells: the CLI carries no wide-character
/// dependency, so CJK or emoji labels can still overflow their column
/// (`specs/design/ux-reports.md` records this limitation).
pub fn truncate_cell(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    let mut truncated = kept.trim_end().to_string();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_duration_hours_and_minutes() {
        assert_eq!(format_duration(9_000_000), "2h 30m"); // 2.5 hours
        assert_eq!(format_duration(3_600_000), "1h 0m"); // 1 hour
        assert_eq!(format_duration(5_400_000), "1h 30m"); // 1.5 hours
    }

    #[test]
    fn test_format_duration_minutes_only() {
        assert_eq!(format_duration(2_700_000), "45m"); // 45 minutes
        assert_eq!(format_duration(60_000), "1m"); // 1 minute
        assert_eq!(format_duration(1_800_000), "30m"); // 30 minutes
    }

    #[test]
    fn test_format_duration_zero() {
        assert_eq!(format_duration(0), "0m");
    }

    #[test]
    fn test_format_duration_floors_seconds() {
        // 45.9 minutes should floor to 45m
        assert_eq!(format_duration(2_754_000), "45m");
    }

    #[test]
    fn test_format_duration_negative_is_zero() {
        // Negative durations should be treated as 0 (defensive)
        assert_eq!(format_duration(-1), "0m");
        assert_eq!(format_duration(-3_600_000), "0m");
    }

    #[test]
    fn test_progress_bar_full() {
        assert_eq!(progress_bar(100, 100), "██████████");
    }

    #[test]
    fn test_progress_bar_partial() {
        assert_eq!(progress_bar(50, 100), "█████░░░░░"); // 50%
        assert_eq!(progress_bar(80, 100), "████████░░"); // 80%
        assert_eq!(progress_bar(20, 100), "██░░░░░░░░"); // 20%
    }

    #[test]
    fn test_progress_bar_minimum() {
        // <5% should get single block for visibility
        assert_eq!(progress_bar(4, 100), "█░░░░░░░░░"); // 4%
        assert_eq!(progress_bar(1, 100), "█░░░░░░░░░"); // 1%
    }

    #[test]
    fn test_progress_bar_zero() {
        // max == 0 (defensive case)
        assert_eq!(progress_bar(0, 0), "░░░░░░░░░░");
    }

    #[test]
    fn test_progress_bar_zero_value_against_nonzero_max() {
        // A row with no direct time renders an empty bar, not a minimum block.
        assert_eq!(progress_bar(0, 100), "░░░░░░░░░░");
    }

    #[test]
    fn test_progress_bar_at_5_percent() {
        // Exactly 5% should round to 1 block (0.05 * 10 = 0.5, rounds to 1)
        assert_eq!(progress_bar(5, 100), "█░░░░░░░░░");
    }

    #[test]
    fn test_format_leverage_reports_ratio_and_handles_no_direct_time() {
        assert_eq!(format_leverage(1_200_000, 12_000_000), "10.0x");
        assert_eq!(format_leverage(0, 600_000), "n/a");
    }

    #[test]
    fn test_truncate_cell_keeps_short_labels_intact() {
        assert_eq!(truncate_cell("legion: dev", 36), "legion: dev");
        assert_eq!(truncate_cell("exactly-six", 11), "exactly-six");
    }

    #[test]
    fn test_truncate_cell_marks_elision() {
        assert_eq!(truncate_cell("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn test_truncate_cell_does_not_leave_a_space_before_the_ellipsis() {
        assert_eq!(truncate_cell("cross-model eval", 13), "cross-model…");
    }

    #[test]
    fn test_truncate_cell_respects_char_boundaries() {
        // Multi-byte characters must not be split mid-code-point.
        assert_eq!(truncate_cell("héllo wörld", 6), "héllo…");
    }
}

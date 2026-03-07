//! Shared utilities for CLI commands.

use std::sync::LazyLock;

use anyhow::Context;
use chrono::{DateTime, Duration, Utc};
use regex::Regex;

/// Pre-compiled regex for relative time parsing.
static RELATIVE_TIME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d+)\s+(minute|hour|day|week)s?\s+ago$").unwrap());

/// Conservative bounds for relative time parsing (~1000 years in minutes).
const MAX_RELATIVE_MINUTES: i64 = 1000 * 365 * 24 * 60;

/// Parse a datetime string as either ISO 8601 or relative time.
///
/// Supports:
/// - ISO 8601: "2026-01-15T10:30:00Z"
/// - Relative: "2 hours ago", "30 minutes ago", "1 day ago", "1 week ago"
pub fn parse_datetime(s: &str) -> anyhow::Result<DateTime<Utc>> {
    // Try ISO 8601 first
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }

    // Try relative time: "N hours/minutes/days/weeks ago"
    let Some(caps) = RELATIVE_TIME_RE.captures(s) else {
        anyhow::bail!(
            "Invalid datetime: {s}. Use ISO 8601 (e.g., 2026-01-15T10:30:00Z) or relative (e.g., '2 hours ago')"
        );
    };

    let n: i64 = caps[1]
        .parse()
        .context("failed to parse number in relative time")?;

    let (max_for_unit, minutes_per_unit) = match &caps[2] {
        "minute" => (MAX_RELATIVE_MINUTES, 1),
        "hour" => (MAX_RELATIVE_MINUTES / 60, 60),
        "day" => (MAX_RELATIVE_MINUTES / (60 * 24), 60 * 24),
        "week" => (MAX_RELATIVE_MINUTES / (60 * 24 * 7), 60 * 24 * 7),
        unit => anyhow::bail!("Unknown time unit: {unit}"),
    };

    if n > max_for_unit {
        anyhow::bail!("Relative time value too large: {n} {}", &caps[2]);
    }

    // Safe to create Duration now that we've validated the range
    let duration = Duration::minutes(n * minutes_per_unit);
    Ok(Utc::now() - duration)
}

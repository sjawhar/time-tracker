//! Streams command for listing streams with time totals and tags.
//!
//! This module implements `tt streams` which displays all streams
//! from the last 7 days with their direct/delegated time and tags.

use std::fmt::Write;

use anyhow::Result;
use chrono::{DateTime, Local, LocalResult, NaiveDate, NaiveTime, TimeZone, Utc};
use serde::Serialize;
use tt_db::Database;

use super::report::format_duration;

// ========== Period Calculation ==========

/// Converts a local date at midnight to UTC.
/// Handles DST ambiguity by picking the earlier time.
fn local_midnight_to_utc(local_date: NaiveDate) -> DateTime<Utc> {
    let midnight = local_date.and_time(NaiveTime::from_hms_opt(0, 0, 0).unwrap());
    match Local.from_local_datetime(&midnight) {
        LocalResult::Single(dt) | LocalResult::Ambiguous(dt, _) => dt.with_timezone(&Utc),
        LocalResult::None => {
            // DST spring-forward gap at midnight
            let one_am = local_date.and_time(NaiveTime::from_hms_opt(1, 0, 0).unwrap());
            Local
                .from_local_datetime(&one_am)
                .unwrap()
                .with_timezone(&Utc)
        }
    }
}

/// Get the last 7 days boundary (inclusive of today).
fn last_7_days_boundary(today: NaiveDate) -> DateTime<Utc> {
    let start_date = today - chrono::Duration::days(6); // Today + 6 days back = 7 days
    local_midnight_to_utc(start_date)
}

// ========== Stream Data ==========

/// Stream data for display.
#[derive(Debug, Clone, Serialize)]
pub struct StreamEntry {
    pub id: String,
    pub id_short: String,
    pub name: Option<String>,
    pub time_direct_ms: i64,
    pub time_delegated_ms: i64,
    pub tags: Vec<String>,
}

/// Get streams from the last 7 days, filtered and sorted.
pub fn get_streams_for_display(db: &Database, today: NaiveDate) -> Result<Vec<StreamEntry>> {
    let period_start = last_7_days_boundary(today);

    let streams_with_tags = db.get_streams_with_tags()?;

    let mut entries: Vec<StreamEntry> = streams_with_tags
        .into_iter()
        .filter(|(stream, _)| {
            // Filter by period: last_event_at must be within last 7 days
            stream.last_event_at.is_some_and(|t| t >= period_start)
        })
        .filter(|(stream, _)| {
            // Exclude zero-time streams
            stream.time_direct_ms > 0 || stream.time_delegated_ms > 0
        })
        .map(|(stream, tags)| {
            let id_short: String = stream.id.chars().take(6).collect();
            StreamEntry {
                id: stream.id,
                id_short,
                name: stream.name,
                time_direct_ms: stream.time_direct_ms,
                time_delegated_ms: stream.time_delegated_ms,
                tags,
            }
        })
        .collect();

    // Sort by total time descending
    entries.sort_by_key(|e| std::cmp::Reverse(e.time_direct_ms + e.time_delegated_ms));

    Ok(entries)
}

// ========== Human-Readable Output ==========

/// Format streams for human-readable output.
pub fn format_streams(entries: &[StreamEntry]) -> String {
    let mut output = String::new();

    writeln!(output, "STREAMS (last 7 days)").unwrap();
    writeln!(output).unwrap();

    if entries.is_empty() {
        writeln!(output, "No streams with activity in the last 7 days.").unwrap();
        writeln!(output).unwrap();
        writeln!(
            output,
            "Hint: Run 'ssh <remote> tt export | tt import' to import events from a remote host."
        )
        .unwrap();
        return output;
    }

    // Header
    writeln!(
        output,
        "{:<7}  {:<22}  {:>8}  {:>9}  Tags",
        "ID", "Name", "Direct", "Delegated"
    )
    .unwrap();
    writeln!(
        output,
        "───────  ──────────────────────  ────────  ─────────  ──────────────────"
    )
    .unwrap();

    // Rows
    for entry in entries {
        let name = entry.name.as_deref().unwrap_or("(unnamed)");
        // Truncate by characters, not bytes, to avoid panics on multi-byte UTF-8
        let name_display = if name.chars().count() > 22 {
            format!("{}...", name.chars().take(19).collect::<String>())
        } else {
            name.to_string()
        };
        let direct = format_duration(entry.time_direct_ms);
        let delegated = format_duration(entry.time_delegated_ms);
        let tags = entry.tags.join(", ");

        writeln!(
            output,
            "{:<7}  {:<22}  {:>8}  {:>9}  {}",
            entry.id_short, name_display, direct, delegated, tags
        )
        .unwrap();
    }

    // Tip
    writeln!(output).unwrap();
    writeln!(
        output,
        "Tip: Use 'tt tag <id> <tag>' to group sessions into projects."
    )
    .unwrap();

    output
}

// ========== JSON Output ==========

/// JSON output structure.
#[derive(Debug, Serialize)]
pub struct JsonStreams {
    pub streams: Vec<StreamEntry>,
    pub period: JsonPeriod,
}

#[derive(Debug, Serialize)]
pub struct JsonPeriod {
    pub start: String,
    pub end: String,
}

/// Format streams as JSON.
pub fn format_streams_json(entries: &[StreamEntry], today: NaiveDate) -> Result<String> {
    let start_date = today - chrono::Duration::days(6);

    let json_streams = JsonStreams {
        streams: entries.to_vec(),
        period: JsonPeriod {
            start: start_date.format("%Y-%m-%d").to_string(),
            end: today.format("%Y-%m-%d").to_string(),
        },
    };

    Ok(serde_json::to_string_pretty(&json_streams)?)
}

// ========== Public Interface ==========

/// Runs the streams command.
pub fn run(db: &Database, json: bool) -> Result<()> {
    let today = Local::now().date_naive();
    let entries = get_streams_for_display(db, today)?;

    if json {
        let output = format_streams_json(&entries, today)?;
        println!("{output}");
    } else {
        let output = format_streams(&entries);
        print!("{output}");
    }

    Ok(())
}

/// Create a new stream with the given name.
///
/// Generates a UUID, inserts the stream into the database, and prints the ID to stdout.
pub fn create(db: &Database, name: String) -> Result<()> {
    use anyhow::Context;
    use tt_db::Stream;
    use uuid::Uuid;

    let now = Utc::now();

    let stream = Stream {
        id: Uuid::new_v4().to_string(),
        name: Some(name),
        created_at: now,
        updated_at: now,
        time_direct_ms: 0,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: true,
    };

    db.insert_stream(&stream)
        .context("failed to create stream")?;
    println!("{}", stream.id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use insta::assert_snapshot;
    use tt_db::Stream;

    fn make_stream(
        id: &str,
        name: Option<&str>,
        direct_ms: i64,
        delegated_ms: i64,
        last_event_at: Option<DateTime<Utc>>,
    ) -> Stream {
        let now = Utc::now();
        Stream {
            id: id.to_string(),
            name: name.map(String::from),
            created_at: now,
            updated_at: now,
            time_direct_ms: direct_ms,
            time_delegated_ms: delegated_ms,
            first_event_at: last_event_at,
            last_event_at,
            needs_recompute: false,
        }
    }

    #[test]
    fn test_streams_empty_database() {
        let db = Database::open_in_memory().unwrap();
        let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();

        let entries = get_streams_for_display(&db, today).unwrap();
        assert!(entries.is_empty());

        let output = format_streams(&entries);
        assert_snapshot!(output);
    }

    #[test]
    fn test_streams_single_stream_no_tags() {
        let db = Database::open_in_memory().unwrap();
        let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
        let recent = Utc.with_ymd_and_hms(2025, 1, 28, 12, 0, 0).unwrap();

        let stream = make_stream(
            "abc123def456",
            Some("tmux/dev/session-1"),
            8_100_000,
            16_200_000,
            Some(recent),
        );
        db.insert_stream(&stream).unwrap();

        let entries = get_streams_for_display(&db, today).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id_short, "abc123");
        assert!(entries[0].tags.is_empty());

        let output = format_streams(&entries);
        assert_snapshot!(output);
    }

    #[test]
    fn test_streams_multiple_with_tags() {
        let db = Database::open_in_memory().unwrap();
        let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
        let recent = Utc.with_ymd_and_hms(2025, 1, 28, 12, 0, 0).unwrap();

        // Stream 1: higher total time, multiple tags
        let stream1 = make_stream(
            "abc123def456",
            Some("tmux/dev/session-1"),
            8_100_000,
            16_200_000,
            Some(recent),
        );
        db.insert_stream(&stream1).unwrap();
        db.add_tag("abc123def456", "acme-webapp").unwrap();
        db.add_tag("abc123def456", "urgent").unwrap();

        // Stream 2: lower total time, one tag
        let stream2 = make_stream(
            "def456ghi789",
            Some("tmux/dev/session-2"),
            2_700_000,
            4_800_000,
            Some(recent),
        );
        db.insert_stream(&stream2).unwrap();
        db.add_tag("def456ghi789", "internal").unwrap();

        // Stream 3: lowest time, no tags
        let stream3 = make_stream(
            "ghi789jkl012",
            Some("tmux/staging/session-1"),
            1_800_000,
            900_000,
            Some(recent),
        );
        db.insert_stream(&stream3).unwrap();

        let entries = get_streams_for_display(&db, today).unwrap();

        // Should be sorted by total time descending
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].id_short, "abc123"); // 24.3M ms total
        assert_eq!(entries[1].id_short, "def456"); // 7.5M ms total
        assert_eq!(entries[2].id_short, "ghi789"); // 2.7M ms total

        // Check tags
        assert_eq!(entries[0].tags, vec!["acme-webapp", "urgent"]);
        assert_eq!(entries[1].tags, vec!["internal"]);
        assert!(entries[2].tags.is_empty());

        let output = format_streams(&entries);
        assert_snapshot!(output);
    }

    #[test]
    fn test_streams_zero_time_excluded() {
        let db = Database::open_in_memory().unwrap();
        let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
        let recent = Utc.with_ymd_and_hms(2025, 1, 28, 12, 0, 0).unwrap();

        // Stream with time
        let stream1 = make_stream(
            "abc123def456",
            Some("has-time"),
            3_600_000,
            1_800_000,
            Some(recent),
        );
        db.insert_stream(&stream1).unwrap();

        // Stream with zero time
        let stream2 = make_stream("def456ghi789", Some("zero-time"), 0, 0, Some(recent));
        db.insert_stream(&stream2).unwrap();

        let entries = get_streams_for_display(&db, today).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id_short, "abc123");
    }

    #[test]
    fn test_streams_7_day_filtering() {
        let db = Database::open_in_memory().unwrap();
        let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();

        // Stream from 3 days ago (should be included)
        let recent = Utc.with_ymd_and_hms(2025, 1, 26, 12, 0, 0).unwrap();
        let stream1 = make_stream(
            "recent123456",
            Some("recent-stream"),
            3_600_000,
            1_800_000,
            Some(recent),
        );
        db.insert_stream(&stream1).unwrap();

        // Stream from 10 days ago (should be excluded)
        let old = Utc.with_ymd_and_hms(2025, 1, 19, 12, 0, 0).unwrap();
        let stream2 = make_stream(
            "old123456789",
            Some("old-stream"),
            7_200_000,
            3_600_000,
            Some(old),
        );
        db.insert_stream(&stream2).unwrap();

        let entries = get_streams_for_display(&db, today).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id_short, "recent");
    }

    #[test]
    fn test_streams_json_output() {
        let db = Database::open_in_memory().unwrap();
        let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
        let recent = Utc.with_ymd_and_hms(2025, 1, 28, 12, 0, 0).unwrap();

        let stream = make_stream(
            "abc123def456",
            Some("tmux/dev/session-1"),
            8_100_000,
            16_200_000,
            Some(recent),
        );
        db.insert_stream(&stream).unwrap();
        db.add_tag("abc123def456", "acme-webapp").unwrap();

        let entries = get_streams_for_display(&db, today).unwrap();
        let output = format_streams_json(&entries, today).unwrap();
        assert_snapshot!(output);
    }

    #[test]
    fn test_streams_no_last_event_at_excluded() {
        let db = Database::open_in_memory().unwrap();
        let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();

        // Stream without last_event_at
        let stream = make_stream(
            "abc123def456",
            Some("no-last-event"),
            3_600_000,
            1_800_000,
            None,
        );
        db.insert_stream(&stream).unwrap();

        let entries = get_streams_for_display(&db, today).unwrap();
        assert!(
            entries.is_empty(),
            "streams without last_event_at should be excluded"
        );
    }

    #[test]
    fn test_streams_unnamed_display() {
        let db = Database::open_in_memory().unwrap();
        let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
        let recent = Utc.with_ymd_and_hms(2025, 1, 28, 12, 0, 0).unwrap();

        // Stream without a name
        let stream = make_stream("abc123def456", None, 3_600_000, 1_800_000, Some(recent));
        db.insert_stream(&stream).unwrap();

        let entries = get_streams_for_display(&db, today).unwrap();
        let output = format_streams(&entries);

        assert!(
            output.contains("(unnamed)"),
            "unnamed streams should display as (unnamed)"
        );
    }

    #[test]
    fn test_streams_short_id() {
        let db = Database::open_in_memory().unwrap();
        let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
        let recent = Utc.with_ymd_and_hms(2025, 1, 28, 12, 0, 0).unwrap();

        // Stream with short ID (less than 6 chars)
        let stream = make_stream(
            "abc",
            Some("short-id-stream"),
            3_600_000,
            1_800_000,
            Some(recent),
        );
        db.insert_stream(&stream).unwrap();

        let entries = get_streams_for_display(&db, today).unwrap();
        assert_eq!(entries[0].id_short, "abc");
    }

    #[test]
    fn test_streams_unicode_name_truncation() {
        let db = Database::open_in_memory().unwrap();
        let today = NaiveDate::from_ymd_opt(2025, 1, 29).unwrap();
        let recent = Utc.with_ymd_and_hms(2025, 1, 28, 12, 0, 0).unwrap();

        // Stream with a long Unicode name (Chinese characters, 3 bytes each)
        // 25 characters: should be truncated to 19 + "..."
        let long_name = "这是一个很长的中文名称用来测试截断功能是否正确工作";
        let stream = make_stream(
            "abc123def456",
            Some(long_name),
            3_600_000,
            1_800_000,
            Some(recent),
        );
        db.insert_stream(&stream).unwrap();

        let entries = get_streams_for_display(&db, today).unwrap();
        // Should not panic, and should produce valid output
        let output = format_streams(&entries);
        assert!(
            output.contains("..."),
            "long names should be truncated with ..."
        );
        // Verify truncation uses character count, not byte count
        assert!(
            !output.contains(long_name),
            "the full long name should not appear in truncated output"
        );
    }
}

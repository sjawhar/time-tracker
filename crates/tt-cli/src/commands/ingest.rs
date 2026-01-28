//! Implementation of the `tt ingest` command.
//!
//! This command is called by tmux hooks on pane focus changes. It appends
//! events to a JSONL buffer file with:
//! - File locking for concurrent write safety
//! - Debouncing to prevent event storms from rapid pane switching

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fs4::fs_std::FileExt;

use tt_core::{RawEvent, generate_event_id};

use crate::cli::IngestArgs;

/// Debounce window for focus events on the same pane.
const DEBOUNCE_MS: u64 = 500;

/// Run the ingest command.
///
/// Appends a tmux pane focus event to the events.jsonl file.
pub fn run(args: IngestArgs) -> Result<()> {
    // Only handle pane-focus events for now
    if args.event_type != "pane-focus" {
        return Ok(());
    }

    let events_path = get_events_path();

    // Ensure directory exists
    if let Some(parent) = events_path.parent() {
        fs::create_dir_all(parent).context("failed to create time-tracker directory")?;
    }

    // Open file with append + read mode, create if not exists
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(&events_path)
        .context("failed to open events file")?;

    // Acquire exclusive lock (blocks until available)
    file.lock_exclusive()
        .context("failed to acquire file lock")?;

    // Check debounce: read last line, compare pane_id + timestamp
    let now = Utc::now();
    if should_debounce(&file, &args.pane, now, Duration::from_millis(DEBOUNCE_MS))? {
        // Lock released on drop
        return Ok(());
    }

    // Generate event
    let timestamp_str = format_timestamp(now);

    let data = serde_json::json!({
        "pane_id": args.pane,
        "session_name": args.session,
        "window_index": args.window,
    });

    let id = generate_event_id(
        "remote.tmux",
        "tmux_pane_focus",
        &timestamp_str,
        &data.to_string(),
    );

    let event = RawEvent {
        id,
        timestamp: now,
        event_type: "tmux_pane_focus".into(),
        source: "remote.tmux".into(),
        data,
        cwd: Some(args.cwd),
    };

    // Append JSON line
    let json = serde_json::to_string(&event).context("failed to serialize event")?;
    writeln!(&file, "{json}").context("failed to write event")?;

    // Lock released on drop
    Ok(())
}

/// Get the path to the events.jsonl file.
fn get_events_path() -> PathBuf {
    // Use XDG data directory on Linux, falling back to ~/.time-tracker
    let base = dirs::data_dir().unwrap_or_else(|| {
        dirs::home_dir().map_or_else(|| PathBuf::from("."), |h| h.join(".local/share"))
    });
    base.join("time-tracker").join("events.jsonl")
}

/// Format a timestamp in RFC3339 format with milliseconds for ID uniqueness.
///
/// Uses milliseconds to ensure events within the same second get unique IDs.
fn format_timestamp(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Check if we should skip this event due to debouncing.
///
/// Returns true if the last event for the same pane was within the debounce window.
fn should_debounce(
    file: &File,
    pane_id: &str,
    now: DateTime<Utc>,
    debounce_window: Duration,
) -> Result<bool> {
    // Read the last line of the file
    let Some(last_line) = read_last_line(file)? else {
        return Ok(false); // No previous events
    };

    // Parse the last event
    let Ok(last_event) = serde_json::from_str::<RawEvent>(&last_line) else {
        return Ok(false); // Invalid JSON, don't debounce
    };

    // Check if it's a pane focus event for the same pane
    if last_event.event_type != "tmux_pane_focus" {
        return Ok(false);
    }

    let Some(last_pane_id) = last_event.data.get("pane_id").and_then(|v| v.as_str()) else {
        return Ok(false);
    };

    if last_pane_id != pane_id {
        return Ok(false);
    }

    // Check if within debounce window
    let elapsed = now.signed_duration_since(last_event.timestamp);
    let debounce_chrono = chrono::Duration::from_std(debounce_window).unwrap_or_default();

    Ok(elapsed < debounce_chrono)
}

/// Read the last line of a file.
fn read_last_line(file: &File) -> Result<Option<String>> {
    let mut reader = BufReader::new(file);

    // Seek to end to get file size
    let file_size = reader.seek(SeekFrom::End(0))?;
    if file_size == 0 {
        return Ok(None);
    }

    // Read backwards to find last newline
    let mut pos = file_size;
    let mut last_line = String::new();

    // Skip trailing newline if present
    if pos > 0 {
        reader.seek(SeekFrom::Start(pos - 1))?;
        let mut buf = [0u8; 1];
        if std::io::Read::read(&mut reader, &mut buf)? == 1 && buf[0] == b'\n' {
            pos -= 1;
        }
    }

    // Find the start of the last line
    while pos > 0 {
        pos -= 1;
        reader.seek(SeekFrom::Start(pos))?;
        let mut buf = [0u8; 1];
        if std::io::Read::read(&mut reader, &mut buf)? == 1 && buf[0] == b'\n' {
            pos += 1; // Move past the newline
            break;
        }
    }

    // Read from that position to end
    reader.seek(SeekFrom::Start(pos))?;
    reader.read_line(&mut last_line)?;

    // Trim trailing newline
    if last_line.ends_with('\n') {
        last_line.pop();
    }

    if last_line.is_empty() {
        Ok(None)
    } else {
        Ok(Some(last_line))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as IoWrite;
    use tempfile::tempdir;

    #[test]
    fn test_format_timestamp() {
        let dt = DateTime::parse_from_rfc3339("2025-01-25T14:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(format_timestamp(dt), "2025-01-25T14:00:00.000Z");
    }

    #[test]
    fn test_format_timestamp_with_millis() {
        let dt = DateTime::parse_from_rfc3339("2025-01-25T14:00:00.123Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(format_timestamp(dt), "2025-01-25T14:00:00.123Z");
    }

    #[test]
    fn test_read_last_line_empty_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty.jsonl");
        let file = File::create(&path).unwrap();
        let result = read_last_line(&file).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_read_last_line_single_line() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("single.jsonl");
        {
            let mut file = File::create(&path).unwrap();
            writeln!(file, r#"{{"id":"1"}}"#).unwrap();
        }
        let file = File::open(&path).unwrap();
        let result = read_last_line(&file).unwrap();
        assert_eq!(result, Some(r#"{"id":"1"}"#.to_string()));
    }

    #[test]
    fn test_read_last_line_multiple_lines() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("multi.jsonl");
        {
            let mut file = File::create(&path).unwrap();
            writeln!(file, r#"{{"id":"1"}}"#).unwrap();
            writeln!(file, r#"{{"id":"2"}}"#).unwrap();
            writeln!(file, r#"{{"id":"3"}}"#).unwrap();
        }
        let file = File::open(&path).unwrap();
        let result = read_last_line(&file).unwrap();
        assert_eq!(result, Some(r#"{"id":"3"}"#.to_string()));
    }

    #[test]
    fn test_debounce_same_pane_within_window() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("debounce.jsonl");

        // Write an event
        let now = Utc::now();
        let event = RawEvent {
            id: "test".into(),
            timestamp: now,
            event_type: "tmux_pane_focus".into(),
            source: "remote.tmux".into(),
            data: serde_json::json!({"pane_id": "%3"}),
            cwd: None,
        };
        {
            let mut file = File::create(&path).unwrap();
            writeln!(file, "{}", serde_json::to_string(&event).unwrap()).unwrap();
        }

        // Check debounce for same pane
        let file = File::open(&path).unwrap();
        let should_skip =
            should_debounce(&file, "%3", now, Duration::from_millis(DEBOUNCE_MS)).unwrap();
        assert!(should_skip, "Should debounce same pane within window");
    }

    #[test]
    fn test_debounce_same_pane_outside_window() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("debounce2.jsonl");

        // Write an event from 1 second ago
        let old_time = Utc::now() - chrono::Duration::seconds(1);
        let event = RawEvent {
            id: "test".into(),
            timestamp: old_time,
            event_type: "tmux_pane_focus".into(),
            source: "remote.tmux".into(),
            data: serde_json::json!({"pane_id": "%3"}),
            cwd: None,
        };
        {
            let mut file = File::create(&path).unwrap();
            writeln!(file, "{}", serde_json::to_string(&event).unwrap()).unwrap();
        }

        // Check debounce for same pane
        let file = File::open(&path).unwrap();
        let should_skip =
            should_debounce(&file, "%3", Utc::now(), Duration::from_millis(DEBOUNCE_MS)).unwrap();
        assert!(!should_skip, "Should not debounce outside window");
    }

    #[test]
    fn test_debounce_different_pane() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("debounce3.jsonl");

        // Write an event for pane %3
        let now = Utc::now();
        let event = RawEvent {
            id: "test".into(),
            timestamp: now,
            event_type: "tmux_pane_focus".into(),
            source: "remote.tmux".into(),
            data: serde_json::json!({"pane_id": "%3"}),
            cwd: None,
        };
        {
            let mut file = File::create(&path).unwrap();
            writeln!(file, "{}", serde_json::to_string(&event).unwrap()).unwrap();
        }

        // Check debounce for different pane
        let file = File::open(&path).unwrap();
        let should_skip =
            should_debounce(&file, "%4", now, Duration::from_millis(DEBOUNCE_MS)).unwrap();
        assert!(!should_skip, "Should not debounce different pane");
    }

    #[test]
    fn test_ingest_creates_file() {
        let dir = tempdir().unwrap();
        let events_path = dir.path().join("events.jsonl");

        // Manually run the ingest logic with a custom path
        let args = IngestArgs {
            event_type: "pane-focus".into(),
            pane: "%5".into(),
            cwd: "/home/test".into(),
            session: "main".into(),
            window: 0,
        };

        // Create file and write event
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&events_path)
            .unwrap();

        let now = Utc::now();
        let timestamp_str = format_timestamp(now);
        let data = serde_json::json!({
            "pane_id": args.pane,
            "session_name": args.session,
            "window_index": args.window,
        });
        let id = generate_event_id(
            "remote.tmux",
            "tmux_pane_focus",
            &timestamp_str,
            &data.to_string(),
        );
        let event = RawEvent {
            id,
            timestamp: now,
            event_type: "tmux_pane_focus".into(),
            source: "remote.tmux".into(),
            data,
            cwd: Some(args.cwd),
        };
        let json = serde_json::to_string(&event).unwrap();
        writeln!(&mut file, "{json}").unwrap();

        // Verify file contents
        let contents = std::fs::read_to_string(&events_path).unwrap();
        assert!(contents.contains("tmux_pane_focus"));
        assert!(contents.contains("%5"));
        assert!(contents.contains("/home/test"));
    }

    #[test]
    fn test_jsonl_format_is_valid() {
        let dir = tempdir().unwrap();
        let events_path = dir.path().join("events.jsonl");

        // Write multiple events
        {
            let mut file = File::create(&events_path).unwrap();
            for i in 0..3 {
                let event = RawEvent {
                    id: format!("test-{i}"),
                    timestamp: Utc::now(),
                    event_type: "tmux_pane_focus".into(),
                    source: "remote.tmux".into(),
                    data: serde_json::json!({"pane_id": format!("%{}", i)}),
                    cwd: Some("/test".into()),
                };
                writeln!(file, "{}", serde_json::to_string(&event).unwrap()).unwrap();
            }
        }

        // Read and parse each line
        let contents = std::fs::read_to_string(&events_path).unwrap();
        for line in contents.lines() {
            let parsed: RawEvent = serde_json::from_str(line).unwrap();
            assert_eq!(parsed.event_type, "tmux_pane_focus");
        }
    }
}

//! Ingest command for receiving events from tmux hooks.
//!
//! This module handles event ingestion on remote machines. Events are written
//! to a JSONL file for later sync to the local machine.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

/// Event data for a tmux pane focus change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneFocusData {
    pub pane_id: String,
    pub session_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_index: Option<u32>,
}

/// An event to be ingested and written to the events file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestEvent {
    pub id: String,
    pub timestamp: String,
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub data: PaneFocusData,
    /// Working directory where the event occurred.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

fn default_source() -> String {
    "remote.tmux".to_string()
}

impl IngestEvent {
    /// Creates a new pane focus event with a deterministic ID.
    pub fn pane_focus(
        pane_id: String,
        session_name: String,
        window_index: Option<u32>,
        cwd: String,
        timestamp: DateTime<Utc>,
    ) -> Self {
        let timestamp_str = timestamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let id = format!("remote.tmux:tmux_pane_focus:{timestamp_str}:{pane_id}");

        Self {
            id,
            timestamp: timestamp_str,
            source: "remote.tmux".to_string(),
            event_type: "tmux_pane_focus".to_string(),
            data: PaneFocusData {
                pane_id,
                session_name,
                window_index,
            },
            cwd: Some(cwd),
        }
    }
}

/// Debounce window for pane focus events (500ms).
const DEBOUNCE_WINDOW_MS: u64 = 500;

/// Returns the default time tracker data directory.
fn default_data_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".time-tracker")
}

/// Returns the path to the events file within the given data directory.
fn events_path(data_dir: &Path) -> PathBuf {
    data_dir.join("events.jsonl")
}

/// Returns the path to the debounce state file within the given data directory.
fn debounce_path(data_dir: &Path) -> PathBuf {
    data_dir.join(".debounce")
}

/// Returns the path to the lock file within the given data directory.
fn lock_path(data_dir: &Path) -> PathBuf {
    data_dir.join(".lock")
}

/// Checks if an event for the given pane should be debounced, and if not,
/// updates the debounce state.
///
/// Returns `true` if the event should be skipped (within debounce window).
fn check_and_update_debounce(data_dir: &Path, pane_id: &str, now_ms: u64) -> Result<bool> {
    let debounce_file = debounce_path(data_dir);

    // Read existing state
    let content = match fs::read_to_string(&debounce_file) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).context("failed to read debounce file"),
    };

    // Parse entries and check for debounce
    // Format: pane_id:unix_millis (use rsplit_once to handle colons in pane_id)
    let mut should_skip = false;
    let mut entries: Vec<(String, u64)> = Vec::new();

    for line in content.lines() {
        if let Some((stored_pane, stored_time)) = line.rsplit_once(':') {
            if let Ok(stored_ms) = stored_time.parse::<u64>() {
                if stored_pane == pane_id {
                    // Check if within debounce window
                    if now_ms.saturating_sub(stored_ms) < DEBOUNCE_WINDOW_MS {
                        should_skip = true;
                    }
                    // Don't keep this pane's old entry (will add new one if not skipping)
                    continue;
                }
                // Keep recent entries from other panes (within 10s)
                if now_ms.saturating_sub(stored_ms) < 10_000 {
                    entries.push((stored_pane.to_string(), stored_ms));
                }
            }
        }
    }

    if should_skip {
        return Ok(true);
    }

    // Add current entry and write back
    entries.push((pane_id.to_string(), now_ms));

    let new_content = entries
        .iter()
        .map(|(p, t)| format!("{p}:{t}"))
        .collect::<Vec<_>>()
        .join("\n");

    fs::write(&debounce_file, new_content).context("failed to write debounce file")?;

    Ok(false)
}

/// Appends an event to the events file.
fn append_event(data_dir: &Path, event: &IngestEvent) -> Result<()> {
    let events_file = events_path(data_dir);

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&events_file)
        .context("failed to open events file")?;

    let json = serde_json::to_string(event).context("failed to serialize event")?;
    writeln!(file, "{json}").context("failed to write event")?;

    Ok(())
}

/// Ingests a pane focus event to the specified data directory.
///
/// This function:
/// 1. Acquires a lock on the data directory
/// 2. Checks if the event should be debounced
/// 3. If not debounced, writes the event and updates debounce state
fn ingest_pane_focus_impl(
    data_dir: &Path,
    pane_id: &str,
    session_name: &str,
    window_index: Option<u32>,
    cwd: &str,
) -> Result<bool> {
    // Validate required fields are not empty
    if pane_id.is_empty() {
        anyhow::bail!("pane_id cannot be empty");
    }
    if session_name.is_empty() {
        anyhow::bail!("session_name cannot be empty");
    }

    fs::create_dir_all(data_dir).context("failed to create data directory")?;

    // Acquire lock
    let lock_file = File::create(lock_path(data_dir)).context("failed to create lock file")?;
    lock_file
        .lock_exclusive()
        .context("failed to acquire lock")?;

    let now = Utc::now();
    // Use try_into to safely convert, falling back to 0 for timestamps before Unix epoch
    // (which would indicate a misconfigured system clock)
    let now_ms: u64 = now.timestamp_millis().try_into().unwrap_or(0);

    // Check debounce and update state in one pass
    if check_and_update_debounce(data_dir, pane_id, now_ms)? {
        tracing::debug!(pane_id, "debounced pane focus event");
        return Ok(false);
    }

    // Create and write event
    let event = IngestEvent::pane_focus(
        pane_id.to_string(),
        session_name.to_string(),
        window_index,
        cwd.to_string(),
        now,
    );
    append_event(data_dir, &event)?;

    tracing::info!(event_id = %event.id, "ingested pane focus event");

    Ok(true)
}

/// Ingests a pane focus event to the default data directory.
///
/// This is the public API used by the CLI.
pub fn ingest_pane_focus(
    pane_id: &str,
    session_name: &str,
    window_index: Option<u32>,
    cwd: &str,
) -> Result<bool> {
    ingest_pane_focus_impl(
        &default_data_dir(),
        pane_id,
        session_name,
        window_index,
        cwd,
    )
}

/// Reads all events from the events file in the specified data directory.
#[cfg(test)]
fn read_events_from(data_dir: &Path) -> Result<Vec<IngestEvent>> {
    use std::io::{BufRead, BufReader};

    let events_file = events_path(data_dir);
    if !events_file.exists() {
        return Ok(Vec::new());
    }

    let file = File::open(&events_file).context("failed to open events file")?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();

    for line in reader.lines() {
        let line = line.context("failed to read line")?;
        let event: IngestEvent = serde_json::from_str(&line).context("failed to parse event")?;
        events.push(event);
    }

    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_event_serialization_matches_spec() {
        let timestamp = DateTime::parse_from_rfc3339("2025-01-29T12:00:00.000Z")
            .unwrap()
            .with_timezone(&Utc);

        let event = IngestEvent::pane_focus(
            "%3".to_string(),
            "dev".to_string(),
            Some(1),
            "/home/user/project".to_string(),
            timestamp,
        );

        let json = serde_json::to_string_pretty(&event).unwrap();
        insta::assert_snapshot!(json);
    }

    #[test]
    fn test_deterministic_id_same_input() {
        let timestamp = DateTime::parse_from_rfc3339("2025-01-29T12:00:00.000Z")
            .unwrap()
            .with_timezone(&Utc);

        let event1 = IngestEvent::pane_focus(
            "%3".to_string(),
            "dev".to_string(),
            None,
            "/home/user".to_string(),
            timestamp,
        );

        let event2 = IngestEvent::pane_focus(
            "%3".to_string(),
            "dev".to_string(),
            None,
            "/home/user".to_string(),
            timestamp,
        );

        assert_eq!(event1.id, event2.id);
    }

    #[test]
    fn test_different_inputs_different_ids() {
        let timestamp = DateTime::parse_from_rfc3339("2025-01-29T12:00:00.000Z")
            .unwrap()
            .with_timezone(&Utc);

        let event1 = IngestEvent::pane_focus(
            "%3".to_string(),
            "dev".to_string(),
            None,
            "/home/user".to_string(),
            timestamp,
        );

        let event2 = IngestEvent::pane_focus(
            "%4".to_string(), // Different pane
            "dev".to_string(),
            None,
            "/home/user".to_string(),
            timestamp,
        );

        assert_ne!(event1.id, event2.id);
    }

    #[test]
    fn test_ingest_creates_events_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join(".time-tracker");

        let result = ingest_pane_focus_impl(&data_dir, "%1", "main", Some(0), "/home/test");

        assert!(result.is_ok());
        assert!(result.unwrap()); // Event was written

        let events = read_events_from(&data_dir).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data.pane_id, "%1");
        assert_eq!(events[0].data.session_name, "main");
        assert_eq!(events[0].cwd, Some("/home/test".to_string()));
    }

    #[test]
    fn test_debounce_within_window() {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join(".time-tracker");

        // First event should be written
        let result1 = ingest_pane_focus_impl(&data_dir, "%1", "main", None, "/home/test");
        assert!(result1.unwrap());

        // Immediate second event for same pane should be debounced
        let result2 = ingest_pane_focus_impl(&data_dir, "%1", "main", None, "/home/test");
        assert!(!result2.unwrap()); // Debounced

        let events = read_events_from(&data_dir).unwrap();
        assert_eq!(events.len(), 1); // Only one event written
    }

    #[test]
    fn test_debounce_different_panes() {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join(".time-tracker");

        // First pane
        let result1 = ingest_pane_focus_impl(&data_dir, "%1", "main", None, "/home/test");
        assert!(result1.unwrap());

        // Different pane should not be debounced
        let result2 = ingest_pane_focus_impl(&data_dir, "%2", "main", None, "/home/test");
        assert!(result2.unwrap());

        let events = read_events_from(&data_dir).unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_debounce_after_window_expires() {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join(".time-tracker");

        // First event
        let result1 = ingest_pane_focus_impl(&data_dir, "%1", "main", None, "/home/test");
        assert!(result1.unwrap());

        // Wait for debounce window to expire
        thread::sleep(Duration::from_millis(550));

        // Second event should be written
        let result2 = ingest_pane_focus_impl(&data_dir, "%1", "main", None, "/home/test");
        assert!(result2.unwrap());

        let events = read_events_from(&data_dir).unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_events_file_is_valid_jsonl() {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join(".time-tracker");

        ingest_pane_focus_impl(&data_dir, "%1", "session1", Some(0), "/path/a").unwrap();

        ingest_pane_focus_impl(&data_dir, "%2", "session2", None, "/path/b").unwrap();

        // Read raw file and verify each line is valid JSON
        let content = fs::read_to_string(events_path(&data_dir)).unwrap();
        for line in content.lines() {
            let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(parsed.is_object());
            assert!(parsed["id"].is_string());
            assert!(parsed["timestamp"].is_string());
            assert!(parsed["source"].is_string());
            assert!(parsed["type"].is_string());
            assert!(parsed["data"].is_object());
        }
    }

    #[test]
    fn test_empty_pane_id_rejected() {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join(".time-tracker");

        let result = ingest_pane_focus_impl(&data_dir, "", "main", None, "/home/test");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("pane_id"));
    }

    #[test]
    fn test_empty_session_name_rejected() {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join(".time-tracker");

        let result = ingest_pane_focus_impl(&data_dir, "%1", "", None, "/home/test");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("session_name"));
    }
}

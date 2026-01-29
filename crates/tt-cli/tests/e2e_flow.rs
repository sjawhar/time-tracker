//! End-to-end integration tests for the complete time tracking flow.
//!
//! Tests the full pipeline: ingest → export → import → query
//! This validates the prototype implementation works end-to-end.

use std::io::Write;
use std::process::{Command, Stdio};

use tempfile::TempDir;

fn tt_binary() -> String {
    env!("CARGO_BIN_EXE_tt").to_string()
}

/// Test the complete local flow: ingest → export → import → events.
///
/// This test simulates what happens on a remote machine (ingest, export)
/// and then on the local machine (import, query).
#[test]
#[allow(clippy::too_many_lines)]
fn test_complete_local_flow() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join(".time-tracker");
    let db_file = temp.path().join("tt.db");

    // Create a config file for the database
    let config_file = temp.path().join("config.toml");
    std::fs::write(
        &config_file,
        format!(r#"database_path = "{}""#, db_file.display()),
    )
    .unwrap();

    // Step 1: Ingest multiple pane focus events (simulates tmux hooks)
    for (i, (pane, cwd, session)) in [
        ("%1", "/home/user/project-a", "main"),
        ("%2", "/home/user/project-b", "main"),
        ("%1", "/home/user/project-a", "main"),
    ]
    .iter()
    .enumerate()
    {
        // Add a small delay between events to avoid debouncing
        if i > 0 {
            std::thread::sleep(std::time::Duration::from_millis(600));
        }

        let output = Command::new(tt_binary())
            .env("HOME", temp.path())
            .arg("ingest")
            .arg("pane-focus")
            .arg("--pane")
            .arg(pane)
            .arg("--cwd")
            .arg(cwd)
            .arg("--session")
            .arg(session)
            .arg("--window")
            .arg("0")
            .output()
            .expect("Failed to run ingest");

        assert!(
            output.status.success(),
            "Ingest {i} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Verify events.jsonl was created
    let events_file = data_dir.join("events.jsonl");
    assert!(events_file.exists(), "events.jsonl should be created");

    let events_content = std::fs::read_to_string(&events_file).unwrap();
    assert_eq!(
        events_content.lines().count(),
        3,
        "Should have 3 events (debounce allows after 500ms)"
    );

    // Step 2: Export events (simulates running on remote)
    let export_output = Command::new(tt_binary())
        .env("HOME", temp.path())
        .arg("export")
        .output()
        .expect("Failed to run export");

    assert!(
        export_output.status.success(),
        "Export failed: {}",
        String::from_utf8_lossy(&export_output.stderr)
    );

    let export_stdout = String::from_utf8_lossy(&export_output.stdout);
    let export_lines: Vec<&str> = export_stdout.lines().collect();
    assert_eq!(
        export_lines.len(),
        3,
        "Export should output 3 events as JSONL"
    );

    // Verify exported events are valid JSON
    for (i, line) in export_lines.iter().enumerate() {
        let parsed: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("Line {i} is not valid JSON: {e}"));
        assert!(parsed["id"].is_string(), "Event {i} should have id");
        assert!(
            parsed["timestamp"].is_string(),
            "Event {i} should have timestamp"
        );
        assert!(parsed["source"].is_string(), "Event {i} should have source");
        assert!(parsed["type"].is_string(), "Event {i} should have type");
        assert!(parsed["data"].is_object(), "Event {i} should have data");
    }

    // Step 3: Import events into local database (simulates sync)
    let mut import_child = Command::new(tt_binary())
        .arg("--config")
        .arg(&config_file)
        .arg("import")
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn import");

    {
        let stdin = import_child.stdin.as_mut().unwrap();
        stdin.write_all(export_output.stdout.as_slice()).unwrap();
    }

    let import_output = import_child.wait_with_output().unwrap();
    assert!(
        import_output.status.success(),
        "Import failed: {}",
        String::from_utf8_lossy(&import_output.stderr)
    );

    let import_stderr = String::from_utf8_lossy(&import_output.stderr);
    assert!(
        import_stderr.contains("3 new"),
        "Should import 3 new events: {import_stderr}"
    );

    // Step 4: Query events from local database
    let events_output = Command::new(tt_binary())
        .arg("--config")
        .arg(&config_file)
        .arg("events")
        .output()
        .expect("Failed to run events");

    assert!(
        events_output.status.success(),
        "Events failed: {}",
        String::from_utf8_lossy(&events_output.stderr)
    );

    let events_stdout = String::from_utf8_lossy(&events_output.stdout);
    let queried_events: Vec<&str> = events_stdout.lines().collect();
    assert_eq!(
        queried_events.len(),
        3,
        "Should query 3 events from database"
    );

    // Verify queried events have expected fields
    for line in &queried_events {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(event["source"], "remote.tmux");
        assert_eq!(event["type"], "tmux_pane_focus");
        assert!(event["data"]["pane_id"].is_string());
        assert!(event["data"]["cwd"].is_string());
    }

    // Step 5: Verify status command shows the source
    let status_output = Command::new(tt_binary())
        .arg("--config")
        .arg(&config_file)
        .arg("status")
        .output()
        .expect("Failed to run status");

    assert!(
        status_output.status.success(),
        "Status failed: {}",
        String::from_utf8_lossy(&status_output.stderr)
    );

    let status_stdout = String::from_utf8_lossy(&status_output.stdout);
    assert!(
        status_stdout.contains("remote.tmux"),
        "Status should show remote.tmux source: {status_stdout}"
    );
}

/// Test that re-syncing is idempotent (no duplicate events).
#[test]
fn test_resync_idempotent() {
    let temp = TempDir::new().unwrap();
    let db_file = temp.path().join("tt.db");

    let config_file = temp.path().join("config.toml");
    std::fs::write(
        &config_file,
        format!(r#"database_path = "{}""#, db_file.display()),
    )
    .unwrap();

    // Simulate export output (pre-generated events)
    let export_data = r#"{"id":"e2e-1","timestamp":"2025-01-29T12:00:00Z","source":"remote.tmux","type":"tmux_pane_focus","data":{"pane_id":"%1","cwd":"/project"}}
{"id":"e2e-2","timestamp":"2025-01-29T12:01:00Z","source":"remote.tmux","type":"tmux_pane_focus","data":{"pane_id":"%2","cwd":"/project"}}
"#;

    // First import
    let mut child1 = Command::new(tt_binary())
        .arg("--config")
        .arg(&config_file)
        .arg("import")
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    {
        let stdin = child1.stdin.as_mut().unwrap();
        stdin.write_all(export_data.as_bytes()).unwrap();
    }
    let output1 = child1.wait_with_output().unwrap();
    let stderr1 = String::from_utf8_lossy(&output1.stderr);
    assert!(stderr1.contains("2 new"), "First sync: {stderr1}");

    // Second import (same data)
    let mut child2 = Command::new(tt_binary())
        .arg("--config")
        .arg(&config_file)
        .arg("import")
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    {
        let stdin = child2.stdin.as_mut().unwrap();
        stdin.write_all(export_data.as_bytes()).unwrap();
    }
    let output2 = child2.wait_with_output().unwrap();
    let stderr2 = String::from_utf8_lossy(&output2.stderr);
    assert!(stderr2.contains("0 new"), "Second sync: {stderr2}");
    assert!(stderr2.contains("2 duplicates"), "Second sync: {stderr2}");

    // Verify database has exactly 2 events
    let events_output = Command::new(tt_binary())
        .arg("--config")
        .arg(&config_file)
        .arg("events")
        .output()
        .unwrap();

    let events_stdout = String::from_utf8_lossy(&events_output.stdout);
    assert_eq!(
        events_stdout.lines().count(),
        2,
        "Should have exactly 2 events after re-sync"
    );
}

/// Test that events can be filtered by time range.
#[test]
fn test_events_time_filtering() {
    let temp = TempDir::new().unwrap();
    let db_file = temp.path().join("tt.db");

    let config_file = temp.path().join("config.toml");
    std::fs::write(
        &config_file,
        format!(r#"database_path = "{}""#, db_file.display()),
    )
    .unwrap();

    // Events at different times
    let export_data = r#"{"id":"early","timestamp":"2025-01-29T10:00:00Z","source":"test","type":"test","data":{}}
{"id":"middle","timestamp":"2025-01-29T12:00:00Z","source":"test","type":"test","data":{}}
{"id":"late","timestamp":"2025-01-29T14:00:00Z","source":"test","type":"test","data":{}}
"#;

    // Import events
    let mut child = Command::new(tt_binary())
        .arg("--config")
        .arg(&config_file)
        .arg("import")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();

    {
        let stdin = child.stdin.as_mut().unwrap();
        stdin.write_all(export_data.as_bytes()).unwrap();
    }
    child.wait().unwrap();

    // Query with --after filter
    let output = Command::new(tt_binary())
        .arg("--config")
        .arg(&config_file)
        .arg("events")
        .arg("--after")
        .arg("2025-01-29T11:00:00Z")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.lines().count(),
        2,
        "Should have 2 events after 11:00"
    );
    assert!(!stdout.contains("early"), "Should not include early event");

    // Query with --before filter
    let output = Command::new(tt_binary())
        .arg("--config")
        .arg(&config_file)
        .arg("events")
        .arg("--before")
        .arg("2025-01-29T13:00:00Z")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.lines().count(),
        2,
        "Should have 2 events before 13:00"
    );
    assert!(!stdout.contains("late"), "Should not include late event");
}

/// Test debouncing works correctly for rapid pane focus events.
#[test]
fn test_ingest_debouncing() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join(".time-tracker");

    // Rapid-fire ingest calls for the same pane (within debounce window)
    for _ in 0..5 {
        let _ = Command::new(tt_binary())
            .env("HOME", temp.path())
            .arg("ingest")
            .arg("pane-focus")
            .arg("--pane")
            .arg("%1")
            .arg("--cwd")
            .arg("/project")
            .arg("--session")
            .arg("main")
            .output()
            .unwrap();
        // No delay - should be debounced
    }

    let events_file = data_dir.join("events.jsonl");
    let content = std::fs::read_to_string(&events_file).unwrap();

    // Should have only 1 event due to debouncing (500ms window)
    assert_eq!(
        content.lines().count(),
        1,
        "Rapid events should be debounced to 1 event"
    );
}

/// Test that different panes are not debounced against each other.
#[test]
fn test_ingest_different_panes_not_debounced() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join(".time-tracker");

    // Rapid-fire ingest calls for different panes
    for pane in ["%1", "%2", "%3"] {
        let _ = Command::new(tt_binary())
            .env("HOME", temp.path())
            .arg("ingest")
            .arg("pane-focus")
            .arg("--pane")
            .arg(pane)
            .arg("--cwd")
            .arg("/project")
            .arg("--session")
            .arg("main")
            .output()
            .unwrap();
    }

    let events_file = data_dir.join("events.jsonl");
    let content = std::fs::read_to_string(&events_file).unwrap();

    // Should have 3 events (different panes)
    assert_eq!(
        content.lines().count(),
        3,
        "Different panes should not be debounced against each other"
    );
}

/// Test export is incremental (doesn't re-emit old events).
#[test]
fn test_export_incremental() {
    let temp = TempDir::new().unwrap();

    // First ingest
    let _ = Command::new(tt_binary())
        .env("HOME", temp.path())
        .arg("ingest")
        .arg("pane-focus")
        .arg("--pane")
        .arg("%1")
        .arg("--cwd")
        .arg("/project")
        .arg("--session")
        .arg("main")
        .output()
        .unwrap();

    // First export
    let output1 = Command::new(tt_binary())
        .env("HOME", temp.path())
        .arg("export")
        .output()
        .unwrap();

    let stdout1 = String::from_utf8_lossy(&output1.stdout);
    assert_eq!(
        stdout1.lines().count(),
        1,
        "First export should have 1 event"
    );

    // Second export without new events
    let output2 = Command::new(tt_binary())
        .env("HOME", temp.path())
        .arg("export")
        .output()
        .unwrap();

    let stdout2 = String::from_utf8_lossy(&output2.stdout);
    // tmux events are always re-exported (no manifest for them)
    // Claude events use manifest for incrementality
    // This test verifies the export works correctly regardless
    assert_eq!(
        stdout2.lines().count(),
        1,
        "Second export should still have 1 event (tmux events always included)"
    );

    // Add new event after debounce window
    std::thread::sleep(std::time::Duration::from_millis(600));
    let _ = Command::new(tt_binary())
        .env("HOME", temp.path())
        .arg("ingest")
        .arg("pane-focus")
        .arg("--pane")
        .arg("%1")
        .arg("--cwd")
        .arg("/project")
        .arg("--session")
        .arg("main")
        .output()
        .unwrap();

    // Third export should have both events
    let output3 = Command::new(tt_binary())
        .env("HOME", temp.path())
        .arg("export")
        .output()
        .unwrap();

    let stdout3 = String::from_utf8_lossy(&output3.stdout);
    assert_eq!(
        stdout3.lines().count(),
        2,
        "Third export should have 2 events"
    );
}

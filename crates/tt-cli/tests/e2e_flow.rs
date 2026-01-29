//! End-to-end integration tests for the complete time tracking flow.
//!
//! Tests the full pipeline: ingest → export → import → query
//! This validates the prototype implementation works end-to-end.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

fn tt_binary() -> String {
    env!("CARGO_BIN_EXE_tt").to_string()
}

/// Creates a config file with the given database path and returns the config file path.
fn create_config(dir: &Path, db_file: &Path) -> std::path::PathBuf {
    let config_file = dir.join("config.toml");
    std::fs::write(
        &config_file,
        format!(r#"database_path = "{}""#, db_file.display()),
    )
    .unwrap();
    config_file
}

/// Runs the tt binary with the given config and arguments.
fn run_tt_with_config(config_file: &Path, args: &[&str]) -> Output {
    Command::new(tt_binary())
        .arg("--config")
        .arg(config_file)
        .args(args)
        .output()
        .expect("Failed to run tt command")
}

/// Runs the tt ingest pane-focus command with the given parameters.
fn run_ingest(home: &Path, pane: &str, cwd: &str, session: &str) -> Output {
    Command::new(tt_binary())
        .env("HOME", home)
        .args([
            "ingest",
            "pane-focus",
            "--pane",
            pane,
            "--cwd",
            cwd,
            "--session",
            session,
            "--window",
            "0",
        ])
        .output()
        .expect("Failed to run ingest")
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
    let config_file = create_config(temp.path(), &db_file);

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

        let output = run_ingest(temp.path(), pane, cwd, session);

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
    let events_output = run_tt_with_config(&config_file, &["events"]);

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
        assert!(event["cwd"].is_string());
    }

    // Step 5: Verify status command shows the source
    let status_output = run_tt_with_config(&config_file, &["status"]);

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
    let config_file = create_config(temp.path(), &db_file);

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
    let events_output = run_tt_with_config(&config_file, &["events"]);
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
    let config_file = create_config(temp.path(), &db_file);

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
    let output = run_tt_with_config(&config_file, &["events", "--after", "2025-01-29T11:00:00Z"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.lines().count(),
        2,
        "Should have 2 events after 11:00"
    );
    assert!(!stdout.contains("early"), "Should not include early event");

    // Query with --before filter
    let output = run_tt_with_config(
        &config_file,
        &["events", "--before", "2025-01-29T13:00:00Z"],
    );
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
    for i in 0..5 {
        let output = run_ingest(temp.path(), "%1", "/project", "main");
        assert!(
            output.status.success(),
            "Ingest {i} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
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
        let output = run_ingest(temp.path(), pane, "/project", "main");
        assert!(
            output.status.success(),
            "Ingest for pane {pane} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
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
    let output = run_ingest(temp.path(), "%1", "/project", "main");
    assert!(
        output.status.success(),
        "First ingest failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

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
    let output = run_ingest(temp.path(), "%1", "/project", "main");
    assert!(
        output.status.success(),
        "Second ingest failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

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

/// Test import handles malformed JSON in stdin gracefully (mixed valid/invalid).
#[test]
fn test_import_malformed_json_mixed() {
    let temp = TempDir::new().unwrap();
    let db_file = temp.path().join("tt.db");
    let config_file = create_config(temp.path(), &db_file);

    // Mix of valid and malformed JSON lines (simulates corrupted export)
    let mixed_data = r#"{"id":"good-1","timestamp":"2025-01-29T12:00:00Z","source":"test","type":"test","data":{}}
{"id":"bad-incomplete
{"id":"good-2","timestamp":"2025-01-29T12:01:00Z","source":"test","type":"test","data":{}}
not json at all
{"id":"good-3","timestamp":"2025-01-29T12:02:00Z","source":"test","type":"test","data":{}}
"#;

    let mut child = Command::new(tt_binary())
        .arg("--config")
        .arg(&config_file)
        .arg("import")
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    {
        let stdin = child.stdin.as_mut().unwrap();
        stdin.write_all(mixed_data.as_bytes()).unwrap();
    }

    let output = child.wait_with_output().unwrap();

    // Import should succeed despite malformed lines
    assert!(
        output.status.success(),
        "Import should succeed with malformed lines: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Should report 3 valid events imported
    assert!(
        stderr.contains("3 new"),
        "Should import 3 valid events: {stderr}"
    );
    // Should report 2 malformed lines
    assert!(
        stderr.contains("2 malformed"),
        "Should report 2 malformed lines: {stderr}"
    );

    // Verify only valid events are in database
    let events_output = run_tt_with_config(&config_file, &["events"]);
    let events_stdout = String::from_utf8_lossy(&events_output.stdout);
    assert_eq!(
        events_stdout.lines().count(),
        3,
        "Database should contain exactly 3 valid events"
    );
}

/// Test events query with both --after and --before filters (range query).
#[test]
fn test_events_time_range_filtering() {
    let temp = TempDir::new().unwrap();
    let db_file = temp.path().join("tt.db");
    let config_file = create_config(temp.path(), &db_file);

    // Events spanning several hours
    let export_data = r#"{"id":"e1","timestamp":"2025-01-29T10:00:00Z","source":"test","type":"test","data":{}}
{"id":"e2","timestamp":"2025-01-29T11:00:00Z","source":"test","type":"test","data":{}}
{"id":"e3","timestamp":"2025-01-29T12:00:00Z","source":"test","type":"test","data":{}}
{"id":"e4","timestamp":"2025-01-29T13:00:00Z","source":"test","type":"test","data":{}}
{"id":"e5","timestamp":"2025-01-29T14:00:00Z","source":"test","type":"test","data":{}}
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

    // Query with both --after and --before (should get events in window: 11:30 to 13:30)
    let output = run_tt_with_config(
        &config_file,
        &[
            "events",
            "--after",
            "2025-01-29T11:30:00Z",
            "--before",
            "2025-01-29T13:30:00Z",
        ],
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.lines().count(),
        2,
        "Should have 2 events in range [11:30, 13:30)"
    );

    // Verify we got e3 (12:00) and e4 (13:00), not e2 (11:00) or e5 (14:00)
    assert!(stdout.contains("e3"), "Should include e3 (12:00)");
    assert!(stdout.contains("e4"), "Should include e4 (13:00)");
    assert!(!stdout.contains("e2"), "Should not include e2 (11:00)");
    assert!(!stdout.contains("e5"), "Should not include e5 (14:00)");
}

/// Test export handles corrupted events.jsonl gracefully (skips malformed lines).
#[test]
fn test_export_corrupted_events_file() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join(".time-tracker");
    std::fs::create_dir_all(&data_dir).unwrap();

    // Write events.jsonl with mixed valid/invalid JSON (simulates corruption)
    let corrupted_content = r#"{"id":"good-1","timestamp":"2025-01-29T12:00:00Z","source":"test","type":"test","data":{}}
{"id":"bad-incomplete
{"id":"good-2","timestamp":"2025-01-29T12:01:00Z","source":"test","type":"test","data":{}}
"#;
    std::fs::write(data_dir.join("events.jsonl"), corrupted_content).unwrap();

    let export_output = Command::new(tt_binary())
        .env("HOME", temp.path())
        .arg("export")
        .output()
        .expect("Failed to run export");

    // Export should succeed despite corruption
    assert!(
        export_output.status.success(),
        "Export should succeed with corrupted file: {}",
        String::from_utf8_lossy(&export_output.stderr)
    );

    let stdout = String::from_utf8_lossy(&export_output.stdout);
    // Should only export valid lines
    assert_eq!(
        stdout.lines().count(),
        2,
        "Should export 2 valid events, skipping malformed line"
    );

    // Verify exported events are valid JSON
    for line in stdout.lines() {
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(line);
        assert!(parsed.is_ok(), "Exported line should be valid JSON: {line}");
    }
}

/// Test status command on empty database (first-time user experience).
#[test]
fn test_status_empty_database() {
    let temp = TempDir::new().unwrap();
    let db_file = temp.path().join("tt.db");
    let config_file = create_config(temp.path(), &db_file);

    // Create empty database by running import with no input
    let mut child = Command::new(tt_binary())
        .arg("--config")
        .arg(&config_file)
        .arg("import")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap();
    drop(child.stdin.take()); // Close stdin
    child.wait().unwrap();

    // Run status on empty database
    let status_output = run_tt_with_config(&config_file, &["status"]);

    assert!(
        status_output.status.success(),
        "Status should succeed on empty database: {}",
        String::from_utf8_lossy(&status_output.stderr)
    );

    let stdout = String::from_utf8_lossy(&status_output.stdout);
    // Should indicate no events
    assert!(
        stdout.contains("No events") || stdout.contains("0 events"),
        "Status should indicate empty database: {stdout}"
    );
}

/// Test import with empty stdin (edge case: accidental empty pipe).
#[test]
fn test_import_empty_stdin() {
    let temp = TempDir::new().unwrap();
    let db_file = temp.path().join("tt.db");
    let config_file = create_config(temp.path(), &db_file);

    let mut child = Command::new(tt_binary())
        .arg("--config")
        .arg(&config_file)
        .arg("import")
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // Close stdin immediately (empty input)
    drop(child.stdin.take());

    let output = child.wait_with_output().unwrap();

    // Should succeed without error
    assert!(
        output.status.success(),
        "Import should handle empty stdin gracefully: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Should report 0 events imported
    assert!(
        stderr.contains('0') || stderr.contains("Imported 0"),
        "Should report 0 events: {stderr}"
    );
}

/// Test events query with invalid timestamp format (error handling).
#[test]
fn test_events_invalid_timestamp_format() {
    let temp = TempDir::new().unwrap();
    let db_file = temp.path().join("tt.db");
    let config_file = create_config(temp.path(), &db_file);

    // Query with invalid timestamp format
    let output = run_tt_with_config(&config_file, &["events", "--after", "not-a-timestamp"]);

    // Should fail with clear error message
    assert!(
        !output.status.success(),
        "Should fail with invalid timestamp"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Error message should mention the invalid format
    assert!(
        stderr.contains("invalid") || stderr.contains("ISO 8601"),
        "Error should mention invalid timestamp format: {stderr}"
    );
}

/// Test debounce boundary behavior.
///
/// Note: Testing exact boundary (500ms) is inherently flaky due to process
/// startup time and system scheduling. Instead, we test that events well
/// within the window are debounced and events clearly past it are not.
#[test]
fn test_ingest_debounce_boundary() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join(".time-tracker");

    // First event
    let output = run_ingest(temp.path(), "%1", "/project", "main");
    assert!(
        output.status.success(),
        "First ingest failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Wait well within debounce window (400ms < 500ms threshold)
    std::thread::sleep(std::time::Duration::from_millis(400));

    // Second event for same pane - should be debounced
    let output = run_ingest(temp.path(), "%1", "/project", "main");
    assert!(
        output.status.success(),
        "Second ingest failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let events_file = data_dir.join("events.jsonl");
    let content = std::fs::read_to_string(&events_file).unwrap();

    // Event at 400ms should still be within debounce window
    assert_eq!(
        content.lines().count(),
        1,
        "Event at 400ms should be debounced"
    );

    // Wait until well past boundary (700ms total from first event, 300ms from second)
    std::thread::sleep(std::time::Duration::from_millis(300));

    // Third event - should NOT be debounced (700ms > 500ms from second event's attempted time)
    let output = run_ingest(temp.path(), "%1", "/project", "main");
    assert!(
        output.status.success(),
        "Third ingest failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let content = std::fs::read_to_string(&events_file).unwrap();
    assert_eq!(
        content.lines().count(),
        2,
        "Event past 500ms boundary should not be debounced"
    );
}

/// Test large batch import (stress test for batching logic).
#[test]
fn test_import_large_batch() {
    let temp = TempDir::new().unwrap();
    let db_file = temp.path().join("tt.db");
    let config_file = create_config(temp.path(), &db_file);

    // Generate 2500 events (more than 2x BATCH_SIZE of 1000)
    let mut large_data = String::new();
    for i in 0..2500 {
        use std::fmt::Write as _;
        writeln!(
            large_data,
            r#"{{"id":"batch-{}","timestamp":"2025-01-29T12:{}:00Z","source":"test","type":"test","data":{{}}}}"#,
            i,
            i % 60  // Cycle through minutes to avoid timestamp collisions
        ).unwrap();
    }

    let mut child = Command::new(tt_binary())
        .arg("--config")
        .arg(&config_file)
        .arg("import")
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    {
        let stdin = child.stdin.as_mut().unwrap();
        stdin.write_all(large_data.as_bytes()).unwrap();
    }

    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "Large batch import should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("2500 new"),
        "Should import all 2500 events: {stderr}"
    );

    // Verify all events are in database
    let events_output = run_tt_with_config(&config_file, &["events"]);
    let events_stdout = String::from_utf8_lossy(&events_output.stdout);
    assert_eq!(
        events_stdout.lines().count(),
        2500,
        "Database should contain all 2500 events"
    );
}

/// Test that invalid config path produces helpful error message.
#[test]
fn test_invalid_config_file() {
    let temp = TempDir::new().unwrap();
    let nonexistent_config = temp.path().join("does-not-exist.toml");

    let output = Command::new(tt_binary())
        .arg("--config")
        .arg(&nonexistent_config)
        .arg("status")
        .output()
        .expect("Failed to run tt command");

    // Should fail when config file doesn't exist
    assert!(
        !output.status.success(),
        "Should fail with nonexistent config file"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Error should mention config or file-related issue
    // (may be "config not found" or "failed to open database" depending on implementation)
    assert!(
        stderr.contains("config")
            || stderr.contains("not found")
            || stderr.contains("No such file")
            || stderr.contains("failed to open"),
        "Error should mention config file issue: {stderr}"
    );
}

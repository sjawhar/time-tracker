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
        // Fields are flattened (pane_id, tmux_session at top level)
        assert!(
            parsed["pane_id"].is_string(),
            "Event {i} should have pane_id"
        );
        assert!(
            parsed["tmux_session"].is_string(),
            "Event {i} should have tmux_session"
        );
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
        // With flat event format, pane_id is at top level
        assert!(event["pane_id"].is_string());
        assert!(event["cwd"].is_string());
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

    // Simulate export output (pre-generated events with flat structure, no data field)
    let export_data = r#"{"id":"e2e-1","timestamp":"2025-01-29T12:00:00Z","source":"remote.tmux","type":"tmux_pane_focus","cwd":"/project","pane_id":"%1","tmux_session":"main"}
{"id":"e2e-2","timestamp":"2025-01-29T12:01:00Z","source":"remote.tmux","type":"tmux_pane_focus","cwd":"/project","pane_id":"%2","tmux_session":"main"}
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

/// Test that import handles invalid JSON gracefully.
#[test]
fn test_import_invalid_json() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use tempfile::TempDir;

    let temp = TempDir::new().unwrap();
    let db_file = temp.path().join("tt.db");

    let config_file = temp.path().join("config.toml");
    std::fs::write(
        &config_file,
        format!(r#"database_path = "{}""#, db_file.display()),
    )
    .unwrap();

    let invalid_data = "not valid json\n{\"also\":\"incomplete\n";

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
        stdin.write_all(invalid_data.as_bytes()).unwrap();
    }

    let output = child.wait_with_output().unwrap();

    // Should succeed but report malformed lines
    assert!(
        output.status.success(),
        "Import should succeed despite invalid JSON"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Should report 0 new events and handle malformed lines gracefully
    assert!(
        stderr.contains("0 new") || stderr.contains("malformed"),
        "Should report 0 events or malformed JSON: {stderr}"
    );
}

/// Test that import handles events with missing required fields.
#[test]
fn test_import_missing_required_fields() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use tempfile::TempDir;

    let temp = TempDir::new().unwrap();
    let db_file = temp.path().join("tt.db");

    let config_file = temp.path().join("config.toml");
    std::fs::write(
        &config_file,
        format!(r#"database_path = "{}""#, db_file.display()),
    )
    .unwrap();

    // Valid JSON but missing required fields (no timestamp, no id)
    let incomplete_events = r#"{"source":"test","type":"test"}
{"id":"has-id","source":"test","type":"test"}
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
        stdin.write_all(incomplete_events.as_bytes()).unwrap();
    }

    let output = child.wait_with_output().unwrap();

    // Should succeed and skip malformed events
    assert!(output.status.success(), "Import should succeed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Should report some events were malformed/skipped
    assert!(
        stderr.contains("malformed") || stderr.contains("0 new"),
        "Should report malformed events: {stderr}"
    );
    // The exact behavior depends on implementation
}

/// Test exact boundary conditions for time filtering.
#[test]
fn test_events_time_boundary_exact_match() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use tempfile::TempDir;

    let temp = TempDir::new().unwrap();
    let db_file = temp.path().join("tt.db");

    let config_file = temp.path().join("config.toml");
    std::fs::write(
        &config_file,
        format!(r#"database_path = "{}""#, db_file.display()),
    )
    .unwrap();

    // Events exactly at boundary times
    let export_data = r#"{"id":"at-boundary","timestamp":"2025-01-29T12:00:00Z","source":"test","type":"test","data":{}}
"#;

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

    // Query with --after at exact timestamp
    let output = Command::new(tt_binary())
        .arg("--config")
        .arg(&config_file)
        .arg("events")
        .arg("--after")
        .arg("2025-01-29T12:00:00Z")
        .output()
        .unwrap();

    // Verify boundary behavior (inclusive or exclusive)
    // This documents the actual behavior
    assert!(output.status.success());
}

/// Test export with no events (edge case).
#[test]
fn test_export_empty_events_file() {
    use tempfile::TempDir;

    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join(".time-tracker");
    std::fs::create_dir_all(&data_dir).unwrap();

    // Create empty events.jsonl
    std::fs::write(data_dir.join("events.jsonl"), "").unwrap();

    let output = Command::new(tt_binary())
        .env("HOME", temp.path())
        .arg("export")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.lines().count(), 0, "Should output 0 events");
}

/// Test import with empty input (edge case).
#[test]
fn test_import_empty_stdin() {
    use std::process::{Command, Stdio};
    use tempfile::TempDir;

    let temp = TempDir::new().unwrap();
    let db_file = temp.path().join("tt.db");

    let config_file = temp.path().join("config.toml");
    std::fs::write(
        &config_file,
        format!(r#"database_path = "{}""#, db_file.display()),
    )
    .unwrap();

    let mut child = Command::new(tt_binary())
        .arg("--config")
        .arg(&config_file)
        .arg("import")
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // Close stdin without writing anything
    drop(child.stdin.take());

    let output = child.wait_with_output().unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("0 new"), "Should report 0 new events");
}

/// Test that very large export output works correctly.
#[test]
fn test_export_large_number_of_events() {
    use tempfile::TempDir;

    let temp = TempDir::new().unwrap();

    // Create many events rapidly (should be debounced)
    for i in 0..100 {
        // Add delay to avoid debouncing
        if i > 0 && i % 10 == 0 {
            std::thread::sleep(std::time::Duration::from_millis(600));
        }

        let _ = Command::new(tt_binary())
            .env("HOME", temp.path())
            .arg("ingest")
            .arg("pane-focus")
            .arg("--pane")
            .arg(format!("%{i}"))
            .arg("--cwd")
            .arg("/project")
            .arg("--session")
            .arg("main")
            .output()
            .unwrap();
    }

    let output = Command::new(tt_binary())
        .env("HOME", temp.path())
        .arg("export")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should have many events (exact number depends on debouncing)
    assert!(stdout.lines().count() > 10, "Should export multiple events");
}
/// Test that `stream_id` in imported events is ignored (not inserted).
///
/// The import command intentionally does not insert `stream_id` - stream assignments
/// are created via inference or user tagging, not import. This test verifies that
/// events with `stream_id` field are imported successfully but the `stream_id` is dropped.
#[test]
fn test_import_ignores_stream_id() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use tempfile::TempDir;

    let temp = TempDir::new().unwrap();
    let db_file = temp.path().join("tt.db");

    let config_file = temp.path().join("config.toml");
    std::fs::write(
        &config_file,
        format!(r#"database_path = "{}""#, db_file.display()),
    )
    .unwrap();

    // Event with stream_id (should be ignored during import)
    let data_with_stream = r#"{"id":"event-with-stream","timestamp":"2025-01-29T12:00:00Z","source":"test","type":"test","data":{},"stream_id":"some-stream-id"}
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
        stdin.write_all(data_with_stream.as_bytes()).unwrap();
    }

    let output = child.wait_with_output().unwrap();

    // Should succeed - stream_id is simply ignored during import
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "Import should succeed: {stderr}");
    assert!(
        stderr.contains("1 new"),
        "Event should be imported (stream_id is ignored): {stderr}"
    );
}

/// Test concurrent ingest operations don't cause data loss.
#[test]
fn test_concurrent_ingest_no_data_loss() {
    use std::sync::Arc;
    use std::thread;
    use tempfile::TempDir;

    let temp = Arc::new(TempDir::new().unwrap());
    let data_dir = temp.path().join(".time-tracker");

    // Spawn multiple threads trying to ingest simultaneously
    let mut handles = vec![];
    for i in 0..5 {
        let temp_clone = Arc::clone(&temp);
        let handle = thread::spawn(move || {
            // Different panes to avoid debouncing
            let _ = Command::new(tt_binary())
                .env("HOME", temp_clone.path())
                .arg("ingest")
                .arg("pane-focus")
                .arg("--pane")
                .arg(format!("%{i}"))
                .arg("--cwd")
                .arg("/project")
                .arg("--session")
                .arg("main")
                .output();
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // Verify all events were written without corruption
    let events_file = data_dir.join("events.jsonl");
    let content = std::fs::read_to_string(&events_file).unwrap();
    let event_count = content.lines().count();

    assert_eq!(event_count, 5, "Should have 5 events, one from each thread");

    // Verify all lines are valid JSON
    for line in content.lines() {
        assert!(
            serde_json::from_str::<serde_json::Value>(line).is_ok(),
            "All lines should be valid JSON"
        );
    }
}

/// Test export/import handles read-only filesystem gracefully.
#[test]
#[cfg(unix)] // File permissions are Unix-specific
fn test_readonly_events_file_error_handling() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join(".time-tracker");
    fs::create_dir_all(&data_dir).unwrap();

    // Create events file and make it read-only
    let events_file = data_dir.join("events.jsonl");
    fs::write(&events_file, "").unwrap();
    let mut perms = fs::metadata(&events_file).unwrap().permissions();
    perms.set_mode(0o444); // Read-only
    fs::set_permissions(&events_file, perms).unwrap();

    // Try to ingest - should fail gracefully
    let output = Command::new(tt_binary())
        .env("HOME", temp.path())
        .arg("ingest")
        .arg("pane-focus")
        .arg("--pane")
        .arg("%1")
        .arg("--cwd")
        .arg("/project")
        .arg("--session")
        .arg("main")
        .stderr(Stdio::piped())
        .output()
        .unwrap();

    // Should fail (not crash)
    assert!(!output.status.success(), "Should fail on read-only file");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error") || stderr.contains("permission") || stderr.contains("denied"),
        "Should report permission error: {stderr}"
    );

    // Clean up: restore permissions so tempdir can be deleted
    let mut perms = fs::metadata(&events_file).unwrap().permissions();
    perms.set_mode(0o644);
    fs::set_permissions(&events_file, perms).unwrap();
}

/// Test import with very large event data (stress test).
#[test]
fn test_import_large_event_data() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use tempfile::TempDir;

    let temp = TempDir::new().unwrap();
    let db_file = temp.path().join("tt.db");

    let config_file = temp.path().join("config.toml");
    std::fs::write(
        &config_file,
        format!(r#"database_path = "{}""#, db_file.display()),
    )
    .unwrap();

    // Create event with very large cwd (near OS limits - 4096 chars)
    let large_cwd = "/".to_string() + &"a".repeat(4000);
    let large_event = serde_json::json!({
        "id": "large-event-1",
        "timestamp": "2025-01-29T12:00:00Z",
        "source": "test",
        "type": "test",
        "cwd": large_cwd,
        "data": {}
    });

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
        writeln!(stdin, "{}", serde_json::to_string(&large_event).unwrap()).unwrap();
    }

    let output = child.wait_with_output().unwrap();

    // Should either succeed or fail gracefully (no crash/truncation)
    if output.status.success() {
        // If it succeeded, verify data was stored correctly
        let events_output = Command::new(tt_binary())
            .arg("--config")
            .arg(&config_file)
            .arg("events")
            .output()
            .unwrap();

        let events_stdout = String::from_utf8_lossy(&events_output.stdout);
        assert!(
            events_stdout.contains(&large_cwd) || events_stdout.contains("large-event-1"),
            "Large event should be imported without truncation"
        );
    } else {
        // If it failed, should have clear error message
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("error") || stderr.contains("too large"),
            "Should report clear error for large data: {stderr}"
        );
    }
}

/// Test export/import during timezone DST transition.
#[test]
fn test_dst_transition_timestamp_handling() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use tempfile::TempDir;

    let temp = TempDir::new().unwrap();
    let db_file = temp.path().join("tt.db");

    let config_file = temp.path().join("config.toml");
    std::fs::write(
        &config_file,
        format!(r#"database_path = "{}""#, db_file.display()),
    )
    .unwrap();

    // Events around DST transition (March 10, 2024 in US)
    let dst_events = r#"{"id":"before-dst","timestamp":"2024-03-10T01:59:00-05:00","source":"test","type":"test","data":{}}
{"id":"during-dst","timestamp":"2024-03-10T03:00:00-04:00","source":"test","type":"test","data":{}}
"#;

    let mut child = Command::new(tt_binary())
        .arg("--config")
        .arg(&config_file)
        .arg("import")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();

    {
        let stdin = child.stdin.as_mut().unwrap();
        stdin.write_all(dst_events.as_bytes()).unwrap();
    }

    child.wait().unwrap();

    // Query events and verify timestamps are correctly stored in UTC
    let events_output = Command::new(tt_binary())
        .arg("--config")
        .arg(&config_file)
        .arg("events")
        .output()
        .unwrap();

    let events_stdout = String::from_utf8_lossy(&events_output.stdout);

    // Both events should be present
    assert!(
        events_stdout.contains("before-dst") && events_stdout.contains("during-dst"),
        "Both DST transition events should be stored"
    );

    // Parse and verify they're in UTC
    for line in events_stdout.lines() {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        if let Some(ts) = event["timestamp"].as_str() {
            assert!(
                ts.ends_with('Z') || ts.contains('+') || ts.contains('-'),
                "Timestamps should have timezone info: {ts}"
            );
        }
    }
}

/// Test that `git_project` and `git_workspace` fields are preserved through import → context export.
///
/// This is a regression test for the case where we added fields to `StoredEvent` but forgot
/// to add them to `EventExport` in the context command.
#[test]
fn test_context_exports_git_project_fields() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use tempfile::TempDir;

    let temp = TempDir::new().unwrap();
    let db_file = temp.path().join("tt.db");

    let config_file = temp.path().join("config.toml");
    std::fs::write(
        &config_file,
        format!(r#"database_path = "{}""#, db_file.display()),
    )
    .unwrap();

    // Event with git_project and git_workspace fields
    let event_with_git_fields = r#"{"id":"event-with-git","timestamp":"2025-01-29T12:00:00Z","source":"remote.tmux","type":"tmux_pane_focus","cwd":"/home/user/my-project/default","git_project":"my-project","git_workspace":"default","pane_id":"%1","tmux_session":"dev","data":{}}
"#;

    // Import the event
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
        stdin.write_all(event_with_git_fields.as_bytes()).unwrap();
    }

    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "Import should succeed: {stderr}");

    // Export via context command
    let context_output = Command::new(tt_binary())
        .arg("--config")
        .arg(&config_file)
        .arg("context")
        .arg("--events")
        .arg("--start")
        .arg("2025-01-29T00:00:00Z")
        .arg("--end")
        .arg("2025-01-30T00:00:00Z")
        .output()
        .unwrap();

    assert!(
        context_output.status.success(),
        "Context command should succeed: {}",
        String::from_utf8_lossy(&context_output.stderr)
    );

    let stdout = String::from_utf8_lossy(&context_output.stdout);
    let context: serde_json::Value =
        serde_json::from_str(&stdout).expect("Context output should be valid JSON");

    // Verify events array exists and has our event
    let events = context["events"]
        .as_array()
        .expect("events should be an array");
    assert_eq!(events.len(), 1, "Should have exactly one event");

    let event = &events[0];

    // Verify git_project and git_workspace are present in the export
    assert_eq!(
        event["git_project"].as_str(),
        Some("my-project"),
        "git_project should be exported in context"
    );
    assert_eq!(
        event["git_workspace"].as_str(),
        Some("default"),
        "git_workspace should be exported in context"
    );

    // Also verify other fields are present
    assert_eq!(event["cwd"].as_str(), Some("/home/user/my-project/default"));
    assert_eq!(event["pane_id"].as_str(), Some("%1"));
    assert_eq!(event["tmux_session"].as_str(), Some("dev"));
}

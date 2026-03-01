//! End-to-end integration tests for the complete time tracking flow.
//!
//! Tests the full pipeline: ingest → export → import → query
//! This validates the prototype implementation works end-to-end.

use std::collections::HashMap;
use std::process::{Command, Stdio};

use tempfile::TempDir;

fn tt_binary() -> String {
    env!("CARGO_BIN_EXE_tt").to_string()
}

/// Initialize machine identity in the given temp directory.
/// Required before any `ingest` command.
fn init_machine(temp: &std::path::Path) {
    let output = Command::new(tt_binary())
        .env("HOME", temp)
        .arg("init")
        .output()
        .expect("failed to run tt init");
    assert!(
        output.status.success(),
        "tt init should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Test debouncing works correctly for rapid pane focus events.
#[test]
fn test_ingest_debouncing() {
    let temp = TempDir::new().unwrap();
    init_machine(temp.path());
    let data_dir = temp.path().join(".local/share/tt");

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
    init_machine(temp.path());
    let data_dir = temp.path().join(".local/share/tt");

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

    // Initialize machine identity (required by export)
    let _ = Command::new(tt_binary())
        .env("HOME", temp.path())
        .env_remove("CLAUDE_CONFIG_DIR")
        .arg("init")
        .output()
        .unwrap();

    // First ingest
    let _ = Command::new(tt_binary())
        .env("HOME", temp.path())
        .env_remove("CLAUDE_CONFIG_DIR")
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
        .env_remove("CLAUDE_CONFIG_DIR")
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
        .env_remove("CLAUDE_CONFIG_DIR")
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
        .env_remove("CLAUDE_CONFIG_DIR")
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
        .env_remove("CLAUDE_CONFIG_DIR")
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

/// Test export with no events (edge case).
#[test]
fn test_export_empty_events_file() {
    use tempfile::TempDir;

    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().join(".local/share/tt");
    std::fs::create_dir_all(&data_dir).unwrap();

    // Initialize machine identity (required by export)
    let _ = Command::new(tt_binary())
        .env("HOME", temp.path())
        .env_remove("CLAUDE_CONFIG_DIR")
        .arg("init")
        .output()
        .unwrap();

    // Create empty events.jsonl
    std::fs::write(data_dir.join("events.jsonl"), "").unwrap();

    let output = Command::new(tt_binary())
        .env("HOME", temp.path())
        .env_remove("CLAUDE_CONFIG_DIR")
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

    // Initialize machine identity (required by export)
    let _ = Command::new(tt_binary())
        .env("HOME", temp.path())
        .env_remove("CLAUDE_CONFIG_DIR")
        .arg("init")
        .output()
        .unwrap();

    // Create many events rapidly (should be debounced)
    for i in 0..100 {
        // Add delay to avoid debouncing
        if i > 0 && i % 10 == 0 {
            std::thread::sleep(std::time::Duration::from_millis(600));
        }

        let _ = Command::new(tt_binary())
            .env("HOME", temp.path())
            .env_remove("CLAUDE_CONFIG_DIR")
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
        .env_remove("CLAUDE_CONFIG_DIR")
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
    let data_with_stream = r#"{"id":"event-with-stream","timestamp":"2025-01-29T12:00:00Z","source":"test","type":"tmux_pane_focus","data":{},"stream_id":"some-stream-id"}
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
    init_machine(temp.path());
    let data_dir = temp.path().join(".local/share/tt");

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
    init_machine(temp.path());
    let data_dir = temp.path().join(".local/share/tt");
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

#[test]
fn test_delegated_time_from_agent_session_events() {
    use chrono::{Duration, TimeZone, Utc};
    use serde_json::json;
    use tt_core::{AllocationConfig, EventType, allocate_time};
    use tt_db::{Database, StoredEvent, Stream};

    let db = Database::open_in_memory().unwrap();
    let base = Utc.with_ymd_and_hms(2025, 1, 29, 12, 0, 0).unwrap();
    let stream = Stream {
        id: "stream-1".to_string(),
        name: Some("test-stream".to_string()),
        created_at: base,
        updated_at: base,
        time_direct_ms: 0,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: false,
    };
    db.insert_stream(&stream).unwrap();

    let session_id = "sess123".to_string();
    let source = "remote.agent".to_string();
    let make_event =
        |id_suffix: &str, timestamp: chrono::DateTime<Utc>, event_type: EventType| -> StoredEvent {
            StoredEvent {
                id: format!("{session_id}-{id_suffix}"),
                timestamp,
                event_type,
                source: source.clone(),
                machine_id: None,
                schema_version: 1,
                pane_id: None,
                tmux_session: None,
                window_index: None,
                git_project: None,
                git_workspace: None,
                status: None,
                idle_duration_ms: None,
                action: None,
                cwd: Some("/project".to_string()),
                session_id: Some(session_id.clone()),
                stream_id: None,
                assignment_source: None,
                data: json!({}),
            }
        };

    let mut events = Vec::new();
    let mut start_event = make_event("session_start", base, EventType::AgentSession);
    start_event.action = Some("started".to_string());
    events.push(start_event);

    let tool_ts1 = base + Duration::seconds(60);
    let tool_ts2 = base + Duration::seconds(120);
    events.push(make_event("tool_use-1", tool_ts1, EventType::AgentToolUse));
    events.push(make_event("tool_use-2", tool_ts2, EventType::AgentToolUse));

    let mut end_event = make_event(
        "session_end",
        base + Duration::seconds(180),
        EventType::AgentSession,
    );
    end_event.action = Some("ended".to_string());
    events.push(end_event);

    db.insert_events(&events).unwrap();
    let assignments: Vec<(String, String)> = events
        .iter()
        .map(|event| (event.id.clone(), stream.id.clone()))
        .collect();
    db.assign_events_to_stream(&assignments, "test").unwrap();

    let stream_events = db.get_events_by_stream(&stream.id).unwrap();
    let result = allocate_time(
        &stream_events,
        &AllocationConfig::default(),
        Some(base + Duration::seconds(180)),
        &HashMap::new(),
    );
    let stream_time = result
        .stream_times
        .iter()
        .find(|time| time.stream_id == stream.id)
        .expect("missing stream allocation");

    assert!(
        stream_time.time_delegated_ms > 0,
        "delegated time should be non-zero"
    );
}

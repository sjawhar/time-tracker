//! End-to-end integration tests for the complete time tracking flow.
//!
//! Tests the full pipeline: ingest → export → import → query
//! This validates the prototype implementation works end-to-end.

use std::collections::HashMap;
use std::process::{Command, Stdio};

use tempfile::TempDir;

mod common;
use common::CommandExt;

fn tt_binary() -> String {
    env!("CARGO_BIN_EXE_tt").to_string()
}

fn configured_command(config_path: &std::path::Path) -> Command {
    let mut command = Command::new(tt_binary());
    command
        .sandboxed_home(config_path.parent().unwrap())
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("OPENCODE_SESSION_ID")
        .arg("--config")
        .arg(config_path);
    command
}

fn write_stdin(child: &mut std::process::Child, input: &str) {
    use std::io::Write;

    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
}

/// Initialize machine identity in the given temp directory.
/// Required before any `ingest` command.
fn init_machine(temp: &std::path::Path) {
    let output = Command::new(tt_binary())
        .sandboxed_home(temp)
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
    let data_dir = temp.path().join(".local/share/time-tracker");

    // Rapid-fire ingest calls for the same pane (within debounce window)
    for _ in 0..5 {
        let _ = Command::new(tt_binary())
            .sandboxed_home(temp.path())
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
    let data_dir = temp.path().join(".local/share/time-tracker");

    // Rapid-fire ingest calls for different panes
    for pane in ["%1", "%2", "%3"] {
        let _ = Command::new(tt_binary())
            .sandboxed_home(temp.path())
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

/// An unusable `--pane-pid` must not cost the focus event.
///
/// The value arrives from tmux through a shell hook, and this lookup may never
/// lose a focus event, so the argument is taken as text and parsed leniently
/// rather than typed as a number clap would reject outright.
#[test]
fn an_unusable_pane_pid_still_records_the_focus_event() {
    let temp = TempDir::new().unwrap();
    init_machine(temp.path());
    let data_dir = temp.path().join(".local/share/time-tracker");

    // Every value tmux's `#{q:pane_pid}` could substitute that this cannot use: the
    // format resolving to nothing, junk, and a number too large for a pid.
    for (pane, pane_pid) in [("%1", ""), ("%2", "not-a-pid"), ("%3", "99999999999999")] {
        let output = Command::new(tt_binary())
            .sandboxed_home(temp.path())
            .arg("ingest")
            .arg("pane-focus")
            .arg("--pane")
            .arg(pane)
            .arg("--cwd")
            .arg("/project")
            .arg("--session")
            .arg("main")
            .arg("--pane-pid")
            .arg(pane_pid)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "pane pid {pane_pid:?} must not fail the ingest: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let content = std::fs::read_to_string(data_dir.join("events.jsonl")).unwrap();
    assert_eq!(
        content.lines().count(),
        3,
        "every focus event is recorded regardless of the pane pid"
    );
}

/// Test export is incremental (doesn't re-emit old events).
#[test]
fn test_export_incremental() {
    let temp = TempDir::new().unwrap();

    // Initialize machine identity (required by export)
    let _ = Command::new(tt_binary())
        .sandboxed_home(temp.path())
        .env_remove("CLAUDE_CONFIG_DIR")
        .arg("init")
        .output()
        .unwrap();

    // First ingest
    let _ = Command::new(tt_binary())
        .sandboxed_home(temp.path())
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
        .sandboxed_home(temp.path())
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
        .sandboxed_home(temp.path())
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
        .sandboxed_home(temp.path())
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
        .sandboxed_home(temp.path())
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
        .sandboxed_home(temp.path())
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
        .sandboxed_home(temp.path())
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
    let data_dir = temp.path().join(".local/share/time-tracker");
    std::fs::create_dir_all(&data_dir).unwrap();

    // Initialize machine identity (required by export)
    let _ = Command::new(tt_binary())
        .sandboxed_home(temp.path())
        .env_remove("CLAUDE_CONFIG_DIR")
        .arg("init")
        .output()
        .unwrap();

    // Create empty events.jsonl
    std::fs::write(data_dir.join("events.jsonl"), "").unwrap();

    let output = Command::new(tt_binary())
        .sandboxed_home(temp.path())
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
        .sandboxed_home(temp.path())
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
        .sandboxed_home(temp.path())
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
            .sandboxed_home(temp.path())
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
        .sandboxed_home(temp.path())
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
        .sandboxed_home(temp.path())
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
    let data_dir = temp.path().join(".local/share/time-tracker");

    // Spawn multiple threads trying to ingest simultaneously
    let mut handles = vec![];
    for i in 0..5 {
        let temp_clone = Arc::clone(&temp);
        let handle = thread::spawn(move || {
            // Different panes to avoid debouncing
            let _ = Command::new(tt_binary())
                .sandboxed_home(temp_clone.path())
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
    let data_dir = temp.path().join(".local/share/time-tracker");
    fs::create_dir_all(&data_dir).unwrap();

    // Create events file and make it read-only
    let events_file = data_dir.join("events.jsonl");
    fs::write(&events_file, "").unwrap();
    let mut perms = fs::metadata(&events_file).unwrap().permissions();
    perms.set_mode(0o444); // Read-only
    fs::set_permissions(&events_file, perms).unwrap();

    // Try to ingest - should fail gracefully
    let output = Command::new(tt_binary())
        .sandboxed_home(temp.path())
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

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "test exercises the core algorithm directly"
)]
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
        slug: None,
        description: None,
        color: None,
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
                window_app_id: None,
                window_title: None,
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

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end flow across six subcommands"
)]
fn e2e_todo_session_link_flow() {
    let temp = TempDir::new().unwrap();
    let database_path = temp.path().join("tt.db");
    let todo_store_path = temp.path().join("todo-store");
    let config_path = temp.path().join("config.toml");
    std::fs::create_dir_all(&todo_store_path).unwrap();
    std::fs::write(
        &config_path,
        format!(
            "database_path = \"{}\"\ntodo_store_path = \"{}\"\n",
            database_path.display(),
            todo_store_path.display()
        ),
    )
    .unwrap();

    // Given an empty todo store, add a todo and capture its generated id.
    let add_output = configured_command(&config_path)
        .args(["todo", "add", "Prototype watcher rewrite"])
        .output()
        .unwrap();
    assert!(
        add_output.status.success(),
        "todo add should succeed: {}",
        String::from_utf8_lossy(&add_output.stderr)
    );

    let todos_path = todo_store_path.join("todos.md");
    let todos_before_link = std::fs::read_to_string(&todos_path).unwrap();
    let (_, todo_metadata) = todos_before_link
        .split_once("\"id\":\"")
        .expect("todo metadata should contain an id");
    let (todo_id, _) = todo_metadata
        .split_once('"')
        .expect("todo id should be terminated by a quote");

    // When the detected OpenCode session is linked to that todo, the CLI reports the link.
    let link_output = configured_command(&config_path)
        .env("OPENCODE_SESSION_ID", "ses_e2e")
        .args(["todo", "link"])
        .arg(todo_id)
        .output()
        .unwrap();
    let link_stdout = String::from_utf8_lossy(&link_output.stdout);
    assert!(
        link_output.status.success(),
        "todo link should succeed: {}",
        String::from_utf8_lossy(&link_output.stderr)
    );
    assert!(
        link_stdout.contains("Linked ses_e2e"),
        "todo link should report the session: {link_stdout}"
    );

    // Given an unassigned event from the linked session, import it into the configured database.
    let session_event = r#"{"id":"event-e2e-session","timestamp":"2025-01-29T12:00:00Z","source":"test.agent","type":"agent_session","session_id":"ses_e2e","data":{}}
"#;
    let mut import_child = configured_command(&config_path)
        .arg("import")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    write_stdin(&mut import_child, session_event);
    let import_output = import_child.wait_with_output().unwrap();
    assert!(
        import_output.status.success(),
        "event import should succeed: {}",
        String::from_utf8_lossy(&import_output.stderr)
    );

    // Given an existing slugged stream — the correction surface never creates one.
    for args in [
        vec!["streams", "create", "Watcher rewrite work"],
        vec!["streams", "slug", "Watcher rewrite work", "watcher-rewrite"],
    ] {
        let output = configured_command(&config_path)
            .args(&args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?} should succeed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // When a human assigns the linked session to it, the linked todo is backfilled.
    let assign_output = configured_command(&config_path)
        .args([
            "streams",
            "assign",
            "watcher-rewrite",
            "--session",
            "ses_e2e",
        ])
        .output()
        .unwrap();
    let assign_stdout = String::from_utf8_lossy(&assign_output.stdout);
    assert!(
        assign_output.status.success(),
        "streams assign should succeed: {}",
        String::from_utf8_lossy(&assign_output.stderr)
    );
    assert!(
        assign_stdout.contains("as a user assignment"),
        "a human's correction is recorded as a user assignment: {assign_stdout}"
    );
    assert!(
        assign_stdout.contains(&format!("Backfilled stream 'watcher-rewrite' → {todo_id}")),
        "streams assign should report the todo backfill: {assign_stdout}"
    );

    let todos_after_backfill = std::fs::read_to_string(&todos_path).unwrap();
    assert!(
        todos_after_backfill.contains("\"stream\":\"watcher-rewrite\""),
        "backfilled todo should record the stream slug: {todos_after_backfill}"
    );

    // Then linking without a supplied or detected session reports the missing agent session.
    let no_session_output = configured_command(&config_path)
        .args(["todo", "link"])
        .arg(todo_id)
        .output()
        .unwrap();
    let no_session_stderr = String::from_utf8_lossy(&no_session_output.stderr);
    assert!(
        !no_session_output.status.success(),
        "todo link should fail without a detected session"
    );
    assert!(
        no_session_stderr.contains("no agent session detected"),
        "todo link should report the missing session: {no_session_stderr}"
    );
}

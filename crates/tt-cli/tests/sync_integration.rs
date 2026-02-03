//! Integration tests for the sync command.

use std::io::Write;
use std::process::{Command, Stdio};

use tempfile::NamedTempFile;

/// Test that `tt sync` correctly imports events from a mock export.
///
/// This test verifies the sync pipeline by testing import with mock data
/// and checking that the sync CLI is properly wired up.
#[test]
fn test_sync_import_pipeline_with_mock_data() {
    // Create mock JSONL events (same format as tt export)
    let mock_events = r#"{"id":"sync-test-1","timestamp":"2025-01-29T12:00:00Z","source":"remote.tmux","type":"tmux_pane_focus","data":{}}
{"id":"sync-test-2","timestamp":"2025-01-29T12:01:00Z","source":"remote.tmux","type":"tmux_pane_focus","data":{}}
{"id":"sync-test-3","timestamp":"2025-01-29T12:02:00Z","source":"remote.agent","type":"agent_session","data":{"action":"started"}}
"#;

    // Create a temp database
    let db_file = NamedTempFile::new().unwrap();
    let db_path = db_file.path().to_string_lossy();

    // Create a temp config file pointing to the temp database
    let mut config_file = NamedTempFile::new().unwrap();
    writeln!(config_file, r#"database_path = "{db_path}""#).unwrap();
    config_file.flush().unwrap();

    let tt_binary = env!("CARGO_BIN_EXE_tt");

    // Test the import directly by piping mock data
    let mut child = Command::new(tt_binary)
        .arg("--config")
        .arg(config_file.path())
        .arg("import")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn tt import");

    // Write mock events to stdin
    {
        let stdin = child.stdin.as_mut().unwrap();
        stdin.write_all(mock_events.as_bytes()).unwrap();
    }

    // Wait for import to complete
    let output = child
        .wait_with_output()
        .expect("Failed to wait for tt import");

    assert!(
        output.status.success(),
        "Import failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Check stderr for success message
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("3 new") || stderr.contains("Imported 3"),
        "Expected import success message: {stderr}"
    );
}

/// Test that sync with a non-existent host fails gracefully.
#[test]
fn test_sync_nonexistent_host_fails_gracefully() {
    let db_file = NamedTempFile::new().unwrap();
    let db_path = db_file.path().to_string_lossy();

    let mut config_file = NamedTempFile::new().unwrap();
    writeln!(config_file, r#"database_path = "{db_path}""#).unwrap();
    config_file.flush().unwrap();

    let tt_binary = env!("CARGO_BIN_EXE_tt");
    let output = Command::new(tt_binary)
        .arg("--config")
        .arg(config_file.path())
        .arg("sync")
        .arg("nonexistent-host-12345-integration-test")
        .output()
        .expect("Failed to run tt sync");

    // Should fail (SSH can't connect)
    assert!(!output.status.success());

    // Error message should mention SSH or the failure
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("SSH") || stderr.contains("ssh") || stderr.contains("Could not resolve"),
        "Expected SSH error in stderr: {stderr}"
    );
}

/// Test that sync command help is correctly registered.
#[test]
fn test_sync_command_registered() {
    let tt_binary = env!("CARGO_BIN_EXE_tt");

    // Check that tt --help mentions sync
    let output = Command::new(tt_binary)
        .arg("--help")
        .output()
        .expect("Failed to run tt --help");

    assert!(output.status.success());
    let help_text = String::from_utf8_lossy(&output.stdout);
    assert!(
        help_text.contains("sync"),
        "Expected 'sync' in help output: {help_text}"
    );
}

/// Test that sync command --help shows expected content.
#[test]
fn test_sync_help_content() {
    let tt_binary = env!("CARGO_BIN_EXE_tt");

    let output = Command::new(tt_binary)
        .arg("sync")
        .arg("--help")
        .output()
        .expect("Failed to run tt sync --help");

    assert!(output.status.success());
    let help_text = String::from_utf8_lossy(&output.stdout);

    // Verify key help content
    assert!(
        help_text.contains("Sync events from a remote host"),
        "Expected description: {help_text}"
    );
    assert!(
        help_text.contains("SSH"),
        "Expected SSH mention: {help_text}"
    );
    assert!(
        help_text.contains("REMOTE"),
        "Expected REMOTE argument: {help_text}"
    );
}

/// Test idempotent sync behavior via import.
///
/// Syncing (importing) the same events twice should result in 0 new events
/// on the second run.
#[test]
fn test_sync_idempotent() {
    let mock_events = r#"{"id":"idem-1","timestamp":"2025-01-29T12:00:00Z","source":"test","type":"test","data":{}}
{"id":"idem-2","timestamp":"2025-01-29T12:01:00Z","source":"test","type":"test","data":{}}
"#;

    let db_file = NamedTempFile::new().unwrap();
    let db_path = db_file.path().to_string_lossy();

    let mut config_file = NamedTempFile::new().unwrap();
    writeln!(config_file, r#"database_path = "{db_path}""#).unwrap();
    config_file.flush().unwrap();

    let tt_binary = env!("CARGO_BIN_EXE_tt");

    // First import
    let mut child1 = Command::new(tt_binary)
        .arg("--config")
        .arg(config_file.path())
        .arg("import")
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    {
        let stdin = child1.stdin.as_mut().unwrap();
        stdin.write_all(mock_events.as_bytes()).unwrap();
    }
    let output1 = child1.wait_with_output().unwrap();
    assert!(output1.status.success());

    let stderr1 = String::from_utf8_lossy(&output1.stderr);
    assert!(
        stderr1.contains("2 new"),
        "First import should have 2 new: {stderr1}"
    );

    // Second import of same events
    let mut child2 = Command::new(tt_binary)
        .arg("--config")
        .arg(config_file.path())
        .arg("import")
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    {
        let stdin = child2.stdin.as_mut().unwrap();
        stdin.write_all(mock_events.as_bytes()).unwrap();
    }
    let output2 = child2.wait_with_output().unwrap();
    assert!(output2.status.success());

    let stderr2 = String::from_utf8_lossy(&output2.stderr);
    assert!(
        stderr2.contains("0 new"),
        "Second import should have 0 new (all duplicates): {stderr2}"
    );
    assert!(
        stderr2.contains("2 duplicates"),
        "Second import should have 2 duplicates: {stderr2}"
    );
}

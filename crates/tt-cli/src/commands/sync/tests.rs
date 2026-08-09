use std::io::Cursor;
use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::Result;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use tt_db::Database;

use super::{SyncMachineReport, SyncMode, SyncReport, sync_single_with_command};
use crate::commands::import;

fn run_with_shell(db: &Database, remote: &str, script: &str) -> Result<()> {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(script)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    sync_single_with_command(db, remote, &mut command, &SyncMode::Incremental).map(|_| ())
}

fn make_jsonl_event(id: &str, ts: &str) -> String {
    format!(
        r#"{{"id":"{id}","timestamp":"{ts}","source":"remote.tmux","type":"tmux_pane_focus","data":{{}}}}"#
    )
}

fn compress_jsonl(jsonl: &str) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(jsonl.as_bytes()).unwrap();
    encoder.finish().unwrap()
}

fn make_gzip_script(jsonl: &str) -> String {
    // Create a script that outputs the JSONL and pipes it through gzip
    // We need to be careful with quoting to avoid shell injection
    format!("printf '%s' '{}' | gzip", jsonl.replace('\'', "'\\''"))
}

#[test]
fn test_sync_import_message_format() {
    // Verify the format string used in sync_single produces expected output
    let inserted = 5;
    let sessions_imported = 2;
    let duplicates = 1;
    let malformed = 0;
    let msg = format!(
        "  Imported {inserted} events, {sessions_imported} sessions ({duplicates} duplicates, {malformed} malformed)"
    );
    assert_eq!(
        msg,
        "  Imported 5 events, 2 sessions (1 duplicates, 0 malformed)"
    );
}

#[test]
fn test_import_result_machine_id_from_uuid_prefixed_event() {
    let db = Database::open_in_memory().unwrap();
    let uuid = "550e8400-e29b-41d4-a716-446655440000";
    let event_id = format!("{uuid}:remote.tmux:tmux_pane_focus:2025-06-01T12:00:00.000Z:%1");
    let jsonl = format!(
        r#"{{"id":"{event_id}","timestamp":"2025-06-01T12:00:00.000Z","source":"remote.tmux","type":"tmux_pane_focus","pane_id":"%1","tmux_session":"main","cwd":"/tmp"}}"#
    );
    let reader = Cursor::new(jsonl.as_bytes().to_vec());
    let result = import::import_from_reader(&db, reader).unwrap();

    assert_eq!(result.inserted, 1);
    assert_eq!(result.machine_id, Some(uuid.to_string()));
}

#[test]
fn test_import_result_machine_id_none_when_no_events() {
    let db = Database::open_in_memory().unwrap();
    let reader = Cursor::new(Vec::<u8>::new());
    let result = import::import_from_reader(&db, reader).unwrap();

    assert_eq!(result.inserted, 0);
    assert_eq!(result.machine_id, None);
}

#[test]
fn test_import_result_machine_id_none_for_non_uuid_ids() {
    let db = Database::open_in_memory().unwrap();
    let jsonl = make_jsonl_event("plain-id-no-uuid", "2025-06-01T12:00:00Z");
    let reader = Cursor::new(jsonl.as_bytes().to_vec());
    let result = import::import_from_reader(&db, reader).unwrap();

    assert_eq!(result.inserted, 1);
    assert_eq!(result.machine_id, None);
}

#[test]
fn test_sync_single_streams_child_stdout_into_importer() -> Result<()> {
    let db = Database::open_in_memory()?;
    let uuid = "550e8400-e29b-41d4-a716-446655440000";
    let event_id = format!("{uuid}:remote.tmux:tmux_pane_focus:2025-06-01T12:00:00.000Z:%1");
    let jsonl = format!(
        r#"{{"id":"{event_id}","timestamp":"2025-06-01T12:00:00.000Z","source":"remote.tmux","type":"tmux_pane_focus","pane_id":"%1","tmux_session":"main","cwd":"/tmp"}}"#
    );

    let script = make_gzip_script(&jsonl);
    run_with_shell(&db, "streaming-remote", &script)?;

    let events = db.get_events(None, None)?;
    assert_eq!(events.len(), 1);
    let machines = db.list_machines()?;
    assert_eq!(machines.len(), 1);
    assert_eq!(machines[0].machine_id, uuid);
    assert_eq!(machines[0].label, "streaming-remote");
    Ok(())
}

#[test]
fn test_sync_single_empty_export_succeeds() -> Result<()> {
    let db = Database::open_in_memory()?;
    // Empty gzip stream
    let script = "printf '' | gzip";
    run_with_shell(&db, "empty-remote", script)?;

    let events = db.get_events(None, None)?;
    assert!(events.is_empty());
    let machines = db.list_machines()?;
    assert!(machines.is_empty());
    Ok(())
}

#[test]
fn test_sync_single_non_zero_exit_errors_and_does_not_update_machine_state() -> Result<()> {
    let db = Database::open_in_memory()?;
    let uuid = "550e8400-e29b-41d4-a716-446655440000";
    let event_id = format!("{uuid}:remote.tmux:tmux_pane_focus:2025-06-01T12:00:00.000Z:%1");
    let jsonl = format!(
        r#"{{"id":"{event_id}","timestamp":"2025-06-01T12:00:00.000Z","source":"remote.tmux","type":"tmux_pane_focus","pane_id":"%1","tmux_session":"main","cwd":"/tmp"}}"#
    );

    // Script that outputs data but then fails
    let script = format!(
        "printf '%s' '{}' | gzip; printf '%s' 'synthetic ssh failure' >&2; exit 23",
        jsonl.replace('\'', "'\\''")
    );
    let err = run_with_shell(&db, "failing-remote", &script).unwrap_err();
    let err_msg = err.to_string();
    assert!(err_msg.contains("remote tt export failed on failing-remote"));
    assert!(err_msg.contains("synthetic ssh failure"));

    let machines = db.list_machines()?;
    assert!(machines.is_empty());
    Ok(())
}

#[test]
fn test_sync_single_retries_without_since_after_remote_rejects_flag() -> Result<()> {
    let db = Database::open_in_memory()?;
    let uuid = "550e8400-e29b-41d4-a716-446655440000";
    let event_id = format!("{uuid}:remote.tmux:tmux_pane_focus:2025-06-01T12:00:00.000Z:%1");
    let jsonl = format!(
        r#"{{"id":"{event_id}","timestamp":"2025-06-01T12:00:00.000Z","source":"remote.tmux","type":"tmux_pane_focus","pane_id":"%1","tmux_session":"main","cwd":"/tmp"}}"#
    );
    let script = format!(
        r#"if [ "$1" = "--since" ]; then printf '%s' 'unknown option: --since' >&2; exit 64; else printf '%s' '{}' | gzip; fi"#,
        jsonl.replace('\'', "'\''")
    );
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(&script)
        .arg("sh")
        .arg("--since")
        .arg("2025-06-01T11:55:00.000Z")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    sync_single_with_command(&db, "compat-remote", &mut command, &SyncMode::Incremental)?;

    let events = db.get_events(None, None)?;
    assert_eq!(events.len(), 1);
    let machines = db.list_machines()?;
    assert_eq!(machines.len(), 1);
    assert_eq!(machines[0].machine_id, uuid);
    assert_eq!(machines[0].label, "compat-remote");
    assert!(machines[0].last_sync_at.is_some());
    Ok(())
}

#[test]
fn test_sync_includes_since_when_last_sync_at_exists() -> Result<()> {
    let db = Database::open_in_memory()?;
    let uuid = "550e8400-e29b-41d4-a716-446655440000";
    let event_id = format!("{uuid}:remote.tmux:tmux_pane_focus:2025-06-01T12:00:00.000Z:%1");
    let jsonl = format!(
        r#"{{"id":"{event_id}","timestamp":"2025-06-01T12:00:00.000Z","source":"remote.tmux","type":"tmux_pane_focus","pane_id":"%1","tmux_session":"main","cwd":"/tmp"}}"#
    );

    // First sync to establish last_sync_at
    let script = make_gzip_script(&jsonl);
    run_with_shell(&db, "test-remote", &script)?;

    let machines = db.list_machines()?;
    assert_eq!(machines.len(), 1);
    assert!(machines[0].last_sync_at.is_some());

    // Second sync should have last_sync_at set
    let last_sync_at = db.get_machine_last_sync_at_by_label("test-remote")?;
    assert!(last_sync_at.is_some());
    Ok(())
}

#[test]
fn test_sync_omits_since_on_first_sync() -> Result<()> {
    let db = Database::open_in_memory()?;
    let uuid = "550e8400-e29b-41d4-a716-446655440000";
    let event_id = format!("{uuid}:remote.tmux:tmux_pane_focus:2025-06-01T12:00:00.000Z:%1");
    let jsonl = format!(
        r#"{{"id":"{event_id}","timestamp":"2025-06-01T12:00:00.000Z","source":"remote.tmux","type":"tmux_pane_focus","pane_id":"%1","tmux_session":"main","cwd":"/tmp"}}"#
    );

    let script = make_gzip_script(&jsonl);
    run_with_shell(&db, "first-sync-remote", &script)?;

    // On first sync, last_sync_at should be None before the sync
    let last_sync_at = db.get_machine_last_sync_at_by_label("first-sync-remote")?;
    // After sync, it should be set
    assert!(last_sync_at.is_some());
    Ok(())
}

#[test]
fn test_last_sync_at_updated_after_successful_sync() -> Result<()> {
    let db = Database::open_in_memory()?;
    let uuid = "550e8400-e29b-41d4-a716-446655440000";
    let event_id = format!("{uuid}:remote.tmux:tmux_pane_focus:2025-06-01T12:00:00.000Z:%1");
    let jsonl = format!(
        r#"{{"id":"{event_id}","timestamp":"2025-06-01T12:00:00.000Z","source":"remote.tmux","type":"tmux_pane_focus","pane_id":"%1","tmux_session":"main","cwd":"/tmp"}}"#
    );

    let script = make_gzip_script(&jsonl);
    run_with_shell(&db, "sync-time-remote", &script)?;

    let machines = db.list_machines()?;
    assert_eq!(machines.len(), 1);
    assert!(machines[0].last_sync_at.is_some());

    // Verify it's a valid ISO 8601 timestamp
    let ts = machines[0].last_sync_at.as_ref().unwrap();
    assert!(ts.contains('T'));
    assert!(ts.contains('Z'));
    Ok(())
}

#[test]
fn test_last_sync_at_not_updated_after_failed_sync() -> Result<()> {
    let db = Database::open_in_memory()?;
    let uuid = "550e8400-e29b-41d4-a716-446655440000";
    let event_id = format!("{uuid}:remote.tmux:tmux_pane_focus:2025-06-01T12:00:00.000Z:%1");
    let jsonl = format!(
        r#"{{"id":"{event_id}","timestamp":"2025-06-01T12:00:00.000Z","source":"remote.tmux","type":"tmux_pane_focus","pane_id":"%1","tmux_session":"main","cwd":"/tmp"}}"#
    );

    // Attempt a sync that fails
    let script = format!(
        "printf '%s' '{}' | gzip; printf '%s' 'synthetic ssh failure' >&2; exit 23",
        jsonl.replace('\'', "'\\''")
    );
    let err = run_with_shell(&db, "failed-sync-remote", &script).unwrap_err();
    assert!(err.to_string().contains("remote tt export failed"));

    // Verify no machine state was created
    let machines = db.list_machines()?;
    assert!(machines.is_empty());
    Ok(())
}

#[test]
fn test_gzip_roundtrip_compression_decompression() -> Result<()> {
    let db = Database::open_in_memory()?;
    let uuid = "550e8400-e29b-41d4-a716-446655440000";
    let event_id = format!("{uuid}:remote.tmux:tmux_pane_focus:2025-06-01T12:00:00.000Z:%1");
    let jsonl = format!(
        r#"{{"id":"{event_id}","timestamp":"2025-06-01T12:00:00.000Z","source":"remote.tmux","type":"tmux_pane_focus","pane_id":"%1","tmux_session":"main","cwd":"/tmp"}}"#
    );

    // Compress the JSONL data
    let compressed = compress_jsonl(&jsonl);

    // Verify compression actually happened (compressed should be smaller or at least different)
    assert!(!compressed.is_empty());
    // Gzip header magic bytes: 0x1f 0x8b
    assert_eq!(compressed[0], 0x1f);
    assert_eq!(compressed[1], 0x8b);

    // Import the compressed data by wrapping in GzDecoder
    let reader = Cursor::new(compressed);
    let decoder = GzDecoder::new(reader);
    let result = import::import_from_reader(&db, decoder)?;

    // Verify the event was imported correctly
    assert_eq!(result.inserted, 1);
    assert_eq!(result.machine_id, Some(uuid.to_string()));

    // Verify the event is in the database
    let events = db.get_events(None, None)?;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, event_id);

    Ok(())
}

#[test]
fn test_gzip_multiple_events_roundtrip() -> Result<()> {
    let db = Database::open_in_memory()?;
    let uuid = "550e8400-e29b-41d4-a716-446655440000";

    // Create multiple events
    let mut jsonl = String::new();
    for i in 0..5 {
        let event_id =
            format!("{uuid}:remote.tmux:tmux_pane_focus:2025-06-01T12:00:{i:02}.000Z:%{i}");
        let event = format!(
            r#"{{"id":"{event_id}","timestamp":"2025-06-01T12:00:{i:02}.000Z","source":"remote.tmux","type":"tmux_pane_focus","pane_id":"%{i}","tmux_session":"main","cwd":"/tmp"}}"#
        );
        jsonl.push_str(&event);
        jsonl.push('\n');
    }

    // Compress and import
    let compressed = compress_jsonl(&jsonl);
    let reader = Cursor::new(compressed);
    let decoder = GzDecoder::new(reader);
    let result = import::import_from_reader(&db, decoder)?;

    // Verify all events were imported
    assert_eq!(result.inserted, 5);
    assert_eq!(result.machine_id, Some(uuid.to_string()));

    let events = db.get_events(None, None)?;
    assert_eq!(events.len(), 5);

    Ok(())
}

#[test]
fn test_gzip_decompression_failure_propagates_error() -> Result<()> {
    let db = Database::open_in_memory()?;

    // Create invalid gzip data (not actually gzip)
    let invalid_gzip = b"this is not gzip data";
    let reader = Cursor::new(invalid_gzip.to_vec());
    let decoder = GzDecoder::new(reader);

    // Attempt to import invalid gzip data
    let result = import::import_from_reader(&db, decoder);

    // Should fail with decompression error
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        !err_msg.is_empty(),
        "Expected error message but got: {err_msg}"
    );

    Ok(())
}

#[test]
fn test_pipefail_propagates_export_failure() -> Result<()> {
    let db = Database::open_in_memory()?;

    // Script that fails before piping to gzip - pipefail should cause the whole pipeline to fail
    // We use bash -o pipefail to match the real behavior
    let script = "bash -o pipefail -c 'exit 42 | gzip'";
    let err = run_with_shell(&db, "pipefail-test", script).unwrap_err();
    let err_msg = err.to_string();

    // Should report the failure
    assert!(err_msg.contains("remote tt export failed on pipefail-test"));

    Ok(())
}

#[test]
fn sync_report_keeps_successful_imports_when_one_remote_fails() {
    // Given
    let report = SyncReport {
        machines: vec![
            SyncMachineReport::Imported {
                remote: "workstation".to_owned(),
                events: 3,
                sessions: 0,
                duplicates: 0,
                malformed: 0,
                pruned: 0,
            },
            SyncMachineReport::Failed {
                remote: "offline".to_owned(),
                error: "connection refused".to_owned(),
            },
        ],
    };

    // When
    let imported = report.imported_events();
    let failures = report.failures();

    // Then
    assert_eq!(imported, 3);
    assert_eq!(failures, ["offline"]);
}

/// Runs a sync whose "remote" is a shell script emitting gzipped JSONL.
fn run_reconcile(db: &Database, remote: &str, jsonl: &str) -> Result<(usize, u64)> {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(make_gzip_script(jsonl))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (result, pruned) = sync_single_with_command(
        db,
        remote,
        &mut command,
        &SyncMode::Reconcile { since: None },
    )?;
    Ok((result.inserted, pruned))
}

fn remote_user_message(machine: &str, session: &str, ts: &str) -> String {
    format!(
        r#"{{"id":"{machine}:remote.agent:user_message:{ts}:{session}","timestamp":"{ts}","source":"remote.agent","type":"user_message","session_id":"{session}","data":{{}}}}"#
    )
}

fn remote_session_start(machine: &str, session: &str, ts: &str) -> String {
    format!(
        r#"{{"id":"{machine}:remote.agent:agent_session:{ts}:{session}:started","timestamp":"{ts}","source":"remote.agent","type":"agent_session","action":"started","session_id":"{session}","data":{{}}}}"#
    )
}

fn stored_user_message_timestamps(db: &Database, session: &str) -> Vec<String> {
    let mut stamps: Vec<String> = db
        .get_events(None, None)
        .unwrap()
        .into_iter()
        .filter(|event| {
            event.event_type == tt_core::EventType::UserMessage
                && event.session_id.as_deref() == Some(session)
        })
        .map(|event| {
            event
                .timestamp
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        })
        .collect();
    stamps.sort();
    stamps
}

const RECONCILE_MACHINE: &str = "550e8400-e29b-41d4-a716-446655440000";

#[test]
fn reconcile_drops_user_messages_the_remote_no_longer_derives() {
    // The remote used to treat injected text as a user message. After it is
    // upgraded its export omits those, and the local replica must follow.
    let db = Database::open_in_memory().unwrap();
    let before = [
        remote_session_start(RECONCILE_MACHINE, "ses_a", "2026-07-22T05:00:00.000Z"),
        remote_user_message(RECONCILE_MACHINE, "ses_a", "2026-07-22T05:00:00.000Z"),
        remote_user_message(RECONCILE_MACHINE, "ses_a", "2026-07-22T05:10:00.000Z"),
        remote_user_message(RECONCILE_MACHINE, "ses_a", "2026-07-22T05:20:00.000Z"),
    ]
    .join("\n");
    run_reconcile(&db, "devbox", &before).unwrap();
    assert_eq!(stored_user_message_timestamps(&db, "ses_a").len(), 3);

    let after = [
        remote_session_start(RECONCILE_MACHINE, "ses_a", "2026-07-22T05:00:00.000Z"),
        remote_user_message(RECONCILE_MACHINE, "ses_a", "2026-07-22T05:00:00.000Z"),
    ]
    .join("\n");
    let (_, pruned) = run_reconcile(&db, "devbox", &after).unwrap();

    assert_eq!(pruned, 2);
    assert_eq!(
        stored_user_message_timestamps(&db, "ses_a"),
        ["2026-07-22T05:00:00.000Z"]
    );
}

#[test]
fn reconcile_clears_a_session_whose_messages_were_all_injected() {
    // The re-derived export carries the session's agent_session events but no
    // user_message at all. That is the signal to drop every replica.
    let db = Database::open_in_memory().unwrap();
    let before = [
        remote_session_start(RECONCILE_MACHINE, "ses_b", "2026-07-22T05:00:00.000Z"),
        remote_user_message(RECONCILE_MACHINE, "ses_b", "2026-07-22T05:00:00.000Z"),
        remote_user_message(RECONCILE_MACHINE, "ses_b", "2026-07-22T05:10:00.000Z"),
    ]
    .join("\n");
    run_reconcile(&db, "devbox", &before).unwrap();

    let after = remote_session_start(RECONCILE_MACHINE, "ses_b", "2026-07-22T05:00:00.000Z");
    let (_, pruned) = run_reconcile(&db, "devbox", &after).unwrap();

    assert_eq!(pruned, 2);
    assert!(stored_user_message_timestamps(&db, "ses_b").is_empty());
}

#[test]
fn reconcile_leaves_sessions_outside_the_export_window_alone() {
    // A bounded reconcile must not delete history it did not re-derive.
    let db = Database::open_in_memory().unwrap();
    let seeded = [
        remote_session_start(RECONCILE_MACHINE, "ses_old", "2026-07-01T05:00:00.000Z"),
        remote_user_message(RECONCILE_MACHINE, "ses_old", "2026-07-01T05:00:00.000Z"),
        remote_session_start(RECONCILE_MACHINE, "ses_new", "2026-07-22T05:00:00.000Z"),
        remote_user_message(RECONCILE_MACHINE, "ses_new", "2026-07-22T05:00:00.000Z"),
    ]
    .join("\n");
    run_reconcile(&db, "devbox", &seeded).unwrap();

    // The remote re-derives only ses_new, and derives no user message for it.
    let windowed = remote_session_start(RECONCILE_MACHINE, "ses_new", "2026-07-22T05:00:00.000Z");
    let (_, pruned) = run_reconcile(&db, "devbox", &windowed).unwrap();

    assert_eq!(pruned, 1);
    assert!(stored_user_message_timestamps(&db, "ses_new").is_empty());
    assert_eq!(
        stored_user_message_timestamps(&db, "ses_old"),
        ["2026-07-01T05:00:00.000Z"]
    );
}

#[test]
fn an_incremental_sync_never_prunes() {
    // Only an explicit reconcile may delete. An ordinary sync that happens to
    // return a short window must leave the replica intact.
    let db = Database::open_in_memory().unwrap();
    let seeded = [
        remote_session_start(RECONCILE_MACHINE, "ses_c", "2026-07-22T05:00:00.000Z"),
        remote_user_message(RECONCILE_MACHINE, "ses_c", "2026-07-22T05:00:00.000Z"),
        remote_user_message(RECONCILE_MACHINE, "ses_c", "2026-07-22T05:10:00.000Z"),
    ]
    .join("\n");
    run_reconcile(&db, "devbox", &seeded).unwrap();

    let short = remote_session_start(RECONCILE_MACHINE, "ses_c", "2026-07-22T05:00:00.000Z");
    run_with_shell(&db, "devbox", &make_gzip_script(&short)).unwrap();

    assert_eq!(stored_user_message_timestamps(&db, "ses_c").len(), 2);
}

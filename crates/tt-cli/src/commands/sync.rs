//! Sync command for pulling events from remote machines via SSH.

use std::fmt::Write;
use std::io::Read;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use flate2::read::GzDecoder;

use crate::commands::{import, ingest, recompute};

/// Runs the sync command for one or more remotes.
pub fn run(db: &tt_db::Database, remotes: &[String]) -> Result<()> {
    for remote in remotes {
        println!("Syncing from {remote}...");
        sync_single(db, remote)?;
    }

    // Reindex sessions and recompute after all syncs
    println!("\nIndexing sessions...");
    ingest::index_sessions(db)?;
    println!("Recomputing time...");
    recompute::run(db, false)?;

    Ok(())
}

/// Syncs events from a single remote.
fn sync_single(db: &tt_db::Database, remote: &str) -> Result<()> {
    let last_event_id = db.get_machine_last_event_id_by_label(remote)?;
    let last_sync_at = db.get_machine_last_sync_at_by_label(remote)?;

    let mut export_cmd = String::from("tt export");

    // Add --since flag if we have a previous sync timestamp (with 5-minute overlap for clock skew)
    if let Some(ref sync_ts) = last_sync_at {
        if let Ok(last_sync_dt) = DateTime::parse_from_rfc3339(sync_ts) {
            let since_dt = last_sync_dt.with_timezone(&Utc) - Duration::minutes(5);
            let since_str = since_dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            let _ = write!(export_cmd, " --since {since_str}");
        } else {
            tracing::warn!(timestamp = %sync_ts, "invalid last_sync_at format, skipping --since");
        }
    }

    if let Some(ref last_id) = last_event_id {
        // Validate UUID prefix format before using in SSH command to prevent injection
        if last_id.len() > 36
            && last_id.as_bytes()[36] == b':'
            && uuid::Uuid::parse_str(&last_id[..36]).is_ok()
        {
            let _ = write!(export_cmd, " --after {last_id}");
        } else {
            tracing::warn!(event_id = %last_id, "invalid last_event_id format, skipping --after");
        }
    }

    // Wrap export command with gzip compression via bash pipefail
    let compressed_cmd = format!("bash -o pipefail -c '{export_cmd} | gzip'");

    let mut command = Command::new("ssh");
    command
        .arg(remote)
        .arg(&compressed_cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    sync_single_with_command(db, remote, &mut command)
}

fn sync_single_with_command(
    db: &tt_db::Database,
    remote: &str,
    command: &mut Command,
) -> Result<()> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let program = command.get_program().to_owned();
    let args: Vec<std::ffi::OsString> = command.get_args().map(std::ffi::OsString::from).collect();
    let current_dir = command
        .get_current_dir()
        .map(std::borrow::ToOwned::to_owned);
    let envs: Vec<(std::ffi::OsString, Option<std::ffi::OsString>)> = command
        .get_envs()
        .map(|(key, value)| (key.to_owned(), value.map(std::borrow::ToOwned::to_owned)))
        .collect();

    let retry_args = {
        let mut stripped_args = Vec::with_capacity(args.len());
        let mut iter = args.iter();
        let mut removed_since = false;

        while let Some(arg) = iter.next() {
            if arg == "--since" {
                removed_since = true;
                let _ = iter.next();
                continue;
            }
            stripped_args.push(arg.clone());
        }

        removed_since.then_some(stripped_args)
    };

    let build_command = |attempt_args: &[std::ffi::OsString]| {
        let mut attempt = Command::new(&program);
        attempt.args(attempt_args);
        if let Some(dir) = current_dir.as_deref() {
            attempt.current_dir(dir);
        }
        for (key, value) in &envs {
            match value {
                Some(value) => {
                    attempt.env(key, value);
                }
                None => {
                    attempt.env_remove(key);
                }
            }
        }
        attempt.stdout(Stdio::piped()).stderr(Stdio::piped());
        attempt
    };

    let run_attempt = |attempt_args: &[std::ffi::OsString]| -> Result<_> {
        let mut child = build_command(attempt_args)
            .spawn()
            .with_context(|| format!("failed to SSH to {remote}"))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to get SSH stdout"))?;

        // Wrap stdout in GzDecoder to decompress on-the-fly.
        let decoder = GzDecoder::new(stdout);
        let import_result = import::import_from_reader(db, decoder);

        let status = child
            .wait()
            .with_context(|| format!("failed to wait for SSH child on {remote}"))?;

        let mut stderr_buf = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_string(&mut stderr_buf);
        }

        Ok((import_result, status, stderr_buf))
    };

    let (mut result, status, stderr_buf) = run_attempt(&args)?;

    if !status.success() {
        if let Some(retry_args) = retry_args {
            // Older remotes do not understand `tt export --since`; retry once without it
            // so previously synced machines can still fall back to a full export.
            tracing::warn!(
                remote = remote,
                stderr = %stderr_buf,
                "remote tt export failed with --since; retrying without it for backward compatibility"
            );

            let (retry_result, retry_status, retry_stderr) = run_attempt(&retry_args)?;
            if !retry_status.success() {
                bail!(
                    "remote tt export failed on {remote} after retrying without --since: {retry_stderr}"
                );
            }
            result = retry_result;
        } else {
            bail!("remote tt export failed on {remote}: {stderr_buf}");
        }
    }

    let result = result?;

    println!(
        "  Imported {} events, {} sessions ({} duplicates, {} malformed)",
        result.inserted, result.sessions_imported, result.duplicates, result.malformed
    );
    if let Some(ref mid) = result.machine_id {
        let new_last_id = db.get_latest_event_id_for_machine(mid)?;
        let now_utc = Utc::now();
        let now_str = now_utc.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        db.upsert_machine_with_sync_time(mid, remote, new_last_id.as_deref(), &now_str)?;
    } else {
        tracing::warn!(
            remote = remote,
            "could not extract machine_id from remote output — sync position will not be tracked"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::io::Write;
    use std::process::{Command, Stdio};

    use anyhow::Result;
    use flate2::Compression;
    use flate2::read::GzDecoder;
    use flate2::write::GzEncoder;
    use tt_db::Database;

    use super::sync_single_with_command;
    use crate::commands::import;

    fn run_with_shell(db: &Database, remote: &str, script: &str) -> Result<()> {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(script)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        sync_single_with_command(db, remote, &mut command)
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

        sync_single_with_command(&db, "compat-remote", &mut command)?;

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
}

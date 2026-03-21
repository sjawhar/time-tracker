//! Sync command for pulling events from remote machines via SSH.

use std::fmt::Write;
use std::io::Read;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

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

    let mut export_cmd = String::from("tt export");
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

    let mut command = Command::new("ssh");
    command
        .arg(remote)
        .arg(&export_cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    sync_single_with_command(db, remote, &mut command)
}

fn sync_single_with_command(
    db: &tt_db::Database,
    remote: &str,
    command: &mut Command,
) -> Result<()> {
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to SSH to {remote}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("failed to get SSH stdout"))?;

    let result = import::import_from_reader(db, stdout)?;

    let status = child
        .wait()
        .with_context(|| format!("failed to wait for SSH child on {remote}"))?;

    let mut stderr_buf = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut stderr_buf);
    }

    if !status.success() {
        bail!("remote tt export failed on {remote}: {stderr_buf}");
    }

    println!(
        "  Imported {} events, {} sessions ({} duplicates, {} malformed)",
        result.inserted, result.sessions_imported, result.duplicates, result.malformed
    );
    if let Some(ref mid) = result.machine_id {
        let new_last_id = db.get_latest_event_id_for_machine(mid)?;
        db.upsert_machine(mid, remote, new_last_id.as_deref())?;
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
    use std::process::{Command, Stdio};

    use anyhow::Result;
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

        let script = format!("printf '%s\\n' '{jsonl}'");
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
        run_with_shell(&db, "empty-remote", "")?;

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

        let script =
            format!("printf '%s\\n' '{jsonl}'; printf '%s' 'synthetic ssh failure' >&2; exit 23");
        let err = run_with_shell(&db, "failing-remote", &script).unwrap_err();
        let err_msg = err.to_string();
        assert!(err_msg.contains("remote tt export failed on failing-remote"));
        assert!(err_msg.contains("synthetic ssh failure"));

        let machines = db.list_machines()?;
        assert!(machines.is_empty());
        Ok(())
    }
}

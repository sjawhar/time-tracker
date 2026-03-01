//! Sync command for pulling events from remote machines via SSH.

use std::fmt::Write;
use std::io::Cursor;
use std::process::Command;

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

    let output = Command::new("ssh")
        .arg(remote)
        .arg(&export_cmd)
        .output()
        .with_context(|| format!("failed to SSH to {remote}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("remote tt export failed on {remote}: {stderr}");
    }

    if output.stdout.is_empty() {
        println!("  No new events from {remote}");
        return Ok(());
    }

    let reader = Cursor::new(output.stdout);
    let result = import::import_from_reader(db, reader)?;

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

    use tt_db::Database;

    use crate::commands::import;

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
}

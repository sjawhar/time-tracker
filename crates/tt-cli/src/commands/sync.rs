//! Sync command for pulling events from remote machines via SSH.

use std::fmt::Write;
use std::io::Cursor;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::commands::{import, ingest, recompute};
use crate::machine::extract_machine_id;

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

    let machine_id = extract_machine_id_from_output(&output.stdout);

    let reader = Cursor::new(output.stdout);
    let result = import::import_from_reader(db, reader)?;

    println!(
        "  Imported {} events, {} sessions ({} duplicates, {} malformed)",
        result.inserted, result.sessions_imported, result.duplicates, result.malformed
    );
    if let Some(ref mid) = machine_id {
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

/// Extracts the machine UUID from the first line of export output.
fn extract_machine_id_from_output(stdout: &[u8]) -> Option<String> {
    let first_line = stdout.split(|&b| b == b'\n').next()?;
    let first_line = std::str::from_utf8(first_line).ok()?;
    let value: serde_json::Value = serde_json::from_str(first_line).ok()?;
    let id = value.get("id")?.as_str()?;
    extract_machine_id(id)
}

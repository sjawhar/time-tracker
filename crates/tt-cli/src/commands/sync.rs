//! Sync command for pulling events from a remote host via SSH.
//!
//! This module executes `ssh <remote> tt export` and pipes the output to the
//! local import logic, inserting events into the `SQLite` database.

use std::io::Cursor;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use tt_db::Database;

use crate::commands::import::{ImportResult, import_from_reader};

/// Syncs events from a remote host via SSH.
///
/// Executes `ssh <remote> tt export` and imports the output into the local database.
/// SSH failures (connection refused, command not found, permission denied) are
/// returned as errors with the SSH stderr included.
///
/// Uses SSH with a 30-second connection timeout to prevent indefinite blocking.
/// Additional timeouts can be configured via SSH config (`ServerAliveInterval`, etc.).
pub fn sync_from_remote(db: &Database, remote: &str) -> Result<ImportResult> {
    tracing::info!(remote, "syncing from remote");

    let output = Command::new("ssh")
        .arg("-o")
        .arg("ConnectTimeout=30")
        .arg("-o")
        .arg("ServerAliveInterval=15")
        .arg("-o")
        .arg("ServerAliveCountMax=2")
        .arg(remote)
        .arg("tt")
        .arg("export")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("failed to execute ssh command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);
        bail!(
            "SSH command failed (exit code {exit_code}): {stderr}",
            exit_code = exit_code,
            stderr = stderr.trim()
        );
    }

    import_from_reader(db, Cursor::new(output.stdout))
}

/// Runs the sync command.
pub fn run(db: &Database, remote: &str) -> Result<ImportResult> {
    let result = sync_from_remote(db, remote)?;

    eprintln!(
        "Synced from {remote}: {} events ({} new, {} duplicates, {} malformed)",
        result.total_read, result.inserted, result.duplicates, result.malformed
    );

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_ssh_connection_failure() {
        // Test that SSH connection failures produce clear error messages
        let db = Database::open_in_memory().unwrap();

        // Use an invalid host that will fail to connect
        let result = sync_from_remote(&db, "nonexistent-host-12345");

        assert!(result.is_err());
        let err = result.unwrap_err();
        let err_str = err.to_string();
        // Should contain some indication of SSH failure
        assert!(
            err_str.contains("SSH") || err_str.contains("ssh"),
            "Error should mention SSH: {err_str}"
        );
    }
}

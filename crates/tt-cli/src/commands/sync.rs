//! Sync command for pulling events from remote machines via SSH.

use std::fmt::Write;
use std::io::Read;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use flate2::read::GzDecoder;

use crate::commands::{import, ingest, machines};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncMachineReport {
    Imported {
        remote: String,
        events: usize,
        sessions: usize,
        duplicates: usize,
        malformed: usize,
        /// Replicated `user_message` rows the remote no longer derives.
        /// Always zero outside [`SyncMode::Reconcile`].
        pruned: u64,
    },
    Failed {
        remote: String,
        error: String,
    },
}

/// How much of a remote's history a sync should pull.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncMode {
    /// Pull only what has appeared since the last sync. The default.
    Incremental,

    /// Re-derive the remote's events and converge the local replica onto them.
    ///
    /// Imports are `INSERT OR IGNORE`, so an incremental sync can only ever add
    /// rows. When the remote's extractor changes — for instance once it stops
    /// treating harness-injected text as a user message — the rows it no longer
    /// produces have to be deleted here. Reconciling asks the remote for a full
    /// re-derivation and drops local `user_message` events for the covered
    /// sessions that the remote did not re-emit.
    Reconcile {
        /// Restrict the re-derivation to sessions updated at or after this
        /// RFC3339 time. `None` re-derives the remote's entire history, which
        /// re-scans every session it holds.
        since: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub machines: Vec<SyncMachineReport>,
}

impl SyncReport {
    pub fn imported_events(&self) -> usize {
        self.machines
            .iter()
            .map(|machine| match machine {
                SyncMachineReport::Imported { events, .. } => *events,
                SyncMachineReport::Failed { .. } => 0,
            })
            .sum()
    }

    pub fn failures(&self) -> Vec<&str> {
        self.machines
            .iter()
            .filter_map(|machine| match machine {
                SyncMachineReport::Imported { .. } => None,
                SyncMachineReport::Failed { remote, .. } => Some(remote.as_str()),
            })
            .collect()
    }
}

/// Runs the sync command for one or more remotes.
pub fn run(db: &tt_db::Database, remotes: &[String], mode: &SyncMode) -> Result<()> {
    let report = sync_all(db, remotes, mode)?;
    for machine in &report.machines {
        match machine {
            SyncMachineReport::Imported {
                remote,
                events,
                sessions,
                duplicates,
                malformed,
                pruned,
            } => {
                println!("Syncing from {remote}...");
                println!(
                    "  Imported {events} events, {sessions} sessions ({duplicates} duplicates, {malformed} malformed)"
                );
                if *pruned > 0 {
                    println!("  Pruned {pruned} user_message events {remote} no longer derives");
                }
            }
            SyncMachineReport::Failed { remote, error } => {
                println!("Syncing from {remote}...");
                bail!("sync from {remote} failed: {error}");
            }
        }
    }

    println!("\nIndexing sessions...");
    // Incremental: sync pulls a *remote's* events, and this pass indexes this
    // machine's own transcripts, which the daemon is already keeping current.
    ingest::index_sessions(db, ingest::ScanMode::Incremental)?;

    // Stream times are deliberately NOT recomputed here. Recomputation walks the
    // entire event history — minutes of CPU and gigabytes of RSS — to refresh
    // totals that only `tt streams` reads, and it runs once per remote. `tt
    // streams` states how old its totals are; `tt recompute` refreshes them.
    warn_about_dark_machines(db, Utc::now())?;

    Ok(())
}

/// Warns about every known machine that has stopped producing events.
///
/// Covers remotes outside this invocation on purpose: a machine nobody syncs
/// anymore is exactly the one that goes dark unnoticed.
fn warn_about_dark_machines(db: &tt_db::Database, now: DateTime<Utc>) -> Result<()> {
    let statuses = machines::load_statuses(db)?;
    let warnings = machines::dark_machine_warnings(&statuses, now);
    if !warnings.is_empty() {
        eprintln!();
        for warning in &warnings {
            eprintln!("{warning}");
        }
    }
    Ok(())
}

pub fn sync_all(db: &tt_db::Database, remotes: &[String], mode: &SyncMode) -> Result<SyncReport> {
    let mut machines = Vec::with_capacity(remotes.len());
    for remote in remotes {
        match sync_single_quiet(db, remote, mode) {
            Ok((result, pruned)) => machines.push(SyncMachineReport::Imported {
                remote: remote.clone(),
                events: result.inserted,
                sessions: result.sessions_imported,
                duplicates: result.duplicates,
                malformed: result.malformed,
                pruned,
            }),
            Err(error) => machines.push(SyncMachineReport::Failed {
                remote: remote.clone(),
                error: error.to_string(),
            }),
        }
    }

    Ok(SyncReport { machines })
}

/// Builds the remote `tt export` invocation for `mode`.
///
/// Reconciling deliberately drops the `--after` event cursor: that cursor makes
/// the remote emit only events newer than the last one we hold, which is the
/// opposite of what a re-derivation needs.
fn build_export_command(db: &tt_db::Database, remote: &str, mode: &SyncMode) -> Result<String> {
    let mut export_cmd = String::from("tt export");

    match mode {
        SyncMode::Reconcile { since } => {
            if let Some(since) = since {
                let parsed = DateTime::parse_from_rfc3339(since).with_context(|| {
                    format!("invalid --since timestamp '{since}': expected RFC3339")
                })?;
                let since_str = parsed
                    .with_timezone(&Utc)
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                let _ = write!(export_cmd, " --since {since_str}");
            }
            return Ok(export_cmd);
        }
        SyncMode::Incremental => {}
    }

    let last_sync_at = db.get_machine_last_sync_at_by_label(remote)?;
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

    let last_event_id = db.get_machine_last_event_id_by_label(remote)?;
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

    Ok(export_cmd)
}

fn sync_single_quiet(
    db: &tt_db::Database,
    remote: &str,
    mode: &SyncMode,
) -> Result<(import::ImportResult, u64)> {
    let export_cmd = build_export_command(db, remote, mode)?;

    // Wrap export command with gzip compression via bash pipefail
    let compressed_cmd = format!("bash -o pipefail -c '{export_cmd} | gzip'");

    let mut command = Command::new("ssh");
    command
        .arg(remote)
        .arg(&compressed_cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    sync_single_with_command(db, remote, &mut command, mode)
}

/// Drops `--since <value>` from an SSH argument list.
///
/// Returns `None` when there was nothing to drop, so callers can tell "no
/// fallback available" apart from "fallback is the same command".
fn args_without_since(args: &[std::ffi::OsString]) -> Option<Vec<std::ffi::OsString>> {
    let mut stripped = Vec::with_capacity(args.len());
    let mut iter = args.iter();
    let mut removed_since = false;

    while let Some(arg) = iter.next() {
        if arg == "--since" {
            removed_since = true;
            let _ = iter.next();
            continue;
        }
        stripped.push(arg.clone());
    }

    removed_since.then_some(stripped)
}

fn sync_single_with_command(
    db: &tt_db::Database,
    remote: &str,
    command: &mut Command,
    mode: &SyncMode,
) -> Result<(import::ImportResult, u64)> {
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

    let retry_args = args_without_since(&args);

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

    let reconciling = matches!(mode, SyncMode::Reconcile { .. });
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
        let import_result = if reconciling {
            import::import_from_reader_reconciling(db, decoder)
        } else {
            import::import_from_reader(db, decoder)
        };

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

    // Converge the replica: anything the remote covered but did not re-emit is
    // a row an older extractor wrote and the current one rejects.
    let pruned = if reconciling {
        db.prune_user_message_events(&result.derived_user_messages)
            .with_context(|| format!("failed to reconcile user messages from {remote}"))?
    } else {
        0
    };

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

    Ok((result, pruned))
}

#[cfg(test)]
mod tests;

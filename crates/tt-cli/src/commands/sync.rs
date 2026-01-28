//! Sync command for pulling remote events into the local database.

use std::collections::BTreeMap;
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use clap::Args;
use serde::{Deserialize, Serialize};

use tt_db::Database;

use crate::Config;
use crate::commands::import;

#[derive(Debug, Args)]
pub struct SyncArgs {
    /// Remote host to pull from (e.g. user@host).
    pub remote: String,
}

#[derive(Debug)]
pub struct SyncReport {
    pub remote: String,
    pub imported: usize,
    pub last_event_id: Option<String>,
    pub last_event_timestamp: Option<String>,
}

pub fn run(args: &SyncArgs, config: &Config) -> Result<SyncReport> {
    let state_path = default_sync_state_path()?;
    run_with_state_path(args, config, "ssh", &state_path)
}

fn run_with_state_path(
    args: &SyncArgs,
    config: &Config,
    ssh_command: &str,
    state_path: &Path,
) -> Result<SyncReport> {
    let mut state = SyncState::load(state_path)?;

    let mut child = Command::new(ssh_command)
        .arg(&args.remote)
        .arg("tt")
        .arg("export")
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("failed to start ssh to {}", args.remote))?;

    let stdout = child
        .stdout
        .take()
        .context("failed to capture ssh stdout")?;
    let reader = BufReader::new(stdout);
    let events = import::parse_events(reader, Some(&args.remote))
        .with_context(|| format!("failed to parse events from {}", args.remote))?;

    let status = child
        .wait()
        .with_context(|| format!("failed to wait for ssh to {}", args.remote))?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "ssh to {} exited with status {status}",
            args.remote
        ));
    }

    let mut db = Database::open(&config.database_path)
        .with_context(|| format!("failed to open {}", config.database_path.display()))?;
    let imported = db.insert_events(&events)?;

    let last_event_id = events.last().map(|event| event.id.clone());
    let last_event_timestamp = events.last().map(|event| event.timestamp.clone());
    let synced_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    state.record_sync(
        &args.remote,
        last_event_id.clone(),
        last_event_timestamp.clone(),
        synced_at,
    );
    state.save(state_path)?;

    Ok(SyncReport {
        remote: args.remote.clone(),
        imported,
        last_event_id,
        last_event_timestamp,
    })
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SyncState {
    remotes: BTreeMap<String, SyncPosition>,
}

#[derive(Debug, Serialize, Deserialize)]
#[allow(clippy::struct_field_names)]
struct SyncPosition {
    last_event_id: Option<String>,
    last_event_timestamp: Option<String>,
    last_synced_at: String,
}

impl SyncState {
    fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(contents) => {
                let parsed = serde_json::from_str(&contents)
                    .with_context(|| format!("failed to parse {}", path.display()))?;
                Ok(parsed)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(self).context("failed to encode sync state")?;
        fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }

    fn record_sync(
        &mut self,
        remote: &str,
        last_event_id: Option<String>,
        last_event_timestamp: Option<String>,
        synced_at: String,
    ) {
        let entry = self
            .remotes
            .entry(remote.to_string())
            .or_insert_with(|| SyncPosition {
                last_event_id: None,
                last_event_timestamp: None,
                last_synced_at: synced_at.clone(),
            });

        if last_event_id.is_some() {
            entry.last_event_id = last_event_id;
        }
        if last_event_timestamp.is_some() {
            entry.last_event_timestamp = last_event_timestamp;
        }
        entry.last_synced_at = synced_at;
    }
}

fn default_sync_state_path() -> Result<PathBuf> {
    let config_dir = dirs::config_dir().context("failed to determine config directory")?;
    Ok(config_dir.join("tt").join("sync.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::commands::ingest::{IngestArgs, IngestCommand, PaneFocusArgs, run_with_base_dir};
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use tempfile::TempDir;
    #[cfg(unix)]
    use tt_db::Database;

    #[test]
    fn load_missing_state_returns_default() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("missing.json");
        let state = SyncState::load(&path).unwrap();
        assert!(state.remotes.is_empty());
    }

    #[test]
    fn sync_state_round_trips() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("sync.json");
        let mut state = SyncState::default();
        state.record_sync(
            "devbox",
            Some("event-1".to_string()),
            Some("2025-01-01T00:00:00Z".to_string()),
            "2025-01-02T00:00:00Z".to_string(),
        );
        state.save(&path).unwrap();

        let loaded = SyncState::load(&path).unwrap();
        let position = loaded.remotes.get("devbox").unwrap();
        assert_eq!(position.last_event_id.as_deref(), Some("event-1"));
        assert_eq!(
            position.last_event_timestamp.as_deref(),
            Some("2025-01-01T00:00:00Z")
        );
        assert_eq!(position.last_synced_at, "2025-01-02T00:00:00Z");
    }

    #[cfg(unix)]
    #[test]
    fn end_to_end_tmux_focus_syncs_into_sqlite() {
        let remote_root = TempDir::new().expect("remote temp dir");
        let remote_base = remote_root.path().join(".time-tracker");
        let ingest_args = IngestArgs {
            command: IngestCommand::PaneFocus(PaneFocusArgs {
                pane: "%1".to_string(),
                cwd: "/tmp/project".to_string(),
                session: "dev".to_string(),
                window_index: Some(1),
            }),
        };
        run_with_base_dir(ingest_args, &remote_base).expect("run tt ingest");

        let events_path = remote_base.join("events.jsonl");
        let events_contents = fs::read_to_string(&events_path).expect("events.jsonl written");
        assert_eq!(events_contents.lines().count(), 1);

        let fake_bin = TempDir::new().expect("fake bin dir");
        let ssh_path = fake_bin.path().join("ssh");
        let script = format!("#!/bin/sh\nset -eu\ncat \"{}\"\n", events_path.display());
        fs::write(&ssh_path, script).expect("write fake ssh");
        let mut perms = fs::metadata(&ssh_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&ssh_path, perms).unwrap();

        let local_dir = TempDir::new().expect("local temp dir");
        let local_db = local_dir.path().join("tt.db");
        let state_path = local_dir.path().join("sync.json");
        let config = Config {
            database_path: local_db.clone(),
            api_key: None,
        };
        let args = SyncArgs {
            remote: "devbox".to_string(),
        };
        run_with_state_path(&args, &config, ssh_path.to_str().unwrap(), &state_path)
            .expect("run tt sync");

        let db = Database::open(&local_db).expect("open local db");
        let events = db.list_events().expect("list events");
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.kind, "tmux_pane_focus");
        assert_eq!(event.source, "remote.tmux");
        assert_eq!(event.cwd.as_deref(), Some("/tmp/project"));
        let data: serde_json::Value = serde_json::from_str(&event.data).unwrap();
        assert_eq!(data["pane_id"], "%1");
        assert_eq!(data["session_name"], "dev");
        assert_eq!(data["window_index"], 1);
        assert_eq!(data["cwd"], "/tmp/project");
    }
}

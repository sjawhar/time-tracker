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
    let mut state = SyncState::load(&state_path)?;

    let mut child = Command::new("ssh")
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
    state.save(&state_path)?;

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
}

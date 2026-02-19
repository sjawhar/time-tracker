//! Machine identity management.
//!
//! Each machine gets a persistent UUID stored in `machine.json`.
//! This UUID is used to namespace event IDs for multi-machine sync.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Machine identity stored in `machine.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineIdentity {
    /// Persistent UUID for this machine.
    pub machine_id: String,
    /// Human-friendly label (e.g., "devbox").
    pub label: String,
}

/// Returns the path to machine.json in the XDG data directory.
pub fn machine_json_path() -> Result<PathBuf> {
    let data_dir = crate::config::dirs_data_path().context("could not determine data directory")?;
    Ok(data_dir.join("machine.json"))
}

/// Loads machine identity from machine.json.
///
/// Returns `None` if the file doesn't exist.
/// Returns an error if the file exists but is unreadable/unparseable.
pub fn load_machine_identity() -> Result<Option<MachineIdentity>> {
    load_from(&machine_json_path()?)
}

/// Loads machine identity from a specific path.
fn load_from(path: &Path) -> Result<Option<MachineIdentity>> {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let identity: MachineIdentity =
                serde_json::from_str(&content).context("failed to parse machine.json")?;
            Ok(Some(identity))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("failed to read machine.json"),
    }
}

/// Loads machine identity, failing with a helpful message if not found.
///
/// Use this in commands that require machine identity (ingest, export).
pub fn require_machine_identity() -> Result<MachineIdentity> {
    load_machine_identity()?.context("No machine identity found. Run 'tt init' first.")
}

/// Initializes machine identity.
///
/// If machine.json already exists, returns the existing identity
/// (updating the label if a new one is provided).
/// If it doesn't exist, generates a new UUID and writes machine.json.
pub fn init_machine(label: Option<&str>) -> Result<MachineIdentity> {
    init_machine_at(&machine_json_path()?, label)
}

/// Initializes machine identity at a specific path.
///
/// `pub(crate)` so tests in other modules (e.g., ingest tests) can use it.
pub(crate) fn init_machine_at(path: &Path, label: Option<&str>) -> Result<MachineIdentity> {
    let default_label = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string());

    let identity = if let Some(mut existing) = load_from(path)? {
        if let Some(new_label) = label {
            existing.label = new_label.to_string();
            save_to(path, &existing)?;
        }
        existing
    } else {
        let identity = MachineIdentity {
            machine_id: Uuid::new_v4().to_string(),
            label: label.unwrap_or(&default_label).to_string(),
        };
        save_to(path, &identity)?;
        identity
    };

    Ok(identity)
}

/// Writes machine identity to a specific path.
fn save_to(path: &Path, identity: &MachineIdentity) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("failed to create data directory")?;
    }
    let json = serde_json::to_string_pretty(identity).context("failed to serialize identity")?;
    std::fs::write(path, json).context("failed to write machine.json")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_creates_new_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");

        let identity = init_machine_at(&path, Some("testbox")).unwrap();
        assert_eq!(identity.label, "testbox");
        assert!(!identity.machine_id.is_empty());
        Uuid::parse_str(&identity.machine_id).unwrap();
    }

    #[test]
    fn test_init_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");

        let first = init_machine_at(&path, Some("testbox")).unwrap();
        let second = init_machine_at(&path, None).unwrap();
        assert_eq!(first.machine_id, second.machine_id);
        assert_eq!(first.label, second.label);
    }

    #[test]
    fn test_init_updates_label() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");

        let first = init_machine_at(&path, Some("old-name")).unwrap();
        let second = init_machine_at(&path, Some("new-name")).unwrap();
        assert_eq!(first.machine_id, second.machine_id);
        assert_eq!(second.label, "new-name");
    }

    #[test]
    fn test_load_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");
        assert!(load_from(&path).unwrap().is_none());
    }

    #[test]
    fn test_load_existing_returns_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");

        init_machine_at(&path, Some("testbox")).unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert_eq!(loaded.label, "testbox");
    }
}

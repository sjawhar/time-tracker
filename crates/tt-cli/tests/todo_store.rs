use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tt_cli::Config;
use tt_cli::todo_store::{preflight_sync_conflicts, store_dir};

#[test]
fn store_dir_returns_configured_todo_store_path() {
    let config = Config {
        database_path: PathBuf::from("/tmp/tt.db"),
        todo_store_path: PathBuf::from("/tmp/todos"),
    };

    assert_eq!(store_dir(&config), Path::new("/tmp/todos"));
}

#[test]
fn preflight_sync_conflicts_succeeds_when_store_dir_is_missing() {
    let temp_dir = TempDir::new().unwrap();
    let missing_store = temp_dir.path().join("missing-store");

    let result = preflight_sync_conflicts(&missing_store);

    assert!(result.is_ok());
    assert!(!missing_store.exists());
}

#[test]
fn preflight_sync_conflicts_succeeds_when_store_dir_has_no_conflicts() {
    let temp_dir = TempDir::new().unwrap();

    let result = preflight_sync_conflicts(temp_dir.path());

    assert!(result.is_ok());
}

#[test]
fn preflight_sync_conflicts_errors_with_conflict_filename_when_conflict_exists() {
    let temp_dir = TempDir::new().unwrap();
    let conflict_path = temp_dir.path().join("todos.sync-conflict-20260623.md");
    fs::write(&conflict_path, "conflicted").unwrap();

    let error = preflight_sync_conflicts(temp_dir.path()).unwrap_err();
    let message = error.to_string();

    assert!(message.contains("todos.sync-conflict-20260623.md"));
}

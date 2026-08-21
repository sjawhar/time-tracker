use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

mod common;
use common::CommandExt;

fn tt_binary() -> String {
    env!("CARGO_BIN_EXE_tt").to_string()
}

fn run_tt(store: &Path, args: &[&str]) -> std::process::Output {
    Command::new(tt_binary())
        .sandboxed_home(store.parent().unwrap())
        .env("TT_TODO_STORE_PATH", store)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn todo_next_rejects_sync_conflict_files() {
    // Given: a todo store containing a Syncthing conflict file.
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("todos.sync-conflict-20260623.md"), "conflict").unwrap();

    // When: the read-only next command is run against that store.
    let output = run_tt(&store, &["todo", "next"]);

    // Then: the command fails before parsing and lists the conflict path.
    assert!(
        !output.status.success(),
        "todo next should reject conflicts"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("todos.sync-conflict-20260623.md"),
        "stderr should list conflict file: {stderr}"
    );
}

#[test]
fn todo_ls_keeps_idless_todo_file_byte_identical() {
    // Given: a todo store with a human-authored todo missing the id metadata field.
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    std::fs::create_dir_all(&store).unwrap();
    let todos_path = store.join("todos.md");
    let original = "- [ ] Human note <!-- tt-todo:{\"priority\":[\"ipi\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":true} -->\n";
    std::fs::write(&todos_path, original).unwrap();

    // When: the read-only list command renders the store.
    let output = run_tt(&store, &["todo", "ls"]);

    // Then: the item is surfaced with a diagnostic and the file bytes are unchanged.
    assert!(
        output.status.success(),
        "todo ls should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Human note"),
        "stdout should show todo: {stdout}"
    );
    assert!(
        stdout.contains("line 1"),
        "stdout should show diagnostic: {stdout}"
    );
    let after = std::fs::read_to_string(&todos_path).unwrap();
    assert_eq!(after, original, "todo ls must not rewrite missing ids");
}

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

fn tt_binary() -> String {
    env!("CARGO_BIN_EXE_tt").to_string()
}

fn run_tt(store: &Path, args: &[&str]) -> std::process::Output {
    Command::new(tt_binary())
        .env("TT_TODO_STORE_PATH", store)
        .args(args)
        .output()
        .unwrap()
}

fn assert_success(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "command should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn seed(store: &Path) {
    std::fs::create_dir_all(store).unwrap();
    std::fs::write(
        store.join("priorities.md"),
        "- [ ] Diversification <!-- tt-priority:{\"slug\":\"diversification\",\"value\":4,\"status\":\"active\"} -->\n- [ ] IPI launch <!-- tt-priority:{\"slug\":\"ipi\",\"value\":9,\"status\":\"active\"} -->\n",
    )
    .unwrap();
    std::fs::write(
        store.join("todos.md"),
        "- [ ] Ping Peter <!-- tt-todo:{\"id\":\"td_pingpeter1\",\"priority\":[\"diversification\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n- [ ] Unrelated <!-- tt-todo:{\"id\":\"td_unrelated1\",\"priority\":[\"ipi\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n",
    )
    .unwrap();
    std::fs::write(
        store.join("streams.md"),
        "- Sales stream <!-- tt-stream:{\"priority\":\"diversification\"} -->\n",
    )
    .unwrap();
}

fn read_all(store: &Path) -> (String, String, String) {
    (
        std::fs::read_to_string(store.join("priorities.md")).unwrap(),
        std::fs::read_to_string(store.join("todos.md")).unwrap(),
        std::fs::read_to_string(store.join("streams.md")).unwrap(),
    )
}

#[test]
fn rename_rewrites_priorities_todos_and_streams() {
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    seed(&store);

    assert_success(&run_tt(
        &store,
        &["priority", "rename", "diversification", "sales"],
    ));

    let priorities = std::fs::read_to_string(store.join("priorities.md")).unwrap();
    assert!(
        priorities.contains("\"slug\":\"sales\""),
        "priorities:\n{priorities}"
    );
    assert!(
        !priorities.contains("diversification"),
        "old slug remains:\n{priorities}"
    );

    let todos = std::fs::read_to_string(store.join("todos.md")).unwrap();
    assert!(
        todos.contains("\"priority\":[\"sales\"]"),
        "todos:\n{todos}"
    );
    assert!(
        todos.contains("\"priority\":[\"ipi\"]"),
        "unrelated todo changed:\n{todos}"
    );
    assert!(
        !todos.contains("diversification"),
        "old slug remains in todos:\n{todos}"
    );

    let streams = std::fs::read_to_string(store.join("streams.md")).unwrap();
    assert!(
        streams.contains("\"priority\":\"sales\""),
        "streams:\n{streams}"
    );
}

#[test]
fn rename_to_existing_slug_fails_and_leaves_files_unchanged() {
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    seed(&store);
    let before = read_all(&store);

    let output = run_tt(&store, &["priority", "rename", "diversification", "ipi"]);

    assert!(
        !output.status.success(),
        "rename onto existing slug should fail"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("already exists"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        read_all(&store),
        before,
        "all three files must be byte-unchanged"
    );
}

#[test]
fn rename_absent_old_slug_fails_and_leaves_files_unchanged() {
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    seed(&store);
    let before = read_all(&store);

    let output = run_tt(&store, &["priority", "rename", "ghost", "sales"]);

    assert!(
        !output.status.success(),
        "renaming an absent slug should fail"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not found"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        read_all(&store),
        before,
        "all three files must be byte-unchanged"
    );
}

#[test]
fn rename_same_slug_fails() {
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    seed(&store);
    let before = read_all(&store);

    let output = run_tt(
        &store,
        &["priority", "rename", "diversification", "diversification"],
    );

    assert!(
        !output.status.success(),
        "renaming a slug to itself should fail"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("identical"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        read_all(&store),
        before,
        "all three files must be byte-unchanged"
    );
}

#[test]
fn rename_refuses_when_sync_conflict_file_present() {
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    seed(&store);
    let before = read_all(&store);
    std::fs::write(
        store.join("todos.sync-conflict-20260623-000000.md"),
        "conflicted copy",
    )
    .unwrap();

    let output = run_tt(&store, &["priority", "rename", "diversification", "sales"]);

    assert!(
        !output.status.success(),
        "rename must refuse when a sync-conflict file exists"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("sync-conflict"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        read_all(&store),
        before,
        "the three store files must be byte-unchanged"
    );
}

#[test]
fn rename_preserves_existing_description() {
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(
        store.join("priorities.md"),
        "- [ ] old <!-- tt-priority:{\"slug\":\"old\",\"value\":4,\"status\":\"active\",\"description\":\"keep me\"} -->\n",
    )
    .unwrap();

    assert_success(&run_tt(&store, &["priority", "rename", "old", "new"]));

    let priorities = std::fs::read_to_string(store.join("priorities.md")).unwrap();
    assert_eq!(
        priorities,
        "- [ ] new <!-- tt-priority:{\"slug\":\"new\",\"value\":4,\"status\":\"active\",\"description\":\"keep me\"} -->\n"
    );
}

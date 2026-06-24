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

#[test]
fn block_then_unblock_round_trips_to_byte_identical() {
    // Given: a store with priorities and one unblocked todo (no `block` field on disk).
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(
        store.join("priorities.md"),
        "- [ ] IPI launch <!-- tt-priority:{\"slug\":\"ipi\",\"value\":9,\"status\":\"active\"} -->\n",
    )
    .unwrap();
    let todos_path = store.join("todos.md");
    let original = "- [ ] Launch task <!-- tt-todo:{\"id\":\"td_launch0001\",\"priority\":[\"ipi\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n";
    std::fs::write(&todos_path, original).unwrap();

    // When: the todo is blocked.
    assert_success(&run_tt(
        &store,
        &["todo", "block", "td_launch0001", "waiting on Peter"],
    ));

    // Then: the block reason is persisted, and `next` shows a Blocked section (not Main).
    let blocked = std::fs::read_to_string(&todos_path).unwrap();
    assert!(
        blocked.contains("\"block\":\"waiting on Peter\""),
        "block reason not written:\n{blocked}"
    );
    let next = run_tt(&store, &["todo", "next"]);
    assert_success(&next);
    let next_out = String::from_utf8_lossy(&next.stdout);
    assert!(
        next_out.contains("Blocked"),
        "next missing Blocked:\n{next_out}"
    );

    // And `ls` surfaces the reason.
    let ls = run_tt(&store, &["todo", "ls"]);
    assert_success(&ls);
    assert!(
        String::from_utf8_lossy(&ls.stdout).contains("blocked:\"waiting on Peter\""),
        "ls missing block reason"
    );

    // When: the todo is unblocked.
    assert_success(&run_tt(&store, &["todo", "unblock", "td_launch0001"]));

    // Then: the file is byte-identical to the original (block field fully removed).
    assert_eq!(std::fs::read_to_string(&todos_path).unwrap(), original);
}

#[test]
fn block_empty_reason_fails_and_leaves_file_unchanged() {
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    std::fs::create_dir_all(&store).unwrap();
    let todos_path = store.join("todos.md");
    let original = "- [ ] Task <!-- tt-todo:{\"id\":\"td_task00001\",\"priority\":[],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n";
    std::fs::write(&todos_path, original).unwrap();

    let output = run_tt(&store, &["todo", "block", "td_task00001", "   "]);

    assert!(!output.status.success(), "empty reason should fail");
    assert_eq!(std::fs::read_to_string(&todos_path).unwrap(), original);
}

#[test]
fn block_trims_whitespace_reason() {
    // Given: a store with one open todo.
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    std::fs::create_dir_all(&store).unwrap();
    let todos_path = store.join("todos.md");
    std::fs::write(
        &todos_path,
        "- [ ] Task <!-- tt-todo:{\"id\":\"td_task00001\",\"priority\":[],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n",
    )
    .unwrap();

    // When: blocked with a padded reason.
    assert_success(&run_tt(
        &store,
        &["todo", "block", "td_task00001", "  waiting  "],
    ));

    // Then: the stored reason is trimmed.
    let after = std::fs::read_to_string(&todos_path).unwrap();
    assert!(
        after.contains("\"block\":\"waiting\""),
        "reason not trimmed:\n{after}"
    );
}

#[test]
fn block_done_todo_fails_and_leaves_file_byte_identical() {
    // Given: a store with a DONE todo.
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    std::fs::create_dir_all(&store).unwrap();
    let todos_path = store.join("todos.md");
    let original = "- [x] Finished <!-- tt-todo:{\"id\":\"td_done00001\",\"priority\":[],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n";
    std::fs::write(&todos_path, original).unwrap();

    // When: an attempt is made to block the done todo.
    let output = run_tt(&store, &["todo", "block", "td_done00001", "waiting"]);

    // Then: it fails and the file is byte-identical.
    assert!(!output.status.success(), "blocking a done todo should fail");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("done"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read_to_string(&todos_path).unwrap(), original);
}

#[test]
fn re_blocking_replaces_the_reason() {
    // Given: a store with one open todo.
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    std::fs::create_dir_all(&store).unwrap();
    let todos_path = store.join("todos.md");
    std::fs::write(
        &todos_path,
        "- [ ] Task <!-- tt-todo:{\"id\":\"td_task00001\",\"priority\":[],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n",
    )
    .unwrap();

    // When: blocked twice with different reasons.
    assert_success(&run_tt(
        &store,
        &["todo", "block", "td_task00001", "first reason"],
    ));
    assert_success(&run_tt(
        &store,
        &["todo", "block", "td_task00001", "second reason"],
    ));

    // Then: only the latest reason remains.
    let after = std::fs::read_to_string(&todos_path).unwrap();
    assert!(
        after.contains("\"block\":\"second reason\""),
        "latest reason missing:\n{after}"
    );
    assert!(
        !after.contains("first reason"),
        "stale reason remains:\n{after}"
    );
}

#[test]
fn unblock_already_unblocked_is_byte_identical_no_op() {
    // Given: a store with an unblocked todo (no `block` field on disk).
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    std::fs::create_dir_all(&store).unwrap();
    let todos_path = store.join("todos.md");
    let original = "- [ ] Task <!-- tt-todo:{\"id\":\"td_task00001\",\"priority\":[],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n";
    std::fs::write(&todos_path, original).unwrap();

    // When: unblock runs on a todo that is already unblocked.
    assert_success(&run_tt(&store, &["todo", "unblock", "td_task00001"]));

    // Then: the file is byte-identical.
    assert_eq!(std::fs::read_to_string(&todos_path).unwrap(), original);
}

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

fn todo_id_from_line(line: &str) -> String {
    let marker = "\"id\":\"";
    let start = line.find(marker).unwrap() + marker.len();
    let rest = &line[start..];
    let end = rest.find('"').unwrap();
    rest[..end].to_string()
}

fn assert_crockford_todo_id(id: &str) {
    assert!(id.starts_with("td_"), "id should have td_ prefix: {id}");
    let suffix = &id[3..];
    assert_eq!(suffix.len(), 10, "id suffix should be 10 chars: {id}");
    assert!(
        suffix
            .chars()
            .all(|ch| "0123456789abcdefghjkmnpqrstvwxyz".contains(ch)),
        "id should use lowercase Crockford base32 alphabet: {id}"
    );
}

#[test]
fn todo_and_priority_mutations_update_only_target_records() {
    // Given: a temp markdown store with two priorities and one lower-ranked todo.
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    assert_success(&run_tt(
        &store,
        &["priority", "add", "chores", "--value", "1"],
    ));
    assert_success(&run_tt(&store, &["priority", "add", "ipi", "--value", "9"]));
    std::fs::write(
        store.join("todos.md"),
        "- [ ] Low task <!-- tt-todo:{\"id\":\"td_lowtask01\",\"priority\":[\"chores\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n",
    )
    .unwrap();

    // When: a higher-ranked todo is added.
    assert_success(&run_tt(
        &store,
        &[
            "todo",
            "add",
            "Important task",
            "--priority",
            "ipi",
            "--quick",
        ],
    ));

    // Then: the new todo has a Crockford id and is inserted above the lower-ranked item.
    let todos = std::fs::read_to_string(store.join("todos.md")).unwrap();
    let lines = todos.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("Important task"), "todos:\n{todos}");
    assert!(lines[1].contains("Low task"), "todos:\n{todos}");
    let new_id = todo_id_from_line(lines[0]);
    assert_crockford_todo_id(&new_id);

    // When: done, defer, rank, and priority value mutations are applied.
    assert_success(&run_tt(&store, &["todo", "done", &new_id]));
    assert_success(&run_tt(&store, &["todo", "defer", &new_id, "2026-06-30"]));
    assert_success(&run_tt(&store, &["todo", "rank", "td_lowtask01", "--top"]));
    assert_success(&run_tt(&store, &["priority", "value", "ipi", "7"]));

    // Then: each target record changed and unrelated records remain present.
    let todos = std::fs::read_to_string(store.join("todos.md")).unwrap();
    let lines = todos.lines().collect::<Vec<_>>();
    assert!(
        lines[0].contains("Low task"),
        "rank should move low task to top:\n{todos}"
    );
    assert!(
        lines[1].starts_with("- [x] Important task"),
        "done should check item:\n{todos}"
    );
    assert!(
        lines[1].contains("\"when\":\"2026-06-30\""),
        "defer should set when:\n{todos}"
    );
    let priorities = std::fs::read_to_string(store.join("priorities.md")).unwrap();
    assert!(
        priorities.contains("\"slug\":\"ipi\",\"value\":7,\"status\":\"active\""),
        "priority value should update:\n{priorities}"
    );
}

#[test]
fn mutation_failure_leaves_todo_file_byte_identical() {
    // Given: a store with one todo.
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    std::fs::create_dir_all(&store).unwrap();
    let todos_path = store.join("todos.md");
    let original = "- [ ] Existing <!-- tt-todo:{\"id\":\"td_existing1\",\"priority\":[],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n";
    std::fs::write(&todos_path, original).unwrap();

    // When: a missing id is marked done.
    let output = run_tt(&store, &["todo", "done", "td_missing1"]);

    // Then: the command fails and the file is byte-identical.
    assert!(!output.status.success(), "missing id should fail");
    let after = std::fs::read_to_string(&todos_path).unwrap();
    assert_eq!(after, original);
}

#[test]
fn mutations_preserve_raw_lines_and_read_only_ls_does_not_add_ids() {
    // Given: a store containing raw section headers, malformed lines, and an id-less todo.
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(
        store.join("priorities.md"),
        "- [ ] ipi <!-- tt-priority:{\"slug\":\"ipi\",\"value\":9,\"status\":\"active\"} -->\n",
    )
    .unwrap();
    let todos_path = store.join("todos.md");
    let original = "## Later\n\nthis is raw and malformed\n- [ ] Needs id <!-- tt-todo:{\"priority\":[\"ipi\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n";
    std::fs::write(&todos_path, original).unwrap();

    // When: read-only list runs, then normalize-ids explicitly mutates id-less todos.
    assert_success(&run_tt(&store, &["todo", "ls"]));
    assert_eq!(std::fs::read_to_string(&todos_path).unwrap(), original);
    assert_success(&run_tt(&store, &["todo", "normalize-ids"]));

    // Then: raw lines and headers survive byte-for-byte, and only normalize-ids adds an id.
    let after = std::fs::read_to_string(&todos_path).unwrap();
    assert!(after.contains("## Later\n\nthis is raw and malformed\n"));
    assert!(after.contains("Needs id"));
    assert!(
        after.contains("\"id\":\"td_"),
        "normalize-ids should mint id:\n{after}"
    );
}

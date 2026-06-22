use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use tt_core::todos::{parse_priorities, parse_todos};

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

fn assert_todos_parse_clean(contents: &str) {
    let (_, diagnostics) = parse_todos(contents);
    assert!(
        diagnostics.is_empty(),
        "todo diagnostics: {diagnostics:?}\n{contents}"
    );
}

fn assert_priorities_parse_clean(contents: &str) {
    let (_, diagnostics) = parse_priorities(contents);
    assert!(
        diagnostics.is_empty(),
        "priority diagnostics: {diagnostics:?}\n{contents}"
    );
}

#[test]
fn mutations_do_not_fuse_newline_less_store_lines() {
    // Given: a newline-less raw header, todo, and priority store.
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(
        store.join("priorities.md"),
        "- [ ] IPI launch <!-- tt-priority:{\"slug\":\"ipi\",\"value\":9,\"status\":\"active\"} -->",
    )
    .unwrap();
    let todos_path = store.join("todos.md");
    std::fs::write(&todos_path, "## Later").unwrap();

    // When: todo add appends after a newline-less raw header.
    assert_success(&run_tt(
        &store,
        &["todo", "add", "First task", "--priority", "ipi"],
    ));

    // Then: the raw header remains a separate line and the file parses cleanly.
    let after_add = std::fs::read_to_string(&todos_path).unwrap();
    assert_eq!(
        after_add.lines().next().unwrap(),
        "## Later",
        "todo add should not fuse the header:\n{after_add}"
    );
    assert_todos_parse_clean(&after_add);

    // Given: a newline-less two-todo file.
    let first_line = after_add.lines().nth(1).unwrap().to_string();
    let first_id = todo_id_from_line(&first_line);
    let second_line = "- [ ] Second task <!-- tt-todo:{\"id\":\"td_second001\",\"priority\":[],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->";
    std::fs::write(&todos_path, format!("{first_line}\n{second_line}")).unwrap();

    // When: rank moves the newline-less final todo above the first todo.
    assert_success(&run_tt(
        &store,
        &["todo", "rank", "td_second001", "--above", &first_id],
    ));

    // Then: both todo records remain separate and parseable.
    let after_rank = std::fs::read_to_string(&todos_path).unwrap();
    assert_eq!(
        after_rank.lines().collect::<Vec<_>>(),
        vec![second_line, first_line.as_str()],
        "rank should not fuse newline-less moved line:\n{after_rank}"
    );
    assert_todos_parse_clean(&after_rank);

    // When: priority add appends after a newline-less priority.
    assert_success(&run_tt(
        &store,
        &["priority", "add", "Admin", "--value", "1"],
    ));

    // Then: both priority records remain separate and parseable.
    let priorities = std::fs::read_to_string(store.join("priorities.md")).unwrap();
    assert_eq!(
        priorities.lines().count(),
        2,
        "priority add fused lines:\n{priorities}"
    );
    assert_priorities_parse_clean(&priorities);
}

#[test]
fn todo_add_respects_pinned_absolute_index() {
    // Given: a pinned todo between two non-pinned todos with different ranks.
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(
        store.join("priorities.md"),
        "- [ ] High <!-- tt-priority:{\"slug\":\"high\",\"value\":9,\"status\":\"active\"} -->\n- [ ] Mid <!-- tt-priority:{\"slug\":\"mid\",\"value\":5,\"status\":\"active\"} -->\n- [ ] Low <!-- tt-priority:{\"slug\":\"low\",\"value\":1,\"status\":\"active\"} -->\n",
    )
    .unwrap();
    let todos_path = store.join("todos.md");
    std::fs::write(
        &todos_path,
        "- [ ] High task <!-- tt-todo:{\"id\":\"td_high00001\",\"priority\":[\"high\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n- [ ] Pinned task <!-- tt-todo:{\"id\":\"td_pinned001\",\"priority\":[\"low\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":true,\"quick\":false} -->\n- [ ] Low task <!-- tt-todo:{\"id\":\"td_low000001\",\"priority\":[\"low\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n",
    )
    .unwrap();

    // When: a mid-ranked todo is added.
    assert_success(&run_tt(
        &store,
        &["todo", "add", "Mid task", "--priority", "mid"],
    ));

    // Then: it is inserted in non-pinned rank order while pinned stays at index 1.
    let todos = std::fs::read_to_string(&todos_path).unwrap();
    let lines = todos.lines().collect::<Vec<_>>();
    assert!(lines[0].contains("High task"), "todos:\n{todos}");
    assert!(lines[1].contains("Pinned task"), "todos:\n{todos}");
    assert!(lines[2].contains("Mid task"), "todos:\n{todos}");
    assert!(lines[3].contains("Low task"), "todos:\n{todos}");
}

#[test]
fn ambiguous_todo_id_failure_leaves_file_byte_identical() {
    // Given: a hand-edited store containing duplicate todo ids.
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    std::fs::create_dir_all(&store).unwrap();
    let todos_path = store.join("todos.md");
    let original = "- [ ] First <!-- tt-todo:{\"id\":\"td_dupe00001\",\"priority\":[],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n- [ ] Second <!-- tt-todo:{\"id\":\"td_dupe00001\",\"priority\":[],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n";
    std::fs::write(&todos_path, original).unwrap();

    // When: a mutation targets the duplicate id.
    let output = run_tt(&store, &["todo", "done", "td_dupe00001"]);

    // Then: the command reports ambiguity and leaves bytes unchanged.
    assert!(!output.status.success(), "duplicate id should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ambiguous"),
        "stderr should mention ambiguity: {stderr}"
    );
    assert_eq!(std::fs::read_to_string(&todos_path).unwrap(), original);
}

#[test]
fn priority_add_accepts_explicit_slug() {
    // Given: an empty store.
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");

    // When: a priority is added with an explicit slug.
    assert_success(&run_tt(
        &store,
        &[
            "priority",
            "add",
            "IPI launch",
            "--slug",
            "ipi",
            "--value",
            "9",
        ],
    ));

    // Then: the explicit slug is written instead of the derived title slug.
    let priorities = std::fs::read_to_string(store.join("priorities.md")).unwrap();
    assert_eq!(
        priorities,
        "- [ ] IPI launch <!-- tt-priority:{\"slug\":\"ipi\",\"value\":9,\"status\":\"active\"} -->\n"
    );
}

#[test]
fn invalid_todo_add_date_does_not_create_store_directory() {
    // Given: a missing store directory.
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");

    // When: todo add receives an invalid date argument.
    let output = run_tt(&store, &["todo", "add", "Bad date", "--when", "not-a-date"]);

    // Then: it fails before creating the store directory.
    assert!(!output.status.success(), "invalid date should fail");
    assert!(!store.exists(), "invalid input must not create store dir");
}

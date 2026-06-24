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
        "- [ ] ipi <!-- tt-priority:{\"slug\":\"ipi\",\"value\":9,\"status\":\"active\"} -->\n",
    )
    .unwrap();
}

#[test]
fn describe_sets_trimmed_then_clears_with_empty_and_whitespace() {
    // Given: a priority without a description.
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    seed(&store);

    // When: the description is set with surrounding whitespace.
    assert_success(&run_tt(
        &store,
        &["priority", "describe", "ipi", "  IPI: contract resume  "],
    ));
    let after_set = std::fs::read_to_string(store.join("priorities.md")).unwrap();

    // Then: the description is trimmed before storage.
    assert_eq!(
        after_set,
        "- [ ] ipi <!-- tt-priority:{\"slug\":\"ipi\",\"value\":9,\"status\":\"active\",\"description\":\"IPI: contract resume\"} -->\n"
    );

    // When: the description is cleared with whitespace-only text.
    assert_success(&run_tt(&store, &["priority", "describe", "ipi", "   "]));
    let after_ws_clear = std::fs::read_to_string(store.join("priorities.md")).unwrap();

    // Then: the `description` key is omitted again.
    assert_eq!(
        after_ws_clear,
        "- [ ] ipi <!-- tt-priority:{\"slug\":\"ipi\",\"value\":9,\"status\":\"active\"} -->\n"
    );

    // When: the description is set again and cleared with an empty string.
    assert_success(&run_tt(&store, &["priority", "describe", "ipi", "again"]));
    assert_success(&run_tt(&store, &["priority", "describe", "ipi", ""]));
    let after_empty_clear = std::fs::read_to_string(store.join("priorities.md")).unwrap();

    // Then: the description remains absent.
    assert_eq!(
        after_empty_clear,
        "- [ ] ipi <!-- tt-priority:{\"slug\":\"ipi\",\"value\":9,\"status\":\"active\"} -->\n"
    );
}

#[test]
fn describe_absent_slug_fails_and_leaves_file_unchanged() {
    // Given: a priority file with one known slug.
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    seed(&store);
    let before = std::fs::read_to_string(store.join("priorities.md")).unwrap();

    // When: a missing priority is described.
    let output = run_tt(&store, &["priority", "describe", "ghost", "nope"]);

    // Then: the command fails and the priority file is byte-unchanged.
    assert!(
        !output.status.success(),
        "describing an absent slug should fail"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not found"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(store.join("priorities.md")).unwrap(),
        before,
        "file must be byte-unchanged on failure"
    );
}

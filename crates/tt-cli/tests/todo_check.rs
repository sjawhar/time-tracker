use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;

const PRIORITIES: &str = "- [ ] High <!-- tt-priority:{\"slug\":\"high\",\"value\":9,\"status\":\"active\"} -->\n- [ ] Low <!-- tt-priority:{\"slug\":\"low\",\"value\":1,\"status\":\"active\"} -->\n- [ ] Old <!-- tt-priority:{\"slug\":\"old\",\"value\":5,\"status\":\"done\"} -->\n";
const TODOS: &str = "- [ ] Low first <!-- tt-todo:{\"id\":\"td_low000001\",\"priority\":[\"low\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n- [ ] High second <!-- tt-todo:{\"id\":\"td_high00001\",\"priority\":[\"high\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n- [ ] Orphaned <!-- tt-todo:{\"id\":\"td_orphan01\",\"priority\":[\"old\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":false,\"quick\":false} -->\n- [ ] Pinned high <!-- tt-todo:{\"id\":\"td_pinned01\",\"priority\":[\"high\"],\"stream\":null,\"when\":null,\"due\":null,\"pin\":true,\"quick\":false} -->\n";

fn tt_binary() -> String {
    env!("CARGO_BIN_EXE_tt").to_string()
}

fn write_config(temp: &TempDir) -> (PathBuf, PathBuf) {
    let store = temp.path().join("todo-store");
    let config = temp.path().join("config.toml");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(
        &config,
        format!(
            "database_path = \"{}\"\ntodo_store_path = \"{}\"\n",
            temp.path().join("tt.db").display(),
            store.display()
        ),
    )
    .unwrap();
    (config, store)
}

#[test]
fn todo_check_reports_ordering_orphans_and_deliberate_pins() {
    let temp = TempDir::new().unwrap();
    let (config, store) = write_config(&temp);
    std::fs::write(store.join("priorities.md"), PRIORITIES).unwrap();
    std::fs::write(store.join("todos.md"), TODOS).unwrap();

    let output = Command::new(tt_binary())
        .arg("--config")
        .arg(&config)
        .args(["todo", "check"])
        .output()
        .unwrap();

    assert!(output.status.success(), "todo check should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "misordered",
        "td_high00001",
        "orphaned",
        "td_orphan01",
        "pinned",
        "td_pinned01",
    ] {
        assert!(stdout.contains(expected), "missing {expected}: {stdout}");
    }
    assert!(!stdout.contains("misordered: td_pinned01"));
}

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use tempfile::TempDir;
use tt_db::{Database, Stream};

fn tt_binary() -> String {
    env!("CARGO_BIN_EXE_tt").to_string()
}

fn write_config(temp: &TempDir) -> (PathBuf, PathBuf, PathBuf) {
    let db = temp.path().join("tt.db");
    let store = temp.path().join("todo-store");
    let config = temp.path().join("config.toml");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(
        &config,
        format!(
            "database_path = \"{}\"\ntodo_store_path = \"{}\"\n",
            db.display(),
            store.display()
        ),
    )
    .unwrap();
    (config, db, store)
}

fn write_alpha_priority(store: &Path) {
    std::fs::write(
        store.join("priorities.md"),
        "- [ ] Alpha <!-- tt-priority:{\"slug\":\"alpha\",\"value\":1,\"status\":\"active\"} -->\n",
    )
    .unwrap();
}

#[test]
fn todo_drift_errors_on_unresolved_stream_link() {
    let temp = TempDir::new().unwrap();
    let (config, db_path, store) = write_config(&temp);
    write_alpha_priority(&store);
    std::fs::write(
        store.join("streams.md"),
        "- Missing Stream <!-- tt-stream:{\"priority\":\"alpha\"} -->\n",
    )
    .unwrap();
    let _db = Database::open(&db_path).unwrap();

    let output = Command::new(tt_binary())
        .arg("--config")
        .arg(&config)
        .args(["todo", "drift", "--json"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown stream"));
}

#[test]
fn todo_drift_reports_existing_idle_linked_stream_as_zero_percent() {
    let temp = TempDir::new().unwrap();
    let (config, db_path, store) = write_config(&temp);
    write_alpha_priority(&store);
    std::fs::write(
        store.join("streams.md"),
        "- Idle Stream <!-- tt-stream:{\"priority\":\"alpha\"} -->\n",
    )
    .unwrap();
    insert_stream(&Database::open(&db_path).unwrap());

    let json = run_drift_json(&config);

    assert_eq!(json["priorities"][0]["priority_slug"], json!("alpha"));
    assert_eq!(json["priorities"][0]["direct_share"], json!(0.0));
    assert_eq!(
        json["priorities"][0]["direct_plus_delegated_share"],
        json!(0.0)
    );
    assert_eq!(json["priorities"][0]["direct_ms"], json!(0));
    assert_eq!(json["priorities"][0]["direct_plus_delegated_ms"], json!(0));
}

fn run_drift_json(config: &PathBuf) -> Value {
    let output = Command::new(tt_binary())
        .arg("--config")
        .arg(config)
        .args(["todo", "drift", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "todo drift should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn insert_stream(db: &Database) {
    let now = chrono::Utc::now();
    db.insert_stream(&Stream {
        id: "idle".to_string(),
        name: Some("Idle Stream".to_string()),
        created_at: now,
        updated_at: now,
        time_direct_ms: 0,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: false,
    })
    .unwrap();
}

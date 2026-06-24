use std::path::Path;
use std::process::{Command, Output};

use chrono::Utc;
use tempfile::TempDir;
use tt_core::todos::{StreamFileItem, parse_streams};
use tt_db::{Database, Stream};

fn tt_binary() -> String {
    env!("CARGO_BIN_EXE_tt").to_string()
}

fn write_config(config_path: &Path, db_path: &Path) {
    std::fs::write(
        config_path,
        format!(r#"database_path = "{}""#, db_path.display()),
    )
    .unwrap();
}

fn insert_stream(db_path: &Path, id: &str, name: Option<&str>) {
    let db = Database::open(db_path).unwrap();
    let now = Utc::now();
    db.insert_stream(&Stream {
        id: id.to_string(),
        name: name.map(String::from),
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

fn run_tt(config_path: &Path, store: &Path, args: &[&str]) -> Output {
    Command::new(tt_binary())
        .arg("--config")
        .arg(config_path)
        .env("TT_TODO_STORE_PATH", store)
        .args(args)
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_failure_contains(output: &Output, expected: &str) {
    assert!(!output.status.success(), "command should fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(expected),
        "stderr should contain {expected:?}: {stderr}"
    );
}

fn priority_line(slug: &str) -> String {
    format!(
        "- [ ] {slug} <!-- tt-priority:{{\"slug\":\"{slug}\",\"value\":9,\"status\":\"active\"}} -->\n"
    )
}

#[test]
fn streams_link_writes_exact_stream_name_and_priority_slug() {
    // Given: one named DB stream and a matching priority in the markdown store.
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    let config_path = temp.path().join("config.toml");
    let db_path = temp.path().join("tt.db");
    write_config(&config_path, &db_path);
    insert_stream(&db_path, "stream-1", Some("Fable 5 DPI"));
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("priorities.md"), priority_line("ipi")).unwrap();

    // When: the stream is linked by exact display name.
    let output = run_tt(
        &config_path,
        &store,
        &["streams", "link", "Fable 5 DPI", "ipi"],
    );

    // Then: streams.md contains the exact display name and reparses cleanly.
    assert_success(&output);
    let streams = std::fs::read_to_string(store.join("streams.md")).unwrap();
    assert!(streams.contains("- Fable 5 DPI"), "streams.md:\n{streams}");
    assert!(
        streams.contains("\"priority\":\"ipi\""),
        "streams.md:\n{streams}"
    );
    let (parsed, diagnostics) = parse_streams(&streams);
    assert!(
        diagnostics.is_empty(),
        "stream diagnostics: {diagnostics:?}"
    );
    assert!(
        parsed.items.iter().any(|line| matches!(
            &line.item,
            StreamFileItem::Link(link) if link.stream == "Fable 5 DPI" && link.priority == "ipi"
        )),
        "parsed streams: {parsed:?}"
    );
}

#[test]
fn streams_link_rejects_duplicate_stream_names_without_changing_file() {
    // Given: two DB streams share the same name and streams.md already has unrelated content.
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    let config_path = temp.path().join("config.toml");
    let db_path = temp.path().join("tt.db");
    write_config(&config_path, &db_path);
    insert_stream(&db_path, "stream-1", Some("Shared"));
    insert_stream(&db_path, "stream-2", Some("Shared"));
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("priorities.md"), priority_line("ipi")).unwrap();
    let streams_path = store.join("streams.md");
    let original = "# stream links\n";
    std::fs::write(&streams_path, original).unwrap();

    // When: the ambiguous name is linked.
    let output = run_tt(&config_path, &store, &["streams", "link", "Shared", "ipi"]);

    // Then: the command fails and streams.md is byte-identical.
    assert_failure_contains(&output, "'Shared' is ambiguous: 2 streams share that name");
    assert_eq!(std::fs::read_to_string(&streams_path).unwrap(), original);
}

#[test]
fn streams_link_rejects_unnamed_stream_target_without_changing_file() {
    // Given: the only DB stream is unnamed, and the user tries its ID as the target.
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    let config_path = temp.path().join("config.toml");
    let db_path = temp.path().join("tt.db");
    write_config(&config_path, &db_path);
    insert_stream(&db_path, "stream-unnamed", None);
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("priorities.md"), priority_line("ipi")).unwrap();
    let streams_path = store.join("streams.md");
    let original = "# stream links\n";
    std::fs::write(&streams_path, original).unwrap();

    // When: link targets the unnamed stream by ID-like text.
    let output = run_tt(
        &config_path,
        &store,
        &["streams", "link", "stream-unnamed", "ipi"],
    );

    // Then: exact-name resolution rejects it and streams.md is unchanged.
    assert_failure_contains(&output, "no stream named 'stream-unnamed'");
    assert_eq!(std::fs::read_to_string(&streams_path).unwrap(), original);
}

#[test]
fn streams_link_rejects_second_link_for_same_stream_without_changing_file() {
    // Given: streams.md already links the target stream to a priority.
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    let config_path = temp.path().join("config.toml");
    let db_path = temp.path().join("tt.db");
    write_config(&config_path, &db_path);
    insert_stream(&db_path, "stream-1", Some("Fable 5 DPI"));
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(
        store.join("priorities.md"),
        format!("{}{}", priority_line("ipi"), priority_line("admin")),
    )
    .unwrap();
    let streams_path = store.join("streams.md");
    let original = "- Fable 5 DPI <!-- tt-stream:{\"priority\":\"ipi\"} -->\n";
    std::fs::write(&streams_path, original).unwrap();

    // When: a second link is attempted for the same exact stream name.
    let output = run_tt(
        &config_path,
        &store,
        &["streams", "link", "Fable 5 DPI", "admin"],
    );

    // Then: v1 one-priority-per-stream is enforced and the file is unchanged.
    assert_failure_contains(&output, "stream 'Fable 5 DPI' already has a priority link");
    assert_eq!(std::fs::read_to_string(&streams_path).unwrap(), original);
}

#[test]
fn streams_link_rejects_missing_priority_slug_without_changing_file() {
    // Given: a named stream exists but priorities.md lacks the requested slug.
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    let config_path = temp.path().join("config.toml");
    let db_path = temp.path().join("tt.db");
    write_config(&config_path, &db_path);
    insert_stream(&db_path, "stream-1", Some("Fable 5 DPI"));
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("priorities.md"), priority_line("ipi")).unwrap();
    let streams_path = store.join("streams.md");
    let original = "# stream links\n";
    std::fs::write(&streams_path, original).unwrap();

    // When: link targets a missing priority slug.
    let output = run_tt(
        &config_path,
        &store,
        &["streams", "link", "Fable 5 DPI", "missing"],
    );

    // Then: the command fails and streams.md is byte-identical.
    assert_failure_contains(&output, "priority 'missing' not found");
    assert_eq!(std::fs::read_to_string(&streams_path).unwrap(), original);
}

#[test]
fn streams_link_rejects_sync_conflict_before_changing_file() {
    // Given: a valid stream and priority but a Syncthing conflict file is present.
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("todos");
    let config_path = temp.path().join("config.toml");
    let db_path = temp.path().join("tt.db");
    write_config(&config_path, &db_path);
    insert_stream(&db_path, "stream-1", Some("Fable 5 DPI"));
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("priorities.md"), priority_line("ipi")).unwrap();
    std::fs::write(store.join("streams.sync-conflict-20260623.md"), "conflict").unwrap();

    // When: stream linking runs against the conflicted store.
    let output = run_tt(
        &config_path,
        &store,
        &["streams", "link", "Fable 5 DPI", "ipi"],
    );

    // Then: the conflict blocks the mutation and streams.md is not created.
    assert_failure_contains(&output, "streams.sync-conflict-20260623.md");
    assert!(!store.join("streams.md").exists());
}

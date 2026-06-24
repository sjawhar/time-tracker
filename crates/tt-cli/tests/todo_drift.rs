use std::path::PathBuf;
use std::process::Command;

use chrono::{Duration, Local};
use serde_json::{Value, json};
use tempfile::TempDir;
use tt_cli::commands::report::{Period, get_period_boundaries};
use tt_core::EventType;
use tt_db::{Database, StoredEvent, Stream};

const PRIORITIES: &str = "- [ ] Alpha <!-- tt-priority:{\"slug\":\"alpha\",\"value\":3,\"status\":\"active\"} -->\n- [ ] Beta <!-- tt-priority:{\"slug\":\"beta\",\"value\":1,\"status\":\"active\"} -->\n";
const STREAMS: &str = "- Alpha Stream <!-- tt-stream:{\"priority\":\"alpha\"} -->\n- Beta Stream <!-- tt-stream:{\"priority\":\"beta\"} -->\n";

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

#[test]
fn todo_drift_json_reports_both_lenses_and_unattributed_time() {
    let temp = TempDir::new().unwrap();
    let (config, db_path, store) = write_config(&temp);
    std::fs::write(store.join("priorities.md"), PRIORITIES).unwrap();
    std::fs::write(store.join("streams.md"), STREAMS).unwrap();
    let db = Database::open(&db_path).unwrap();
    insert_three_stream_fixture(&db, current_week_start() + Duration::hours(1));
    drop(db);

    let json = run_drift_json(&config);

    assert_eq!(json["priorities"][0]["priority_slug"], json!("alpha"));
    assert_eq!(json["priorities"][0]["importance_share"], json!(0.75));
    assert_eq!(json["priorities"][0]["direct_ms"], json!(300_000));
    assert_eq!(
        json["priorities"][0]["direct_plus_delegated_ms"],
        json!(1_200_000)
    );
    assert_eq!(json["unattributed"]["direct_ms"], json!(300_000));
}

#[test]
fn todo_drift_without_stream_links_puts_all_time_in_unattributed_bucket() {
    let temp = TempDir::new().unwrap();
    let (config, db_path, store) = write_config(&temp);
    std::fs::write(
        store.join("priorities.md"),
        "- [ ] Alpha <!-- tt-priority:{\"slug\":\"alpha\",\"value\":1,\"status\":\"active\"} -->\n",
    )
    .unwrap();
    let db = Database::open(&db_path).unwrap();
    let base = current_week_start() + Duration::hours(2);
    insert_stream(&db, "loose", Some("Loose"), base);
    db.insert_event(&event(spec(
        "loose-focus",
        base,
        EventType::TmuxPaneFocus,
        "loose",
    )))
    .unwrap();
    drop(db);

    let json = run_drift_json(&config);

    assert_eq!(json["priorities"][0]["direct_ms"], json!(0));
    assert_eq!(json["unattributed"]["direct_ms"], json!(300_000));
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

fn current_week_start() -> chrono::DateTime<chrono::Utc> {
    let (start, _end) = get_period_boundaries(Period::Week, Local::now().date_naive());
    start
}

fn insert_three_stream_fixture(db: &Database, base: chrono::DateTime<chrono::Utc>) {
    insert_stream(db, "alpha", Some("Alpha Stream"), base);
    insert_stream(db, "beta", Some("Beta Stream"), base);
    insert_stream(db, "unnamed", None, base);
    let specs = [
        spec("focus-alpha", base, EventType::TmuxPaneFocus, "alpha"),
        spec(
            "focus-beta",
            base + Duration::minutes(5),
            EventType::TmuxPaneFocus,
            "beta",
        ),
        spec(
            "focus-unnamed",
            base + Duration::minutes(10),
            EventType::TmuxPaneFocus,
            "unnamed",
        ),
        spec(
            "agent-start",
            base + Duration::minutes(15),
            EventType::AgentSession,
            "alpha",
        )
        .session("alpha-agent", "started"),
        spec(
            "agent-tool",
            base + Duration::minutes(20),
            EventType::AgentToolUse,
            "alpha",
        )
        .session_id("alpha-agent"),
        spec(
            "agent-end",
            base + Duration::minutes(35),
            EventType::AgentSession,
            "alpha",
        )
        .session("alpha-agent", "ended"),
    ];
    db.insert_events(&specs.into_iter().map(event).collect::<Vec<_>>())
        .unwrap();
}

fn insert_stream(
    db: &Database,
    id: &str,
    name: Option<&str>,
    timestamp: chrono::DateTime<chrono::Utc>,
) {
    db.insert_stream(&Stream {
        id: id.to_string(),
        name: name.map(ToString::to_string),
        created_at: timestamp,
        updated_at: timestamp,
        time_direct_ms: 0,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: false,
    })
    .unwrap();
}

#[derive(Clone, Copy)]
struct EventSpec<'a> {
    id: &'a str,
    timestamp: chrono::DateTime<chrono::Utc>,
    event_type: EventType,
    stream_id: &'a str,
    session_id: Option<&'a str>,
    action: Option<&'a str>,
}

impl<'a> EventSpec<'a> {
    const fn session_id(mut self, session_id: &'a str) -> Self {
        self.session_id = Some(session_id);
        self
    }

    const fn session(mut self, session_id: &'a str, action: &'a str) -> Self {
        self.session_id = Some(session_id);
        self.action = Some(action);
        self
    }
}

const fn spec<'a>(
    id: &'a str,
    timestamp: chrono::DateTime<chrono::Utc>,
    event_type: EventType,
    stream_id: &'a str,
) -> EventSpec<'a> {
    EventSpec {
        id,
        timestamp,
        event_type,
        stream_id,
        session_id: None,
        action: None,
    }
}

fn event(spec: EventSpec<'_>) -> StoredEvent {
    StoredEvent {
        id: spec.id.to_string(),
        timestamp: spec.timestamp,
        event_type: spec.event_type,
        source: "test".to_string(),
        machine_id: None,
        schema_version: 1,
        pane_id: None,
        tmux_session: None,
        window_index: None,
        git_project: None,
        git_workspace: None,
        status: None,
        idle_duration_ms: None,
        window_app_id: None,
        window_title: None,
        action: spec.action.map(ToString::to_string),
        cwd: Some("/tmp/project".to_string()),
        session_id: spec.session_id.map(ToString::to_string),
        stream_id: Some(spec.stream_id.to_string()),
        assignment_source: Some("test".to_string()),
        data: json!({}),
    }
}

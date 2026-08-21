//! `tt sync` must import events without dragging the whole history through
//! time recomputation, and must shout about remotes that have gone dark.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use tempfile::TempDir;
use tt_db::{Database, StoredEvent, Stream};

mod common;
use common::CommandExt;

/// Times seeded on the stream before syncing. Recomputation would overwrite
/// these with the values its allocation produces, so they double as a tripwire.
const SEEDED_DIRECT_MS: i64 = 111;
const SEEDED_DELEGATED_MS: i64 = 222;

const SYNCED_MACHINE_UUID: &str = "550e8400-e29b-41d4-a716-446655440000";

fn tt_binary() -> String {
    env!("CARGO_BIN_EXE_tt").to_string()
}

struct Fixture {
    home: TempDir,
    config: PathBuf,
    db_path: PathBuf,
    fake_bin: PathBuf,
}

impl Fixture {
    /// Builds a hermetic home: its own config, database, and a `ssh` stub on
    /// `PATH` that answers with a gzipped export.
    fn new() -> Self {
        let home = TempDir::new().unwrap();
        let db_path = home.path().join("tt.db");
        let store = home.path().join("todo-store");
        let config = home.path().join("config.toml");
        let fake_bin = home.path().join("bin");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::create_dir_all(&fake_bin).unwrap();
        std::fs::write(
            &config,
            format!(
                "database_path = \"{}\"\ntodo_store_path = \"{}\"\n",
                db_path.display(),
                store.display()
            ),
        )
        .unwrap();
        write_fake_ssh(&fake_bin);
        Self {
            home,
            config,
            db_path,
            fake_bin,
        }
    }

    fn db(&self) -> Database {
        Database::open(&self.db_path).unwrap()
    }

    fn sync(&self, remote: &str) -> Output {
        let path = format!(
            "{}:{}",
            self.fake_bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new(tt_binary())
            .sandboxed_home(self.home.path())
            .env("PATH", path)
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("OPENCODE_SESSION_ID")
            .arg("--config")
            .arg(&self.config)
            .args(["sync", remote])
            .output()
            .unwrap()
    }
}

/// Writes an `ssh` stub that ignores its arguments and emits one gzipped event
/// dated now, so the synced remote reads as live.
fn write_fake_ssh(bin: &Path) {
    let timestamp = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let event_id = format!("{SYNCED_MACHINE_UUID}:remote.tmux:tmux_pane_focus:{timestamp}:%9");
    let event = json!({
        "id": event_id,
        "timestamp": timestamp,
        "source": "remote.tmux",
        "type": "tmux_pane_focus",
        "pane_id": "%9",
        "tmux_session": "main",
        "cwd": "/synced",
    });
    let ssh = bin.join("ssh");
    std::fs::write(
        &ssh,
        format!("#!/bin/sh\nprintf '%s\\n' '{event}' | gzip\n"),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn make_event(
    id: &str,
    timestamp: DateTime<Utc>,
    event_type: tt_core::EventType,
    action: Option<&str>,
) -> StoredEvent {
    StoredEvent {
        id: id.to_owned(),
        timestamp,
        event_type,
        source: "remote.agent".to_owned(),
        machine_id: None,
        schema_version: 1,
        pane_id: Some("%1".to_owned()),
        tmux_session: None,
        window_index: None,
        git_project: None,
        git_workspace: None,
        status: None,
        idle_duration_ms: None,
        window_app_id: None,
        window_title: None,
        action: action.map(str::to_owned),
        cwd: Some("/project".to_owned()),
        session_id: Some("sess1".to_owned()),
        stream_id: Some("stream-1".to_owned()),
        assignment_source: Some("inferred".to_owned()),
        data: json!({}),
    }
}

/// Seeds a stream that is flagged for recomputation and backed by events that
/// allocate very different times from the seeded ones.
fn seed_recomputable_stream(db: &Database) {
    let base = Utc::now() - Duration::hours(2);
    db.insert_stream(&Stream {
        id: "stream-1".to_owned(),
        name: Some("seeded".to_owned()),
        slug: None,
        description: None,
        color: None,
        created_at: base,
        updated_at: base,
        time_direct_ms: SEEDED_DIRECT_MS,
        time_delegated_ms: SEEDED_DELEGATED_MS,
        first_event_at: Some(base),
        last_event_at: Some(base + Duration::minutes(30)),
        needs_recompute: true,
    })
    .unwrap();

    let events = [
        make_event("e1", base, tt_core::EventType::TmuxPaneFocus, None),
        make_event(
            "e2",
            base,
            tt_core::EventType::AgentSession,
            Some("started"),
        ),
        make_event(
            "e3",
            base + Duration::minutes(5),
            tt_core::EventType::AgentToolUse,
            None,
        ),
        make_event(
            "e4",
            base + Duration::minutes(30),
            tt_core::EventType::AgentSession,
            Some("ended"),
        ),
    ];
    for event in &events {
        db.insert_event(event).unwrap();
        db.assign_event_to_stream(&event.id, "stream-1", "inferred")
            .unwrap();
    }
}

#[test]
fn sync_imports_events_without_recomputing_stream_times() {
    // Given: a stream flagged for recomputation, with stale stored times
    let fixture = Fixture::new();
    seed_recomputable_stream(&fixture.db());

    // When: syncing a remote that returns one new event
    let output = fixture.sync("live-remote");
    assert!(
        output.status.success(),
        "tt sync should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Then: the event landed, so the import half of sync still works
    let db = fixture.db();
    assert!(
        db.get_events(None, None)
            .unwrap()
            .iter()
            .any(|event| event.id.starts_with(SYNCED_MACHINE_UUID)),
        "sync should still import remote events"
    );

    // Then: the stream's times are untouched and it stays flagged, because
    // recomputation never ran
    let stream = db.get_stream("stream-1").unwrap().unwrap();
    assert_eq!(stream.time_direct_ms, SEEDED_DIRECT_MS);
    assert_eq!(stream.time_delegated_ms, SEEDED_DELEGATED_MS);
    assert!(
        stream.needs_recompute,
        "sync must leave recomputation to 'tt recompute'"
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("Recomputing time"),
        "sync should not announce a recompute it no longer performs"
    );
}

#[test]
fn sync_warns_about_a_machine_that_has_gone_dark() {
    // Given: a known remote whose newest event is seven weeks old
    let fixture = Fixture::new();
    {
        let db = fixture.db();
        db.upsert_machine("dark-uuid", "ghost-wispr", None).unwrap();
        let mut stale = make_event(
            "old-1",
            Utc::now() - Duration::days(49),
            tt_core::EventType::TmuxPaneFocus,
            None,
        );
        stale.machine_id = Some("dark-uuid".to_owned());
        stale.stream_id = None;
        stale.assignment_source = None;
        db.insert_event(&stale).unwrap();
    }

    // When: syncing an unrelated remote
    let output = fixture.sync("live-remote");
    assert!(output.status.success());

    // Then: the dark remote is named on stderr with how long it has been silent
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("⚠ ghost-wispr has sent no events for 49d"),
        "expected a dark-machine warning, got: {stderr}"
    );
    // And the remote that just reported is not flagged
    assert!(
        !stderr.contains("live-remote"),
        "a reporting remote should not be flagged: {stderr}"
    );
}

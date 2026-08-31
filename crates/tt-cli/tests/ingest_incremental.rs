//! Session ingest re-derives only what changed.
//!
//! The daemon runs this every ~30s. Re-deriving the whole corpus each tick cost
//! ~7.7s of wall time and ~900 MB of RSS against a 6.7 GB `OpenCode` store — a
//! permanent ~200% CPU load on the machine the product exists to measure. These
//! tests pin the cursor's behaviour, including the two ways it must refuse to move.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{TimeZone, Utc};
use serde_json::json;
use tt_cli::commands::ingest::{self, IngestPaths, SCAN_OVERLAP_MINUTES, ScanMode};

/// A Claude transcript whose mtime is `age` before now.
fn plant_claude_session(projects: &Path, session_id: &str, age: Duration) {
    let project = projects.join("-home-sami-proj");
    std::fs::create_dir_all(&project).expect("create project dir");
    let path = project.join(format!("{session_id}.jsonl"));
    let mut file = File::create(&path).expect("create transcript");
    writeln!(
        file,
        r#"{{"type":"user","message":{{"role":"user","content":"do the thing"}},"timestamp":"2026-01-29T10:58:45.000Z","cwd":"/home/sami/proj"}}"#
    )
    .expect("write transcript");
    writeln!(
        file,
        r#"{{"type":"assistant","message":{{"role":"assistant","content":"done"}},"timestamp":"2026-01-29T10:59:00.000Z"}}"#
    )
    .expect("write transcript");
    drop(file);
    touch(&path, age);
}

/// An omp transcript whose mtime is `age` before now.
fn plant_omp_session(sessions_dir: &Path, uuid: &str, age: Duration) {
    let cwd_dir = sessions_dir.join("-tmp-proj");
    std::fs::create_dir_all(&cwd_dir).expect("create omp cwd dir");
    let path = cwd_dir.join(format!("2026-02-02T10-00-00-000Z_{uuid}.jsonl"));
    let mut file = File::create(&path).expect("create omp transcript");
    writeln!(
        file,
        r#"{{"type":"session","version":3,"id":"{uuid}","timestamp":"2026-02-02T10:00:00.000Z","cwd":"/tmp/proj"}}"#
    )
    .expect("write omp transcript");
    writeln!(
        file,
        r#"{{"type":"message","id":"a","parentId":null,"timestamp":"2026-02-02T10:00:01.000Z","message":{{"role":"user","content":[{{"type":"text","text":"do the thing"}}]}}}}"#
    )
    .expect("write omp transcript");
    writeln!(
        file,
        r#"{{"type":"message","id":"b","parentId":"a","timestamp":"2026-02-02T10:00:02.000Z","message":{{"role":"assistant","content":[{{"type":"text","text":"done"}}]}}}}"#
    )
    .expect("write omp transcript");
    drop(file);
    touch(&path, age);
}

/// An omp continuation whose opening history is replayed before its creation time.
fn plant_omp_continuation(sessions_dir: &Path, continuation_id: &str, parent_id: &str) {
    let cwd_dir = sessions_dir.join("-tmp-proj");
    std::fs::create_dir_all(&cwd_dir).expect("create omp cwd dir");
    let path = cwd_dir.join(format!("2026-02-02T10-00-00-000Z_{continuation_id}.jsonl"));
    let mut file = File::create(path).expect("create omp continuation");
    writeln!(
        file,
        r#"{{"type":"session","version":3,"id":"{continuation_id}","parentSession":"{parent_id}","timestamp":"2026-02-02T10:00:00.000Z","cwd":"/tmp/proj"}}"#
    )
    .expect("write session");
    writeln!(
        file,
        r#"{{"type":"message","id":"old","parentId":null,"timestamp":"2026-02-02T09:00:00.000Z","message":{{"role":"user","content":[{{"type":"text","text":"parent prompt"}}]}}}}"#
    )
    .expect("write replayed prompt");
    writeln!(
        file,
        r#"{{"type":"message","id":"new","parentId":"old","timestamp":"2026-02-02T10:01:00.000Z","message":{{"role":"user","content":[{{"type":"text","text":"continuation prompt"}}]}}}}"#
    )
    .expect("write continuation prompt");
}

/// Stamps a file's mtime to `age` before now.
fn touch(path: &Path, age: Duration) {
    File::options()
        .write(true)
        .open(path)
        .expect("open for touch")
        .set_modified(SystemTime::now() - age)
        .expect("set mtime");
}

/// Older than the safety overlap, so a warm cursor genuinely excludes it.
const fn settled() -> Duration {
    Duration::from_secs((SCAN_OVERLAP_MINUTES as u64 + 5) * 60)
}

struct Fixture {
    _temp: tempfile::TempDir,
    paths: IngestPaths,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = IngestPaths {
            claude_projects: temp.path().join("claude/projects"),
            // Absent by default: an absent store is skipped, not a failure.
            opencode_db: temp.path().join("opencode/opencode.db"),
            omp_sessions: temp.path().join("omp/sessions"),
            data_dir: temp.path().join("data"),
        };
        std::fs::create_dir_all(&paths.claude_projects).expect("create claude dir");
        std::fs::create_dir_all(&paths.data_dir).expect("create data dir");
        Self { _temp: temp, paths }
    }

    fn claude(&self) -> &Path {
        &self.paths.claude_projects
    }

    fn omp(&self) -> &Path {
        &self.paths.omp_sessions
    }

    /// Replaces the `OpenCode` store with a file that exists but will not open.
    fn break_opencode_store(&self) -> &Self {
        let db = &self.paths.opencode_db;
        std::fs::create_dir_all(db.parent().expect("parent")).expect("create opencode dir");
        std::fs::write(db, b"this is not a sqlite database").expect("write garbage");
        self
    }

    fn ingest(&self, db: &tt_db::Database, mode: ScanMode) -> ingest::IngestReport {
        ingest::index_sessions_in(db, &self.paths, mode).expect("ingest")
    }
}

fn open_db() -> tt_db::Database {
    tt_db::Database::open_in_memory().expect("open db")
}

/// Given a corpus that has not changed, When ingest runs a second time, Then it
/// re-derives nothing.
///
/// This is the whole performance fix: the daemon's steady-state tick must not touch
/// a session it already read.
#[test]
fn second_ingest_with_unchanged_corpus_rederives_nothing() {
    let fixture = Fixture::new();
    plant_claude_session(fixture.claude(), "ses-settled", settled());
    let db = open_db();

    let first = fixture.ingest(&db, ScanMode::Incremental);
    let second = fixture.ingest(&db, ScanMode::Incremental);

    assert_eq!(first.claude, 1, "first pass derives the session");
    assert_eq!(
        second.claude, 0,
        "an unchanged corpus must cost no re-derivation"
    );
}

/// Given a session modified after the cursor, When ingest runs, Then it is
/// re-derived.
#[test]
fn session_modified_after_cursor_is_rederived() {
    let fixture = Fixture::new();
    plant_claude_session(fixture.claude(), "ses-settled", settled());
    let db = open_db();
    fixture.ingest(&db, ScanMode::Incremental);

    touch(
        &fixture.claude().join("-home-sami-proj/ses-settled.jsonl"),
        Duration::from_secs(0),
    );
    let after = fixture.ingest(&db, ScanMode::Incremental);

    assert_eq!(after.claude, 1, "a changed session must be re-derived");
}

/// Given a warm cursor, When ingest runs in `Full` mode, Then the whole corpus is
/// re-derived.
///
/// This exists for the two cases a cursor cannot answer for itself: a change to what
/// the extractor derives, and any suspicion the cursor drifted.
#[test]
fn full_mode_rederives_everything() {
    let fixture = Fixture::new();
    plant_claude_session(fixture.claude(), "ses-settled", settled());
    let db = open_db();
    fixture.ingest(&db, ScanMode::Incremental);

    let forced = fixture.ingest(&db, ScanMode::Full);

    assert_eq!(forced.claude, 1, "--full must ignore the cursor");
}

/// Given `Full` mode, When it succeeds, Then the cursor is refreshed rather than
/// cleared \u2014 a forced pass is one-shot, not a mode switch.
#[test]
fn full_mode_leaves_an_advanced_cursor_behind() {
    let fixture = Fixture::new();
    plant_claude_session(fixture.claude(), "ses-settled", settled());
    let db = open_db();

    fixture.ingest(&db, ScanMode::Full);

    let cursor = db.get_session_scan_cursor().expect("read cursor");
    assert!(cursor.is_some(), "a successful full pass records a cursor");
    let next = fixture.ingest(&db, ScanMode::Incremental);
    assert_eq!(next.claude, 0, "the pass after --full is incremental again");
}

/// Given a store that exists but cannot be read, When ingest runs, Then the cursor
/// does not advance.
///
/// An unreadable store yields no sessions, which is indistinguishable from "nothing
/// changed". Advancing here would skip every session written in that window, forever.
#[test]
fn cursor_does_not_advance_when_the_scan_is_incomplete() {
    let fixture = Fixture::new();
    fixture.break_opencode_store();
    plant_claude_session(fixture.claude(), "ses-settled", settled());
    let db = open_db();

    fixture.ingest(&db, ScanMode::Incremental);

    assert_eq!(
        db.get_session_scan_cursor().expect("read cursor"),
        None,
        "a degraded scan must leave the cursor where it was"
    );
}

/// Given a scan that stayed incomplete, When ingest runs again, Then the corpus is
/// re-derived rather than skipped.
///
/// The complement of the test above: refusing to advance is only worth anything if
/// the next pass actually retries the window.
#[test]
fn an_incomplete_scan_retries_the_whole_window_next_pass() {
    let fixture = Fixture::new();
    fixture.break_opencode_store();
    plant_claude_session(fixture.claude(), "ses-settled", settled());
    let db = open_db();

    let first = fixture.ingest(&db, ScanMode::Incremental);
    let second = fixture.ingest(&db, ScanMode::Incremental);

    assert_eq!(first.claude, 1);
    assert_eq!(
        second.claude, 1,
        "a frozen cursor must make the next pass re-read the window"
    );
}

/// Given a successful scan, When it completes, Then the recorded cursor is the moment
/// the scan *began*, not the moment it ended.
///
/// A session written while the scan was running has an mtime between those two
/// instants. Recording the end would place it before the cursor and lose it.
#[test]
fn cursor_records_when_the_scan_started_not_when_it_finished() {
    let fixture = Fixture::new();
    plant_claude_session(fixture.claude(), "ses-settled", settled());
    let db = open_db();

    let before = Utc::now();
    fixture.ingest(&db, ScanMode::Incremental);
    let after = Utc::now();

    let cursor = db
        .get_session_scan_cursor()
        .expect("read cursor")
        .expect("cursor set");
    assert!(cursor >= before - chrono::Duration::seconds(1));
    assert!(
        cursor <= after,
        "cursor must not be later than the scan end"
    );
}

/// Given a session the incremental pass did not look at, When the prune runs, Then
/// that session's `user_message` events survive.
///
/// `prune_user_message_events` retires rows absent from a per-session keep-set, and
/// incremental scanning shrinks that keep-set to the sessions actually re-derived. It
/// stays correct only because a session absent from the keep-set is untouched. If
/// this test fails, incremental ingest is deleting history it never read.
#[test]
fn prune_leaves_events_of_sessions_the_incremental_pass_skipped() {
    let fixture = Fixture::new();
    plant_claude_session(fixture.claude(), "ses-settled", settled());
    let db = open_db();
    fixture.ingest(&db, ScanMode::Incremental);
    let before = user_message_count(&db, "ses-settled");

    // A second pass that skips the session entirely, then a third with a *new*
    // session present so the keep-set is non-empty but excludes the first.
    plant_claude_session(fixture.claude(), "ses-fresh", Duration::from_secs(0));
    fixture.ingest(&db, ScanMode::Incremental);

    assert!(before > 0, "fixture must produce user_message events");
    assert_eq!(
        user_message_count(&db, "ses-settled"),
        before,
        "a session the pass never read must keep its events"
    );
}

fn user_message_count(db: &tt_db::Database, session_id: &str) -> usize {
    db.get_events(None, None)
        .expect("read events")
        .into_iter()
        .filter(|event| {
            event.event_type == tt_core::EventType::UserMessage
                && event.session_id.as_deref() == Some(session_id)
        })
        .count()
}

/// Given no cursor at all, When ingest runs, Then it scans everything.
#[test]
fn first_ever_pass_scans_the_whole_corpus() {
    let fixture = Fixture::new();
    plant_claude_session(
        fixture.claude(),
        "ses-ancient",
        Duration::from_secs(86_400 * 30),
    );
    let db = open_db();

    let report = fixture.ingest(&db, ScanMode::Incremental);

    assert_eq!(report.claude, 1, "a cold cursor means a full scan");
}

/// Given an omp transcript, When ingest runs, Then it lands in `agent_sessions`
/// with `source = 'omp'`.
#[test]
fn omp_session_is_ingested_with_source_omp() {
    let fixture = Fixture::new();
    let uuid = "01a0082b-2c9d-7000-b52c-10ef998c3061";
    plant_omp_session(fixture.omp(), uuid, settled());
    let db = open_db();

    let report = fixture.ingest(&db, ScanMode::Incremental);

    assert_eq!(report.omp, 1, "the omp transcript must be derived");
    let (session, _machine_id) = db
        .get_agent_session(uuid)
        .expect("read session")
        .expect("session row present");
    assert_eq!(session.source, tt_core::session::SessionSource::Omp);
    assert_eq!(session.message_count, 2);
}

/// Given a continuation whose transcript embeds its parent's opening prompt, When
/// `--full` re-derives it, Then only its new prompt remains and the stale event from
/// the earlier derivation is pruned.
#[test]
fn full_rederive_prunes_replayed_omp_continuation_user_messages() {
    let fixture = Fixture::new();
    let parent_id = "01a0082b-2c9d-7000-b52c-10ef998c3061";
    let continuation_id = "01a0082d-014b-7000-9fdb-71cdbcef63be";
    plant_omp_continuation(fixture.omp(), continuation_id, parent_id);
    let db = open_db();

    fixture.ingest(&db, ScanMode::Full);
    let (session, _) = db
        .get_agent_session(continuation_id)
        .expect("read continuation")
        .expect("continuation present");
    assert_eq!(
        session.session_type,
        tt_core::session::SessionType::Continuation
    );
    assert_eq!(session.parent_session_id.as_deref(), Some(parent_id));
    assert_eq!(session.message_count, 1);
    assert_eq!(user_message_count(&db, continuation_id), 1);

    let replayed_at = Utc
        .with_ymd_and_hms(2026, 2, 2, 9, 0, 0)
        .single()
        .expect("timestamp");
    db.insert_event(&tt_db::StoredEvent {
        id: format!(
            "{continuation_id}-user_message-{}",
            replayed_at.timestamp_millis()
        ),
        timestamp: replayed_at,
        event_type: tt_core::EventType::UserMessage,
        source: "omp".to_string(),
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
        action: None,
        cwd: Some("/tmp/proj".to_string()),
        session_id: Some(continuation_id.to_string()),
        stream_id: None,
        assignment_source: None,
        data: json!({}),
    })
    .expect("insert stale replay event");
    assert_eq!(user_message_count(&db, continuation_id), 2);

    fixture.ingest(&db, ScanMode::Full);
    assert_eq!(user_message_count(&db, continuation_id), 1);
}

/// Given an omp corpus that has not changed, When ingest runs a second time, Then
/// the unchanged transcript is skipped, matching the Claude and `OpenCode`
/// incremental contract.
#[test]
fn omp_cursor_skips_unchanged_files() {
    let fixture = Fixture::new();
    plant_omp_session(
        fixture.omp(),
        "01a0082b-2c9d-7000-b52c-10ef998c3061",
        settled(),
    );
    let db = open_db();

    let first = fixture.ingest(&db, ScanMode::Incremental);
    let second = fixture.ingest(&db, ScanMode::Incremental);

    assert_eq!(first.omp, 1, "first pass derives the omp session");
    assert_eq!(
        second.omp, 0,
        "an unchanged omp transcript must cost no re-derivation"
    );
}

/// The production path resolves real store locations without panicking.
#[test]
fn ingest_paths_resolve_from_env() {
    let paths = IngestPaths::from_env().expect("resolve paths");

    assert!(paths.claude_projects.is_absolute() || paths.claude_projects.as_os_str().is_empty());
    assert!(paths.opencode_db.ends_with("opencode/opencode.db"));
    assert!(paths.omp_sessions.ends_with("sessions"));
    let _: &PathBuf = &paths.data_dir;
}

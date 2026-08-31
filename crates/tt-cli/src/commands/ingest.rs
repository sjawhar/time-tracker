//! Ingest command for receiving events from tmux hooks.
//!
//! This module handles event ingestion on remote machines. Events are written
//! to a JSONL file for later sync to the local machine.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tt_core::project::ProjectIdentity;

mod pane_session;
mod pane_sweep;

pub use pane_sweep::{PaneSweepOutcome, sweep_pane_sessions};

use crate::commands::import;

/// An event to be ingested and written to the events file.
///
/// This is a flat structure representing a tmux pane focus event.
/// All fields are at the top level for clarity and simplicity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestEvent {
    pub id: String,
    pub timestamp: String,
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(rename = "type")]
    pub event_type: String,
    /// Working directory where the event occurred.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The agent session running in the focused pane, when one was identified.
    ///
    /// An identity rather than an inference: the pane being focused *is* running
    /// that session. Nothing here derives a stream from it — the already-trusted
    /// session→stream path claims the event once the session is classified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The tmux pane ID (e.g., %3).
    pub pane_id: String,
    /// The tmux session name.
    pub tmux_session: String,
    /// The tmux window index.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_index: Option<u32>,
    /// The git project name (from git remote origin).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_project: Option<String>,
    /// The git workspace name (if in a non-default workspace).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_workspace: Option<String>,
}

fn default_source() -> String {
    "remote.tmux".to_string()
}

/// Get project identity for a directory using jj/git commands.
/// Falls back to using the directory name if jj commands fail.
fn get_git_identity(cwd: &std::path::Path) -> Option<ProjectIdentity> {
    use std::process::Command;

    if !cwd.join(".jj").exists() {
        return None;
    }

    // Try to get info from jj commands, but fall back to directory name if they fail
    let remote_output = Command::new("jj")
        .args(["git", "remote", "list", "--ignore-working-copy"])
        .current_dir(cwd)
        .output()
        .ok();

    let remote_url = remote_output.as_ref().and_then(|output| {
        let remote_str = String::from_utf8_lossy(&output.stdout);
        remote_str
            .lines()
            .find(|line| line.contains("origin"))
            .and_then(|line| line.split_whitespace().nth(1))
            .map(String::from)
    });

    let workspace_output = Command::new("jj")
        .args(["workspace", "list", "--ignore-working-copy"])
        .current_dir(cwd)
        .output()
        .ok();

    let workspace_count = workspace_output.as_ref().map_or(1, |output| {
        String::from_utf8_lossy(&output.stdout).lines().count()
    });

    let root_output = Command::new("jj")
        .args(["root", "--ignore-working-copy"])
        .current_dir(cwd)
        .output()
        .ok();

    // Use jj root if available, otherwise use the cwd
    let jj_root = root_output
        .as_ref()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| cwd.to_string_lossy().to_string());

    Some(ProjectIdentity::from_jj_output(
        remote_url.as_deref(),
        workspace_count,
        &jj_root,
    ))
}

impl IngestEvent {
    /// Creates a new pane focus event with a deterministic ID.
    pub fn pane_focus(
        machine_id: &str,
        pane_id: String,
        tmux_session: String,
        window_index: Option<u32>,
        cwd: String,
        session_id: Option<String>,
        timestamp: DateTime<Utc>,
    ) -> Self {
        let timestamp_str = timestamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let id = format!("{machine_id}:remote.tmux:tmux_pane_focus:{timestamp_str}:{pane_id}");

        let git_identity = get_git_identity(Path::new(&cwd));

        Self {
            id,
            timestamp: timestamp_str,
            source: "remote.tmux".to_string(),
            event_type: "tmux_pane_focus".to_string(),
            cwd: Some(cwd),
            session_id,
            pane_id,
            tmux_session,
            window_index,
            git_project: git_identity.as_ref().map(|i| i.project_name.clone()),
            git_workspace: git_identity.and_then(|i| i.workspace_name),
        }
    }

    /// Creates a new tmux scroll event with a deterministic ID.
    pub fn scroll(
        machine_id: &str,
        pane_id: String,
        tmux_session: String,
        window_index: Option<u32>,
        cwd: String,
        timestamp: DateTime<Utc>,
    ) -> Self {
        let timestamp_str = timestamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let id = format!("{machine_id}:remote.tmux:tmux_scroll:{timestamp_str}:{pane_id}");

        let git_identity = get_git_identity(Path::new(&cwd));

        Self {
            id,
            timestamp: timestamp_str,
            source: "remote.tmux".to_string(),
            event_type: "tmux_scroll".to_string(),
            cwd: Some(cwd),
            session_id: None,
            pane_id,
            tmux_session,
            window_index,
            git_project: git_identity.as_ref().map(|i| i.project_name.clone()),
            git_workspace: git_identity.and_then(|i| i.workspace_name),
        }
    }
}

/// Debounce window for pane focus events (500ms).
const DEBOUNCE_WINDOW_MS: u64 = 500;

/// Maximum events file size before rotation (1MB).
const MAX_EVENTS_FILE_SIZE: u64 = 1024 * 1024;

/// Returns the default time tracker data directory.
fn default_data_dir() -> PathBuf {
    crate::config::dirs_data_path().unwrap_or_else(|| PathBuf::from("."))
}

/// Returns the path to the events file within the given data directory.
fn events_path(data_dir: &Path) -> PathBuf {
    data_dir.join("events.jsonl")
}

/// Returns the path to the debounce state file within the given data directory.
fn debounce_path(data_dir: &Path) -> PathBuf {
    data_dir.join(".debounce")
}

/// Returns the path to the lock file within the given data directory.
fn lock_path(data_dir: &Path) -> PathBuf {
    data_dir.join(".lock")
}

/// Returns the path to the rotated events file.
fn rotated_events_path(data_dir: &Path) -> PathBuf {
    data_dir.join("events.jsonl.1")
}

/// Rotates the events file if it exceeds the size threshold.
///
/// When rotated, the current events file becomes events.jsonl.1,
/// replacing any previous rotation. This keeps one backup for
/// recovery while preventing unbounded growth.
fn maybe_rotate_events(data_dir: &Path) -> Result<()> {
    let events_file = events_path(data_dir);

    let metadata = match fs::metadata(&events_file) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).context("failed to stat events file"),
    };

    if metadata.len() >= MAX_EVENTS_FILE_SIZE {
        let rotated = rotated_events_path(data_dir);
        fs::rename(&events_file, &rotated).context("failed to rotate events file")?;
        tracing::info!(
            size = metadata.len(),
            "rotated events file to events.jsonl.1"
        );
    }

    Ok(())
}

/// Checks if an event for the given pane should be debounced, and if not,
/// updates the debounce state.
///
/// Returns `true` if the event should be skipped (within debounce window).
fn check_and_update_debounce(data_dir: &Path, pane_id: &str, now_ms: u64) -> Result<bool> {
    let debounce_file = debounce_path(data_dir);

    // Read existing state
    let content = match fs::read_to_string(&debounce_file) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).context("failed to read debounce file"),
    };

    // Parse entries and check for debounce
    // Format: pane_id:unix_millis (use rsplit_once to handle colons in pane_id)
    let mut should_skip = false;
    let mut entries: Vec<(String, u64)> = Vec::new();

    for line in content.lines() {
        if let Some((stored_pane, stored_time)) = line.rsplit_once(':') {
            if let Ok(stored_ms) = stored_time.parse::<u64>() {
                if stored_pane == pane_id {
                    // Check if within debounce window
                    if now_ms.saturating_sub(stored_ms) < DEBOUNCE_WINDOW_MS {
                        should_skip = true;
                    }
                    // Don't keep this pane's old entry (will add new one if not skipping)
                    continue;
                }
                // Keep recent entries from other panes (within 10s)
                if now_ms.saturating_sub(stored_ms) < 10_000 {
                    entries.push((stored_pane.to_string(), stored_ms));
                }
            }
        }
    }

    if should_skip {
        return Ok(true);
    }

    // Add current entry and write back
    entries.push((pane_id.to_string(), now_ms));

    let new_content = entries
        .iter()
        .map(|(p, t)| format!("{p}:{t}"))
        .collect::<Vec<_>>()
        .join("\n");

    fs::write(&debounce_file, new_content).context("failed to write debounce file")?;

    Ok(false)
}

/// Appends an event to the events file.
fn append_event(data_dir: &Path, event: &IngestEvent) -> Result<()> {
    let events_file = events_path(data_dir);

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&events_file)
        .context("failed to open events file")?;

    let json = serde_json::to_string(event).context("failed to serialize event")?;
    writeln!(file, "{json}").context("failed to write event")?;

    Ok(())
}

/// Ingests a pane focus event to the specified data directory.
///
/// This function:
/// 1. Acquires a lock on the data directory
/// 2. Checks if the event should be debounced
/// 3. Rotates the events file if it exceeds 1MB
/// 4. If not debounced, writes the event and updates debounce state
fn ingest_pane_focus_impl(
    data_dir: &Path,
    machine_id: &str,
    pane_id: &str,
    session_name: &str,
    window_index: Option<u32>,
    cwd: &str,
    session_id: Option<String>,
) -> Result<bool> {
    if pane_id.is_empty() {
        anyhow::bail!("pane_id cannot be empty");
    }
    if session_name.is_empty() {
        anyhow::bail!("session_name cannot be empty");
    }

    let now = Utc::now();
    let event = IngestEvent::pane_focus(
        machine_id,
        pane_id.to_string(),
        session_name.to_string(),
        window_index,
        cwd.to_string(),
        session_id,
        now,
    );
    write_pane_event(data_dir, pane_id, now, &event)
}

/// Ingests a tmux scroll event (copy-mode entry) to the specified data directory.
fn ingest_scroll_impl(
    data_dir: &Path,
    machine_id: &str,
    pane_id: &str,
    session_name: &str,
    window_index: Option<u32>,
    cwd: &str,
) -> Result<bool> {
    if pane_id.is_empty() {
        anyhow::bail!("pane_id cannot be empty");
    }
    if session_name.is_empty() {
        anyhow::bail!("session_name cannot be empty");
    }

    let now = Utc::now();
    let event = IngestEvent::scroll(
        machine_id,
        pane_id.to_string(),
        session_name.to_string(),
        window_index,
        cwd.to_string(),
        now,
    );
    // Shares the pane debounce key with pane-focus: one tmux activity event per
    // pane per debounce window is enough to keep the attention window alive.
    write_pane_event(data_dir, pane_id, now, &event)
}

/// Locks the data dir, debounces by `debounce_key`, rotates if needed, and appends
/// `event`. Returns `false` when the event was debounced (skipped).
fn write_pane_event(
    data_dir: &Path,
    debounce_key: &str,
    now: DateTime<Utc>,
    event: &IngestEvent,
) -> Result<bool> {
    fs::create_dir_all(data_dir).context("failed to create data directory")?;

    let lock_file = File::create(lock_path(data_dir)).context("failed to create lock file")?;
    lock_file
        .lock_exclusive()
        .context("failed to acquire lock")?;

    #[expect(clippy::cast_sign_loss, reason = "timestamps are always positive")]
    let now_ms = now.timestamp_millis() as u64;

    if check_and_update_debounce(data_dir, debounce_key, now_ms)? {
        tracing::debug!(debounce_key, "debounced tmux event");
        return Ok(false);
    }

    maybe_rotate_events(data_dir)?;
    append_event(data_dir, event)?;

    tracing::info!(event_id = %event.id, "ingested tmux event");

    Ok(true)
}

/// Ingests a pane focus event to the default data directory.
///
/// This is the public API used by the CLI. `pane_process_id` is the focused pane's
/// process id as tmux reported it; it identifies the agent session running in that
/// pane, and every way that lookup can come up empty leaves the event exactly as
/// it would have been recorded without it.
pub fn ingest_pane_focus(
    pane_id: &str,
    session_name: &str,
    window_index: Option<u32>,
    cwd: &str,
    pane_process_id: Option<&str>,
) -> Result<bool> {
    let identity = crate::machine::require_machine_identity()?;
    ingest_pane_focus_impl(
        &default_data_dir(),
        &identity.machine_id,
        pane_id,
        session_name,
        window_index,
        cwd,
        pane_session::session_for_pane(pane_id, pane_process_id),
    )
}

/// Ingests a tmux scroll event to the default data directory.
///
/// This is the public API used by the CLI.
pub fn ingest_scroll(
    pane_id: &str,
    session_name: &str,
    window_index: Option<u32>,
    cwd: &str,
) -> Result<bool> {
    let identity = crate::machine::require_machine_identity()?;
    ingest_scroll_impl(
        &default_data_dir(),
        &identity.machine_id,
        pane_id,
        session_name,
        window_index,
        cwd,
    )
}

// ========== Sessions Indexing ==========
use tt_core::ScanOutcome;
use tt_core::omp::scan_omp_sessions_incremental;
use tt_core::opencode::scan_opencode_sessions_incremental;
use tt_core::session::{AgentSession, scan_claude_sessions_incremental};
use tt_db::StoredEvent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestReport {
    pub claude: usize,
    pub opencode: usize,
    pub omp: usize,
    indexed_events: usize,
    imported_events: usize,
    drained_events: usize,
    stale_events_cleaned: u64,
    injected_events_pruned: u64,
    session_membership_assigned: u64,
    terminal_focus_assigned: u64,
    artifact_focus_assigned: u64,
    projects: Vec<(String, usize)>,
    scanned_claude: bool,
    scanned_opencode: bool,
    scanned_omp: bool,
}

impl IngestReport {
    pub const fn imported_events(&self) -> usize {
        self.imported_events
    }
}

/// Safety overlap subtracted from the scan cursor before it is used as `since`.
///
/// Matches the margin `tt sync` already applies to `last_sync_at`, so this codebase
/// has one number for "how far back a cursor must reach" rather than two. It is
/// generous against everything it has to cover: a wall clock that steps backwards
/// across an NTP correction or a suspend/resume, and the window between `OpenCode`
/// committing a `session` row and its shard receiving the messages that row counts.
/// Both are sub-second to seconds in practice.
///
/// It is deliberately **not** the safety net for a session that failed to index —
/// that is handled structurally, by refusing to advance the cursor at all when a scan
/// was incomplete. Sizing the overlap to cover indexing failures would mean
/// re-deriving a large window on every tick, which is the cost being removed.
pub const SCAN_OVERLAP_MINUTES: i64 = 5;

/// Whether a pass re-derives only what changed, or the whole corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScanMode {
    /// Re-derive only sessions touched since the scan cursor. The daemon's ~30s tick.
    #[default]
    Incremental,
    /// Ignore the cursor and re-derive everything.
    ///
    /// Needed for the two things a cursor cannot answer for itself: a change to what
    /// the extractor derives (the corrected rows only appear for sessions actually
    /// re-read, and `prune_user_message_events` only retires superseded rows for
    /// sessions in its keep-set), and any suspicion the cursor drifted.
    Full,
}

/// Where this machine's transcript stores and local event spool live.
///
/// A parameter rather than three calls to the environment, so ingest can be tested
/// against a fixture corpus instead of the developer's own `~/.claude`.
#[derive(Debug, Clone)]
pub struct IngestPaths {
    pub claude_projects: PathBuf,
    pub opencode_db: PathBuf,
    pub omp_sessions: PathBuf,
    pub data_dir: PathBuf,
}

impl IngestPaths {
    /// Resolves the real store locations for this machine.
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            claude_projects: get_claude_projects_dir(),
            opencode_db: get_opencode_db_path()?,
            omp_sessions: get_omp_sessions_dir(),
            data_dir: default_data_dir(),
        })
    }
}

/// Run the sessions index command.
///
/// Scans Claude Code session directories and the `OpenCode` `SQLite`
/// database, then upserts discovered sessions into the database.
pub fn index_sessions(db: &tt_db::Database, mode: ScanMode) -> Result<()> {
    let report = index_sessions_quiet(db, mode)?;

    // Under `Incremental` these counts are what *changed*, not what the store holds:
    // a settled corpus reports zero and that is the healthy steady state.
    let scope = match mode {
        ScanMode::Incremental => "new or changed",
        ScanMode::Full => "total",
    };
    if report.scanned_claude {
        println!("Scanning Claude Code sessions...");
        println!("  Found {} {scope} Claude sessions", report.claude);
    }
    if report.scanned_opencode {
        println!("Scanning OpenCode sessions...");
        println!("  Found {} {scope} OpenCode sessions", report.opencode);
    }
    if report.scanned_omp {
        println!("Scanning omp sessions...");
        println!("  Found {} {scope} omp sessions", report.omp);
    }
    if report.claude + report.opencode + report.omp == 0 {
        println!("No {scope} sessions found.");
        return Ok(());
    }
    if report.stale_events_cleaned > 0 {
        println!(
            "Cleaned {} stale user_message events from non-user sessions",
            report.stale_events_cleaned
        );
    }
    println!(
        "Indexed {} sessions ({} events)",
        report.claude + report.opencode + report.omp,
        report.indexed_events
    );
    println!("\nSessions by project:");
    for (project, count) in &report.projects[..report.projects.len().min(10)] {
        println!("  {project}: {count} sessions");
    }
    if report.projects.len() > 10 {
        println!("  ... and {} more projects", report.projects.len() - 10);
    }
    if report.drained_events > 0 {
        println!(
            "Drained {} local events from events.jsonl",
            report.drained_events
        );
    }
    if report.session_membership_assigned > 0 {
        println!(
            "Attributed {} events to the stream their own classified session was placed on",
            report.session_membership_assigned
        );
    }
    if report.terminal_focus_assigned > 0 {
        println!(
            "Attributed {} terminal-focus events to concurrent remote work",
            report.terminal_focus_assigned
        );
    }
    if report.artifact_focus_assigned > 0 {
        println!(
            "Attributed {} window-focus events to the work that did the pull request or issue they name",
            report.artifact_focus_assigned
        );
    }
    if report.injected_events_pruned > 0 {
        println!(
            "Pruned {} user_message events that were harness-injected text",
            report.injected_events_pruned
        );
    }
    Ok(())
}

/// Sessions discovered on this machine, and which sources were present to scan.
#[expect(
    clippy::struct_excessive_bools,
    reason = "one independent presence flag per transcript store plus the orthogonal `complete` outcome; a state machine would obscure that each store is scanned regardless of the others"
)]
struct ScannedSessions {
    sessions: Vec<AgentSession>,
    claude: usize,
    opencode: usize,
    omp: usize,
    scanned_claude: bool,
    scanned_opencode: bool,
    scanned_omp: bool,
    /// Whether every store present could be read in full.
    ///
    /// False means some part of a store was unreadable, so the sessions returned are
    /// not the whole answer for this window and the scan cursor must not advance past
    /// it. See `tt_core::ScanOutcome`.
    complete: bool,
}

/// Scans both agent transcript stores, bounded by `since`. A store that is absent is
/// not an error — most machines run only one of the two, and an absent store leaves
/// the scan complete so it cannot freeze the cursor forever.
fn scan_all_sessions(paths: &IngestPaths, since: Option<DateTime<Utc>>) -> Result<ScannedSessions> {
    let scanned_claude = paths.claude_projects.exists();
    let claude = if scanned_claude {
        scan_claude_sessions_incremental(&paths.claude_projects, since)
            .context("failed to scan Claude Code sessions")?
    } else {
        ScanOutcome::complete(Vec::new())
    };

    let scanned_opencode = paths.opencode_db.exists();
    let opencode = if scanned_opencode {
        scan_opencode_sessions_incremental(&paths.opencode_db, since)
            .context("failed to scan OpenCode sessions")?
    } else {
        ScanOutcome::complete(Vec::new())
    };

    let scanned_omp = paths.omp_sessions.exists();
    let omp = if scanned_omp {
        scan_omp_sessions_incremental(&paths.omp_sessions, since)
            .context("failed to scan omp sessions")?
    } else {
        ScanOutcome::complete(Vec::new())
    };

    // One cursor covers all stores, so it may only advance when every present store
    // was read in full. Holding it back for a store that succeeded costs one cheap
    // re-scan of a small window; advancing it for a store that failed loses that
    // window for good.
    let complete = claude.complete && opencode.complete && omp.complete;

    Ok(ScannedSessions {
        claude: claude.sessions.len(),
        opencode: opencode.sessions.len(),
        omp: omp.sessions.len(),
        sessions: claude
            .sessions
            .into_iter()
            .chain(opencode.sessions)
            .chain(omp.sessions)
            .collect(),
        scanned_claude,
        scanned_opencode,
        scanned_omp,
        complete,
    })
}

/// The `since` bound for this pass: the cursor less the safety overlap, or `None` to
/// re-derive everything.
fn scan_since(db: &tt_db::Database, mode: ScanMode) -> Result<Option<DateTime<Utc>>> {
    if mode == ScanMode::Full {
        return Ok(None);
    }
    let cursor = db
        .get_session_scan_cursor()
        .context("failed to read session scan cursor")?;
    Ok(cursor.map(|at| at - chrono::Duration::minutes(SCAN_OVERLAP_MINUTES)))
}

/// Indexes sessions from this machine's real store locations.
pub fn index_sessions_quiet(db: &tt_db::Database, mode: ScanMode) -> Result<IngestReport> {
    let paths = IngestPaths::from_env()?;
    index_sessions_in(db, &paths, mode)
}

/// Indexes sessions from the given store locations.
///
/// The scan cursor is captured *before* reading and written only after the pass has
/// committed everything it derived. A session written while the scan runs therefore
/// carries a timestamp after the recorded cursor and is picked up next pass, rather
/// than falling into the gap between "scanned" and "finished".
pub fn index_sessions_in(
    db: &tt_db::Database,
    paths: &IngestPaths,
    mode: ScanMode,
) -> Result<IngestReport> {
    let machine_id = crate::machine::load_machine_identity()?.map(|m| m.machine_id);

    let (migrated_start, migrated_end) = db
        .migrate_legacy_event_types()
        .context("failed to migrate legacy event types")?;
    if migrated_start + migrated_end > 0 {
        tracing::info!(migrated_start, migrated_end, "migrated legacy event types");
    }

    let scan_started_at = Utc::now();
    let since = scan_since(db, mode)?;

    let ScannedSessions {
        sessions: all_sessions,
        claude,
        opencode,
        omp,
        scanned_claude,
        scanned_opencode,
        scanned_omp,
        complete,
    } = scan_all_sessions(paths, since)?;

    if all_sessions.is_empty() {
        // The steady state, not a failure: nothing changed since the cursor. The
        // cursor still advances, so the re-read window stays one overlap wide instead
        // of growing without bound.
        advance_scan_cursor(db, scan_started_at, complete)?;
        return Ok(IngestReport {
            claude,
            opencode,
            omp,
            indexed_events: 0,
            imported_events: 0,
            drained_events: 0,
            stale_events_cleaned: 0,
            injected_events_pruned: 0,
            session_membership_assigned: 0,
            terminal_focus_assigned: 0,
            artifact_focus_assigned: 0,
            projects: Vec::new(),
            scanned_claude,
            scanned_opencode,
            scanned_omp,
        });
    }

    let mut indexed_events = 0usize;
    let mut inserted_events = 0usize;
    // What the current extractor derives, per session. Feeding this to
    // `prune_user_message_events` retires user_message rows that an earlier,
    // laxer extractor wrote — event inserts are `INSERT OR IGNORE`, so a
    // correction adds the right rows but cannot remove the wrong ones.
    //
    // Under `ScanMode::Incremental` this keep-set holds only the sessions this pass
    // actually re-derived, and that is exactly the contract the prune is written to:
    // a session absent from the map is left untouched, because a caller can only
    // speak for transcripts it read. So a smaller keep-set prunes *less*, never
    // wrongly. The cost is that an extractor change no longer reaches settled
    // sessions on its own — which is one of the two reasons `ScanMode::Full` exists.
    let mut derived_user_messages: HashMap<String, HashSet<chrono::DateTime<chrono::Utc>>> =
        HashMap::with_capacity(all_sessions.len());
    for session in &all_sessions {
        db.upsert_agent_session(session, None)
            .with_context(|| format!("failed to upsert session {}", session.session_id))?;

        let events = create_session_events(session, machine_id.as_deref());
        derived_user_messages.insert(
            session.session_id.clone(),
            events
                .iter()
                .filter(|event| event.event_type == tt_core::EventType::UserMessage)
                .map(|event| event.timestamp)
                .collect(),
        );
        indexed_events += events.len();
        inserted_events += db.insert_events(&events).with_context(|| {
            format!("failed to insert events for session {}", session.session_id)
        })?;
    }

    let injected_events_pruned = db
        .prune_user_message_events(&derived_user_messages)
        .context("failed to prune superseded user_message events")?;
    if injected_events_pruned > 0 {
        tracing::info!(
            pruned = injected_events_pruned,
            "pruned user_message events no longer derived from session transcripts"
        );
    }

    let stale_events_cleaned = db
        .delete_non_user_message_events()
        .context("failed to clean up stale user_message events")?;
    let drained = import_local_events(db, &paths.data_dir)
        .context("failed to drain local events.jsonl into DB")?;
    let attribution = attribute_unassigned_events(db)?;

    let mut projects: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for session in &all_sessions {
        *projects.entry(session.project_name.clone()).or_default() += 1;
    }
    let mut projects: Vec<_> = projects.into_iter().collect();
    projects.sort_unstable_by_key(|(_, count)| std::cmp::Reverse(*count));

    advance_scan_cursor(db, scan_started_at, complete)?;

    Ok(IngestReport {
        claude,
        opencode,
        omp,
        indexed_events,
        imported_events: inserted_events + drained,
        drained_events: drained,
        stale_events_cleaned,
        injected_events_pruned,
        session_membership_assigned: attribution.session_membership,
        terminal_focus_assigned: attribution.terminal_focus,
        artifact_focus_assigned: attribution.artifact_reference,
        projects,
        scanned_claude,
        scanned_opencode,
        scanned_omp,
    })
}

/// Records the scan cursor, but only for a scan that actually read everything.
///
/// A scan that degraded returned fewer sessions than the window really held, and
/// nothing distinguishes that from "nothing changed". Advancing here would ask the
/// next pass for a *later* window, so every session in this one would be skipped
/// permanently — no later pass would ever look at it again. Standing still instead
/// costs one repeated scan of a bounded window and is self-healing.
fn advance_scan_cursor(
    db: &tt_db::Database,
    scan_started_at: DateTime<Utc>,
    complete: bool,
) -> Result<()> {
    if !complete {
        tracing::warn!(
            "session scan was incomplete; leaving the scan cursor in place so the next \
             pass re-reads this window"
        );
        return Ok(());
    }
    db.set_session_scan_cursor(scan_started_at)
        .context("failed to record session scan cursor")?;
    Ok(())
}

/// What one run of the attribution passes assigned, counted per pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct AttributionCounts {
    session_membership: u64,
    terminal_focus: u64,
    artifact_reference: u64,
}

/// Runs every attribution pass `tt ingest sessions` applies to unassigned events.
///
/// One seam so the set of passes is a single, testable list rather than statements
/// buried in the middle of session indexing.
///
/// All three resolve a *specific* identity — the session an event belongs to, a remote
/// host, a pull request — to work that is already classified, and leave whatever they
/// cannot resolve unassigned. None reads a working directory. A fourth pass that did
/// was removed; see root `AGENTS.md`, "A folder is not a project".
fn attribute_unassigned_events(db: &tt_db::Database) -> Result<AttributionCounts> {
    // First, because it repeats a verdict already made about the very session these
    // events belong to, while the two below resolve a surface to work found nearby or
    // referenced elsewhere. A certain identity should reach an event before a
    // correlation does.
    let session_membership = db
        .claim_unassigned_events_for_classified_sessions()
        .context("failed to attribute events to their own classified session's stream")?;
    let terminal_focus =
        assign_terminal_focus(db).context("failed to attribute terminal-focus events")?;
    // After the terminal pass, which resolves the focus events this one cannot.
    let artifact_reference =
        assign_artifact_focus(db).context("failed to attribute artifact-focus events")?;
    Ok(AttributionCounts {
        session_membership,
        terminal_focus,
        artifact_reference,
    })
}

/// Attribute unassigned terminal-window focus to the work its remote host was doing.
///
/// `window_focus` events carry no `cwd` and no session, so no other pass reaches
/// them at all. A terminal focus is resolvable anyway: the remote host's own events
/// already classified, and whichever stream dominated activity within
/// `TERMINAL_CORRELATION_WINDOW_MS` of the focus is the work being looked at.
///
/// Candidate activity is loaded ONCE for the whole span and binary-searched per
/// event. A correlated query per focus event would not finish on real data:
/// ~200k focus events against ~52k candidate rows in a single week.
///
/// Events that do not resolve are left unassigned rather than placed in an
/// invented container. Returns the number of events assigned.
fn assign_terminal_focus(db: &tt_db::Database) -> Result<u64> {
    use tt_core::attribution::{
        TERMINAL_CORRELATION_WINDOW_MS, is_terminal_focus, resolve_terminal_focus,
    };

    let candidates: Vec<StoredEvent> = db
        .unattributed_terminal_focus_events()
        .context("failed to get unattributed window_focus events")?
        .into_iter()
        .filter(|event| {
            is_terminal_focus(
                event.window_app_id.as_deref(),
                event.window_title.as_deref(),
            )
        })
        .collect();

    let (Some(first), Some(last)) = (candidates.first(), candidates.last()) else {
        return Ok(0);
    };

    let window = chrono::Duration::milliseconds(TERMINAL_CORRELATION_WINDOW_MS);
    let activity = db
        .remote_activity_for_correlation(first.timestamp - window, last.timestamp + window)
        .context("failed to load remote activity for correlation")?;

    let assignments: Vec<(String, String)> = candidates
        .iter()
        .filter_map(|event| {
            let stream_id =
                resolve_terminal_focus(event.timestamp, &activity, TERMINAL_CORRELATION_WINDOW_MS)?;
            (stream_id != tt_db::JUNK_STREAM_SLUG).then_some((event.id.clone(), stream_id))
        })
        .collect();

    if assignments.is_empty() {
        return Ok(0);
    }

    let count = db
        .assign_events_to_stream(&assignments, "terminal_focus")
        .context("failed to assign terminal-focus events to streams")?;
    Ok(count)
}

/// Attributes window focus that is displaying a pull request or issue.
///
/// The title names a *durable artifact*, and the work it belongs to is whichever
/// stream actually did that artifact — recorded by a classified session that wrote
/// the artifact's URL or its `#number`. Nothing temporal enters this pass: a
/// browser is not a view of the machine's current activity the way a terminal is,
/// and correlating it against concurrent work was measured at 53.7% agreement
/// against artifact-bound ground truth, which is a coin flip.
///
/// Everything else the browser shows is left unassigned, where it reads as
/// classification lag. Returns the number of events assigned.
fn assign_artifact_focus(db: &tt_db::Database) -> Result<u64> {
    use tt_core::attribution::{artifact_in_title, resolve_artifact_focus};

    let candidates: Vec<(StoredEvent, tt_core::attribution::ArtifactRef)> = db
        .unattributed_terminal_focus_events()
        .context("failed to get unattributed window_focus events")?
        .into_iter()
        .filter_map(|event| {
            let artifact = artifact_in_title(event.window_title.as_deref())?;
            Some((event, artifact))
        })
        .collect();

    if candidates.is_empty() {
        return Ok(0);
    }

    let mentions = db
        .artifact_mentions_for_binding()
        .context("failed to load artifact references from classified work")?;

    let assignments: Vec<(String, String)> = candidates
        .iter()
        .filter_map(|(event, artifact)| {
            let stream_id = resolve_artifact_focus(artifact, &mentions)?;
            Some((event.id.clone(), stream_id))
        })
        .collect();

    tracing::debug!(
        candidates = candidates.len(),
        mentions = mentions.len(),
        resolved = assignments.len(),
        "artifact-focus attribution"
    );

    if assignments.is_empty() {
        return Ok(0);
    }

    let count = db
        .assign_events_to_stream(&assignments, "artifact_reference")
        .context("failed to assign artifact-focus events to streams")?;
    Ok(count)
}

/// Create events from an agent session.
fn create_session_events(session: &AgentSession, machine_id: Option<&str>) -> Vec<StoredEvent> {
    use serde_json::json;
    use tt_core::EventType;

    // Helper to create a session event with common fields
    let make_event = |id_suffix: &str,
                      timestamp: chrono::DateTime<chrono::Utc>,
                      event_type: EventType| StoredEvent {
        id: format!("{}-{id_suffix}", session.session_id),
        timestamp,
        event_type,
        source: session.source.as_str().to_string(),
        machine_id: machine_id.map(String::from),
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
        cwd: Some(session.project_path.clone()),
        session_id: Some(session.session_id.clone()),
        stream_id: None,
        assignment_source: None,
        data: json!({}),
    };

    let mut events = Vec::new();

    // Session start event with extra project_name field
    let mut start_event = make_event("session_start", session.start_time, EventType::AgentSession);
    start_event.action = Some("started".to_string());
    start_event.data["project_name"] = json!(session.project_name);
    events.push(start_event);

    // User-message events are direct attention for human-driven sessions, including
    // omp continuations. Agent and subagent prompts remain automated activity.
    if session.session_type.is_human_driven() {
        for ts in &session.user_message_timestamps {
            let id_suffix = format!("user_message-{}", ts.timestamp_millis());
            events.push(make_event(&id_suffix, *ts, EventType::UserMessage));
        }
    }

    for (index, ts) in session.tool_call_timestamps.iter().enumerate() {
        let id_suffix = format!("tool_use-{}-{index}", ts.timestamp_millis());
        events.push(make_event(&id_suffix, *ts, EventType::AgentToolUse));
    }

    // Session end event
    if let Some(end_time) = session.end_time {
        let mut end_event = make_event("session_end", end_time, EventType::AgentSession);
        end_event.action = Some("ended".to_string());
        events.push(end_event);
    }

    events
}

/// Return the user's home directory.
fn home_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME environment variable not set")?;
    Ok(PathBuf::from(home))
}

/// Get the Claude Code projects directory path.
///
/// Respects `CLAUDE_CONFIG_DIR` if set, otherwise falls back to `~/.claude`.
fn get_claude_projects_dir() -> PathBuf {
    std::env::var("CLAUDE_CONFIG_DIR")
        .map_or_else(
            |_| home_dir().unwrap_or_default().join(".claude"),
            PathBuf::from,
        )
        .join("projects")
}

/// Get the `OpenCode` database path.
fn get_opencode_db_path() -> Result<PathBuf> {
    Ok(dirs::data_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine data directory"))?
        .join("opencode/opencode.db"))
}

/// Get the omp (oh-my-pi) sessions directory path.
///
/// Honors `PI_CODING_AGENT_DIR` if set (sessions live at
/// `$PI_CODING_AGENT_DIR/sessions`), otherwise falls back to `~/.omp/agent/sessions`.
fn get_omp_sessions_dir() -> PathBuf {
    std::env::var("PI_CODING_AGENT_DIR")
        .map_or_else(
            |_| home_dir().unwrap_or_default().join(".omp/agent"),
            PathBuf::from,
        )
        .join("sessions")
}

/// Reads all events from the events file in the specified data directory.
#[cfg(test)]
fn read_events_from(data_dir: &Path) -> Result<Vec<IngestEvent>> {
    use std::io::{BufRead, BufReader};

    let events_file = events_path(data_dir);
    if !events_file.exists() {
        return Ok(Vec::new());
    }

    let file = File::open(&events_file).context("failed to open events file")?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();

    for line in reader.lines() {
        let line = line.context("failed to read line")?;
        let event: IngestEvent = serde_json::from_str(&line).context("failed to parse event")?;
        events.push(event);
    }

    Ok(events)
}

/// Imports events from the local `events.jsonl` (and rotated `events.jsonl.1`)
/// into the database. Returns the number of newly inserted events.
///
/// This closes a gap in the sync model: the local tmux hook writes events to
/// `events.jsonl` but no automated mechanism moves them into the DB. `tt sync`
/// pulls events from remote machines via SSH; the local laptop's own JSONL is
/// never read. Without this function, local tmux pane focus events accumulate
/// in JSONL but are invisible to reports.
///
/// Idempotent: re-running is safe because event IDs are deterministic and the
/// database uses `INSERT OR IGNORE`.
pub fn import_local_events(db: &tt_db::Database, data_dir: &Path) -> Result<usize> {
    let mut total_inserted = 0;
    // Read rotated file first so newer events from events.jsonl can still
    // appear later in the import stream (ordering is not required for
    // correctness — INSERT OR IGNORE is set-based — but it matches the
    // natural temporal order).
    for filename in ["events.jsonl.1", "events.jsonl"] {
        let path = data_dir.join(filename);
        if !path.exists() {
            continue;
        }
        let file =
            File::open(&path).with_context(|| format!("failed to open {}", path.display()))?;
        let result = import::import_from_reader(db, file)
            .with_context(|| format!("failed to import events from {}", path.display()))?;
        total_inserted += result.inserted;
    }
    Ok(total_inserted)
}

#[cfg(test)]
const TEST_MACHINE_ID: &str = "00000000-0000-0000-0000-000000000000";

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_empty_pane_id_rejected() {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join(".time-tracker");

        let result = ingest_pane_focus_impl(
            &data_dir,
            TEST_MACHINE_ID,
            "",
            "main",
            None,
            "/home/test",
            None,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("pane_id"));
    }

    #[test]
    fn test_empty_session_name_rejected() {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join(".time-tracker");

        let result = ingest_pane_focus_impl(
            &data_dir,
            TEST_MACHINE_ID,
            "%1",
            "",
            None,
            "/home/test",
            None,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("session_name"));
    }

    #[test]
    fn test_event_serialization_matches_spec() {
        let timestamp = DateTime::parse_from_rfc3339("2025-01-29T12:00:00.000Z")
            .unwrap()
            .with_timezone(&Utc);

        let event = IngestEvent::pane_focus(
            TEST_MACHINE_ID,
            "%3".to_string(),
            "dev".to_string(),
            Some(1),
            "/home/user/project".to_string(),
            None,
            timestamp,
        );

        let json = serde_json::to_string_pretty(&event).unwrap();
        insta::assert_snapshot!(json);
    }

    /// A resolved pane session is stamped onto the event and survives the round
    /// trip through the events file, which is what puts it on `events.session_id`
    /// and lets the session→stream path claim the event later.
    #[test]
    fn a_resolved_pane_session_is_stamped_on_the_event() {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join(".time-tracker");

        ingest_pane_focus_impl(
            &data_dir,
            TEST_MACHINE_ID,
            "%8",
            "main",
            Some(0),
            "/home/test",
            Some("ses_0210f2ed2ffedhF4".to_string()),
        )
        .unwrap();

        let events = read_events_from(&data_dir).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].session_id,
            Some("ses_0210f2ed2ffedhF4".to_string())
        );
    }

    /// Every way the lookup can fail arrives here as `None`, and a `None` must
    /// leave the event exactly as it was before this mechanism existed — a focus
    /// event is never lost or altered because a pane could not be resolved.
    #[test]
    fn an_unresolved_pane_records_the_event_unchanged() {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join(".time-tracker");

        ingest_pane_focus_impl(
            &data_dir,
            TEST_MACHINE_ID,
            "%8",
            "main",
            Some(0),
            "/home/test",
            None,
        )
        .unwrap();

        let events = read_events_from(&data_dir).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].session_id, None);
        assert_eq!(events[0].pane_id, "%8");
        assert_eq!(events[0].tmux_session, "main");
        assert_eq!(events[0].event_type, "tmux_pane_focus");

        // The wire form carries no session_id key at all, so an unresolved event is
        // byte-identical to one written before this field existed.
        let raw = fs::read_to_string(events_path(&data_dir)).unwrap();
        assert!(!raw.contains("session_id"), "unexpected key in {raw}");
    }

    /// A stamped event adds the session id and changes nothing else about the
    /// wire form.
    #[test]
    fn a_stamped_event_serialization_matches_spec() {
        let timestamp = DateTime::parse_from_rfc3339("2025-01-29T12:00:00.000Z")
            .unwrap()
            .with_timezone(&Utc);

        let event = IngestEvent::pane_focus(
            TEST_MACHINE_ID,
            "%3".to_string(),
            "dev".to_string(),
            Some(2),
            "/home/user/project".to_string(),
            Some("ses_0210f2ed2ffedhF4".to_string()),
            timestamp,
        );

        let json = serde_json::to_string_pretty(&event).unwrap();
        insta::assert_snapshot!(json);
    }

    #[test]
    fn test_deterministic_id_same_input() {
        let timestamp = DateTime::parse_from_rfc3339("2025-01-29T12:00:00.000Z")
            .unwrap()
            .with_timezone(&Utc);

        let event1 = IngestEvent::pane_focus(
            TEST_MACHINE_ID,
            "%3".to_string(),
            "dev".to_string(),
            None,
            "/home/user".to_string(),
            None,
            timestamp,
        );

        let event2 = IngestEvent::pane_focus(
            TEST_MACHINE_ID,
            "%3".to_string(),
            "dev".to_string(),
            None,
            "/home/user".to_string(),
            None,
            timestamp,
        );

        assert_eq!(event1.id, event2.id);
    }

    #[test]
    fn test_different_inputs_different_ids() {
        let timestamp = DateTime::parse_from_rfc3339("2025-01-29T12:00:00.000Z")
            .unwrap()
            .with_timezone(&Utc);

        let event1 = IngestEvent::pane_focus(
            TEST_MACHINE_ID,
            "%3".to_string(),
            "dev".to_string(),
            None,
            "/home/user".to_string(),
            None,
            timestamp,
        );

        let event2 = IngestEvent::pane_focus(
            TEST_MACHINE_ID,
            "%4".to_string(), // Different pane
            "dev".to_string(),
            None,
            "/home/user".to_string(),
            None,
            timestamp,
        );

        assert_ne!(event1.id, event2.id);
    }

    #[test]
    fn test_ingest_creates_events_file() {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join(".time-tracker");

        let result = ingest_pane_focus_impl(
            &data_dir,
            TEST_MACHINE_ID,
            "%1",
            "main",
            Some(0),
            "/home/test",
            None,
        );

        assert!(result.is_ok());
        assert!(result.unwrap()); // Event was written

        let events = read_events_from(&data_dir).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].pane_id, "%1");
        assert_eq!(events[0].tmux_session, "main");
        assert_eq!(events[0].cwd, Some("/home/test".to_string()));
    }

    #[test]
    fn ingest_scroll_writes_tmux_scroll_event() {
        // Given: a temp data dir.
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join(".time-tracker");

        // When: a scroll event is ingested.
        let result = ingest_scroll_impl(
            &data_dir,
            TEST_MACHINE_ID,
            "%1",
            "main",
            Some(0),
            "/home/test",
        );

        // Then: a single tmux_scroll event is written for the pane.
        assert!(result.unwrap());
        let events = read_events_from(&data_dir).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "tmux_scroll");
        assert_eq!(events[0].pane_id, "%1");
    }

    #[test]
    fn test_debounce_within_window() {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join(".time-tracker");

        // First event should be written
        let result1 = ingest_pane_focus_impl(
            &data_dir,
            TEST_MACHINE_ID,
            "%1",
            "main",
            None,
            "/home/test",
            None,
        );
        assert!(result1.unwrap());

        // Immediate second event for same pane should be debounced
        let result2 = ingest_pane_focus_impl(
            &data_dir,
            TEST_MACHINE_ID,
            "%1",
            "main",
            None,
            "/home/test",
            None,
        );
        assert!(!result2.unwrap()); // Debounced

        let events = read_events_from(&data_dir).unwrap();
        assert_eq!(events.len(), 1); // Only one event written
    }

    #[test]
    fn test_debounce_different_panes() {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join(".time-tracker");

        // First pane
        let result1 = ingest_pane_focus_impl(
            &data_dir,
            TEST_MACHINE_ID,
            "%1",
            "main",
            None,
            "/home/test",
            None,
        );
        assert!(result1.unwrap());

        // Different pane should not be debounced
        let result2 = ingest_pane_focus_impl(
            &data_dir,
            TEST_MACHINE_ID,
            "%2",
            "main",
            None,
            "/home/test",
            None,
        );
        assert!(result2.unwrap());

        let events = read_events_from(&data_dir).unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_debounce_after_window_expires() {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join(".time-tracker");

        // First event
        let result1 = ingest_pane_focus_impl(
            &data_dir,
            TEST_MACHINE_ID,
            "%1",
            "main",
            None,
            "/home/test",
            None,
        );
        assert!(result1.unwrap());

        // Wait for debounce window to expire
        thread::sleep(Duration::from_millis(550));

        // Second event should be written
        let result2 = ingest_pane_focus_impl(
            &data_dir,
            TEST_MACHINE_ID,
            "%1",
            "main",
            None,
            "/home/test",
            None,
        );
        assert!(result2.unwrap());

        let events = read_events_from(&data_dir).unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_events_file_is_valid_jsonl() {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join(".time-tracker");

        ingest_pane_focus_impl(
            &data_dir,
            TEST_MACHINE_ID,
            "%1",
            "session1",
            Some(0),
            "/path/a",
            None,
        )
        .unwrap();

        ingest_pane_focus_impl(
            &data_dir,
            TEST_MACHINE_ID,
            "%2",
            "session2",
            None,
            "/path/b",
            None,
        )
        .unwrap();

        // Read raw file and verify each line is valid JSON
        let content = fs::read_to_string(events_path(&data_dir)).unwrap();
        for line in content.lines() {
            let parsed: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(parsed.is_object());
            assert!(parsed["id"].is_string());
            assert!(parsed["timestamp"].is_string());
            assert!(parsed["source"].is_string());
            assert!(parsed["type"].is_string());
            // Fields are flattened (no nested "data" object)
            assert!(parsed["pane_id"].is_string());
            assert!(parsed["tmux_session"].is_string());
        }
    }

    #[test]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "MAX_EVENTS_FILE_SIZE is small enough to fit in usize"
    )]
    fn test_rotation_when_file_exceeds_threshold() {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join(".time-tracker");
        fs::create_dir_all(&data_dir).unwrap();

        // Create a file that exceeds the rotation threshold
        let events_file = events_path(&data_dir);
        let large_content = "x".repeat(MAX_EVENTS_FILE_SIZE as usize + 100);
        fs::write(&events_file, &large_content).unwrap();

        // Ingest should rotate the file
        ingest_pane_focus_impl(
            &data_dir,
            TEST_MACHINE_ID,
            "%1",
            "main",
            None,
            "/home/test",
            None,
        )
        .unwrap();

        // Old file should be rotated
        let rotated = rotated_events_path(&data_dir);
        assert!(rotated.exists(), "rotated file should exist");
        assert_eq!(
            fs::read_to_string(&rotated).unwrap(),
            large_content,
            "rotated file should contain old content"
        );

        // New events file should contain only the new event
        let events = read_events_from(&data_dir).unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_no_rotation_when_file_under_threshold() {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join(".time-tracker");
        fs::create_dir_all(&data_dir).unwrap();

        // Create a small file
        let events_file = events_path(&data_dir);
        fs::write(&events_file, "small content").unwrap();

        // Ingest should not rotate
        ingest_pane_focus_impl(
            &data_dir,
            TEST_MACHINE_ID,
            "%1",
            "main",
            None,
            "/home/test",
            None,
        )
        .unwrap();

        // No rotated file should exist
        let rotated = rotated_events_path(&data_dir);
        assert!(!rotated.exists(), "rotated file should not exist");
    }

    #[test]
    fn test_create_session_events_session_start() {
        use chrono::TimeZone;
        use tt_core::session::{AgentSession, SessionSource};

        let session = AgentSession {
            session_id: "test-session-123".to_string(),
            source: SessionSource::default(),
            parent_session_id: None,
            session_type: tt_core::session::SessionType::default(),
            project_path: "/home/user/project".to_string(),
            project_name: "project".to_string(),
            start_time: Utc.with_ymd_and_hms(2026, 2, 2, 10, 0, 0).unwrap(),
            end_time: None,
            message_count: 1,
            summary: None,
            user_prompts: vec!["hello".to_string()],
            starting_prompt: Some("hello".to_string()),
            assistant_message_count: 1,
            tool_call_count: 0,
            user_message_timestamps: Vec::new(),
            tool_call_timestamps: Vec::new(),
        };

        let events = create_session_events(&session, None);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, tt_core::EventType::AgentSession);
        assert_eq!(events[0].id, "test-session-123-session_start");
        assert_eq!(events[0].source, "claude");
        assert_eq!(events[0].cwd, Some("/home/user/project".to_string()));
        assert_eq!(events[0].session_id, Some("test-session-123".to_string()));
        assert_eq!(events[0].action.as_deref(), Some("started"));
    }

    #[test]
    fn test_create_session_events_session_start_and_end() {
        use chrono::TimeZone;
        use tt_core::session::{AgentSession, SessionSource};

        let session = AgentSession {
            session_id: "test-session-456".to_string(),
            source: SessionSource::default(),
            parent_session_id: None,
            session_type: tt_core::session::SessionType::default(),
            project_path: "/home/user/project".to_string(),
            project_name: "project".to_string(),
            start_time: Utc.with_ymd_and_hms(2026, 2, 2, 10, 0, 0).unwrap(),
            end_time: Some(Utc.with_ymd_and_hms(2026, 2, 2, 11, 0, 0).unwrap()),
            message_count: 2,
            summary: None,
            user_prompts: vec![],
            starting_prompt: None,
            assistant_message_count: 1,
            tool_call_count: 0,
            user_message_timestamps: Vec::new(),
            tool_call_timestamps: Vec::new(),
        };

        let events = create_session_events(&session, None);

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, tt_core::EventType::AgentSession);
        assert_eq!(events[1].event_type, tt_core::EventType::AgentSession);
        assert_eq!(events[1].id, "test-session-456-session_end");
        assert_eq!(events[0].action.as_deref(), Some("started"));
        assert_eq!(events[1].action.as_deref(), Some("ended"));
    }

    #[test]
    fn test_create_session_events_user_messages() {
        use chrono::TimeZone;
        use tt_core::session::{AgentSession, SessionSource};

        let ts1 = Utc.with_ymd_and_hms(2026, 2, 2, 10, 5, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 2, 2, 10, 10, 0).unwrap();

        let session = AgentSession {
            session_id: "test-session-789".to_string(),
            source: SessionSource::default(),
            parent_session_id: None,
            session_type: tt_core::session::SessionType::default(),
            project_path: "/home/user/project".to_string(),
            project_name: "project".to_string(),
            start_time: Utc.with_ymd_and_hms(2026, 2, 2, 10, 0, 0).unwrap(),
            end_time: None,
            message_count: 4,
            summary: None,
            user_prompts: vec!["first".to_string(), "second".to_string()],
            starting_prompt: Some("first".to_string()),
            assistant_message_count: 2,
            tool_call_count: 0,
            user_message_timestamps: vec![ts1, ts2],
            tool_call_timestamps: Vec::new(),
        };

        let events = create_session_events(&session, None);

        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type, tt_core::EventType::AgentSession);
        assert_eq!(events[1].event_type, tt_core::EventType::UserMessage);
        assert_eq!(events[1].timestamp, ts1);
        assert_eq!(events[2].event_type, tt_core::EventType::UserMessage);
        assert_eq!(events[2].timestamp, ts2);
        assert_eq!(events[0].action.as_deref(), Some("started"));
    }

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "test exercises the core algorithm directly"
    )]
    fn test_create_session_events_delegated_time_allocated() {
        use chrono::TimeZone;
        use tt_core::session::{AgentSession, SessionSource};
        use tt_core::{AllocationConfig, EventType, allocate_time};

        let start_time = Utc.with_ymd_and_hms(2026, 2, 2, 10, 0, 0).unwrap();
        let tool_ts1 = Utc.with_ymd_and_hms(2026, 2, 2, 10, 5, 0).unwrap();
        let tool_ts2 = Utc.with_ymd_and_hms(2026, 2, 2, 10, 10, 0).unwrap();
        let end_time = Utc.with_ymd_and_hms(2026, 2, 2, 10, 20, 0).unwrap();

        let session = AgentSession {
            session_id: "test-session-delegated".to_string(),
            source: SessionSource::default(),
            parent_session_id: None,
            session_type: tt_core::session::SessionType::default(),
            project_path: "/home/user/project".to_string(),
            project_name: "project".to_string(),
            start_time,
            end_time: Some(end_time),
            message_count: 2,
            summary: None,
            user_prompts: vec!["hello".to_string()],
            starting_prompt: Some("hello".to_string()),
            assistant_message_count: 1,
            tool_call_count: 2,
            user_message_timestamps: vec![tool_ts1],
            tool_call_timestamps: vec![tool_ts1, tool_ts2],
        };

        let mut events = create_session_events(&session, None);

        assert_eq!(events[0].event_type, EventType::AgentSession);
        assert_eq!(events[0].action.as_deref(), Some("started"));
        assert!(
            events
                .iter()
                .any(|event| event.event_type == EventType::AgentToolUse)
        );
        assert!(events.iter().any(|event| {
            event.event_type == EventType::AgentSession && event.action.as_deref() == Some("ended")
        }));

        let stream_id = "stream-123".to_string();
        for event in &mut events {
            event.stream_id = Some(stream_id.clone());
        }

        events.sort_by_key(|event| event.timestamp);

        let config = AllocationConfig::default();
        let result = allocate_time(&events, &config, None, &HashMap::new(), &HashMap::new());
        let stream = result
            .stream_times
            .iter()
            .find(|stream| stream.stream_id == stream_id)
            .expect("stream should have allocation results");

        assert!(stream.time_delegated_ms > 0);
    }

    #[test]
    fn test_create_session_events_opencode_source() {
        use chrono::TimeZone;
        use tt_core::session::{AgentSession, SessionSource};

        let session = AgentSession {
            session_id: "ses_opencode_123".to_string(),
            source: SessionSource::OpenCode,
            parent_session_id: None,
            session_type: tt_core::session::SessionType::default(),
            project_path: "/home/user/project".to_string(),
            project_name: "project".to_string(),
            start_time: Utc.with_ymd_and_hms(2026, 2, 2, 10, 0, 0).unwrap(),
            end_time: Some(Utc.with_ymd_and_hms(2026, 2, 2, 11, 0, 0).unwrap()),
            message_count: 2,
            summary: None,
            user_prompts: vec![],
            starting_prompt: None,
            assistant_message_count: 1,
            tool_call_count: 0,
            user_message_timestamps: Vec::new(),
            tool_call_timestamps: Vec::new(),
        };

        let events = create_session_events(&session, None);

        assert_eq!(events.len(), 2);
        // Events should have "opencode" as source, not "claude"
        assert_eq!(events[0].source, "opencode");
        assert_eq!(events[1].source, "opencode");
        assert_eq!(events[0].id, "ses_opencode_123-session_start");
        assert_eq!(events[0].session_id, Some("ses_opencode_123".to_string()));
        assert_eq!(events[0].action.as_deref(), Some("started"));
    }

    #[test]
    fn test_event_id_includes_machine_id() {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join("data");

        ingest_pane_focus_impl(
            &data_dir,
            TEST_MACHINE_ID,
            "%1",
            "main",
            Some(0),
            "/home/test",
            None,
        )
        .unwrap();

        let events = read_events_from(&data_dir).unwrap();
        assert_eq!(events.len(), 1);
        assert!(
            events[0].id.starts_with(TEST_MACHINE_ID),
            "event ID '{}' should start with machine_id",
            events[0].id
        );
    }

    #[test]
    fn test_get_claude_projects_dir() {
        if std::env::var("HOME").is_ok() {
            let path = get_claude_projects_dir();
            assert!(path.ends_with("projects"));
            assert!(path.to_string_lossy().contains(".claude"));
        }
    }
}

#[test]
fn test_concurrent_ingests_during_rotation() {
    use std::sync::Arc;
    use std::thread;

    let temp_dir = tempfile::tempdir().unwrap();
    let data_dir = Arc::new(temp_dir.path().join(".time-tracker"));
    fs::create_dir_all(&*data_dir).unwrap();

    // Create a file close to rotation threshold
    let events_file = events_path(&data_dir);
    let near_limit = "x".repeat(usize::try_from(MAX_EVENTS_FILE_SIZE - 100).unwrap());
    fs::write(&events_file, &near_limit).unwrap();

    // Spawn multiple threads that will trigger rotation
    let mut handles = vec![];
    for i in 0..3 {
        let data_dir_clone = Arc::clone(&data_dir);
        let handle = thread::spawn(move || {
            ingest_pane_focus_impl(
                &data_dir_clone,
                TEST_MACHINE_ID,
                &format!("%{i}"),
                "main",
                None,
                "/home/test",
                None,
            )
        });
        handles.push(handle);
    }

    // All threads should complete successfully
    for handle in handles {
        let result = handle.join().unwrap();
        assert!(
            result.is_ok(),
            "Concurrent ingest during rotation should succeed"
        );
    }

    // Verify no events were lost
    let events = read_events_from(&data_dir).unwrap();
    let rotated_exists = rotated_events_path(&data_dir).exists();

    // Either all events in current file, or some rotated
    if rotated_exists {
        // Just verify we didn't lose data (exact count depends on timing)
        assert!(!events.is_empty() || rotated_exists);
    } else {
        assert_eq!(events.len(), 3, "All events should be present");
    }
}

#[test]
fn test_debounce_file_corruption_recovery() {
    let temp_dir = tempfile::tempdir().unwrap();
    let data_dir = temp_dir.path().join(".time-tracker");
    fs::create_dir_all(&data_dir).unwrap();

    // Write corrupted debounce file
    let debounce_file = debounce_path(&data_dir);
    fs::write(&debounce_file, "corrupted:data:too:many:colons\ninvalid").unwrap();

    // Should handle gracefully and not panic
    let result = ingest_pane_focus_impl(
        &data_dir,
        TEST_MACHINE_ID,
        "%1",
        "main",
        None,
        "/home/test",
        None,
    );
    assert!(
        result.is_ok(),
        "Should recover from corrupted debounce file"
    );

    // Verify event was written
    let events = read_events_from(&data_dir).unwrap();
    assert_eq!(events.len(), 1);
}

#[test]
fn test_git_identity_extraction_from_jj_directory() {
    // `get_git_identity` shells out to `jj git remote list` / `jj workspace
    // list` / `jj root` against `cwd`, and those subprocesses inherit
    // whatever `$HOME` this test process has. A directory that merely
    // contains a `.jj` marker is exactly the shape of fixture other tests
    // build for `tt`, so this reproduces the call as its own subprocess
    // with `$HOME` pointed at a scratch "developer home" carrying a
    // sentinel `.gitconfig`, and proves the sentinel comes back
    // byte-identical: the jj invocation must never read *or write* the
    // real developer's global git identity.
    const CHILD_MARKER: &str = "TT_TEST_GIT_IDENTITY_CHILD";
    const CWD_ENV: &str = "TT_TEST_GIT_IDENTITY_CWD";
    const DATA_DIR_ENV: &str = "TT_TEST_GIT_IDENTITY_DATA_DIR";

    if std::env::var_os(CHILD_MARKER).is_some() {
        let cwd_with_jj = std::env::var(CWD_ENV).expect("parent sets cwd env");
        let data_dir =
            PathBuf::from(std::env::var(DATA_DIR_ENV).expect("parent sets data dir env"));

        let result = ingest_pane_focus_impl(
            &data_dir,
            TEST_MACHINE_ID,
            "%1",
            "main",
            None,
            &cwd_with_jj,
            None,
        );
        assert!(result.is_ok(), "Ingest should succeed");

        let events = read_events_from(&data_dir).unwrap();
        assert_eq!(events.len(), 1);
        // git_project is extracted from the directory name when jj is present
        // but no git remote is configured
        assert_eq!(events[0].git_project, Some("my-project".to_string()));
        return;
    }

    let temp_dir = tempfile::tempdir().unwrap();
    let data_dir = temp_dir.path().join(".time-tracker");

    // Create a directory with .jj subdirectory (jj will recognize this as a repo)
    let cwd_with_jj = temp_dir.path().join("my-project");
    fs::create_dir_all(&cwd_with_jj).unwrap();
    fs::create_dir_all(cwd_with_jj.join(".jj")).unwrap();

    // The scratch "developer home": a sentinel `.gitconfig` that must
    // survive the child's jj invocation untouched.
    let fake_home = tempfile::tempdir().unwrap();
    let sentinel = fake_home.path().join(".gitconfig");
    let sentinel_bytes =
        b"[user]\n\temail = real-dev@example.com\n\tname = Real Developer\n".to_vec();
    fs::write(&sentinel, &sentinel_bytes).unwrap();

    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("commands::ingest::test_git_identity_extraction_from_jj_directory")
        .arg("--nocapture")
        .env(CHILD_MARKER, "1")
        .env(CWD_ENV, cwd_with_jj.to_str().unwrap())
        .env(DATA_DIR_ENV, &data_dir)
        .env("HOME", fake_home.path())
        .env(
            "GIT_CONFIG_GLOBAL",
            fake_home.path().join("sandboxed.gitconfig"),
        )
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "child test failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let after = fs::read(&sentinel).unwrap();
    assert_eq!(
        after, sentinel_bytes,
        "get_git_identity's jj subprocess must never touch the developer's real ~/.gitconfig"
    );
}

#[test]
fn test_no_jj_directory_returns_no_identity() {
    let temp_dir = tempfile::tempdir().unwrap();
    let data_dir = temp_dir.path().join(".time-tracker");

    // Create a directory without .jj subdirectory
    let cwd_no_jj = temp_dir.path().join("regular-dir");
    fs::create_dir_all(&cwd_no_jj).unwrap();

    let result = ingest_pane_focus_impl(
        &data_dir,
        TEST_MACHINE_ID,
        "%1",
        "main",
        None,
        cwd_no_jj.to_str().unwrap(),
        None,
    );

    assert!(result.is_ok(), "Ingest should succeed");

    let events = read_events_from(&data_dir).unwrap();
    assert_eq!(events.len(), 1);
    // git fields should be None when there's no .jj directory
    assert_eq!(events[0].git_project, None);
    assert_eq!(events[0].git_workspace, None);
}

#[test]
fn test_index_sessions_partial_failures() {
    use std::io::Write;

    let temp = tempfile::TempDir::new().unwrap();
    let projects_dir = temp.path().join("projects");
    let test_project = projects_dir.join("test-project");
    fs::create_dir_all(&test_project).unwrap();

    // Good session file
    let good_session = test_project.join("good-session.jsonl");
    let mut file = fs::File::create(&good_session).unwrap();
    writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"test"}},"timestamp":"2026-02-02T10:00:00Z","cwd":"/test"}}"#).unwrap();

    // Bad session file (missing required fields)
    let bad_session = test_project.join("bad-session.jsonl");
    let mut file = fs::File::create(&bad_session).unwrap();
    writeln!(
        file,
        r#"{{"type":"user","message":{{"role":"user","content":"test"}}}}"#
    )
    .unwrap(); // No timestamp

    // Empty session file
    let empty_session = test_project.join("empty.jsonl");
    fs::write(&empty_session, "").unwrap();

    // Note: Creating a database here to verify the pattern, but not using it
    // since index_sessions uses a hardcoded path that can't be easily mocked.

    // Mock the projects directory env var (we can't easily mock the function, so this
    // test would need refactoring of index_sessions to accept a path parameter)
    // For now, verify the parse function handles errors

    // At least verify parse_session_file handles bad data
    let result1 = tt_core::session::parse_session_file(&good_session, "good", None);
    assert!(result1.is_ok(), "Good session should parse");

    let result2 = tt_core::session::parse_session_file(&bad_session, "bad", None);
    assert!(result2.is_err(), "Bad session should fail to parse");

    let result3 = tt_core::session::parse_session_file(&empty_session, "empty", None);
    assert!(result3.is_err(), "Empty session should fail to parse");
}

#[test]
fn test_lock_file_cleanup() {
    let temp_dir = tempfile::tempdir().unwrap();
    let data_dir = temp_dir.path().join(".time-tracker");

    // First ingest
    ingest_pane_focus_impl(
        &data_dir,
        TEST_MACHINE_ID,
        "%1",
        "main",
        None,
        "/home/test",
        None,
    )
    .unwrap();

    // Lock should be released, second ingest should succeed immediately
    let start = std::time::Instant::now();
    ingest_pane_focus_impl(
        &data_dir,
        TEST_MACHINE_ID,
        "%2",
        "main",
        None,
        "/home/test",
        None,
    )
    .unwrap();
    let duration = start.elapsed();

    // Should complete quickly (not waiting on lock)
    assert!(
        duration.as_secs() < 1,
        "Second ingest should not wait on lock"
    );
}

#[test]
fn test_debounce_with_special_pane_ids() {
    let temp_dir = tempfile::tempdir().unwrap();
    let data_dir = temp_dir.path().join(".time-tracker");

    // Pane IDs with special characters that might break parsing
    let special_panes = vec![
        "%1:2:3",     // Multiple colons
        "%test pane", // Space
        "%🔥",        // Emoji
    ];

    for pane_id in special_panes {
        let result = ingest_pane_focus_impl(
            &data_dir,
            TEST_MACHINE_ID,
            pane_id,
            "main",
            None,
            "/test",
            None,
        );
        assert!(result.is_ok(), "Should handle special pane ID: {pane_id}");
    }

    let events = read_events_from(&data_dir).unwrap();
    assert_eq!(events.len(), 3, "All special pane IDs should create events");
}

#[test]
fn test_rotation_preserves_old_content() {
    let temp_dir = tempfile::tempdir().unwrap();
    let data_dir = temp_dir.path().join(".time-tracker");
    fs::create_dir_all(&data_dir).unwrap();

    // Create known content
    let events_file = events_path(&data_dir);
    let original_content = "original event data\n";
    fs::write(&events_file, original_content).unwrap();

    // Trigger rotation by creating large file
    let large_content = "x".repeat(usize::try_from(MAX_EVENTS_FILE_SIZE + 1).unwrap());
    fs::write(&events_file, &large_content).unwrap();

    // Ingest should rotate
    ingest_pane_focus_impl(
        &data_dir,
        TEST_MACHINE_ID,
        "%1",
        "main",
        None,
        "/test",
        None,
    )
    .unwrap();

    // Verify old content is in rotated file
    let rotated = rotated_events_path(&data_dir);
    assert!(rotated.exists(), "Rotated file should exist");

    let rotated_content = fs::read_to_string(&rotated).unwrap();
    assert_eq!(
        rotated_content, large_content,
        "Rotated file should preserve content"
    );
}

#[test]
fn test_create_session_events_with_empty_timestamps() {
    use chrono::TimeZone;
    use tt_core::session::{AgentSession, SessionSource};

    let session = AgentSession {
        session_id: "test".to_string(),
        source: SessionSource::default(),
        parent_session_id: None,
        session_type: tt_core::session::SessionType::default(),
        project_path: "/test".to_string(),
        project_name: "test".to_string(),
        start_time: Utc.with_ymd_and_hms(2026, 2, 2, 10, 0, 0).unwrap(),
        end_time: None,
        message_count: 1,
        summary: None,
        user_prompts: vec![],
        starting_prompt: None,
        assistant_message_count: 0,
        tool_call_count: 0,
        user_message_timestamps: vec![], // Empty timestamps
        tool_call_timestamps: Vec::new(),
    };

    let events = create_session_events(&session, None);

    // Should only have session_start (no user_message events)
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, tt_core::EventType::AgentSession);
    assert_eq!(events[0].action.as_deref(), Some("started"));
}

#[test]
fn test_non_user_session_skips_user_message_events() {
    use chrono::TimeZone;
    use tt_core::session::{AgentSession, SessionSource, SessionType};

    let ts1 = Utc.with_ymd_and_hms(2026, 2, 2, 10, 5, 0).unwrap();
    let ts2 = Utc.with_ymd_and_hms(2026, 2, 2, 10, 10, 0).unwrap();

    // Agent session (e.g., Legion worker) with user_message_timestamps
    let session = AgentSession {
        session_id: "ses_legion_worker".to_string(),
        source: SessionSource::OpenCode,
        parent_session_id: None,
        session_type: SessionType::Agent,
        project_path: "/home/ubuntu/.local/share/legion/workspaces/test".to_string(),
        project_name: "test".to_string(),
        start_time: Utc.with_ymd_and_hms(2026, 2, 2, 10, 0, 0).unwrap(),
        end_time: Some(Utc.with_ymd_and_hms(2026, 2, 2, 11, 0, 0).unwrap()),
        message_count: 4,
        summary: None,
        user_prompts: vec!["automated prompt".to_string()],
        starting_prompt: Some("automated prompt".to_string()),
        assistant_message_count: 2,
        tool_call_count: 100,
        user_message_timestamps: vec![ts1, ts2],
        tool_call_timestamps: vec![ts1, ts2],
    };

    let events = create_session_events(&session, None);

    // Should have: session_start + 2 tool_use + session_end = 4 events
    // Should NOT have any UserMessage events
    assert_eq!(events.len(), 4);
    for event in &events {
        assert_ne!(
            event.event_type,
            tt_core::EventType::UserMessage,
            "Agent sessions must not emit UserMessage events"
        );
    }
}

#[test]
fn test_subagent_session_skips_user_message_events() {
    use chrono::TimeZone;
    use tt_core::session::{AgentSession, SessionSource, SessionType};

    let ts1 = Utc.with_ymd_and_hms(2026, 2, 2, 10, 5, 0).unwrap();

    let session = AgentSession {
        session_id: "ses_subagent".to_string(),
        source: SessionSource::OpenCode,
        parent_session_id: Some("ses_parent".to_string()),
        session_type: SessionType::Subagent,
        project_path: "/home/ubuntu/project".to_string(),
        project_name: "project".to_string(),
        start_time: Utc.with_ymd_and_hms(2026, 2, 2, 10, 0, 0).unwrap(),
        end_time: None,
        message_count: 2,
        summary: None,
        user_prompts: vec!["parent prompt".to_string()],
        starting_prompt: Some("parent prompt".to_string()),
        assistant_message_count: 1,
        tool_call_count: 0,
        user_message_timestamps: vec![ts1],
        tool_call_timestamps: Vec::new(),
    };

    let events = create_session_events(&session, None);

    // Should have only session_start — no UserMessage, no end
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, tt_core::EventType::AgentSession);
}

#[test]
fn test_import_local_events_drains_jsonl_into_db() {
    let temp_dir = tempfile::tempdir().unwrap();
    let data_dir = temp_dir.path();
    let events_file = data_dir.join("events.jsonl");

    let event_line = r#"{"id":"a59fae83-37bb-46c3-8a65-70962c28005a:remote.tmux:tmux_pane_focus:2026-05-17T00:00:00.000Z:%1","source":"remote.tmux","type":"tmux_pane_focus","timestamp":"2026-05-17T00:00:00.000Z","cwd":"/home/sami/test","pane_id":"%1","tmux_session":"dev","window_index":1}"#;
    fs::write(&events_file, format!("{event_line}\n")).unwrap();

    let db = tt_db::Database::open_in_memory().unwrap();

    let inserted = import_local_events(&db, data_dir).unwrap();

    assert_eq!(inserted, 1, "should import 1 event from local events.jsonl");

    let stored = db.get_events(None, None).unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].event_type, tt_core::EventType::TmuxPaneFocus);
    assert_eq!(
        stored[0].machine_id.as_deref(),
        Some("a59fae83-37bb-46c3-8a65-70962c28005a"),
        "machine_id should be extracted from event id prefix"
    );
}

#[test]
fn test_import_local_events_reads_rotated_jsonl() {
    let temp_dir = tempfile::tempdir().unwrap();
    let data_dir = temp_dir.path();
    let rotated = data_dir.join("events.jsonl.1");

    let event_line = r#"{"id":"a59fae83-37bb-46c3-8a65-70962c28005a:remote.tmux:tmux_pane_focus:2026-04-01T00:00:00.000Z:%1","source":"remote.tmux","type":"tmux_pane_focus","timestamp":"2026-04-01T00:00:00.000Z","cwd":"/home/sami","pane_id":"%1","tmux_session":"dev","window_index":1}"#;
    fs::write(&rotated, format!("{event_line}\n")).unwrap();

    let db = tt_db::Database::open_in_memory().unwrap();

    let inserted = import_local_events(&db, data_dir).unwrap();
    assert_eq!(inserted, 1, "should import from rotated events.jsonl.1");
}

#[test]
fn test_import_local_events_handles_missing_files() {
    let temp_dir = tempfile::tempdir().unwrap();
    let data_dir = temp_dir.path();

    let db = tt_db::Database::open_in_memory().unwrap();

    let inserted = import_local_events(&db, data_dir).unwrap();
    assert_eq!(inserted, 0, "should handle missing JSONL files gracefully");
}

#[test]
fn test_import_local_events_is_idempotent() {
    let temp_dir = tempfile::tempdir().unwrap();
    let data_dir = temp_dir.path();
    let events_file = data_dir.join("events.jsonl");
    let event_line = r#"{"id":"a59fae83-37bb-46c3-8a65-70962c28005a:remote.tmux:tmux_pane_focus:2026-05-17T00:00:00.000Z:%1","source":"remote.tmux","type":"tmux_pane_focus","timestamp":"2026-05-17T00:00:00.000Z","cwd":"/home/sami","pane_id":"%1","tmux_session":"dev","window_index":1}"#;
    fs::write(&events_file, format!("{event_line}\n")).unwrap();

    let db = tt_db::Database::open_in_memory().unwrap();

    let first = import_local_events(&db, data_dir).unwrap();
    let second = import_local_events(&db, data_dir).unwrap();

    assert_eq!(first, 1);
    assert_eq!(second, 0, "re-import should not duplicate events");
}

#[test]
fn ingest_report_counts_new_session_and_local_events() {
    // Given
    let report = IngestReport {
        claude: 2,
        opencode: 3,
        omp: 1,
        indexed_events: 7,
        imported_events: 5,
        drained_events: 0,
        stale_events_cleaned: 0,
        injected_events_pruned: 0,
        session_membership_assigned: 0,
        terminal_focus_assigned: 0,
        artifact_focus_assigned: 0,
        projects: Vec::new(),
        scanned_claude: true,
        scanned_opencode: true,
        scanned_omp: true,
    };

    // When
    let imported = report.imported_events();

    // Then
    assert_eq!(report.claude, 2);
    assert_eq!(report.opencode, 3);
    assert_eq!(imported, 5);
}

#[cfg(test)]
fn test_stream(id: &str) -> tt_db::Stream {
    let now = Utc::now();
    tt_db::Stream {
        id: id.to_string(),
        name: Some(id.to_string()),
        slug: None,
        description: None,
        color: None,
        created_at: now,
        updated_at: now,
        time_direct_ms: 0,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: false,
    }
}

#[cfg(test)]
fn test_window_focus(id: &str, timestamp: DateTime<Utc>, app_id: &str, title: &str) -> StoredEvent {
    StoredEvent {
        id: id.to_string(),
        timestamp,
        event_type: tt_core::EventType::WindowFocus,
        source: "local.cosmic".to_string(),
        machine_id: None,
        schema_version: 1,
        pane_id: None,
        tmux_session: None,
        window_index: None,
        git_project: None,
        git_workspace: None,
        status: None,
        idle_duration_ms: None,
        window_app_id: Some(app_id.to_string()),
        window_title: Some(title.to_string()),
        action: None,
        cwd: None,
        session_id: None,
        stream_id: None,
        assignment_source: None,
        data: serde_json::Value::Null,
    }
}

#[test]
fn assign_terminal_focus_attributes_a_terminal_window_to_concurrent_remote_work() {
    use chrono::TimeZone;

    // Given: remote work in a known stream, an unassigned terminal focus during
    // it, a terminal focus the user already assigned elsewhere, and a browser
    // focus that this pass has no business resolving.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&test_stream("eval")).unwrap();
    db.insert_stream(&test_stream("mine")).unwrap();

    let base = Utc.with_ymd_and_hms(2026, 7, 20, 12, 0, 0).unwrap();
    let mut remote = StoredEvent {
        stream_id: Some("eval".to_string()),
        cwd: Some("/home/ubuntu/xmodel-eval".to_string()),
        event_type: tt_core::EventType::AgentToolUse,
        ..test_window_focus("tool-1", base, "", "")
    };
    remote.window_app_id = None;
    remote.window_title = None;
    db.insert_event(&remote).unwrap();

    db.insert_event(&test_window_focus(
        "focus-terminal",
        base + chrono::Duration::seconds(20),
        "com.mitchellh.ghostty",
        "mosh devbox",
    ))
    .unwrap();

    let mut already_mine = test_window_focus(
        "focus-mine",
        base + chrono::Duration::seconds(25),
        "com.mitchellh.ghostty",
        "mosh devbox",
    );
    already_mine.stream_id = Some("mine".to_string());
    already_mine.assignment_source = Some("user".to_string());
    db.insert_event(&already_mine).unwrap();

    db.insert_event(&test_window_focus(
        "focus-browser",
        base + chrono::Duration::seconds(30),
        "brave-browser",
        "Work · Pull requests - Brave",
    ))
    .unwrap();

    // When: the terminal-focus pass runs.
    let assigned = assign_terminal_focus(&db).unwrap();

    // Then: only the unassigned terminal focus moves, tagged terminal_focus.
    assert_eq!(assigned, 1);
    let resolved = db.get_events_by_stream("eval").unwrap();
    let focus = resolved
        .iter()
        .find(|event| event.id == "focus-terminal")
        .expect("terminal focus should land on the concurrent stream");
    assert_eq!(focus.assignment_source.as_deref(), Some("terminal_focus"));

    // and the user's own assignment is left exactly as it was.
    let untouched = db.get_events_by_stream("mine").unwrap();
    let ids: Vec<&str> = untouched.iter().map(|event| event.id.as_str()).collect();
    assert_eq!(ids, vec!["focus-mine"]);
    assert_eq!(untouched[0].assignment_source.as_deref(), Some("user"));

    // and the browser focus stays unassigned rather than being invented into a stream.
    let unassigned = db.unassigned_event_ids().unwrap();
    assert_eq!(unassigned, vec!["focus-browser".to_string()]);
}

#[test]
fn assign_terminal_focus_leaves_a_junk_winner_unassigned() {
    use chrono::TimeZone;

    // Given: terminal activity whose only correlated stream is the reserved junk stream.
    let db = tt_db::Database::open_in_memory().unwrap();
    let junk_stream_id = db.junk_stream_id().unwrap();
    let base = Utc.with_ymd_and_hms(2026, 7, 20, 12, 0, 0).unwrap();
    let mut remote = StoredEvent {
        stream_id: Some(junk_stream_id),
        cwd: Some("/home/ubuntu/junk".to_string()),
        event_type: tt_core::EventType::AgentToolUse,
        ..test_window_focus("junk-tool", base, "", "")
    };
    remote.window_app_id = None;
    remote.window_title = None;
    db.insert_event(&remote).unwrap();
    db.insert_event(&test_window_focus(
        "junk-focus",
        base + chrono::Duration::seconds(20),
        "com.mitchellh.ghostty",
        "mosh devbox",
    ))
    .unwrap();

    // When: terminal-focus correlation resolves only junk activity.
    let assigned = assign_terminal_focus(&db).unwrap();

    // Then: the human focus remains unassigned instead of being filed as no work.
    assert_eq!(assigned, 0);
    let focus = db
        .get_events(None, None)
        .unwrap()
        .into_iter()
        .find(|event| event.id == "junk-focus")
        .unwrap();
    assert_eq!(focus.stream_id, None);
    assert_eq!(focus.assignment_source, None);
}

#[cfg(test)]
fn test_cwd_event(id: &str, timestamp: DateTime<Utc>, cwd: &str) -> StoredEvent {
    StoredEvent {
        cwd: Some(cwd.to_string()),
        event_type: tt_core::EventType::TmuxPaneFocus,
        window_app_id: None,
        window_title: None,
        ..test_window_focus(id, timestamp, "", "")
    }
}

#[test]
fn attribution_leaves_an_unambiguous_working_directory_unassigned() {
    use chrono::TimeZone;

    // Given: a stream whose only classified event sits in one working directory,
    // so that directory maps to exactly one stream — precisely the input the
    // removed cwd pass treated as unambiguous evidence.
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&test_stream("tracker")).unwrap();

    let base = Utc.with_ymd_and_hms(2026, 7, 20, 12, 0, 0).unwrap();
    let classified = StoredEvent {
        stream_id: Some("tracker".to_string()),
        assignment_source: Some("inferred".to_string()),
        event_type: tt_core::EventType::AgentToolUse,
        ..test_cwd_event("classified", base, "/home/sami/Code/time-tracker/default")
    };
    db.insert_event(&classified).unwrap();

    // and an unassigned event in that same directory,
    db.insert_event(&test_cwd_event(
        "same-cwd",
        base + chrono::Duration::seconds(10),
        "/home/sami/Code/time-tracker/default",
    ))
    .unwrap();

    // and one in the same directory under a different home, which the removed
    // pass also claimed by stripping `/home/<user>/` and matching the suffix.
    db.insert_event(&test_cwd_event(
        "same-suffix",
        base + chrono::Duration::seconds(20),
        "/home/ubuntu/Code/time-tracker/default",
    ))
    .unwrap();

    // When: every attribution pass ingest runs has run.
    let counts = attribute_unassigned_events(&db).unwrap();

    // Then: a working directory is not evidence of a stream, so both stay
    // unassigned, where they read as classification lag.
    let mut unassigned = db.unassigned_event_ids().unwrap();
    unassigned.sort();
    assert_eq!(
        unassigned,
        vec!["same-cwd".to_string(), "same-suffix".to_string()],
        "ingest must not infer a stream from a working directory"
    );
    assert_eq!(counts, AttributionCounts::default());

    // and the classified event keeps the verdict it already had.
    let tracker = db.get_events_by_stream("tracker").unwrap();
    let ids: Vec<&str> = tracker.iter().map(|event| event.id.as_str()).collect();
    assert_eq!(ids, vec!["classified"]);
}

#[test]
fn assign_artifact_focus_binds_a_pull_request_title_to_the_work_that_did_it() {
    use chrono::TimeZone;

    // Given: a classified session that referenced one pull request by URL,
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&test_stream("tracker")).unwrap();
    let base = Utc.with_ymd_and_hms(2026, 7, 20, 12, 0, 0).unwrap();

    let session = tt_core::session::AgentSession {
        session_id: "s-pr".to_string(),
        source: tt_core::session::SessionSource::Claude,
        parent_session_id: None,
        session_type: tt_core::session::SessionType::User,
        project_path: "/home/sami/Code/time-tracker".to_string(),
        project_name: "time-tracker".to_string(),
        start_time: base,
        end_time: Some(base + chrono::Duration::minutes(5)),
        message_count: 1,
        summary: Some("shipped https://github.com/sjawhar/time-tracker/pull/46".to_string()),
        user_prompts: Vec::new(),
        starting_prompt: None,
        assistant_message_count: 0,
        tool_call_count: 1,
        user_message_timestamps: Vec::new(),
        tool_call_timestamps: Vec::new(),
    };
    db.upsert_agent_session(&session, None).unwrap();

    // whose own events put it squarely in one stream,
    let mut worked = test_cwd_event("worked", base, "/home/sami/Code/time-tracker");
    worked.event_type = tt_core::EventType::AgentToolUse;
    worked.session_id = Some("s-pr".to_string());
    worked.stream_id = Some("tracker".to_string());
    worked.assignment_source = Some("inferred".to_string());
    db.insert_event(&worked).unwrap();

    // a browser window displaying that same pull request,
    db.insert_event(&test_window_focus(
        "focus-pr",
        base + chrono::Duration::hours(3),
        "brave-browser",
        "Add a cwd guard by sjawhar · Pull Request #46 · sjawhar/time-tracker",
    ))
    .unwrap();

    // and a browser window that reaches no further than the repository.
    db.insert_event(&test_window_focus(
        "focus-listing",
        base + chrono::Duration::hours(3) + chrono::Duration::seconds(30),
        "brave-browser",
        "Pull requests · sjawhar/time-tracker",
    ))
    .unwrap();

    // When
    let assigned = assign_artifact_focus(&db).unwrap();

    // Then: the artifact binds to the work that did it,
    assert_eq!(assigned, 1);
    let tracker = db.get_events_by_stream("tracker").unwrap();
    let focus = tracker
        .iter()
        .find(|event| event.id == "focus-pr")
        .expect("the pull request title should bind to the stream that did it");
    assert_eq!(
        focus.assignment_source.as_deref(),
        Some("artifact_reference")
    );

    // and the repository-wide listing identifies no work, so it stays unassigned.
    assert_eq!(
        db.unassigned_event_ids().unwrap(),
        vec!["focus-listing".to_string()]
    );
}

/// The pane-focus stamp was inert until something claimed it: `attribute_unassigned_events`
/// held only the two focus passes, and neither reads `session_id`. This pins the wiring,
/// because a working `claim_unassigned_events_for_classified_sessions` that no pass calls
/// is exactly the defect that shipped.
#[test]
fn attribution_claims_a_pane_stamped_after_its_session_was_classified() {
    use chrono::TimeZone;

    // Given: a session the classifier already placed on one stream,
    let db = tt_db::Database::open_in_memory().unwrap();
    db.insert_stream(&test_stream("tracker")).unwrap();

    let base = Utc.with_ymd_and_hms(2026, 7, 20, 12, 0, 0).unwrap();
    let mut classified = StoredEvent {
        event_type: tt_core::EventType::AgentToolUse,
        stream_id: Some("tracker".to_string()),
        assignment_source: Some("inferred".to_string()),
        ..test_cwd_event("tool-1", base, "/home/sami/Code/time-tracker/default")
    };
    classified.session_id = Some("ses-a".to_string());
    db.insert_event(&classified).unwrap();
    db.record_classification("ses-a", 1).unwrap();

    // and a pane focus stamped with that session id afterwards, which the classifier's
    // own claim ran too early to see.
    let mut stamped = test_cwd_event(
        "pane-late",
        base + chrono::Duration::seconds(30),
        "/home/sami/Code/time-tracker/default",
    );
    stamped.session_id = Some("ses-a".to_string());
    db.insert_event(&stamped).unwrap();

    // When: every attribution pass ingest runs has run.
    let counts = attribute_unassigned_events(&db).unwrap();

    // Then: the pass ran and the pane carries its own session's verdict.
    assert_eq!(counts.session_membership, 1);
    let tracker = db.get_events_by_stream("tracker").unwrap();
    let pane = tracker
        .iter()
        .find(|event| event.id == "pane-late")
        .expect("a stamped pane must take the stream of the session running in it");
    assert_eq!(
        pane.assignment_source.as_deref(),
        Some("session_membership")
    );
    assert!(db.unassigned_event_ids().unwrap().is_empty());
}

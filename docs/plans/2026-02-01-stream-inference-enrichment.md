# Stream Inference via Claude Code Enrichment - Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace directory-based stream inference with LLM-powered inference using Claude Code session data and jj project identity.

**Architecture:** (1) Index Claude Code sessions extracting project paths from session content, (2) enrich focus events with jj project identity, (3) use LLM to cluster events+sessions into meaningful streams. Windows are 24 hours with 4 hour overlap to carry context forward.

**Tech Stack:** Rust, serde_json (JSONL parsing), rusqlite (storage), tt-llm (Claude API), chrono (timestamps), rayon (parallel file processing)

**BREAKING CHANGE:** Delete `~/.time-tracker/db.sqlite` and reimport events after implementation.

---

## Task 1: Update InferredStream Type in tt-core

**Files:**
- Modify: `crates/tt-core/src/inference.rs`
- Modify: `crates/tt-core/Cargo.toml`

**Step 1: Add serde dependency to tt-core/Cargo.toml**

Add after existing dependencies:

```toml
serde = { workspace = true, features = ["derive"] }
```

**Step 2: Run cargo check to verify dependency works**

Run: `cargo check -p tt-core`
Expected: PASS (compiles)

**Step 3: Update InferredStream struct**

Edit `crates/tt-core/src/inference.rs`, replace the existing `InferredStream` struct:

```rust
/// A stream produced by inference.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InferredStream {
    /// Unique identifier (UUID).
    pub id: StreamId,
    /// Human-readable name.
    pub name: String,
    /// Session IDs that belong to this stream (for LLM inference).
    #[serde(default)]
    pub session_ids: Vec<String>,
    /// Confidence score (0.0-1.0) for human review. Default 1.0 for directory-based inference.
    #[serde(default = "default_confidence")]
    pub confidence: f32,
    /// Timestamp of the first event in this stream.
    pub first_event_at: DateTime<Utc>,
    /// Timestamp of the last event in this stream.
    pub last_event_at: DateTime<Utc>,
}

fn default_confidence() -> f32 { 1.0 }
```

**Step 4: Run tests to verify no regressions**

Run: `cargo test -p tt-core`
Expected: PASS

**Step 5: Commit**

```bash
jj describe -m "feat(core): add session_ids and confidence to InferredStream"
```

---

## Task 2: Session Indexing Module

**Files:**
- Create: `crates/tt-core/src/session.rs`
- Modify: `crates/tt-core/src/lib.rs`
- Modify: `crates/tt-core/Cargo.toml`

**Step 1: Add dependencies to tt-core/Cargo.toml**

```toml
tracing.workspace = true
rayon = "1.10"
```

**Step 2: Run cargo check**

Run: `cargo check -p tt-core`
Expected: PASS

**Step 3: Write the failing test**

Create `crates/tt-core/src/session.rs`:

```rust
//! Claude Code session indexing with performance optimizations.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use chrono::{DateTime, Utc};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Buffer size for BufReader (64KB for optimal performance on large files)
const BUFFER_SIZE: usize = 64 * 1024;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("no messages found in session")]
    NoMessages,
    #[error("no project path found in session")]
    NoProjectPath,
}

/// An indexed Claude Code session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    pub session_id: String,
    pub parent_session_id: Option<String>,
    pub project_path: String,
    pub project_name: String,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pub message_count: i32,
    pub summary: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_parse_session_extracts_cwd_and_summary() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, r#"{{"type":"user","message":{{"role":"user","content":"hello"}},"timestamp":"2026-01-29T10:58:45.000Z","cwd":"/home/sami/time-tracker/default"}}"#).unwrap();
        writeln!(file, r#"{{"type":"assistant","message":{{"role":"assistant","content":"hi"}},"timestamp":"2026-01-29T10:59:00.000Z"}}"#).unwrap();
        writeln!(file, r#"{{"type":"summary","summary":"Implementing export command","leafUuid":"abc"}}"#).unwrap();

        let entry = parse_session_file(file.path(), "test-session", None).unwrap();

        assert_eq!(entry.project_path, "/home/sami/time-tracker/default");
        assert_eq!(entry.summary.as_deref(), Some("Implementing export command"));
        assert_eq!(entry.message_count, 2);
    }

    #[test]
    fn test_extract_project_name_from_workspace_path() {
        assert_eq!(extract_project_name("/home/sami/time-tracker/default"), "time-tracker");
        assert_eq!(extract_project_name("/home/sami/pivot/main"), "pivot");
        assert_eq!(extract_project_name("/home/sami/.dotfiles"), ".dotfiles");
    }
}
```

**Step 4: Run test to verify it fails**

Run: `cargo test -p tt-core session`
Expected: FAIL with "cannot find function `parse_session_file`"

**Step 5: Write implementation**

Add to `crates/tt-core/src/session.rs` before the `#[cfg(test)]` block:

```rust
/// Minimal struct for typed deserialization (faster than serde_json::Value)
#[derive(Deserialize)]
struct MessageHeader {
    #[serde(rename = "type")]
    msg_type: Option<String>,
    cwd: Option<String>,
    summary: Option<String>,
    timestamp: Option<String>,
}

/// Check if a line might contain relevant data (pre-filter before JSON parse)
fn might_be_relevant(line: &str) -> bool {
    line.contains("\"type\":") || line.contains("\"cwd\":")
}

/// Parse a Claude Code session JSONL file.
pub fn parse_session_file(path: &Path, session_id: &str, parent_session_id: Option<&str>) -> Result<SessionEntry, SessionError> {
    let file = File::open(path)?;
    let reader = BufReader::with_capacity(BUFFER_SIZE, file);

    let mut message_count = 0i32;
    let mut first_timestamp: Option<DateTime<Utc>> = None;
    let mut last_timestamp: Option<DateTime<Utc>> = None;
    let mut summary: Option<String> = None;
    let mut project_path: Option<String> = None;

    for line in reader.lines() {
        let line = line?;

        if line.len() < 10 || !might_be_relevant(&line) {
            continue;
        }

        let header: MessageHeader = match serde_json::from_str(&line) {
            Ok(h) => h,
            Err(e) => {
                tracing::trace!(error = %e, "skipping malformed JSON line");
                continue;
            }
        };

        if project_path.is_none() {
            if let Some(cwd) = header.cwd {
                project_path = Some(cwd);
            }
        }

        match header.msg_type.as_deref() {
            Some("summary") => {
                summary = header.summary;
            }
            Some("user") | Some("assistant") => {
                message_count += 1;
                if let Some(ts_str) = header.timestamp {
                    if let Ok(ts) = DateTime::parse_from_rfc3339(&ts_str) {
                        let ts = ts.with_timezone(&Utc);
                        if first_timestamp.is_none() {
                            first_timestamp = Some(ts);
                        }
                        last_timestamp = Some(ts);
                    }
                }
            }
            _ => {}
        }
    }

    let start_time = first_timestamp.ok_or(SessionError::NoMessages)?;
    let project_path = project_path.ok_or(SessionError::NoProjectPath)?;

    Ok(SessionEntry {
        session_id: session_id.to_string(),
        parent_session_id: parent_session_id.map(String::from),
        project_name: extract_project_name(&project_path),
        project_path,
        start_time,
        end_time: if last_timestamp != first_timestamp { last_timestamp } else { None },
        message_count,
        summary,
    })
}

/// Extract project name from path.
pub fn extract_project_name(path: &str) -> String {
    let path_obj = Path::new(path);
    let basename = path_obj.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");

    const WORKSPACE_NAMES: &[&str] = &["default", "main", "dev", "feature", "master"];

    if WORKSPACE_NAMES.contains(&basename) {
        path_obj
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or(basename)
            .to_string()
    } else {
        basename.to_string()
    }
}
```

**Step 6: Run tests**

Run: `cargo test -p tt-core session`
Expected: PASS

**Step 7: Add scan_sessions_directory function**

Add to `crates/tt-core/src/session.rs`:

```rust
struct SessionFile {
    path: std::path::PathBuf,
    session_id: String,
    parent_session_id: Option<String>,
}

/// Scan Claude Code projects directory and build session index.
pub fn scan_sessions_directory(projects_dir: &Path) -> Result<Vec<SessionEntry>, SessionError> {
    if !projects_dir.exists() {
        return Ok(Vec::new());
    }

    let mut session_files: Vec<SessionFile> = Vec::new();

    for project_entry in std::fs::read_dir(projects_dir)? {
        let project_entry = project_entry?;
        let project_path = project_entry.path();

        if !project_path.is_dir() {
            continue;
        }

        for session_entry in std::fs::read_dir(&project_path)? {
            let session_entry = session_entry?;
            let session_path = session_entry.path();

            if session_path.is_file() && session_path.extension().map_or(false, |e| e == "jsonl") {
                let session_id = session_path
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();

                session_files.push(SessionFile {
                    path: session_path,
                    session_id,
                    parent_session_id: None,
                });
            } else if session_path.is_dir() {
                let subagents_dir = session_path.join("subagents");
                if subagents_dir.exists() {
                    let parent_session_id = session_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(String::from);

                    if let Ok(subagent_entries) = std::fs::read_dir(&subagents_dir) {
                        for subagent_entry in subagent_entries.flatten() {
                            let subagent_path = subagent_entry.path();

                            if subagent_path.is_file() && subagent_path.extension().map_or(false, |e| e == "jsonl") {
                                let session_id = subagent_path
                                    .file_stem()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("")
                                    .to_string();

                                session_files.push(SessionFile {
                                    path: subagent_path,
                                    session_id,
                                    parent_session_id: parent_session_id.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    let mut entries: Vec<SessionEntry> = session_files
        .par_iter()
        .filter_map(|sf| {
            match parse_session_file(&sf.path, &sf.session_id, sf.parent_session_id.as_deref()) {
                Ok(entry) => Some(entry),
                Err(e) => {
                    tracing::warn!(path = ?sf.path, error = %e, "skipping invalid session");
                    None
                }
            }
        })
        .collect();

    entries.sort_by_key(|e| e.start_time);
    Ok(entries)
}
```

**Step 8: Add module to lib.rs**

Edit `crates/tt-core/src/lib.rs`, add:

```rust
pub mod session;
```

**Step 9: Run all tests**

Run: `cargo test -p tt-core`
Expected: PASS

**Step 10: Commit**

```bash
jj describe -m "feat(core): add Claude Code session indexing module"
```

---

## Task 3: Database Storage for Sessions

**Files:**
- Modify: `crates/tt-db/src/lib.rs`

**Step 1: Write the failing test**

Add to `crates/tt-db/src/lib.rs` in the `#[cfg(test)]` module:

```rust
#[test]
fn test_session_storage() {
    use chrono::TimeZone;
    use tt_core::session::SessionEntry;

    let db = Database::open_in_memory().unwrap();

    let entry = SessionEntry {
        session_id: "test-session".to_string(),
        parent_session_id: None,
        project_path: "/home/user/project".to_string(),
        project_name: "project".to_string(),
        start_time: chrono::Utc.with_ymd_and_hms(2026, 1, 29, 10, 0, 0).unwrap(),
        end_time: Some(chrono::Utc.with_ymd_and_hms(2026, 1, 29, 11, 0, 0).unwrap()),
        message_count: 10,
        summary: Some("Test session".to_string()),
    };

    db.upsert_session(&entry).unwrap();

    let start = chrono::Utc.with_ymd_and_hms(2026, 1, 29, 9, 0, 0).unwrap();
    let end = chrono::Utc.with_ymd_and_hms(2026, 1, 29, 12, 0, 0).unwrap();
    let sessions = db.sessions_in_range(start, end).unwrap();

    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].project_name, "project");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p tt-db session_storage`
Expected: FAIL with "method `upsert_session` not found"

**Step 3: Add sessions table to create_tables()**

In `crates/tt-db/src/lib.rs`, find the `create_tables()` function and add after existing CREATE TABLE statements:

```rust
conn.execute_batch(r#"
    CREATE TABLE IF NOT EXISTS sessions (
        session_id TEXT PRIMARY KEY,
        parent_session_id TEXT,
        project_path TEXT NOT NULL,
        project_name TEXT NOT NULL,
        start_time TEXT NOT NULL,
        end_time TEXT,
        message_count INTEGER NOT NULL,
        summary TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_sessions_start_time ON sessions(start_time);
    CREATE INDEX IF NOT EXISTS idx_sessions_project_path ON sessions(project_path);
    CREATE INDEX IF NOT EXISTS idx_sessions_parent ON sessions(parent_session_id);
"#)?;
```

**Step 4: Add upsert_session method**

Add to `impl Database`:

```rust
/// Insert or update a session entry.
pub fn upsert_session(&self, entry: &tt_core::session::SessionEntry) -> Result<(), DbError> {
    self.conn.execute(
        "INSERT INTO sessions (session_id, parent_session_id, project_path, project_name, start_time, end_time, message_count, summary)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(session_id) DO UPDATE SET
            parent_session_id = excluded.parent_session_id,
            project_path = excluded.project_path,
            project_name = excluded.project_name,
            start_time = excluded.start_time,
            end_time = excluded.end_time,
            message_count = excluded.message_count,
            summary = excluded.summary",
        rusqlite::params![
            entry.session_id,
            entry.parent_session_id,
            entry.project_path,
            entry.project_name,
            entry.start_time.to_rfc3339(),
            entry.end_time.map(|t| t.to_rfc3339()),
            entry.message_count,
            entry.summary,
        ],
    )?;
    Ok(())
}
```

**Step 5: Add sessions_in_range method**

Add to `impl Database`:

```rust
/// Get sessions that overlap with a time range.
pub fn sessions_in_range(
    &self,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Vec<tt_core::session::SessionEntry>, DbError> {
    let mut stmt = self.conn.prepare(
        "SELECT session_id, parent_session_id, project_path, project_name, start_time, end_time, message_count, summary
         FROM sessions
         WHERE start_time <= ?2 AND (end_time IS NULL OR end_time >= ?1)
         ORDER BY start_time"
    )?;

    let mut sessions = Vec::new();
    let mut rows = stmt.query(rusqlite::params![start.to_rfc3339(), end.to_rfc3339()])?;

    while let Some(row) = rows.next()? {
        let session_id: String = row.get(0)?;
        let start_time_str: String = row.get(4)?;
        let end_time_str: Option<String> = row.get(5)?;

        let start_time = match DateTime::parse_from_rfc3339(&start_time_str) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(e) => {
                tracing::warn!(session_id, error = %e, "skipping session with malformed start_time");
                continue;
            }
        };

        let end_time = match end_time_str {
            Some(s) => match DateTime::parse_from_rfc3339(&s) {
                Ok(dt) => Some(dt.with_timezone(&Utc)),
                Err(e) => {
                    tracing::warn!(session_id, error = %e, "skipping session with malformed end_time");
                    continue;
                }
            },
            None => None,
        };

        sessions.push(tt_core::session::SessionEntry {
            session_id,
            parent_session_id: row.get(1)?,
            project_path: row.get(2)?,
            project_name: row.get(3)?,
            start_time,
            end_time,
            message_count: row.get(6)?,
            summary: row.get(7)?,
        });
    }

    Ok(sessions)
}
```

**Step 6: Run test**

Run: `cargo test -p tt-db session_storage`
Expected: PASS

**Step 7: Commit**

```bash
jj describe -m "feat(db): add sessions table with time range queries"
```

---

## Task 4: jj Project Identity

**Files:**
- Create: `crates/tt-core/src/project.rs`
- Modify: `crates/tt-core/src/lib.rs`
- Modify: `crates/tt-cli/src/commands/ingest.rs`

**Step 1: Write the failing test**

Create `crates/tt-core/src/project.rs`:

```rust
//! jj/git project identity extraction.

use std::path::Path;

/// Project identity from jj context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectIdentity {
    pub project_name: String,
    pub workspace_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_git_remote_url() {
        assert_eq!(
            parse_remote_name("https://github.com/user/time-tracker.git"),
            Some("time-tracker".to_string())
        );
        assert_eq!(
            parse_remote_name("git@github.com:user/dotfiles.git"),
            Some("dotfiles".to_string())
        );
    }

    #[test]
    fn test_project_identity_multi_workspace() {
        let identity = ProjectIdentity::from_jj_output(
            Some("https://github.com/user/time-tracker.git"),
            2,
            "/home/sami/time-tracker/default",
        );

        assert_eq!(identity.project_name, "time-tracker");
        assert_eq!(identity.workspace_name.as_deref(), Some("default"));
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p tt-core parse_git_remote`
Expected: FAIL with "cannot find function `parse_remote_name`"

**Step 3: Write implementation**

Add to `crates/tt-core/src/project.rs` before the tests:

```rust
/// Extract repo name from a git remote URL.
pub fn parse_remote_name(url: &str) -> Option<String> {
    let name = url
        .rsplit('/')
        .next()
        .or_else(|| url.rsplit(':').next())?
        .trim_end_matches(".git");

    if name.is_empty() { None } else { Some(name.to_string()) }
}

impl ProjectIdentity {
    /// Build identity from jj command outputs.
    pub fn from_jj_output(
        remote_url: Option<&str>,
        workspace_count: usize,
        jj_root: &str,
    ) -> Self {
        let workspace_name = Path::new(jj_root)
            .file_name()
            .and_then(|n| n.to_str())
            .map(String::from);

        let remote_name = remote_url.and_then(parse_remote_name);

        let project_name = if workspace_count > 1 {
            remote_name.or_else(|| {
                Path::new(jj_root)
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .map(String::from)
            })
        } else {
            remote_name.or_else(|| workspace_name.clone())
        }.unwrap_or_else(|| "unknown".to_string());

        Self { project_name, workspace_name }
    }
}
```

**Step 4: Add module to lib.rs**

Edit `crates/tt-core/src/lib.rs`, add:

```rust
pub mod project;
```

**Step 5: Run tests**

Run: `cargo test -p tt-core project`
Expected: PASS

**Step 6: Add get_jj_identity to tt-cli/src/commands/ingest.rs**

Add this function:

```rust
use tt_core::project::ProjectIdentity;

/// Get project identity for a directory using jj commands.
fn get_jj_identity(cwd: &std::path::Path) -> Option<ProjectIdentity> {
    use std::process::Command;

    if !cwd.join(".jj").exists() {
        return None;
    }

    let remote_output = Command::new("jj")
        .args(["git", "remote", "list", "--ignore-working-copy"])
        .current_dir(cwd)
        .output()
        .ok()?;

    let remote_str = String::from_utf8_lossy(&remote_output.stdout);
    let remote_url = remote_str
        .lines()
        .find(|line| line.contains("origin"))
        .and_then(|line| line.split_whitespace().nth(1));

    let workspace_output = Command::new("jj")
        .args(["workspace", "list", "--ignore-working-copy"])
        .current_dir(cwd)
        .output()
        .ok()?;

    let workspace_count = String::from_utf8_lossy(&workspace_output.stdout)
        .lines()
        .count();

    let root_output = Command::new("jj")
        .args(["root", "--ignore-working-copy"])
        .current_dir(cwd)
        .output()
        .ok()?;

    let jj_root = String::from_utf8_lossy(&root_output.stdout).trim().to_string();

    Some(ProjectIdentity::from_jj_output(remote_url, workspace_count, &jj_root))
}
```

**Step 7: Update PaneFocusData struct**

In `crates/tt-cli/src/commands/ingest.rs`, update the struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneFocusData {
    pub pane_id: String,
    pub session_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jj_project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jj_workspace: Option<String>,
}
```

**Step 8: Update IngestEvent::pane_focus to use jj context**

Update the function to call `get_jj_identity` and populate the new fields.

**Step 9: Run tests and update snapshots**

Run: `cargo test -p tt-cli`
Then: `cargo insta review` to update snapshots

**Step 10: Commit**

```bash
jj describe -m "feat(ingest): add jj project identity to focus events"
```

---

## Task 5: LLM Stream Inference

**Files:**
- Modify: `crates/tt-llm/src/lib.rs`

**Step 1: Write the failing test**

Add to `crates/tt-llm/src/lib.rs` tests:

```rust
#[test]
fn test_stream_inference_prompt() {
    use tt_core::session::SessionEntry;
    use chrono::TimeZone;

    let sessions = vec![
        SessionEntry {
            session_id: "s1".to_string(),
            parent_session_id: None,
            project_path: "/home/sami/time-tracker/default".to_string(),
            project_name: "time-tracker".to_string(),
            start_time: chrono::Utc.with_ymd_and_hms(2026, 1, 29, 10, 0, 0).unwrap(),
            end_time: Some(chrono::Utc.with_ymd_and_hms(2026, 1, 29, 11, 0, 0).unwrap()),
            message_count: 10,
            summary: Some("Implementing export command".to_string()),
        },
    ];

    let prompt = build_stream_inference_prompt(&sessions, &[]);

    assert!(prompt.contains("time-tracker"));
    assert!(prompt.contains("Implementing export"));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p tt-llm stream_inference`
Expected: FAIL with "cannot find function `build_stream_inference_prompt`"

**Step 3: Write build_stream_inference_prompt**

Add to `crates/tt-llm/src/lib.rs`:

```rust
use tt_core::session::SessionEntry;
use tt_core::inference::InferredStream;

/// Build prompt for LLM stream inference.
pub fn build_stream_inference_prompt(
    sessions: &[SessionEntry],
    previous_streams: &[InferredStream],
) -> String {
    let mut prompt = String::new();

    prompt.push_str("You are analyzing Claude Code sessions to group them into meaningful work streams.\n\n");

    if !previous_streams.is_empty() {
        prompt.push_str("## Context from Previous Window\n\n");
        for stream in previous_streams {
            prompt.push_str(&format!("- \"{}\" (id: {})\n", stream.name, stream.id));
        }
        prompt.push('\n');
    }

    prompt.push_str("## Sessions to Analyze\n\n");

    for session in sessions {
        prompt.push_str(&format!(
            "Session: {}\n  Project: {}\n  Summary: {}\n\n",
            session.session_id,
            session.project_name,
            session.summary.as_deref().unwrap_or("(no summary)"),
        ));
    }

    prompt.push_str(r#"Respond with JSON:
{"streams": [{"id": "string", "name": "string", "session_ids": ["array"], "confidence": 0.0-1.0}]}
"#);

    prompt
}
```

**Step 4: Run test**

Run: `cargo test -p tt-llm stream_inference`
Expected: PASS

**Step 5: Write infer_streams function**

Add the async function for calling the LLM API (see full implementation in codebase).

**Step 6: Run all tests**

Run: `cargo test -p tt-llm`
Expected: PASS

**Step 7: Commit**

```bash
jj describe -m "feat(llm): add stream inference with context carryover"
```

---

## Task 6: CLI Integration

**Files:**
- Modify: `crates/tt-cli/src/commands/infer.rs`
- Modify: `crates/tt-cli/src/main.rs`

**Step 1: Add --llm flag to infer command**

Update CLI args in `main.rs`:

```rust
Infer {
    #[arg(long)]
    llm: bool,
    #[arg(long)]
    start: Option<DateTime<Utc>>,
    #[arg(long)]
    end: Option<DateTime<Utc>>,
    #[arg(long)]
    dry_run: bool,
},
```

**Step 2: Add sessions index subcommand**

```rust
Sessions {
    #[command(subcommand)]
    action: SessionsAction,
},

#[derive(Subcommand)]
enum SessionsAction {
    Index,
}
```

**Step 3: Implement infer --llm handler**

Update infer command to check for `--llm` flag and call LLM inference.

**Step 4: Test manually**

Run: `cargo run -- sessions index`
Run: `cargo run -- infer --llm --dry-run`
Expected: Shows prompt preview

**Step 5: Commit**

```bash
jj describe -m "feat(cli): integrate LLM inference into tt infer command"
```

---

## Summary

**6 tasks** implementing:

1. **Update InferredStream type** - add `session_ids` and `confidence` fields
2. **Session indexing module** - parse Claude Code sessions with rayon, typed deserialization
3. **Database storage** - sessions table with time range queries
4. **jj project identity** - pure parsing in tt-core, process spawning in tt-cli
5. **LLM stream inference** - prompt builder with context carryover
6. **CLI integration** - `tt infer --llm` and `tt sessions index`

**Key decisions:**
- Breaking change accepted - delete and recreate DB
- Unified `InferredStream` type in tt-core
- tt-core remains pure (no IO) - process spawning in tt-cli

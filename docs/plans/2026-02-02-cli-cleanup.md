# CLI Cleanup Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Consolidate CLI commands by removing redundant shortcuts and organizing subcommands under consistent namespaces.

**Architecture:** Remove `infer` (replaced by `context`), `week`/`today`/`yesterday` shortcuts, merge `stream create` into `streams create`, move `sessions index` into `ingest sessions`. All changes are pure deletions or renames - no new functionality.

**Tech Stack:** Rust, clap, existing command implementations

---

## Summary of Changes

| Before | After | Reason |
|--------|-------|--------|
| `tt infer` | deleted | Replaced by `tt context` |
| `tt week` | deleted | Use `tt report --week` |
| `tt today` | deleted | Use `tt report --day` |
| `tt yesterday` | deleted | Use `tt report --last-day` |
| `tt streams` | `tt streams list` | Namespace consistency |
| `tt stream create` | `tt streams create` | Namespace consistency |
| `tt sessions index` | `tt ingest sessions` | Consolidate under ingest |

---

### Task 1: Delete the `infer` Command

**Files:**
- Delete: `crates/tt-cli/src/commands/infer.rs`
- Modify: `crates/tt-cli/src/commands/mod.rs`
- Modify: `crates/tt-cli/src/cli.rs`
- Modify: `crates/tt-cli/src/main.rs`

**Step 1: Delete infer.rs**

```bash
rm crates/tt-cli/src/commands/infer.rs
```

**Step 2: Remove from mod.rs**

In `crates/tt-cli/src/commands/mod.rs`, remove line:
```rust
pub mod infer;
```

**Step 3: Remove from cli.rs**

In `crates/tt-cli/src/cli.rs`, delete the entire `Infer` variant (lines 75-94):
```rust
    /// Output context for stream inference.
    ///
    /// Outputs JSON context containing events and Claude Code sessions in a
    /// time range. This context is intended for Claude Code to read and use
    /// when creating streams.
    Infer {
        /// Clear all inferred assignments first.
        ///
        /// User assignments are preserved.
        #[arg(long)]
        force: bool,

        /// Start of time range (ISO 8601 or relative like "2 hours ago").
        #[arg(long)]
        start: Option<String>,

        /// End of time range (ISO 8601, defaults to now).
        #[arg(long)]
        end: Option<String>,
    },
```

**Step 4: Remove from main.rs**

In `crates/tt-cli/src/main.rs`, remove the `infer` import and match arm:

Remove from imports:
```rust
use tt_cli::commands::{
    context, events, export, import, infer, ingest, ...
```

Remove match arm:
```rust
        Some(Commands::Infer { force, start, end }) => {
            let (db, _config) = open_database(cli.config.as_deref())?;
            infer::run(&db, *force, start.clone(), end.clone())?;
        }
```

**Step 5: Verify compilation**

Run: `cargo build -p tt-cli`
Expected: PASS

---

### Task 2: Delete Shortcut Commands (week, today, yesterday)

**Files:**
- Modify: `crates/tt-cli/src/cli.rs`
- Modify: `crates/tt-cli/src/main.rs`

**Step 1: Remove from cli.rs**

Delete these three command variants from `Commands` enum (lines 138-157):
```rust
    /// Shortcut for `report --week`.
    Week {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Shortcut for `report --day`.
    Today {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Shortcut for `report --last-day`.
    Yesterday {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
```

**Step 2: Remove from main.rs**

Remove these match arms:
```rust
        Some(Commands::Week { json }) => {
            let (db, _config) = open_database(cli.config.as_deref())?;
            report::run(&db, report::Period::Week, *json)?;
        }
        Some(Commands::Today { json }) => {
            let (db, _config) = open_database(cli.config.as_deref())?;
            report::run(&db, report::Period::Day, *json)?;
        }
        Some(Commands::Yesterday { json }) => {
            let (db, _config) = open_database(cli.config.as_deref())?;
            report::run(&db, report::Period::LastDay, *json)?;
        }
```

**Step 3: Verify compilation**

Run: `cargo build -p tt-cli`
Expected: PASS

---

### Task 3: Restructure Streams (list + create under one namespace)

**Files:**
- Modify: `crates/tt-cli/src/cli.rs`
- Modify: `crates/tt-cli/src/main.rs`
- Modify: `crates/tt-cli/src/commands/streams.rs`
- Delete: `crates/tt-cli/src/commands/stream.rs`
- Modify: `crates/tt-cli/src/commands/mod.rs`
- Modify: `crates/tt-cli/src/lib.rs`

**Step 1: Update cli.rs**

Replace current `Streams` and `Stream` commands with a single `Streams` that has subcommands:

Remove:
```rust
    /// List all streams with time totals and tags.
    ///
    /// Shows streams from the last 7 days, sorted by total time.
    /// Use 'tt tag <id> <tag>' to organize streams into projects.
    Streams {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Manage streams.
    #[command(subcommand)]
    Stream(StreamCommands),
```

Replace with:
```rust
    /// Manage streams.
    #[command(subcommand)]
    Streams(StreamsAction),
```

Replace `StreamCommands` enum with `StreamsAction`:
```rust
/// Streams subcommand actions.
#[derive(Debug, Subcommand)]
pub enum StreamsAction {
    /// List streams with time totals and tags.
    ///
    /// Shows streams from the last 7 days, sorted by total time.
    /// Use 'tt tag <id> <tag>' to organize streams into projects.
    List {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Create a new stream (prints ID to stdout).
    Create {
        /// Name for the stream.
        name: String,
    },
}
```

**Step 2: Update lib.rs exports**

Replace `StreamCommands` with `StreamsAction`:
```rust
pub use cli::{Cli, Commands, IngestEvent, SessionsAction, StreamsAction};
```

**Step 3: Move create functionality to streams.rs**

Add to `crates/tt-cli/src/commands/streams.rs` at the end (before tests):
```rust
/// Create a new stream with the given name.
///
/// Generates a UUID, inserts the stream into the database, and prints the ID to stdout.
pub fn create(db: &Database, name: String) -> Result<()> {
    use chrono::Utc;
    use tt_db::Stream;
    use uuid::Uuid;

    let now = Utc::now();

    let stream = Stream {
        id: Uuid::new_v4().to_string(),
        name: Some(name),
        created_at: now,
        updated_at: now,
        time_direct_ms: 0,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: true,
    };

    db.insert_stream(&stream)
        .context("failed to create stream")?;
    println!("{}", stream.id);
    Ok(())
}
```

Also add the tests from stream.rs to streams.rs (in the tests module):
```rust
    #[test]
    fn test_create_stream_exists_in_db() {
        let db = Database::open_in_memory().unwrap();
        let name = "My Test Stream".to_string();
        create(&db, name.clone()).unwrap();

        let resolved = db.resolve_stream("My Test Stream").unwrap();
        assert!(resolved.is_some());
        let stream = resolved.unwrap();
        assert_eq!(stream.name, Some(name));
        assert_eq!(stream.time_direct_ms, 0);
        assert_eq!(stream.time_delegated_ms, 0);
        assert!(stream.needs_recompute);
    }

    #[test]
    fn test_create_stream_generates_uuid() {
        let db = Database::open_in_memory().unwrap();
        create(&db, "Stream 1".to_string()).unwrap();
        create(&db, "Stream 2".to_string()).unwrap();

        let stream1 = db.resolve_stream("Stream 1").unwrap().unwrap();
        let stream2 = db.resolve_stream("Stream 2").unwrap().unwrap();

        assert_ne!(stream1.id, stream2.id);
        assert_eq!(stream1.id.len(), 36);
        assert_eq!(stream2.id.len(), 36);
    }
```

**Step 4: Delete stream.rs**

```bash
rm crates/tt-cli/src/commands/stream.rs
```

**Step 5: Update mod.rs**

Remove:
```rust
pub mod stream;
```

**Step 6: Update main.rs**

Remove `stream` from imports:
```rust
use tt_cli::commands::{
    context, events, export, import, ingest, recompute, report, sessions, status, stream,
    streams, suggest, sync, tag,
};
```

Change to:
```rust
use tt_cli::commands::{
    context, events, export, import, ingest, recompute, report, status,
    streams, suggest, sync, tag,
};
```

Update imports from lib:
```rust
use tt_cli::{Cli, Commands, Config, IngestEvent, SessionsAction, StreamCommands};
```

Change to:
```rust
use tt_cli::{Cli, Commands, Config, IngestEvent, SessionsAction, StreamsAction};
```

Replace match arms:
```rust
        Some(Commands::Streams { json }) => {
            let (db, _config) = open_database(cli.config.as_deref())?;
            streams::run(&db, *json)?;
        }
        Some(Commands::Stream(cmd)) => {
            let (db, _config) = open_database(cli.config.as_deref())?;
            match cmd {
                StreamCommands::Create { name } => stream::create(&db, name.clone())?,
            }
        }
```

With:
```rust
        Some(Commands::Streams(action)) => {
            let (db, _config) = open_database(cli.config.as_deref())?;
            match action {
                StreamsAction::List { json } => streams::run(&db, *json)?,
                StreamsAction::Create { name } => streams::create(&db, name.clone())?,
            }
        }
```

**Step 7: Verify compilation and tests**

Run: `cargo build -p tt-cli && cargo test -p tt-cli`
Expected: PASS

---

### Task 4: Move Sessions Index Under Ingest

**Files:**
- Modify: `crates/tt-cli/src/cli.rs`
- Modify: `crates/tt-cli/src/main.rs`
- Modify: `crates/tt-cli/src/commands/ingest.rs`
- Delete: `crates/tt-cli/src/commands/sessions.rs`
- Modify: `crates/tt-cli/src/commands/mod.rs`
- Modify: `crates/tt-cli/src/lib.rs`

**Step 1: Update cli.rs**

Remove `Sessions` command:
```rust
    /// Manage Claude Code session indexing.
    Sessions {
        #[command(subcommand)]
        action: SessionsAction,
    },
```

Remove `SessionsAction` enum:
```rust
/// Sessions subcommand actions.
#[derive(Debug, Subcommand)]
pub enum SessionsAction {
    /// Index Claude Code sessions from ~/.claude/projects/.
    ///
    /// Scans session JSONL files and stores metadata in the database.
    Index,
}
```

Update `IngestEvent` enum to add `Sessions` variant:
```rust
/// Event types that can be ingested.
#[derive(Debug, Subcommand)]
pub enum IngestEvent {
    /// Record a pane focus event.
    PaneFocus {
        /// The tmux pane ID (e.g., %3).
        #[arg(long)]
        pane: String,

        /// The current working directory of the pane.
        #[arg(long)]
        cwd: String,

        /// The tmux session name.
        #[arg(long)]
        session: String,

        /// The tmux window index (optional).
        #[arg(long)]
        window: Option<u32>,
    },

    /// Index Claude Code sessions from ~/.claude/projects/.
    ///
    /// Scans session JSONL files and stores metadata in the database.
    Sessions,
}
```

**Step 2: Update lib.rs exports**

Remove `SessionsAction`:
```rust
pub use cli::{Cli, Commands, IngestEvent, StreamsAction};
```

**Step 3: Move sessions functionality to ingest.rs**

Add session indexing functionality to `crates/tt-cli/src/commands/ingest.rs`. Add at the end of the file (before tests):

```rust
// ========== Sessions Indexing ==========

use std::path::PathBuf;
use tt_core::session::{ClaudeSession, scan_claude_sessions};
use tt_db::StoredEvent;

/// Run the sessions index command.
///
/// Scans `~/.claude/projects/` for Claude Code session files and upserts
/// them into the database.
pub fn index_sessions(db: &tt_db::Database) -> Result<()> {
    let projects_dir = get_claude_projects_dir()?;

    if !projects_dir.exists() {
        println!(
            "No Claude Code projects directory found at: {}",
            projects_dir.display()
        );
        println!("Sessions will be indexed once Claude Code creates session files.");
        return Ok(());
    }

    println!("Scanning {}...", projects_dir.display());

    let sessions =
        scan_claude_sessions(&projects_dir).context("failed to scan Claude Code sessions")?;

    if sessions.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }

    let mut event_count = 0usize;
    for session in &sessions {
        db.upsert_claude_session(session)
            .with_context(|| format!("failed to upsert session {}", session.claude_session_id))?;

        let events = create_session_events(session);
        event_count += events.len();
        db.insert_events(&events).with_context(|| {
            format!(
                "failed to insert events for session {}",
                session.claude_session_id
            )
        })?;
    }

    println!(
        "Indexed {} sessions ({} events)",
        sessions.len(),
        event_count
    );

    let mut projects: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for session in &sessions {
        *projects.entry(&session.project_name).or_default() += 1;
    }

    let mut project_list: Vec<_> = projects.iter().collect();
    project_list.sort_by(|a, b| b.1.cmp(a.1));

    println!("\nSessions by project:");
    for (project, count) in project_list.iter().take(10) {
        println!("  {project}: {count} sessions");
    }

    if project_list.len() > 10 {
        println!("  ... and {} more projects", project_list.len() - 10);
    }

    Ok(())
}

/// Create events from a Claude session.
fn create_session_events(session: &ClaudeSession) -> Vec<StoredEvent> {
    use serde_json::json;

    let mut events = Vec::new();

    events.push(StoredEvent {
        id: format!("{}-session_start", session.claude_session_id),
        timestamp: session.start_time,
        event_type: "session_start".to_string(),
        source: "claude".to_string(),
        schema_version: 1,
        data: json!({
            "claude_session_id": session.claude_session_id,
            "project_path": session.project_path,
            "project_name": session.project_name,
        }),
        cwd: Some(session.project_path.clone()),
        session_id: Some(session.claude_session_id.clone()),
        stream_id: None,
        assignment_source: None,
    });

    for ts in &session.user_message_timestamps {
        events.push(StoredEvent {
            id: format!(
                "{}-user_message-{}",
                session.claude_session_id,
                ts.timestamp_millis()
            ),
            timestamp: *ts,
            event_type: "user_message".to_string(),
            source: "claude".to_string(),
            schema_version: 1,
            data: json!({
                "claude_session_id": session.claude_session_id,
                "project_path": session.project_path,
            }),
            cwd: Some(session.project_path.clone()),
            session_id: Some(session.claude_session_id.clone()),
            stream_id: None,
            assignment_source: None,
        });
    }

    if let Some(end_time) = session.end_time {
        events.push(StoredEvent {
            id: format!("{}-session_end", session.claude_session_id),
            timestamp: end_time,
            event_type: "session_end".to_string(),
            source: "claude".to_string(),
            schema_version: 1,
            data: json!({
                "claude_session_id": session.claude_session_id,
                "project_path": session.project_path,
            }),
            cwd: Some(session.project_path.clone()),
            session_id: Some(session.claude_session_id.clone()),
            stream_id: None,
            assignment_source: None,
        });
    }

    events
}

/// Get the Claude Code projects directory path.
fn get_claude_projects_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME environment variable not set")?;
    Ok(PathBuf::from(home).join(".claude").join("projects"))
}
```

Add the tests from sessions.rs to ingest.rs tests module:
```rust
    #[test]
    fn test_create_session_events_session_start() {
        use chrono::TimeZone;
        use tt_core::session::ClaudeSession;

        let session = ClaudeSession {
            claude_session_id: "test-session-123".to_string(),
            parent_session_id: None,
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
        };

        let events = create_session_events(&session);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "session_start");
        assert_eq!(events[0].id, "test-session-123-session_start");
        assert_eq!(events[0].source, "claude");
        assert_eq!(events[0].cwd, Some("/home/user/project".to_string()));
        assert_eq!(events[0].session_id, Some("test-session-123".to_string()));
    }

    #[test]
    fn test_create_session_events_session_start_and_end() {
        use chrono::TimeZone;
        use tt_core::session::ClaudeSession;

        let session = ClaudeSession {
            claude_session_id: "test-session-456".to_string(),
            parent_session_id: None,
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
        };

        let events = create_session_events(&session);

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "session_start");
        assert_eq!(events[1].event_type, "session_end");
        assert_eq!(events[1].id, "test-session-456-session_end");
    }

    #[test]
    fn test_create_session_events_user_messages() {
        use chrono::TimeZone;
        use tt_core::session::ClaudeSession;

        let ts1 = Utc.with_ymd_and_hms(2026, 2, 2, 10, 5, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2026, 2, 2, 10, 10, 0).unwrap();

        let session = ClaudeSession {
            claude_session_id: "test-session-789".to_string(),
            parent_session_id: None,
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
        };

        let events = create_session_events(&session);

        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type, "session_start");
        assert_eq!(events[1].event_type, "user_message");
        assert_eq!(events[1].timestamp, ts1);
        assert_eq!(events[2].event_type, "user_message");
        assert_eq!(events[2].timestamp, ts2);
    }

    #[test]
    fn test_get_claude_projects_dir() {
        if std::env::var("HOME").is_ok() {
            let path = get_claude_projects_dir().unwrap();
            assert!(path.ends_with("projects"));
            assert!(path.to_string_lossy().contains(".claude"));
        }
    }
```

**Step 4: Delete sessions.rs**

```bash
rm crates/tt-cli/src/commands/sessions.rs
```

**Step 5: Update mod.rs**

Remove:
```rust
pub mod sessions;
```

**Step 6: Update main.rs**

Remove `sessions` from imports and `SessionsAction` from lib imports:
```rust
use tt_cli::commands::{
    context, events, export, import, ingest, recompute, report, status,
    streams, suggest, sync, tag,
};
use tt_cli::{Cli, Commands, Config, IngestEvent, StreamsAction};
```

Remove Sessions match arm:
```rust
        Some(Commands::Sessions { action }) => {
            let (db, _config) = open_database(cli.config.as_deref())?;
            match action {
                SessionsAction::Index => sessions::index(&db)?,
            }
        }
```

Update Ingest match arm to handle Sessions:
```rust
        Some(Commands::Ingest { event }) => {
            match event {
                IngestEvent::PaneFocus {
                    pane,
                    cwd,
                    session,
                    window,
                } => {
                    let written = ingest::ingest_pane_focus(pane, session, *window, cwd)?;
                    if written {
                        tracing::debug!("event ingested");
                    } else {
                        tracing::debug!("event debounced");
                    }
                }
                IngestEvent::Sessions => {
                    let (db, _config) = open_database(cli.config.as_deref())?;
                    ingest::index_sessions(&db)?;
                }
            }
        }
```

**Step 7: Verify compilation and tests**

Run: `cargo build -p tt-cli && cargo test -p tt-cli`
Expected: PASS

---

### Task 5: Run Full Test Suite and Verify

**Step 1: Run all tests**

Run: `cargo test`
Expected: All tests pass

**Step 2: Run clippy**

Run: `cargo clippy --all-targets`
Expected: No warnings

**Step 3: Check formatting**

Run: `cargo fmt --check`
Expected: No formatting issues

**Step 4: Manual verification**

Verify the new CLI structure:
```bash
cargo run -- --help
cargo run -- streams --help
cargo run -- ingest --help
```

Expected output should show:
- `tt streams list` and `tt streams create`
- `tt ingest pane-focus` and `tt ingest sessions`
- No `infer`, `week`, `today`, `yesterday`, `stream`, or `sessions` at top level

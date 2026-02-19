# Multi-Machine Sync Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Enable time tracking across multiple remote dev servers with a local laptop as the aggregation point, including XDG directory cleanup, machine identity, namespaced event IDs, schema v8, sync command, and per-machine stream boundaries.

**Architecture:** Add machine UUID as a first-class concept. Each remote gets a persistent UUID via `tt init`. Event IDs are prefixed with the machine UUID to prevent collisions. A new `tt sync` command does incremental SSH pulls with per-remote sync state. All directory paths migrate to XDG conventions. Schema bumps from v7 to v8 (breaking — fresh DB required).

**Tech Stack:** Rust, SQLite (rusqlite), uuid (already in workspace), dirs (already in workspace), SSH (std::process::Command), serde/serde_json

**Design doc:** `docs/plans/2026-02-19-multi-machine-sync-design.md`

---

### Task 1: Add XDG directory helpers to config.rs

**Files:**
- Modify: `crates/tt-cli/src/config.rs:64-66`

**Step 1: Write tests for new path helpers**

Add to a new `#[cfg(test)] mod tests` at the bottom of `config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dirs_data_path_returns_some() {
        // dirs::data_dir() returns Some on all supported platforms
        assert!(dirs_data_path().is_some());
    }

    #[test]
    fn test_dirs_state_path_returns_some() {
        assert!(dirs_state_path().is_some());
    }

    #[test]
    fn test_dirs_data_path_ends_with_tt() {
        let path = dirs_data_path().unwrap();
        assert_eq!(path.file_name().unwrap(), "tt");
    }

    #[test]
    fn test_dirs_state_path_ends_with_tt() {
        let path = dirs_state_path().unwrap();
        assert_eq!(path.file_name().unwrap(), "tt");
    }

    #[test]
    fn test_default_config_uses_data_dir_for_db() {
        let config = Config::default();
        let data_dir = dirs_data_path().unwrap();
        assert_eq!(config.database_path, data_dir.join("tt.db"));
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p tt-cli --lib config::tests -- -v`
Expected: FAIL — `dirs_data_path` and `dirs_state_path` don't exist yet

**Step 3: Add the helper functions and update default**

In `crates/tt-cli/src/config.rs`, add after the existing `dirs_config_path()` (line 66):

```rust
/// Returns the platform-specific data directory for tt.
///
/// On Linux: `~/.local/share/tt`
pub fn dirs_data_path() -> Option<PathBuf> {
    dirs::data_dir().map(|p| p.join("tt"))
}

/// Returns the platform-specific state directory for tt.
///
/// On Linux: `~/.local/state/tt`
pub fn dirs_state_path() -> Option<PathBuf> {
    dirs::state_dir().map(|p| p.join("tt"))
}
```

Make both `pub` (they'll be used by `ingest.rs` and `export.rs`).

Update `Config::default()` (line 27) to use `dirs_data_path` instead of `dirs_config_path`:

```rust
impl Default for Config {
    fn default() -> Self {
        let data_dir = dirs_data_path().unwrap_or_else(|| PathBuf::from("."));
        Self {
            database_path: data_dir.join("tt.db"),
        }
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p tt-cli --lib config::tests -- -v`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/tt-cli/src/config.rs
git commit -m "feat: add XDG data and state directory helpers

Move default database_path from ~/.config/tt/ to ~/.local/share/tt/
to follow XDG Base Directory conventions."
```

---

### Task 2: Update ingest.rs to use XDG data path

**Files:**
- Modify: `crates/tt-cli/src/commands/ingest.rs:140-144`

**Step 1: Update default_data_dir()**

Replace the `default_data_dir()` function (lines 140-145):

```rust
/// Returns the default time tracker data directory.
fn default_data_dir() -> PathBuf {
    crate::config::dirs_data_path()
        .unwrap_or_else(|| PathBuf::from("."))
}
```

This replaces `dirs::home_dir().join(".time-tracker")` with `dirs::data_dir().join("tt")`.

**Step 2: Run existing ingest tests**

Run: `cargo test -p tt-cli ingest -- -v`
Expected: PASS — tests use `tempfile::tempdir()` so don't depend on the default path

**Step 3: Commit**

```bash
git add crates/tt-cli/src/commands/ingest.rs
git commit -m "refactor: use XDG data dir for ingest events"
```

---

### Task 3: Update export.rs to use XDG data and state paths

**Files:**
- Modify: `crates/tt-cli/src/commands/export.rs:107-112, 131-143`

**Step 1: Update default_data_dir()**

Replace lines 107-112:

```rust
/// Returns the default time tracker data directory.
fn default_data_dir() -> PathBuf {
    crate::config::dirs_data_path()
        .unwrap_or_else(|| PathBuf::from("."))
}
```

**Step 2: Update the manifest path in run_impl()**

In `run_impl()` (line 141), the manifest path currently resolves to `data_dir.join("claude-manifest.json")`. Change it to use the state directory:

```rust
fn run_impl(data_dir: &Path, claude_dir: &Path, output: &mut dyn Write) -> Result<()> {
    // Export tmux events
    let events_file = data_dir.join("events.jsonl");
    if events_file.exists() {
        export_tmux_events(&events_file, output)?;
    }

    // Export Claude events with incremental parsing
    if claude_dir.exists() {
        let state_dir = crate::config::dirs_state_path()
            .unwrap_or_else(|| data_dir.to_path_buf());
        let manifest_path = state_dir.join("claude-manifest.json");
        export_claude_events(claude_dir, &manifest_path, output)?;
    }

    Ok(())
}
```

**Step 3: Run existing export tests**

Run: `cargo test -p tt-cli export -- -v`
Expected: PASS — tests inject paths via `run_impl()` parameters

**Step 4: Commit**

```bash
git add crates/tt-cli/src/commands/export.rs
git commit -m "refactor: use XDG data/state dirs for export"
```

---

### Task 4: Update tmux-hook.conf and README

**Files:**
- Modify: `config/tmux-hook.conf`
- Modify: `README.md`

**Step 1: Update tmux-hook.conf**

Change all `$HOME/.time-tracker/hook.log` references to `$HOME/.local/state/tt/hook.log`. Also add a `mkdir -p` to ensure the state dir exists. The hook lines (26-32) become:

```conf
set-hook -ga pane-focus-in 'run-shell -b "mkdir -p $HOME/.local/state/tt && tt ingest pane-focus --pane \"#{pane_id}\" --cwd \"#{pane_current_path}\" --session \"#{session_name}\" --window \"#{window_index}\" 2>>$HOME/.local/state/tt/hook.log"'
set-hook -ga after-select-pane 'run-shell -b "mkdir -p $HOME/.local/state/tt && tt ingest pane-focus --pane \"#{pane_id}\" --cwd \"#{pane_current_path}\" --session \"#{session_name}\" --window \"#{window_index}\" 2>>$HOME/.local/state/tt/hook.log"'
set-hook -ga session-window-changed 'run-shell -b "mkdir -p $HOME/.local/state/tt && tt ingest pane-focus --pane \"#{pane_id}\" --cwd \"#{pane_current_path}\" --session \"#{session_name}\" --window \"#{window_index}\" 2>>$HOME/.local/state/tt/hook.log"'
set-hook -ga client-session-changed 'run-shell -b "mkdir -p $HOME/.local/state/tt && tt ingest pane-focus --pane \"#{pane_id}\" --cwd \"#{pane_current_path}\" --session \"#{session_name}\" --window \"#{window_index}\" 2>>$HOME/.local/state/tt/hook.log"'
```

Also update the comments (lines 14-15) from `~/.time-tracker/` to the new paths.

**Step 2: Update README.md**

Replace all path references:
- `~/.time-tracker/events.jsonl` → `~/.local/share/tt/events.jsonl`
- `~/.time-tracker/hook.log` → `~/.local/state/tt/hook.log`
- `~/.time-tracker/` → `~/.local/share/tt/` (data context) or `~/.local/state/tt/` (log/state context)
- Fix the existing inconsistency: the data layout table (line 223-226) should show `~/.local/share/tt/` for both events.jsonl and tt.db
- Update the database default comment (line 212) from `~/.local/share/tt/tt.db` (already correct in README, now matches code)

**Step 3: Commit**

```bash
git add config/tmux-hook.conf README.md
git commit -m "docs: update all paths to XDG conventions

Replace ~/.time-tracker/ with ~/.local/share/tt/ (data)
and ~/.local/state/tt/ (logs, manifests)."
```

---

### Task 5: Add machine identity module

**Files:**
- Create: `crates/tt-cli/src/machine.rs`
- Modify: `crates/tt-cli/src/main.rs` (add `pub mod machine`)

**Step 1: Write tests for machine identity**

Create `crates/tt-cli/src/machine.rs`:

```rust
//! Machine identity management.
//!
//! Each machine gets a persistent UUID stored in `machine.json`.
//! This UUID is used to namespace event IDs for multi-machine sync.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Machine identity stored in `machine.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MachineIdentity {
    /// Persistent UUID for this machine.
    pub machine_id: String,
    /// Human-friendly label (e.g., "devbox").
    pub label: String,
}

/// Returns the path to machine.json in the XDG data directory.
pub fn machine_json_path() -> Result<PathBuf> {
    let data_dir = crate::config::dirs_data_path()
        .context("could not determine data directory")?;
    Ok(data_dir.join("machine.json"))
}

/// Loads machine identity from machine.json.
///
/// Returns `None` if the file doesn't exist.
/// Returns an error if the file exists but is unreadable/unparseable.
pub fn load_machine_identity() -> Result<Option<MachineIdentity>> {
    load_from(&machine_json_path()?)
}

/// Loads machine identity from a specific path (for testing).
fn load_from(path: &Path) -> Result<Option<MachineIdentity>> {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let identity: MachineIdentity =
                serde_json::from_str(&content).context("failed to parse machine.json")?;
            Ok(Some(identity))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("failed to read machine.json"),
    }
}

/// Loads machine identity, failing with a helpful message if not found.
///
/// Use this in commands that require machine identity (ingest, export).
pub fn require_machine_identity() -> Result<MachineIdentity> {
    load_machine_identity()?.context(
        "No machine identity found. Run 'tt init' first."
    )
}

/// Initializes machine identity.
///
/// If machine.json already exists, returns the existing identity
/// (updating the label if a new one is provided).
/// If it doesn't exist, generates a new UUID and writes machine.json.
pub fn init_machine(label: Option<&str>) -> Result<MachineIdentity> {
    init_machine_at(&machine_json_path()?, label)
}

/// Initializes machine identity at a specific path (for testing).
fn init_machine_at(path: &Path, label: Option<&str>) -> Result<MachineIdentity> {
    let default_label = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string());

    let identity = match load_from(path)? {
        Some(mut existing) => {
            if let Some(new_label) = label {
                existing.label = new_label.to_string();
                save_to(path, &existing)?;
            }
            existing
        }
        None => {
            let identity = MachineIdentity {
                machine_id: Uuid::new_v4().to_string(),
                label: label.unwrap_or(&default_label).to_string(),
            };
            save_to(path, &identity)?;
            identity
        }
    };

    Ok(identity)
}

/// Writes machine identity to a specific path.
fn save_to(path: &Path, identity: &MachineIdentity) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("failed to create data directory")?;
    }
    let json = serde_json::to_string_pretty(identity).context("failed to serialize identity")?;
    std::fs::write(path, json).context("failed to write machine.json")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_creates_new_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");

        let identity = init_machine_at(&path, Some("testbox")).unwrap();
        assert_eq!(identity.label, "testbox");
        assert!(!identity.machine_id.is_empty());
        // Verify it's a valid UUID
        Uuid::parse_str(&identity.machine_id).unwrap();
    }

    #[test]
    fn test_init_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");

        let first = init_machine_at(&path, Some("testbox")).unwrap();
        let second = init_machine_at(&path, None).unwrap();
        assert_eq!(first.machine_id, second.machine_id);
        assert_eq!(first.label, second.label);
    }

    #[test]
    fn test_init_updates_label() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");

        let first = init_machine_at(&path, Some("old-name")).unwrap();
        let second = init_machine_at(&path, Some("new-name")).unwrap();
        assert_eq!(first.machine_id, second.machine_id);
        assert_eq!(second.label, "new-name");
    }

    #[test]
    fn test_load_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");

        assert!(load_from(&path).unwrap().is_none());
    }

    #[test]
    fn test_load_existing_returns_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");

        init_machine_at(&path, Some("testbox")).unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert_eq!(loaded.label, "testbox");
    }

    #[test]
    fn test_require_fails_when_missing() {
        // This tests the error message path - load_from with nonexistent returns None,
        // require wraps it with context
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("machine.json");
        let result = load_from(&path).unwrap();
        assert!(result.is_none());
    }
}
```

**Step 2: Add hostname dependency to workspace**

In root `Cargo.toml`, add to `[workspace.dependencies]`:

```toml
hostname = "0.4"
```

In `crates/tt-cli/Cargo.toml`, add to `[dependencies]`:

```toml
hostname = "0.4"
```

**Step 3: Register the module**

In `crates/tt-cli/src/main.rs`, add at the top (or wherever modules are declared). Since `main.rs` doesn't declare modules (it imports from `tt_cli`), check how the crate is structured. The lib exports are in the binary via `use tt_cli::...`. You'll need to add `pub mod machine;` in the library root.

Check if there's a `lib.rs` — if not, the modules are in `main.rs`. Looking at the imports, `use tt_cli::{Cli, Commands, Config, IngestEvent, StreamsAction};` tells us there IS a lib.rs. Find and add `pub mod machine;` to it.

**Step 4: Run tests**

Run: `cargo test -p tt-cli machine::tests -- -v`
Expected: PASS

**Step 5: Commit**

```bash
git add Cargo.toml crates/tt-cli/Cargo.toml crates/tt-cli/src/machine.rs crates/tt-cli/src/lib.rs
git commit -m "feat: add machine identity module

Persistent UUID per machine stored in ~/.local/share/tt/machine.json.
tt init generates the UUID; subsequent calls are idempotent."
```

---

### Task 6: Add `tt init` CLI subcommand

**Files:**
- Modify: `crates/tt-cli/src/cli.rs` (add `Init` variant to `Commands`)
- Create: `crates/tt-cli/src/commands/init.rs`
- Modify: `crates/tt-cli/src/commands/mod.rs` (add `pub mod init;`)
- Modify: `crates/tt-cli/src/main.rs` (add dispatch)

**Step 1: Add the CLI definition**

In `crates/tt-cli/src/cli.rs`, add to the `Commands` enum (after `Status`):

```rust
    /// Initialize machine identity for multi-machine sync.
    ///
    /// Generates a persistent UUID for this machine, stored in
    /// `~/.local/share/tt/machine.json`. Idempotent — safe to run again.
    Init {
        /// Human-friendly label for this machine (defaults to hostname).
        #[arg(long)]
        label: Option<String>,
    },
```

**Step 2: Create the command implementation**

Create `crates/tt-cli/src/commands/init.rs`:

```rust
//! Init command for establishing machine identity.

use anyhow::Result;
use crate::machine;

/// Runs the init command.
pub fn run(label: Option<&str>) -> Result<()> {
    let identity = machine::init_machine(label)?;

    println!("Machine ID: {}", identity.machine_id);
    println!("Label:      {}", identity.label);
    println!("Saved to:   {}", machine::machine_json_path()?.display());

    Ok(())
}
```

**Step 3: Register module and dispatch**

In `crates/tt-cli/src/commands/mod.rs`, add:

```rust
pub mod init;
```

In `crates/tt-cli/src/main.rs`, add the import and match arm. In the `use` line (~line 7), add `init`. In the `match &cli.command` block, add before `None =>`:

```rust
        Some(Commands::Init { label }) => {
            init::run(label.as_deref())?;
        }
```

**Step 4: Run build and verify**

Run: `cargo build -p tt-cli`
Expected: builds without errors

Run: `cargo test -p tt-cli -- -v`
Expected: all existing tests still pass

**Step 5: Commit**

```bash
git add crates/tt-cli/src/cli.rs crates/tt-cli/src/commands/init.rs crates/tt-cli/src/commands/mod.rs crates/tt-cli/src/main.rs
git commit -m "feat: add tt init subcommand

Initializes machine identity (UUID + label) for multi-machine sync."
```

---

### Task 7: Prepend machine_id to event IDs in ingest.rs

**Files:**
- Modify: `crates/tt-cli/src/commands/ingest.rs:105-131, 265-339`

**Step 1: Write test for machine-prefixed event IDs**

Add to the existing tests in `ingest.rs`:

```rust
    #[test]
    fn test_event_id_includes_machine_id() {
        let temp_dir = tempfile::tempdir().unwrap();
        let data_dir = temp_dir.path().join("data");
        let machine_path = temp_dir.path().join("machine.json");

        // Create machine identity
        let identity = crate::machine::init_machine_at_path(&machine_path, Some("test")).unwrap();

        ingest_pane_focus_with_machine(&data_dir, &identity.machine_id, "%1", "main", Some(0), "/home/test").unwrap();

        let events = read_events_from(&data_dir).unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].id.starts_with(&identity.machine_id),
            "event ID '{}' should start with machine_id '{}'",
            events[0].id, identity.machine_id);
    }
```

**Step 2: Update IngestEvent::pane_focus() to accept machine_id**

Change the `pane_focus` constructor (line 107) to take `machine_id: &str` as the first parameter:

```rust
impl IngestEvent {
    /// Creates a new pane focus event with a deterministic ID.
    pub fn pane_focus(
        machine_id: &str,
        pane_id: String,
        tmux_session: String,
        window_index: Option<u32>,
        cwd: String,
        timestamp: DateTime<Utc>,
    ) -> Self {
        let timestamp_str = timestamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let id = format!("{machine_id}:remote.tmux:tmux_pane_focus:{timestamp_str}:{pane_id}");
        // ... rest unchanged
    }
}
```

**Step 3: Update ingest_pane_focus_impl and public API**

Add `machine_id: &str` parameter to `ingest_pane_focus_impl()` (line 272) and pass it through to `IngestEvent::pane_focus()`.

Update the public `ingest_pane_focus()` (line 326) to load the machine identity:

```rust
pub fn ingest_pane_focus(
    pane_id: &str,
    session_name: &str,
    window_index: Option<u32>,
    cwd: &str,
) -> Result<bool> {
    let identity = crate::machine::require_machine_identity()?;
    ingest_pane_focus_impl(
        &default_data_dir(),
        &identity.machine_id,
        pane_id,
        session_name,
        window_index,
        cwd,
    )
}
```

Also create a test-friendly variant that accepts a data_dir and machine_id.

**Step 4: Fix all existing ingest tests**

Existing tests call `ingest_pane_focus_impl()` directly. Update them to pass a machine_id (use a fixed UUID like `"test-machine-00000000-0000-0000-0000-000000000000"`). Update the expected event ID format in any assertions.

**Step 5: Run tests**

Run: `cargo test -p tt-cli ingest -- -v`
Expected: PASS

**Step 6: Commit**

```bash
git add crates/tt-cli/src/commands/ingest.rs
git commit -m "feat: prepend machine_id to ingest event IDs

Events now include the machine UUID prefix for multi-machine
deduplication. Requires tt init before first use."
```

---

### Task 8: Prepend machine_id to event IDs in export.rs

**Files:**
- Modify: `crates/tt-cli/src/commands/export.rs:122-129, 382-460`

**Step 1: Update run() to load machine identity**

Update `run()` (line 123) to load the machine identity and pass it through:

```rust
pub fn run() -> Result<()> {
    let identity = crate::machine::require_machine_identity()?;
    run_impl(
        &default_data_dir(),
        &default_claude_dir(),
        &identity.machine_id,
        &mut std::io::stdout(),
    )
}
```

**Step 2: Thread machine_id through run_impl and emit functions**

Add `machine_id: &str` parameter to `run_impl()`, `export_claude_events()`, `export_single_claude_log()`, `process_claude_entry()`, and all the `emit_*` functions.

In each `emit_*` function, prepend the machine_id to the event ID. For example, in `emit_session_start()` (line 390):

```rust
id: format!("{machine_id}:remote.agent:agent_session:{timestamp}:{session_id}:started"),
```

Same pattern for `emit_user_message()` and `emit_tool_uses()`.

**Step 3: Note about tmux event passthrough**

`export_tmux_events()` passes through events.jsonl lines verbatim. Since Task 7 already makes ingest write machine-prefixed IDs, these events will already have the prefix. No change needed to `export_tmux_events()`.

**Step 4: Fix existing export tests**

Tests call `run_impl()` directly. Update them to pass a test machine_id. Update ID assertions to expect the prefix.

**Step 5: Run tests**

Run: `cargo test -p tt-cli export -- -v`
Expected: PASS

**Step 6: Commit**

```bash
git add crates/tt-cli/src/commands/export.rs
git commit -m "feat: prepend machine_id to export event IDs

Claude session events now include the machine UUID prefix."
```

---

### Task 9: Add machine_id to StoredEvent and update schema to v8

**Files:**
- Modify: `crates/tt-db/src/lib.rs:40, 116-187, 363-444, 470-501`

**Step 1: Add machine_id field to StoredEvent**

In the `StoredEvent` struct (after line 128, the `source` field), add:

```rust
    /// Machine UUID that generated this event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
```

**Step 2: Bump schema version**

Change line 40:

```rust
const SCHEMA_VERSION: i32 = 8;
```

**Step 3: Add machine_id column to events table**

In the `init()` `CREATE TABLE events` SQL (line 371-391), add after `source TEXT NOT NULL`:

```sql
                machine_id TEXT,
```

Add an index:

```sql
            CREATE INDEX IF NOT EXISTS idx_events_machine ON events(machine_id);
```

**Step 4: Add machines table**

In the `init()` SQL batch, add after the `agent_sessions` table:

```sql
            -- Machines table: tracks known remote machines for sync
            CREATE TABLE IF NOT EXISTS machines (
                machine_id TEXT PRIMARY KEY,
                label TEXT,
                last_sync_at TEXT,
                last_event_id TEXT
            );
```

**Step 5: Add machine_id column to agent_sessions table**

In the `CREATE TABLE agent_sessions` SQL, add after `tool_call_count INTEGER DEFAULT 0`:

```sql
                machine_id TEXT
```

**Step 6: Update insert_events()**

Update the INSERT statement in `insert_events()` (line 476) to include `machine_id`:

```rust
let mut stmt = tx.prepare(
    "INSERT OR IGNORE INTO events (id, timestamp, type, source, machine_id, schema_version, cwd, git_project, git_workspace, pane_id, tmux_session, window_index, status, idle_duration_ms, action, session_id, stream_id, assignment_source)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
)?;
```

Add `event.machine_id` to the params list (after `event.source`).

**Step 7: Update all event-reading queries**

Search for all `SELECT ... FROM events` queries and add `machine_id` to the column list. Update the corresponding row-to-struct mappings.

**Step 8: Update upsert_agent_session()**

Add `machine_id` parameter to the INSERT and ON CONFLICT DO UPDATE statements.

**Step 9: Run tests**

Run: `cargo test -p tt-db -- -v`
Expected: PASS (tests use `open_in_memory()` which creates fresh schema)

**Step 10: Commit**

```bash
git add crates/tt-db/src/lib.rs
git commit -m "feat: schema v8 with machine_id column and machines table

Breaking schema change - old databases will fail to open.
Add machine_id to events and agent_sessions tables.
Add machines table for sync state tracking."
```

---

### Task 10: Populate machine_id during import

**Files:**
- Modify: `crates/tt-cli/src/commands/import.rs:33-99`

**Step 1: Write test for machine_id extraction during import**

Add to import tests:

```rust
    #[test]
    fn test_import_extracts_machine_id() {
        let db = Database::open_in_memory().unwrap();

        let event = r#"{"id":"a1b2c3d4-e5f6-7890-abcd-ef1234567890:remote.tmux:tmux_pane_focus:2025-01-29T12:00:00.000Z:%1","timestamp":"2025-01-29T12:00:00.000Z","source":"remote.tmux","type":"tmux_pane_focus","pane_id":"%1","tmux_session":"main","cwd":"/tmp"}"#;
        let input = Cursor::new(format!("{event}\n"));
        import_from_reader(&db, input).unwrap();

        let events = db.get_events_in_range(
            &DateTime::parse_from_rfc3339("2025-01-29T00:00:00Z").unwrap().with_timezone(&Utc),
            &DateTime::parse_from_rfc3339("2025-01-30T00:00:00Z").unwrap().with_timezone(&Utc),
        ).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].machine_id.as_deref(), Some("a1b2c3d4-e5f6-7890-abcd-ef1234567890"));
    }
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p tt-cli import::tests::test_import_extracts_machine_id -- -v`
Expected: FAIL — machine_id not being extracted

**Step 3: Add machine_id extraction in import_from_reader**

After deserializing a `StoredEvent` (line 65), extract the machine_id from the event ID prefix:

```rust
            Ok(mut event) => {
                event.stream_id = None;
                event.assignment_source = None;

                // Extract machine_id from event ID prefix (UUID before first colon-separated source)
                if event.machine_id.is_none() {
                    event.machine_id = extract_machine_id(&event.id);
                }

                result.total_read += 1;
                batch.push(event);
                // ...
            }
```

Add the extraction helper:

```rust
/// Extracts the machine UUID prefix from an event ID.
///
/// Event IDs are formatted as `{machine_uuid}:{source}:{type}:{timestamp}:{discriminator}`.
/// Returns `None` if the ID doesn't start with a valid UUID.
fn extract_machine_id(event_id: &str) -> Option<String> {
    // UUID v4 is exactly 36 chars: 8-4-4-4-12
    if event_id.len() > 36 && event_id.as_bytes()[36] == b':' {
        let candidate = &event_id[..36];
        if uuid::Uuid::parse_str(candidate).is_ok() {
            return Some(candidate.to_string());
        }
    }
    None
}
```

**Step 4: Run tests**

Run: `cargo test -p tt-cli import -- -v`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/tt-cli/src/commands/import.rs
git commit -m "feat: extract machine_id from event ID prefix during import"
```

---

### Task 11: Add machine_id to context export

**Files:**
- Modify: `crates/tt-cli/src/commands/context.rs:40-60`

**Step 1: Add machine_id field to EventExport**

In the `EventExport` struct, add:

```rust
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
```

**Step 2: Populate it from StoredEvent**

Find where `EventExport` is constructed from a `StoredEvent` and add `machine_id: event.machine_id.clone()`.

**Step 3: Run tests and update snapshots if needed**

Run: `cargo test -p tt-cli context -- -v`

If snapshot tests exist for context output, run `cargo insta review` to update them.

**Step 4: Commit**

```bash
git add crates/tt-cli/src/commands/context.rs
git commit -m "feat: include machine_id in context export

Stream inference can now see which machine each event came from."
```

---

### Task 12: Add machine_id to session event creation in ingest.rs

**Files:**
- Modify: `crates/tt-cli/src/commands/ingest.rs:424-479`

**Step 1: Update create_session_events() to set machine_id**

The `create_session_events()` function (line 424) creates `StoredEvent`s from `AgentSession`s. The events it creates need `machine_id` set. Since these are created from locally-scanned sessions (the `ingest sessions` command runs on the machine where sessions live), read the local machine identity:

In `index_sessions()` (line 350), load the machine identity at the start and pass it through:

```rust
pub fn index_sessions(db: &tt_db::Database) -> Result<()> {
    let machine_id = crate::machine::load_machine_identity()?
        .map(|m| m.machine_id);
    // ... existing code ...
    let events = create_session_events(session, machine_id.as_deref());
    // ...
}
```

In `create_session_events()`, add `machine_id: Option<&str>` parameter and set it on each event in the `make_event` closure:

```rust
    let make_event = |id_suffix: &str,
                      timestamp: chrono::DateTime<chrono::Utc>,
                      event_type: EventType| StoredEvent {
        id: format!("{}-{id_suffix}", session.session_id),
        machine_id: machine_id.map(String::from),
        // ... rest unchanged
    };
```

**Step 2: Run tests**

Run: `cargo test -p tt-cli ingest -- -v`
Expected: PASS

**Step 3: Commit**

```bash
git add crates/tt-cli/src/commands/ingest.rs
git commit -m "feat: set machine_id on session-derived events"
```

---

### Task 13: Add `tt sync` subcommand

**Files:**
- Modify: `crates/tt-cli/src/cli.rs` (add `Sync` variant)
- Create: `crates/tt-cli/src/commands/sync.rs`
- Modify: `crates/tt-cli/src/commands/mod.rs`
- Modify: `crates/tt-cli/src/main.rs`

**Step 1: Add CLI definition**

In `crates/tt-cli/src/cli.rs`, add to `Commands`:

```rust
    /// Sync events from remote machine(s) via SSH.
    ///
    /// Runs `tt export` on each remote via SSH and imports the events
    /// into the local database. Tracks sync position per remote for
    /// incremental pulls.
    Sync {
        /// Remote host(s) to sync from (SSH alias or user@host).
        #[arg(required = true)]
        remotes: Vec<String>,
    },
```

**Step 2: Create the sync command**

Create `crates/tt-cli/src/commands/sync.rs`:

```rust
//! Sync command for pulling events from remote machines via SSH.

use std::io::Cursor;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::commands::{import, ingest, recompute};

/// Runs the sync command for one or more remotes.
pub fn run(db: &tt_db::Database, remotes: &[String]) -> Result<()> {
    for remote in remotes {
        println!("Syncing from {remote}...");
        sync_single(db, remote)?;
    }

    // Reindex sessions and recompute after all syncs
    println!("\nIndexing sessions...");
    ingest::index_sessions(db)?;
    println!("Recomputing time...");
    recompute::run(db, false)?;

    Ok(())
}

/// Syncs events from a single remote.
fn sync_single(db: &tt_db::Database, remote: &str) -> Result<()> {
    // Look up last sync position for this remote
    let last_event_id = db.get_machine_last_event_id_by_label(remote)?;

    // Build SSH command
    let mut export_cmd = String::from("tt export");
    if let Some(ref last_id) = last_event_id {
        export_cmd.push_str(&format!(" --after {last_id}"));
    }

    let output = Command::new("ssh")
        .arg(remote)
        .arg(&export_cmd)
        .output()
        .with_context(|| format!("failed to SSH to {remote}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("remote tt export failed on {remote}: {stderr}");
    }

    if output.stdout.is_empty() {
        println!("  No new events from {remote}");
        return Ok(());
    }

    // Import the events
    let reader = Cursor::new(output.stdout);
    let result = import::import_from_reader(db, reader)?;

    println!(
        "  Imported {} events ({} duplicates, {} malformed)",
        result.inserted, result.duplicates, result.malformed
    );

    // Extract machine_id from first imported event to register/update the machine
    // The machine_id comes from the event ID prefix
    if result.inserted > 0 || result.duplicates > 0 {
        // Get the most recent event to find the machine_id
        if let Some(machine_id) = find_machine_id_from_remote(db, remote)? {
            db.upsert_machine(&machine_id, remote)?;
        }
    }

    Ok(())
}

/// Finds the machine_id associated with events from a remote.
///
/// Looks at the most recent events that match the remote label pattern.
fn find_machine_id_from_remote(db: &tt_db::Database, _remote: &str) -> Result<Option<String>> {
    // For now, look at the most recently inserted event's machine_id
    // A more robust approach would track this in the sync flow
    Ok(db.get_latest_machine_id()?)
}
```

**Step 3: Register and dispatch**

Add `pub mod sync;` to `commands/mod.rs`.

In `main.rs`, add the dispatch:

```rust
        Some(Commands::Sync { remotes }) => {
            let (db, _config) = open_database(cli.config.as_deref())?;
            sync::run(&db, remotes)?;
        }
```

**Step 4: Add DB helper methods**

In `crates/tt-db/src/lib.rs`, add methods:

```rust
    /// Inserts or updates a machine entry.
    pub fn upsert_machine(
        &self,
        machine_id: &str,
        label: &str,
    ) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT INTO machines (machine_id, label, last_sync_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(machine_id) DO UPDATE SET
                label = excluded.label,
                last_sync_at = excluded.last_sync_at",
            params![machine_id, label, format_timestamp(Utc::now())],
        )?;
        Ok(())
    }

    /// Gets the last event ID synced from a machine identified by label.
    pub fn get_machine_last_event_id_by_label(
        &self,
        label: &str,
    ) -> Result<Option<String>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT last_event_id FROM machines WHERE label = ?1",
        )?;
        let result = stmt.query_row(params![label], |row| row.get(0)).optional()?;
        Ok(result)
    }

    /// Gets the machine_id from the most recently inserted event that has one.
    pub fn get_latest_machine_id(&self) -> Result<Option<String>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT machine_id FROM events WHERE machine_id IS NOT NULL ORDER BY timestamp DESC LIMIT 1",
        )?;
        let result = stmt.query_row([], |row| row.get(0)).optional()?;
        Ok(result)
    }
```

You'll need to add `use rusqlite::OptionalExtension;` if not already imported.

**Step 5: Run build**

Run: `cargo build -p tt-cli`
Expected: builds successfully

**Step 6: Commit**

```bash
git add crates/tt-cli/src/cli.rs crates/tt-cli/src/commands/sync.rs crates/tt-cli/src/commands/mod.rs crates/tt-cli/src/main.rs crates/tt-db/src/lib.rs
git commit -m "feat: add tt sync subcommand

Pulls events from remote machines via SSH, imports into local DB,
tracks sync state per machine."
```

---

### Task 14: Add --after flag to tt export

**Files:**
- Modify: `crates/tt-cli/src/cli.rs` (add `after` field to `Export`)
- Modify: `crates/tt-cli/src/commands/export.rs:122-146`
- Modify: `crates/tt-cli/src/main.rs` (pass `after` to export)

**Step 1: Add CLI flag**

Change `Export` in `cli.rs` from a unit variant to a struct variant:

```rust
    /// Export all events for sync to local machine.
    ///
    /// Reads events from `~/.local/share/tt/events.jsonl` and parses Claude Code
    /// session logs, outputting combined events as JSONL to stdout.
    Export {
        /// Only export events after this event ID (for incremental sync).
        #[arg(long)]
        after: Option<String>,
    },
```

**Step 2: Thread --after through export_tmux_events**

Update `run()` to accept the after parameter:

```rust
pub fn run(after: Option<&str>) -> Result<()> {
    let identity = crate::machine::require_machine_identity()?;
    run_impl(
        &default_data_dir(),
        &default_claude_dir(),
        &identity.machine_id,
        after,
        &mut std::io::stdout(),
    )
}
```

In `export_tmux_events()`, add filtering: if `after` is `Some`, skip lines until finding the event with that ID, then emit everything after it. Since events.jsonl is append-only and chronological, this is a simple scan-and-skip:

```rust
fn export_tmux_events(events_file: &Path, after: Option<&str>, output: &mut dyn Write) -> Result<()> {
    let file = File::open(events_file).context("failed to open events.jsonl")?;
    let reader = BufReader::new(file);
    let mut past_marker = after.is_none(); // If no --after, emit everything

    for (line_num, line) in reader.lines().enumerate() {
        // ... existing error handling ...

        if !past_marker {
            // Check if this line contains the marker event ID
            if let Some(after_id) = after {
                if line.contains(after_id) {
                    past_marker = true;
                }
            }
            continue; // Skip this line and all before it
        }

        // ... existing JSON validation and output ...
    }

    Ok(())
}
```

**Step 3: Update main.rs dispatch**

```rust
        Some(Commands::Export { after }) => {
            export::run(after.as_deref())?;
        }
```

**Step 4: Run tests**

Run: `cargo test -p tt-cli export -- -v`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/tt-cli/src/cli.rs crates/tt-cli/src/commands/export.rs crates/tt-cli/src/main.rs
git commit -m "feat: add --after flag to tt export for incremental sync"
```

---

### Task 15: Add `tt machines` subcommand

**Files:**
- Modify: `crates/tt-cli/src/cli.rs`
- Create: `crates/tt-cli/src/commands/machines.rs`
- Modify: `crates/tt-cli/src/commands/mod.rs`
- Modify: `crates/tt-cli/src/main.rs`

**Step 1: Add CLI definition**

In `cli.rs`, add to `Commands`:

```rust
    /// List known remote machines and their sync status.
    Machines,
```

**Step 2: Create command**

Create `crates/tt-cli/src/commands/machines.rs`:

```rust
//! Machines command for listing known remotes.

use anyhow::Result;
use tt_db::Database;

pub fn run(db: &Database) -> Result<()> {
    let machines = db.list_machines()?;

    if machines.is_empty() {
        println!("No machines registered yet. Run 'tt sync <remote>' to import from a remote.");
        return Ok(());
    }

    println!("{:<38} {:<20} {}", "MACHINE ID", "LABEL", "LAST SYNC");
    for machine in &machines {
        let last_sync = machine.last_sync_at.as_deref().unwrap_or("never");
        println!("{:<38} {:<20} {}", machine.machine_id, machine.label, last_sync);
    }

    Ok(())
}
```

**Step 3: Add DB types and query**

In `crates/tt-db/src/lib.rs`, add a struct and query method:

```rust
/// A known remote machine.
#[derive(Debug, Clone)]
pub struct Machine {
    pub machine_id: String,
    pub label: String,
    pub last_sync_at: Option<String>,
    pub last_event_id: Option<String>,
}
```

```rust
    /// Lists all known machines.
    pub fn list_machines(&self) -> Result<Vec<Machine>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT machine_id, label, last_sync_at, last_event_id FROM machines ORDER BY label",
        )?;
        let machines = stmt.query_map([], |row| {
            Ok(Machine {
                machine_id: row.get(0)?,
                label: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                last_sync_at: row.get(2)?,
                last_event_id: row.get(3)?,
            })
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(machines)
    }
```

**Step 4: Register and dispatch**

Add `pub mod machines;` to `commands/mod.rs`. Add dispatch to `main.rs`:

```rust
        Some(Commands::Machines) => {
            let (db, _config) = open_database(cli.config.as_deref())?;
            machines::run(&db)?;
        }
```

**Step 5: Run build**

Run: `cargo build -p tt-cli`
Expected: builds successfully

**Step 6: Commit**

```bash
git add crates/tt-cli/src/cli.rs crates/tt-cli/src/commands/machines.rs crates/tt-cli/src/commands/mod.rs crates/tt-cli/src/main.rs crates/tt-db/src/lib.rs
git commit -m "feat: add tt machines subcommand

Lists known remote machines with their sync status."
```

---

### Task 16: Update snapshot tests

**Files:**
- Modify: snapshot files in `crates/tt-cli/src/commands/snapshots/`

**Step 1: Run all tests and identify failures**

Run: `cargo test -p tt-cli -- -v`

Snapshot tests will fail because StoredEvent now has a `machine_id` field and event IDs have changed format.

**Step 2: Review and accept snapshot changes**

Run: `cargo insta review`

Carefully review each snapshot diff to ensure changes are only:
- Addition of `machine_id` field
- Updated event ID format with UUID prefix
- No unintended changes

**Step 3: Run clippy**

Run: `cargo clippy --all-targets`
Expected: zero warnings

**Step 4: Run full test suite**

Run: `cargo test`
Expected: all pass

**Step 5: Commit**

```bash
git add -A
git commit -m "test: update snapshots for schema v8 and machine_id"
```

---

### Task 17: Update AGENTS.md and documentation

**Files:**
- Modify: `AGENTS.md`
- Modify: `README.md` (if not fully done in Task 4)

**Step 1: Update AGENTS.md**

Add `tt init` and `tt sync` and `tt machines` to the commands section. Update the Key Types table to include `Machine` and `MachineIdentity`. Update the Structure section to mention `machine.rs`. Update the "Where to Look" table.

**Step 2: Update README.md**

Update the Multi-Machine Setup section to reflect the actual `tt init` + `tt sync` workflow. Document the XDG directory layout. Add the one-time migration note for existing events.jsonl.

**Step 3: Commit**

```bash
git add AGENTS.md README.md
git commit -m "docs: update for multi-machine sync support"
```

---

### Task 18: Run full CI checks

**Step 1: Format check**

Run: `cargo fmt --check`
Expected: no formatting issues

**Step 2: Lint**

Run: `cargo clippy --all-targets`
Expected: zero warnings

**Step 3: Full test suite**

Run: `cargo test`
Expected: all pass

**Step 4: Dependency audit**

Run: `cargo deny check`
Expected: no issues (hostname crate should be fine)

**Step 5: Fix any issues found, then final commit if needed**

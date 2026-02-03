# Git Project Field Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add explicit `git_project` and `git_workspace` fields to events so project names are correctly captured from git repository info rather than directory names.

**Architecture:** Rename `jj_project`/`jj_workspace` → `git_project`/`git_workspace` throughout. Add explicit fields to `StoredEvent`. Remove the `#[serde(flatten)]` hack on the `data` field. Bump schema version (breaking change - old databases will fail to open).

**Tech Stack:** Rust, SQLite, serde

**Breaking Change:** This is intentionally backward-incompatible. Old databases will not open. Users must delete their database and re-sync from `events.jsonl`.

---

### Task 1: Rename fields in IngestEvent and helper function

**Files:**
- Modify: `crates/tt-cli/src/commands/ingest.rs`

**Step 1: Rename struct fields (lines 38-44)**

```rust
    /// The git project name (from git remote origin).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_project: Option<String>,
    /// The git workspace name (if in a non-default workspace).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_workspace: Option<String>,
```

**Step 2: Rename helper function (line 50)**

```rust
/// Get project identity for a directory using jj/git commands.
fn get_git_identity(cwd: &std::path::Path) -> Option<ProjectIdentity> {
```

**Step 3: Update constructor (lines 109-121)**

```rust
        let git_identity = get_git_identity(Path::new(&cwd));

        Self {
            // ... other fields ...
            git_project: git_identity.as_ref().map(|i| i.project_name.clone()),
            git_workspace: git_identity.and_then(|i| i.workspace_name),
        }
```

**Step 4: Update tests (lines 915-937)**

```rust
    assert_eq!(events[0].git_project, Some("my-project".to_string()));
```
and:
```rust
    assert_eq!(events[0].git_project, None);
    assert_eq!(events[0].git_workspace, None);
```

**Step 5: Run tests**

Run: `cargo test -p tt-cli ingest`
Expected: PASS

**Step 6: Commit**

```bash
jj describe -m "refactor(ingest): rename jj_project to git_project"
```

---

### Task 2: Add explicit fields to StoredEvent and remove flatten hack

**Files:**
- Modify: `crates/tt-db/src/lib.rs`

**Step 1: Add explicit fields to StoredEvent (after line 142)**

```rust
    /// Git project name (from remote origin).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_project: Option<String>,

    /// Git workspace name (if in a non-default workspace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_workspace: Option<String>,
```

**Step 2: Remove the flatten hack (lines 134-138)**

Delete:
```rust
    /// Type-specific JSON payload.
    /// When deserializing, unknown fields are collected here via `#[serde(flatten)]`.
    /// This allows both nested `data` format and flat format to work.
    #[serde(default, flatten)]
    pub data: serde_json::Value,
```

Replace with explicit tmux fields (since we know exactly what fields exist):
```rust
    /// Tmux pane ID (for tmux_pane_focus events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,

    /// Tmux session name (for tmux_pane_focus events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_session: Option<String>,

    /// Tmux window index (for tmux_pane_focus events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_index: Option<u32>,
```

**Step 3: Update InferableEvent impl (lines 183-185)**

```rust
    fn git_project(&self) -> Option<&str> {
        self.git_project.as_deref()
    }
```

**Step 4: Remove AllocatableEvent::data() method if no longer needed**

Check if anything uses `event.data()` and remove or update accordingly.

**Step 5: Run tests**

Run: `cargo test -p tt-db`
Expected: Some tests may fail if they rely on the `data` field - fix them

**Step 6: Commit**

```bash
jj describe -m "refactor(db): add explicit fields to StoredEvent, remove flatten hack"
```

---

### Task 3: Update database schema (breaking change)

**Files:**
- Modify: `crates/tt-db/src/lib.rs`

**Step 1: Bump schema version**

Find `const SCHEMA_VERSION: i32 = 5;` and change to:
```rust
const SCHEMA_VERSION: i32 = 6;
```

**Step 2: Update CREATE TABLE (lines 293-310)**

```sql
CREATE TABLE IF NOT EXISTS events (
    id TEXT PRIMARY KEY,
    timestamp TEXT NOT NULL,
    type TEXT NOT NULL,
    source TEXT NOT NULL,
    schema_version INTEGER DEFAULT 1,
    cwd TEXT,
    git_project TEXT,
    git_workspace TEXT,
    pane_id TEXT,
    tmux_session TEXT,
    window_index INTEGER,
    session_id TEXT,
    stream_id TEXT,
    assignment_source TEXT DEFAULT 'inferred'
);
```

**Step 3: Update INSERT statement**

Find the insert_events function and update to include all new columns.

**Step 4: Update SELECT statement**

Find get_events and update to read all new columns.

**Step 5: Remove any migration functions for old versions**

Since this is a clean break, remove `migrate_from_v*` functions or simplify to just fail on old schemas.

**Step 6: Run tests**

Run: `cargo test -p tt-db`
Expected: PASS

**Step 7: Commit**

```bash
jj describe -m "feat(db): update schema to v6 with explicit event fields (breaking)"
```

---

### Task 4: Rename trait method and update inference

**Files:**
- Modify: `crates/tt-core/src/inference.rs`

**Step 1: Rename trait method (line 53)**

```rust
    fn git_project(&self) -> Option<&str>;
```

**Step 2: Update comments (lines 231, 240-241)**

```rust
/// - Prefers `git_project` from the first event if available
...
    // Try git_project from first event
    let base_name = events.first().and_then(|e| e.git_project()).map_or_else(
```

**Step 3: Update test helper struct (lines 307-362)**

```rust
        git_project: Option<String>,
...
        fn with_git_project(mut self, project: &str) -> Self {
            self.git_project = Some(project.to_string());
            self
        }
...
        fn git_project(&self) -> Option<&str> {
            self.git_project.as_deref()
        }
```

**Step 4: Rename all test functions (lines 591-660)**

- `test_jj_project_preferred_over_directory` → `test_git_project_preferred_over_directory`
- `test_fallback_to_directory_without_jj_project` → `test_fallback_to_directory_without_git_project`
- `test_jj_project_same_name_different_directories` → `test_git_project_same_name_different_directories`
- `test_mixed_jj_project_and_no_jj_project` → `test_mixed_git_project_and_no_git_project`

Update all `.with_jj_project(` to `.with_git_project(` in test bodies.

**Step 5: Run tests**

Run: `cargo test -p tt-core`
Expected: PASS

**Step 6: Commit**

```bash
jj describe -m "refactor(core): rename jj_project to git_project in inference"
```

---

### Task 5: Final verification and cleanup

**Step 1: Search for any remaining jj_project references**

Run: `grep -r "jj_project" crates/`
Expected: No matches

**Step 2: Update project.rs doc comment (line 1)**

```rust
//! Git project identity extraction.
```

**Step 3: Run full test suite**

Run: `cargo test`
Expected: PASS

**Step 4: Run clippy**

Run: `cargo clippy --all-targets`
Expected: No warnings

**Step 5: Test end-to-end**

```bash
# Delete old database (breaking change)
rm -f ~/.local/share/tt/events.db

# Ingest a test event
cargo run -- ingest --pane-id %999 --tmux-session test --cwd /home/sami/time-tracker/default

# Check the events file
tail -1 ~/.time-tracker/events.jsonl | jq '{git_project, git_workspace, cwd}'

# Import and verify
cargo run -- import < ~/.time-tracker/events.jsonl
cargo run -- events | tail -1 | jq '{git_project, git_workspace}'
```

Expected: `git_project` and `git_workspace` fields present and round-trip correctly.

**Step 6: Commit**

```bash
jj describe -m "feat: rename jj_project to git_project for proper project identification

BREAKING CHANGE: Schema version bumped to 6. Old databases will not open.
Delete ~/.local/share/tt/events.db and re-import from events.jsonl."
```

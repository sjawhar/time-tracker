# OpenCode Session Ingestion Implementation Plan

**Goal:** Extend session ingestion to support both Claude Code and OpenCode with a unified `AgentSession` model.

**Design:** See `2026-02-05-opencode-session-ingestion.md`

**Breaking change (pre-alpha):** No migration/backfill. Existing `claude_sessions` data is discarded; re-ingest required (delete DB if needed).

---

## Task 1: Add SessionSource Enum

**Files:**
- Modify: `crates/tt-core/src/session.rs`

**Step 1:** Add `SessionSource` enum after `SessionType`:

```rust
/// Source of the coding session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionSource {
    #[default]
    Claude,
    OpenCode,
}

impl SessionSource {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::OpenCode => "opencode",
        }
    }
}

impl std::fmt::Display for SessionSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for SessionSource {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claude" => Ok(Self::Claude),
            "opencode" => Ok(Self::OpenCode),
            _ => Err(format!("invalid session source: {s}")),
        }
    }
}
```

---

## Task 2: Rename ClaudeSession to AgentSession

**Files:**
- Modify: `crates/tt-core/src/session.rs`
- Modify: `crates/tt-core/src/lib.rs`

**Step 1:** In `session.rs`, rename the struct and field:

- `ClaudeSession` → `AgentSession`
- `claude_session_id` → `session_id`
- Add `source: SessionSource` field after `session_id`

```rust
/// An indexed coding assistant session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSession {
    pub session_id: String,
    /// Source tool (Claude Code or OpenCode).
    #[serde(default)]
    pub source: SessionSource,
    pub parent_session_id: Option<String>,
    // ... rest unchanged
}
```

**Step 2:** Update `parse_session_file` to set `source: SessionSource::Claude` and use `session_id` parameter name.

**Step 3:** Update `lib.rs` exports:
```rust
pub use session::{AgentSession, SessionSource, SessionType, parse_session_file, scan_claude_sessions, extract_project_name};
```

**Step 4:** Fix all test struct initializations to use new names and add `source` field.

---

## Task 3: Update tt-db Schema

**Files:**
- Modify: `crates/tt-db/src/lib.rs`

**Step 1:** Increment `SCHEMA_VERSION` (next version).

Session IDs have distinct formats per source (Claude: UUIDs or `agent-*`, OpenCode: `ses_*`), so no collision is possible. Single-column primary key is sufficient.

**Step 2:** Update table creation SQL - rename `claude_sessions` to `agent_sessions`, rename `claude_session_id` to `session_id`, add `source` column:

```sql
CREATE TABLE IF NOT EXISTS agent_sessions (
    session_id TEXT PRIMARY KEY,
    source TEXT NOT NULL DEFAULT 'claude',
    parent_session_id TEXT,
    session_type TEXT NOT NULL DEFAULT 'user',
    project_path TEXT NOT NULL,
    project_name TEXT NOT NULL,
    start_time TEXT NOT NULL,
    end_time TEXT,
    message_count INTEGER NOT NULL,
    summary TEXT,
    user_prompts TEXT DEFAULT '[]',
    starting_prompt TEXT,
    assistant_message_count INTEGER DEFAULT 0,
    tool_call_count INTEGER DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_agent_sessions_start_time ON agent_sessions(start_time);
CREATE INDEX IF NOT EXISTS idx_agent_sessions_project_path ON agent_sessions(project_path);
CREATE INDEX IF NOT EXISTS idx_agent_sessions_parent ON agent_sessions(parent_session_id);
```

**Step 3:** Rename methods:
- `upsert_claude_session` → `upsert_agent_session`
- `claude_sessions_in_range` → `agent_sessions_in_range`
- Update SQL in these methods to use new table/column names

**Step 4:** Update method signatures to use `AgentSession` and include `source` in INSERT/SELECT.

---

## Task 4: Create OpenCode Parser Module

**Files:**
- Create: `crates/tt-core/src/opencode.rs`
- Modify: `crates/tt-core/src/lib.rs`

**Step 1:** Create `opencode.rs` with structs for OpenCode JSON format:

```rust
//! OpenCode session parsing.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use chrono::{DateTime, TimeZone, Utc};
use rayon::prelude::*;
use serde::Deserialize;

use crate::session::{AgentSession, SessionError, SessionSource, SessionType, extract_project_name};

/// OpenCode session metadata.
#[derive(Debug, Deserialize)]
struct OpenCodeSession {
    id: String,
    directory: String,
    title: Option<String>,
    #[serde(rename = "parentID")]
    parent_id: Option<String>,
    time: OpenCodeTime,
}

#[derive(Debug, Deserialize)]
struct OpenCodeTime {
    created: i64,  // unix ms
    updated: Option<i64>,
}

/// OpenCode message metadata.
#[derive(Debug, Deserialize)]
struct OpenCodeMessage {
    id: String,
    role: String,
    time: OpenCodeMessageTime,
}

#[derive(Debug, Deserialize)]
struct OpenCodeMessageTime {
    created: i64,
}

/// OpenCode message part.
#[derive(Debug, Deserialize)]
struct OpenCodePart {
    #[serde(rename = "type")]
    part_type: String,
    text: Option<String>,
}
```

**Step 2:** Add parsing function:

```rust
/// Maximum user prompts to extract.
const MAX_USER_PROMPTS: usize = 5;
const MAX_PROMPT_LENGTH: usize = 2000;
const MAX_USER_MESSAGE_TIMESTAMPS: usize = 1000;

fn truncate_prompt(content: &str) -> String {
    if content.len() <= MAX_PROMPT_LENGTH {
        return content.to_string();
    }
    let mut end = MAX_PROMPT_LENGTH;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &content[..end])
}

fn unix_ms_to_datetime(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(ms).single().unwrap_or_else(Utc::now)
}

pub fn parse_opencode_session(
    storage_dir: &Path,
    session_file: &Path,
) -> Result<AgentSession, SessionError> {
    // Implementation: read session, messages, parts
    // Return AgentSession with source: SessionSource::OpenCode
    todo!()
}
```

Reuse `SessionError` (extend with OpenCode-specific variants only if needed).

**Step 3:** Add scanner function:

```rust
/// Scan OpenCode storage directory for sessions.
pub fn scan_opencode_sessions(storage_dir: &Path) -> Result<Vec<AgentSession>, SessionError> {
    let session_dir = storage_dir.join("session");
    if !session_dir.exists() {
        return Ok(Vec::new());
    }

    let mut session_files = Vec::new();
    
    // Collect all session files
    for project_entry in fs::read_dir(&session_dir)? {
        let project_path = project_entry?.path();
        if !project_path.is_dir() {
            continue;
        }
        for session_entry in fs::read_dir(&project_path)? {
            let session_path = session_entry?.path();
            if session_path.extension().is_some_and(|e| e == "json") {
                session_files.push(session_path);
            }
        }
    }

    // Parse in parallel
    let sessions: Vec<AgentSession> = session_files
        .par_iter()
        .filter_map(|path| {
            parse_opencode_session(storage_dir, path).ok()
        })
        .collect();

    Ok(sessions)
}
```

**Step 4:** Add to `lib.rs`:
```rust
pub mod opencode;
pub use opencode::scan_opencode_sessions;
```

**Step 5:** Write unit tests with sample data.

---

## Task 5: Implement OpenCode Parser Logic

**Files:**
- Modify: `crates/tt-core/src/opencode.rs`

**Step 1:** Implement `parse_opencode_session`:

1. Read session JSON file
2. Read all message files from `message/{session_id}/`
3. For each user message, read parts from `part/{message_id}/`
4. Count assistant messages and tool parts
5. Build and return `AgentSession`
6. Use session `time.created` for `start_time`; `end_time` from last message or `time.updated`

**Step 2:** Handle edge cases:
- Missing message directories: treat as empty session (message count 0) and use session timestamps
- Missing part directories: ignore parts for that message
- Malformed JSON files: skip session with a warning
- Empty sessions: allow (message count 0, no prompts)
- Sort messages by `time.created` before counting/extracting prompts

---

## Task 6: Update CLI Ingest Command

**Files:**
- Modify: `crates/tt-cli/src/commands/ingest.rs`

**Step 1:** Update imports:
```rust
use tt_core::session::{AgentSession, scan_claude_sessions};
use tt_core::opencode::scan_opencode_sessions;
```

**Step 2:** Rename `index_sessions` internals:
- `scan_claude_sessions` stays as-is
- Add call to `scan_opencode_sessions`
- Combine results

**Step 3:** Update `get_claude_projects_dir` → keep for Claude, add `get_opencode_storage_dir`:
```rust
fn get_opencode_storage_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME environment variable not set")?;
    Ok(PathBuf::from(home).join(".local/share/opencode/storage"))
}
```

**Step 4:** Update `index_sessions` to scan both:
```rust
pub fn index_sessions(db: &tt_db::Database) -> Result<()> {
    let mut all_sessions = Vec::new();
    
    // Claude Code
    let claude_dir = get_claude_projects_dir()?;
    if claude_dir.exists() {
        println!("Scanning Claude Code sessions...");
        let claude_sessions = scan_claude_sessions(&claude_dir)?;
        println!("  Found {} Claude sessions", claude_sessions.len());
        all_sessions.extend(claude_sessions);
    }
    
    // OpenCode
    let opencode_dir = get_opencode_storage_dir()?;
    if opencode_dir.exists() {
        println!("Scanning OpenCode sessions...");
        let opencode_sessions = scan_opencode_sessions(&opencode_dir)?;
        println!("  Found {} OpenCode sessions", opencode_sessions.len());
        all_sessions.extend(opencode_sessions);
    }
    
    // ... rest of indexing logic
}
```

**Step 5:** Update `create_session_events` to use `AgentSession` and set event source based on `session.source`.

**Step 6:** Update database calls to use renamed methods.

---

## Task 7: Update Remaining References

**Files:**
- Modify: `crates/tt-cli/src/commands/context.rs` (if it uses ClaudeSession)
- Modify: any other files referencing old names

**Step 1:** Search for remaining references:
```bash
rg "ClaudeSession|claude_session_id|claude_sessions" crates/
```

**Step 2:** Update each reference to new names.

---

## Task 8: Final Verification

**Step 1:** Run all checks:
```bash
cargo clippy --all-targets
cargo fmt --check
cargo test --all
```

**Step 2:** Manual verification:
```bash
cargo run -- ingest sessions
```

**Step 3:** Verify output shows both sources:
```
Scanning Claude Code sessions...
  Found 120 Claude sessions
Scanning OpenCode sessions...
  Found 30 OpenCode sessions
Indexed 150 sessions (X events)
```

**Step 4:** Query to verify data:
```bash
cargo run -- context --days 1
```

---

## Summary

| Task | Description | Crate |
|------|-------------|-------|
| 1 | Add `SessionSource` enum | tt-core |
| 2 | Rename `ClaudeSession` → `AgentSession` | tt-core |
| 3 | Update database schema (agent_sessions table) | tt-db |
| 4 | Create OpenCode parser module (skeleton) | tt-core |
| 5 | Implement OpenCode parser logic | tt-core |
| 6 | Update CLI ingest command | tt-cli |
| 7 | Update remaining references | all |
| 8 | Final verification | - |

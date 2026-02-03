# Session Type Field Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add a `session_type` field to distinguish user sessions from autonomous agent sessions, making it easy to query only user-initiated activity.

**Architecture:** Add a `SessionType` enum (`user`, `agent`, `subagent`) to `tt-core`, derive the type from session ID patterns during parsing, persist to the `claude_sessions` table, and propagate to events for efficient querying.

**Tech Stack:** Rust, SQLite, serde

---

## Background

Session IDs follow these patterns:
- **User sessions:** UUID format (e.g., `d66718b7-3b37-47c8-b3a6-f01b637d8c13`)
- **Background agents:** `agent-aprompt_suggestion-*`, `agent-acompact-*`
- **Task subagents:** `agent-a{7-char-hash}` (e.g., `agent-a913a65`)

Currently, filtering requires string pattern matching on every query. This plan adds a typed field for efficient filtering.

---

## Task 1: Add SessionType Enum to tt-core

**Files:**
- Modify: `crates/tt-core/src/session.rs:1-45`

**Step 1: Write the enum and derive function**

Add after the imports, before `ClaudeSession`:

```rust
/// Type of Claude Code session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionType {
    /// Direct user session (UUID format, no agent- prefix)
    #[default]
    User,
    /// Background agent (prompt_suggestion, compact)
    Agent,
    /// Task tool subagent (agent-a{hash})
    Subagent,
}

impl SessionType {
    /// Derive session type from session ID.
    #[must_use]
    pub fn from_session_id(session_id: &str) -> Self {
        if !session_id.starts_with("agent-") {
            Self::User
        } else if session_id.contains("prompt_suggestion") || session_id.contains("compact") {
            Self::Agent
        } else {
            Self::Subagent
        }
    }

    /// Returns the string representation for SQL storage.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
            Self::Subagent => "subagent",
        }
    }
}

impl std::fmt::Display for SessionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for SessionType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user" => Ok(Self::User),
            "agent" => Ok(Self::Agent),
            "subagent" => Ok(Self::Subagent),
            _ => Err(format!("invalid session type: {s}")),
        }
    }
}
```

**Step 2: Run clippy to verify no errors**

Run: `cargo clippy -p tt-core --all-targets`
Expected: No errors (warnings OK for now)

**Step 3: Commit**

```bash
jj describe -m "feat(core): add SessionType enum for user/agent/subagent distinction"
```

---

## Task 2: Add session_type Field to ClaudeSession

**Files:**
- Modify: `crates/tt-core/src/session.rs` (ClaudeSession struct ~line 42-66)

**Step 1: Add field to struct**

Add after `parent_session_id`:

```rust
    /// Type of session (user, agent, subagent).
    #[serde(default)]
    pub session_type: SessionType,
```

**Step 2: Export SessionType from lib.rs**

In `crates/tt-core/src/lib.rs`, update the session re-export to include `SessionType`:

```rust
pub use session::{ClaudeSession, SessionType, index_sessions, parse_session_file};
```

**Step 3: Run clippy**

Run: `cargo clippy -p tt-core --all-targets`
Expected: Errors about missing `session_type` in struct initialization (expected, will fix in next tasks)

**Step 4: Commit**

```bash
jj describe -m "feat(core): add session_type field to ClaudeSession"
```

---

## Task 3: Derive session_type During Parsing

**Files:**
- Modify: `crates/tt-core/src/session.rs` (parse_session_file function ~line 143-240)

**Step 1: Set session_type in parse_session_file**

Find the `ClaudeSession` construction (around line 230-240) and add:

```rust
        session_type: SessionType::from_session_id(session_id),
```

**Step 2: Fix test struct initializations**

Search for `ClaudeSession {` in the file and add `session_type: SessionType::User,` (or derive from the test session_id) to each test instance.

**Step 3: Run tests**

Run: `cargo test -p tt-core`
Expected: All tests pass

**Step 4: Commit**

```bash
jj describe -m "feat(core): derive session_type from session ID during parsing"
```

---

## Task 4: Add Tests for SessionType

**Files:**
- Modify: `crates/tt-core/src/session.rs` (test module at end of file)

**Step 1: Write unit tests for SessionType::from_session_id**

Add to the `#[cfg(test)]` module:

```rust
    #[test]
    fn test_session_type_from_user_session() {
        let session_id = "d66718b7-3b37-47c8-b3a6-f01b637d8c13";
        assert_eq!(SessionType::from_session_id(session_id), SessionType::User);
    }

    #[test]
    fn test_session_type_from_prompt_suggestion_agent() {
        let session_id = "agent-aprompt_suggestion-05a0b3";
        assert_eq!(SessionType::from_session_id(session_id), SessionType::Agent);
    }

    #[test]
    fn test_session_type_from_compact_agent() {
        let session_id = "agent-acompact-63da16";
        assert_eq!(SessionType::from_session_id(session_id), SessionType::Agent);
    }

    #[test]
    fn test_session_type_from_task_subagent() {
        let session_id = "agent-a913a65";
        assert_eq!(SessionType::from_session_id(session_id), SessionType::Subagent);
    }

    #[test]
    fn test_session_type_roundtrip() {
        for st in [SessionType::User, SessionType::Agent, SessionType::Subagent] {
            let s = st.as_str();
            let parsed: SessionType = s.parse().unwrap();
            assert_eq!(parsed, st);
        }
    }
```

**Step 2: Run tests**

Run: `cargo test -p tt-core session_type`
Expected: All 5 tests pass

**Step 3: Commit**

```bash
jj describe -m "test(core): add SessionType derivation tests"
```

---

## Task 5: Update Database Schema

**Files:**
- Modify: `crates/tt-db/src/lib.rs`

**Step 1: Bump SCHEMA_VERSION**

Change line 40:
```rust
const SCHEMA_VERSION: i32 = 5;
```

**Step 2: Add column to claude_sessions table**

Find `CREATE TABLE IF NOT EXISTS claude_sessions` (around line 336) and add after `parent_session_id TEXT,`:

```sql
                session_type TEXT NOT NULL DEFAULT 'user',
```

Do this for ALL schema version blocks (v2, v3, v4 migrations if they have claude_sessions).

**Step 3: Add migration for v4 -> v5**

Find the migration match block and add a case for v4:

```rust
                    4 => {
                        // v4 -> v5: Add session_type column to claude_sessions
                        self.conn.execute_batch(
                            "ALTER TABLE claude_sessions ADD COLUMN session_type TEXT NOT NULL DEFAULT 'user';",
                        )?;
                    }
```

**Step 4: Run tests**

Run: `cargo test -p tt-db`
Expected: Tests pass (may need to update expected schema version in tests)

**Step 5: Commit**

```bash
jj describe -m "feat(db): add session_type column to claude_sessions (schema v5)"
```

---

## Task 6: Update Session Insert/Query in tt-db

**Files:**
- Modify: `crates/tt-db/src/lib.rs` (insert_claude_sessions and related functions)

**Step 1: Update INSERT statement**

Find `INSERT INTO claude_sessions` (around line 1032) and add `session_type` to the column list and values.

**Step 2: Update SELECT statement**

Find `SELECT claude_session_id, parent_session_id` (around line 1078) and add `session_type` to the select list.

**Step 3: Update row parsing**

Where `ClaudeSession` is constructed from row data, add:
```rust
session_type: row.get::<_, String>(N)?.parse().unwrap_or_default(),
```
(Adjust N for the column index)

**Step 4: Fix test struct initializations**

Add `session_type: SessionType::User,` to test `ClaudeSession` instances in tt-db.

**Step 5: Run tests**

Run: `cargo test -p tt-db`
Expected: All tests pass

**Step 6: Commit**

```bash
jj describe -m "feat(db): persist and retrieve session_type for claude_sessions"
```

---

## Task 7: Update Event Creation to Include session_type

**Files:**
- Modify: `crates/tt-cli/src/commands/ingest.rs` (create_session_events ~line 397)

**Step 1: Add session_type to event data**

In `create_session_events`, update the `make_event` closure to include session_type in the data JSON:

```rust
            data: json!({
                "claude_session_id": session.claude_session_id,
                "project_path": session.project_path,
                "session_type": session.session_type.as_str(),
            }),
```

**Step 2: Fix test struct initializations in ingest.rs**

Add `session_type: SessionType::User,` (or appropriate type) to test `ClaudeSession` instances.

**Step 3: Run tests**

Run: `cargo test -p tt-cli`
Expected: All tests pass

**Step 4: Commit**

```bash
jj describe -m "feat(cli): include session_type in user_message event data"
```

---

## Task 8: Update Context Command to Filter by session_type

**Files:**
- Modify: `crates/tt-cli/src/commands/context.rs`

**Step 1: Fix any ClaudeSession struct initializations**

Add `session_type: SessionType::User,` to any test or placeholder `ClaudeSession` instances.

**Step 2: Run tests**

Run: `cargo test -p tt-cli context`
Expected: All tests pass

**Step 3: Commit**

```bash
jj describe -m "fix(cli): update context command for session_type field"
```

---

## Task 9: Full Integration Test

**Files:**
- No new files

**Step 1: Run full test suite**

Run: `cargo test --all`
Expected: All tests pass

**Step 2: Run clippy**

Run: `cargo clippy --all-targets`
Expected: No errors

**Step 3: Run fmt check**

Run: `cargo fmt --check`
Expected: No formatting issues

**Step 4: Manual verification**

```bash
# Re-ingest sessions
cargo run -- ingest --claude

# Query events and verify session_type is populated
cargo run -- events | jq -s 'group_by(.data.session_type) | map({type: .[0].data.session_type, count: length})'
```

Expected: See counts for "user", "agent", "subagent"

**Step 5: Final commit**

```bash
jj describe -m "feat: add session_type field for user/agent/subagent distinction

Adds SessionType enum (user, agent, subagent) to distinguish:
- user: Direct user sessions (UUID format)
- agent: Background agents (prompt_suggestion, compact)
- subagent: Task tool subagents (agent-a{hash})

This enables efficient filtering of user activity without string pattern matching.

Schema version bumped to v5 with migration from v4."
```

---

## Summary

| Task | Description | Files |
|------|-------------|-------|
| 1 | Add SessionType enum | tt-core/session.rs |
| 2 | Add field to ClaudeSession | tt-core/session.rs, lib.rs |
| 3 | Derive during parsing | tt-core/session.rs |
| 4 | Add unit tests | tt-core/session.rs |
| 5 | Update DB schema | tt-db/lib.rs |
| 6 | Update DB insert/query | tt-db/lib.rs |
| 7 | Include in event data | tt-cli/ingest.rs |
| 8 | Update context command | tt-cli/context.rs |
| 9 | Integration test | - |

**Queries after implementation:**

```sql
-- User activity only
SELECT * FROM events
WHERE json_extract(data, '$.session_type') = 'user';

-- Or via claude_sessions join
SELECT e.* FROM events e
JOIN claude_sessions cs ON e.session_id = cs.claude_session_id
WHERE cs.session_type = 'user';
```

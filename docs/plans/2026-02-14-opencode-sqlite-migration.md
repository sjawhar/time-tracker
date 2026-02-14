# OpenCode SQLite Migration - Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Update OpenCode session ingestion to read from SQLite database instead of JSON files, matching OpenCode's new storage format.

**Architecture:** Replace filesystem walks + JSON deserialization with rusqlite queries against `~/.local/share/opencode/opencode.db`. The public API changes from `scan_opencode_sessions(storage_dir: &Path)` to `scan_opencode_sessions(db_path: &Path)`. Output type (`AgentSession`) is unchanged. Drop rayon parallelism (single SQLite connection with cached prepared statements is faster than opening thousands of files).

**Tech Stack:** rusqlite (already a workspace dependency, `bundled` feature includes JSON1 extension — `json_extract()` works out of the box)

---

## Context

OpenCode migrated from normalized JSON files (`storage/session/`, `storage/message/`, `storage/part/`) to a single SQLite database. The schema:

- `session`: `id`, `directory`, `title` (NOT NULL), `parent_id`, `time_created` (ms), `time_updated` (ms)
- `message`: `id`, `session_id`, `time_created` (ms), `data` (JSON with `role` field)
- `part`: `id`, `message_id`, `session_id`, `time_created` (ms), `data` (JSON with `type` + optional `text`)

Part types we care about: `text` (for user prompts), `tool` (for tool call count). Others (`step-finish`, `step-start`, `reasoning`, `agent`, `compaction`, `file`, `patch`) ignored.

## Review Notes

**Performance:** Per-session querying (3K sessions × message query + tool count query + part queries for user messages only) against a 6.3GB db. With `prepare_cached` and indexes on `message(session_id)` and `part(session_id)`, each query is an indexed lookup. Tool count uses a single `SELECT COUNT(*) ... WHERE session_id = ?1` per session rather than per-message queries. Start with this approach; if too slow, refactor to 3-4 bulk queries + HashMap aggregation in Rust.

**Correctness:**
- `json_extract()` returns SQL NULL for missing keys — all extracted values must be handled as `Option<String>` on the Rust side
- Tool count uses one per-session query: `SELECT COUNT(*) FROM part WHERE session_id = ?1 AND json_extract(data, '$.type') = 'tool'`
- `session.title` is NOT NULL but can be empty string — map empty → `None` for summary

**Robustness:**
- If the db file exists but has no `session` table (schema drift / corrupt db), the `prepare()` call will fail. Catch this as a `SessionError::Database` and return empty `Vec` with a warning, rather than propagating the error up. This matches the current graceful behavior for `scan_nonexistent_dir`.
- OpenCode uses WAL journal mode. Read-only + `SQLITE_OPEN_NO_MUTEX` is safe for concurrent reads.

**API change:** `parse_opencode_session()` is currently `pub`. Removing it is a breaking change to `tt-core`'s public API. Nothing outside the crate uses it (only `ingest.rs` uses `scan_opencode_sessions`), so this is fine — but the new `build_agent_session` should be private.

---

### Task 1: Add rusqlite dependency + SessionError variant

**Files:**
- Modify: `crates/tt-core/Cargo.toml`
- Modify: `crates/tt-core/src/session.rs` (SessionError enum, ~line 122)

**Step 1: Add rusqlite to tt-core dependencies**

In `crates/tt-core/Cargo.toml`, add to `[dependencies]`:
```toml
rusqlite.workspace = true
```

**Step 2: Add Database error variant to SessionError**

In `crates/tt-core/src/session.rs`, add to the `SessionError` enum:
```rust
#[error("database error: {0}")]
Database(#[from] rusqlite::Error),
```

**Step 3: Verify it compiles**

Run: `cargo check -p tt-core`
Expected: compiles with no errors

**Step 4: Commit**

```
feat(tt-core): add rusqlite dependency for OpenCode SQLite ingestion
```

---

### Task 2: Rewrite opencode.rs implementation + tests

**Files:**
- Rewrite: `crates/tt-core/src/opencode.rs` (entire file — implementation and tests together as one atomic change)

#### Implementation

**Remove:**
- `OpenCodeSession`, `OpenCodeTime`, `OpenCodeMessage`, `OpenCodeMessageTime`, `OpenCodePart` structs
- `MessageRole`, `PartType` enums
- `read_json_files()` helper
- `parse_opencode_session()` public function

**Keep:**
- `unix_ms_to_datetime()` helper (still needed for timestamp conversion)

**Add:**
- `SessionRow` struct (maps to SQLite session table columns: `id`, `directory`, `title`, `parent_id`, `time_created`, `time_updated`)
- `scan_opencode_sessions(db_path: &Path)` — opens read-only connection, queries all sessions, calls `build_agent_session` per session. On db open failure or missing `session` table, log a warning and return `Ok(Vec::new())`.
- `build_agent_session(conn, session_row)` — private fn, queries messages + parts for one session, aggregates into `AgentSession`

**SQL queries (4 prepared statements):**

```sql
-- 1. All sessions (run once, not cached)
SELECT id, directory, title, parent_id, time_created, time_updated FROM session

-- 2. Messages for a session (cached, role extracted from JSON data column)
SELECT id, time_created, json_extract(data, '$.role') as role
FROM message WHERE session_id = ?1 ORDER BY time_created

-- 3. Text parts for a user message (cached, for prompt extraction)
SELECT json_extract(data, '$.text') as text
FROM part WHERE message_id = ?1 AND json_extract(data, '$.type') = 'text'
ORDER BY id

-- 4. Tool count for a session (cached, one query instead of per-message)
SELECT COUNT(*) FROM part
WHERE session_id = ?1 AND json_extract(data, '$.type') = 'tool'
```

**Key implementation details:**
- Open with `OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX`
- Set `busy_timeout(Duration::from_secs(5))` for concurrent access with running OpenCode
- Use `prepare_cached` for queries 2, 3, 4 (reused per-session)
- Map empty `session.title` to `None` for `summary` field
- `time_updated` is NOT NULL in SQLite (unlike old JSON where it was Optional), simplify end_time calc
- All `json_extract()` results handled as `Option<String>` (returns NULL for missing keys)

**Message processing loop** (structurally identical to current code):
- Iterate messages sorted by `time_created`
- For user messages: query text parts (query 3), concatenate, apply `truncate_prompt`, collect timestamps
- For assistant messages: just count them (tool count comes from query 4, done once per session)
- Same limits: `MAX_USER_PROMPTS`, `MAX_USER_MESSAGE_TIMESTAMPS`, saturating arithmetic

#### Tests

**SQLite test helpers** (replace JSON fixture helpers):

```rust
fn create_test_db() -> (TempDir, PathBuf)
// Creates temp dir with opencode.db containing schema:
//   session(id, directory, title, parent_id, time_created, time_updated)
//   message(id, session_id, time_created, time_updated, data)
//   part(id, message_id, session_id, time_created, time_updated, data)
// Plus indexes on message(session_id), part(message_id), part(session_id)
// Returns (TempDir, db_path) — TempDir kept alive for RAII cleanup
// Connection closed so scan_opencode_sessions can open it

fn insert_session(db_path, id, directory, title, parent_id, created_ms, updated_ms)
fn insert_message(db_path, id, session_id, role, created_ms)
fn insert_part(db_path, id, message_id, session_id, part_type, text, created_ms)
// Each opens a brief connection, inserts, closes
```

**Tests to port** (1:1 from existing, same assertions, different fixture creation):

| # | Old name | New name | Notes |
|---|----------|----------|-------|
| 1 | `test_parse_basic_session` | `test_basic_session` | |
| 2 | `test_parse_session_with_messages` | `test_session_with_messages` | |
| 3 | `test_parse_subagent_session` | `test_subagent_session` | |
| 4 | `test_parse_session_missing_messages_dir` | `test_session_with_no_messages` | Session in db, no messages inserted |
| 5 | `test_scan_opencode_sessions` | `test_scan_multiple_sessions` | |
| 6 | `test_scan_nonexistent_dir` | `test_scan_nonexistent_db` | Path to non-existent file |
| 7 | `test_user_prompts_limited` | keep | |
| 8 | `test_parse_session_with_invalid_timestamp` | `test_invalid_timestamp` | |
| 9 | `test_end_time_none_when_equal_to_start_time` | keep | |
| 10 | `test_end_time_from_last_message_beats_updated` | keep | |
| 11 | `test_parse_session_malformed_json_file` | `test_malformed_message_data` | Insert message with `data = 'not json'`. `json_extract` returns NULL → role is None → message skipped. Verify session parses with 0 messages. |
| 12 | `test_scan_skips_malformed_sessions` | keep | Session with empty id + valid session in same db |
| 13 | `test_parse_session_with_messages_verifies_end_time` | keep | |
| 14 | `test_end_time_none_when_updated_before_created` | keep | |
| 15 | `test_empty_session_id_rejected` | keep | |

**Drop:**
- `test_parse_session_missing_required_fields` (SQLite schema enforces NOT NULL)

**Add:**
- `test_scan_corrupt_db` — db file exists but has no tables (empty file). Returns empty vec with warning, not error.

**Step 1:** Write implementation + tests (entire opencode.rs rewrite)

**Step 2:** Run tests: `cargo test -p tt-core -- opencode` → all pass

**Step 3:** Run clippy: `cargo clippy -p tt-core --all-targets` → no warnings

**Step 4: Commit**

```
feat(tt-core): migrate OpenCode ingestion from JSON files to SQLite

OpenCode now stores sessions in ~/.local/share/opencode/opencode.db
instead of JSON files. Update scan_opencode_sessions() to query the
SQLite database directly using rusqlite.
```

---

### Task 3: Update CLI path in ingest.rs

**Files:**
- Modify: `crates/tt-cli/src/commands/ingest.rs` (~lines 364-371, 476-478)

**Step 1: Rename and update the path function**

```rust
// Old:
fn get_opencode_storage_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join(".local/share/opencode/storage"))
}

// New:
fn get_opencode_db_path() -> Result<PathBuf> {
    Ok(home_dir()?.join(".local/share/opencode/opencode.db"))
}
```

**Step 2: Update the call site in index_sessions()**

```rust
// Old:
let opencode_dir = get_opencode_storage_dir()?;
if opencode_dir.exists() {
    let opencode_sessions = scan_opencode_sessions(&opencode_dir)...

// New:
let opencode_db = get_opencode_db_path()?;
if opencode_db.exists() {
    let opencode_sessions = scan_opencode_sessions(&opencode_db)...
```

**Step 3: Run full test suite**

Run: `cargo test`
Expected: all tests pass

**Step 4: Run clippy**

Run: `cargo clippy --all-targets`
Expected: no warnings

**Step 5: Commit**

```
chore(tt-cli): update OpenCode path from storage dir to SQLite db
```

---

### Task 4: Update AGENTS.md documentation

**Files:**
- Modify: `crates/tt-core/AGENTS.md` (Session Scanning section)
- Modify: `docs/plans/2026-02-05-opencode-session-ingestion.md` (add migration note)

Update references from JSON storage to SQLite. Note that `parse_opencode_session()` no longer exists as a public function and `scan_opencode_sessions` now takes a db path instead of a storage directory.

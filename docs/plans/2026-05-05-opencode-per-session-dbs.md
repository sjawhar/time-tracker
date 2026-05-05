# OpenCode Per-Session Shard DBs - Design + Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Restore `agent_tool_use` event ingestion (and therefore delegated time) for OpenCode sessions on devbox-mx, where a forked OpenCode build now stores messages/parts in per-session SQLite shards instead of the monolithic `opencode.db`.

**Tech stack:** rusqlite (existing workspace dep), no schema changes.

---

## Context

### What changed in OpenCode

The user's personal OpenCode fork shards heavy-write tables (`message`, `part`, `todo`, `event`) out of the monolithic `~/.local/share/opencode/opencode.db` into per-session databases at `~/.local/share/opencode/sessions/ses_<id>.db`. The `session` table itself stays in the monolithic db — including `time_updated`, which is kept fresh as session activity progresses.

Empirically (May 5 2026, devbox-mx):

- Monolithic `opencode.db` `session` table: 5609 rows, last entry today.
- Monolithic `opencode.db` `message`/`part` tables: last entry **April 13 2026**. New sessions write zero rows here.
- `sessions/ses_*.db` files: 81 shards spanning April 5 → May 5. Each shard contains `message` + `part` tables with **identical schema** to the monolithic versions, scoped to one session_id.
- For sessions with a shard, the monolithic message/part tables have zero rows for that session_id. **No overlap.** It's strictly either-or.

The fork may go away later, so the fix must keep working when shards aren't present.

### Why `tt` broke

`tt-core/src/opencode.rs::build_agent_session` uses the monolithic connection for three queries:

1. `collect_message_stats` (counts user/assistant messages, collects user prompts and timestamps)
2. `count_tool_calls` (`tool_call_count`)
3. `collect_tool_call_timestamps` (`tool_call_timestamps`)

For post-April-13 sessions all three return empty. `tool_call_count = 0` and `tool_call_timestamps = []` mean `create_session_events` emits no `agent_tool_use` events, which means `allocation.rs` allocates zero delegated time to those sessions.

## Design

### Approach

In `build_agent_session`, before running the three message/part queries:

1. Compute `sessions_dir = db_path.parent().join("sessions")` (passed down from `scan_opencode_sessions`).
2. Check if `sessions_dir/<session_id>.db` exists.
3. If it does, open a read-only connection to it and use that connection for the three queries.
4. Otherwise, use the monolithic connection (current behavior — old sessions, or non-sharded OpenCode builds).

If the shard exists but fails to open (corrupt, permissions), log a warning and fall back to the monolithic connection so we degrade to current behavior rather than dropping the session entirely.

### Why this is the simplest viable change

- Schema is identical, so the existing prepared SQL works unchanged on whichever connection.
- No double-counting — sharded sessions return zero from monolithic anyway, but we don't even try.
- `since` filtering keeps using monolithic `session.time_updated`, which is still fresh.
- Old sessions and non-sharded OpenCode builds are unaffected.
- Zero new types, no module restructuring. The sharding is an OpenCode implementation detail, not a stable abstraction worth modeling. If the fork drops sharding, deletion is local.

### Performance

`since`-filtered scans typically yield tens of sessions; each opens at most one extra read-only SQLite connection. Cold full scans hit ~5600 sessions but only the 81 shards trigger an extra open — negligible compared to the cost we already pay for prepared queries on the monolithic db.

### Things considered and rejected

- **`ATTACH` all shards** — SQLite attached-db cap (default 10) makes this a non-starter at 81+ shards.
- **`UNION` between monolithic and shard for every session** — wasteful, no overlap to combine.
- **Hardcoded date cutoff** — fragile, breaks if monolithic and shards ever co-exist for one session.
- **New `OpenCodeStorage` abstraction** — YAGNI, fork may revert.

---

## Implementation Tasks

### Task 1: Plumb `sessions_dir` through to `build_agent_session`

**Files:**
- Modify: `crates/tt-core/src/opencode.rs`

**Step 1.** Compute `sessions_dir` once at the top of `scan_opencode_sessions`:

```rust
let sessions_dir = db_path.parent().map(|p| p.join("sessions"));
```

`Option<PathBuf>` because `db_path.parent()` returns `Option`. Pass `sessions_dir.as_deref()` (`Option<&Path>`) into both `build_agent_session` call sites.

**Step 2.** Update `build_agent_session` signature:

```rust
fn build_agent_session(
    main_conn: &Connection,
    sessions_dir: Option<&Path>,
    session_row: SessionRow,
) -> Result<AgentSession, SessionError> { ... }
```

### Task 2: Open shard connection when present

**Files:**
- Modify: `crates/tt-core/src/opencode.rs`

**Step 1.** Add a small helper:

```rust
fn open_session_shard(sessions_dir: Option<&Path>, session_id: &str) -> Option<Connection> {
    let path = sessions_dir?.join(format!("{session_id}.db"));
    if !path.exists() {
        return None;
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    match Connection::open_with_flags(&path, flags) {
        Ok(conn) => {
            if let Err(err) = conn.busy_timeout(Duration::from_secs(5)) {
                tracing::warn!(?path, error = %err, "failed to set OpenCode shard timeout");
                return None;
            }
            Some(conn)
        }
        Err(err) => {
            tracing::warn!(?path, error = %err, "failed to open OpenCode session shard");
            None
        }
    }
}
```

Returns `None` for: missing file, open failure, busy_timeout failure. All cases fall back to the main connection.

**Step 2.** In `build_agent_session`, pick the connection for the three message/part queries:

```rust
let shard_conn = open_session_shard(sessions_dir, &session_row.id);
let stats_conn = shard_conn.as_ref().unwrap_or(main_conn);
let message_stats = collect_message_stats(stats_conn, &session_row.id)?;
let tool_call_count = count_tool_calls(stats_conn, &session_row.id)?;
let tool_call_timestamps = collect_tool_call_timestamps(stats_conn, &session_row.id)?;
```

The three helpers already take `&Connection` — no signature changes there.

### Task 3: Tests

**Files:**
- Modify: `crates/tt-core/src/opencode.rs` (test module)

Reuse `create_test_db()` for the monolithic db. Add a parallel `create_test_shard()` helper that creates `<temp>/sessions/<session_id>.db` with the `message` + `part` schema. New tests:

1. **`test_messages_and_parts_read_from_shard_when_present`** — Create monolithic with session row but no messages/parts; create a shard for that session_id with one user message + one tool part. Assert `tool_call_count == 1` and `user_prompts` contains the shard's text.

2. **`test_falls_back_to_monolithic_when_no_shard`** — Existing tests already cover this implicitly, but add an explicit one: monolithic has the session + messages, no shard file exists. Assert behavior matches pre-change baseline.

3. **`test_shard_takes_precedence_over_monolithic_when_both_exist`** — Belt-and-suspenders: create messages in BOTH monolithic and shard for the same session_id, assert shard wins (i.e., we read from shard, not monolithic). This documents the chosen semantics in case OpenCode's behavior ever changes.

4. **`test_corrupt_shard_falls_back_to_monolithic`** — Write garbage bytes to `sessions/<id>.db`; create messages in the monolithic. Assert the session still parses and uses the monolithic data (with a logged warning).

### Task 4: Documentation

**Files:**
- Modify: `crates/tt-core/AGENTS.md`

Update the row for `opencode.rs` to mention sharded shards:

```
| `opencode.rs` | ~840 | OpenCode session scanning. Reads `session` from monolithic `opencode.db`; reads `message`/`part` from per-session shard at `sessions/<id>.db` when present, else from monolithic. |
```

And add a note under "Session Scanning":

> OpenCode forks may shard messages/parts into per-session SQLite files at `~/.local/share/opencode/sessions/<id>.db`. `build_agent_session` opens the shard when present and falls back to the monolithic connection when not. Schema is identical between the two.

### Task 5: Verify, commit, push

**Step 1.** From repo root:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test -p tt-core opencode
cargo test
```

All four must pass.

**Step 2.** End-to-end check on devbox-mx data:

```bash
tt ingest sessions
tt recompute
tt report --weeks 4
```

The report should now show non-zero delegated time for post-April-13 OpenCode sessions. Spot-check one recent session:

```bash
sqlite3 ~/.local/share/time-tracker/tt.db \
  "SELECT COUNT(*) FROM events WHERE type='agent_tool_use' AND source='opencode' AND timestamp > '2026-04-14T00:00:00Z'"
```

Should now be > 0.

**Step 3.** Push.

```bash
jj bookmark set fix-opencode-shard-dbs
jj git push --named fix-opencode-shard-dbs=@
```

**Step 4.** Deploy to devbox-mx.

```bash
./scripts/deploy-remote.sh devbox-mx
ssh devbox-mx tt ingest sessions   # backfills events
```

Then on the local machine: `tt sync devbox-mx` to pull the new events into local tt.db.

---

## Out of scope

- Changes to `tt-llm` — still doesn't exist.
- Changing the public API of `scan_opencode_sessions`.
- Backfilling old `agent_tool_use` events without `tt sessions` re-running (re-running already covers it via the existing upsert/insert path).
- A `OpenCodeStorage` abstraction. If the user's fork drops sharding, deletion is straightforward.

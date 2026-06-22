# COSMIC Window/Idle Watcher — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **Commit policy (project override):** This repo uses **jj, not git**, and **one commit per PR**. IGNORE per-task commit steps. Do the work across all tasks, then make a SINGLE commit at the end (final task): `jj describe -m "..."` then `jj new`. Do not run `git add`/`git commit`.

**Goal:** Capture COSMIC active-window (app+title) and idle state as tt events, attributed to streams via the existing classify pipeline, so non-terminal time appears in reports.

**Architecture:** A `tt watch` daemon polls Wayland (COSMIC toplevel-info + ext-idle-notify) and **inserts `StoredEvent`s directly into SQLite** via `db.insert_events()` (one connection, `busy_timeout` set, WAL serializes the ~1/sec writes). Classify groups them into "window runs"; allocation turns `WindowFocus` into attention-window direct time. No JSONL, no drain — window events are live in `tt report` immediately.

**Tech Stack:** Rust (edition 2024), `wayland-client` 0.31, `wayland-protocols` 0.32 (staging), `cctk` (git `pop-os/cosmic-protocols`, **GPL-3.0-only** → tt is relicensed GPL-3.0-only), `rusqlite`, `clap`, `serde`.

**Spec:** `specs/design/2026-06-14-cosmic-window-idle-watcher-design.md`

---

## Oracle Corrections (supersede the task text below where they conflict)

Oracle reviewed this plan against the real source. Apply these — they fix real bugs/gaps. Each is keyed to a task.

**Blockers (must fix):**

1. **Task 7 — focus close-order.** Closing the prior interval resolves the stream from the *current*
   `window_focus_state`, so you MUST: **(a)** close the old `focus_state` interval using the OLD
   `window_focus_state`, **(b)** then update `window_focus_state` to the new app/stream, **(c)** then
   open the new `Focused { stream, focus_start: now }`. Doing (b) before (a) misattributes the prior
   app's time to the new one. Regression test: `tmux_focus(t0,"A")`, `window_focus(t10,"slack",Some("S"))`,
   `period_end=t11` → A gets ~10 min (capped at `attention_window`), S starts at t10.
2. **Task 8 — `--apply` cannot use a synthetic cwd.** Window events have `cwd = NULL`;
   `assign_events_by_pattern` is `WHERE cwd LIKE ?`, so an `app:<id>` label assigns **zero rows**.
   Instead: add `event_ids: Vec<String>` to each window-run cluster in the `--json` output; add a
   new `assign_by_event_ids: [{ event_ids: [...], stream }]` arm to `ClassifyApplyInput`; add
   `Database::assign_events_by_ids(&[String], stream_id, "inferred")` (`UPDATE events SET stream_id=?,
   assignment_source=? WHERE id IN (...)`, chunked). `--apply` uses it for window runs. (Sessions/cwd
   paths stay as-is.)
3. **Task 1 — update ALL read paths, not just `insert_events`/`build_data_json`.** Add
   `window_app_id`/`window_title` to the SELECT column lists and `row_to_event` index mapping in:
   `row_to_event`, `get_events`, `get_events_in_range`, `get_agent_session_start_events`,
   `get_events_by_stream`, `get_events_without_stream`. Introduce a shared `EVENT_COLUMNS` const to
   avoid drift across the ~6 SQL strings; bump the column indices in `row_to_event`. Update test literals.
4. **Task 6 — `cargo deny` git source.** `deny.toml` has `unknown-git = "deny"` and no `allow-git`, so
   the pinned `cctk` git dep trips CI regardless of the license allowlist. Add the cctk git URL under
   `[sources] allow-git = [...]` in addition to adding `GPL-3.0-only` to the license allowlist.

**Should-fix:**

5. **Task 4 — real idle duration.** Store `idle_start: Option<DateTime<Utc>>` in `EmitState`. On idle:
   set it and emit the backdated `AfkChange(idle)`. On active: emit `AfkChange(active)` with
   `idle_duration_ms = (now - idle_start)`, clear `idle_start`, then re-emit the current `WindowFocus`.
   (The `idle_ms: 0` stub in Task 4 Step 3 is a real gap.)
6. **Task 4 — real debounce (not poll cadence).** Add `pending: Option<(ActiveWindow, DateTime<Utc>)>`;
   emit a window *change* only after `(app,title)` has been stable ≥ `debounce` (default 500 ms).
   Startup and idle-resume emit immediately. (Otherwise `--poll-ms 100` floods events.)
7. **Task 4 — ID format.** Use the existing millisecond-`Z` timestamp format (match `ingest.rs`, not
   `to_rfc3339()`), and include a short stable hash of the title in the `WindowFocus` id so same-app
   title changes within one timestamp bucket don't collide. Keep `status` in the `AfkChange` id.
8. **Task 5 — `busy_timeout` placement.** Put `conn.busy_timeout(Duration::from_secs(30))?` immediately
   after `Connection::open`, BEFORE the WAL pragma and `init()`/migration. Use 30 s (not 5 s) in case
   `tt sync`/`tt import` hold batch write locks.
9. **Task 2 — transactional migration.** Wrap the two `ALTER TABLE`s + the `schema_info` version bump
   in one transaction, and do NOT early-`return` before the idempotent `CREATE TABLE IF NOT EXISTS` /
   `CREATE INDEX IF NOT EXISTS` block — let schema/index reconciliation run after the migration.
10. **Task 7 — do NOT change the global `attention_window_ms` default.** It's shared with terminal/tmux
    allocation and `recompute`/`report` (changing it churns reports + snapshots). Window focus reuses
    the existing cap. Drop the "set ≈180 s" instruction; only reconcile the doc-comment vs `300_000`.
11. **Task 6 — incremental Wayland backend.** First land a compiling `CosmicBackend::new()` + one-shot
    `poll()` verified with `--no-write`; only then wire the daemon loop. "Implement per design" alone is
    not concrete enough for the Wayland API risk.

**Nice-to-have:** expose `--once --no-write` for manual/systemd debugging; add an
`events(window_app_id, timestamp)` index once window runs are common; update the stale "old schemas
fail fast" comment in `tt-db/src/lib.rs` after the v8→v9 migration lands.

## File Structure

**Modify:**
- `crates/tt-db/src/lib.rs` — add `window_app_id`/`window_title` to `StoredEvent`, `insert_events` SQL, `build_data_json`; bump `SCHEMA_VERSION` 8→9; add CREATE TABLE columns + v8→v9 migration in `init()`.
- `crates/tt-core/src/allocation.rs` — `WindowFocus` arm establishes focus; `resolve_focus_stream` browser fallback.
- `crates/tt-cli/src/cli.rs` — add `Watch` subcommand.
- `crates/tt-cli/src/main.rs` — dispatch `Watch`.
- `crates/tt-cli/src/commands/mod.rs` — register `watch` module.
- `Cargo.toml` (workspace) — relicense `license = "GPL-3.0-only"`; `deny.toml` — allow GPL-3.0 in the license allowlist.
- `crates/tt-cli/src/commands/classify.rs` — window-run synthesis.
- `crates/tt-cli/Cargo.toml` — Wayland deps.

**Create:**
- `crates/tt-cli/src/commands/watch/mod.rs` — `tt watch` command, sync poll loop, event-emission state machine (pure, tested).
- `crates/tt-cli/src/commands/watch/backend.rs` — `WindowBackend` trait + `Snapshot`/`ActiveWindow`/`IdleState` + `FakeBackend` (tests).
- `crates/tt-cli/src/commands/watch/cosmic.rs` — `CosmicBackend` (Wayland; manually verified).
- `config/tt-watch.service` — systemd user unit. `LICENSE` — GPL-3.0 text at repo root.

---

## Phase 1 — Schema, StoredEvent, storage

### Task 1: Add window fields to `StoredEvent` + persistence

**Files:**
- Modify: `crates/tt-db/src/lib.rs` (`StoredEvent` struct ~125-200; `insert_events` ~501-541; `build_data_json` ~212-266)
- Test: `crates/tt-db/src/lib.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn test_insert_event_stores_window_fields() {
    let db = Database::open_in_memory().unwrap();
    let ts = Utc.with_ymd_and_hms(2026, 6, 14, 10, 0, 0).unwrap();
    let mut event = make_event("win-1", ts, tt_core::EventType::WindowFocus);
    event.source = "local.cosmic".to_string();
    event.cwd = None;
    event.pane_id = None;
    event.tmux_session = None;
    event.window_index = None;
    event.window_app_id = Some("firefox".to_string());
    event.window_title = Some("Proposal - Google Docs".to_string());

    assert!(db.insert_event(&event).unwrap());
    let events = db.get_events(None, None).unwrap();
    let got = events.iter().find(|e| e.id == "win-1").unwrap();
    assert_eq!(got.window_app_id.as_deref(), Some("firefox"));
    assert_eq!(got.window_title.as_deref(), Some("Proposal - Google Docs"));
    // build_data_json must expose app/title for the allocation layer:
    let data = got.build_data_json();
    assert_eq!(data.get("app").and_then(|v| v.as_str()), Some("firefox"));
    assert_eq!(data.get("title").and_then(|v| v.as_str()), Some("Proposal - Google Docs"));
}
```

(Also add `window_app_id: None, window_title: None` to the existing `make_event` helper and every `StoredEvent { .. }` literal in tests/non-test code so it compiles.)

- [ ] **Step 2: Run test, verify it fails to compile** (`window_app_id` field missing).

Run: `cargo test -p tt-db test_insert_event_stores_window_fields`
Expected: compile error `no field 'window_app_id' on type StoredEvent`.

- [ ] **Step 3: Add the fields + persistence**

In `StoredEvent` (after `idle_duration_ms`):
```rust
    /// Active-window application id (for `window_focus` events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_app_id: Option<String>,

    /// Active-window title (for `window_focus` events).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_title: Option<String>,
```

In `insert_events`, extend the column list, the `VALUES (?1..?20)` placeholders, and the `params![...]` to include `event.window_app_id, event.window_title` (append after `assignment_source`). Update the column count to match.

In `build_data_json`, before the final `Object(map)`:
```rust
    if let Some(ref v) = self.window_app_id {
        map.insert("app".to_string(), serde_json::Value::String(v.clone()));
    }
    if let Some(ref v) = self.window_title {
        map.insert("title".to_string(), serde_json::Value::String(v.clone()));
    }
```
> Note: `data["app"]` (not `window_app_id`) is what `allocation.rs` reads — this mapping is load-bearing.

- [ ] **Step 4: Run test, verify it passes.**

Run: `cargo test -p tt-db test_insert_event_stores_window_fields`
Expected: PASS.

### Task 2: Schema v8→v9 migration

**Files:**
- Modify: `crates/tt-db/src/lib.rs` (`SCHEMA_VERSION` line 40; CREATE TABLE events ~391-412; `init()` match ~366-381)
- Test: same tests module

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn test_migration_v8_to_v9_adds_columns_preserves_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v8.db");
    // Build a minimal v8 DB: schema_info=8 + an events row WITHOUT window columns.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_info (version INTEGER NOT NULL);
             INSERT INTO schema_info (version) VALUES (8);
             CREATE TABLE events (id TEXT PRIMARY KEY, timestamp TEXT NOT NULL, type TEXT NOT NULL,
               source TEXT NOT NULL, machine_id TEXT, schema_version INTEGER DEFAULT 1, cwd TEXT,
               git_project TEXT, git_workspace TEXT, pane_id TEXT, tmux_session TEXT,
               window_index INTEGER, status TEXT, idle_duration_ms INTEGER, action TEXT,
               session_id TEXT, stream_id TEXT, assignment_source TEXT DEFAULT 'inferred');
             INSERT INTO events (id, timestamp, type, source) VALUES ('old-1','2026-06-01T00:00:00Z','tmux_pane_focus','remote.tmux');",
        ).unwrap();
    }
    // Opening should migrate, not fail.
    let db = Database::open(&path).unwrap();
    let events = db.get_events(None, None).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].window_app_id, None); // old rows get NULL
    // New window event inserts fine (columns exist).
    let ts = Utc.with_ymd_and_hms(2026, 6, 14, 10, 0, 0).unwrap();
    let mut e = make_event("win-2", ts, tt_core::EventType::WindowFocus);
    e.window_app_id = Some("slack".to_string());
    assert!(db.insert_event(&e).unwrap());
}

#[test]
fn test_open_fails_on_newer_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v10.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_info (version INTEGER NOT NULL);
             INSERT INTO schema_info (version) VALUES (10);",
        ).unwrap();
    }
    assert!(matches!(Database::open(&path), Err(DbError::SchemaVersionMismatch { found: 10, .. })));
}
```

- [ ] **Step 2: Run, verify failure.**

Run: `cargo test -p tt-db test_migration_v8_to_v9_adds_columns_preserves_rows test_open_fails_on_newer_schema`
Expected: `test_migration...` FAILS (v8 currently returns `SchemaVersionMismatch`).

- [ ] **Step 3: Implement**

Set `const SCHEMA_VERSION: i32 = 9;`. Add `window_app_id TEXT, window_title TEXT` to the `CREATE TABLE events` DDL (so fresh DBs include them). In `init()`'s match, add a migration arm before the catch-all `Some(v)`:
```rust
        Some(8) => {
            self.conn.execute("ALTER TABLE events ADD COLUMN window_app_id TEXT", [])?;
            self.conn.execute("ALTER TABLE events ADD COLUMN window_title TEXT", [])?;
            self.conn.execute("UPDATE schema_info SET version = ?1", params![SCHEMA_VERSION])?;
            return Ok(());
        }
```

- [ ] **Step 4: Run, verify pass + full db suite green.**

Run: `cargo test -p tt-db`
Expected: PASS (incl. the existing `test_schema_version_check` — update its expectation if it hard-codes 8).

---

## Phase 2 — Watcher (`tt watch`)

### Task 3: Backend trait + types + fake

**Files:**
- Create: `crates/tt-cli/src/commands/watch/backend.rs`
- Modify: `crates/tt-cli/src/commands/mod.rs` (add `pub mod watch;`), `crates/tt-cli/src/commands/watch/mod.rs` (add `pub mod backend;`)

- [ ] **Step 1: Write the failing test** (in `backend.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fake_backend_yields_scripted_snapshots() {
        let mut b = FakeBackend::new(vec![
            Snapshot { active: Some(ActiveWindow { app_id: "firefox".into(), title: "Docs".into() }), idle: IdleState::Active },
            Snapshot { active: None, idle: IdleState::Idle { since_ms: 180_000 } },
        ]);
        assert_eq!(b.poll().unwrap().idle, IdleState::Active);
        assert!(matches!(b.poll().unwrap().idle, IdleState::Idle { .. }));
    }
}
```

- [ ] **Step 2: Run, verify failure** (module/types missing).

Run: `cargo test -p tt-cli fake_backend_yields_scripted_snapshots`
Expected: compile error.

- [ ] **Step 3: Implement types + trait + fake**

```rust
use anyhow::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveWindow { pub app_id: String, pub title: String }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleState { Active, Idle { since_ms: i64 } }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot { pub active: Option<ActiveWindow>, pub idle: IdleState }

pub trait WindowBackend { fn poll(&mut self) -> Result<Snapshot>; }

#[cfg(test)]
pub struct FakeBackend { scripted: std::collections::VecDeque<Snapshot> }
#[cfg(test)]
impl FakeBackend { pub fn new(s: Vec<Snapshot>) -> Self { Self { scripted: s.into() } } }
#[cfg(test)]
impl WindowBackend for FakeBackend {
    fn poll(&mut self) -> Result<Snapshot> {
        self.scripted.pop_front().ok_or_else(|| anyhow::anyhow!("no more snapshots"))
    }
}
```

- [ ] **Step 4: Run, verify pass.** `cargo test -p tt-cli fake_backend_yields_scripted_snapshots` → PASS.

### Task 4: Event-emission state machine (the tested core)

**Files:**
- Modify: `crates/tt-cli/src/commands/watch/mod.rs`
- Test: same file

This is the heart. A pure function maps (previous emitter state, new `Snapshot`, `now`) → events to append. Encodes: emit `WindowFocus` on startup / `(app,title)` change / idle→active resume; emit `AfkChange(idle)` backdated by `since_ms`; emit `AfkChange(active)` with `idle_duration_ms`; 500ms debounce handled by caller poll cadence.

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    fn aw(app: &str, title: &str) -> ActiveWindow { ActiveWindow { app_id: app.into(), title: title.into() } }
    fn t(s: i64) -> chrono::DateTime<Utc> { Utc.with_ymd_and_hms(2026,6,14,10,0,0).unwrap() + chrono::Duration::seconds(s) }

    #[test]
    fn emits_window_focus_on_startup_and_change() {
        let mut st = EmitState::default();
        let e1 = st.observe(&Snapshot{active:Some(aw("firefox","Docs")),idle:IdleState::Active}, t(0));
        assert_eq!(e1.len(), 1);
        assert_eq!(e1[0].event_type, EventType::WindowFocus);
        assert_eq!(e1[0].window_app_id.as_deref(), Some("firefox"));
        // same window → no event
        assert!(st.observe(&Snapshot{active:Some(aw("firefox","Docs")),idle:IdleState::Active}, t(1)).is_empty());
        // title change → event
        let e2 = st.observe(&Snapshot{active:Some(aw("firefox","Other")),idle:IdleState::Active}, t(2));
        assert_eq!(e2.len(), 1);
        assert_eq!(e2[0].window_title.as_deref(), Some("Other"));
    }

    #[test]
    fn idle_is_backdated_and_resume_reemits_window() {
        let mut st = EmitState::default();
        st.observe(&Snapshot{active:Some(aw("slack","general")),idle:IdleState::Active}, t(0));
        // idle detected at t=185, idle for 185s → AfkChange(idle) backdated to t=0.
        let idle = st.observe(&Snapshot{active:Some(aw("slack","general")),idle:IdleState::Idle{since_ms:185_000}}, t(185));
        let afk = idle.iter().find(|e| e.event_type==EventType::AfkChange).unwrap();
        assert_eq!(afk.status.as_deref(), Some("idle"));
        assert_eq!(afk.timestamp, t(0)); // backdated
        // resume → AfkChange(active) + a fresh WindowFocus (even though window unchanged)
        let resume = st.observe(&Snapshot{active:Some(aw("slack","general")),idle:IdleState::Active}, t(600));
        assert!(resume.iter().any(|e| e.event_type==EventType::AfkChange && e.status.as_deref()==Some("active")));
        assert!(resume.iter().any(|e| e.event_type==EventType::WindowFocus));
    }
}
```

- [ ] **Step 2: Run, verify failure.** `cargo test -p tt-cli -- watch::` → compile error (`EmitState` missing).

- [ ] **Step 3: Implement `EmitState` + `observe`**

```rust
pub mod backend;
use backend::{ActiveWindow, IdleState, Snapshot};
use chrono::{DateTime, Utc};
use tt_db::StoredEvent;
use tt_core::EventType;

#[derive(Default)]
pub struct EmitState {
    last_window: Option<ActiveWindow>,
    is_idle: bool,
    machine_id: String,
}

impl EmitState {
    pub fn new(machine_id: String) -> Self { Self { machine_id, ..Default::default() } }

    /// Pure: given a snapshot at `now`, return events to append.
    pub fn observe(&mut self, snap: &Snapshot, now: DateTime<Utc>) -> Vec<StoredEvent> {
        let mut out = Vec::new();
        // Idle transitions first.
        match snap.idle {
            IdleState::Idle { since_ms } if !self.is_idle => {
                let start = now - chrono::Duration::milliseconds(since_ms);
                out.push(self.afk(start, "idle", None));
                self.is_idle = true;
            }
            IdleState::Active if self.is_idle => {
                out.push(self.afk(now, "active", Some(/*idle duration*/ 0))); // caller fills real dur if tracked
                self.is_idle = false;
                if let Some(w) = &snap.active { out.push(self.window(now, w)); } // resume re-emit
                self.last_window = snap.active.clone();
                return out;
            }
            _ => {}
        }
        if self.is_idle { return out; }
        // Window change / startup.
        if snap.active.as_ref() != self.last_window.as_ref() {
            if let Some(w) = &snap.active { out.push(self.window(now, w)); }
            self.last_window = snap.active.clone();
        }
        out
    }

    fn window(&self, ts: DateTime<Utc>, w: &ActiveWindow) -> StoredEvent {
        let id = format!("{}:local.cosmic:window_focus:{}:{}", self.machine_id, ts.to_rfc3339(), w.app_id);
        StoredEvent {
            id, timestamp: ts, event_type: EventType::WindowFocus, source: "local.cosmic".into(),
            machine_id: Some(self.machine_id.clone()), schema_version: 1,
            window_app_id: Some(w.app_id.clone()), window_title: Some(w.title.clone()),
            cwd: None, pane_id: None, tmux_session: None, window_index: None,
            git_project: None, git_workspace: None, status: None, idle_duration_ms: None,
            action: None, session_id: None, stream_id: None, assignment_source: None,
            data: serde_json::Value::Null,
        }
    }
    fn afk(&self, ts: DateTime<Utc>, status: &str, idle_ms: Option<i64>) -> StoredEvent {
        let id = format!("{}:local.cosmic:afk_change:{}:{}", self.machine_id, ts.to_rfc3339(), status);
        StoredEvent {
            id, timestamp: ts, event_type: EventType::AfkChange, source: "local.cosmic".into(),
            machine_id: Some(self.machine_id.clone()), schema_version: 1,
            status: Some(status.into()), idle_duration_ms: idle_ms,
            window_app_id: None, window_title: None, cwd: None, pane_id: None,
            tmux_session: None, window_index: None, git_project: None, git_workspace: None,
            action: None, session_id: None, stream_id: None, assignment_source: None,
            data: serde_json::Value::Null,
        }
    }
}
```
(Track real idle duration on the resume event by stashing the idle-start timestamp in `EmitState`; the test above asserts presence, refine the value when wiring `since_ms`.)

- [ ] **Step 4: Run, verify pass.** `cargo test -p tt-cli -- watch::` → PASS.

### Task 5: DB insertion path (busy_timeout + insert loop)

**Files:**
- Modify: `crates/tt-db/src/lib.rs` (`open()`: add a `busy_timeout` pragma)
- Modify: `crates/tt-cli/src/commands/watch/mod.rs` (`run_once` helper: observe → insert)
- Test: `watch/mod.rs`

- [ ] **Step 1: Write failing test** — `run_once(&db, &mut backend, &mut state, now)` driven by a `FakeBackend` script against `Database::open_in_memory()`; assert the emitted events are persisted (`db.get_events(None, None)`).
- [ ] **Step 2: Run, verify failure** (`run_once` missing).
- [ ] **Step 3:** In `Database::open`, add `conn.pragma_update(None, "busy_timeout", 5000)?;` (concurrent writers wait instead of erroring `SQLITE_BUSY`). Implement `run_once`: `let events = state.observe(snap, now); db.insert_events(&events)?;` (`INSERT OR IGNORE` dedups).
- [ ] **Step 4:** Run `cargo test -p tt-cli -p tt-db` → PASS.

### Task 6: `CosmicBackend` + `tt watch` command (Wayland; manual verification)

**Files:**
- Create: `crates/tt-cli/src/commands/watch/cosmic.rs`
- Modify: `crates/tt-cli/Cargo.toml`, `cli.rs`, `main.rs`, `watch/mod.rs`

- [ ] **Step 1: Add deps + relicense.** In `crates/tt-cli/Cargo.toml`:
```toml
wayland-client = "0.31"
wayland-protocols = { version = "0.32", features = ["staging", "client"] }
cctk = { git = "https://github.com/pop-os/cosmic-protocols", package = "cosmic-client-toolkit", rev = "8e84152" }
```
  Then relicense (cctk is GPL-3.0-only): set the workspace `Cargo.toml` `license = "GPL-3.0-only"`, add a GPL-3.0 `LICENSE` file at repo root, and add `"GPL-3.0-only"` to the `deny.toml` license allowlist so `cargo deny check` passes.
- [ ] **Step 2: Implement `CosmicBackend`** per the design + librarian findings: bind `ext_foreign_toplevel_list_v1` (app_id/title), `zcosmic_toplevel_info_v1` (activated state), `ext_idle_notifier_v1` + seat (idle, timeout = config). `poll()` does one `event_queue.roundtrip()` and returns a `Snapshot` (active = the toplevel with `Activated` state; idle from the idle notification). **Fail fast** in `new()` if the COSMIC global is absent (`globals.bind::<ZcosmicToplevelInfoV1,_,_>()` returns `Err`).
- [ ] **Step 3: Add `Watch` subcommand** in `cli.rs` (`Watch { #[arg(long)] idle_timeout: Option<u64>, #[arg(long)] poll_ms: Option<u64>, #[arg(long)] no_write: bool }`), dispatch in `main.rs` to `watch::run(...)`. `run` opens the DB, builds `CosmicBackend`, loops: `sleep(poll_ms)` → `backend.poll()` → `EmitState::observe(now)` → `db.insert_events(&events)` (or print, if `--no-write`).
- [ ] **Step 4: MANUAL verification** (no unit test — Wayland):
  Run: `cargo run -p tt-cli -- watch --no-write -vvv`
  Expected: real `app_id`/`title` for the focused window (NOT "unknown"); switching windows prints new `WindowFocus`; leaving idle prints backdated `AfkChange(idle)`; **confirm scroll/mouse-motion resets idle** (the §1 open-question check). Then run without `--no-write` and confirm rows land in the DB **live** — `tt classify --json` shows the window events (no `tt ingest` needed).

---

## Phase 3 — Allocation

### Task 7: `WindowFocus` establishes focus + browser fallback

**Files:**
- Modify: `crates/tt-core/src/allocation.rs` (`WindowFocus` arm ~490-497; `resolve_focus_stream` ~695-706)
- Test: same file `mod tests`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn window_focus_accrues_direct_time_for_gui_app() {
    // Slack desktop (non-terminal, non-browser) focused, then period ends.
    let events = vec![
        TestEvent::window_focus(ts(0), "slack", Some("S")),
    ];
    let result = allocate_time(&events, &test_config(), Some(ts(1)), &HashMap::new(), &HashMap::new());
    let s = get_stream_time(&result, "S").expect("stream S");
    assert_eq!(s.time_direct_ms, 60 * 1000); // 1 min, capped at attention_window
}

#[test]
fn window_focus_browser_without_tab_falls_back_to_window_stream() {
    let events = vec![ TestEvent::window_focus(ts(0), "firefox", Some("P")) ];
    let result = allocate_time(&events, &test_config(), Some(ts(1)), &HashMap::new(), &HashMap::new());
    assert_eq!(get_stream_time(&result, "P").unwrap().time_direct_ms, 60 * 1000);
}
```
(`TestEvent::window_focus(ts, app, stream)` already exists.)

- [ ] **Step 2: Run, verify failure** — both fail (today `WindowFocus` accrues 0).

Run: `cargo test -p tt-core window_focus_accrues_direct_time_for_gui_app window_focus_browser_without_tab_falls_back_to_window_stream`
Expected: FAIL (direct_ms == 0).

- [ ] **Step 3: Implement** — in the `WindowFocus` arm, after updating `window_focus_state`, close the prior focus interval (capped at `attention_window_ms`, via the existing `add_direct` + `resolve_focus_stream` pattern used by `tmux_pane_focus`) and set `focus_state = FocusState::Focused { stream_id: <resolved or window stream>, focus_start: event_time }`. In `resolve_focus_stream`, change the browser arm to fall back to `window_state.stream_id` when `browser_stream_id` is `None`:
```rust
        Some(app) if is_browser_app(app) =>
            browser_stream_id.map(String::from).or_else(|| window_state.stream_id.clone()),
```

- [ ] **Step 4: Run, verify pass + no regressions.**

Run: `cargo test -p tt-core`
Expected: PASS (the 34 existing allocation tests stay green; if any window/browser test encodes the old "accrues 0" behavior, update it to the new semantics).

---

## Phase 4 — Classify

### Task 8: Window-run synthesis

**Files:**
- Modify: `crates/tt-cli/src/commands/classify.rs` (insertion after the `cluster_events()` call ~line 174; reuse `EventCluster` or add a `WindowRun`)
- Test: same file

- [ ] **Step 1: Write failing test** — given several `WindowFocus` events (no cwd, no session) with the same app and contiguous timestamps split by a >gap, assert the classify output groups them into runs keyed by app + sampled titles (not one empty-cwd blob).
- [ ] **Step 2: Run, verify failure.**
- [ ] **Step 3: Implement** `synthesize_window_runs(&[&StoredEvent]) -> Vec<EventCluster>`: filter `event_type == WindowFocus`, sort by timestamp, split on idle/gaps (>30 min, matching `cluster_events`), group contiguous same-`window_app_id`; set `cwd` to a synthetic `app:<app_id>` label and collapse repeated titles into the cluster (extend `EventCluster` with an optional `titles: Vec<String>` + `app_id` field, `#[serde(skip_serializing_if)]`). Call it on the `WindowFocus` subset and merge into `clusters` before the `unclassified` retain.
- [ ] **Step 4: Run, verify pass** + `cargo insta review` if snapshot output changed.

---

## Phase 5 — Ops (systemd + license)

### Task 9: systemd units

**Files:**
- Create: `config/tt-watch.service`. Plus repo-root `LICENSE` (GPL-3.0) + `deny.toml` allowlist update (done in Task 6).

- [ ] **Step 1:** Write `tt-watch.service` (user service): `ExecStart=%h/.local/bin/tt watch`, `Restart=on-failure`, `RestartSec=5`, `ExecStartPre=/bin/sleep 5`, and `PassEnvironment`/`Environment` for `WAYLAND_DISPLAY`/`XDG_RUNTIME_DIR`/`DBUS_SESSION_BUS_ADDRESS` (or `systemctl --user import-environment`). (No ingest timer — the daemon writes to the DB directly.)
- [ ] **Step 2: Manual verification:** `systemctl --user daemon-reload && systemctl --user enable --now tt-watch.service`; confirm `systemctl --user status tt-watch` is active and DB rows grow (`tt classify --json`); `loginctl enable-linger` if it must survive logout.

---

## Final Task: Single commit (jj)

- [ ] Run the full gate: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`. Fix anything.
- [ ] One commit (NOT per-task):
```bash
jj describe -m "feat: COSMIC window/idle watcher (tt watch) + allocation/classify integration; relicense GPL-3.0-only"
jj new
```

---

## Self-Review (against spec)

- Spec §1 daemon writes **direct to SQLite** → Tasks 5–6 (busy_timeout + `insert_events`). ✓
- §3 event model (WindowFocus on change/startup/resume; AfkChange backdated) → Task 4. ✓
- §4 schema/migration → Tasks 1–2. ✓
- §5 allocation (establish focus + browser fallback; attention_window is the cap) → Task 7. ✓
- §6 classify window runs → Task 8. ✓
- §7 systemd + GPL relicense → Task 9 + Task 6 (Step 1). ✓
- Defaults (idle 180s, poll 1s, debounce, attention_window) → Task 6 flags + Task 7. ✓
- Open question (scroll resets idle) → Task 6 manual check. ✓

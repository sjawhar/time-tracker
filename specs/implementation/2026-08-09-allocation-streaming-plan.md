# Streaming Allocation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make time allocation stream events instead of materializing the whole history, so `tt recompute` stops costing 250 seconds and 8.9 GB to compute a group-by that SQLite does in 0.73 seconds and 6.4 MB.

**Architecture:** `tt_core::allocation::allocate_time` is *already* a single forward pass over timestamp-sorted events (`for event in events`, state in `HashMap`s, no random access). It only holds the whole history because of its **signature** — it takes `&[E]`. This plan (1) deletes a redundant second full load in `recompute::run`, (2) re-exposes the existing pass as an incremental `Allocator` with `push`/`finish`, (3) feeds it directly from a streaming SQLite query, and (4) parallelizes the per-stream interval union that runs at the end. No allocation *rule* changes. Every number the product reports must come out byte-identical.

**Result:** Wall time regressed from **250.48 s** to **269.72 s** and **270.95 s** while CPU remained **99% = 1 of 24**. Task 4's parallel per-stream union measured **264.34 s** and **267.95 s** with CPU still **99% = 1 of 24**, produced no CPU-utilisation change, and was reverted; the remaining time is in the streaming path rather than finalisation. Peak RSS fell from **8,928,316 KB (8.9 GB)** to **119,184 KB** and **120,012 KB** (~120 MB, 75x), beating the <1 GB target and no longer scaling with history length. Final review found the wall-time regression inherent to row-by-row decoding in the streaming path, not a fixable defect: the scan has no per-event query and uses `idx_events_timestamp`.

**Tech Stack:** Rust (edition 2021), rusqlite 0.34 (streaming via `Statement::query`), rayon 1.10 (already a `tt-core` dependency, already used in `session.rs` and `opencode.rs`), insta snapshots, `anyhow` in `tt-cli` / `thiserror` in `tt-core` + `tt-db`.

## Measured Baseline (re-measure before and after; do not trust these numbers stale)

| | wall | peak RSS | cores |
|---|---|---|---|
| `tt recompute` over 2,738,805 events (2.2 GB db) | **250.48 s** | **8,928,316 KB (8.9 GB)** | 99% = 1 of 24 |
| `sqlite3 "SELECT stream_id, count(*), min(timestamp), max(timestamp) FROM events GROUP BY stream_id"` | **0.73 s** | **6,380 KB** | 1 |

Machine: 24 cores, 89 GB RAM.

Reproduce the baseline with:

```bash
cd /home/sami/Code/time-tracker/default
cp ~/.local/share/time-tracker/tt.db /tmp/perf.db
cargo build --release --bin tt
TT_DATABASE_PATH=/tmp/perf.db /usr/bin/time -f 'wall=%es cpu=%P peak_rss=%MKB' \
  ./target/release/tt recompute
```

## Global Constraints

- **Direct time must not change by one millisecond.** Acceptance for every task: `tt report` for `2026-07-20` prints `Direct time: 16h 23m` and for `2026-07-13..2026-07-20` prints `Direct time: 74h 20m`, against a copy of the live database, before and after.
- **Delegated time sums across parallel agents by design and routinely exceeds wall clock.** Never union, cap, or clamp delegated intervals. `test_parallel_agent_sessions_sum_delegated_beyond_wall_clock` (`crates/tt-core/src/allocation.rs:2402`) guards this; if it fails the change is wrong, not the test.
- **Direct time is a union, not a sum**, and can never exceed wall clock: `test_total_direct_time_never_exceeds_wall_clock` (`allocation.rs:2360`).
- **All 55 `#[test]` cases in `crates/tt-core/src/allocation.rs` must pass unmodified** through Tasks 1–4. If a test needs editing to pass, stop: the behavior changed.
- `tt_core::allocation::allocate_time` is clippy-blocked for outside callers (`clippy.toml` `disallowed-methods`, which today lists exactly that one path and nothing else). `tt_db::allocate_for_period` stays the only permitted entry point — `crates/tt-db/AGENTS.md:73-75` states it, and `allocate_for_period` (`crates/tt-db/src/lib.rs:4629-4632`) carries the sole `#[expect(clippy::disallowed_methods, reason = "single permitted allocation boundary")]`. **Making `Allocator::{new, push, finish}` public opens a second, unguarded path, so this plan closes it deliberately**: Task 2 adds `tt_core::allocation::Allocator::new` to `disallowed-methods`, and Task 3 keeps a single `#[expect]` on `allocate_for_period`. Gating `new` alone is sufficient and intended — nothing can `push` or `finish` without first constructing one.
- **Named behaviour change: the new aggregate queries count rows the in-memory path silently drops.** `Database::row_to_event` (`crates/tt-db/src/lib.rs:3657-3675`) returns `Ok(None)` — dropping the row after a `warn!` — both for a `timestamp` that `DateTime::parse_from_rfc3339` cannot read *and* for a `type` string `tt_core::EventType::from_str` does not know. Everything reached through `get_events` / `get_events_in_range` is therefore already filtered; `SELECT MIN(timestamp) …` and the split-session `GROUP BY` are not. So `event_time_bounds` and `sessions_spanning_multiple_streams` can legitimately report a wider span and more sessions than today's `recompute` does. That divergence is accepted **only once it is measured**: Task 1 Step 1 counts the affected population on a copy of the live database, and if it is not zero the plan **stops and surfaces the number** rather than quietly changing what the bounds and the warning cover.
- `allocate_for_period`'s `end` is **exclusive**; it queries `start..=end − 1ms` internally. Preserve exactly.
- Errors: `thiserror` (`DbError`) in `tt-db`, `thiserror` in `tt-core`, `anyhow` + `.context(...)` in `tt-cli`.
- Lints: workspace denies `unsafe_code`; `clippy::all`/`pedantic`/`nursery` are warnings and CI runs `-D warnings`. **Zero clippy warnings.** Never bare `#[allow]` — use `#[expect(lint, reason = "...")]`.
- `cargo fmt` clean (`max_width = 100`).
- No new dependencies. `rayon` is already available to `tt-core` only.
- **jj, not git.** Commit with `jj describe -m "..."` then `jj new`. Never use `git`. Never use backticks or `$(...)` inside a double-quoted `jj describe -m` message — it executes them; use `jj describe --stdin < file` for any message containing them.
- One commit per task is fine *while implementing this plan* (they are review checkpoints). Squash before opening a PR; the repo ships one commit per PR.

---

## File Structure

| File | Responsibility after this plan |
|---|---|
| `crates/tt-core/src/allocation.rs` (modify, 2,490 lines) | Owns the allocation rules. Gains `Allocator` (`new`/`push`/`finish`); `allocate_time` becomes a thin wrapper over it. No rule changes. |
| `crates/tt-db/src/lib.rs` (modify, ~10,400 lines) | Gains `event_time_bounds`, `sessions_spanning_multiple_streams`, `sessions_with_tool_use_but_no_start`, `for_each_event_in_range`. `allocate_for_period` rewritten to stream into `Allocator`. |
| `crates/tt-cli/src/commands/recompute.rs` (modify, 478 lines) | Stops loading the full history. Uses the two new aggregate queries. |
| `AGENTS.md` (modify) | Records the measured before/after and the rule that allocation must never re-materialize the history. |

`allocation.rs` is already far over any size ceiling and is the house pattern for this repo (`tt-db/src/lib.rs` is 10k lines). Do **not** split it as part of this plan; that is a separate change with its own review.

---

### Task 1: Stop `recompute` loading the whole history

`crates/tt-cli/src/commands/recompute.rs:71` calls `db.get_events(None, None)`, materializing all 2,738,805 events. That Vec is used for exactly three things: a warning about sessions split across streams (lines 80–105), `earliest` and `latest` (lines 107–108). Then `allocate_for_period` loads every event **again**. This task deletes the first copy. `Database::event_time_bounds` already exists and answers the bounds half, so only the split-session query is new.

**Files:**
- Modify: `crates/tt-db/src/lib.rs` (add **one** method near `get_events_in_range`, line ~1472 — `event_time_bounds` is already there at line 1436)
- Modify: `crates/tt-cli/src/commands/recompute.rs:1-118`
- Test: `crates/tt-db/src/lib.rs` (`mod tests`), `crates/tt-cli/src/commands/recompute.rs` (`mod tests`)

**Interfaces:**
- Consumes:
  - `Database::event_time_bounds(&self) -> Result<Option<EventTimeBounds>, DbError>` — **already exists** at `crates/tt-db/src/lib.rs:1436`. `EventTimeBounds` is the **tuple type alias** `(DateTime<Utc>, DateTime<Utc>)` declared at `crates/tt-db/src/lib.rs:249`, so the value destructures as `let (earliest, latest) = …`; `None` means the table is empty. Nothing in the tree calls it today, so this task is its first consumer.
- Produces:
  - `Database::sessions_spanning_multiple_streams(&self) -> Result<Vec<(String, Vec<String>)>, DbError>` — session id → its distinct stream ids, only sessions with 2+, ordered by session id, each inner list ordered.

- [ ] **Step 1: Measure the raw-SQL divergence before changing anything**

The aggregates this task relies on read the `events` table directly. The code they replace does not: everything reached through `get_events` / `get_events_in_range` passes `Database::row_to_event` (`crates/tt-db/src/lib.rs:3657-3675`), which returns `Ok(None)` — dropping the row after a `warn!` — both for a timestamp `DateTime::parse_from_rfc3339` cannot read and for a `type` string `tt_core::EventType::from_str` does not know. So `MIN(timestamp)`, `MAX(timestamp)` and the split-session `GROUP BY` can legitimately cover rows today's `recompute` never sees. Measure that population **first**, on the pre-change binary:

```bash
cd /home/sami/Code/time-tracker/default
cp ~/.local/share/time-tracker/tt.db /tmp/divergence.db
cargo build --release --bin tt

# Exact count: those two warnings are row_to_event's only two Ok(None) paths, and `tt`
# logs at a WARN floor by default (crates/tt-cli/src/logging.rs). Run this on the
# PRE-change binary, while recompute still loads every event through get_events.
TT_DATABASE_PATH=/tmp/divergence.db ./target/release/tt recompute 2>&1 \
  | grep -cE 'skipping event with (malformed timestamp|unknown type)'

# Cross-check in SQL against the raw table the aggregates read. The type list is the
# complete FromStr arm set at crates/tt-core/src/event_type.rs:40-49.
sqlite3 /tmp/divergence.db "
SELECT 'unknown_type', count(*) FROM events
 WHERE type NOT IN ('agent_session','agent_tool_use','user_message','tmux_pane_focus',
                    'tmux_scroll','afk_change','window_focus','browser_tab');
SELECT 'off_shape_timestamp', count(*) FROM events
 WHERE timestamp NOT GLOB
   '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9]*Z';"
```

Expected: `0` from the `grep -c`, and `0` from both SQL counts.

The `grep -c` is the **exact** figure, because it counts the two skip paths themselves. The GLOB is a *shape* screen and is deliberately stricter than `parse_from_rfc3339` — a valid `+00:00` offset would be flagged here and read fine by `row_to_event` — so it can over-report but never under-report, and it exists to name rows rather than to count them.

**If every count is 0, record the three numbers and continue.** The divergence is then a difference in what the two paths *could* report with no row in the database that makes them differ, which is what the Global Constraints entry accepts.

**If any count is non-zero, stop. Do not continue to Step 2.** Collect the offending rows and surface them:

```bash
sqlite3 /tmp/divergence.db "
SELECT id, timestamp, type, source, machine_id FROM events
 WHERE type NOT IN ('agent_session','agent_tool_use','user_message','tmux_pane_focus',
                    'tmux_scroll','afk_change','window_focus','browser_tab')
    OR timestamp NOT GLOB
       '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9]*Z'
 LIMIT 20;"
```

Report the counts and the sample. A non-zero count means this task silently widens the reported span or the warning set, and which way to resolve it — repair the rows, filter the SQL to match `row_to_event`, or accept a documented widening — is a decision for whoever owns the data, not for the implementer. `tt recompute` on the live copy takes ~250 s; that is the cost this plan removes, and paying it once here is the point.

- [ ] **Step 2: Read the existing `event_time_bounds` and pin it with a test**

`Database::event_time_bounds` **already exists** at `crates/tt-db/src/lib.rs:1436`. Read it before writing anything:

Run: `sed -n '1429,1461p' crates/tt-db/src/lib.rs`

It returns `Result<Option<EventTimeBounds>, DbError>`, and `EventTimeBounds` is a **type alias for a tuple**, not a struct: `pub type EventTimeBounds = (DateTime<Utc>, DateTime<Utc>);` at `crates/tt-db/src/lib.rs:249`. So the value destructures as `let (earliest, latest) = …`, which is exactly what Step 8 and Task 5 do. It parses both bounds with an inline closure that fails loudly on unreadable text. Do **not** add a `parse_event_bound` free function or a `MalformedEventBound` error type — this crate has neither, needs neither, and adding them would duplicate `parse_stream_timestamp` (line 258) for no gain.

It has no caller and no test today (`grep -rn event_time_bounds crates/` returns the definition alone), so this task is its first consumer. Give it the coverage it never had — add to `crates/tt-db/src/lib.rs` inside `mod tests`:

```rust
    #[test]
    fn event_time_bounds_reports_the_first_and_last_event() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.event_time_bounds().unwrap(), None);

        db.insert_events(&[
            make_event("b", Utc.with_ymd_and_hms(2026, 3, 2, 9, 0, 0).unwrap(), EventType::UserMessage),
            make_event("a", Utc.with_ymd_and_hms(2026, 3, 1, 9, 0, 0).unwrap(), EventType::UserMessage),
            make_event("c", Utc.with_ymd_and_hms(2026, 3, 3, 9, 0, 0).unwrap(), EventType::UserMessage),
        ])
        .unwrap();

        let (first, last) = db.event_time_bounds().unwrap().unwrap();
        assert_eq!(first, Utc.with_ymd_and_hms(2026, 3, 1, 9, 0, 0).unwrap());
        assert_eq!(last, Utc.with_ymd_and_hms(2026, 3, 3, 9, 0, 0).unwrap());
    }
```

- [ ] **Step 3: Run it — it must pass immediately**

Run: `cargo test -p tt-db --lib event_time_bounds_reports_the_first_and_last_event`
Expected: `test result: ok. 1 passed`. This is **not** a TDD cycle — the method exists, so a failure here means the shape read in Step 2 was misread, not that there is code to write.

- [ ] **Step 4: Write the failing test for `sessions_spanning_multiple_streams`**

```rust
    #[test]
    fn sessions_spanning_multiple_streams_reports_only_the_split_ones() {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("stream-a", Some("A"))).unwrap();
        db.insert_stream(&make_stream("stream-b", Some("B"))).unwrap();

        let mut split_one = make_event("e1", Utc.with_ymd_and_hms(2026, 3, 1, 9, 0, 0).unwrap(), EventType::UserMessage);
        split_one.session_id = Some("split".to_string());
        split_one.stream_id = Some("stream-a".to_string());
        let mut split_two = make_event("e2", Utc.with_ymd_and_hms(2026, 3, 1, 9, 1, 0).unwrap(), EventType::UserMessage);
        split_two.session_id = Some("split".to_string());
        split_two.stream_id = Some("stream-b".to_string());
        let mut settled = make_event("e3", Utc.with_ymd_and_hms(2026, 3, 1, 9, 2, 0).unwrap(), EventType::UserMessage);
        settled.session_id = Some("settled".to_string());
        settled.stream_id = Some("stream-a".to_string());
        db.insert_events(&[split_one, split_two, settled]).unwrap();

        assert_eq!(
            db.sessions_spanning_multiple_streams().unwrap(),
            vec![("split".to_string(), vec!["stream-a".to_string(), "stream-b".to_string()])]
        );
    }
```

- [ ] **Step 5: Run it and confirm it fails**

Run: `cargo test -p tt-db --lib sessions_spanning_multiple_streams_reports_only_the_split_ones`
Expected: compile error — method not found.

- [ ] **Step 6: Implement `sessions_spanning_multiple_streams`**

```rust
    /// Sessions whose events point at more than one stream, with those streams.
    ///
    /// A data-integrity report `recompute` prints, and it never needed the events in
    /// memory to produce it. Ordered by session id, and each stream list ordered, so the
    /// warning block is stable between runs.
    pub fn sessions_spanning_multiple_streams(
        &self,
    ) -> Result<Vec<(String, Vec<String>)>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, stream_id FROM events
             WHERE session_id IS NOT NULL AND stream_id IS NOT NULL
               AND session_id IN (
                   SELECT session_id FROM events
                   WHERE session_id IS NOT NULL AND stream_id IS NOT NULL
                   GROUP BY session_id
                   HAVING COUNT(DISTINCT stream_id) > 1
               )
             GROUP BY session_id, stream_id
             ORDER BY session_id ASC, stream_id ASC",
        )?;
        let mut grouped: Vec<(String, Vec<String>)> = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let session_id: String = row.get(0)?;
            let stream_id: String = row.get(1)?;
            match grouped.last_mut() {
                Some((existing, streams)) if *existing == session_id => streams.push(stream_id),
                _ => grouped.push((session_id, vec![stream_id])),
            }
        }
        Ok(grouped)
    }
```

- [ ] **Step 7: Run it and confirm it passes**

Run: `cargo test -p tt-db --lib sessions_spanning_multiple_streams_reports_only_the_split_ones`
Expected: `test result: ok. 1 passed`

- [ ] **Step 8: Rewrite `recompute::run` to use them**

In `crates/tt-cli/src/commands/recompute.rs`, delete the `let events = db.get_events(None, None)…` block, the `events.is_empty()` guard, the `tracing::debug!(event_count = …)` line, the `session_streams` `HashMap` build, and the `earliest`/`latest` lines derived from `events`. That is **lines 69–108 exactly** — from the `// Get all events` comment through `let latest = …`, stopping before `let config = AllocationConfig::default();` on line 109. Replace that region with:

```rust
    // Bounds and the split-session warning are aggregates. Loading every event to derive
    // them cost 8.9 GB of RSS on the live corpus, and `allocate_for_period` loads the
    // events it needs itself.
    let Some((earliest, latest)) = db
        .event_time_bounds()
        .context("failed to read event time bounds")?
    else {
        println!("No events to process.");
        return Ok(());
    };

    // Sessions split across streams undercount, so they are reported. Not fatal: only the
    // user can say which stream such a session belongs to.
    for (session_id, stream_ids) in db
        .sessions_spanning_multiple_streams()
        .context("failed to check for sessions split across streams")?
    {
        let shown = &session_id[..session_id.len().min(30)];
        eprintln!(
            "Warning: session {} has events in {} streams: {:?}",
            shown,
            stream_ids.len(),
            stream_ids,
        );
        eprintln!("  Use 'tt streams assign <stream-ref> --session {shown}' to settle it.");
    }
```

Keep the `let config = AllocationConfig::default();` and the `allocate_for_period(db, earliest, latest + Duration::milliseconds(1), None, &config)` call exactly as they are.

Two imports go stale with that region and **both must be fixed, or `-D warnings` fails CI on an unused import**:

- `use std::collections::HashMap;` (line 6) — its only use in the file is the `session_streams` build on line 82. Delete the line. `HashSet` and `BTreeSet` elsewhere in the file are spelled out in full (`std::collections::HashSet<…>` on lines 30 and 132), so they do not depend on it.
- `use chrono::{Duration, Utc};` (line 9) — `Utc` is used only by the two `map_or_else(Utc::now, …)` calls on lines 107–108. Narrow it to `use chrono::Duration;`. `mod tests` has its own `use chrono::{TimeZone, Utc};` (line 182) and is unaffected.

- [ ] **Step 9: Verify the whole suite and the lints**

Run: `cargo fmt && cargo clippy --all-targets && cargo test`
Expected: fmt silent, **zero** clippy warnings, all tests pass (baseline is 1,154 passing).

- [ ] **Step 10: Verify the numbers did not move, and measure the win**

```bash
cp ~/.local/share/time-tracker/tt.db /tmp/t1.db
cargo build --release --bin tt
TT_DATABASE_PATH=/tmp/t1.db ./target/release/tt report --start 2026-07-20 --end 2026-07-21 | grep '^Direct time:'
TT_DATABASE_PATH=/tmp/t1.db ./target/release/tt report --start 2026-07-13 --end 2026-07-20 | grep '^Direct time:'
TT_DATABASE_PATH=/tmp/t1.db /usr/bin/time -f 'wall=%es cpu=%P peak_rss=%MKB' ./target/release/tt recompute | tail -3
```

Expected: `Direct time: 16h 23m` and `Direct time: 74h 20m` (unchanged). `peak_rss` roughly halved from 8,928,316 KB, because one of the two full copies is gone. Record the actual figure — the next task is measured against it.

- [ ] **Step 11: Commit**

```bash
jj describe -m "perf(recompute): derive bounds and split-session warning by aggregate

recompute loaded all 2,738,805 events to compute a MIN, a MAX, and a
group-by, then allocate_for_period loaded every event again. Two full
copies of the history, 8.9 GB peak RSS, to produce two timestamps and a
warning. Both are now single aggregate queries, and the one that already
existed (event_time_bounds) had no caller and no test.
jj new
```

---

### Task 2: Expose the existing pass as an incremental `Allocator`

`allocate_time` (`crates/tt-core/src/allocation.rs:195`, with its attributes on 194) is already a single forward pass: the locals are declared on lines 202–209, the two helper closures on 213–240, the event loop is `for event in events {` on line 242 and closes on 632, and finalization (`end_time`, the focus close, `final_attributions`, the `remove(UNASSIGNED_STREAM_ID)`, the per-stream union, the `AllocationResult` literal) runs from 634 to 718, with the function closing on 719. Nothing indexes or re-reads the slice. This task is a **pure refactor** that turns that pass into a struct you can feed one event at a time. No rule changes, no test edits.

**Files:**
- Modify: `crates/tt-core/src/allocation.rs:194-719`
- Modify: `clippy.toml` (Step 7 — gate the new entry point)
- Test: `crates/tt-core/src/allocation.rs` (`mod tests`) — existing 55 tests must pass untouched, plus one new equivalence test.

**Interfaces:**
- Produces:
  - `pub struct Allocator<'a>` holding every `let mut` currently local to `allocate_time` (`focus_state`, `window_focus_state`, `browser_focus_state`, `tmux_focus_stream_id`, `agent_sessions`, `stream_times`, `activity_intervals`, `last_event_time`) plus borrows of `config: &'a AllocationConfig`, `session_end_times: &'a HashMap<String, DateTime<Utc>>`, `session_types: &'a HashMap<String, SessionType>`, and `period_end: Option<DateTime<Utc>>`.
  - `Allocator::new(config: &'a AllocationConfig, period_end: Option<DateTime<Utc>>, session_end_times: &'a HashMap<String, DateTime<Utc>>, session_types: &'a HashMap<String, SessionType>) -> Self`
  - `Allocator::push<E: AllocatableEvent>(&mut self, event: &E)`
  - `Allocator::finish(mut self) -> AllocationResult` — **`mut self`, not `self`**: finalization mutates `stream_times` via `.remove(UNASSIGNED_STREAM_ID)` (`allocation.rs:698-700`), so a plain `self` does not compile.
- Consumes: nothing from Task 1.

- [ ] **Step 1: Write the equivalence test first**

Add to `mod tests` in `crates/tt-core/src/allocation.rs`. It pins that feeding events one at a time is identical to passing the slice — the property every later task depends on:

```rust
    #[test]
    fn pushing_events_one_at_a_time_equals_allocating_the_whole_slice() {
        // Given: a corpus exercising focus, agent work, and an unassigned stretch.
        let events = vec![
            TestEvent::tmux_focus(ts(0), "stream-a"),
            TestEvent::agent_session(ts(1), "started", "session-1", Some("stream-a")),
            TestEvent::agent_tool_use(ts(2), "session-1", "stream-a"),
            TestEvent::agent_tool_use(ts(30), "session-1", "stream-a"),
            TestEvent::tmux_focus_unassigned(ts(60)),
            TestEvent::agent_session(ts(61), "ended", "session-1", Some("stream-a")),
        ];
        let config = test_config();
        let end_times = HashMap::new();
        let types = HashMap::new();

        // When: the slice path and the incremental path both run.
        let from_slice = allocate_time(&events, &config, None, &end_times, &types);
        let mut allocator = Allocator::new(&config, None, &end_times, &types);
        for event in &events {
            allocator.push(event);
        }
        let from_pushes = allocator.finish();

        // Then: identical in every field, not merely in the totals.
        assert_eq!(from_pushes.total_tracked_ms, from_slice.total_tracked_ms);
        assert_eq!(from_pushes.unassigned_direct_ms, from_slice.unassigned_direct_ms);
        assert_eq!(from_pushes.unassigned_delegated_ms, from_slice.unassigned_delegated_ms);
        let mut pushed = from_pushes.stream_times;
        let mut sliced = from_slice.stream_times;
        pushed.sort_by(|a, b| a.stream_id.cmp(&b.stream_id));
        sliced.sort_by(|a, b| a.stream_id.cmp(&b.stream_id));
        assert_eq!(pushed.len(), sliced.len());
        for (p, s) in pushed.iter().zip(sliced.iter()) {
            assert_eq!(p.stream_id, s.stream_id);
            assert_eq!(p.time_direct_ms, s.time_direct_ms, "direct for {}", p.stream_id);
            assert_eq!(p.time_delegated_ms, s.time_delegated_ms, "delegated for {}", p.stream_id);
        }
    }
```

These are the real fixture names and signatures, read from source — do not invent new ones and do not add duplicates. `TestEvent` and its builders live at `allocation.rs:828-978`: `tmux_focus(ts, stream_id: &str)`, `tmux_focus_unassigned(ts)`, `agent_session(ts, action: &str, session_id: &str, stream_id: Option<&str>)`, `agent_tool_use(ts, session_id: &str, stream_id: &str)`, `user_message(ts, session_id, stream_id)`, `window_focus(ts, app, stream_id: Option<&str>)`. **`ts` takes one argument — `fn ts(minutes: i64) -> DateTime<Utc>` at `allocation.rs:980`**, offsetting from a fixed 2025-01-15 09:00 base; it is not a `(y, m, d, h, m, s)` constructor. `test_config()` (line 820) is the 60 s attention window every other test uses, which is why the corpus above spaces its focus events 60 minutes apart.

No `#[expect(clippy::disallowed_methods, …)]` is needed inside the test: `mod tests` already carries one at module level (`allocation.rs:810-814`, reason `"tt-core tests exercise the core algorithm directly"`), and it covers both the `allocate_time` call here and the `Allocator::new` calls this task's Step 7 makes lintable.

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p tt-core --lib pushing_events_one_at_a_time_equals_allocating_the_whole_slice`
Expected: compile error — `cannot find struct 'Allocator'`.

- [ ] **Step 3: Declare the struct and `new`**

Insert immediately before `pub fn allocate_time` in `crates/tt-core/src/allocation.rs`:

```rust
/// The allocation pass, fed one event at a time.
///
/// [`allocate_time`] has always been a single forward walk over timestamp-sorted events
/// with its state in maps — it never indexed the slice or looked backwards. Taking
/// `&[E]` was therefore a property of the signature and not of the algorithm, and it was
/// an expensive one: `tt recompute` held all 2,738,805 events to run it, peaking at
/// 8.9 GB where the equivalent SQLite aggregate uses 6.4 MB.
///
/// Splitting the walk from its input lets a caller stream rows straight from SQLite.
/// Memory becomes a function of concurrently-open sessions, streams, and recorded
/// intervals rather than of history length.
///
/// Events must still arrive in ascending timestamp order. That requirement is the
/// algorithm's, not the container's, and pushing them out of order silently produces
/// wrong time exactly as passing an unsorted slice always did.
pub struct Allocator<'a> {
    config: &'a AllocationConfig,
    period_end: Option<DateTime<Utc>>,
    session_end_times: &'a HashMap<String, DateTime<Utc>>,
    session_types: &'a HashMap<String, SessionType>,
    focus_state: FocusState,
    window_focus_state: WindowFocusState,
    browser_focus_state: BrowserFocusState,
    tmux_focus_stream_id: Option<String>,
    agent_sessions: HashMap<String, AgentSession>,
    stream_times: HashMap<String, StreamAllocation>,
    activity_intervals: Vec<Interval>,
    last_event_time: Option<DateTime<Utc>>,
}

impl<'a> Allocator<'a> {
    /// Starts a pass with no events seen yet.
    #[must_use]
    pub fn new(
        config: &'a AllocationConfig,
        period_end: Option<DateTime<Utc>>,
        session_end_times: &'a HashMap<String, DateTime<Utc>>,
        session_types: &'a HashMap<String, SessionType>,
    ) -> Self {
        Self {
            config,
            period_end,
            session_end_times,
            session_types,
            focus_state: FocusState::Unfocused,
            window_focus_state: WindowFocusState::default(),
            browser_focus_state: BrowserFocusState::default(),
            tmux_focus_stream_id: None,
            agent_sessions: HashMap::new(),
            stream_times: HashMap::new(),
            activity_intervals: Vec::new(),
            last_event_time: None,
        }
    }
}
```

- [ ] **Step 4: Move the loop body into `push`**

Add `pub fn push<E: AllocatableEvent>(&mut self, event: &E)` to that `impl`, and move the **body of the `for event in events` loop** into it verbatim — `crates/tt-core/src/allocation.rs` **lines 243 through 631 inclusive**, where line 242 is `for event in events {` and line 632 is its closing brace. Then make it compile by mechanical substitution, **with the two exceptions below — they are why this move is not purely mechanical and they are the only two**:

**The substitution.** Every bare local becomes a field: `focus_state` → `self.focus_state`, `agent_sessions` → `self.agent_sessions`, `stream_times` → `self.stream_times`, `activity_intervals` → `self.activity_intervals`, `last_event_time` → `self.last_event_time`, `tmux_focus_stream_id` → `self.tmux_focus_stream_id`, `window_focus_state` → `self.window_focus_state`, `browser_focus_state` → `self.browser_focus_state`, `config` → `self.config`, `period_end` → `self.period_end`, `session_end_times` → `self.session_end_times`, `session_types` → `self.session_types`.

**Exception 1: there are TWO closures to lift, not one.** Both are defined between the locals and the loop, both close over nothing but their arguments, and both are called from the loop body *and* from the finalization block Step 5 moves into `finish`. Lift each to a **private associated function** on `Allocator`, body copied verbatim from source. A scan of lines 202–242 confirms these are the only two closures there:
  - `add_direct` — `allocation.rs:213-224`; called at lines 315, 358, 400, 444, 558, 605 (all inside `push`) and 649 (inside `finish`).
  - `add_delegated` — `allocation.rs:227-240`; called at lines 287 and 511 (inside `push`) and 684 (inside `finish`).

```rust
    fn add_direct(
        stream_id: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        intervals: &mut Vec<Interval>,
        times: &mut HashMap<String, StreamAllocation>,
    ) {
        if end > start {
            let interval = Interval { start, end };
            let allocation = times.entry(stream_id.to_string()).or_default();
            allocation.focus_intervals.push(interval);
            intervals.push(interval);
        }
    }

    fn add_delegated(
        stream_id: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        intervals: &mut Vec<Interval>,
        times: &mut HashMap<String, StreamAllocation>,
    ) {
        if end > start {
            let duration_ms = (end - start).num_milliseconds();
            let interval = Interval { start, end };
            let allocation = times.entry(stream_id.to_string()).or_default();
            allocation.delegated_ms += duration_ms;
            allocation.delegated_intervals.push(interval);
            intervals.push(interval);
        }
    }
```

Call sites become `Self::add_direct(stream_id, start, end, &mut self.activity_intervals, &mut self.stream_times)` and the same shape for `Self::add_delegated`.

**Exception 2: the loop body contains a `continue`, and inside `push` it must become a bare `return`.** It is at `allocation.rs:429`, in the `EventType::UserMessage` arm, guarded by `if is_subagent_message` (line 428) — a `user_message` emitted by a subagent reflects the parent agent's delegation rather than human attention, so it establishes no focus.

The placement is the whole point. The loop's **last statement is `last_event_time = Some(event_time);` at line 631**, after the `match` and immediately before the closing brace on 632. `continue` therefore *skips* that assignment today: a subagent `user_message` does not advance `last_event_time`, and since `finish` computes `end_time = period_end.or(last_event_time)` (line 635), that is observable behaviour, not an accident.

So write `return;` at exactly the point `continue` occupied — **before** the `self.last_event_time = Some(event_time);` you moved in from line 631, which must be the last statement of `push`. Do **not** "helpfully" replicate the `last_event_time` update before returning, and do not restructure the arm into an `if`/`else` that falls through to it: either would start advancing `last_event_time` on subagent messages and change what `finish` closes intervals against.

A scan of lines 242–632 for `continue`, `break` and `return` finds **exactly this one occurrence and nothing else** — no `break`, no other `continue`, no early `return`, and no `?`. If your scan finds another, stop and re-derive this section rather than guessing at its intent.

**Change no conditions, no arithmetic, no ordering, and no `EventType` arm.** If borrow-checker errors appear because a statement holds `&self.x` while mutating `self.y`, bind locals first (`let config = self.config;`) rather than restructuring logic.

- [ ] **Step 5: Move finalization into `finish`**

Add `pub fn finish(mut self) -> AllocationResult` and move `allocation.rs` lines 634 to 718 into it verbatim — the `end_time` binding, the focus close, the `final_attributions` block, the `for (stream_id, first_tool, session_end)` loop, the `total_tracked_ms` union, the `remove(UNASSIGNED_STREAM_ID)`, the per-stream map, and the `AllocationResult { … }` literal. Apply the same `self.` substitution, and call the two lifted helpers as `Self::add_direct` / `Self::add_delegated`. Nothing else moves.

**The receiver is `mut self`, not `self`.** Line 698 does `stream_times.remove(UNASSIGNED_STREAM_ID)`, which needs the binding to be mutable; `fn finish(self)` does not compile. It is still by value — the pass is consumed, and `stream_times`/`activity_intervals` are moved out rather than cloned.

- [ ] **Step 6: Reduce `allocate_time` to a wrapper**

Replace the whole body of `pub fn allocate_time` with:

```rust
) -> AllocationResult {
    let mut allocator = Allocator::new(config, period_end, session_end_times, session_types);
    for event in events {
        allocator.push(event);
    }
    allocator.finish()
}
```

Keep its signature and its doc comment exactly as they are. Existing callers and every existing test keep working through it unchanged. Its `clippy.toml` `disallowed-methods` entry stays too — Step 7 adds a second entry beside it rather than replacing it.

- [ ] **Step 7: Close the second entry point in `clippy.toml`**

`crates/tt-db/AGENTS.md:73-75` makes `tt_db::allocate_for_period` the only permitted allocation entry point outside `tt-core`, and `clippy.toml` is what enforces it — today with exactly one path. Making `Allocator::{new, push, finish}` public creates a second, unguarded path, so gate it in the same place. Replace `clippy.toml` with:

```toml
disallowed-methods = [
    { path = "tt_core::allocation::allocate_time", reason = "call tt_db::allocate_for_period instead" },
    { path = "tt_core::allocation::Allocator::new", reason = "call tt_db::allocate_for_period instead; the streaming pass has the same single entry point as the slice one" },
]
```

Gating `new` alone is deliberate and sufficient: `push` and `finish` are unreachable without first constructing an `Allocator`, and listing all three would mean three suppressions at every sanctioned site instead of one.

Two call sites become lintable and are handled differently:

- **`allocate_time`'s own body** (the wrapper from Step 6). `disallowed_methods` fires on the call expression regardless of which crate defines the method, so put `#[expect(clippy::disallowed_methods, reason = "allocate_time is the in-crate wrapper over the incremental pass")]` on `pub fn allocate_time` itself. Note it already carries `#[allow(clippy::too_many_lines, clippy::implicit_hasher)]` at `allocation.rs:194` — add the `#[expect]` as a separate attribute rather than folding it in, since the two say different things.
- **`mod tests`** needs nothing: its module-level `#[expect(clippy::disallowed_methods, …)]` at `allocation.rs:810-814` already covers every `Allocator::new` call the tests make.

`allocate_for_period` in `tt-db` is Task 3's job; it already carries the sole `#[expect]` for `allocate_time` and will carry it for `Allocator::new` instead.

Run: `cargo clippy --all-targets`
Expected: zero warnings. In particular no `unfulfilled_lint_expectation` — that would mean an `#[expect]` was placed where the lint does not actually fire.

- [ ] **Step 8: Run the full allocation suite unmodified**

Run: `cargo test -p tt-core --lib allocation`
Expected: all 56 tests pass (the 55 existing plus the new equivalence test), **with no edits to any existing test**. Confirm by name that these three passed:
- `test_total_direct_time_never_exceeds_wall_clock`
- `test_parallel_agent_sessions_sum_delegated_beyond_wall_clock`
- `a_sessions_delegated_span_never_outruns_the_end_event_that_closed_it`

If any existing test required a change to pass, revert and find what the move altered.

- [ ] **Step 9: Full gates**

Run: `cargo fmt && cargo clippy --all-targets && cargo test`
Expected: fmt silent, zero clippy warnings, 1,155 tests passing.

- [ ] **Step 10: Verify the reported numbers are still identical**

```bash
cp ~/.local/share/time-tracker/tt.db /tmp/t2.db
cargo build --release --bin tt
TT_DATABASE_PATH=/tmp/t2.db ./target/release/tt report --start 2026-07-20 --end 2026-07-21 | grep '^Direct time:'
TT_DATABASE_PATH=/tmp/t2.db ./target/release/tt report --start 2026-07-13 --end 2026-07-20 | grep '^Direct time:'
```

Expected: `Direct time: 16h 23m`, `Direct time: 74h 20m`. This task is a pure refactor; anything else means the move changed behavior.

- [ ] **Step 11: Commit**

```bash
jj describe -m "refactor(allocation): expose the pass as an incremental Allocator

allocate_time was already a single forward walk with its state in maps; it
held the whole history only because it took a slice. Allocator::push and
::finish are that same walk, fed one event at a time. allocate_time is now
a wrapper over it, so every caller and all 55 existing tests are untouched.
Allocator::new joins allocate_time in clippy.toml's disallowed-methods, so
the streaming pass has the same single entry point the slice one had.
jj new
```

---

### Task 3: Stream events from SQLite into the `Allocator`

`allocate_for_period` (`crates/tt-db/src/lib.rs:4633`) calls `get_events_in_range`, materializing every event in the window — the second of the two full copies. This task feeds the `Allocator` row by row instead, and replaces the in-memory pre-pass that finds sessions with tool use but no `started` event with a query.

**Files:**
- Modify: `crates/tt-db/src/lib.rs` (add two methods near line 1472; rewrite `allocate_for_period` at 4633)
- Test: `crates/tt-db/src/lib.rs` (`mod tests`)

**Interfaces:**
- Consumes: `tt_core::allocation::Allocator::{new, push, finish}` from Task 2 — note `finish(mut self)`, and that `Allocator::new` is now in `clippy.toml`'s `disallowed-methods`, so `allocate_for_period` keeps the single `#[expect]` it already carries.
- Produces:
  - `Database::for_each_event_in_range<F>(&self, start: DateTime<Utc>, end: DateTime<Utc>, f: F) -> Result<(), DbError> where F: FnMut(StoredEvent)`
  - `Database::sessions_with_tool_use_but_no_start(&self, start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Vec<String>, DbError>` — ordered ascending, distinct.

- [ ] **Step 1: Write the failing test for streaming reads**

```rust
    #[test]
    fn for_each_event_in_range_visits_rows_in_timestamp_order_without_collecting() {
        let db = Database::open_in_memory().unwrap();
        db.insert_events(&[
            make_event("b", Utc.with_ymd_and_hms(2026, 3, 1, 9, 1, 0).unwrap(), EventType::UserMessage),
            make_event("a", Utc.with_ymd_and_hms(2026, 3, 1, 9, 0, 0).unwrap(), EventType::UserMessage),
            make_event("c", Utc.with_ymd_and_hms(2026, 3, 1, 9, 2, 0).unwrap(), EventType::UserMessage),
            make_event("outside", Utc.with_ymd_and_hms(2026, 4, 1, 9, 0, 0).unwrap(), EventType::UserMessage),
        ])
        .unwrap();

        let mut seen = Vec::new();
        db.for_each_event_in_range(
            Utc.with_ymd_and_hms(2026, 3, 1, 9, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 3, 1, 9, 2, 0).unwrap(),
            |event| seen.push(event.id),
        )
        .unwrap();

        assert_eq!(seen, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    }
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p tt-db --lib for_each_event_in_range_visits_rows_in_timestamp_order_without_collecting`
Expected: compile error — method not found.

- [ ] **Step 3: Implement `for_each_event_in_range`**

Mirror `get_events_in_range` (line 1472) exactly, but hand each row to the callback instead of pushing it into a `Vec`:

```rust
    /// Visits every event in `start..=end`, in ascending timestamp order.
    ///
    /// The streaming counterpart of [`Self::get_events_in_range`], which returns a `Vec`
    /// and therefore holds the whole window: on the live corpus that was 2,738,805 rows
    /// and the larger half of `tt recompute`'s 8.9 GB. The allocation pass only ever
    /// walks forward, so it never needed the `Vec`.
    ///
    /// Rows that cannot be read are skipped, matching `get_events_in_range`.
    pub fn for_each_event_in_range<F>(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        mut f: F,
    ) -> Result<(), DbError>
    where
        F: FnMut(StoredEvent),
    {
        let sql = format!(
            "SELECT {EVENT_COLUMNS} FROM events
             WHERE timestamp >= ?1 AND timestamp <= ?2
             ORDER BY timestamp ASC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![format_timestamp(start), format_timestamp(end)])?;
        while let Some(row) = rows.next()? {
            if let Some(event) = Self::row_to_event(row)? {
                f(event);
            }
        }
        Ok(())
    }
```

- [ ] **Step 4: Run it and confirm it passes**

Run: `cargo test -p tt-db --lib for_each_event_in_range_visits_rows_in_timestamp_order_without_collecting`
Expected: `test result: ok. 1 passed`

- [ ] **Step 5: Write the failing test for the missing-start query**

```rust
    #[test]
    fn sessions_with_tool_use_but_no_start_finds_only_the_unstarted() {
        let db = Database::open_in_memory().unwrap();
        let window_start = Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap();
        let window_end = Utc.with_ymd_and_hms(2026, 3, 2, 0, 0, 0).unwrap();

        let mut started = make_event("s", Utc.with_ymd_and_hms(2026, 3, 1, 9, 0, 0).unwrap(), EventType::AgentSession);
        started.session_id = Some("has-start".to_string());
        started.action = Some("started".to_string());
        let mut tool_with = make_event("t1", Utc.with_ymd_and_hms(2026, 3, 1, 9, 1, 0).unwrap(), EventType::AgentToolUse);
        tool_with.session_id = Some("has-start".to_string());
        let mut tool_without = make_event("t2", Utc.with_ymd_and_hms(2026, 3, 1, 9, 2, 0).unwrap(), EventType::AgentToolUse);
        tool_without.session_id = Some("no-start".to_string());
        db.insert_events(&[started, tool_with, tool_without]).unwrap();

        assert_eq!(
            db.sessions_with_tool_use_but_no_start(window_start, window_end).unwrap(),
            vec!["no-start".to_string()]
        );
    }
```

- [ ] **Step 6: Run it and confirm it fails**

Run: `cargo test -p tt-db --lib sessions_with_tool_use_but_no_start_finds_only_the_unstarted`
Expected: compile error — method not found.

- [ ] **Step 7: Implement `sessions_with_tool_use_but_no_start`**

This must reproduce, in SQL, exactly what `allocate_for_period` currently computes at lines 4647–4663: session ids of `agent_tool_use` events in the window whose session has no `agent_session`/`started` event in that same window.

```rust
    /// Sessions with agent tool use in the window but no `started` event in it.
    ///
    /// The query form of the pre-pass `allocate_for_period` used to run over a
    /// materialized `Vec` of every event in the window. Same window, same predicate: a
    /// session whose start fell outside the range needs its start event fetched, or its
    /// delegated span has no opening.
    pub fn sessions_with_tool_use_but_no_start(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<String>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT t.session_id FROM events t
             WHERE t.type = 'agent_tool_use'
               AND t.session_id IS NOT NULL
               AND t.timestamp >= ?1 AND t.timestamp <= ?2
               AND NOT EXISTS (
                   SELECT 1 FROM events s
                   WHERE s.type = 'agent_session' AND s.action = 'started'
                     AND s.session_id = t.session_id
                     AND s.timestamp >= ?1 AND s.timestamp <= ?2
               )
             ORDER BY t.session_id ASC",
        )?;
        let rows = stmt.query_map(
            params![format_timestamp(start), format_timestamp(end)],
            |row| row.get::<_, String>(0),
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
    }
```

Confirm the stored discriminants for `EventType::AgentToolUse` and `EventType::AgentSession` really are the strings `'agent_tool_use'` and `'agent_session'` by checking how `insert_events` serializes `event_type` — if they differ, use the real values.

- [ ] **Step 8: Run it and confirm it passes**

Run: `cargo test -p tt-db --lib sessions_with_tool_use_but_no_start_finds_only_the_unstarted`
Expected: `test result: ok. 1 passed`

- [ ] **Step 9: Pin the ordering the rewrite must reproduce — before rewriting anything**

`allocate_for_period` **prepends** the backfilled start events (`crates/tt-db/src/lib.rs:4665-4668`: `start_events.append(&mut events); events = start_events;`), so a session whose `started` fell outside the window is opened before any of its tool use is seen. That is load-bearing rather than incidental: the `EventType::AgentToolUse` arm (`allocation.rs:529-540`) is `if let Some(session) = agent_sessions.get_mut(session_id)` — it **never creates** a session — so a tool use arriving before its `started` contributes nothing at all and the session's whole delegated span is lost.

The streamed version reproduces it by pushing those starts first and then the window in timestamp order, which is **deliberately not a global timestamp sort**. A start event that is outside the window but *after* it — malformed data, but data — would sort last under a global sort and open the session after its tool use had already been discarded. Pushing the backfilled starts first is correct for both directions.

Write this test now, against the **current collecting implementation**, so it characterizes the behaviour the rewrite must preserve. Add to `crates/tt-db/src/lib.rs` inside `mod tests`:

```rust
    #[test]
    fn a_session_started_before_the_window_still_accrues_delegated_time() {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("stream-a", Some("A"))).unwrap();

        // The session opens an hour before the window and works inside it, so its
        // `started` event is not among the rows the window query returns.
        let mut started = make_event(
            "s",
            Utc.with_ymd_and_hms(2026, 3, 1, 8, 0, 0).unwrap(),
            EventType::AgentSession,
        );
        started.session_id = Some("session-1".to_string());
        started.action = Some("started".to_string());
        started.stream_id = Some("stream-a".to_string());

        let mut first_tool = make_event(
            "t1",
            Utc.with_ymd_and_hms(2026, 3, 1, 9, 5, 0).unwrap(),
            EventType::AgentToolUse,
        );
        first_tool.session_id = Some("session-1".to_string());
        first_tool.stream_id = Some("stream-a".to_string());

        let mut last_tool = make_event(
            "t2",
            Utc.with_ymd_and_hms(2026, 3, 1, 9, 20, 0).unwrap(),
            EventType::AgentToolUse,
        );
        last_tool.session_id = Some("session-1".to_string());
        last_tool.stream_id = Some("stream-a".to_string());

        db.insert_events(&[started, first_tool, last_tool]).unwrap();

        let config = tt_core::AllocationConfig::default();
        let result = allocate_for_period(
            &db,
            Utc.with_ymd_and_hms(2026, 3, 1, 9, 0, 0).unwrap(),
            Utc.with_ymd_and_hms(2026, 3, 1, 9, 30, 0).unwrap(),
            None,
            &config,
        )
        .unwrap();

        // 09:05 (first tool use) to 09:20 (the last event, which closes the still-open
        // session). Zero here means the backfilled start never reached the allocator.
        let stream = result
            .stream_times
            .iter()
            .find(|s| s.stream_id == "stream-a")
            .expect("stream-a should have accrued delegated time");
        assert_eq!(stream.time_delegated_ms, 15 * 60 * 1000);
    }
```

Run: `cargo test -p tt-db --lib a_session_started_before_the_window_still_accrues_delegated_time`
Expected: **PASS already.** Like Task 4's determinism guard, this is written before the change so it can catch a regression the change might introduce; it is not a TDD red step. If it fails now, the prepend is not doing what this step says it does — stop and re-read `allocate_for_period` before rewriting it.

- [ ] **Step 10: Rewrite `allocate_for_period` to stream**

Replace lines 4640–4669 of `crates/tt-db/src/lib.rs` (the `get_events_in_range` load, the two `BTreeSet`/`Vec` pre-passes, and the `start_events.append(&mut events)` splice) so that the function builds the maps first, then streams. Keep everything after the current line 4671 (`agent_sessions_in_range` onward) — including how `session_types` and `session_end_times` are built — unchanged, and keep feeding `Allocator` in the **same order** the old code produced: fetched start events first, then the window's events in timestamp order.

```rust
    let inclusive_end = end - chrono::Duration::milliseconds(1);
    if inclusive_end < start {
        let agent_sessions = db.agent_sessions_in_range(start, end)?;
        // ... build session_types / session_end_times exactly as below, then:
        let allocator = tt_core::allocation::Allocator::new(
            config,
            period_end,
            &session_end_times,
            &session_types,
        );
        return Ok(allocator.finish());
    }
```

then, for the normal path, after `session_types` and `session_end_times` are built:

```rust
    let mut allocator =
        tt_core::allocation::Allocator::new(config, period_end, &session_end_times, &session_types);

    // Start events for sessions whose opening fell outside the window are pushed first,
    // exactly as the old code prepended them to the Vec.
    let missing_session_ids = db.sessions_with_tool_use_but_no_start(start, inclusive_end)?;
    if !missing_session_ids.is_empty() {
        for event in db.get_agent_session_start_events(&missing_session_ids)? {
            allocator.push(&event);
        }
    }

    // Then the window itself, streamed in timestamp order: one row is resident at a time
    // instead of all 2.7M.
    db.for_each_event_in_range(start, inclusive_end, |event| allocator.push(&event))?;

    Ok(allocator.finish())
```

**Keep the `#[expect(clippy::disallowed_methods, …)]` on `allocate_for_period` — do not remove it.** It sits at `crates/tt-db/src/lib.rs:4629-4632` and today reads `reason = "single permitted allocation boundary"`. The `allocate_time` call it covered is gone, but Task 2 Step 7 added `tt_core::allocation::Allocator::new` to `clippy.toml`'s `disallowed-methods`, and this function now calls it **twice** — once in the empty-window early return, once on the normal path. A function-level `#[expect]` covers both, which is why it stays function-level rather than moving onto a statement. Update only its reason so it names what it now permits:

```rust
#[expect(
    clippy::disallowed_methods,
    reason = "single permitted allocation boundary; allocate_for_period is the only caller of Allocator::new"
)]
```

Removing it would fail `cargo clippy -D warnings`; leaving it with the old reason would pass but would misdescribe the boundary. Do **not** add `Allocator::push` or `Allocator::finish` to `clippy.toml` — gating `new` already makes this the single entry point, and listing all three would only multiply suppressions at this one sanctioned site.

- [ ] **Step 11: Full gates plus every allocation test**

Run: `cargo fmt && cargo clippy --all-targets && cargo test`
Expected: fmt silent, zero clippy warnings, all tests pass. Confirm by name that `a_session_started_before_the_window_still_accrues_delegated_time` still passes — it is the ordering guard from Step 9 and the one test that fails if the backfilled starts stop arriving first. The tests in `crates/tt-cli/tests/` that exercise reports and recompute end-to-end are the other real check here — they compare full outputs.

- [ ] **Step 12: Verify numbers and measure the win**

```bash
cp ~/.local/share/time-tracker/tt.db /tmp/t3.db
cargo build --release --bin tt
TT_DATABASE_PATH=/tmp/t3.db ./target/release/tt report --start 2026-07-20 --end 2026-07-21 | grep '^Direct time:'
TT_DATABASE_PATH=/tmp/t3.db ./target/release/tt report --start 2026-07-13 --end 2026-07-20 | grep '^Direct time:'
TT_DATABASE_PATH=/tmp/t3.db /usr/bin/time -f 'wall=%es cpu=%P peak_rss=%MKB' ./target/release/tt recompute | tail -3
```

Expected: `Direct time: 16h 23m` and `Direct time: 74h 20m` still. `peak_rss` now dominated by recorded intervals and per-stream state rather than events — target well under 1 GB against the 8.9 GB baseline. **Record wall, cpu and peak_rss: Task 4 is gated on this peak_rss figure and must be reverted if it raises it.**

- [ ] **Step 13: Commit**

```bash
jj describe -m "perf(allocation): stream events instead of materializing the window

allocate_for_period loaded every event in the range into a Vec and ran two
in-memory pre-passes over it. It now streams rows into Allocator::push and
answers the missing-start question with a query. Same order in, same
numbers out; resident set no longer scales with history length."
jj new
```

---

### Task 4: Parallelize the per-stream union

`finish` unions each stream's `focus_intervals` to get `time_direct_ms`. Streams are independent of one another, so this is the one genuinely CPU-bound, embarrassingly parallel part of the pass — and it currently runs on one of 24 cores. `rayon` is already a `tt-core` dependency (`crates/tt-core/Cargo.toml:15`) and already used in `session.rs:624` and `opencode.rs:219`.

**Files:**
- Modify: `crates/tt-core/src/allocation.rs` (`Allocator::finish`)
- Test: `crates/tt-core/src/allocation.rs` (`mod tests`)

**Interfaces:**
- Consumes: `Allocator::finish(mut self) -> AllocationResult` from Task 2, and the `peak_rss` figure recorded at Task 3 Step 12.
- Produces: no signature change. `finish` stays `finish(mut self) -> AllocationResult`.

- [ ] **Step 1: GATE — decide from Task 3's memory figure whether to do this task at all**

**This task is conditional, and abandoning it is a successful outcome, not a failure.** Task 3 removes the materialization; this one only spends cores on what is left. It can also make things *worse* on the very axis this plan exists to fix. `union_duration_ms` (`crates/tt-core/src/allocation.rs:722-732`) does not union in place — it **clones** the intervals it is handed into a fresh `Vec` (`.iter().filter(|i| i.end > i.start).copied().collect()`) before sorting and merging. Serially exactly one such copy is alive at a time. Under `into_par_iter` rayon runs as many as it has threads — 24 on this machine — so peak RSS rises by roughly the 23 largest additional copies. For a stream with a big `focus_intervals` that is a real number, not a rounding error.

Record the target before touching anything:

```bash
# The peak_rss printed by Task 3 Step 12, in KB. This is the number to beat.
TASK3_PEAK_RSS_KB=<paste the figure recorded at Task 3 Step 12>
echo "Task 3 peak RSS: ${TASK3_PEAK_RSS_KB} KB (baseline was 8,928,316 KB)"
```

Continue to Step 2 **only if** that figure is comfortably inside target — well under 1 GB, an order of magnitude below the 8,928,316 KB baseline. If Task 3 landed near or above that, **stop here**: memory is still the binding constraint, and parallelising the union spends memory to buy wall time. Record the figure, skip to Task 5, and say so in Task 5's `AGENTS.md` entry.

The gate has a second half at Step 8: if the measurement *after* parallelising shows peak RSS above `TASK3_PEAK_RSS_KB`, this task is reverted. That is the gate working, not the plan failing — same shape as `specs/implementation/2026-08-09-read-pool-plan.md`'s Task 1 Step 4, and the same precedent as `CLASSIFY_CONCURRENCY`, which was tried at 16 and reverted for +18% at the cost of rate limiting.

- [ ] **Step 2: Write the determinism test — it must pass before the change**

Parallel iteration must not reorder or perturb results. This test pins that many streams come back with identical, deterministically ordered totals:

```rust
    #[test]
    fn many_streams_union_identically_however_the_work_is_scheduled() {
        // Given: enough streams that rayon will actually split the work.
        // Given: enough streams that rayon will actually split the work, emitted in
        // ascending timestamp order because that is what the pass requires — stream-major
        // order would push timestamps backwards and measure nothing.
        let mut events = Vec::new();
        let mut minute = 0_i64;
        for _round in 0..8 {
            for stream in 0..64 {
                events.push(TestEvent::tmux_focus(ts(minute), &format!("stream-{stream:02}")));
                minute += 1;
            }
        }
        let config = test_config();
        let end_times = HashMap::new();
        let types = HashMap::new();

        // When: the same corpus is allocated twice.
        let run = || {
            let mut allocator = Allocator::new(&config, None, &end_times, &types);
            for event in &events {
                allocator.push(event);
            }
            let mut times = allocator.finish().stream_times;
            times.sort_by(|a, b| a.stream_id.cmp(&b.stream_id));
            times
        };
        let first = run();
        let second = run();

        // Then: byte-identical, and every stream present.
        assert_eq!(first.len(), 64);
        for (a, b) in first.iter().zip(second.iter()) {
            assert_eq!(a.stream_id, b.stream_id);
            assert_eq!(a.time_direct_ms, b.time_direct_ms);
            assert_eq!(a.time_delegated_ms, b.time_delegated_ms);
        }
    }
```

- [ ] **Step 3: Run it and confirm it passes already**

Run: `cargo test -p tt-core --lib many_streams_union_identically_however_the_work_is_scheduled`
Expected: PASS. This is a guard written *before* the optimization so it can catch a regression the optimization might introduce — it is not expected to fail first. Note that explicitly in the commit message.

- [ ] **Step 4: Parallelize the union inside `finish`**

In `Allocator::finish`, the finalization reads as follows. This is the source at `crates/tt-core/src/allocation.rs:695-711` **before** Task 2's move; after Task 2 each of these locals is `self.`-prefixed, which is why the replacement below reads `self.stream_times`:

```rust
    // Calculate total tracked time from interval union
    let total_tracked_ms = union_duration_ms(&activity_intervals);

    let unassigned = stream_times
        .remove(UNASSIGNED_STREAM_ID)
        .unwrap_or_default();

    let stream_times_vec = stream_times
        .into_iter()
        .map(|(stream_id, allocation)| StreamTime {
            stream_id,
            time_direct_ms: union_duration_ms(&allocation.focus_intervals),
            time_delegated_ms: allocation.delegated_ms,
            focus_intervals: allocation.focus_intervals,
            delegated_intervals: allocation.delegated_intervals,
        })
        .collect();
```

Replace only the `stream_times_vec` binding with a parallel map, then sort:

```rust
    let mut stream_times_vec: Vec<StreamTime> = self
        .stream_times
        .into_iter()
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|(stream_id, allocation)| StreamTime {
            stream_id,
            time_direct_ms: union_duration_ms(&allocation.focus_intervals),
            time_delegated_ms: allocation.delegated_ms,
            focus_intervals: allocation.focus_intervals,
            delegated_intervals: allocation.delegated_intervals,
        })
        .collect();
    // Sorted so output order does not depend on how rayon scheduled the work; the previous
    // `HashMap` iteration order was already unspecified, and parallel collection makes that
    // worse rather than different.
    stream_times_vec.sort_by(|a, b| a.stream_id.cmp(&b.stream_id));
```

Add `use rayon::prelude::*;` at the top of `allocation.rs` (it is already a `tt-core` dependency and already imported this way in `session.rs:8`).

**Four things must not move.** The `stream_times.remove(UNASSIGNED_STREAM_ID)` **has to stay before** this map: `UNASSIGNED_STREAM_ID` (`allocation.rs:21`) is the synthetic `"(unassigned)"` bucket that unattributed events accrue into, and the doc contract at line 179 is that the sentinel is removed from `stream_times` before returning. Parallelizing before removing it would publish `(unassigned)` as a real stream row and double-count it in every report. That `remove` is also why `finish` takes **`mut self`** (`allocation.rs:698-700`) — this task does not change that and must not "tidy" it to `self`. `total_tracked_ms` stays a union of `activity_intervals`. And `time_delegated_ms` stays `allocation.delegated_ms` — **a sum across concurrent sessions, never a union**; `test_parallel_agent_sessions_sum_delegated_beyond_wall_clock` (line 2402) fails if that changes.

If the existing code did not sort `stream_times`, adding the sort is still correct and required — parallel collection makes the previous `HashMap` iteration order even less defined than it was. Check whether any insta snapshot depends on that order; if one does, run `cargo insta review` and accept only ordering changes.

- [ ] **Step 5: Run the allocation suite and the determinism guard**

Run: `cargo test -p tt-core --lib allocation`
Expected: all tests pass, including `many_streams_union_identically_however_the_work_is_scheduled`, `test_total_direct_time_never_exceeds_wall_clock`, and `test_parallel_agent_sessions_sum_delegated_beyond_wall_clock`.

- [ ] **Step 6: Full gates**

Run: `cargo fmt && cargo clippy --all-targets && cargo test`
Expected: fmt silent, zero clippy warnings, all tests pass. If insta snapshots changed, `cargo insta review` and confirm every diff is ordering only.

- [ ] **Step 7: Verify numbers, CPU utilization, and peak RSS against the gate**

```bash
cp ~/.local/share/time-tracker/tt.db /tmp/t4.db
cargo build --release --bin tt
TT_DATABASE_PATH=/tmp/t4.db ./target/release/tt report --start 2026-07-20 --end 2026-07-21 | grep '^Direct time:'
TT_DATABASE_PATH=/tmp/t4.db ./target/release/tt report --start 2026-07-13 --end 2026-07-20 | grep '^Direct time:'
TT_DATABASE_PATH=/tmp/t4.db /usr/bin/time -f 'wall=%es cpu=%P peak_rss=%MKB' ./target/release/tt recompute | tail -3
```

Expected: the same two direct-time figures. `cpu=` now above 100% (it was 99% = one core). Record wall, cpu, and **peak_rss** — the last of those is what Step 8 judges against `TASK3_PEAK_RSS_KB`.

- [ ] **Step 8: Apply the gate's exit condition**

Compare Step 7's `peak_rss` with `TASK3_PEAK_RSS_KB` from Step 1. Three outcomes, and two of them revert:

- **peak RSS did not rise and `cpu=` is above 100%** → keep the change and continue to Step 9.
- **peak RSS rose above `TASK3_PEAK_RSS_KB`** → **revert this task**, by the recipe below.
- **peak RSS rose but `cpu=` did not** → **revert as well.** Extra memory for no parallelism is the worst of both.

To revert: restore the serial form quoted in Step 4 — put `stream_times_vec` back to `self.stream_times.into_iter().map(…).collect()` and remove the `use rayon::prelude::*;` import. **Keep the determinism test from Step 2**; it passes either way and costs nothing to leave standing. Then commit the measurement instead of the change, replacing Step 9's message with:

```bash
jj describe -m "perf(allocation): measure and reject parallelising the per-stream union

union_duration_ms clones each stream's intervals before merging, so running
the per-stream map under rayon holds up to one copy per thread instead of
one at a time. Measured on the live corpus: peak RSS <AFTER> KB against
<TASK3_PEAK_RSS_KB> KB serial. Reverted; the serial union stays."
jj new
```

Add both peak-RSS figures to the `AGENTS.md` entry Task 5 writes, and treat this as Step 9 — do not also commit the parallel version. A measured "no" is a result this repo keeps;
see the withdrawn temporal-evidence experiment in `crates/tt-core/AGENTS.md`.

- [ ] **Step 9: Commit**

```bash
jj describe -m "perf(allocation): union each stream's intervals in parallel

Streams are independent at union time, so the one genuinely CPU-bound part
of the pass ran on 1 of 24 cores. rayon was already a dependency and
already used for ingest parsing. Output is sorted by stream id so results
do not depend on scheduling; the determinism guard was written first."
jj new
```

---

### Task 5: Lock the win in and record it

A performance fix with no guard rots. This task adds a **structural** guard — one that proves the streaming path is the one being exercised, rather than inferring it from a stopwatch — and writes the measured numbers into `AGENTS.md` so the next person does not have to rediscover them.

**Files:**
- Test: `crates/tt-db/src/lib.rs` (`mod tests`)
- Modify: `AGENTS.md`
- Modify: `crates/tt-db/AGENTS.md`

**Interfaces:**
- Consumes: everything from Tasks 1–4.

- [ ] **Step 1: Write the failing structural guard**

Assert the thing that is actually true of the fix: **`allocate_for_period` no longer calls `get_events_in_range`.** A wall-clock bound would be weaker and flakier — it depends on the machine, on CI load, and on how much slower "materialized" happens to be at whatever corpus size the test can afford, and it can pass with the collecting form on a fast box. A call counter cannot. It is the same posture as `crates/tt-cli/tests/sync_no_recompute.rs`, which proves `tt sync` does not recompute by observing an effect that only the wrong path produces, rather than by timing it.

Add to `crates/tt-db/src/lib.rs` inside `mod tests`:

```rust
    #[test]
    fn allocate_for_period_streams_the_window_instead_of_collecting_it() {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_stream("stream-a", Some("A"))).unwrap();

        // 5,000 focus events on one stream. The size is incidental to the guard — it is
        // the call count that proves the path — but a corpus large enough to make the
        // streaming loop iterate keeps the correctness assertions meaningful.
        let base = Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap();
        let mut batch = Vec::with_capacity(5_000);
        for index in 0..5_000_i64 {
            let mut event = make_event(
                &format!("f{index}"),
                base + chrono::Duration::seconds(index * 10),
                EventType::TmuxPaneFocus,
            );
            event.stream_id = Some("stream-a".to_string());
            batch.push(event);
        }
        db.insert_events(&batch).unwrap();

        let (first, last) = db.event_time_bounds().unwrap().unwrap();
        let config = tt_core::AllocationConfig::default();

        let before = events_in_range_calls();
        let result = allocate_for_period(
            &db,
            first,
            last + chrono::Duration::milliseconds(1),
            None,
            &config,
        )
        .unwrap();

        // The guard: the collecting reader was never touched.
        assert_eq!(
            events_in_range_calls(),
            before,
            "allocate_for_period called get_events_in_range; the window is being \
             materialized again"
        );

        // And the pass really ran, so the guard is not passing by doing nothing:
        // one stream, with direct time that is a union and so cannot exceed the span.
        assert_eq!(result.stream_times.len(), 1);
        let span_ms = (last - first).num_milliseconds();
        assert!(
            result.stream_times[0].time_direct_ms > 0,
            "no direct time accrued; the streamed events never reached the allocator"
        );
        assert!(
            result.stream_times[0].time_direct_ms <= span_ms,
            "direct {} exceeded span {}",
            result.stream_times[0].time_direct_ms,
            span_ms
        );
    }
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p tt-db --lib allocate_for_period_streams_the_window_instead_of_collecting_it`
Expected: compile error — `cannot find function 'events_in_range_calls' in this scope`.

- [ ] **Step 3: Add the test-only call counter**

Add at module level in `crates/tt-db/src/lib.rs` (not inside `mod tests` — `get_events_in_range` has to see it), next to the other free items near the top of the file:

```rust
/// Counts [`Database::get_events_in_range`] calls made on this thread.
///
/// Exists so a test can assert *structurally* that `allocate_for_period` streams the
/// window rather than collecting it, instead of inferring it from a stopwatch. Thread-
/// local rather than a field on `Database`: `Database` is `Send` but not `Sync` and an
/// allocation runs entirely on its caller's thread, so per-thread counting is exact and
/// unaffected by cargo running tests in parallel. Compiled out entirely outside tests.
#[cfg(test)]
thread_local! {
    static EVENTS_IN_RANGE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times this thread has called [`Database::get_events_in_range`].
#[cfg(test)]
fn events_in_range_calls() -> usize {
    EVENTS_IN_RANGE_CALLS.with(std::cell::Cell::get)
}
```

Then add exactly one line as the first statement of `Database::get_events_in_range` (`crates/tt-db/src/lib.rs:1472`), before the `let sql = format!(…)`:

```rust
        #[cfg(test)]
        EVENTS_IN_RANGE_CALLS.with(|calls| calls.set(calls.get() + 1));
```

That is the whole hook. It adds nothing to release builds, changes no query, and no other method is instrumented — `get_events_in_range` is the only collecting reader `allocate_for_period` ever used.

- [ ] **Step 4: Run it and confirm it passes**

Run: `cargo test -p tt-db --lib allocate_for_period_streams_the_window_instead_of_collecting_it`
Expected: `test result: ok. 1 passed`

- [ ] **Step 5: Confirm the guard has teeth**

Temporarily revert `allocate_for_period` to the collecting form (`let events = db.get_events_in_range(start, inclusive_end)?;` then `allocate_time(&events, …)`), run the test, and confirm it **fails on the call-count assertion** — not on a timing margin, and not intermittently. Then restore the streaming form. Do not commit the reverted state.

Run: `cargo test -p tt-db --lib allocate_for_period_streams_the_window_instead_of_collecting_it -- --nocapture`
Expected (reverted): FAIL, `allocate_for_period called get_events_in_range; the window is being materialized again`. Expected (restored): PASS.

This determinism is the point of the change. A test that merely got slower would leave a reviewer guessing whether the machine was busy.

- [ ] **Step 6: Record the measurement in the root `AGENTS.md`**

Add an entry to the **Anti-Patterns** list in `AGENTS.md`, in the voice of the surrounding entries — dated measurements, and the rule stated as a prohibition:

```markdown
- **Allocation streams the event history and must never materialize it again**: `allocate_time` has always been a single forward walk over timestamp-sorted events with its state in maps, but it took a `&[E]`, and `tt recompute` fed it by loading every row. Measured on the live 2.2 GB corpus (2,738,805 events): **250.48 s wall, 8.9 GB peak RSS, 99% CPU — one of 24 cores** — to produce a per-stream group-by that `sqlite3` answers in **0.73 s and 6.4 MB**. Two full copies were resident, because `recompute::run` loaded the whole history to derive a `MIN`, a `MAX`, and a split-session warning, and then `allocate_for_period` loaded the window again. Both are gone: the bounds and the warning are aggregates (`event_time_bounds`, `sessions_spanning_multiple_streams`), the window streams through `for_each_event_in_range` into `tt_core::allocation::Allocator`, and the missing-start pre-pass is a query (`sessions_with_tool_use_but_no_start`). After: **<RECORD wall/cpu/peak_rss FROM TASK 4 STEP 7, or from TASK 3 STEP 12 if Task 4's gate rejected the change>**. The guard is `allocate_for_period_streams_the_window_instead_of_collecting_it`, and it is **structural rather than timed** — it counts `get_events_in_range` calls, so re-introducing the collecting form fails it deterministically rather than merely slowly; it was verified against that form. Two things this rewrite may not change: the backfilled `agent_session` start events are pushed **before** the window and not merged into timestamp order (a tool use whose session is not yet open contributes nothing, so ordering is the whole mechanism), and the aggregates read the raw `events` table while `row_to_event` silently drops unreadable timestamps and unknown types — measured at **<RECORD the Task 1 Step 1 counts>** affected rows. **The slowness had become load-bearing**, which is the part worth remembering: `tt sync` was made 3.6 min → 18.6 s by *deleting* its recompute call, that deletion is now a rule with its own regression test, and `tt streams` labels its totals with their age rather than refreshing them. A 250-second function shaped three other designs before anyone treated it as a defect. Adding a `Vec` of events back into this path re-imposes all of it.
```

Replace both `<RECORD …>` markers with the real figures before committing. If Task 4's gate rejected parallelising the union, say so in the same entry and give both peak-RSS figures — a measured rejection is a result, and an entry that omits it invites the next person to re-run the same experiment. Also update the existing **`tt sync` must not recompute** entry to note that recomputation is no longer minutes of CPU and gigabytes of RSS — leaving that entry stating the old cost would make two parts of this file disagree. Do **not** delete that rule or re-add `recompute::run` to `sync::run`: the ordering guarantee it protects is separate from its cost, and `tt-cli/tests/sync_no_recompute.rs` guards it.

- [ ] **Step 7: Record the new methods in `crates/tt-db/AGENTS.md`**

Add rows to the **Events** table of the Method Reference. Note that `event_time_bounds` already existed in code and was simply never listed there — documenting it now closes that gap:

```markdown
| `event_time_bounds` | `MIN`/`MAX` of `events.timestamp`, or `None` when empty. What `recompute` used to derive by materializing all 2.7M events. Reads the raw table, so unlike `get_events` it does not drop rows `row_to_event` cannot parse |
| `for_each_event_in_range` | Streaming counterpart of `get_events_in_range`: visits each row in ascending timestamp order without collecting. The allocation pass walks forward only, so it never needed the `Vec` |
| `sessions_spanning_multiple_streams` | Sessions whose events point at more than one stream, with those streams. A data-integrity report, ordered for stable output |
| `sessions_with_tool_use_but_no_start` | Sessions with `agent_tool_use` in a window but no `agent_session`/`started` in it — the query form of a pre-pass that used to run over a materialized `Vec` |
```

And note under the **Allocation entry point** section that `allocate_for_period` now streams into `tt_core::allocation::Allocator` rather than collecting, and that its `end` is still exclusive.

- [ ] **Step 8: Full gates**

Run: `cargo fmt --check && cargo clippy --all-targets && cargo test`
Expected: fmt silent, zero clippy warnings, all tests pass.

- [ ] **Step 9: Final end-to-end verification against the live corpus — the acceptance evidence**

```bash
cp ~/.local/share/time-tracker/tt.db /tmp/final.db
cargo build --release --bin tt --bin tt-serve
for range in "2026-07-20 2026-07-21" "2026-07-13 2026-07-20" "2026-06-29 2026-07-06"; do
  set -- $range
  TT_DATABASE_PATH=/tmp/final.db ./target/release/tt report --start $1 --end $2 \
    | grep -E '^(Wall clock|Direct time|Delegated|Leverage):'
done
TT_DATABASE_PATH=/tmp/final.db /usr/bin/time -f 'wall=%es cpu=%P peak_rss=%MKB' ./target/release/tt recompute | tail -3
```

Expected: `Direct time: 16h 23m` for Jul 20 and `74h 20m` for the Jul 13 week. Delegated and Leverage unchanged from a pre-change run of the same command on the same copy — capture both and diff them. Then confirm the daemon still builds and starts, since `tt-serve` links `tt-core`:

```bash
systemctl --user stop tt-serve.service && sleep 3
cp target/release/tt-serve ~/.local/bin/tt-serve
systemctl --user start tt-serve.service && sleep 30
systemctl --user is-active tt-serve.service
journalctl --user -u tt-serve.service --since "2 min ago" --no-pager -o cat | grep -ci "ingest failed"
```

Expected: `active`, and `0` ingest failures.

**This step, not a test, is where the performance claim is accepted.** The automated guard proves the streaming path is in use; these figures prove it was worth doing. Record wall, cpu and peak_rss from the `recompute` line and carry them into Step 6's `AGENTS.md` entry.

- [ ] **Step 10: Commit**

```bash
jj describe -m "test(allocation): guard against re-materializing the event history

allocate_for_period is asserted not to call get_events_in_range, through a
test-only per-thread call counter, so re-introducing the collecting form
fails deterministically rather than merely slowly. Verified against that
form. AGENTS.md records the measured before/after and that the old cost had
become load-bearing: tt sync was sped up by deleting its recompute call
rather than by fixing recompute."
jj new
```

---

## Self-Review

**Spec coverage.** The critique was: allocation is single-threaded, materializes everything, and 340× slower than the equivalent database aggregate, and the codebase has been designed around that rather than fixing it. Mapped: redundant full load → Task 1; materialization of the window → Tasks 2 and 3; single-core union → Task 4, now **conditional** on Task 3's memory figure; structural regression guard and the load-bearing-slowness note → Task 5. The async-classifier rearchitecture and the read pool are deliberately **not** here — separate subsystems, separate plans, per the skill's scope check.

A review added seven amendments, and each landed **inside the affected task** rather than in this preamble, because a worker sees only its own task's text:

- **Task 1 was stale.** `Database::event_time_bounds` already exists (`crates/tt-db/src/lib.rs:1436`), returning `Result<Option<EventTimeBounds>, DbError>` where `EventTimeBounds` is the tuple alias at line 249. The invented `parse_event_bound` / `MalformedEventBound` code is gone; Steps 2–3 read the real method and give it the test it never had (it has no caller and no test in the tree), and only `sessions_spanning_multiple_streams` keeps a red→green cycle. Steps are renumbered 1–11.
- **The raw-SQL divergence is named and gated.** `row_to_event` (`lib.rs:3657-3675`) returns `Ok(None)` for both an unreadable timestamp and an unknown type, so the aggregates can count rows the in-memory path drops. Stated in Global Constraints as an accepted, named behaviour change; **measured** in Task 1 Step 1 against a copy of the live database, with an explicit stop-and-surface path if the count is not zero.
- **The single-entry-point invariant is preserved.** Task 2 Step 7 adds `tt_core::allocation::Allocator::new` to `clippy.toml`'s `disallowed-methods` (which today lists only `allocate_time`) and puts an `#[expect]` on `allocate_time`'s own wrapper body; Task 3 Step 10 **keeps** the existing `#[expect]` on `allocate_for_period` and only updates its reason. Task 3's old "leave `clippy.toml` alone" paragraph is gone — the two tasks now agree on which adds the lint entry and which carries the suppression.
- **The 420-line move is no longer described as purely mechanical.** Task 2 Step 4 names both exceptions: two closures to lift (`add_direct` at `allocation.rs:213-224`, `add_delegated` at `227-240`, with call sites listed), and the `continue` at line 429 in the subagent `user_message` guard, which must become a bare `return` placed **before** the `last_event_time = Some(event_time)` assignment moved in from line 631 — replicating that assignment would change what `finish` closes intervals against. A scan of lines 242–632 found that `continue` and no other early exit.
- **Task 3 pins the ordering.** New Step 9 characterizes, against the pre-rewrite code, that a session whose `started` precedes the window still accrues delegated time — the property the prepend at `lib.rs:4665-4668` provides and a global timestamp sort would not.
- **Task 4 is conditional and its signature is fixed.** `finish` is `finish(mut self)` everywhere (the `remove(UNASSIGNED_STREAM_ID)` at `allocation.rs:698-700` mutates `stream_times`). A gate at Step 1 reads Task 3's peak RSS and stops the task if memory is still binding; Step 8 reverts the task if parallelising raised peak RSS, because `union_duration_ms` (`allocation.rs:722-732`) clones each stream's intervals and rayon holds one copy per thread. Modelled on `2026-08-09-read-pool-plan.md`'s Task 1 Step 4.
- **Task 5's guard is structural, not timed.** The `elapsed < 60s` assertion is gone. A `#[cfg(test)]` thread-local counter on `get_events_in_range` lets the test assert `allocate_for_period` never calls it, so the collecting form fails deterministically. The live-corpus measurement stays as acceptance evidence in Step 9.

Every original acceptance gate is intact: `Direct time: 16h 23m` for Jul 20 and `74h 20m` for the week of 2026-07-13 at every task boundary, delegated is a sum and never a union, no event row is ever deleted, zero clippy warnings, `cargo fmt` clean, and `jj` throughout.

**Placeholders.** Two `<RECORD …>` markers in Task 5 Step 6 and one `<AFTER>` / `<TASK3_PEAK_RSS_KB>` pair in Task 4 Step 8 exist because those numbers do not exist until the measurement runs; each step says to fill them before committing. `TASK3_PEAK_RSS_KB` in Task 4 Step 1 is likewise a value the implementer pastes from Task 3 Step 12. No step says "TBD", "similar to Task N", or "add appropriate error handling", and every code step carries real code.

One class of "go and read it" instruction remains deliberate, and it is now narrower than it was: the plan names the exact file and line of every helper it uses, having verified each. Where an earlier revision said "if the helpers are named differently, use the real names", the real names and signatures are now written out — `TestEvent::{tmux_focus, tmux_focus_unassigned, agent_session, agent_tool_use, user_message, window_focus}` and `fn ts(minutes: i64)` at `allocation.rs:980` for `tt-core`, `make_event` (`lib.rs:4700`) and `make_stream(id, Option<&str>)` (`lib.rs:5993`) for `tt-db`. The earlier draft's `make_test_stream`, `make_focus_event`, `make_agent_event`, `make_tool_event` and six-argument `ts` do not exist and have been replaced throughout.

**Type consistency.** `Allocator::new(&'a AllocationConfig, Option<DateTime<Utc>>, &'a HashMap<String, DateTime<Utc>>, &'a HashMap<String, SessionType>) -> Self`, `push<E: AllocatableEvent>(&mut self, &E)` and **`finish(mut self) -> AllocationResult`** are declared in Task 2 and used with exactly those signatures in Tasks 3 and 4 — `mut self` in the Interfaces block, in Step 5, and in Task 4's Interfaces and Step 4 note. `event_time_bounds` is `Result<Option<EventTimeBounds>, DbError>` with `EventTimeBounds = (DateTime<Utc>, DateTime<Utc>)`, destructured as a tuple in Task 1 Step 8 and Task 5 Step 1. `for_each_event_in_range`'s callback is `FnMut(StoredEvent)` (by value) in Task 3 Step 3 and called as `|event| allocator.push(&event)` in Step 10. `sessions_spanning_multiple_streams` returns `Vec<(String, Vec<String>)>` and is iterated as such. `events_in_range_calls() -> usize` is introduced in Task 5 Step 3 and used in Task 5 Step 1's test.

**Risk flagged for the reviewer.** Task 2 moves roughly 420 lines of the codebase's most correctness-critical function, and it is **not** a pure `x` → `self.x` substitution: the `continue`→`return` placement and the two lifted closures are semantic edits inside a mechanical move, and neither is caught by "all tests pass" if it is done plausibly-but-wrongly. It is defended by 55 untouched tests plus the equivalence test, and it is the task most worth reviewing line by line. The `AllocationConfig`/`period_end` borrow split in `Allocator<'a>` is the likeliest place for a borrow-checker fight that tempts someone into restructuring logic; the instruction is to bind locals instead.

**Also verified while amending, and worth a reviewer's attention.** Task 1's `recompute.rs` rewrite strands two imports — `use std::collections::HashMap;` (line 6, used only by the deleted `session_streams` build) and the `Utc` in `use chrono::{Duration, Utc};` (line 9, used only by the deleted `map_or_else(Utc::now, …)` calls) — and an unused import fails CI under `-D warnings`. Both fixes are spelled out in Task 1 Step 8.

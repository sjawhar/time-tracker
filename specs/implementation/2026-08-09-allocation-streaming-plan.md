# Streaming Allocation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make time allocation stream events instead of materializing the whole history, so `tt recompute` stops costing 250 seconds and 8.9 GB to compute a group-by that SQLite does in 0.73 seconds and 6.4 MB.

**Architecture:** `tt_core::allocation::allocate_time` is *already* a single forward pass over timestamp-sorted events (`for event in events`, state in `HashMap`s, no random access). It only holds the whole history because of its **signature** — it takes `&[E]`. This plan (1) deletes a redundant second full load in `recompute::run`, (2) re-exposes the existing pass as an incremental `Allocator` with `push`/`finish`, (3) feeds it directly from a streaming SQLite query, and (4) parallelizes the per-stream interval union that runs at the end. No allocation *rule* changes. Every number the product reports must come out byte-identical.

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
- `tt_core::allocation::allocate_time` is clippy-blocked for outside callers (`clippy.toml` `disallowed-methods`). `tt_db::allocate_for_period` stays the only permitted entry point. Any new public entry point must be added to that policy deliberately, not by accident.
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

`crates/tt-cli/src/commands/recompute.rs:68` calls `db.get_events(None, None)`, materializing all 2,738,805 events. That Vec is used for exactly three things: a warning about sessions split across streams, `earliest`, and `latest`. Then `allocate_for_period` loads every event **again**. This task deletes the first copy.

**Files:**
- Modify: `crates/tt-db/src/lib.rs` (add two methods near `get_events_in_range`, line ~1472)
- Modify: `crates/tt-cli/src/commands/recompute.rs:50-120`
- Test: `crates/tt-db/src/lib.rs` (`mod tests`), `crates/tt-cli/src/commands/recompute.rs` (`mod tests`)

**Interfaces:**
- Produces:
  - `Database::event_time_bounds(&self) -> Result<Option<(DateTime<Utc>, DateTime<Utc>)>, DbError>` — `None` when the table is empty.
  - `Database::sessions_spanning_multiple_streams(&self) -> Result<Vec<(String, Vec<String>)>, DbError>` — session id → its distinct stream ids, only sessions with 2+, ordered by session id, each inner list ordered.

- [ ] **Step 1: Write the failing test for `event_time_bounds`**

Add to `crates/tt-db/src/lib.rs` inside `mod tests`:

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

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p tt-db --lib event_time_bounds_reports_the_first_and_last_event`
Expected: compile error — `no method named 'event_time_bounds' found`.

- [ ] **Step 3: Implement `event_time_bounds`**

Add to `impl Database` in `crates/tt-db/src/lib.rs`, immediately after `get_events_in_range`:

```rust
    /// The timestamps of the first and last event, or `None` when there are none.
    ///
    /// Answers in one aggregate what `recompute` used to answer by materializing every
    /// event and reading `.first()` and `.last()`. Measured on the live 2.2 GB corpus,
    /// that load cost 8.9 GB of RSS to produce two timestamps.
    pub fn event_time_bounds(&self) -> Result<Option<(DateTime<Utc>, DateTime<Utc>)>, DbError> {
        let row: Option<(Option<String>, Option<String>)> = self
            .conn
            .query_row("SELECT MIN(timestamp), MAX(timestamp) FROM events", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?;
        let Some((Some(first), Some(last))) = row else {
            return Ok(None);
        };
        Ok(Some((
            parse_event_bound("MIN(timestamp)", &first)?,
            parse_event_bound("MAX(timestamp)", &last)?,
        )))
    }
```

There is **no** general `parse_timestamp` in this crate, so add a free function and a sibling error type next to the existing `MalformedStreamTimestamp` (`crates/tt-db/src/lib.rs:206`), matching `parse_stream_timestamp` (line 258) exactly in posture — an unreadable timestamp is an **error naming the offending text**, never a substituted `Utc::now()` (see `crates/tt-db/AGENTS.md`, "A timestamp that cannot be read is an error, never a substitution"):

```rust
/// Carries the column and the offending text, because the point of failing here is that a
/// person can go look at the row.
#[derive(Debug, Error)]
#[error("events has an unreadable {column} timestamp {value:?}: {source}")]
struct MalformedEventBound {
    column: &'static str,
    value: String,
    source: chrono::ParseError,
}

fn parse_event_bound(column: &'static str, value: &str) -> Result<DateTime<Utc>, rusqlite::Error> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|source| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(MalformedEventBound {
                    column,
                    value: value.to_string(),
                    source,
                }),
            )
        })
}
```

`rusqlite::Error` converts into `DbError` through the existing `#[from]` on `DbError::Sqlite`, so the `?` in `event_time_bounds` needs nothing further.

- [ ] **Step 4: Run it and confirm it passes**

Run: `cargo test -p tt-db --lib event_time_bounds_reports_the_first_and_last_event`
Expected: `test result: ok. 1 passed`

- [ ] **Step 5: Write the failing test for `sessions_spanning_multiple_streams`**

```rust
    #[test]
    fn sessions_spanning_multiple_streams_reports_only_the_split_ones() {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_test_stream("stream-a", "A")).unwrap();
        db.insert_stream(&make_test_stream("stream-b", "B")).unwrap();

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

- [ ] **Step 6: Run it and confirm it fails**

Run: `cargo test -p tt-db --lib sessions_spanning_multiple_streams_reports_only_the_split_ones`
Expected: compile error — method not found.

- [ ] **Step 7: Implement `sessions_spanning_multiple_streams`**

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

- [ ] **Step 8: Run it and confirm it passes**

Run: `cargo test -p tt-db --lib sessions_spanning_multiple_streams_reports_only_the_split_ones`
Expected: `test result: ok. 1 passed`

- [ ] **Step 9: Rewrite `recompute::run` to use them**

In `crates/tt-cli/src/commands/recompute.rs`, delete the `let events = db.get_events(None, None)...` block, the `session_streams` `HashMap` build, and the `earliest`/`latest` lines derived from `events`. Replace that whole region (currently lines ~66–117) with:

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

Keep the `let config = AllocationConfig::default();` and the `allocate_for_period(db, earliest, latest + Duration::milliseconds(1), None, &config)` call exactly as they are. Remove the now-unused `use std::collections::HashMap;` if nothing else in the file uses it, and remove the `tracing::debug!(event_count = ...)` line that referenced `events`.

- [ ] **Step 10: Verify the whole suite and the lints**

Run: `cargo fmt && cargo clippy --all-targets && cargo test`
Expected: fmt silent, **zero** clippy warnings, all tests pass (baseline is 1,154 passing).

- [ ] **Step 11: Verify the numbers did not move, and measure the win**

```bash
cp ~/.local/share/time-tracker/tt.db /tmp/t1.db
cargo build --release --bin tt
TT_DATABASE_PATH=/tmp/t1.db ./target/release/tt report --start 2026-07-20 --end 2026-07-21 | grep '^Direct time:'
TT_DATABASE_PATH=/tmp/t1.db ./target/release/tt report --start 2026-07-13 --end 2026-07-20 | grep '^Direct time:'
TT_DATABASE_PATH=/tmp/t1.db /usr/bin/time -f 'wall=%es cpu=%P peak_rss=%MKB' ./target/release/tt recompute | tail -3
```

Expected: `Direct time: 16h 23m` and `Direct time: 74h 20m` (unchanged). `peak_rss` roughly halved from 8,928,316 KB, because one of the two full copies is gone. Record the actual figure — the next task is measured against it.

- [ ] **Step 12: Commit**

```bash
jj describe -m "perf(recompute): derive bounds and split-session warning by aggregate

recompute loaded all 2,738,805 events to compute a MIN, a MAX, and a
group-by, then allocate_for_period loaded every event again. Two full
copies of the history, 8.9 GB peak RSS, to produce two timestamps and a
warning. Both are now single aggregate queries."
jj new
```

---

### Task 2: Expose the existing pass as an incremental `Allocator`

`allocate_time` (`crates/tt-core/src/allocation.rs:195`) is already a single forward pass: the event loop is `for event in events` at line 242 and runs to roughly line 660; finalization (`final_attributions`, the per-stream union, the `AllocationResult` literal) runs from line 662 to about line 720. Nothing indexes or re-reads the slice. This task is a **pure refactor** that turns that pass into a struct you can feed one event at a time. No rule changes, no test edits.

**Files:**
- Modify: `crates/tt-core/src/allocation.rs:195-720`
- Test: `crates/tt-core/src/allocation.rs` (`mod tests`) — existing 55 tests must pass untouched, plus one new equivalence test.

**Interfaces:**
- Produces:
  - `pub struct Allocator<'a>` holding every `let mut` currently local to `allocate_time` (`focus_state`, `window_focus_state`, `browser_focus_state`, `tmux_focus_stream_id`, `agent_sessions`, `stream_times`, `activity_intervals`, `last_event_time`) plus borrows of `config: &'a AllocationConfig`, `session_end_times: &'a HashMap<String, DateTime<Utc>>`, `session_types: &'a HashMap<String, SessionType>`, and `period_end: Option<DateTime<Utc>>`.
  - `Allocator::new(config: &'a AllocationConfig, period_end: Option<DateTime<Utc>>, session_end_times: &'a HashMap<String, DateTime<Utc>>, session_types: &'a HashMap<String, SessionType>) -> Self`
  - `Allocator::push<E: AllocatableEvent>(&mut self, event: &E)`
  - `Allocator::finish(self) -> AllocationResult`
- Consumes: nothing from Task 1.

- [ ] **Step 1: Write the equivalence test first**

Add to `mod tests` in `crates/tt-core/src/allocation.rs`. It pins that feeding events one at a time is identical to passing the slice — the property every later task depends on:

```rust
    #[test]
    fn pushing_events_one_at_a_time_equals_allocating_the_whole_slice() {
        // Given: a corpus exercising focus, agent work, and an unassigned stretch.
        let events = vec![
            make_focus_event("f1", ts(2026, 3, 1, 9, 0, 0), Some("stream-a")),
            make_agent_event("s1", ts(2026, 3, 1, 9, 1, 0), "session-1", "started", Some("stream-a")),
            make_tool_event("t1", ts(2026, 3, 1, 9, 2, 0), "session-1", Some("stream-a")),
            make_tool_event("t2", ts(2026, 3, 1, 9, 30, 0), "session-1", Some("stream-a")),
            make_focus_event("f2", ts(2026, 3, 1, 10, 0, 0), None),
            make_agent_event("s2", ts(2026, 3, 1, 10, 1, 0), "session-1", "ended", Some("stream-a")),
        ];
        let config = AllocationConfig::default();
        let end_times = HashMap::new();
        let types = HashMap::new();

        // When: the slice path and the incremental path both run.
        #[expect(
            clippy::disallowed_methods,
            reason = "this test exists to compare the two entry points directly"
        )]
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

Use whatever fixture builders already exist in that `mod tests` for focus, agent-session, and tool-use events — read the existing tests around `allocation.rs:1173` (`test_concurrent_agents`) and reuse their helpers rather than adding new ones. If the helpers are named differently from `make_focus_event` / `make_agent_event` / `make_tool_event` / `ts`, use the real names; do not add duplicates.

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

Add `pub fn push<E: AllocatableEvent>(&mut self, event: &E)` to that `impl`, and move the **body of the `for event in events` loop** (`allocation.rs` line ~243 through the loop's closing brace, ~line 660) into it verbatim. Then make it compile by mechanical substitution only:

- Every bare local becomes a field: `focus_state` → `self.focus_state`, `agent_sessions` → `self.agent_sessions`, `stream_times` → `self.stream_times`, `activity_intervals` → `self.activity_intervals`, `last_event_time` → `self.last_event_time`, `tmux_focus_stream_id` → `self.tmux_focus_stream_id`, `window_focus_state` → `self.window_focus_state`, `browser_focus_state` → `self.browser_focus_state`, `config` → `self.config`, `period_end` → `self.period_end`, `session_end_times` → `self.session_end_times`, `session_types` → `self.session_types`.
- The `add_direct` closure (line ~213) closes over nothing but its arguments, so lift it to a **private associated function** rather than a field:

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
```

Call sites become `Self::add_direct(stream_id, start, end, &mut self.activity_intervals, &mut self.stream_times)`. Do the same for any other closure defined between lines 202 and 242 — check for them; there may be more than one.

**Change no conditions, no arithmetic, no ordering, and no `EventType` arm.** If borrow-checker errors appear because a statement holds `&self.x` while mutating `self.y`, bind locals first (`let config = self.config;`) rather than restructuring logic.

- [ ] **Step 5: Move finalization into `finish`**

Add `pub fn finish(self) -> AllocationResult` and move `allocation.rs` lines ~662 to ~720 into it verbatim — the `final_attributions` block, the `for (stream_id, first_tool, session_end)` loop, the per-stream union, and the `AllocationResult { ... }` literal. Apply the same `self.` substitution. Nothing else moves.

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

Keep its signature, its doc comment, and its `clippy.toml` `disallowed-methods` entry exactly as they are. Existing callers and every existing test keep working through it unchanged.

- [ ] **Step 7: Run the full allocation suite unmodified**

Run: `cargo test -p tt-core --lib allocation`
Expected: all 56 tests pass (the 55 existing plus the new equivalence test), **with no edits to any existing test**. Confirm by name that these three passed:
- `test_total_direct_time_never_exceeds_wall_clock`
- `test_parallel_agent_sessions_sum_delegated_beyond_wall_clock`
- `a_sessions_delegated_span_never_outruns_the_end_event_that_closed_it`

If any existing test required a change to pass, revert and find what the move altered.

- [ ] **Step 8: Full gates**

Run: `cargo fmt && cargo clippy --all-targets && cargo test`
Expected: fmt silent, zero clippy warnings, 1,155 tests passing.

- [ ] **Step 9: Verify the reported numbers are still identical**

```bash
cp ~/.local/share/time-tracker/tt.db /tmp/t2.db
cargo build --release --bin tt
TT_DATABASE_PATH=/tmp/t2.db ./target/release/tt report --start 2026-07-20 --end 2026-07-21 | grep '^Direct time:'
TT_DATABASE_PATH=/tmp/t2.db ./target/release/tt report --start 2026-07-13 --end 2026-07-20 | grep '^Direct time:'
```

Expected: `Direct time: 16h 23m`, `Direct time: 74h 20m`. This task is a pure refactor; anything else means the move changed behavior.

- [ ] **Step 10: Commit**

```bash
jj describe -m "refactor(allocation): expose the pass as an incremental Allocator

allocate_time was already a single forward walk with its state in maps; it
held the whole history only because it took a slice. Allocator::push and
::finish are that same walk, fed one event at a time. allocate_time is now
a wrapper over it, so every caller and all 55 existing tests are untouched."
jj new
```

---

### Task 3: Stream events from SQLite into the `Allocator`

`allocate_for_period` (`crates/tt-db/src/lib.rs:4633`) calls `get_events_in_range`, materializing every event in the window — the second of the two full copies. This task feeds the `Allocator` row by row instead, and replaces the in-memory pre-pass that finds sessions with tool use but no `started` event with a query.

**Files:**
- Modify: `crates/tt-db/src/lib.rs` (add two methods near line 1472; rewrite `allocate_for_period` at 4633)
- Test: `crates/tt-db/src/lib.rs` (`mod tests`)

**Interfaces:**
- Consumes: `tt_core::allocation::Allocator::{new, push, finish}` from Task 2.
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

- [ ] **Step 9: Rewrite `allocate_for_period` to stream**

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

The `#[expect(clippy::disallowed_methods, ...)]` currently on this function's call to `allocate_time` is no longer needed if the call is gone — remove it, and add `tt_core::allocation::Allocator::push` to `clippy.toml`'s `disallowed-methods` **only if** the team wants the same gate on the new entry point; default is to leave `clippy.toml` alone, since `allocate_for_period` remains the single public path.

- [ ] **Step 10: Full gates plus every allocation test**

Run: `cargo fmt && cargo clippy --all-targets && cargo test`
Expected: fmt silent, zero clippy warnings, all tests pass. The tests in `crates/tt-cli/tests/` that exercise reports and recompute end-to-end are the real check here — they compare full outputs.

- [ ] **Step 11: Verify numbers and measure the win**

```bash
cp ~/.local/share/time-tracker/tt.db /tmp/t3.db
cargo build --release --bin tt
TT_DATABASE_PATH=/tmp/t3.db ./target/release/tt report --start 2026-07-20 --end 2026-07-21 | grep '^Direct time:'
TT_DATABASE_PATH=/tmp/t3.db ./target/release/tt report --start 2026-07-13 --end 2026-07-20 | grep '^Direct time:'
TT_DATABASE_PATH=/tmp/t3.db /usr/bin/time -f 'wall=%es cpu=%P peak_rss=%MKB' ./target/release/tt recompute | tail -3
```

Expected: `Direct time: 16h 23m` and `Direct time: 74h 20m` still. `peak_rss` now dominated by recorded intervals and per-stream state rather than events — target well under 1 GB against the 8.9 GB baseline. Record the figure.

- [ ] **Step 12: Commit**

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
- Consumes: `Allocator::finish` from Task 2.
- Produces: no signature change. `finish` keeps returning `AllocationResult`.

- [ ] **Step 1: Write the failing test — determinism under parallelism**

Parallel iteration must not reorder or perturb results. This test pins that many streams come back with identical, deterministically ordered totals:

```rust
    #[test]
    fn many_streams_union_identically_however_the_work_is_scheduled() {
        // Given: enough streams that rayon will actually split the work.
        let mut events = Vec::new();
        for stream in 0..64 {
            let stream_id = format!("stream-{stream:02}");
            for minute in 0..8 {
                events.push(make_focus_event(
                    &format!("f-{stream:02}-{minute}"),
                    ts(2026, 3, 1, 9, minute * 2, 0),
                    Some(&stream_id),
                ));
            }
        }
        let config = AllocationConfig::default();
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

- [ ] **Step 2: Run it and confirm it passes already**

Run: `cargo test -p tt-core --lib many_streams_union_identically_however_the_work_is_scheduled`
Expected: PASS. This one is a guard written *before* the optimization so it can catch a regression the optimization might introduce — it is not expected to fail first. Note that explicitly in the commit message.

- [ ] **Step 3: Parallelize the union inside `finish`**

In `Allocator::finish`, the finalization currently reads (`crates/tt-core/src/allocation.rs:695-718`):

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

**Three things must not move.** The `stream_times.remove(UNASSIGNED_STREAM_ID)` **has to stay before** this map: `UNASSIGNED_STREAM_ID` (`allocation.rs:21`) is the synthetic `"(unassigned)"` bucket that unattributed events accrue into, and the doc contract at line 179 is that the sentinel is removed from `stream_times` before returning. Parallelizing before removing it would publish `(unassigned)` as a real stream row and double-count it in every report. `total_tracked_ms` stays a union of `activity_intervals`. And `time_delegated_ms` stays `allocation.delegated_ms` — **a sum across concurrent sessions, never a union**; `test_parallel_agent_sessions_sum_delegated_beyond_wall_clock` (line 2402) fails if that changes.

If the existing code did not sort `stream_times`, adding the sort is still correct and required — parallel collection makes the previous `HashMap` iteration order even less defined than it was. Check whether any insta snapshot depends on that order; if one does, run `cargo insta review` and accept only ordering changes.

- [ ] **Step 4: Run the allocation suite and the determinism guard**

Run: `cargo test -p tt-core --lib allocation`
Expected: all tests pass, including `many_streams_union_identically_however_the_work_is_scheduled`, `test_total_direct_time_never_exceeds_wall_clock`, and `test_parallel_agent_sessions_sum_delegated_beyond_wall_clock`.

- [ ] **Step 5: Full gates**

Run: `cargo fmt && cargo clippy --all-targets && cargo test`
Expected: fmt silent, zero clippy warnings, all tests pass. If insta snapshots changed, `cargo insta review` and confirm every diff is ordering only.

- [ ] **Step 6: Verify numbers and measure CPU utilization**

```bash
cp ~/.local/share/time-tracker/tt.db /tmp/t4.db
cargo build --release --bin tt
TT_DATABASE_PATH=/tmp/t4.db ./target/release/tt report --start 2026-07-20 --end 2026-07-21 | grep '^Direct time:'
TT_DATABASE_PATH=/tmp/t4.db ./target/release/tt report --start 2026-07-13 --end 2026-07-20 | grep '^Direct time:'
TT_DATABASE_PATH=/tmp/t4.db /usr/bin/time -f 'wall=%es cpu=%P peak_rss=%MKB' ./target/release/tt recompute | tail -3
```

Expected: same two direct-time figures. `cpu=` now above 100% (it was 99% = one core). Record wall, cpu, and peak_rss.

- [ ] **Step 7: Commit**

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

A performance fix with no guard rots. This task adds a test that fails if allocation ever re-materializes the history, and writes the measured numbers into `AGENTS.md` so the next person does not have to rediscover them.

**Files:**
- Test: `crates/tt-db/src/lib.rs` (`mod tests`)
- Modify: `AGENTS.md`
- Modify: `crates/tt-db/AGENTS.md`

**Interfaces:**
- Consumes: everything from Tasks 1–4.

- [ ] **Step 1: Write a guard that catches re-materialization**

Memory ceilings are not directly assertable in a unit test, so assert the property that actually matters — that allocation over a corpus far larger than any test fixture completes quickly and correctly. A re-introduced `Vec` of every event shows up as a large multiple here. The margin is deliberately generous so this is not a flaky timing test; it exists to catch an order-of-magnitude regression, exactly like `a_chunks_calls_all_run_at_the_same_time` in `classify_auto.rs`.

```rust
    #[test]
    fn allocating_a_large_corpus_does_not_scale_with_history_length() {
        let db = Database::open_in_memory().unwrap();
        db.insert_stream(&make_test_stream("stream-a", "A")).unwrap();

        // 200k focus events across one stream: far more than any fixture, few enough to
        // stay a unit test.
        let base = Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap();
        let mut batch = Vec::with_capacity(10_000);
        for index in 0..200_000_i64 {
            let mut event = make_event(
                &format!("f{index}"),
                base + chrono::Duration::seconds(index * 10),
                EventType::TmuxPaneFocus,
            );
            event.stream_id = Some("stream-a".to_string());
            batch.push(event);
            if batch.len() == 10_000 {
                db.insert_events(&batch).unwrap();
                batch.clear();
            }
        }
        if !batch.is_empty() {
            db.insert_events(&batch).unwrap();
        }

        let config = tt_core::AllocationConfig::default();
        let (first, last) = db.event_time_bounds().unwrap().unwrap();
        let started = std::time::Instant::now();
        let result = allocate_for_period(
            &db,
            first,
            last + chrono::Duration::milliseconds(1),
            None,
            &config,
        )
        .unwrap();
        let elapsed = started.elapsed();

        // Correct: one stream, and direct time is a union so it cannot exceed the span.
        assert_eq!(result.stream_times.len(), 1);
        let span_ms = (last - first).num_milliseconds();
        assert!(
            result.stream_times[0].time_direct_ms <= span_ms,
            "direct {} exceeded span {}",
            result.stream_times[0].time_direct_ms,
            span_ms
        );

        // Fast: streaming this is seconds of work. A re-introduced full materialization
        // of every event blows past this by an order of magnitude.
        assert!(
            elapsed < std::time::Duration::from_secs(60),
            "allocation took {elapsed:?} for 200k events; something is materializing the history"
        );
    }
```

- [ ] **Step 2: Run it and confirm it passes, and time it**

Run: `cargo test -p tt-db --lib allocating_a_large_corpus_does_not_scale_with_history_length -- --nocapture`
Expected: PASS. Note the actual elapsed time. If it is anywhere near 60s, the streaming is not working — investigate before continuing rather than raising the bound.

- [ ] **Step 3: Confirm the guard has teeth**

Temporarily revert `allocate_for_period` to the collecting form (`let events = db.get_events_in_range(start, inclusive_end)?;` then `allocate_time(&events, ...)`), run the test, and record what happens — it should be dramatically slower or fail. Then restore the streaming form. Do not commit the reverted state.

Run: `cargo test -p tt-db --lib allocating_a_large_corpus_does_not_scale_with_history_length -- --nocapture`
Expected (reverted): markedly slower elapsed, or failure. Expected (restored): PASS, fast.

- [ ] **Step 4: Record the measurement in the root `AGENTS.md`**

Add an entry to the **Anti-Patterns** list in `AGENTS.md`, in the voice of the surrounding entries — dated measurements, and the rule stated as a prohibition:

```markdown
- **Allocation streams the event history and must never materialize it again**: `allocate_time` has always been a single forward walk over timestamp-sorted events with its state in maps, but it took a `&[E]`, and `tt recompute` fed it by loading every row. Measured on the live 2.2 GB corpus (2,738,805 events): **250.48 s wall, 8.9 GB peak RSS, 99% CPU — one of 24 cores** — to produce a per-stream group-by that `sqlite3` answers in **0.73 s and 6.4 MB**. Two full copies were resident, because `recompute::run` loaded the whole history to derive a `MIN`, a `MAX`, and a split-session warning, and then `allocate_for_period` loaded the window again. Both are gone: the bounds and the warning are aggregates (`event_time_bounds`, `sessions_spanning_multiple_streams`), the window streams through `for_each_event_in_range` into `tt_core::allocation::Allocator`, and the missing-start pre-pass is a query (`sessions_with_tool_use_but_no_start`). After: **<RECORD wall/cpu/peak_rss FROM TASK 4 STEP 6>**. `allocating_a_large_corpus_does_not_scale_with_history_length` is the guard, and it was verified to fail against the collecting form. **The slowness had become load-bearing**, which is the part worth remembering: `tt sync` was made 3.6 min → 18.6 s by *deleting* its recompute call, that deletion is now a rule with its own regression test, and `tt streams` labels its totals with their age rather than refreshing them. A 250-second function shaped three other designs before anyone treated it as a defect. Adding a `Vec` of events back into this path re-imposes all of it.
```

Replace the `<RECORD ...>` marker with the real figures before committing. Also update the existing **`tt sync` must not recompute** entry to note that recomputation is no longer minutes of CPU and gigabytes of RSS — leaving that entry stating the old cost would make two parts of this file disagree. Do **not** delete that rule or re-add `recompute::run` to `sync::run`: the ordering guarantee it protects is separate from its cost, and `tt-cli/tests/sync_no_recompute.rs` guards it.

- [ ] **Step 5: Record the new methods in `crates/tt-db/AGENTS.md`**

Add rows to the **Events** table of the Method Reference:

```markdown
| `event_time_bounds` | `MIN`/`MAX` of `events.timestamp`, or `None` when empty. What `recompute` used to derive by materializing all 2.7M events |
| `for_each_event_in_range` | Streaming counterpart of `get_events_in_range`: visits each row in ascending timestamp order without collecting. The allocation pass walks forward only, so it never needed the `Vec` |
| `sessions_spanning_multiple_streams` | Sessions whose events point at more than one stream, with those streams. A data-integrity report, ordered for stable output |
| `sessions_with_tool_use_but_no_start` | Sessions with `agent_tool_use` in a window but no `agent_session`/`started` in it — the query form of a pre-pass that used to run over a materialized `Vec` |
```

And note under the **Allocation entry point** section that `allocate_for_period` now streams into `tt_core::allocation::Allocator` rather than collecting, and that its `end` is still exclusive.

- [ ] **Step 6: Full gates**

Run: `cargo fmt --check && cargo clippy --all-targets && cargo test`
Expected: fmt silent, zero clippy warnings, all tests pass.

- [ ] **Step 7: Final end-to-end verification against the live corpus**

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

- [ ] **Step 8: Commit**

```bash
jj describe -m "test(allocation): guard against re-materializing the event history

200k-event corpus through allocate_for_period, asserting correctness and an
order-of-magnitude time bound. Verified to fail against the collecting
form. AGENTS.md records the measured before/after and that the old cost had
become load-bearing: tt sync was sped up by deleting its recompute call
rather than by fixing recompute."
jj new
```

---

## Self-Review

**Spec coverage.** The critique was: allocation is single-threaded, materializes everything, and 340× slower than the equivalent database aggregate, and the codebase has been designed around that rather than fixing it. Mapped: redundant full load → Task 1; materialization of the window → Tasks 2 and 3; single-core union → Task 4; regression guard and the load-bearing-slowness note → Task 5. The async-classifier rearchitecture and the read pool are deliberately **not** here — separate subsystems, separate plans, per the skill's scope check.

**Placeholders.** Two intentional `<RECORD ...>` markers in Task 5 Step 4 exist because the numbers do not exist until Task 4 runs; the step says to fill them before committing. Three places instruct the implementer to match existing names they must read (`parse_timestamp`, the interval-union helper near `allocation.rs:727`, the fixture builders in `mod tests`) rather than inventing them — that is deliberate, because guessing a name would be worse than looking, and every one names an exact file and line to look at.

**Type consistency.** `Allocator::{new, push, finish}` is defined in Task 2 and consumed with the same signature in Tasks 3 and 4. `event_time_bounds` returns `Option<(DateTime<Utc>, DateTime<Utc>)>` in Task 1 and is destructured that way in Task 1 Step 9 and Task 5 Step 1. `for_each_event_in_range`'s callback is `FnMut(StoredEvent)` (by value) in Task 3 Step 3 and called as `|event| allocator.push(&event)` in Step 9. `sessions_spanning_multiple_streams` returns `Vec<(String, Vec<String>)>` and is iterated as such.

**Risk flagged for the reviewer.** Task 2 moves roughly 420 lines of the codebase's most correctness-critical function. It is mechanical (`x` → `self.x`) and defended by 55 untouched tests plus a new equivalence test, but it is the task most worth reviewing line by line, and the one where "all tests pass" is least sufficient on its own. The `AllocationConfig`/`period_end` borrow split in `Allocator<'a>` is the likeliest place for a borrow-checker fight that tempts someone into restructuring logic; the instruction is to bind locals instead.

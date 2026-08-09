# Classifier Read Pool Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop every concurrent classification worker serializing its session fetches on a single `Mutex<Database>`, by giving the classifier a small pool of read connections — **but only if measurement shows the contention is real.**

**Architecture:** `DbSessionDetail` (`crates/tt-cli/src/commands/classify_auto/session_detail.rs:45`) holds `db: Mutex<Database>`. Every worker in a `CLASSIFY_CONCURRENCY`-wide chunk that fetches session detail mid-classification takes that one lock, so fetches are serial across the whole chunk. SQLite in WAL mode supports unlimited concurrent readers; we are not using that. This plan replaces the single mutex with a fixed-size pool of read-only connections handed out and returned through a channel.

**Tech Stack:** Rust (edition 2021), rusqlite 0.34, `std::sync::mpsc` (no new dependencies).

## Task 0 is a kill switch

**This plan begins by measuring, and is designed to be abandoned at Task 1 Step 4 if the number is small.** The model call is 15–90 seconds; a session fetch is a handful of indexed SQLite reads. If lock wait is a rounding error against that, building a pool is complexity for nothing, and this repo has a standing habit of reverting changes that measure badly (see `CLASSIFY_CONCURRENCY` in `crates/tt-cli/src/commands/classify_auto.rs`, where 16 was tried and reverted for +18% at the cost of rate limiting). Do not treat reaching Task 2 as the success condition.

## Global Constraints

- **No behavior change whatsoever.** This is purely how reads are served. `tt report` for `2026-07-20` must print `Direct time: 16h 23m` and `2026-07-13..2026-07-20` must print `Direct time: 74h 20m` before and after.
- **Reads only.** The classifier's *writes* stay exactly where they are: on the one `Database` the `Resolver` holds, applied serially. That serialization is a correctness guarantee — `stream_named` is a non-transactional find-and-insert, and two concurrent copies re-create the duplicate-stream failure that cost 55 renames, 5 merges and 9 dissolves. **Nothing in this plan may hand a write path more than one connection.**
- Every pooled connection is opened with `Database::open`, which applies `busy_timeout(30s)`, `synchronous = NORMAL`, and the schema check. Do not open raw `rusqlite::Connection`s.
- Pool size is bounded and never grows on demand: an unbounded pool is a file-descriptor leak with extra steps.
- `unsafe_code` denied. Zero clippy warnings (`-D warnings` in CI). `cargo fmt` clean.
- No new dependencies.
- **jj, not git.** `jj describe -m "..."` then `jj new`. Never `git`. Never backticks or `$(...)` inside a double-quoted `-m` message.

## File Structure

| File | Responsibility after this plan |
|---|---|
| `crates/tt-cli/src/commands/classify_auto/session_detail.rs` (modify) | Serves `SessionDetail` reads from a bounded pool of connections instead of one behind a mutex. Public surface (`session_tools`, `DbSessionDetail::open`, `from_database`) unchanged. |

Nothing else changes. `SessionTools`, the `SessionDetail` trait in `tt-llm`, and every caller stay as they are.

---

### Task 1: Measure the contention before building anything

**Files:**
- Test: `crates/tt-cli/src/commands/classify_auto/session_detail/tests.rs`

**Interfaces:**
- Produces: a number, and a decision.

- [ ] **Step 1: Write a test that measures lock serialization directly**

```rust
    #[test]
    fn concurrent_fetches_report_their_serialization() {
        use std::sync::Arc;
        use std::time::Instant;

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("tt.db");
        {
            let db = tt_db::Database::open(&path).unwrap();
            // One session with enough messages that a fetch is not trivially empty.
            insert_session_with_messages(&db, "session-1", 200);
        }
        let detail = Arc::new(DbSessionDetail::open(&path).unwrap());

        // Serial baseline.
        let started = Instant::now();
        for _ in 0..64 {
            detail.overview("session-1").unwrap();
        }
        let serial = started.elapsed();

        // Eight workers, eight fetches each: the shape a classification chunk produces.
        let started = Instant::now();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let detail = Arc::clone(&detail);
                scope.spawn(move || {
                    for _ in 0..8 {
                        detail.overview("session-1").unwrap();
                    }
                });
            }
        });
        let concurrent = started.elapsed();

        println!("SERIAL   64 fetches: {serial:?}");
        println!("CONCURRENT 8x8      : {concurrent:?}");
        println!("ratio (1.0 = fully serialized): {:.2}", concurrent.as_secs_f64() / serial.as_secs_f64());
    }
```

Write `insert_session_with_messages` using the fixture helpers already in that test module; if none exists for agent sessions, use `tt_db::Database::upsert_agent_session` with a `user_prompts` vector of the requested length. Read the existing tests in that file and match their helpers rather than inventing new ones.

- [ ] **Step 2: Run it and record the numbers**

Run: `cargo test -p tt-cli --lib concurrent_fetches_report_their_serialization -- --nocapture`
Expected: three printed lines. Record all three.

- [ ] **Step 3: Measure the real thing — how much of a live pass is spent fetching**

```bash
cd /home/sami/Code/time-tracker/default
DB=~/.local/share/time-tracker/tt.db
# How many classifications actually fetch at all? A fetch is optional; a payload that
# already names the work spends nothing.
journalctl --user -u tt-serve.service --since "2 hours ago" --no-pager -o cat \
  | sed 's/\x1b\[[0-9;]*m//g' | grep -oE "Auto-classify: .*" | tail -5
```

There is no direct fetch counter today. If one is needed to answer this, add a `fetches` field to `AutoClassifyOutcome` and its summary line **as its own commit** before continuing — an unmeasurable question is not a reason to guess.

- [ ] **Step 4: DECIDE — and stop here if the answer is no**

Continue to Task 2 **only if** the concurrent/serial ratio from Step 2 is meaningfully above ~1.0 (workers genuinely queueing) **and** fetches are frequent enough in a real pass to matter against a 15–90s model call.

If the ratio is near 1.0 but total fetch time is microseconds against minutes of model latency, **stop, and record why**: add a short entry to `crates/tt-cli/src/commands/classify_auto/session_detail.rs`'s module doc naming the measured numbers and stating that the single mutex is deliberate because fetch time is negligible beside the model call. Then commit that and close the plan. A measured "no" is a successful outcome of this plan.

```bash
jj describe -m "docs(session-detail): record why one read connection is enough

Measured: <RATIO> concurrent/serial over 64 fetches, and fetch time is
<FIGURE> against a 15-90s model call. A read pool would be complexity for
a rounding error."
jj new
```

---

### Task 2: Replace the single mutex with a bounded connection pool

Only reachable if Task 1 Step 4 said yes.

**Files:**
- Modify: `crates/tt-cli/src/commands/classify_auto/session_detail.rs`
- Test: `crates/tt-cli/src/commands/classify_auto/session_detail/tests.rs`

**Interfaces:**
- Consumes: nothing from Task 1 beyond its decision.
- Produces: no public API change. `session_tools(path) -> Result<Arc<SessionTools>, tt_db::DbError>`, `DbSessionDetail::open(path)`, and `DbSessionDetail::from_database(db)` keep their exact signatures.

- [ ] **Step 1: Write the failing test — concurrent fetches stop queueing**

```rust
    #[test]
    fn a_pool_serves_concurrent_fetches_without_queueing_on_one_connection() {
        use std::sync::Arc;

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("tt.db");
        {
            let db = tt_db::Database::open(&path).unwrap();
            insert_session_with_messages(&db, "session-1", 200);
        }
        let detail = Arc::new(DbSessionDetail::open(&path).unwrap());

        // Every worker must get an answer, and none may observe a poisoned or exhausted
        // pool. Correctness first; the timing claim is Task 1's job, not a unit test's.
        std::thread::scope(|scope| {
            for _ in 0..READ_POOL_SIZE * 4 {
                let detail = Arc::clone(&detail);
                scope.spawn(move || {
                    let overview = detail.overview("session-1").unwrap();
                    assert_eq!(overview.session_id, "session-1");
                });
            }
        });

        // And the pool is intact afterwards: a connection borrowed is a connection returned.
        assert_eq!(detail.available_connections(), READ_POOL_SIZE);
    }
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p tt-cli --lib a_pool_serves_concurrent_fetches_without_queueing_on_one_connection`
Expected: compile error — `READ_POOL_SIZE` and `available_connections` do not exist.

- [ ] **Step 3: Implement the pool**

Replace the `db: Mutex<Database>` field and its uses in `crates/tt-cli/src/commands/classify_auto/session_detail.rs`:

```rust
use std::sync::{Condvar, Mutex};

/// Read connections the classifier may use at once.
///
/// Matches `CLASSIFY_CONCURRENCY` (8) so a full chunk of workers can each hold one
/// without waiting. SQLite in WAL mode allows unlimited concurrent readers, so the only
/// cost of a connection is a file descriptor and its page cache; the only reason to
/// bound it at all is that an unbounded pool is a descriptor leak with extra steps.
///
/// This is a **read** pool. The classifier's writes stay on the single connection the
/// resolver holds, applied serially, because `stream_named` is a non-transactional
/// find-and-insert and two concurrent copies re-create the duplicate-stream failure that
/// took 55 renames, 5 merges and 9 dissolves to undo.
const READ_POOL_SIZE: usize = 8;

/// Session reads for a classifier, served from a bounded pool of connections.
pub struct DbSessionDetail {
    idle: Mutex<Vec<Database>>,
    returned: Condvar,
}

impl DbSessionDetail {
    /// Opens `READ_POOL_SIZE` connections dedicated to answering the classifier's fetches.
    ///
    /// # Errors
    /// When any connection cannot be opened.
    pub fn open(path: &Path) -> Result<Self, tt_db::DbError> {
        let mut idle = Vec::with_capacity(READ_POOL_SIZE);
        for _ in 0..READ_POOL_SIZE {
            idle.push(Database::open(path)?);
        }
        Ok(Self {
            idle: Mutex::new(idle),
            returned: Condvar::new(),
        })
    }

    /// Wraps one already-open database as a pool of one.
    ///
    /// The in-memory test path: `Database::open_in_memory` gives each connection its own
    /// private database, so a pool of them would not see each other's rows.
    pub fn from_database(db: Database) -> Self {
        Self {
            idle: Mutex::new(vec![db]),
            returned: Condvar::new(),
        }
    }

    /// How many connections are currently not lent out. Test observability.
    #[cfg(test)]
    fn available_connections(&self) -> usize {
        self.idle.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    /// Runs `f` against a borrowed connection, returning it to the pool afterwards.
    ///
    /// The connection is returned even when `f` fails, because a read that errored did
    /// not damage the connection, and losing one per error would drain the pool.
    fn with_connection<T>(
        &self,
        f: impl FnOnce(&Database) -> Result<T, SessionDetailError>,
    ) -> Result<T, SessionDetailError> {
        let db = {
            let mut idle = self.idle.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if let Some(db) = idle.pop() {
                    break db;
                }
                idle = self
                    .returned
                    .wait(idle)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        let result = f(&db);
        self.idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(db);
        self.returned.notify_one();
        result
    }
}
```

Add `use std::sync::PoisonError;`. Then rewrite each `SessionDetail` method body (`session`, `overview`, `messages`, and any others in the `impl SessionDetail for DbSessionDetail` block) to call `self.with_connection(|db| ...)` with the body that currently runs after `self.db.lock()`. **Change no query and no error mapping** — only where the connection comes from.

`from_database` is deliberately a pool of one and can therefore no longer be `const fn`; drop the `const`. Check whether any caller relies on it being `const` (it is used by tests and takes an already-open `Database`), and if the compiler complains, that is the fix.

- [ ] **Step 4: Run it and confirm it passes**

Run: `cargo test -p tt-cli --lib a_pool_serves_concurrent_fetches_without_queueing_on_one_connection`
Expected: `test result: ok. 1 passed`

- [ ] **Step 5: Re-run the Task 1 measurement to prove the pool changed it**

Run: `cargo test -p tt-cli --lib concurrent_fetches_report_their_serialization -- --nocapture`
Expected: the concurrent/serial ratio from Task 1 Step 2 has dropped. Record before and after. **If it did not drop, the pool is not doing anything — revert rather than ship it.**

- [ ] **Step 6: Full gates**

Run: `cargo fmt && cargo clippy --all-targets && cargo test`
Expected: fmt silent, zero clippy warnings, all tests pass. The existing `session_detail/tests.rs` suite must pass unmodified — `from_database` changing from one connection to a pool of one must be invisible to it.

- [ ] **Step 7: Verify the daemon and the numbers**

```bash
cp ~/.local/share/time-tracker/tt.db /tmp/pool.db
cargo build --release --bin tt --bin tt-serve
TT_DATABASE_PATH=/tmp/pool.db ./target/release/tt report --start 2026-07-20 --end 2026-07-21 | grep '^Direct time:'
TT_DATABASE_PATH=/tmp/pool.db ./target/release/tt report --start 2026-07-13 --end 2026-07-20 | grep '^Direct time:'
systemctl --user stop tt-serve.service && sleep 3
cp target/release/tt-serve ~/.local/bin/tt-serve
systemctl --user start tt-serve.service && sleep 60
systemctl --user is-active tt-serve.service
systemctl --user show tt-serve.service -p NRestarts --value
journalctl --user -u tt-serve.service --since "2 min ago" --no-pager -o cat | grep -ci "ingest failed"
ls -l /proc/$(systemctl --user show tt-serve.service -p MainPID --value)/fd | grep -c "tt.db"
```

Expected: `16h 23m`, `74h 20m`, `active`, `0` restarts, `0` ingest failures. The descriptor count rises by roughly `READ_POOL_SIZE` — confirm it is bounded and does not climb over the following minutes.

- [ ] **Step 8: Commit**

```bash
jj describe -m "perf(session-detail): serve classifier reads from a connection pool

Every worker in a concurrent chunk took one Mutex<Database> to fetch
session detail, so fetches were serial across the whole chunk while WAL
allows unlimited concurrent readers. Bounded pool of READ_POOL_SIZE
connections, borrowed and returned. Writes are untouched and stay serial
on the resolver's single connection."
jj new
```

---

## Self-Review

**Spec coverage.** The critique was that `Mutex<Database>` serializes all workers' session fetches while WAL supports concurrent readers. Task 1 measures it, Task 2 fixes it, and Task 1 Step 4 is an explicit exit if the measurement says it does not matter.

**Placeholders.** Two `<RATIO>`/`<FIGURE>` markers in Task 1 Step 4's commit message, which cannot exist before Step 2 runs; the step says to fill them. One instruction to match existing fixture helpers rather than invent them, naming the file to read.

**Type consistency.** `READ_POOL_SIZE` and `available_connections` are introduced in Task 2 Step 3 and used in Task 2 Step 1's test. `with_connection` takes `impl FnOnce(&Database) -> Result<T, SessionDetailError>` and every rewritten method body returns that type. `from_database` keeps its signature but loses `const`, which Step 3 calls out.

**Risk flagged.** The `Condvar` wait loop is the only subtle part: it must re-check the pool after waking, which the `loop`/`pop`/`wait` shape does. A `if let ... else wait` would be a spurious-wakeup bug. Poisoning is handled by recovering the inner value rather than panicking, because a panicking reader does not corrupt a connection.

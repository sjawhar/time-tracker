# Async Classifier Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the sync/async impedance mismatch in the classifier — an async HTTP stack blocked into a sync trait, then re-parallelized with OS threads — so concurrency is futures rather than thread stacks and the stack stops fighting itself.

**Architecture:** `tt-llm` exposes a **sync** `Classifier` trait. `RigClassifier` satisfies it by owning a `tokio::runtime::Runtime` and calling `block_on` (`crates/tt-llm/src/rig_classifier.rs:243,362`). `tt-cli` then gets concurrency back by spawning OS threads (`classify_auto.rs:514`). Above all of it, `tt-server` is async and calls in through 16 `spawn_blocking` sites. So the call path is: async reqwest → blocked into sync → re-parallelized with threads → invoked from async. This plan makes `Classifier` async, deletes `RigClassifier`'s owned runtime, and replaces `std::thread::scope` with `buffered(CLASSIFY_CONCURRENCY)`. Database writes stay exactly where they are: serial, on the one connection the `Resolver` holds.

**Tech Stack:** Rust (edition 2021), `async-trait` (**new dependency — the trait is used as `&dyn Classifier`, and native `async fn` in traits is not dyn-compatible**), `tokio` 1 (already a workspace dependency, `features = ["full"]`), `futures-util` 0.3 (already a workspace dependency), rig-core.

## What this does NOT buy, stated up front

**This plan does not make classification faster today, and anyone executing it expecting a speedup will be disappointed.** Measured 2026-08-09 on the live daemon: at `CLASSIFY_CONCURRENCY = 8` the drain was **7.1 real sessions/min with 0 rate-limit (429) and 0 overloaded (529) responses**; at 16 it was **8.0/min with 2 429s and 6 529s**. The provider is the constraint above 8, not the concurrency mechanism. Eight OS threads parked on network I/O cost microseconds of scheduling against 15–90 second calls.

What it buys is: one runtime instead of two, no thread stack per in-flight call, concurrency that is cheap to raise **if provider limits ever move**, and a `tt-llm` whose shape matches the I/O it actually does. Those are real, and they are architectural rather than metric. **Acceptance is parity, not improvement** — see Task 5.

## Global Constraints

- **No behavior change.** `tt report` for `2026-07-20` prints `Direct time: 16h 23m`; `2026-07-13..2026-07-20` prints `74h 20m`. Before and after, on a copy of the live database.
- **Database writes stay serial on one connection.** `Resolver::stream_named` is a non-transactional find-and-insert; two concurrent copies re-create the duplicate-stream failure that cost 55 renames, 5 merges and 9 dissolves to undo. Today the compiler enforces this because `tt_db::Database` is `Send` but not `Sync` and cannot cross into a `thread::scope` closure. **When the closure becomes a future, that protection weakens: a non-`Send` value can still live across an `.await` in a single-threaded context, and `buffered` does not move the resolver.** The plan keeps writes outside the concurrent section entirely (Task 3) — never move a `Database` into a future.
- **Order of applied verdicts must stay deterministic** (the chunk's own order). `futures_util::stream::buffered` preserves input order; `buffer_unordered` does **not**. Use `buffered`.
- Every existing guard is untouched: junk rules, `is_misnamed_stream`, `stream_exists` validation, subagent inheritance, proposal duplicate-guards, `CLASSIFIER_GENERATION` gating, the retry/timeout/deadline bounds in `transport.rs`.
- `MAX_MODEL_TURNS`, `MAX_FETCH_CALLS`, `REQUEST_TIMEOUT`, `MAX_CLASSIFICATION_MS`, `MAX_PARSE_ATTEMPTS`, `MAX_TRANSPORT_RETRIES` keep their exact values and meanings.
- `unsafe_code` denied. Zero clippy warnings (CI is `-D warnings`). `cargo fmt` clean (`max_width = 100`).
- Errors: `thiserror` (`LlmError`) in `tt-llm`, `anyhow` in `tt-cli`/`tt-server`.
- **jj, not git.** `jj describe -m "..."` then `jj new`. Never `git`. Never backticks or `$(...)` inside a double-quoted `-m` message — use `jj describe --stdin < file` for those.

## File Structure

| File | Responsibility after this plan |
|---|---|
| `Cargo.toml` (modify) | Adds `async-trait` to `[workspace.dependencies]`. |
| `crates/tt-llm/Cargo.toml` (modify) | Depends on `async-trait`. |
| `crates/tt-llm/src/types.rs` (modify, ~`Classifier` at 193, `MockClassifier` at 252) | `Classifier` becomes an async trait. `MockClassifier` implements it. |
| `crates/tt-llm/src/rig_classifier.rs` (modify, 243/275/324/356/394) | Loses its owned `Runtime`; `classify`, `describe_stream`, `classify_with_tools`, `extract`, `within` become async. |
| `crates/tt-llm/src/transport.rs` (modify) | `retrying` becomes async so it can `tokio::time::sleep` instead of blocking a thread. |
| `crates/tt-cli/src/commands/classify_auto.rs` (modify, `classify_concurrently` ~505) | Concurrency via `buffered(CLASSIFY_CONCURRENCY)` on a runtime handle instead of `std::thread::scope`. |
| `crates/tt-cli/src/commands/classify_auto/resolver.rs` (modify, `classify` at 75) | Awaits the classifier through the caller's runtime; health recording unchanged. |
| `crates/tt-cli/src/commands/streams/describe.rs` (modify, ~41) | Drives `describe_stream` on a runtime. |
| `crates/tt-cli/src/main.rs` (modify, `fn main` at 39) | Owns one runtime for the commands that call a model. |
| `crates/tt-server/src/loops/operations.rs` (modify, ~86) | Classification stops going through `spawn_blocking` for the model calls. |

---

### Task 1: Make `Classifier` an async trait, with `RigClassifier` temporarily bridged

Change the trait and `MockClassifier` first, keeping `RigClassifier` compiling via its existing runtime. This isolates the trait change from the rig rewrite so a reviewer can reject one without the other.

**Files:**
- Modify: `Cargo.toml`, `crates/tt-llm/Cargo.toml`, `crates/tt-llm/src/types.rs`
- Modify: `crates/tt-llm/src/rig_classifier.rs` (bridge only)
- Test: `crates/tt-llm/src/types.rs` (`mod tests`)

**Interfaces:**
- Produces:
  - `#[async_trait::async_trait] pub trait Classifier: Send + Sync { async fn classify(&self, input: &ClassificationInput, roster: &[StreamSummary]) -> Result<ClassificationOutput, LlmError>; async fn describe_stream(&self, evidence: &str) -> Result<String, LlmError>; }`

- [ ] **Step 1: Add the dependency**

In the root `Cargo.toml`, under `[workspace.dependencies]`, alongside the existing `tokio` (line 52) and `futures-util` (line 54):

```toml
async-trait = "0.1"
```

In `crates/tt-llm/Cargo.toml`, under `[dependencies]`:

```toml
async-trait.workspace = true
```

- [ ] **Step 2: Run `cargo deny` before relying on the dependency**

Run: `cargo deny check`
Expected: no new advisories or license failures. CI runs this in the Lint job; a dependency that fails it must not be added.

- [ ] **Step 3: Write the failing test**

Add to `mod tests` in `crates/tt-llm/src/types.rs`:

```rust
    #[tokio::test]
    async fn a_mock_classifier_answers_through_the_async_trait() {
        let classifier = MockClassifier::new(vec![Ok(ClassificationOutput {
            choice: StreamChoice::Existing {
                stream_id: "stream-a".to_string(),
            },
            confidence: 0.9,
            reasoning: "clear".to_string(),
        })]);

        let input = ClassificationInput {
            session_id: "session-1".to_string(),
            machine: None,
            cwd: None,
            starting_prompt: None,
            user_prompts: Vec::new(),
            window_titles: Vec::new(),
            started_at: None,
        };

        let output = classifier.classify(&input, &[]).await.unwrap();
        assert_eq!(output.confidence, 0.9);
    }
```

Match the real constructor and field names of `MockClassifier`, `ClassificationOutput`, `StreamChoice`, and `ClassificationInput` by reading `crates/tt-llm/src/types.rs` — the shapes above follow the existing tests in that file, but use the actual names.

`tokio` must be available as a dev-dependency of `tt-llm` for `#[tokio::test]`; it is already a normal dependency, which is sufficient.

- [ ] **Step 4: Run it and confirm it fails**

Run: `cargo test -p tt-llm --lib a_mock_classifier_answers_through_the_async_trait`
Expected: compile error — `.await` on a non-future, or `classify` is not async.

- [ ] **Step 5: Make the trait async**

In `crates/tt-llm/src/types.rs`, replace the trait at line 193:

```rust
/// A model that can place work on a stream.
///
/// Async because every implementation of it does network I/O. It was sync, and
/// `RigClassifier` paid for that by owning a `tokio::runtime::Runtime` and calling
/// `block_on` — so an async HTTP stack was blocked into a sync call, which callers then
/// re-parallelized with OS threads, from inside an async daemon. One runtime, one shape.
///
/// `async_trait` rather than a native `async fn` in trait: this is used as
/// `&dyn Classifier` throughout (`Resolver` holds one, `run_auto` takes one), and native
/// `async fn` in traits is not dyn-compatible.
#[async_trait::async_trait]
pub trait Classifier: Send + Sync {
    /// Places one unit of work, choosing from `roster` or proposing something new.
    ///
    /// # Errors
    /// [`LlmError`] when the provider refuses, the output cannot be parsed, or the
    /// classification exceeds its deadline.
    async fn classify(
        &self,
        input: &ClassificationInput,
        roster: &[StreamSummary],
    ) -> Result<ClassificationOutput, LlmError>;

    /// Writes a one-line description of a stream from evidence drawn from its work.
    ///
    /// # Errors
    /// [`LlmError`] when the provider refuses or the output cannot be parsed.
    async fn describe_stream(&self, evidence: &str) -> Result<String, LlmError>;
}
```

Then annotate `impl Classifier for MockClassifier` (line 252) with `#[async_trait::async_trait]` and make both its methods `async fn`. Its bodies are synchronous and stay exactly as they are — an `async fn` with no `.await` is a ready future, which is correct for a scripted mock.

- [ ] **Step 6: Bridge `RigClassifier` so the crate still compiles**

Annotate `impl Classifier for RigClassifier` (`crates/tt-llm/src/rig_classifier.rs:394`) with `#[async_trait::async_trait]`, make both methods `async fn`, and leave their bodies calling the existing sync internals — which still use `self.runtime.block_on`. Add a comment marking it temporary:

```rust
// TEMPORARY BRIDGE (removed in Task 2): the body below still drives the owned runtime
// with `block_on`. Calling `block_on` from inside an async context panics, so nothing may
// await this impl on a runtime thread until Task 2 lands. The only caller today is
// `tt-cli`, which is still fully synchronous at this point in the plan.
```

This is the one step of this plan that leaves the tree in a knowingly half-migrated state. Task 2 immediately follows and removes it; do not ship Task 1 on its own to a running daemon.

- [ ] **Step 7: Run it and confirm the trait test passes**

Run: `cargo test -p tt-llm --lib a_mock_classifier_answers_through_the_async_trait`
Expected: `test result: ok. 1 passed`

- [ ] **Step 8: Make the rest of the workspace compile**

`cargo check --all-targets` will now fail at every call site. Fix each **minimally**, by wrapping in the caller's existing sync context with a local runtime, so this task changes only the trait:

- `crates/tt-cli/src/commands/classify_auto/resolver.rs:76`
- `crates/tt-cli/src/commands/classify_auto.rs:514`
- `crates/tt-cli/src/commands/streams/describe.rs:41`
- `crates/tt-llm/src/rig_classifier.rs` tests at 963 and 1032 (make them `#[tokio::test] async fn` and `.await`)

For the `tt-cli` sites, the minimal bridge is a single module-level runtime created once:

```rust
/// Drives one async classifier call from synchronous code.
///
/// Scaffolding for the async migration: removed in Task 4, when `tt-cli` owns one runtime
/// at `main` instead of creating them at call sites.
fn block_on_classifier<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime is buildable")
        .block_on(future)
}
```

- [ ] **Step 9: Full gates**

Run: `cargo fmt && cargo clippy --all-targets && cargo test`
Expected: fmt silent, zero clippy warnings, all tests pass (baseline 1,154 plus the new one).

- [ ] **Step 10: Commit**

```bash
jj describe -m "refactor(tt-llm): make Classifier an async trait

Every implementation does network I/O, and the sync signature forced
RigClassifier to own a runtime and block_on inside it. async-trait rather
than a native async fn in trait, because this is used as &dyn Classifier.
RigClassifier is bridged here and rewritten next."
jj new
```

---

### Task 2: Delete `RigClassifier`'s owned runtime

**Files:**
- Modify: `crates/tt-llm/src/rig_classifier.rs` (243, 252-258, 275, 324, 356-362, 394-410, 943)
- Modify: `crates/tt-llm/src/transport.rs` (`retrying`)
- Test: `crates/tt-llm/src/transport/tests.rs`, `crates/tt-llm/src/rig_classifier.rs` (`mod tests`)

**Interfaces:**
- Consumes: the async `Classifier` trait from Task 1.
- Produces:
  - `RigClassifier` with **no** `runtime` field.
  - `async fn RigClassifier::within<T>(&self, deadline: &Deadline, future: impl Future<Output = T> + Send) -> Result<T, LlmError>`
  - `async fn transport::retrying<T, F, Fut>(deadline: &Deadline, attempt: F) -> Result<T, LlmError> where F: FnMut() -> Fut, Fut: Future<Output = Result<T, AttemptFailure>>`

- [ ] **Step 1: Write the failing test — no runtime field, and it works on the caller's runtime**

```rust
    #[tokio::test]
    async fn a_classification_runs_on_the_callers_runtime() {
        // Given: a classifier pointed at a local socket that never answers, so the call
        // resolves through the timeout path rather than the network.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let classifier = RigClassifier::for_test_against(&format!("http://{addr}"));

        // When: awaited directly on this test's runtime — no block_on anywhere.
        let input = test_input();
        let result = classifier.classify(&input, &[]).await;

        // Then: it returns an error rather than panicking with "cannot block_on from
        // within a runtime", which is what an owned runtime would do here.
        assert!(result.is_err(), "expected a transport failure, got {result:?}");
    }
```

Use the real constructor the existing tests at `rig_classifier.rs:943` and `:1032` use for a test classifier — that line currently builds one with `runtime: tokio::runtime::Runtime::new()`, so it will need updating in Step 3 anyway. Name the helper `for_test_against` only if no equivalent exists; otherwise use the existing one.

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p tt-llm --lib a_classification_runs_on_the_callers_runtime`
Expected: panic `Cannot start a runtime from within a runtime`, or a compile error on the test constructor.

- [ ] **Step 3: Remove the runtime and make the internals async**

In `crates/tt-llm/src/rig_classifier.rs`:

1. Delete the `runtime: tokio::runtime::Runtime` field (line 243) and its construction (lines 252–258) and the test construction at 943.
2. `within` (line 356) becomes async and awaits the timeout directly instead of `block_on`ing it:

```rust
    /// Bounds one attempt by what is left of the whole classification's allowance.
    ///
    /// The timeout is constructed *inside* the async block rather than around a
    /// `block_on`, so the clock starts when the future is polled.
    async fn within<T>(
        &self,
        deadline: &Deadline,
        future: impl std::future::Future<Output = T> + Send,
    ) -> Result<T, LlmError> {
        let Some(remaining) = deadline.remaining() else {
            return Err(LlmError::Timeout);
        };
        tokio::time::timeout(remaining, future)
            .await
            .map_err(|_| LlmError::Timeout)
    }
```

Match the existing error variant and `Deadline` API exactly — read lines 351–370 and `crates/tt-llm/src/transport.rs` before writing this; the shape above must not invent a `LlmError::Timeout` if the real variant is named differently.

3. `classify_with_tools` (275), `extract` (324), `classify` (395) and `describe_stream` (403) become `async fn` and `.await` the rig calls they currently drive through `block_on`.
4. The custom HTTP send at lines 513–519 builds its own current-thread runtime to call `HttpClientExt::send`. Make that function async and `.await` the send; delete the runtime.

- [ ] **Step 4: Make `transport::retrying` async**

The retry ladder currently sleeps by blocking. Convert it so it awaits:

```rust
/// Runs `attempt` until it succeeds, is refused, or spends one of the three bounds.
///
/// Async so a waiting retry parks a task instead of a thread. The three bounds are
/// unchanged: at most `MAX_TRANSPORT_RETRIES` retries, at most 20s of cumulative wait,
/// and the classification-wide `Deadline`.
pub(crate) async fn retrying<T, F, Fut>(
    deadline: &Deadline,
    mut attempt: F,
) -> Result<T, LlmError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, AttemptFailure>>,
{
    // ... same control flow as today, with `std::thread::sleep(wait)` replaced by
    // `tokio::time::sleep(wait).await`
}
```

**Change no bound, no jitter, no `Retry-After` handling, and no `is_transient` decision** — only how the wait is performed. `HttpFailure::is_transient` is the single place the status line is drawn and must not move.

- [ ] **Step 5: Run the transport and classifier suites**

Run: `cargo test -p tt-llm`
Expected: all tests pass. The timeout tests in `transport/tests.rs` build their error through rig's real transport against a local never-answering socket; they must still pass, and they are what catch a split `reqwest` in the dependency graph.

- [ ] **Step 6: Run it and confirm the Step 1 test passes**

Run: `cargo test -p tt-llm --lib a_classification_runs_on_the_callers_runtime`
Expected: `test result: ok. 1 passed`

- [ ] **Step 7: Confirm the runtime is actually gone**

Run: `grep -n "block_on\|tokio::runtime::Runtime" crates/tt-llm/src/`
Expected: **no matches** in non-test code. Any remaining match is an unmigrated path.

- [ ] **Step 8: Full gates**

Run: `cargo fmt && cargo clippy --all-targets && cargo test`
Expected: fmt silent, zero clippy warnings, all tests pass.

- [ ] **Step 9: Commit**

```bash
jj describe -m "refactor(tt-llm): drop RigClassifier's owned runtime

It built a tokio Runtime per classifier and block_on'd every model call
inside it, so an async HTTP stack was driven synchronously and then
re-parallelized by callers with OS threads. Calls now run on the caller's
runtime and retry waits park a task instead of a thread. Bounds, jitter,
Retry-After handling and is_transient are untouched."
jj new
```

---

### Task 3: Replace `std::thread::scope` with `buffered`

**Files:**
- Modify: `crates/tt-cli/src/commands/classify_auto.rs` (`classify_concurrently` ~505, `classify_sessions`)
- Modify: `crates/tt-cli/src/commands/classify_auto/resolver.rs` (`classify` at 75, `record_call`)
- Modify: `crates/tt-cli/Cargo.toml` (add `futures-util.workspace = true`)
- Test: `crates/tt-cli/src/commands/classify_auto.rs` (`mod tests`)

**Interfaces:**
- Consumes: async `Classifier` (Task 1), runtime-free `RigClassifier` (Task 2).
- Produces: `Resolver::classify_concurrently` returning `Vec<Result<ClassificationOutput, LlmError>>` in the chunk's input order, as today.

- [ ] **Step 1: Keep the existing concurrency guard, and confirm it still guards**

`a_chunks_calls_all_run_at_the_same_time` in `classify_auto.rs` is the only test that fails if the calls go serial — verified at 35.01s serial versus 0.01s concurrent. **It must survive this task unmodified except for whatever the async signature forces.** Read it before changing anything, and keep its condvar barrier: a barrier that all N calls must reach proves genuine concurrency, and it works identically for futures on a multi-threaded runtime.

- [ ] **Step 2: Run it and record the current pass time**

Run: `cargo test -p tt-cli --lib a_chunks_calls_all_run_at_the_same_time -- --nocapture`
Expected: PASS, fast (about 0.01s).

- [ ] **Step 3: Add `futures-util` to `tt-cli`**

In `crates/tt-cli/Cargo.toml` under `[dependencies]`:

```toml
futures-util.workspace = true
```

- [ ] **Step 4: Rewrite `classify_concurrently`**

Replace the `std::thread::scope` body with a buffered stream, driven on a runtime handle:

```rust
    /// Runs one chunk's model calls at the same time, answering in the chunk's order.
    ///
    /// `buffered` rather than `buffer_unordered`: verdicts are applied in the chunk's own
    /// order so a pass is reproducible, and unordered completion would make the applied
    /// order depend on which call finished first.
    ///
    /// `&self` rather than `&mut self` is still load-bearing. Only the classifier and a
    /// shared roster slice cross into the futures. **The database must never cross**: it
    /// is `Send` but not `Sync`, and where `thread::scope` had the compiler refuse it
    /// outright, a future can hold a non-`Send` value across an `.await`. Writes stay
    /// outside this function entirely, applied serially by the caller.
    fn classify_concurrently(
        &self,
        chunk: &[(ClassificationInput, u32)],
    ) -> Vec<Result<ClassificationOutput, LlmError>> {
        use futures_util::stream::{self, StreamExt};

        let classifier = self.classifier;
        let roster = self.roster.as_slice();
        self.runtime.block_on(async move {
            stream::iter(chunk.iter().map(|(input, _)| input))
                .map(|input| classifier.classify(input, roster))
                .buffered(CLASSIFY_CONCURRENCY)
                .collect::<Vec<_>>()
                .await
        })
    }
```

`self.runtime` is a `tokio::runtime::Handle` added to `Resolver` in Task 4. Until then, use the `block_on_classifier` helper from Task 1 Step 8 so this task compiles on its own.

Delete the `panicked(&*payload)` helper and the `JoinHandle::join` error handling: a future that panics propagates through `buffered` differently, and `Task 3 Step 6` covers it. **Do not delete the panic *test*** — rewrite it (Step 6).

- [ ] **Step 5: Run the concurrency guard again**

Run: `cargo test -p tt-cli --lib a_chunks_calls_all_run_at_the_same_time -- --nocapture`
Expected: PASS, still fast. If it now hangs, the runtime is single-threaded — `buffered` on a current-thread runtime still interleaves at `.await` points, but the test's condvar **blocks** the thread rather than awaiting, so a current-thread runtime deadlocks. Use a multi-threaded runtime (Task 4 makes this the real configuration) or convert the barrier to `tokio::sync::Barrier` and the mock to an async sleep.

- [ ] **Step 6: Restore the panic guarantee as a test**

`a_panicking_worker_costs_its_own_session_and_no_other` must keep its meaning: one failing call costs its own session and nothing else. With futures, a panic inside `buffered` unwinds the whole `block_on`. Restore the guarantee by catching per call:

```rust
                .map(|input| async move {
                    // A panicking call costs its own session, exactly as a panicking
                    // worker thread did: it becomes a failed classification, counted and
                    // recorded, with the session left unassigned.
                    match futures_util::FutureExt::catch_unwind(
                        std::panic::AssertUnwindSafe(classifier.classify(input, roster)),
                    )
                    .await
                    {
                        Ok(result) => result,
                        Err(payload) => Err(panicked(&*payload)),
                    }
                })
```

Keep the existing `panicked(&(dyn Any + Send)) -> LlmError` helper for this; it is still the right conversion. Run the existing panic test and confirm it still asserts 2 assigned, 1 error, `api: 1`, and a `last_error` containing `panicked`.

- [ ] **Step 7: Full gates**

Run: `cargo fmt && cargo clippy --all-targets && cargo test`
Expected: fmt silent, zero clippy warnings, all tests pass — including `one_chunk_naming_one_new_stream_many_times_creates_it_once`, which proves concurrent verdicts still collapse onto one stream row.

- [ ] **Step 8: Commit**

```bash
jj describe -m "refactor(classify): run a chunk's calls as futures, not threads

std::thread::scope parked an OS thread per in-flight model call. buffered
keeps the chunk's input order, so applied verdicts stay deterministic;
buffer_unordered would not. Writes are still outside the concurrent
section and still serial on the resolver's one connection."
jj new
```

---

### Task 4: One runtime, owned at the top

**Files:**
- Modify: `crates/tt-cli/src/main.rs` (`fn main` at 39)
- Modify: `crates/tt-cli/src/commands/classify_auto.rs` (`run_auto` at 649), `classify_auto/resolver.rs` (`Resolver::new`)
- Modify: `crates/tt-cli/src/commands/streams/describe.rs` (~41)
- Modify: `crates/tt-server/src/loops/operations.rs` (~86)

**Interfaces:**
- Produces:
  - `run_auto(db: &tt_db::Database, config: &Config, classifier: &dyn Classifier, runtime: tokio::runtime::Handle) -> Result<AutoClassifyOutcome>`
  - `Resolver` gains `runtime: tokio::runtime::Handle`.

- [ ] **Step 1: Give `tt-cli` a runtime at `main`**

`crates/tt-cli/src/main.rs:39` is `fn main() -> Result<()>`. Keep it synchronous — `tt` is a CLI and most subcommands touch no network — and build one multi-threaded runtime, passing its `Handle` only to the commands that call a model:

```rust
fn main() -> Result<()> {
    // One runtime for the whole process. Only the model-calling commands use it, but a
    // single owner beats a runtime built per call site, which is what the migration
    // scaffolding did.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build the async runtime")?;
    // ... existing arg parsing and dispatch, passing runtime.handle().clone() to the
    // classify and describe commands
}
```

**Multi-threaded, not current-thread**: `buffered` on a current-thread runtime still interleaves at await points, but any blocking call inside a future (the mock's condvar in tests, or a stray synchronous DB read) stalls every other in-flight call.

- [ ] **Step 2: Thread the handle through**

Add `runtime: tokio::runtime::Handle` to `Resolver` and to `run_auto`'s signature. Replace every use of the temporary `block_on_classifier` helper with `self.runtime.block_on(...)`, then **delete that helper**. Update `crates/tt-cli/src/commands/streams/describe.rs:41` to drive `describe_stream` on the same handle.

- [ ] **Step 3: Stop the daemon double-wrapping**

`crates/tt-server/src/loops/operations.rs:86` runs `run_auto` inside `tokio::task::spawn_blocking`. That is still correct — `run_auto` performs blocking SQLite work between chunks and must not run on an async worker — but it must now be handed the runtime handle:

```rust
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || -> Result<ClassifyAttempt> {
        // ...
        match tt_cli::commands::classify_auto::run_auto(&db, &config, &*classifier, handle) {
```

**`Handle::block_on` from inside `spawn_blocking` is allowed** — `spawn_blocking` runs on a dedicated blocking thread, not an async worker. Calling it from an async worker would panic. Confirm at runtime in Step 5, not by reasoning alone.

- [ ] **Step 4: Full gates**

Run: `cargo fmt && cargo clippy --all-targets && cargo test`
Expected: fmt silent, zero clippy warnings, all tests pass.

Then confirm nothing builds a runtime outside `main` and the daemon:

Run: `grep -rn "Runtime::new\|new_current_thread\|new_multi_thread" crates/ --include=*.rs | grep -v test`
Expected: exactly one match, in `crates/tt-cli/src/main.rs`.

- [ ] **Step 5: Prove it on the live daemon before trusting it**

```bash
cd /home/sami/Code/time-tracker/default
cargo build --release --bin tt --bin tt-serve
systemctl --user stop tt-serve.service && sleep 3
cp target/release/tt-serve ~/.local/bin/tt-serve
cp target/release/tt ~/.local/bin/tt
systemctl --user start tt-serve.service && sleep 120
systemctl --user is-active tt-serve.service
systemctl --user show tt-serve.service -p NRestarts --value
journalctl --user -u tt-serve.service --since "3 min ago" --no-pager -o cat \
  | sed 's/\x1b\[[0-9;]*m//g' | grep -icE "cannot start a runtime|cannot block_on|panic"
```

Expected: `active`, `0` restarts, and **`0`** runtime/panic messages. A `Cannot start a runtime from within a runtime` panic here means `block_on` reached an async worker thread; that is the one failure mode this task can introduce, and it will not show up in unit tests.

- [ ] **Step 6: Commit**

```bash
jj describe -m "refactor(cli): own one runtime at main and thread its handle

Migration scaffolding built a current-thread runtime per call site. tt now
builds one multi-threaded runtime and hands its Handle to the commands that
call a model. The daemon keeps spawn_blocking around run_auto, which is
where block_on is legal, and passes the ambient handle in."
jj new
```

---

### Task 5: Verify parity and record what this did and did not buy

**Files:**
- Modify: `AGENTS.md`
- Modify: `crates/tt-llm/src/lib.rs` (module docs) or `crates/tt-llm/AGENTS.md` if one exists

**Interfaces:**
- Consumes: everything from Tasks 1-4 — the async `Classifier` trait, the runtime-free `RigClassifier`, `classify_concurrently`'s `buffered` stream, and `run_auto`'s `runtime: tokio::runtime::Handle` parameter.
- Produces: no code interface. Produces the measured parity figures that the `AGENTS.md` entry cites, and which any future change to `CLASSIFY_CONCURRENCY` or the concurrency mechanism must be compared against.

- [ ] **Step 1: Measure the drain, the same way it was measured before**

```bash
cd /home/sami/Code/time-tracker/default
DB=~/.local/share/time-tracker/tt.db
real(){ sqlite3 -noheader "$DB" "SELECT count(*) FROM agent_sessions s WHERE s.session_type='user' AND s.session_id NOT IN(SELECT session_id FROM classified_sessions) AND NOT(s.tool_call_count=0 AND s.message_count<=2);"; }
a=$(real); sleep 900; b=$(real)
echo "real/min: $(echo "scale=1;($a-$b)*60/900"|bc)"
journalctl --user -u tt-serve.service --since "16 min ago" --no-pager -o cat \
  | sed 's/\x1b\[[0-9;]*m//g' \
  | { j=$(cat); printf "429=%s 529=%s errors=%s ingest-failures=%s\n" \
      "$(echo "$j"|grep -ciE '429|rate.?limit')" "$(echo "$j"|grep -c 529)" \
      "$(echo "$j"|grep -c 'resolver: automatic classification failed')" \
      "$(echo "$j"|grep -c 'session ingest failed')"; }
```

Expected: **~7 real/min, 0 429, 0 529, 0 ingest failures** — parity with the thread-based implementation measured 2026-08-09. **Parity is success.** A large improvement would be surprising and worth investigating rather than celebrating, since the provider is the constraint; a regression means a future is being awaited serially somewhere.

- [ ] **Step 2: Verify the numbers**

```bash
cp ~/.local/share/time-tracker/tt.db /tmp/async.db
TT_DATABASE_PATH=/tmp/async.db ./target/release/tt report --start 2026-07-20 --end 2026-07-21 | grep '^Direct time:'
TT_DATABASE_PATH=/tmp/async.db ./target/release/tt report --start 2026-07-13 --end 2026-07-20 | grep '^Direct time:'
sqlite3 -noheader "$DB" "SELECT count(*) FROM(SELECT trim(name) FROM streams WHERE name IS NOT NULL GROUP BY trim(name) HAVING count(*)>1);"
```

Expected: `16h 23m`, `74h 20m`, and **0** duplicate stream names — the last is the guard that concurrent verdicts still collapse onto one row.

- [ ] **Step 3: Record it in `AGENTS.md`**

Add to the Anti-Patterns list, in the voice of its neighbours:

```markdown
- **The classifier is async because its work is I/O, and it must not acquire a second runtime**: `Classifier` was a sync trait, so `RigClassifier` owned a `tokio::runtime::Runtime` and `block_on`'d every model call inside it, `tt-cli` re-parallelized those blocking calls with `std::thread::scope`, and `tt-server` invoked the result from an async daemon through `spawn_blocking`. Async HTTP, blocked, re-threaded, called from async. The trait is now async (`async_trait`, because it is used as `&dyn Classifier` and native `async fn` in traits is not dyn-compatible), `tt` owns exactly one multi-threaded runtime at `main`, and a chunk's calls run through `buffered(CLASSIFY_CONCURRENCY)` — **`buffered`, never `buffer_unordered`**, because verdicts are applied in the chunk's own order so a pass is reproducible. **This bought no throughput and was not expected to**: measured before and after at ~7 real sessions/min, because the provider rate-limits above `CLASSIFY_CONCURRENCY = 8` (16 produced 2 429s and 6 529s for +18%). What it removed is the second runtime and a thread stack per in-flight call. The daemon still wraps `run_auto` in `spawn_blocking`, and must: `Handle::block_on` is legal on a blocking thread and panics on an async worker. **Database writes stay outside the concurrent section and stay serial** — `thread::scope` used to make that a compile error, since `Database` is `Send` but not `Sync`; a future can hold a non-`Send` value across an `.await`, so the guarantee is now structural (the resolver applies verdicts after the stream completes) rather than compiler-enforced. Moving a `Database` into one of these futures re-creates the duplicate-stream failure that cost 55 renames, 5 merges and 9 dissolves.
```

Also update the **Async** subsection of `AGENTS.md`'s Code Conventions, which currently states that `tt-llm` exposes a sync `Classifier` and drives rig on an internal runtime. Leaving it would make two parts of the file disagree.

- [ ] **Step 4: Full gates and commit**

Run: `cargo fmt --check && cargo clippy --all-targets && cargo test && cargo deny check`
Expected: all clean.

```bash
jj describe --stdin <<'MSG'
docs(agents): record the async classifier migration and its null result

Parity, not improvement: ~7 real sessions/min before and after, because the
provider rate-limits above CLASSIFY_CONCURRENCY=8. What went away is the
second tokio runtime and a thread stack per in-flight call. Notes that the
serial-write guarantee is now structural rather than compiler-enforced.
MSG
jj new
```

---

## Self-Review

**Spec coverage.** The critique was that the stack blocks an async HTTP client into a sync trait and then re-parallelizes with OS threads, from inside an async daemon. Task 1 makes the trait async, Task 2 deletes the owned runtime and un-blocks the retry ladder, Task 3 replaces threads with futures, Task 4 consolidates on one runtime, Task 5 verifies parity and records it.

**Placeholders.** None. Three steps instruct matching real names in files they name with line numbers (`MockClassifier`'s constructor, `Deadline`'s API and the timeout error variant, the test classifier constructor at `rig_classifier.rs:943`) — deliberate, because guessing a name is worse than reading one.

**Type consistency.** `Classifier::{classify, describe_stream}` are async from Task 1 and awaited in Tasks 2–4. `within` and `retrying` gain `async` in Task 2 and are used that way inside `RigClassifier`. `run_auto` gains `runtime: tokio::runtime::Handle` in Task 4 and is called with it from `operations.rs`. `classify_concurrently` keeps returning `Vec<Result<ClassificationOutput, LlmError>>` in input order throughout.

**Risks flagged for the reviewer.**
1. **Task 1 leaves the tree knowingly half-migrated** (async trait, `RigClassifier` still blocking internally). It is the one point in this plan where the tree must not be deployed. Task 2 immediately follows.
2. **The serial-write guarantee changes character.** Today `thread::scope` plus `Database: !Sync` makes a stray database access inside the concurrent section a compile error. With futures that protection is weaker. This is the single most important thing for a reviewer to check by hand, and it is why Task 3's doc comment says so in the code rather than only here.
3. **`buffered` versus `buffer_unordered`** is a one-word difference that silently makes verdict application order depend on completion order. Named in the constraints, in Task 3's code comment, and in the `AGENTS.md` entry.
4. **`Handle::block_on` panics on an async worker thread** and no unit test will catch it; Task 4 Step 5 checks the live daemon's journal for exactly that panic.

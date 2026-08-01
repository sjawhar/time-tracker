# Attribution Engine (Phase 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Live stream attribution for tt — a semantic classifier (rig + Anthropic API) with confidence-gated proposals, a single allocation entry point, and a `tt-serve` daemon binary that keeps `tt status` current — per `specs/design/2026-07-24-priority-dashboard-design.md` Phase 1.

**Architecture:** Three chunks that land independently: (1a) extract the allocation prep duplicated across report/recompute into `tt_db::allocate_for_period` — pure refactor, zero behavior change; (1b) schema v11 (descriptions, proposals, meta/db_version) + new `tt-llm` crate (rig classifier behind a mockable trait) + `tt classify --auto`, `tt proposals`, `tt streams describe`, and a verdict section in `tt status`; (1c) new `tt-server` crate providing `tt-serve` daemon binary with ingest/sync/classify loops and a minimal axum `/api/status` + SSE skeleton for Phase 2 to build on.

**Tech Stack:** Rust workspace (existing: rusqlite, chrono, clap, anyhow/thiserror, figment, insta). New: `rig-core` (pinned exact version), `schemars` (extractor JSON Schema), `tokio`, `axum` 0.8. **No connection pool in Phase 1** — the daemon (1c) opens a fresh `Database` connection per blocking task/tick; `r2d2`+`r2d2_sqlite` are deferred to Phase 2 (read-heavy dashboard).

## Global Constraints

- Workspace lints: `unsafe_code = "deny"`; clippy all/pedantic/nursery warn; CI runs `cargo clippy --all-targets` with `-D warnings` — zero warnings.
- Lint suppressions: `#[expect(clippy::lint_name, reason = "...")]` only — never bare `#[allow]`.
- Library crates use `thiserror`; `tt-cli` uses `anyhow` with `.context(...)`.
- No `unwrap()` in non-test code (except compile-time-safe patterns).
- Additive migrations only: bump `SCHEMA_VERSION`, migrate supported older versions forward in `init()`, fail fast on unsupported.
- Snapshot tests via `insta`; run `cargo insta review` when output intentionally changes. **Task 1 must NOT change any snapshot.**
- Commit style: conventional commits (`feat:`, `refactor:`, `test:`). This repo uses **jj**, not git: commit = `jj describe -m "..."` then `jj new`. One logical change per commit.
- The spec is the authority: `specs/design/2026-07-24-priority-dashboard-design.md`. Config defaults: confidence threshold 0.8, drift window 90 min, ingest interval 30s, sync interval 60s.
- No caller may invoke `tt_core::allocate_time` directly except inside `tt-core` and `tt_db::allocate_for_period` (Task 1 adds the CI grep).

---

### Task 1: `allocate_for_period` — single allocation entry point

**Files:**
- Modify: `crates/tt-db/src/lib.rs` (new public module `allocation` or top-level fns near the Stream methods)
- Modify: `crates/tt-cli/src/commands/report.rs:300-356` (replace inlined prep)
- Modify: `crates/tt-cli/src/commands/recompute.rs:60-97` (replace inlined prep)
- Modify: `.github/workflows/pr-and-main.yml` (forbidden-pattern grep)
- Test: `crates/tt-db/src/lib.rs` (unit tests in `mod tests`)

**Interfaces:**
- Consumes: `tt_core::{allocate_time, AllocationConfig, AllocationResult, SessionType}`, existing `Database` methods `get_events_in_range`, `get_agent_session_start_events`, `agent_sessions_in_range`.
- Produces (later tasks rely on exactly this):

```rust
/// In tt-db. The ONLY allowed entry point for time allocation outside tt-core.
pub fn allocate_for_period(
    db: &Database,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    period_end: Option<DateTime<Utc>>,   // Some(end) for report/drift/dashboard; None preserves recompute's current semantics
    config: &tt_core::AllocationConfig,
) -> Result<tt_core::AllocationResult, DbError>;
```

Behavior (moved verbatim from report.rs): load events in `[start, end]` ascending; backfill `agent_session(started)` events for sessions that have tool-use events in range but no start event; load `agent_sessions_in_range(start, end)`; build `session_types` and `session_end_times` maps (end times only for sessions that have one); call `allocate_time(&events, config, period_end, &session_end_times, &session_types)`.

- [ ] **Step 1: Write failing unit test in tt-db**

```rust
#[test]
fn test_allocate_for_period_populates_session_maps() {
    let db = Database::open_in_memory().unwrap();
    // Session with an explicit end_time; delegated time must stop at end_time,
    // not at the 30-min timeout heuristic.
    let start = ts(0);
    insert_session_with_end(&db, "sess-a", start, start + chrono::Duration::minutes(10));
    insert_agent_session_event(&db, "e-start", start, "sess-a", "started");
    insert_tool_use_event(&db, "e-tool", start + chrono::Duration::minutes(1), "sess-a");

    let result = allocate_for_period(
        &db, start - chrono::Duration::hours(1), start + chrono::Duration::hours(2),
        Some(start + chrono::Duration::hours(2)), &tt_core::AllocationConfig::default(),
    ).unwrap();

    // 10 min (real end), not 31 min (timeout heuristic after last tool use)
    assert_eq!(total_delegated(&result), 10 * 60 * 1000);
}

#[test]
fn test_allocate_for_period_backfills_missing_session_starts() {
    let db = Database::open_in_memory().unwrap();
    // Tool-use event in range whose session start predates the range:
    // the start event must be backfilled so allocation sees the session.
    insert_agent_session_event(&db, "e-start", ts(0), "sess-b", "started");
    insert_tool_use_event(&db, "e-tool", ts(0) + chrono::Duration::hours(3), "sess-b");
    let result = allocate_for_period(
        &db, ts(0) + chrono::Duration::hours(2), ts(0) + chrono::Duration::hours(4),
        Some(ts(0) + chrono::Duration::hours(4)), &tt_core::AllocationConfig::default(),
    ).unwrap();
    assert!(total_delegated(&result) > 0, "session start must be backfilled");
}
```

Use the existing `make_event`-style fixture helpers in tt-db's test module; add small `insert_*` helpers as needed (follow the existing builder patterns at the bottom of `lib.rs`).

- [ ] **Step 2: Run to verify failure** — `cargo test -p tt-db allocate_for_period` → FAIL (function not found).

- [ ] **Step 3: Implement** — move the prep logic from `report.rs:300-346` into `tt-db` exactly (the `BTreeSet` session-start backfill, the two map builds), keeping report's semantics. Signature above.

- [ ] **Step 4: Run** — `cargo test -p tt-db allocate_for_period` → PASS.

- [ ] **Step 5: Migrate `report.rs`** — replace lines ~300-356 with a call to `tt_db::allocate_for_period(db, period_start, period_end, Some(period_end), &config)`. Delete the now-unused imports (`allocate_time`, `SessionType` if unused).

- [ ] **Step 6: Migrate `recompute.rs`** — replace its inlined prep with `allocate_for_period(db, <earliest event ts>, <latest event ts>, None, &config)`. **`None` preserves current recompute semantics exactly.**

- [ ] **Step 7: Verify zero behavior change** — `cargo test` (workspace). Every existing test including all 18 insta snapshots must pass **unchanged**. If any snapshot differs, the refactor is wrong — do not `insta accept`; fix the wrapper.

- [ ] **Step 8: Add CI grep** — in `.github/workflows/pr-and-main.yml` lint job:

```yaml
- name: Forbid direct allocate_time callers
  run: |
    ! grep -rn 'allocate_time(' crates/tt-cli/src crates/tt-server/src 2>/dev/null \
      | grep -v 'allocate_for_period'
```

- [ ] **Step 9: Lint + commit** — `cargo clippy --all-targets` clean, `cargo fmt`; `jj describe -m "refactor(db): extract allocate_for_period as single allocation entry point"` then `jj new`.

---

### Task 2: Schema v11 — descriptions, colors, meta/db_version, proposals

**Files:**
- Modify: `crates/tt-db/src/lib.rs` (`SCHEMA_VERSION` 10→11, `init()` DDL + forward migration, `Stream` struct, `STREAM_COLUMNS`, `row_to_stream`, `insert_stream`, new accessors, new `Proposal` struct)
- Test: same file, `mod tests`

**Interfaces:**
- Produces:

```rust
// Stream gains two fields (Option<String>): description, color.
pub struct Proposal {
    pub id: String,                       // uuid
    pub created_at: DateTime<Utc>,
    pub session_id: Option<String>,       // session-level proposal…
    pub event_ids: Option<Vec<String>>,   // …or event-level (JSON array column)
    pub proposed_stream_id: Option<String>,      // None => proposes a new stream
    pub proposed_new_stream: Option<String>,     // JSON {name, description, tags}
    pub confidence: f64,
    pub reasoning: String,
    pub status: ProposalStatus,           // Pending | Accepted | Rejected
}

/// Classifier health, persisted in `meta` (survives across daemon/CLI processes).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ClassifierHealth {
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub consecutive_failures: u32,
}

impl Database {
    pub fn set_stream_description(&self, stream_id: &str, description: &str) -> Result<(), DbError>;
    pub fn set_stream_color(&self, stream_id: &str, color: Option<&str>) -> Result<(), DbError>;
    pub fn bump_db_version(&self) -> Result<i64, DbError>;   // UPDATE meta, returns new value
    pub fn get_db_version(&self) -> Result<i64, DbError>;
    pub fn insert_proposal(&self, p: &Proposal) -> Result<(), DbError>;
    pub fn get_proposals(&self, status: Option<ProposalStatus>) -> Result<Vec<Proposal>, DbError>;
    pub fn set_proposal_status(&self, id: &str, status: ProposalStatus) -> Result<(), DbError>;
    pub fn has_rejected_proposal(&self, session_id: &str, stream_id: &str) -> Result<bool, DbError>;
    pub fn get_pending_proposal_for_session(&self, session_id: &str) -> Result<Option<Proposal>, DbError>;
    // Re-check bookkeeping (spec: one re-check after more prompts accumulate)
    pub fn record_classification(&self, session_id: &str, prompt_count: u32) -> Result<(), DbError>;
    pub fn get_recheck_candidates(&self) -> Result<Vec<(String, u32)>, DbError>; // (session_id, prompt_count_at_classification), rechecked=0
    pub fn mark_rechecked(&self, session_id: &str) -> Result<(), DbError>;   // classified_sessions.rechecked = 1
    // Event-level proposal lookups for window-run proposals (session_id NULL, event_ids set):
    pub fn get_pending_proposal_for_events(&self, event_ids: &[String]) -> Result<Option<Proposal>, DbError>;
    // Rejection memory for a proposed NEW stream on a session (proposed_stream_id IS NULL):
    pub fn has_rejected_new_stream_proposal(&self, session_id: &str) -> Result<bool, DbError>;
    // Narrow classifier-path assignment helpers (amendment 1). assign_events_by_session_id/_by_ids
    // skip ONLY 'user', so they overwrite 'inferred'/'todo_link'; classifier paths must be narrower:
    pub fn assign_unassigned_events_by_session_id(&self, session_id: &str, stream_id: &str, source: &str) -> Result<u64, DbError>;   // WHERE session_id = ? AND stream_id IS NULL
    pub fn assign_unassigned_events_by_ids(&self, ids: &[String], stream_id: &str, source: &str) -> Result<u64, DbError>;           // WHERE id IN (...) AND stream_id IS NULL
    pub fn reassign_inferred_events_by_session_id(&self, session_id: &str, stream_id: &str, source: &str) -> Result<u64, DbError>;  // WHERE session_id = ? AND assignment_source = 'inferred'
    // Session lookup carrying machine_id (agent_sessions_in_range drops the column, lib.rs:1429):
    pub fn get_agent_session(&self, session_id: &str) -> Result<Option<(tt_core::session::AgentSession, Option<String>)>, DbError>;  // (session, machine_id)
    pub fn unclassified_session_ids(&self) -> Result<Vec<String>, DbError>;  // DISTINCT session_id WHERE stream_id IS NULL AND session_id IS NOT NULL
    // Classifier health (amendment 14) — meta-backed; these writes MUST NOT bump db_version:
    pub fn get_classifier_health(&self) -> Result<ClassifierHealth, DbError>;
    pub fn record_classifier_success(&self, at: DateTime<Utc>) -> Result<(), DbError>;              // sets last_success_at, clears consecutive_failures
    pub fn record_classifier_failure(&self, at: DateTime<Utc>, error: &str) -> Result<(), DbError>; // sets last_error, increments consecutive_failures
}
```

**`db_version` is a mutation contract (amendment 3).** Every DB method that changes attribution-visible state bumps `meta.db_version` **inside the same transaction, only when rows actually changed** (`count > 0`), so external writers — notably `tt-watcher`'s `insert_events` (lib.rs:190) — are detected by the daemon's watcher with no extra call site. Methods that MUST bump: `insert_events`; every assignment helper (`assign_events_by_session_id`, `assign_events_by_ids`, the three narrow helpers above, `assign_event_to_stream`, `assign_events_to_stream`, `clear_inferred_assignments`); proposal mutations (`insert_proposal`, `set_proposal_status`); stream mutations (`insert_stream`, `set_stream_description`, `set_stream_color`, `set_stream_slug`, `update_stream_times`, `mark_streams_for_recompute`); machine sync-state writers (the setters paired with `get_machine_last_event_id_by_label` / `get_machine_last_sync_at_by_label`). Methods that MUST NOT bump (internal bookkeeping, no verdict impact): `record_classification`, `mark_rechecked`, and the three `*_classifier_*` health writers. Classifier health persists in `meta` under dedicated keys (`classifier_last_success_at`, `classifier_last_error`, `classifier_consecutive_failures`) — no new table. `bump_db_version()` stays public for tests and any future explicit use.

- [ ] **Step 1: Failing tests** — migration test (open a v10 fixture DB → `init()` migrates to 11, `streams.description` queryable, `meta.db_version = 0`), proposal round-trip test (insert → get pending → accept → not in pending; `has_rejected_proposal` true after reject), `bump_db_version` increments. **Also cover the amendment additions:** `insert_events`/assignment/proposal/stream/machine-sync mutations each advance `db_version` while `record_classification`/`mark_rechecked`/classifier-health writers do NOT; `assign_unassigned_events_by_session_id` leaves a `user`- or `inferred`-assigned event untouched and fills only `stream_id IS NULL`; `reassign_inferred_events_by_session_id` rewrites only `inferred` rows; `get_agent_session` returns the row's `machine_id`; `unclassified_session_ids` lists only sessions with a NULL-stream event; `has_rejected_new_stream_proposal` true after rejecting a new-stream proposal; `get_pending_proposal_for_events` finds an event-level pending proposal; `get_classifier_health` round-trips success/failure state. Follow the existing migration-test pattern near `lib.rs:2186`.
- [ ] **Step 2: Run** — `cargo test -p tt-db schema_v11 proposal` → FAIL.
- [ ] **Step 3: Implement** — DDL in `init()` for fresh DBs AND the 10→11 forward path:

```sql
ALTER TABLE streams ADD COLUMN description TEXT;
ALTER TABLE streams ADD COLUMN color TEXT;
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
INSERT OR IGNORE INTO meta(key, value) VALUES('db_version', '0');
CREATE TABLE IF NOT EXISTS proposals (
    id TEXT PRIMARY KEY, created_at TEXT NOT NULL,
    session_id TEXT, event_ids TEXT,
    proposed_stream_id TEXT, proposed_new_stream TEXT,
    confidence REAL NOT NULL, reasoning TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'
);
CREATE INDEX IF NOT EXISTS idx_proposals_status ON proposals(status);
-- Classifier re-check bookkeeping (one re-check per session after prompts grow)
CREATE TABLE IF NOT EXISTS classified_sessions (
    session_id TEXT PRIMARY KEY,
    classified_at TEXT NOT NULL,
    prompt_count INTEGER NOT NULL,
    rechecked INTEGER NOT NULL DEFAULT 0
);
```

Extend `Stream`, `STREAM_COLUMNS`, `row_to_stream`, `insert_stream` (existing older-version migrations must add the columns too). All existing tests must still pass (Stream construction sites gain `description: None, color: None`).
- [ ] **Step 4: Run** — `cargo test -p tt-db` → PASS. **Step 5:** clippy/fmt, commit `feat(db): schema v11 — stream descriptions/colors, meta db_version, proposals`.

---

### Task 3: `tt-llm` crate — rig classifier behind a mockable trait

**Files:**
- Create: `crates/tt-llm/Cargo.toml`, `crates/tt-llm/src/lib.rs`, `crates/tt-llm/src/types.rs`, `crates/tt-llm/src/rig_classifier.rs`, `crates/tt-llm/src/prompt.rs`
- Modify: root `Cargo.toml` (workspace members + `rig-core = "=X.Y.Z"` pinned to latest at implementation time, `tokio = { version = "1", features = ["rt-multi-thread", "macros"] }`, `schemars` for the extractor's JSON Schema — pin to the version rig re-exports/expects)
- Test: `crates/tt-llm/src/lib.rs` tests with a `MockClassifier`; `prompt.rs` **structural** assertions on the assembled prompt (no natural-language snapshot — amendment 7)

**Interfaces:**
- Produces:

```rust
pub struct ClassificationInput {
    pub session_id: String,
    pub machine: Option<String>,
    pub cwd: Option<String>,
    pub starting_prompt: Option<String>,
    pub user_prompts: Vec<String>,        // most recent, truncated to prompt budget
    pub window_titles: Vec<String>,       // for window-run classification; empty for sessions
}
pub struct StreamSummary {
    pub slug: Option<String>, pub id: String, pub name: Option<String>,
    pub description: Option<String>, pub tags: Vec<String>,
    pub last_active: Option<DateTime<Utc>>,
}
pub enum StreamChoice { Existing { stream_id: String }, New { name: String, description: String } }
pub struct ClassificationOutput { pub choice: StreamChoice, pub confidence: f64, pub reasoning: String }

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("api key env var {0} not set")] MissingApiKey(String),
    #[error("model call failed: {0}")] Api(String),
    #[error("unparseable model output: {0}")] Parse(String),
}

/// Exported publicly (not cfg(test)) — Tasks 4, 5, and 8 use it in their tests. Interior
/// mutability with `&self` + `Send + Sync` (the daemon shares `dyn Classifier` across threads):
/// both scripts live behind a `Mutex`.
pub struct MockClassifier {
    pub scripted: std::sync::Mutex<std::collections::VecDeque<Result<ClassificationOutput, LlmError>>>,
    pub descriptions: std::sync::Mutex<std::collections::VecDeque<Result<String, LlmError>>>,
}

/// Serde/JSON-Schema struct rig's extractor fills, then mapped into `ClassificationOutput`.
#[derive(Debug, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
pub struct ClassificationExtract {
    pub stream_id: Option<String>,        // Some => existing; None => new stream
    pub new_stream_name: Option<String>,  // required when stream_id is None
    pub new_stream_description: Option<String>,
    pub confidence: f64,
    pub reasoning: String,
}

pub trait Classifier: Send + Sync {
    fn classify(&self, input: &ClassificationInput, roster: &[StreamSummary])
        -> Result<ClassificationOutput, LlmError>;   // sync facade; RigClassifier owns a tokio Runtime internally
    /// Stream-description generation (amendment 8 — defined here, consumed by Task 5, not ad hoc).
    fn describe_stream(&self, evidence: &str) -> Result<String, LlmError>;
}
pub struct RigClassifier { /* model name, api key, runtime */ }
impl RigClassifier { pub fn from_config(model: &str, api_key_env: &str) -> Result<Self, LlmError>; }
```

Design notes locked here: the trait is **sync** (tt-cli is sync; `RigClassifier` wraps `tokio::runtime::Runtime::block_on` internally — the daemon in Task 8 calls it via `spawn_blocking`) and **`Send + Sync`** so the daemon can hold `dyn Classifier` across threads. Structured output via rig's extractor into `ClassificationExtract` (derives `Deserialize + Serialize + schemars::JsonSchema`; the flattened `stream_id`/`new_stream_*` fields are LLM-friendly), then mapped to `ClassificationOutput`. `describe_stream` reuses the same extractor/model with a string output. One retry on parse failure. No agent tools in this task — the golden-set evaluation during 1b dogfooding decides whether a fetch-more-context tool earns its complexity (spec allows up to 3 turns).

- [ ] **Step 1: Failing prompt tests** — `prompt::build(input, roster)` returns a String; assert **structurally** (amendment 7), not by snapshotting the natural-language text: every roster stream's slug/id appears exactly once (roster inclusion); a `user_prompts` entry longer than the budget is truncated to ≤ the budget (~500 chars) and only the most-recent 5 survive; the new-stream instruction (name+description required) and the `confidence` 0..1 instruction are present as substrings.
- [ ] **Step 2:** `cargo test -p tt-llm` → FAIL. **Step 3:** implement `types.rs` + `prompt.rs` (+ the `ClassificationExtract` mapping). **Step 4:** PASS. No prompt snapshot to bless; classifier *behavior* is covered by `MockClassifier` scripts here and by recorded-response fixtures in the consumer tasks (golden set, amendment 7).
- [ ] **Step 5: RigClassifier** — implement against rig's extractor API (check docs.rs for the pinned version; the API renames across minors). Config: model + `api_key_env` (default `ANTHROPIC_API_KEY`) come from Task 4's config additions. No unit test hits the network: gate an ignored integration test `#[ignore = "requires ANTHROPIC_API_KEY"]` that classifies one hardcoded input against the real API for manual verification.
- [ ] **Step 6:** clippy/fmt (new crate inherits workspace lints), commit `feat(llm): tt-llm crate with rig-backed stream classifier`.

---

### Task 4: Resolver + `tt classify --auto`

**Files:**
- Create: `crates/tt-cli/src/commands/classify_auto.rs`
- Modify: `crates/tt-cli/src/cli.rs` (add `--auto` flag to Classify), `crates/tt-cli/src/commands/mod.rs`, `crates/tt-cli/src/commands/classify.rs` (expose window-run construction — see Step 0 pre-step), `crates/tt-cli/src/config.rs` (classifier config section), `crates/tt-cli/Cargo.toml` (dep tt-llm)
- Test: `classify_auto.rs` `mod tests` with `MockClassifier`

**Interfaces:**
- Consumes: `tt_llm::{Classifier, MockClassifier (test), ClassificationInput, StreamSummary, StreamChoice, ClassificationOutput}`, Task 2 proposal accessors, the **narrow** assignment helpers `assign_unassigned_events_by_session_id` / `reassign_inferred_events_by_session_id` / `assign_unassigned_events_by_ids` (NOT `assign_events_by_session_id`, which only skips `'user'` and would clobber `todo_link`/`inferred` — amendment 1), the session lookups `get_agent_session` (carries `machine_id`) / `unclassified_session_ids`, the recheck helpers `record_classification` / `get_recheck_candidates` / `mark_rechecked`, the classifier-health writers `record_classifier_success` / `record_classifier_failure`, `insert_stream`, and the pre-step helper `crate::commands::classify::build_unassigned_window_runs` (amendment 2).
- Produces:

```rust
pub struct AutoClassifyOutcome { pub assigned: u64, pub proposed: u64, pub skipped: u64, pub errors: u64 }
pub fn run_auto(db: &Database, config: &Config, classifier: &dyn Classifier) -> Result<AutoClassifyOutcome>;
```

Config additions (figment defaults → TOML → `TT_*`): `classifier.model` (default `"claude-haiku-4-5"`), `classifier.confidence_threshold` (0.8), `classifier.api_key_env` (`"ANTHROPIC_API_KEY"`), plus (used by later tasks) `wip_limit` (4), `drift_window_min` (90), `serve.port` (8765), `ingest_interval_s` (30), `sync_interval_s` (60).

Resolver logic per spec (order matters):
1. Candidates = `unclassified_session_ids()` minus sessions with a pending proposal (`get_pending_proposal_for_session`). Sessions fully covered by `user`/`todo_link` have no `stream_id IS NULL` events, so they don't appear.
2. Build `ClassificationInput` from `get_agent_session(sid)` — it returns the `tt_core::session::AgentSession` **and** its `machine_id` (which `agent_sessions_in_range` drops), giving `starting_prompt`, `user_prompts`, `cwd`/`project_path`, `machine`. Roster from `get_streams()` (+descriptions).
3. Call classifier. `Existing` + confidence ≥ threshold → `assign_unassigned_events_by_session_id(sid, stream, "inferred")` (fills only NULL events). `Existing` + below → skip if `has_rejected_proposal(sid, stream)`, else `insert_proposal`. `New` + ≥ threshold → skip if `has_rejected_new_stream_proposal(sid)`, else create stream (uuid, name, description, `needs_recompute: true`) then `assign_unassigned_events_by_session_id(sid, new_id, "inferred")`. `New` + below → proposal with `proposed_new_stream` JSON.
4. Classifier error → count, `record_classifier_failure`, log at `warn`, continue (failure posture: stay unclassified).
5. **Window runs**: `build_unassigned_window_runs(db, start, end)` (the pre-step helper — synthesizes `WindowRun`s over `window_focus` events whose `stream_id IS NULL`); classify each with `window_titles` populated (empty prompts); same gate; existing-stream assignments via `assign_unassigned_events_by_ids(run.event_ids, stream, "inferred")`; event-level proposals deduped via `get_pending_proposal_for_events(run.event_ids)`.
6. **Re-checks**: `record_classification(sid, prompt_count)` on every inferred assignment; each run, `get_recheck_candidates()` whose current `user_prompts.len()` exceeds the recorded count get re-classified once via `reassign_inferred_events_by_session_id` (touches only `inferred` rows — `user`/`todo_link` are structurally safe). Call `mark_rechecked(sid)` regardless of outcome.
7. On a run with ≥ 1 successful classify, `record_classifier_success(now)`. `db_version` bumps happen **inside** the assignment/proposal/stream helpers (amendment 3) — no manual `bump_db_version()` here. Print outcome summary.

- [ ] **Step 0 (pre-step): Expose window-run construction (amendment 2)** — in `commands/classify.rs`, make `WindowRun` and its fields `pub(crate)` and add `pub(crate) fn build_unassigned_window_runs(db: &tt_db::Database, start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Vec<WindowRun>>` that loads events in range, keeps only `window_focus` with `stream_id IS NULL`, and reuses the existing private `synthesize_window_runs`. No behavior change to `tt classify` output; add a small test that a NULL-stream window run is returned and an assigned one is not.
- [ ] **Step 1: Failing tests** (MockClassifier returning scripted outputs): high-confidence existing → events assigned `inferred` via the narrow helper; low-confidence → proposal row, events still NULL; rejected existing pairing not re-proposed; rejected new-stream not re-proposed (`has_rejected_new_stream_proposal`); high-confidence new → stream created with description + events assigned; a `todo_link`- or `user`-assigned event is **never** overwritten (the narrow helpers only touch NULL/`inferred` — reuse the protection-test pattern from `classify.rs:1687`); recheck reclassifies only `inferred` events and calls `mark_rechecked`; a total classifier failure records `record_classifier_failure`.
- [ ] **Step 2:** FAIL → **Step 3:** implement → **Step 4:** PASS.
- [ ] **Step 5: Wire CLI** — `tt classify --auto` constructs `RigClassifier::from_config` and calls `run_auto`; print outcome. Follow `cli.rs` + `mod.rs` dispatch conventions (see `tag.rs` for the simple pattern).
- [ ] **Step 6:** Full `cargo test`, clippy/fmt, commit `feat(classify): confidence-gated auto classification via tt-llm`.

---

### Task 5: `tt streams describe` + `--backfill`

**Files:**
- Modify: `crates/tt-cli/src/commands/streams.rs` (or `streams/` submodule following existing structure), `crates/tt-cli/src/cli.rs` (StreamsAction::Describe)
- Test: same file

**Interfaces:** consumes `set_stream_description` (Task 2) and `Classifier::describe_stream` (Task 3; classifier construction reused from Task 4). Produces CLI only:
`tt streams describe <stream-ref> "<text>"` (ref = id | slug | exact name, same resolution as `streams slug`) and `tt streams describe --backfill [--apply]`.

Backfill: for every stream with `description IS NULL`, assemble its recent evidence (last ~10 session starting-prompts + window titles from its events), call `Classifier::describe_stream(evidence)` (defined on the trait in Task 3 — amendment 8, not added ad hoc here) for a 1-2 sentence description. Default prints `slug: proposed description` for review; `--apply` writes them via `set_stream_description`. Deterministic part unit-tested with `MockClassifier` (script its `descriptions` queue); evidence assembly asserted structurally.

- [ ] **Step 1:** failing test for manual describe (sets column, errors on unknown ref) → **Step 2:** FAIL → **Step 3:** implement manual path → **Step 4:** PASS → **Step 5:** backfill path with mock test → **Step 6:** commit `feat(streams): describe command + LLM-assisted backfill`.

---

### Task 6: `tt proposals` CLI

**Files:**
- Create: `crates/tt-cli/src/commands/proposals.rs`
- Modify: `cli.rs`, `commands/mod.rs`
- Test: in-file + insta snapshot for `ls` output

**Interfaces:** consumes Task 2 accessors + `assign_events_by_session_id` / `assign_events_by_ids`.

`tt proposals ls` — table: short id, age, session/event count, proposed stream (or `new: <name>`), confidence, reasoning (truncated 80 chars). `tt proposals accept <id>` — in one transaction: create stream first if `proposed_new_stream`; assign events with source **`user`** (accepting = user confirmation, protected); set status accepted; mark affected stream `needs_recompute` (the assignment/proposal/stream helpers bump `db_version` internally — amendment 3, no manual call). `tt proposals reject <id> [--stream <ref>]` — with `--stream`: assign to that stream as `user`; without: status rejected, events stay NULL (rejection memory via Task 2).

- [ ] **Step 1:** failing tests (accept assigns `user` + creates new stream when proposed; reject leaves NULL + `has_rejected_proposal` true; reject --stream assigns `user`) → **Step 2:** FAIL → **Step 3:** implement → **Step 4:** PASS + snapshot `ls` → **Step 5:** commit `feat(proposals): ls/accept/reject`.

---

### Task 7: Drift engine + `tt status` verdict

**Files:**
- Create: `crates/tt-cli/src/drift.rs`
- Modify: `crates/tt-cli/src/commands/status.rs` (verdict section prepended to existing output), `crates/tt-cli/src/commands/todo.rs` (add the `top_todo_view` pre-step helper — see Step 0), `crates/tt-cli/src/lib.rs` (`pub mod drift` — daemon reuses it)
- Test: `drift.rs` `mod tests` + status snapshot update

**Interfaces:**
- Consumes: `allocate_for_period` (Task 1), the pre-step helper `crate::commands::todo::top_todo_view(config, today)` (wraps the **private** `commands::todo::view` — `TodoView` is `pub` but `mod view` is private at todo.rs:18, so the crate-root `drift.rs` cannot reach it directly — amendment 9), `tt_core::todos::{compute_drift, priority_rank}` for priority ranking (public in tt-core; do NOT reimplement), `get_proposals(Some(Pending))`, `list_machines` (lib.rs:1608 — **not** `get_machines`; amendment 10), `get_classifier_health` (amendment 14).
- Produces:

```rust
#[derive(Debug, Clone, serde::Serialize)]   // Verdict + every nested type below derive Serialize (amendment 10)
pub struct Verdict {
    pub current_stream: Option<CurrentStream>,   // {stream_id, name, since: DateTime<Utc>}
    pub top_todo: Option<TopTodo>,               // {id, text, stream_slug: Option<String>}
    pub aligned: Option<bool>,                   // None when either side unknown
    pub wip: WipStatus,                          // {in_flight: Vec<StreamActivity>, limit: u32, wind_down_candidate: Option<String>}
    pub alignment_share: Option<f64>,            // direct-time share on top-priority work in window
    pub pending_proposals: u64,
    pub machines: Vec<MachineFreshness>,         // {label, last_sync_at}
    pub classifier: tt_db::ClassifierHealth,     // {last_success_at, last_error, consecutive_failures} (amendment 14)
}
pub fn compute_verdict(db: &Database, config: &Config, now: DateTime<Utc>) -> Result<Verdict>;
```

Semantics (spec, drift-engine section): current stream = stream of the latest focus-ish event (`tmux_pane_focus`/`window_focus`) within the attention window; `since` = start of the contiguous run on that stream. In-flight = streams appearing in `allocate_for_period(now - drift_window, now, Some(now))` with direct or delegated ms > 0. Aligned = current stream matches the top todo's linked stream (top todo + its `stream_slug` come from `top_todo_view`; resolve the slug with `get_stream_by_slug`; unlinked top todo → `aligned: None`, surfaced as "top todo has no stream link"). Wind-down candidate = in-flight stream with the weakest priority linkage, ranked from `top_todo_view`'s `priorities` + `stream_links` via `tt_core::todos::priority_rank` (no access to `todo/drift.rs` internals needed). Machine freshness maps `list_machines()` → `MachineFreshness { label, last_sync_at }`. Classifier health comes from `get_classifier_health`. **No new attribution semantics — everything goes through `allocate_for_period`.**

- [ ] **Step 0 (pre-step): Expose `top_todo_view` (amendment 9)** — in `commands/todo.rs` (which can see the private `mod view`), add `pub(crate) fn top_todo_view(config: &Config, today: NaiveDate) -> Result<TopTodoView>` returning `TopTodoView { top: Option<TopTodo>, priorities: Vec<tt_core::todos::Priority>, stream_links: Vec<tt_core::todos::StreamPriorityLink> }`, built by reusing `TodoView::from_loaded` (top = `main.first()`) + the existing `pub(super)` `priority_items` / `stream_links`. Do **not** make `mod view` public. `TopTodo { id, text, stream_slug: Option<String> }` is the `Verdict` field type (define once in `drift.rs`; the helper returns `drift::TopTodo`).
- [ ] **Step 1:** failing tests with fixture events: aligned-true case, drifting case, WIP over limit picks lowest-priority candidate, empty DB → all-None verdict.
- [ ] **Step 2:** FAIL → **Step 3:** implement → **Step 4:** PASS.
- [ ] **Step 5:** `tt status` renders the verdict block above existing content:

```
NOW   hawk eval infra — 38m   ⚠ not top priority
TOP   ship dashboard spec (tt-dash)
WIP   5/4 — consider winding down: legion-triage
      3 proposals pending · devbox 32s · rpi 4m
      classifier ok · last run 2m ago          (or: ⚠ classifier failing — 3× since 14:02)
```

`cargo insta review` for the status snapshot (intentional change). **Step 6:** commit `feat(status): priority verdict via drift engine`.

---

### Task 8: `tt-server` crate — the daemon

**Files:**
- Create: `crates/tt-server/Cargo.toml` (deps: tt-cli, tt-db, tt-core, tt-llm, tokio, axum 0.8, tracing), `crates/tt-server/src/main.rs` (binary `tt-serve`), `crates/tt-server/src/lib.rs`, `crates/tt-server/src/loops.rs`, `crates/tt-server/src/api.rs`, `crates/tt-server/src/sse.rs`
- Create: `config/tt-serve.service` (copy structure from `config/tt-watcher.service`)
- Modify: root `Cargo.toml` members
- Test: `loops.rs` unit tests; e2e in Task 9

**Interfaces:**
- Consumes: `tt_cli::drift::compute_verdict`, and **quiet, count-returning** entry points refactored from the existing CLI code (amendment 13): `ingest::index_sessions` (currently `-> Result<()>`, ingest.rs:438) gains a sibling `index_sessions_quiet(db) -> Result<IngestReport>` (`{ claude: usize, opencode: usize }`, no stdout; the CLI keeps printing by wrapping it); `sync::run` (currently `-> Result<()>` and prints, sync.rs:14) gains `sync_all(db, &[String]) -> Result<SyncReport>` (per-machine counts, no stdout; `tt sync` wraps it). Also `classify_auto::run_auto`, `get_db_version`, `get_classifier_health`. Both quiet variants insert via `insert_events`, so they advance `db_version` automatically.
- Produces: binary `tt-serve` (separate binary like `tt-watcher`, keeps main `tt` musl-static and tokio-free); HTTP `GET /api/status` → `Verdict` as JSON; `GET /api/sse` → SSE events `status_changed {}`, `events_appended {count}`, `heartbeat {}` every 30s.

Structure (single process, spec architecture): tokio main with three interval loops + one watcher —
1. **Ingest loop** (30s): open a fresh `Database`, `spawn_blocking` → `index_sessions_quiet` (Claude + OpenCode scan, same path as `tt ingest sessions`), drop the connection; a non-zero total arms the classify debounce.
2. **Sync loop** (60s): fresh `Database` per tick → `spawn_blocking` → `sync_all` for all `list_machines()` remotes; the connection is dropped before the next tick; per-machine failures log `warn` and back off (skip next 5 ticks after a failure — simple counter, no exponential machinery).
3. **Classify loop**: after any tick that ingested/imported > 0 events, debounce 5s, then `spawn_blocking` → open a fresh `Database` + `run_auto` with a shared `RigClassifier` (`Send + Sync`); on `Err` from `run_auto` itself, `record_classifier_failure`; drop the connection. Errors: log, continue.
4. **db_version watcher** (2s): open a fresh `Database`, read `get_db_version`, drop it; on change broadcast `events_appended` + recompute verdict (fresh `Database`) → broadcast `status_changed`.

DB access (amendment 11 + coordinator decision): **no `Arc<Mutex<Database>>` and no r2d2 in Phase 1.** `tt_db::Database` is `Send` but not `Sync`; each blocking task/tick opens its **own** `Database::open(&db_path)` at the top of its `spawn_blocking` closure and drops it before any subsequent SSH/LLM work — so a DB handle is never held across `.await` and never shared between threads (structural, not by discipline). Single-user load makes per-tick connections cheap; the r2d2 pool arrives with the read-heavy dashboard in Phase 2. Set a short `busy_timeout` on each connection so a concurrent external writer (`tt-watcher`, or the e2e test's direct insert) can't trip `SQLITE_BUSY`. SSE: `tokio::sync::broadcast` capacity 1024, `Lagged` → send `resync_required`.

- [ ] **Step 1:** failing loop tests: classify-debounce (two rapid db_version bumps → one `run_auto` call, mock classifier), sync-backoff (failing remote skipped for 5 ticks), watcher broadcasts on external write (**temp-file DB, not `:memory:`** — the fresh-connection pattern means separate connections must share a file; a second connection bumps `db_version` and the watcher must observe it).
- [ ] **Step 2:** FAIL → **Step 3:** implement lib + loops → **Step 4:** PASS.
- [ ] **Step 5:** axum router (`/api/status`, `/api/sse`) + `main.rs` (clap: `--port`, `--db`; tracing-subscriber init; graceful shutdown on ctrl-c).
- [ ] **Step 6:** `config/tt-serve.service` (After=network.target, `ExecStart=%h/.local/bin/tt-serve`, Restart=on-failure — mirror tt-watcher.service).
- [ ] **Step 7:** workspace `cargo test` + clippy/fmt; commit `feat(server): tt-serve daemon — ingest/sync/classify loops, status API, SSE skeleton`.

---

### Task 9: End-to-end test + deploy script

**Files:**
- Create: `crates/tt-server/tests/e2e_serve.rs`
- Modify: `scripts/deploy-remote.sh` (also build/copy `tt-serve`)

**Interfaces:** consumes the `tt-serve` binary (spawn via `Command` on an ephemeral port with a temp-dir DB, pattern from `tt-cli/tests/e2e_flow.rs`).

- [ ] **Step 1:** failing e2e: spawn `tt-serve --port 0 --db <tmp>` (parse bound port from stdout — add a `listening on` line in Task 8 if missing); `GET /api/status` → 200 with null-ish verdict; **insert a focus event by opening the same temp-file DB from the test and calling `db.insert_events(&[focus_event])` directly** (amendment 13). This is deliberate: `tt ingest pane-focus` writes `events.jsonl`, not the DB, and the daemon's ingest loop only scans agent sessions (not JSONL), so a JSONL event would never reach the DB; the direct insert also exercises the `db_version` mutation contract (`insert_events` bumps it), so the watcher fires. Assert SSE delivers `events_appended` within 5s and `/api/status` then reflects a current stream. (Temp-**file** DB, not `:memory:`, so the daemon's fresh connections and the test share state.)
- [ ] **Step 2:** FAIL → **Step 3:** fix wiring until PASS (no sleeps > 100ms in a poll loop; 10s hard timeout).
- [ ] **Step 4:** deploy script: build `tt-serve` release, copy alongside `tt`, print systemd enable instructions.
- [ ] **Step 5:** Full workspace gates: `cargo test && cargo clippy --all-targets && cargo fmt --check && cargo deny check`. Commit `test(server): e2e daemon coverage + deploy`.

---

## Phase-1 exit gate (from the spec — verify manually before calling Phase 1 done)

Run the daemon for a real workday: `tt status` names what you're doing within ~a minute of a switch; misattributions fixable in one command (`tt proposals accept` / `tt classify --apply`); classifier health visible. **Done means Sami tries it and finds it useful.** Only then plan Phase 2 (dashboard SPA) — informed by classifier quality observed here.

---

## Revision log (oracle review)

Applied `.superpowers/sdd/oracle-plan-review.md` (14 amendments + 2 coordinator decisions). Where each landed:

1. **Classifier assignment helpers** — Task 2 interface adds `assign_unassigned_events_by_session_id` / `assign_unassigned_events_by_ids` (first-pass, `stream_id IS NULL`) and `reassign_inferred_events_by_session_id` (recheck, `assignment_source = 'inferred'`); Task 4 Consumes + resolver steps 3/5/6 use them instead of `assign_events_by_session_id` (which only skips `'user'`).
2. **Expose window-run construction** — Task 4 Files + new Step 0 pre-step: `WindowRun` → `pub(crate)` and `build_unassigned_window_runs` in `commands/classify.rs`, consumed by the resolver (step 5).
3. **`db_version` mutation contract** — Task 2 prose enumerates the bump-on-`count>0`-inside-the-method rule (insert_events, all assignment helpers, proposal/stream/machine-sync mutations) and the no-bump bookkeeping set; Task 4 step 7 drops the manual end-of-run bump; Task 6 accept path likewise; Task 9 e2e leans on it.
4. **Recheck/proposal interface** — Task 2 adds `mark_rechecked`, `get_pending_proposal_for_events` (event-level), `has_rejected_new_stream_proposal` (new-stream rejection memory).
5. **Session lookup + machine_id** — Task 2 adds `get_agent_session(session_id) -> (AgentSession, machine_id)` and `unclassified_session_ids`; Task 4 step 2 builds `ClassificationInput.machine` from them (`agent_sessions_in_range` drops the column, lib.rs:1429).
6. **tt-llm trait/rig** — Task 3 interface: `Classifier: Send + Sync`, `MockClassifier` with `Mutex<VecDeque<…>>` (× 2 queues), `ClassificationExtract` deriving `Deserialize + Serialize + schemars::JsonSchema`; `schemars` added to Tech Stack + Task 3 Cargo.toml.
7. **Behavior/golden-set prompt tests** — Task 3 Test line + Steps 1–4 replace the prose insta snapshot with structural assertions (roster inclusion, truncation, instruction substrings) and recorded-response/mock fixtures; the `insta review` bless step is removed.
8. **`describe_stream` in Task 3** — added to the `Classifier` trait (single mockable interface); Task 5 Interfaces + backfill now *consume* it rather than defining it ad hoc.
9. **Drift-engine access to todo ranking** — Task 7 Files + new Step 0 pre-step expose `pub(crate) top_todo_view(config, today)` from `commands/todo.rs` wrapping the private `mod view`; Consumes/Semantics updated; `mod view` stays private.
10. **Drift DB names + serialization** — Task 7 Consumes/Semantics use `list_machines` (not `get_machines`); `Verdict` + nested types derive `serde::Serialize`.
11. **No DB lock across SSH/LLM** — Task 8 DB-access paragraph rewritten to a fresh `Database` per blocking task/tick (no `Arc<Mutex<Database>>`, no r2d2 — coordinator decision); Tech Stack + loop bullets updated; watcher/e2e tests switched to temp-file DBs (`:memory:` can't be shared across connections).
12. **`tt serve` vs `tt-serve`** — standardized on the separate `tt-serve` binary (coordinator decision); plan Goal line + design `## CLI additions` (design:319) amended.
13. **Precise loop/e2e inputs** — Task 8 Consumes specifies `index_sessions_quiet -> IngestReport` and `sync_all -> SyncReport` refactors; Task 9 Step 1 specifies direct `insert_events` on a temp-file DB (not `tt ingest pane-focus` JSONL) and explains why.
14. **Classifier health in status/verdict** — Task 2 adds meta-backed `ClassifierHealth` + `get/record_classifier_*`; Task 4 records success/failure; Task 7 `Verdict.classifier` field rendered in `tt status`; Task 8 daemon records failures and serves it via `/api/status`.

Coordinator decisions: (12) separate `tt-serve` binary — applied to plan + design; (11) fresh `Database` connection per blocking task/tick, no `Arc<Mutex>`/r2d2 — applied to Task 8 + Tech Stack.

No amendment was judged wrong; all 14 applied as specified.

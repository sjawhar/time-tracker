# Classification Layer Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace raw SQL in LLM skills with a `tt classify` command that correctly assigns events to streams, fixing split-session bugs and delegated time undercount.

**Architecture:** New `classify` CLI command with two modes (show unclassified data, apply assignments). Fixes to `context` output, `ingest` auto-assignment, and allocation algorithm. Events remain the atomic unit; sessions are a convenience grouping for bulk assignment.

**Tech Stack:** Rust (tt-cli, tt-core, tt-db). Clap for CLI. serde for JSON. SQLite via rusqlite.

**Design doc:** `specs/design/2026-02-27-classification-layer-design.md`

---

## Task 1: Add `tt classify` CLI skeleton

**Files:**
- Create: `crates/tt-cli/src/commands/classify.rs`
- Modify: `crates/tt-cli/src/commands/mod.rs`
- Modify: `crates/tt-cli/src/cli.rs`
- Modify: `crates/tt-cli/src/main.rs`

**Step 1:** Add `classify` module to `commands/mod.rs`:
```rust
pub mod classify;
```

**Step 2:** Add `Classify` variant to `Commands` enum in `cli.rs`:
```rust
/// Classify events into streams.
///
/// Show unclassified sessions and events, or apply LLM-proposed
/// stream assignments.
Classify {
    /// Apply assignments from JSON file or stdin ("-").
    #[arg(long, value_name = "FILE")]
    apply: Option<String>,

    /// Only show unclassified events (no stream_id).
    #[arg(long)]
    unclassified: bool,

    /// Compact summary (one line per session/cluster).
    #[arg(long)]
    summary: bool,

    /// Output as JSON.
    #[arg(long)]
    json: bool,

    /// Start of time range (ISO 8601 or relative like "2 days ago").
    #[arg(long)]
    start: Option<String>,

    /// End of time range (ISO 8601, defaults to now).
    #[arg(long)]
    end: Option<String>,
},
```

**Step 3:** Create `classify.rs` with stub functions:
```rust
pub fn run_classify_show(...) -> Result<()> { todo!() }
pub fn run_classify_apply(...) -> Result<()> { todo!() }
```

**Step 4:** Add dispatch in `main.rs` matching the pattern of other commands.

**Step 5:** Verify it compiles: `cargo build`

**Step 6:** Commit: `feat(cli): add tt classify command skeleton`

---

## Task 2: Implement `tt classify` show mode — sessions

**Files:**
- Modify: `crates/tt-cli/src/commands/classify.rs`
- Modify: `crates/tt-db/src/lib.rs` (if new queries needed)
- Test: `crates/tt-cli/src/commands/snapshots/` (snapshot tests)

**Step 1:** Write a test that creates a DB with agent sessions and events, runs classify show, and snapshots the output.

**Step 2:** Run test to verify it fails.

**Step 3:** Implement `run_classify_show`:
- Parse time range (reuse `parse_time_range` from `context.rs`)
- Query agent_sessions in range from DB
- For each session: session_id, source, project_path, start/end, duration, summary/starting_prompt (truncated to 120 chars), tool_call_count, user_prompt_count
- If `--unclassified`: filter to sessions whose events have no stream_id
- If session's project_path matches a prior classification, show proposed_stream
- Output as table (default) or JSON (`--json`)

**Step 4:** Run tests, verify pass.

**Step 5:** Commit: `feat(classify): show mode for sessions`

---

## Task 3: Implement `tt classify` show mode — non-session events

**Files:**
- Modify: `crates/tt-cli/src/commands/classify.rs`

**Step 1:** Write test: DB with tmux_pane_focus events (no session_id), verify they appear in output.

**Step 2:** Run test to verify it fails.

**Step 3:** Extend `run_classify_show` to include non-session events:
- Query events in range where session_id IS NULL
- Group by CWD + time clusters (events within 30min of each other at same CWD = one cluster)
- For each cluster: CWD, time range, event count, event types
- If `--unclassified`: filter to events with no stream_id
- Append to output after sessions section

**Step 4:** Run tests, verify pass.

**Step 5:** Commit: `feat(classify): show non-session event clusters`

---

## Task 4: Implement `--summary` flag for classify

**Files:**
- Modify: `crates/tt-cli/src/commands/classify.rs`

**Step 1:** Write test with snapshot for summary output.

**Step 2:** Implement compact output:
- Sessions: one line per session: `{id_short} | {project_name} | {duration} | {tools} tools | {summary_truncated}`
- Clusters: one line per cluster: `{cwd_short} | {time_range} | {count} events`

**Step 3:** Run tests, verify pass.

**Step 4:** Commit: `feat(classify): add --summary compact output`

---

## Task 5: Define and parse the `--apply` JSON format

**Files:**
- Modify: `crates/tt-cli/src/commands/classify.rs`

**Step 1:** Define serde structs for the apply input:
```rust
#[derive(Deserialize)]
struct ClassifyApplyInput {
    #[serde(default)]
    streams: Vec<StreamDef>,
    #[serde(default)]
    assign_by_session: Vec<SessionAssignment>,
    #[serde(default)]
    assign_by_pattern: Vec<PatternAssignment>,
}

#[derive(Deserialize)]
struct StreamDef {
    name: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct SessionAssignment {
    session_id: String,
    stream: String,  // stream name (matched or created)
}

#[derive(Deserialize)]
struct PatternAssignment {
    cwd_like: String,
    #[serde(default)]
    start: Option<String>,
    #[serde(default)]
    end: Option<String>,
    stream: String,
}
```

**Step 2:** Write test: parse valid JSON input, verify structs populated.

**Step 3:** Write test: parse from file path and from stdin ("-").

**Step 4:** Implement reading from file or stdin in `run_classify_apply`.

**Step 5:** Run tests, verify pass.

**Step 6:** Commit: `feat(classify): parse --apply JSON format`

---

## Task 6: Implement `--apply` stream creation and tagging

**Files:**
- Modify: `crates/tt-cli/src/commands/classify.rs`
- Modify: `crates/tt-db/src/lib.rs` (add `get_stream_by_name` if not exists)

**Step 1:** Write test: apply input with new stream definitions, verify streams created with tags.

**Step 2:** Implement stream resolution in `run_classify_apply`:
- For each stream in `streams[]`: check if name exists, create if not, apply tags
- Build name→id mapping for use by assignments

**Step 3:** Run tests, verify pass.

**Step 4:** Commit: `feat(classify): create streams and apply tags on --apply`

---

## Task 7: Implement `--apply` session assignment

**Files:**
- Modify: `crates/tt-cli/src/commands/classify.rs`
- Modify: `crates/tt-db/src/lib.rs` (add `assign_events_by_session` method)

**Step 1:** Write test: create events with session_id, apply session assignment, verify ALL events for that session get stream_id (including events outside the time window).

**Step 2:** Write test: verify events with `assignment_source = 'user'` are NOT overwritten.

**Step 3:** Add DB method:
```rust
/// Assign all events for a session to a stream.
/// Skips events with assignment_source = 'user'.
pub fn assign_events_by_session(
    &self,
    session_id: &str,
    stream_id: &str,
) -> Result<u64>
```

**Step 4:** Call from `run_classify_apply` for each `assign_by_session` entry.

**Step 5:** Run tests, verify pass.

**Step 6:** Commit: `feat(classify): session-based event assignment`

---

## Task 8: Implement `--apply` pattern assignment

**Files:**
- Modify: `crates/tt-cli/src/commands/classify.rs`
- Modify: `crates/tt-db/src/lib.rs` (add `assign_events_by_pattern` method)

**Step 1:** Write test: create tmux_pane_focus events (no session_id), apply pattern assignment by CWD + time range, verify events assigned.

**Step 2:** Add DB method:
```rust
/// Assign events matching a CWD pattern and optional time range to a stream.
/// Skips events with assignment_source = 'user'.
pub fn assign_events_by_pattern(
    &self,
    cwd_like: &str,
    start: Option<&str>,
    end: Option<&str>,
    stream_id: &str,
) -> Result<u64>
```

**Step 3:** Call from `run_classify_apply` for each `assign_by_pattern` entry.

**Step 4:** Run tests, verify pass.

**Step 5:** Commit: `feat(classify): pattern-based event assignment`

---

## Task 9: Auto-recompute after `--apply`

**Files:**
- Modify: `crates/tt-cli/src/commands/classify.rs`

**Step 1:** Write test: apply assignments, verify streams have updated time_direct_ms / time_delegated_ms.

**Step 2:** After all assignments, call `recompute::run_recompute(db, true)` (force mode on affected streams).

**Step 3:** Print summary: "Assigned N events to M streams. Recomputed."

**Step 4:** Run tests, verify pass.

**Step 5:** Commit: `feat(classify): auto-recompute after apply`

---

## Task 10: Allocation algorithm — use session end_time

**Files:**
- Modify: `crates/tt-core/src/allocation.rs`
- Modify: trait `AllocatableEvent` or add session metadata parameter
- Test: existing tests in `allocation.rs` + new test

**Step 1:** Write test: agent session with known end_time, large gap between tool calls (>30min), verify delegated time spans from first_tool_use to end_time (not cut short by timeout).

**Step 2:** Run test to verify it fails (current algorithm uses timeout).

**Step 3:** Extend `allocate_time` to accept session metadata (a map of session_id → end_time). When checking for agent timeout, if session has a known end_time, use it instead of `last_tool_use + timeout`. The timeout heuristic only applies when end_time is unknown.

Design consideration: The `AllocatableEvent` trait could gain a method `fn session_end_time(&self) -> Option<DateTime<Utc>>`, or `allocate_time` could accept a `HashMap<String, DateTime<Utc>>` for session end times. The latter is simpler and avoids changing the trait.

**Step 4:** Run all allocation tests, verify pass (existing tests should not break).

**Step 5:** Update `recompute.rs` to load session end_times from DB and pass to `allocate_time`.

**Step 6:** Commit: `fix(allocation): use session end_time instead of timeout heuristic`

---

## Task 11: Split-session validation in recompute

**Files:**
- Modify: `crates/tt-cli/src/commands/recompute.rs`
- Modify: `crates/tt-db/src/lib.rs` (add query for split sessions)

**Step 1:** Write test: create events for one session split across two streams, run recompute, verify warning emitted.

**Step 2:** Add DB method:
```rust
/// Find sessions with events in multiple streams.
pub fn find_split_sessions(&self) -> Result<Vec<(String, Vec<String>)>>
// Returns (session_id, vec_of_stream_ids)
```

**Step 3:** Call before recompute, print warnings:
```
Warning: session ses_abc has events in 2 streams: "stream A", "stream B"
  Use 'tt classify --apply' to fix.
```

**Step 4:** Run tests, verify pass.

**Step 5:** Commit: `fix(recompute): warn on split sessions`

---

## Task 12: Conservative auto-assignment in ingest

**Files:**
- Modify: `crates/tt-cli/src/commands/ingest.rs` (function `auto_assign_events_to_streams`)

**Step 1:** Write test: create two streams with similar CWD patterns, ingest events with ambiguous CWD, verify they are NOT auto-assigned.

**Step 2:** Write test: ingest events with CWD matching exactly one stream, verify they ARE auto-assigned.

**Step 3:** Modify `auto_assign_events_to_streams`:
- Only assign when CWD maps to exactly one stream (no ambiguity)
- Never auto-create streams (remove any stream creation logic)
- Log skipped events at debug level

**Step 4:** Run all ingest tests, verify pass.

**Step 5:** Commit: `fix(ingest): conservative auto-assignment, no stream creation`

---

## Task 13: `tt context` — add `--unclassified` flag

**Files:**
- Modify: `crates/tt-cli/src/cli.rs` (add flag to Context variant)
- Modify: `crates/tt-cli/src/commands/context.rs`

**Step 1:** Add `--unclassified` flag to Context CLI args.

**Step 2:** Write test: events with and without stream_id, `--unclassified` only shows those without.

**Step 3:** Filter events and agents where stream_id is None when flag is set.

**Step 4:** Run tests, verify pass.

**Step 5:** Commit: `feat(context): add --unclassified filter`

---

## Task 14: `tt context` — add `--summary` flag

**Files:**
- Modify: `crates/tt-cli/src/cli.rs`
- Modify: `crates/tt-cli/src/commands/context.rs`

**Step 1:** Add `--summary` flag.

**Step 2:** Write snapshot test for summary output.

**Step 3:** When `--summary` is set, output compact format:
- Agents: one object per session with id, project, duration, tools, summary (truncated)
- Events: grouped by CWD clusters, one object per cluster with cwd, time range, count

**Step 4:** Run tests, verify pass.

**Step 5:** Commit: `feat(context): add --summary compact mode`

---

## Task 15: Update infer-streams skill

**Files:**
- Modify: `.opencode/skills/infer-streams/SKILL.md`

**Step 1:** Replace the Python/SQL assignment workflow with `tt classify` commands:
- Phase 2 uses `tt context --unclassified --summary --agents --start ...`
- Phase 4 produces JSON matching the `--apply` format
- Phase 4 runs `tt classify --apply` instead of Python scripts
- Remove all raw SQL instructions

**Step 2:** Commit: `docs(skills): update infer-streams to use tt classify`

---

## Task 16: Update daily-standup skill

**Files:**
- Modify: `.opencode/skills/daily-standup/SKILL.md`

**Step 1:** Update Phase 4 to invoke infer-streams (which now uses `tt classify`).

**Step 2:** Remove any references to raw SQL or Python event assignment.

**Step 3:** Commit: `docs(skills): update daily-standup to use tt classify`

---

## Task 17: Integration test — end-to-end classify workflow

**Files:**
- Modify: `crates/tt-cli/tests/e2e_flow.rs`

**Step 1:** Write integration test:
1. Create in-memory DB with agent sessions + tmux events
2. Run `tt classify --unclassified --json` — verify sessions and event clusters appear
3. Pipe classify JSON output through a mock "LLM" (just a transform that assigns streams)
4. Run `tt classify --apply` with the assignments
5. Verify all events assigned, no splits
6. Run `tt recompute --force`
7. Verify streams have correct time allocations

**Step 2:** Run test, verify pass.

**Step 3:** Commit: `test: end-to-end classify workflow`

**Step 4:** Run full test suite: `cargo test && cargo clippy --all-targets`

**Step 5:** Commit any fixes.

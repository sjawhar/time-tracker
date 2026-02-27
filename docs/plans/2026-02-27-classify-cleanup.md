# Classification Layer Cleanup Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Remove dead weight after adding `tt classify`. Consolidate overlapping commands, delete the old `classify-streams` skill, and update all skill references from `tt context` to `tt classify`.

**Architecture:** Deprecate `tt context` (move `parse_datetime` + gap logic to shared utils), delete the `classify-streams` skill (fully replaced by `tt classify --apply`), update skill files that reference old commands. Keep `streams list`, `streams create`, `tag`, and `recompute` as useful standalone tools — they're small and serve diagnostic/manual-override purposes.

**Tech Stack:** Rust (tt-cli). Markdown skill files.

---

### Task 1: Extract shared utilities from context.rs

`classify.rs` imports `context::parse_datetime`. Before we can deprecate context, extract the shared pieces.

**Files:**
- Create: `crates/tt-cli/src/commands/util.rs`
- Modify: `crates/tt-cli/src/commands/mod.rs`
- Modify: `crates/tt-cli/src/commands/classify.rs`
- Modify: `crates/tt-cli/src/commands/context.rs`

**Step 1:** Create `util.rs` with `parse_datetime`, `RELATIVE_TIME_RE`, and `MAX_RELATIVE_MINUTES` moved from `context.rs`.

**Step 2:** Update `mod.rs` to add `pub mod util;`

**Step 3:** Update `classify.rs` to import from `super::util::parse_datetime` instead of `super::context::parse_datetime`.

**Step 4:** Update `context.rs` to import from `super::util` instead of defining them locally.

**Step 5:** Run `cargo test -- --skip weekly_reports && cargo clippy --all-targets`

**Step 6:** Commit: `refactor: extract shared time parsing to util.rs`

---

### Task 2: Add `--gaps` flag to classify

Move the one unique feature of `context` into `classify` so nothing is lost.

**Files:**
- Modify: `crates/tt-cli/src/cli.rs` (add `--gaps` and `--gap_threshold` to Classify)
- Modify: `crates/tt-cli/src/commands/classify.rs`
- Modify: `crates/tt-cli/src/main.rs` (pass new args)

**Step 1:** Add the gap flags to the Classify CLI variant:
```rust
/// Include gaps between user input events.
#[arg(long)]
gaps: bool,

/// Minimum gap duration to include (minutes).
#[arg(long, default_value = "5")]
gap_threshold: u32,
```

**Step 2:** Add a `gaps` field to `ClassifyOutput` (optional `Vec<GapExport>`). Reuse the gap struct definition from `context.rs` (or define a simple one).

**Step 3:** In `run_show`, if `gaps` is true, compute gaps between user events the same way context.rs does (filter to user event types, sort by timestamp, find gaps > threshold).

**Step 4:** Run tests and clippy.

**Step 5:** Commit: `feat(classify): add --gaps flag (migrated from context)`

---

### Task 3: Deprecate `tt context`

**Files:**
- Modify: `crates/tt-cli/src/commands/context.rs` (add deprecation warning at top of `run`)
- Modify: `crates/tt-cli/src/cli.rs` (update help text)

**Step 1:** Add deprecation notice to the `Context` variant doc comment:
```rust
/// [DEPRECATED] Output context for stream inference (JSON).
///
/// Use `tt classify` instead, which provides the same data plus
/// stream proposals and `--apply` for assignments.
```

**Step 2:** Add a deprecation warning at the top of `context::run`:
```rust
eprintln!("Warning: `tt context` is deprecated. Use `tt classify` instead.");
eprintln!("  tt classify --json            (replaces tt context --events --agents)");
eprintln!("  tt classify --unclassified    (replaces tt context --unclassified)");
eprintln!("  tt classify --gaps            (replaces tt context --gaps)");
eprintln!();
```

**Step 3:** Run tests (existing context tests should still pass with the warning).

**Step 4:** Commit: `deprecate: mark tt context as deprecated in favor of tt classify`

---

### Task 4: Delete the `classify-streams` skill

The old `classify-streams` skill does manual `tt tag` commands one-by-one and references `ontology.toml`. It's fully replaced by `tt classify --apply` which handles stream creation, assignment, and tagging in one JSON payload.

**Files:**
- Delete: `.opencode/skills/classify-streams/SKILL.md`
- Delete: `.opencode/skills/classify-streams/` (entire directory)

**Step 1:** Verify the skill directory exists and check for any other files in it.

**Step 2:** Delete the directory.

**Step 3:** Commit: `chore: remove classify-streams skill (replaced by tt classify --apply)`

---

### Task 5: Update daily-standup skill to use classify

The daily-standup skill still references `tt context` in its workflow diagram and Phase 3.

**Files:**
- Modify: `.opencode/skills/daily-standup/SKILL.md`

**Step 1:** Update the workflow diagram to replace `tt context` with `tt classify`:
- Change `gather [label="3. Gather context\ntt context --events --agents"]` → `gather [label="3. Gather context\ntt classify --json"]`

**Step 2:** Update Phase 3 to use `tt classify`:
```bash
tt classify --json --start "$START" --end "$END"
```
Instead of `tt context --events --agents`.

**Step 3:** Update the common mistakes table: replace `tt context` references with `tt classify`.

**Step 4:** Commit: `docs(skills): update daily-standup to use tt classify`

---

### Task 6: Update weekly-review skill to use classify

**Files:**
- Modify: `.opencode/skills/weekly-review/SKILL.md`

**Step 1:** Replace all `tt context` references with equivalent `tt classify` commands:
- `tt context --events` → `tt classify --json`
- `tt context --agents --streams` → `tt classify --json`
- `tt context --streams` note about lifetime totals → still valid, update command name

**Step 2:** Commit: `docs(skills): update weekly-review to use tt classify`

---

### Task 7: Update AGENTS.md

The root `AGENTS.md` references `tt context` in the "Where to Look" table.

**Files:**
- Modify: `AGENTS.md`

**Step 1:** Check for any references to `context`, `classify-streams`, or outdated workflow descriptions.

**Step 2:** Add `classify` to the command list. Note `context` as deprecated.

**Step 3:** Commit: `docs: update AGENTS.md for classify workflow`

---

### Task 8: Final verification

**Step 1:** Run `cargo test -- --skip weekly_reports` — all tests pass.

**Step 2:** Run `cargo clippy --all-targets` — zero warnings.

**Step 3:** Run `cargo fmt --check` — no formatting issues.

**Step 4:** Grep for any remaining `tt context` references that aren't the deprecated command itself:
```bash
grep -r "tt context" --include="*.md" --include="*.rs" | grep -v "deprecated\|DEPRECATED\|context.rs\|context::"
```

**Step 5:** Verify the diff is net-negative on lines (more deletions than additions).

**Step 6:** Commit: `chore: final cleanup verification`

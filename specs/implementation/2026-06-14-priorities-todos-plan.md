# Priorities & Todos — implementation plan

**Date:** 2026-06-14
**Design:** [`specs/design/2026-06-14-priorities-todos-design.md`](../design/2026-06-14-priorities-todos-design.md)
**Status:** Draft (plan only — no implementation yet)

## Summary

Build `tt todo` / `tt priority` subcommands over a **markdown file store**. Files are the source
of truth; `tt` reads/writes them and runs the views + checks. **SQLite schema untouched** — the
drift check *reuses* `tt`'s existing time/report logic, it doesn't add tables.

Crate placement (follows existing patterns):
- **`tt-core`** — file-store model + parse/serialize, like `session.rs` / `opencode.rs`. New
  module `todos.rs` (priorities, todos, links; round-trip parse). Ordering + check logic here.
- **`tt-cli/src/cli.rs`** — `Todo(TodoAction)` and `Priority(PriorityAction)` enums.
- **`tt-cli/src/commands/todo.rs`, `priority.rs`** — dispatch + rendering; `insta` snapshots in
  `commands/snapshots/`.
- Drift reuses the existing `report` path in `tt-core`.

Phases are ordered so each is independently useful — stop after Phase 3 and there's a working,
glanceable, editable list with dates; the checks (4) and sync (6) layer on after.

## Phase 1 — File store: model + parse/serialize (`tt-core`)

**Goal:** `tt` can round-trip the three artifacts losslessly; seed from real data.

- `tt-core/src/todos.rs`: types for `Priority`, `Todo`, stream→priority link; a parser and
  serializer over the GFM line grammar (decided in Phase 1, must render cleanly in any viewer).
- Stable todo IDs (short handle) for later `done`/`rank`/`defer`.
- Seed `priorities.md` from the current `w25/priorities.md` (assign slugs + `value`).

**Verify:** unit tests — parse → serialize → parse is identity on a fixture covering every field
(`pin`, `when:`, `due:`, `quick`, multi-priority, done). `cargo test -p tt-core`.

## Phase 2 — `tt todo next` (the view)

**Goal:** the ranked now-list renders correctly, with ordering + date semantics.

- `tt todo next` in `tt-cli`. Ordering: **priority order by default; manual position/`pin`
  wins**. Hide `when:` (deferred) items from the main list (show under a `## Later` section or
  `--later`). Surface `due:`/overdue. Flags: `--top N`, `--quick`, `--json`, `--by-priority`.

**Verify:** `insta` snapshots over a fixture store — default order, a pinned override, a deferred
item hidden, an overdue item surfaced, `--quick`, `--json`. `cargo insta review`.

## Phase 3 — Mutation verbs

**Goal:** add/complete/defer/reposition + priority management, all round-tripping through the files.

- `tt todo add "<text>" [--priority <slug>…] [--stream <id>] [--due <date>] [--when <date>] [--quick] [--pin]`
- `tt todo done <id>` · `tt todo defer <id> <date>` · `tt todo rank <id> --top|--above|--below` · `tt todo ls`
- `tt priority add "<title>" --value N` · `tt priority value <slug> N` · `tt priority ls` · `tt priority done <slug>`
- `tt stream link <stream> <priority>`

**Verify:** e2e in `tt-cli/tests/` (spawn the binary, temp store): add → appears at right rank;
done → drops; defer → moves to Later; rank → reorders; multi-priority add resolves links.

## Phase 4 — Checks (alignment + drift)

**Goal:** deterministic, agent-free checks that the order matches priorities and that time matches importance.

- `tt todo check` — **alignment**: report todos whose manual position diverges from priority
  order; mark `pin`ned ones as deliberate (not flagged). Logic in `tt-core`.
- `tt todo drift` — **time vs. importance**: reuse the `report` path for per-stream time, join to
  priorities via the stream links, compare each priority's importance share vs. time share, flag
  divergences. **Never recompute time** — reuse existing logic (same principle as `infer-streams`).
  *(Decide: standalone subcommands vs. `tt todo next --check`.)*

**Verify:** unit tests on fixtures for both; then run `drift` against this week's real data and
spot-check one priority's time share against `tt report --week`.

## Phase 5 — The skill (agent layer)

**Goal:** the agent drives the commands with the right behavior.

- `.opencode/skills/todo/SKILL.md` (name mirrors the command). Encodes the contract from the
  design (tight list = `tt todo next`; faithful edits; analysis stays in chat; partner-mode ≠
  mutation; order shared / value the user's) plus standup integration (resurface due deferred
  items; write `w<NN>/<DATE>.md` plan-vs-actual archives).

**Verify:** a dry run of "what should I do" → returns `tt todo next` output, nothing else; a
faithful add/done round-trip; a standup pass resurfaces a deferred item and writes an archive.

## Phase 6 — Cross-machine sync (do last; orthogonal)

**Goal:** edits on one machine appear on the others.

**Grounded state (2026-06-14):** Syncthing is **not installed on either box**; `devbox-mx` has
only `w24` in daily-standups vs. local `w24`+`w25` (already drifted). So this is install +
headless-pair, not "share a folder." I have SSH to `devbox-mx` (`ubuntu`), so I can do it all.

- Install Syncthing on local + `devbox-mx`; headless-pair via REST/`config.xml` (no GUI); share
  `~/.local/share/time-tracker/daily-standups/` bidirectionally; one-time reconcile the existing
  drift first. SSH `tt sync` (events) untouched.

**Verify:** round-trip an edit A→B and B→A; no conflict copies on a normal single-user edit.

## Commit

One commit when you ask:
```
jj describe -m "feat: tt todo — priorities & todos over a markdown file store"
jj new
```

## Out of scope (design non-goals)

Recurrence, subtasks, GitHub sync, knowledge graph, Notion, SQLite schema changes, GUI/TUI.
Due dates and defer are **in**.

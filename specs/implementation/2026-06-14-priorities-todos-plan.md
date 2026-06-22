# Priorities & Todos — implementation plan

**Date:** 2026-06-14
**Design:** [`specs/design/2026-06-14-priorities-todos-design.md`](../design/2026-06-14-priorities-todos-design.md)
**Status:** Draft (plan only — no implementation yet; Oracle-reviewed twice)

## Summary

Build `tt todo` / `tt priority` subcommands over a **markdown file store**. Files are the source
of truth; `tt` reads/writes them and runs the views + checks. **SQLite schema untouched** — the
drift check *reuses* `tt`'s existing report/time path read-only, adds no tables.

Crate placement (follows existing patterns):
- **`tt-core/src/todos.rs`** — types for `Priority`/`Todo`/link, parser + serializer, `priority_rank`,
  and the *pure* alignment + drift arithmetic (operates on data passed in). Like `session.rs`.
- **`tt-cli/src/cli.rs`** — `Todo(TodoAction)` and `Priority(PriorityAction)` enums; `tt streams link`
  extends the existing `StreamsAction`.
- **`tt-cli/src/commands/todo.rs`, `priority.rs`** — dispatch, rendering, **ID minting** (uses `uuid`,
  already a `tt-cli` dep via machine identity → derive a short base32 id; `tt-core` has no rand dep),
  and the drift orchestration; `insta` snapshots in `commands/snapshots/`.
- **Drift orchestration is in `tt-cli`, not `tt-core`.** `generate_report_data_for_date`
  (`tt-cli/src/commands/report.rs`) is `pub` and reachable, but needs a live `Database`, `Period`,
  `generated_at`, `reference_date`, and timezone, and fetches events/sessions/streams/tags. So the
  entry point is `todo::drift(db, config, period)` (not a pure call), which gathers per-stream time
  then hands it to the pure `tt-core` drift function. `main.rs` must pass `Config` (not just
  `Database`) into the todo command branch. We do **not** move report-data into tt-core for v1.

Phase 1 carries the cross-cutting model decisions (grammar, IDs, `priority_rank`, ordering rule,
stream links); Phases 2+ depend on those. Stop after Phase 3 and there's a working, glanceable,
editable list with dates; checks (4) and sync (6) layer on after.

## Phase 1 — File store: model + parse/serialize (`tt-core`)

**Goal:** `tt` round-trips the three artifacts; tolerant of human edits; seeded from real data.

- `tt-core/src/todos.rs`: types for `Priority`, `Todo`, stream→priority link; parser + serializer
  over the GFM line grammar (renders cleanly in any viewer). Grammar covers `id:`, multi
  `priority:`, `stream:`, `when:`, `due:`, `pin`, `quick`, and `- [x]` = done.
- **`streams.md` keyed by stream *name*** (not UUID — UUIDs are per-machine and cleared on import);
  one priority per stream (a second link errors).
- **`priority_rank(todo)`** = max `value` among **active** linked priorities (none/only-inactive →
  below all), plus the ordering rule (line order canonical; `add` inserts by rank; no auto-resort).
- **Tolerant parse:** parse into valid entries **plus preserved raw lines** for anything malformed;
  do not crash the store. (IDs are *not* minted here — minting is a `tt-cli` `add`-time concern.)
- Seed `priorities.md` from the current `w25/priorities.md` (assign slugs + `value`).

**Verify (`cargo test -p tt-core`):** (a) parse → serialize → parse is identity on a fixture
covering every field (`id`, `pin`, `when:`, `due:`, `quick`, multi-priority, stream link, done);
(b) a malformed line is preserved verbatim and surfaced as a diagnostic, not dropped or crashed;
(c) `priority_rank` cases — multi, none, **only-inactive**, tie.

## Phase 2 — `tt todo next` (the view)

**Goal:** the now-list renders correctly with ordering, sections, and date semantics.

- `tt todo next` in `tt-cli`. Renders `todos.md` in **line order** (canonical; `next` never
  re-sorts). A separate **"Due" section** at top holds due/overdue items (canonical order within).
  Hide `when:` (deferred) items from the main list (under `## Later` / `--later`). `due:` overrides
  `when:`. Apply the pin algorithm (lift pins, evaluate non-pinned, reinsert at fixed indices).
- Preflight-scan the store for `*.sync-conflict-*` and error if any exist.
- Flags: `--top N`, `--quick`, `--json`, `--by-priority`, `--later`.

**Verify (`cargo insta review`):** snapshots over a fixture store — default order, pinned override,
deferred item hidden, a due item in the Due section, a both-deferred-and-due item still surfaced,
`--quick`, `--json`; plus a conflict-file-present error case.

## Phase 3 — Mutation verbs

**Goal:** add/complete/defer/reposition + priority + stream-link management, round-tripping the files.

- `tt todo add "<text>" [--priority <slug>…] [--stream <name>] [--due <date>] [--when <date>] [--quick] [--pin]`
  — mints the `id`, inserts at the `priority_rank` position.
- `tt todo done <id>` · `tt todo defer <id> <date>` · `tt todo rank <id> --top|--above|--below` ·
  `tt todo ls` · `tt todo normalize-ids` (the *only* read-path that may add missing IDs).
- `tt priority add "<title>" --value N` · `tt priority value <slug> N` · `tt priority ls` · `tt priority done <slug>`
- `tt streams link <stream-name> <prio-slug>` (extends `StreamsAction`).
- **No silent rewrites:** read-only commands never write IDs; only the mutation touching a todo, or
  `normalize-ids`, adds them.

**Verify:** e2e in `tt-cli/tests/` (spawn binary, temp store via `TT_*`/`XDG_DATA_HOME`): add →
appears at right rank with a fresh id; done → drops; defer → Later; rank → reorders; `ls` on a store
with a hand-added id-less todo does **not** rewrite the file; multi-link stream link → errors.

## Phase 4 — Checks (alignment + drift)

**Goal:** deterministic, agent-free checks that order matches priorities and time matches importance.

- `tt todo check` — **alignment**: report non-pinned todos out of `priority_rank` order; warn on
  todos linked only to inactive/missing priorities; pinned ones reported as deliberate. Pure `tt-core`.
- `tt todo drift` — **time vs. importance** via `todo::drift(db, config, period)`:
  - call `generate_report_data_for_date` for per-stream time;
  - **fix the boundary bug first:** report periods are documented half-open but the DB query is
    inclusive (`timestamp <= ?2`, `tt-db/src/lib.rs`), so midnight events double-count. Use `< end`
    (or end − 1ms) on the path drift consumes.
  - join stream **name** → priority via `streams.md` (error on unresolved/ambiguous);
  - hand to a pure `tt-core` fn computing, per active priority, importance share vs. time share —
    **both direct-only and direct+delegated**; unlinked stream time → an **"unattributed" bucket**.

**Verify:** deterministic `tt-core` unit tests over synthetic report data + fixture priorities —
both time lenses, the unattributed bucket, an inactive priority, an unresolved stream name. Then one
real-data smoke run of `drift`, spot-checked against `tt report --week`.

## Phase 5 — The skill (agent layer)

**Goal:** the agent drives the commands with the right behavior.

- `.opencode/skills/todo/SKILL.md`. Encodes the design contract (tight list = `tt todo next`;
  faithful edits; analysis stays in chat; partner-mode ≠ mutation; order shared / value the user's),
  standup integration (resurface due deferred items; write `w<NN>/<DATE>.md` plan-vs-actual
  archives), and conflict-file awareness (surface, don't paper over).

**Verify:** "what should I do" → returns `tt todo next` output, nothing else; a faithful add/done
round-trip; a standup pass resurfaces a deferred item and writes an archive.

## Phase 6 — Cross-machine sync (do last; orthogonal)

**Goal:** edits on one machine appear on the others.

Syncthing is **not installed on either box**; the machines have already drifted. I have SSH to
`devbox-mx` (`ubuntu`), so I can do install + headless-pair from here.

- Install Syncthing on local + `devbox-mx`; headless-pair via REST/`config.xml` (no GUI); share
  **only** `~/.local/share/time-tracker/daily-standups/` bidirectionally.
- **`.stignore`** excluding `tt.db`, `tt.db-wal`, `tt.db-shm`, `events.jsonl`, `machine.json` — never
  touch the files SSH `tt sync` owns.
- **Reconcile by preflight inventory + backup of both sides**, then a one-way merge with local as
  authoritative — do not assume which weeks exist. SSH `tt sync` (events) untouched.

**Verify:** round-trip an edit A→B and B→A; on a normal single-user edit, no `*.sync-conflict-*`
files; and confirm `tt todo` commands refuse to run if a conflict file is ever present.

## Commit

One commit when you ask:
```
jj describe -m "feat: tt todo — priorities & todos over a markdown file store"
jj new
```

## Out of scope (design non-goals)

Recurrence, subtasks, GitHub sync, knowledge graph, Notion, SQLite schema changes, configurable
timezone, GUI/TUI. Due dates and defer are **in**.

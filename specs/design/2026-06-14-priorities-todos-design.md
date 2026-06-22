# Priorities & Todos — design

**Date:** 2026-06-14
**Status:** Draft (brainstormed; Oracle-reviewed twice; pending final review)

## Problem

`tt` tracks where time *went*, but there's no record of what *matters most* or what to do next. A week of running priorities out of ad-hoc markdown + a single agent surfaced three failures:

- **No single referenceable list.** To know what to work on, the user had to ask the agent and wade through its interpretation — there was never a place to just *look*.
- **Agent noise.** "What should I do now?" produced ~13 items, several of them "don't do X," mixed with misreads.
- **Drift.** Priorities got muddled across the week instead of staying straight.

The time-tracker's purpose is to keep the user vectored on the highest-impact work. That needs a durable, glanceable record of priorities + next actions, and a way to check that *time spent* matches *stated importance*.

## What this is

A small `tt` feature: **`tt todo`** / **`tt priority`** subcommands over a **markdown file store**.

- **Files are the source of truth** (the user's call): human-readable, `cat`/`grep`/`Edit`-friendly, diffable, Syncthing-able. Proven — the past week already ran on markdown.
- **`tt` holds the code** that reads/writes those files and runs the views + checks. The **SQLite schema is untouched** — these commands operate on files; the drift check *reuses* `tt`'s existing report/time path (read-only).
- **A skill sits on top** as the agent behavior layer (faithful edits, the noise-fix contract, standup integration).

What's genuinely `tt`-unique is the tie to tracked time: the **priority-alignment** and **time-drift** checks. The rest is generic todo-app mechanics we're choosing to build, with eyes open.

## Model

- **Priority** — a ranked objective. Fields: `slug`, `title`, `value` (scalar importance, user-set), `status` (active/done/dropped), optional `note`. Importance is a **scalar value**, not an ordinal position. Lives in `priorities.md`.
- **Todo** — a concrete action. Fields: `id` (short base32, minted by `tt todo add`), `text`, relations to **one or more priorities** (direct and/or via its stream), `status` (open/done), optional `when:<date>` (defer), optional `due:<date>` (deadline), optional `pin` (deliberate placement), optional `quick`. **Position is the line's order in `todos.md`** — no separate position field. Lives in `todos.md`.
- **Stream** (already in `tt`) — linked to a priority. Links live in `streams.md`, one line per link, **keyed by stream *name*, not DB UUID** (UUIDs are minted fresh per machine and cleared on import, so they don't survive sync — names are the stable cross-machine identity). **v1: a stream maps to exactly one priority** (a second link errors). A todo's `stream:` contributes that stream's priority on top of any direct `priority:` links.

## Ordering

**The file's line order is the canonical order** — `tt todo next` prints `todos.md` top-to-bottom (deferred/due handling below). Priority values never continuously re-sort; they define a *reference* order used only by `add` and `check`:

- **`priority_rank(todo)` = the *maximum* `value` among the *active* priorities it serves** (direct + via stream). A todo linked to no active priority ranks below all linked todos. Max, not sum.
- **On `add`**, the new line is inserted at the position implied by its `priority_rank` (descending). Nothing auto-resorts afterward.
- **`pin` / `check` algorithm (mechanical):** lift pinned lines out, evaluate the *non-pinned subsequence* against descending `priority_rank`, then reinsert pinned lines at their fixed indices for rendering. `add` inserts into the non-pinned subsequence and projects back into the file. `check` flags non-pinned lines out of rank order, and **warns on todos linked only to inactive/missing priorities**. Pinned lines are reported as deliberate, never nagged.

So manual line order always wins and sticks; priority order is only the yardstick `add` and `check` use.

## Dates

**v1 uses system-local time (`chrono::Local`)** — documented, not configurable. The user's machines run America/Panama, so local time is correct in practice; a configurable timezone is deferred. (Note: `tt report`'s `--json` `timezone` field is cosmetic today — boundaries are already computed with `Local` — so there's nothing to "reuse" there; this is a known pre-existing wart we are not fixing here.)

Bare dates = local calendar days.

- **`when:` — defer ("surface on Thursday").** Hidden from the main `tt todo next` list until the *start* of that local day; held in a `## Later` section. At standup the agent resurfaces items whose day has come.
- **`due:` — deadline ("done by Thursday").** Overdue after the *end* of that local day.
- **Rendering:** due/overdue items appear in a **separate "Due" section** at the top of `next`, preserving canonical line order *within* each section. This is sectioning, not a re-sort of the main list. A todo that is both deferred and `due:`/overdue still appears in the Due section — **`due:` overrides `when:`** (a snooze never hides an overdue item).

## Drift (the time loop)

For each **active** priority, compare:

- **importance share** = its `value` ÷ Σ values of active priorities.
- **time share** = time on its linked streams ÷ total tracked time.

Reported **two ways side by side**: a **direct-only** lens (where your attention went) and a **direct+delegated** lens (where the work went). A large gap = drift (over- or under-served).

- Time is per-stream (from the report path); joined to priorities via `streams.md` **by stream name**. Unresolved/ambiguous stream names error loudly.
- **v1: one priority per stream** (no splitting/double-counting).
- Stream time linked to **no** priority is shown as an explicit **"unattributed" bucket** — itself a signal (off-list work), not silently dropped.
- Surfaced on request (`tt todo drift`), never as list clutter.

## Commands (`tt`, over the files; SQLite untouched)

- `tt todo next` — **the view**: ranked now-list + a "Due" section. Flags: `--top N`, `--quick`, `--json`, `--by-priority`, `--later`.
- `tt todo add "<text>" [--priority <slug>…] [--stream <name>] [--due <date>] [--when <date>] [--quick] [--pin]`
- `tt todo done <id>` · `tt todo defer <id> <date>` · `tt todo rank <id> …` · `tt todo ls` · `tt todo normalize-ids`
- `tt todo check` — alignment · `tt todo drift` — importance vs. tracked time *(may fold into `next --check`/`--drift`)*
- `tt priority add "<title>" --value N` · `tt priority value <slug> N` · `tt priority ls` · `tt priority done <slug>`
- `tt streams link <stream-name> <prio-slug>` — **extends the existing `tt streams` command**, not a new `tt stream` namespace.

The checks are **deterministic and runnable without the agent**. Every command preflight-scans the store for Syncthing conflict files (`*.sync-conflict-*`) and errors if any exist, so divergent edits are never silently ignored.

## Storage layout

Under `~/.local/share/time-tracker/daily-standups/`:

- **Current (live):** `priorities.md`, `todos.md`, `streams.md` at the root — what you edit and what `tt todo next` reads.
- **Archives (reflection):** dated `w<NN>/<DATE>.md` snapshots of *planned vs. actually accomplished*, written by the standup / weekly-review skills at boundaries. No daemon.

## The skill (agent layer)

A thin skill on top of the commands:

1. **One tight ranked list.** "What should I do" → `tt todo next`. Never 13 items, never "don't-do X."
2. **Faithful edits only.** The user's words; no editorializing in item text.
3. **The list stays clean.** Context, analysis, caveats → conversation, never stored items.
4. **Partner mode ≠ mutation.** When asked to think/prioritize, the agent reads the list + `tt todo drift`/`check` and reasons in chat; it doesn't change the list unless told.
5. **Order is shared; value is the user's.** The agent may reposition todos; it never sets a priority's `value` unless asked.
6. **Standup integration.** Resurface deferred items whose day has come; write the day/week archive snapshots.

## Sync

**Syncthing** for the file store — continuous, peer-to-peer, offline-tolerant, no server. Not yet installed on either machine; setup (install + headless-pair over SSH) is the last, orthogonal step.

- A `.stignore` excludes `tt.db`, `tt.db-wal`, `tt.db-shm`, `events.jsonl`, `machine.json` — Syncthing must never touch the files the SSH `tt sync` owns. Share only `daily-standups/`.
- First-time reconcile via a preflight inventory + backup of both sides (local is authoritative), not a hardcoded assumption about which weeks exist.
- The existing SSH `tt sync` (events) is untouched.

## Non-goals (YAGNI)

- Recurrence / reminders / notifications.
- Subtasks.
- GitHub issue/PR sync.
- A knowledge graph relating todos to context.
- **Notion** — out for v1; if ever wanted, a *one-way* files→Notion mirror, never bidirectional.
- **SQLite schema changes** — commands touch files only.
- **Configurable timezone** — v1 is system-local.
- A separate GUI/TUI.

*(Due dates and defer are explicitly **in**, per the user.)*

## Resolved decisions

- **Timezone** — v1 = system-local (`chrono::Local`); the report's cosmetic tz field is not relied on.
- **Ordering / pin** — line order canonical; `priority_rank` = max *active* linked `value`; mechanical lift-pins/evaluate/reinsert algorithm.
- **Todo IDs** — short base32, minted by `tt todo add`. **No silent rewrites:** read-only commands never write; missing IDs are minted only by a mutation touching that todo, or by explicit `tt todo normalize-ids`.
- **Stream links** — `streams.md`, keyed by stream **name**; one priority per stream in v1.
- **Drift** — importance share vs. time share, both **direct** and **direct+delegated**; unattributed bucket; join by stream name.
- **Dates** — system-local calendar days; due/overdue in a separate section; `due:` overrides `when:`.
- **Robustness** — tolerant parse: valid entries + preserved raw lines; reads continue with diagnostics; writes refuse only when they can't preserve a malformed line byte-for-byte. Conflict-file preflight on every command.
- **`quick`** — powers only the `--quick` filter; no sort/schedule effect.

## Open (deferred to implementation)

- Exact byte-level GFM line grammar (tag order/delimiters) — must render cleanly in any viewer.
- Whether `check`/`drift` are standalone subcommands or `next --check`/`--drift`.

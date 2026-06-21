# Priorities & Todos — design

**Date:** 2026-06-14
**Status:** Draft (brainstormed; pending review)

## Problem

`tt` tracks where time *went*, but there's no record of what *matters most* or what to do next. A week of running priorities out of ad-hoc markdown + a single agent surfaced three failures:

- **No single referenceable list.** To know what to work on, the user had to ask the agent and wade through its interpretation — there was never a place to just *look*.
- **Agent noise.** "What should I do now?" produced ~13 items, several of them "don't do X," mixed with misreads.
- **Drift.** Priorities got muddled across the week instead of staying straight.

The time-tracker's purpose is to keep the user vectored on the highest-impact work. That needs a durable, glanceable record of priorities + next actions, and a way to check that *time spent* matches *stated importance*.

## What this is

A small `tt` feature: **`tt todo`** subcommands over a **markdown file store**.

- **Files are the source of truth** (the user's call): human-readable, `cat`/`grep`/`Edit`-friendly, diffable, Syncthing-able. Proven — the past week already ran on markdown.
- **`tt` holds the code** that reads/writes those files and runs the views and checks. The **SQLite schema is untouched** — these commands operate on files, not the events DB; the drift check *reuses* `tt`'s existing time data.
- **A skill sits on top** as the agent behavior layer (faithful edits, the noise-fix contract, standup integration).

What's genuinely `tt`-unique here is the tie to tracked time: the **priority-alignment** and **time-drift** checks. The rest (todos, ordering, dates) is generic todo-app mechanics we're choosing to build rather than adopt, with eyes open.

## Model

- **Priority** — a ranked objective. Fields: `slug`, `title`, `value` (scalar importance, user-set), `status` (active/done/dropped), optional `note`. Importance is a **scalar value**, not an ordinal position. Lives in `priorities.md`.
- **Todo** — a concrete action. Fields: `text`, relations to **one or more priorities** (directly and/or via its stream), `status` (open/done), explicit **position**, optional `pin` (manual-position override), optional `when:<date>` (defer), optional `due:<date>` (deadline), optional `quick` flag. Lives in `todos.md`.
- **Stream** (already in `tt`) — linked to a priority. The link lives in the file store (a field/line), not the DB.

## Ordering

Both auto and manual, by design:

- **Default = priority order.** A todo's natural rank comes from the `value` of the priorities it serves. Add a todo and it slots into priority order automatically.
- **Manual placement wins and sticks.** Reposition a todo (`pin`) and that placement is respected, regardless of priority order.
- **Alignment check.** `tt` flags where a *manual* placement diverges from what priority-order would produce — so you can catch an override that's gone stale. Pinned todos are reported as deliberate, not nagged.

## Dates

Two distinct concepts, both supported:

- **`when:` — defer ("surface on Thursday").** Hidden from `tt todo next` until the date; held in a `## Later` section. At standup the agent resurfaces items whose day has come.
- **`due:` — deadline ("done by Thursday").** Drives overdue / due-soon surfacing, and can pull an otherwise-low item up when it's about to be late.

## Commands (`tt`, over the files; SQLite untouched)

- `tt todo next` — **the view**: the ranked now-list. Flags: `--top N`, `--quick`, `--json`, `--by-priority`. This is the glanceable answer to "what should I do."
- `tt todo add "<text>" [--priority <slug>…] [--stream <id>] [--due <date>] [--when <date>] [--quick] [--pin]`
- `tt todo done <id>` · `tt todo defer <id> <date>` · `tt todo rank <id> …` (reposition) · `tt todo ls`
- `tt todo check` — alignment (order vs. priority) · `tt todo drift` — importance vs. tracked time *(may fold into `next --check`)*
- `tt priority add "<title>" --value N` · `tt priority value <slug> N` · `tt priority ls` · `tt priority done <slug>`
- `tt stream link <stream> <priority>`

The checks are **deterministic and runnable without the agent** — that's what "double-check that my ordering matches my priorities" needs.

## Storage layout

Under `~/.local/share/time-tracker/daily-standups/`:

- **Current (live):** `priorities.md`, `todos.md` at the root — what you edit day-to-day and what `tt todo next` reads.
- **Archives (reflection):** dated `w<NN>/<DATE>.md` snapshots of *planned vs. actually accomplished*, written by the standup / weekly-review skills at day/week boundaries. No daemon — archiving happens when you run those.

## The skill (agent layer)

A thin skill on top of the commands:

1. **One tight ranked list.** "What should I do" → `tt todo next`. Never 13 items, never "don't-do X."
2. **Faithful edits only.** The user's words; no editorializing baked into item text.
3. **The list stays clean.** Context, analysis, caveats → conversation, never stored items.
4. **Partner mode ≠ mutation.** When asked to think/prioritize, the agent reads the list + `tt todo drift`/`check` and reasons in chat; it doesn't change the list unless told.
5. **Order is shared; value is the user's.** The agent may reposition todos; it never sets a priority's `value` unless asked.
6. **Standup integration.** Resurface deferred items whose day has come; write the day/week archive snapshots.

## Sync

**Syncthing** for the file store — continuous, peer-to-peer, offline-tolerant, no server. Not yet installed on either machine; setup (install + headless-pair over SSH) is the last, orthogonal step. The existing SSH `tt sync` (events) is untouched.

## Non-goals (YAGNI)

- Recurrence / reminders / notifications.
- Subtasks.
- GitHub issue/PR sync.
- A knowledge graph relating todos to context.
- **Notion** — out for v1. If ever wanted, a *one-way* files→Notion mirror; never bidirectional.
- **SQLite schema changes** — the new commands touch files only.
- A separate GUI/TUI.

*(Due dates and defer are explicitly **in**, per the user.)*

## Open (implementation-level)

- Exact file grammar (inline tags) — must stay valid GFM that renders cleanly in any viewer.
- Whether `check`/`drift` are subcommands or flags on `next`.
- Stream→priority link storage (inline on the stream vs. a small `streams.md`).
- ID scheme for todos (stable handle for `done`/`rank`/`defer`).

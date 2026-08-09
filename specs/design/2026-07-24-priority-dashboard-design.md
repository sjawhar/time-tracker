# Priority Dashboard: Live Attribution + Always-On Alignment Display

Supersedes `2026-05-27-dashboard-design.md`. That spec designed a timeline
visualization for verifying and correcting time allocation. This one re-anchors
the same rendering architecture around a different mission and adds the
subsystem that makes it possible: live stream attribution.

## Mission

Help Sami always work on the top priority. The dashboard is an always-on
instrument (dedicated portrait monitor, watched all day) that continuously
answers:

1. **What am I doing right now?** — named, attributed to a stream, live.
2. **What should I be doing?** — the top priority, from the existing
   `tt todo` / `tt priority` ranking.
3. **Should I stop something?** — too many things in flight, or sustained time
   on low-priority work.

When the answer to (1) and (2) diverge, the display makes it visceral. The
correct "next thing" is sometimes *nothing new* — finish or stop something
instead.

**Definition of done (user's words): "I try it and find it useful. Anything
before that is not done. I'm going to put it up on a screen and watch it all
day long."** All checklists below are intermediate gates, not the finish line.

## Why this needs a new subsystem

The mission requires knowing what the user is doing *right now*, attributed to
a stream. Today attribution has three sources:

- `user` — explicit assignments. Protected, never overwritten. Live.
- `todo_link` — sessions linked to todos via `tt todo link`. Deterministic. Live.
- `inferred` — everything else, via the manual `tt classify` LLM loop in a
  chat session. High-friction, batch, runs ~daily at best.

Everything not user-assigned or todo-linked is unclassified until the manual
loop runs. A live display built on that would be mostly gray. **Live semantic
classification is therefore a prerequisite, and it is independently valuable**
— it kills the classify-loop friction even if the dashboard never ships.

## Decisions made during design (with rationale)

| Decision | Choice | Why |
|---|---|---|
| Todo integration | Integrated from day one | The mission *is* priority alignment; a pure visualization misses the point |
| Form factor | One responsive web surface, portrait-first | Dedicated portrait monitor; degrades gracefully when sharing the screen. No separate widget app, no tiny-strip mode |
| Attribution basis | Semantic content of agent sessions, plus context signals (cwd, window titles) | Directories host many streams over time; session content is what actually identifies the work. Todo links are the deterministic fast path |
| Trust contract | Confidence-gated: auto-assign when sure, propose when not | Wrong silent guesses pollute the ambient display; constant confirmations recreate the friction we're killing |
| New streams | Same confidence gate as assignments | "Always propose" would nag on clear novel work; "always create" would hallucinate streams |
| Drift signals | Time-weighted misalignment AND WIP count (configurable limit) | Captures both "wrong thing" and "too many things" failure modes |
| LLM harness | `rig` crate (rig-core), direct Anthropic API, pinned version | Researched Jul 2026: headless agent CLIs (claude -p etc.) have documented production failure modes (silent output loss, session-discarding bugs, no failure taxonomy). rig is the mature Rust-native option: typed agent loop, structured output, `Result`-shaped failures. No official Anthropic Rust SDK exists |
| Daemon shape | One process, `tt serve`: ingest + sync + classify + drift + dashboard | Single-user tool; one systemd unit, one log. Classifier failures degrade to "unclassified", never crash the server |
| Phasing | Attribution engine first, dashboard second | Classifier quality is the core bet; validate it cheaply via CLI before building UI on top |

## Architecture

```
                        ┌─────────────────────────────────────────┐
                        │  tt serve  (systemd user service)       │
  tmux hooks ──┐        │                                         │
  tt-watcher ──┼──────► │  1. Ingest loop    (sessions, ~30s)     │
  (write to db │        │  2. Sync loop      (remotes, ~60s)      │
   directly)   │        │  3. Classify loop  (resolver + rig)     │
               │        │  4. Drift engine   (rolling allocation) │
      ▼        │        │  5. Dashboard      (axum + SSE + SPA)   │
   tt.db  ◄────┴───────►│                                         │
      ▲                 └────────────┬────────────────────────────┘
      │                              │ SSE / HTTP (localhost)
  tt CLI (report, todo,              ▼
  classify, status,            browser (portrait monitor)
  proposals)
```

### Ingest loop

What `tt ingest sessions` does today, in-process on a timer (~30s): scan
Claude session JSONL and the OpenCode SQLite DB for new/updated sessions.
tmux hooks and tt-watcher keep writing to the DB directly as today — the
daemon notices via the `meta.db_version` counter.

### Sync loop

Runs the existing SSH pull (`tt sync` logic) against all known machines on a
timer (~60s, configurable). Unreachable machines back off quietly. Facts
verified against the current code:

- Export carries **full session metadata including `starting_prompt` and
  `user_prompts`** for Claude and OpenCode sessions — the classifier has
  semantic content for remote work. Classification runs once, on the hub.
- Import clears `stream_id`/`assignment_source` only on **incoming** event
  objects; `INSERT OR IGNORE` preserves existing rows. Periodic sync and the
  classifier do not fight: new events arrive unclassified and enter the
  pipeline.
- Remote "now" is only as fresh as the last successful sync; the UI must show
  per-machine staleness rather than pretending.

The 2026-03-24 automatic-sync design (S3 hub, any-machine-is-hub) remains a
separate future effort. This daemon automates today's SSH pull; it does not
change the sync architecture. Todos/priorities continue to sync via Syncthing
(markdown store) — unchanged, conflict-file preflight stays.

### Attribution engine (resolver chain)

Every event/session gets its stream from the first layer that answers:

1. **`user`** — explicit assignment. Sacred (existing behavior).
2. **`todo_link`** — session linked to a todo with a known stream (existing).
3. **`inferred`** — semantic classifier, auto-committed only above a
   confidence threshold (config, default 0.8). Correctable via normal
   reassignment — that is the undo path.
4. **`proposed`** — below threshold: stored in the `proposals` table, shown
   in UI/CLI for one-keystroke confirm. **Excluded from allocation and drift
   math** until accepted. Pending proposals register as "unclassified
   pressure" on the display.

Classifier mechanics:

- **Trigger**: after each ingest/sync tick, debounced, over new unclassified
  sessions and window-focus runs.
- **Input**: session starting prompt + recent user prompts + cwd + machine,
  alongside the stream roster (slug, name, **description**, tags, recent
  activity summary). Window-focus runs (browser etc.) classify on title
  batches through the same pipeline.
- **Agent loop**: rig, max ~3 turns, with tools to fetch more session content
  or a stream's recent sessions when ambiguous. Typed output:
  `{stream: existing-slug | new-stream, confidence, reasoning}`.
- **New streams** pass the same confidence gate: clear novel work auto-creates
  (marked `inferred`, undoable); ambiguous novel work becomes a proposal.
- **Re-checks**: a session classified early gets one re-check after more
  prompts accumulate. `user`/`todo_link` assignments are never touched.
- **Rejection memory**: rejected proposals persist so the same pairing is not
  re-proposed.
- **Failure posture**: API errors → events stay unclassified, retry with
  backoff, classifier health visible in UI/status. Never crashes the daemon.

### Drift engine

**No new attribution semantics.** It runs the same `allocate_for_period`
(below) over a rolling recent window (config `drift_window_min`, default 90)
where direct attention went against priority values from the todo/priority
store. Working with remote agents through an SSH pane is direct time on that
stream because the remote machine's tmux focus events say so — same algorithm
as `tt report`, shorter window.

Outputs:

- **Verdict**: current stream (from latest focus events), duration on it,
  aligned/drifting vs the top priority.
- **Alignment**: rolling-window share of direct time on top-priority work;
  sustained misalignment escalates the display.
- **WIP**: count of in-flight streams — any direct attention or agent
  activity within the drift window — vs
  the configurable limit; over limit → suggest winding down the in-flight
  stream with the weakest priority linkage.

### Allocation contract (carried over, still required)

A single `tt-db::allocate_for_period(db, period)` wrapper (tt-db, not tt-core:
tt-core cannot depend on tt-db's `Database`) populates
`session_end_times` and `session_types` from `agent_sessions` and calls
`allocate_time` with explicit `period_end`. Callers: `tt report`,
`tt recompute`, the drift engine, and `/api/timeline`. Numbers cannot diverge
by construction. Note: the superseded spec described an empty-`session_end_times`
bug in `report.rs`; that was fixed before this design (report now mirrors
recompute). The wrapper is a pure consolidation refactor — no behavior change,
existing snapshot tests must stay green.
`report.rs`; reported numbers will change (documented in release notes).

### Backend stack

Carried over from the superseded spec (research still current): `axum` 0.8 +
built-in SSE, `tokio`, `rust-embed` (filesystem in debug, embedded in
release), `r2d2`+`r2d2_sqlite` pool with `spawn_blocking` for every DB call,
single serialized writer, `broadcast` channel for SSE fan-out, `db_version`
polling for external-write detection. Forbidden patterns and CI grep
enforcement as previously specified. New: `rig-core` (pinned) + a Haiku-class
model for classification.

### Frontend stack

Carried over: Svelte 5 (runes, pinned `^5.48`), Vite, TypeScript, d3-scale /
d3-zoom / d3-brush / d3-quadtree, @tanstack/virtual, schemeTableau10 palette,
@floating-ui/dom. Canvas (ribbons/dots) + SVG (axis/labels/markers) + HTML
(rail, palette, tooltips) layering.

## Visual design

Portrait-first. Two regions: a ~260px **left rail** (the cockpit) and the
**timeline** filling the rest, full height.

### Rail, top to bottom

1. **Verdict card** — what you're doing now, for how long, colored: green =
   top priority, red = drifting (e.g. "hawk eval infra · 38 min ·
   not your top priority").
2. **Top priority card** — what you should be on (`tt todo next` #1) + time
   it received today.
3. **WIP card** — in-flight streams vs limit; over limit names the
   wind-down candidate.
4. **Next** — top ~3 todos from the same ranking the CLI prints; expandable.
   Each todo shows how many agent sessions are currently linked to it
   ("hawk #88 · 2 agents running") — the todo list doubles as a map of
   deployed delegation.
5. **Agent sessions** — one row per running session across all machines:
   harness, stream, machine, duration, and the linked todo's text. Amber =
   gone quiet. Unlinked = dashed row with one-keystroke link action. (Seed of
   the future agent-management surface.)
6. **Proposals** — pending classifier guesses with reasoning; accept/reject
   inline.
7. **Machines** — per-remote sync freshness ("devbox 32s · rpi 4m").

### Timeline

The work-trial-timeline anatomy, live: time flows down, **top = now**, streams
as columns with badges in a sticky strip. Within a column: solid fill = direct
attention, faded = delegated, 1px centerline = alive but dormant, dots =
user messages, ▲/▼ session start/end markers with a link glyph when the
session is todo-linked, subagent ticks, PR badges in a gutter (via `gh`,
TTL-cached), hatched idle folds (fixed height, click to expand). Proposed
assignments render as **ghost segments** — hatched, translucent, dashed
border — in the proposed stream's column. New events slide in at the top;
scrolled-away state pauses auto-follow with an "N new events" pill.
Piecewise-linear time scale (idle compression), zoom via Ctrl+wheel, viewport
state in URL hash.

Column order: by `first_event_at`, stable while scrolling (columns never
rearrange). Stream colors: schemeTableau10 by index, per-stream override in
`streams.color`.

### Responsive behavior

No bespoke compact mode. The rail is sacred and always renders intact. The
timeline degrades: less height = less visible history; less width = columns
compress, then least-recently-active streams collapse to centerlines. Below
~500px width the rail alone fills the window.

### Keyboard-first + command palette (hard constraint)

Every action reachable without a mouse; anything not keyboard-reachable is a
bug. `j/k` time cursor, `h/l` stream focus, `Enter` open, `gg`/`G` now/oldest,
`/` filter, `y/n` proposals, `r` reassign, `m` merge, `s` split, `L` link
session→todo, `Esc` dismiss. **Ctrl+K command palette** fuzzy-matches every
command and entity (streams, todos, sessions); operations with arguments
prompt for them with pickers in the palette. No modal dialogs. Destructive
ops confirm inline in the palette (`Enter` to confirm) + 5s undo toast.
Mouse remains as a convenience layer (drag-select ranges).

## Edit operations

Kept for MVP: **reassign** (drag-select or palette → target stream),
**rename**, **retag**, **recolor**, **merge**, **split** (events after a time
point move to a new stream), **session↔todo link/unlink**, **proposal
accept/reject**. Transactional semantics for reassign/merge/split as
specified in the superseded spec (unchanged). **Delete is dropped** — its
events must go somewhere, so it is merge by another name.

Accepting a proposal writes `user` source (protected). Rejecting returns
events to unclassified and records the rejection. Every mutation bumps
`db_version`, broadcasts SSE, and schedules a single-flight background
recompute over the affected period.

## Data model changes (additive only)

```sql
-- Classifier substrate: what each stream is about
ALTER TABLE streams ADD COLUMN description TEXT;

-- Per-stream color override (NULL = palette index)
ALTER TABLE streams ADD COLUMN color TEXT;

-- External-write detection
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
INSERT INTO meta VALUES('db_version', '0');

-- Confidence-gated classifier proposals
CREATE TABLE proposals (
    id TEXT PRIMARY KEY,
    created_at TEXT NOT NULL,
    session_id TEXT,              -- session-level proposal, or
    event_ids TEXT,               -- JSON array for event-level proposals
    proposed_stream_id TEXT,      -- NULL when proposing a new stream
    proposed_new_stream TEXT,     -- JSON {name, description, tags} when new
    confidence REAL NOT NULL,
    reasoning TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'  -- pending | accepted | rejected
);
```

`SCHEMA_VERSION` bumps; older supported versions migrate forward in `init()`
per project convention. Existing-stream description backfill: an assisted
batch pass (LLM drafts from stream history, user skims) shipped as
`tt streams describe --backfill`.

Config additions (`config.toml` / `TT_*`): `wip_limit`, `drift_window_min`,
`classifier.model`, `classifier.confidence_threshold`, `classifier.api_key_env`,
`sync_interval_s`, `ingest_interval_s`, `serve.port`.

## API surface

Reads: `/api/status` (verdict, top priority, WIP, alignment, proposal count,
classifier health, machine freshness), `/api/timeline?before=&duration=`
(ribbons + point events + ghosts + idle gaps, same shape as the superseded
spec plus proposals), `/api/todos`, `/api/sessions` (active, with links),
`/api/streams`, `/api/proposals`, `/api/prs`.

Writes: `PATCH /api/streams/:id` (rename/retag/recolor/describe),
`POST /api/streams/:id/merge`, `POST /api/streams/:id/split`,
`POST /api/events/assign`, `POST /api/proposals/:id/accept|reject`,
`POST /api/sessions/:id/link` (todo link/unlink).

SSE: `connected`, `events_appended`, `stream_*`, `proposal_*`,
`status_changed` (verdict/WIP/alignment deltas), `recompute_*`,
`resync_required`, `heartbeat`.

## CLI additions

- `tt-serve [--port N]` — the daemon binary (systemd unit `tt-serve.service`,
  pattern follows `tt-watcher.service`).
- `tt status` — the rail as text: verdict, top priority, WIP, proposals,
  machine freshness. Ships in Phase 1, before any UI.
- `tt classify --auto` — one-shot classifier run (Phase 1b validation
  surface; the daemon calls the same code path).
- `tt proposals ls|accept|reject` — terminal proposal handling.
- `tt streams describe <ref> ["text"] | --backfill` — descriptions.

## Phasing

Each phase lands independently useful. Sequencing reshuffles before scope
cuts.

**1a — Allocation wrapper** (days): `allocate_for_period` extracted, callers
migrated. Pure refactor, zero behavior change (snapshots stay green). Lands
first so the drift engine and dashboard build on the single entry point.

**1b — Classifier, on demand** (~1–2 wk): `tt-llm` crate (rig, pinned),
`streams.description` + backfill, resolver chain, proposals table,
`tt classify --auto`, `tt proposals`, `tt status` (static computation).
The core bet validated cheaply: run it by hand, watch its judgment, tune
prompt/tools/threshold before it goes always-on.

**1c — Daemon** (~1 wk): `tt serve` with ingest/sync/classify loops, drift
engine, SSE plumbing, systemd unit. `tt status` becomes live. Gate: it names
what you're doing within ~a minute of a switch, and misattributions are one
command away from fixed.

**2 — Dashboard** (~3–4 wk): SPA skeleton + SSE wiring → timeline rendering
(all ribbon states, points, folds, ghosts, PR markers, virtualized scroll) →
rail → edit ops + palette + keyboard map. Gate: the dogfood — it lives on the
portrait monitor and gets watched all day; standup prep without `tt report`;
at least one real drift catch that changed behavior; auto-assignment
corrections < ~1/day (else tune thresholds toward proposing).

## Testing

- **tt-llm**: golden-set tests — replayed real session snippets + stream
  roster fixtures with expected assignments; runs against recorded responses
  (no live API in CI). Confidence-gate boundary tests.
- **Daemon**: loop tests with in-memory DB (ingest tick, sync tick with a
  fake remote command, classify tick with a stub model). Single-flight
  recompute; SSE multi-subscriber + `Lagged` → `resync_required`;
  `db_version` external-write detection.
- **API**: per-handler tests with `Database::open_in_memory()`; snapshot
  tests for `/api/timeline` and `/api/status` JSON.
- **Frontend**: Vitest component tests (palette, rail, proposals);
  time-scale unit tests (piecewise idle compression); Playwright
  pixel-snapshots for canvas rendering.
- **E2E**: spawn `tt serve` on an ephemeral port; write events; assert SSE +
  status changes; drive edit ops via HTTP.

## Non-goals (this project)

- Automatic-sync redesign (S3 hub) — separate effort, daemon uses SSH pull.
- Steering-vs-light message classification; verbatim per-event hover text
  (v2 `event_text` table); orchestrator session sub-spans
  (`session_stream_spans`, v3) — as in the superseded spec.
- Multi-user/hosted; mobile.
- Agent management (start/stop/steer agents from the dashboard) — the rail's
  session list is deliberately its seed, but no control actions in MVP.

## Known risks

- **Classifier judgment is the core bet.** Mitigated by Phase 1b's on-demand
  validation period and the confidence gate; worst case the gate ratchets
  toward "propose" and the dashboard still beats today's manual loop.
- **rig ships breaking changes** most minor versions. Pinned; bumps are
  deliberate.
- **API cost/availability**: classification calls are small (Haiku-class,
  pennies/day); failure posture is "stay unclassified, retry".
- **Async discipline** in a formerly-sync codebase: the forbidden-pattern CI
  greps from the superseded spec apply.

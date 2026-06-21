# tt Dashboard

A persistent, live, browser-based interface to the time-tracker. Visualizes where direct/delegated time is allocated per stream over a continuous timeline, with inline edit affordances for stream assignment. Replaces `tt report`'s aggregate-number-only output as the primary human-facing surface for consuming tt data.

## Problem

The current `tt report` produces aggregate numbers (`Nh direct / Mh delegated` per stream) that consistently feel wrong on heavy-parallel-agent days. Tuning `attention_window_ms` has been whack-a-mole: PR #13 capped intervals to prevent inflation, PR #15 bumped 1min→5min ("underattributed"), PR #41 fixed subagent contamination — and direct time still feels too high every standup.

The deeper problem isn't the algorithm — the algorithm faithfully computes what it was designed to compute. The problem is:

1. **No way to verify the numbers.** You can argue about whether 12h of direct time is "right" but you can't see WHERE it came from per stream over time.
2. **No way to fix misattributions inline.** Wrong stream assignment requires dropping to CLI, writing `classify --apply` JSON, and running recompute. Friction prevents calibration.
3. **No way to spot bugs.** The recent finding that long-running sessions emit duplicate `agent_session(ended)` events — silently killing delegated tracking for build-graph #11416 — was invisible in the numbers but would have been obvious in a timeline view.

## Goals

- Continuous-time visualization of per-stream activity, scrollable through arbitrary history
- Inline edit operations: rename / retag / recolor / delete / merge / split / reassign events
- Live updates: events from `tt ingest sessions` / `tt sync` in other terminals appear without refresh
- Time numbers match `tt report` exactly (same `tt-core::allocate_time` code path)
- Distributed as part of the single `tt` binary — no separate runtime, no `npm install` at user-side

## Non-goals (MVP)

- **LLM-narrative attribution for orchestrator sessions** (one session driving multiple logical streams). Deferred to v3 via `session_stream_spans` table.
- **Verbatim per-event prompt text on hover.** MVP uses what's already in `agent_sessions.starting_prompt` + `user_prompts[]` (truncated). Full text needs a v2 `event_text` table + extended sync.
- **Steering-vs-light user_message dot classification.** Requires LLM classifier. v2.
- **Multi-user / hosted.** Single-user, local-only.
- **Mobile / responsive.** Desktop-first, <1024px wide will not render usefully.
- **Undo/redo stack.** Toast-based 5s undo for inline ops + modal confirmation for destructive ops is MVP.

## Constraints

### Hard

- Single self-contained `tt` binary. `cargo build --release` produces a working dashboard — no separate file-serving step.
- Dashboard, `tt report`, and `tt recompute` all call a single new `tt-core::allocate_for_period` wrapper that populates `session_end_times` and `session_types` from `agent_sessions` and runs `allocate_time` with explicit `period_end`. Numbers cannot diverge by construction. Scope includes fixing `report.rs`'s current empty-`session_end_times` bug as part of building the wrapper. (The duplicate-`ended`-events bug is tracked separately and not in this project's scope.)
- No breaking schema changes. Two additive changes for MVP: `meta` table + nullable `streams.color` column.
- All data local. SSE over `localhost`. No external services.

### Soft

- 10–30 concurrent streams + several days of history should render at 60fps during scroll.
- Avoid bleeding-edge frameworks; maintain-for-years posture.
- Sync with the project's existing conventions (Rust workspace lints, `anyhow` in CLI, `thiserror` in libs).

## Architecture

### Process model

```
   user@laptop
        │
        ├─ tt dashboard ──────► axum server (localhost:8765)
        │                          │
        │                          ├─ HTTP/SSE
        │                          │
        │                          ▼
        ▼                       ~/.local/share/time-tracker/tt.db
   browser (Svelte 5 SPA)
   ◀───────────────────────────────┘
              SSE
```

`tt dashboard [--port N] [--no-browser]` starts the server, optionally opens the browser to `http://localhost:8765/`. Server keeps running until Ctrl-C. Frontend assets are embedded into the binary at build time via `rust-embed`; in debug builds, `rust-embed` reads from the filesystem for hot-reload during development.

External CLI invocations (`tt ingest sessions`, `tt sync`) write directly to tt-db as today. The dashboard server detects writes by polling a single-row `meta.db_version` counter every 2s and broadcasts SSE events to connected clients.

### Backend stack (Rust)

| Concern | Library | Version |
|---|---|---|
| HTTP server | `axum` | 0.8.9 |
| SSE | `axum::response::sse` (built-in) | — |
| Async runtime | `tokio` | 1.52 |
| Asset embedding | `rust-embed` (with `axum-ex` feature) | 8.11.0 |
| Browser open | `open` | 5.3.5 |
| JSON | `serde` + `axum::Json` | — |

`axum` chosen over `actix-web` / `salvo` / `rocket`: built-in SSE, ergonomic extractors, native Tokio integration. `rust-embed` chosen over `include_dir!`: actively maintained, axum integration, dev-mode filesystem reading.

### Allocation contract

A single function is the only allowed entry point for time allocation:

```rust
// in tt-core
pub fn allocate_for_period(
    db: &Database,
    period: (DateTime<Utc>, DateTime<Utc>),
) -> Result<AllocationResult>;
```

Invariants enforced by the wrapper:

- Loads ALL events in the period range from `tt-db`, sorted ascending by timestamp.
- Populates `session_end_times` from `agent_sessions.end_time` for every session that has one. **Not** empty.
- Populates `session_types` from `agent_sessions.session_type`. **Not** empty.
- Passes `period_end = period.1` explicitly so open focus intervals close correctly at the boundary.
- Returns `AllocationResult` exactly as `allocate_time` does today.

Callers:

| Caller | Where |
|---|---|
| `tt report` | `tt-cli/src/commands/report.rs` — replaces direct `allocate_time` call |
| `tt recompute` | `tt-cli/src/commands/recompute.rs` — replaces inlined session-prep |
| `tt dashboard` | `tt-dashboard::api::timeline` |

This wrapper fixes `report.rs`'s current empty-`session_end_times` bug. After this lands, `tt report` numbers will change — delegated time may shrink for sessions whose actual `end_time` is now respected vs the previous 30-minute timeout heuristic. This is intended and documented in release notes.

The wrapper does NOT fix the duplicate-`agent_session(ended)` bug (under-counts delegated for long-running sessions like build-graph #11416). That fix is tracked separately and lands as a parallel cleanup; the dashboard will faithfully visualize whatever the algorithm currently computes.

**Forbidden**: any caller invoking `allocate_time` directly. Adding a new caller requires extending `allocate_for_period`'s contract first. CI enforces via grep over `crates/tt-cli` and `crates/tt-dashboard` (allow-listed inside `tt-core` only).

### Backend concurrency model

The project is currently fully synchronous: `tt-db` wraps a single `rusqlite::Connection`. Adding `axum` + `tokio` requires explicit discipline.

**Pattern**: connection pool + `spawn_blocking` for every DB call.

- **`r2d2` + `r2d2_sqlite`**: pool of 4 connections (sufficient for single-user load).
- **Every DB operation runs in `tokio::task::spawn_blocking`** — `rusqlite` is synchronous; never `.await` while holding a connection.
- **Connections acquired per request**, released as soon as the handler's DB work completes. SSE handlers do NOT hold a connection; they hold a `broadcast::Receiver` and re-acquire when they need to fetch.
- **Writes are serialized through a single dedicated writer connection** (SQLite handles concurrent reads but writes need a single writer). All mutating handlers go through a `Mutex<Connection>` guarded by `spawn_blocking`.

**Forbidden patterns** (CI grep enforcement):

- `Mutex<Connection>` held across `.await`.
- Direct `rusqlite::Connection::execute(...)` outside `spawn_blocking`.
- Shared global connection across async tasks.

`tt-db` gets a new `Database::pool() -> Pool<SqliteConnectionManager>` constructor used by `tt-dashboard`. Existing sync callers (`tt ingest`, `tt sync`, etc.) keep using `Database::open()` — unchanged.

### Frontend stack

| Concern | Library | Version |
|---|---|---|
| Framework | Svelte 5 (runes) | pinned `^5.48.0` |
| Build | Vite 8 + Rolldown | — |
| Language | TypeScript | — |
| Coord math | `d3-scale`, `d3-zoom`, `d3-brush` | latest |
| Hit testing | `d3-quadtree` | latest |
| Virtualization | `@tanstack/virtual` | 3.13+ |
| Color palette | `d3-scale-chromatic` | latest |
| Tooltip placement | `@floating-ui/dom` | latest |

Svelte 5 chosen over React 19 / Solid 1.x / Lit / Vue 3 / Preact based on:
- Reactivity model (runes) maps cleanly to SSE-driven state.
- Compiles to direct DOM mutations: ~3× better INP than vdom-based frameworks under continuous SVG updates (benchmark verified May 2026).
- Bundle ~35–55KB gzipped for the whole dashboard.
- Stable since Oct 2024; weekly releases; no breaking changes pending. (Solid 2.0 beta means picking Solid 1.x today commits to a migration in 12–18 months.)
- Caveat: 5.36–5.48 had SVG perf regressions from the async rewrite — pinning `^5.48.0` avoids these.

### Rendering layers

At peak (30 streams × ~300 visible events per stream = ~9k elements), plain SVG runs at <30fps during scroll. Use a three-layer hybrid (pattern from Chrome DevTools Performance panel, JointJS, Felt):

| Layer | Tech | Contents |
|---|---|---|
| 1 (back) | **Canvas 2D** | Ribbon fills (solid/faded/centerline), event dots, subagent ticks, hatched idle bands |
| 2 (middle) | **SVG overlay** | Time axis labels, stream column headers, day-boundary lines, PR markers + lines |
| 3 (front) | **HTML** | Tooltips, context menus, edit panels, drag-select highlight, "↑ N new events" pill |

Canvas handles dense per-pixel work. SVG handles crisp text and CSS hover/focus on a small element count. HTML handles inputs/forms/links. Each layer has its own pointer-events handling.

## Visual model

### Layout

```
┌─────────┬──────────┬──────────┬──────────┬─────┐
│  Time   │ Stream A │ Stream B │ Stream … │ ... │
│  axis   │ (col-1)  │ (col-2)  │          │     │
├─────────┼──────────┼──────────┼──────────┼─────┤
│ 14:30   │ ████ ◆   │ ░░░░     │ ████     │     │
│ 14:25   │ ████     │ ░░░░     │       ▼  │     │
│ 14:20   │ ████ ◆   │ ░░░░ #12515         │     │
│ ────────┼─ idle fold (54 min, click) ────┼─────│
│ 13:20   │   │      │ ████ ◆   │ ████     │     │
│ 13:15   │ ████     │ ████     │   │      │     │
└─────────┴──────────┴──────────┴──────────┴─────┘
```

- **Top of viewport = now**, scroll down for older history.
- **Time axis** (~80px, sticky-left, SVG): hour ticks every 60min, minor ticks every 5min, day boundaries as bold lines + date labels.
- **Stream columns** (~100px each, configurable, no horizontal scroll until total width exceeds viewport): ordered left-to-right by `first_event_at` globally.

### Stream column lifespan

Column for stream X is **visually mounted** when the viewport's time range intersects X's `[first_event_at, last_event_at]` lifespan. Order is stable — columns never rearrange as you scroll; they only appear/disappear at the lifespan boundaries.

Within the lifespan, four visual states per moment:

| State | Visual | Meaning |
|---|---|---|
| **Solid fill** | full-opacity stream color | Direct focus (user on this stream's pane) |
| **Faded fill** | ~30% opacity stream color | Delegated only (agent running, user focus elsewhere) |
| **Centerline** | 1px line, ~20% opacity | Alive but dormant (within lifespan, no current activity) |
| **Empty** | nothing | Outside lifespan (before `first_event_at` or after `last_event_at`) |

The centerline state means a stream paused overnight reads as `ribbon → centerline → ribbon` rather than disappearing and reappearing. Columns stay put through quiet stretches.

Solid + delegated overlap (supervised work) = solid (active state wins).

### Point events

Plotted at the event's Y coordinate inside the relevant column:

| Event | Visual |
|---|---|
| `user_message` | Filled dot, radius ~3px, stream color |
| `subagent_start` | Side tick on column's left edge, 4×2 rect, agent-type color |
| `session_start` | ▲ on column edge |
| `session_end` | ▼ on column edge |

**MVP plots all `user_message` events identically (filled dot).** The steering-vs-light distinction is a v2 feature once a classifier exists.

Subagent type colors are independent of stream palette: `explore` green, `librarian` cyan, `oracle` purple, `Sisyphus-Jr` red, `Momus` orange, `Metis` amber.

### PR markers

PRs are linked to streams via existing `pr:NNNNN` tags. Server resolves tags by shelling out to `gh pr view NNNNN --json …` with an in-memory TTL cache.

Visual: at the column's Y for `opened_at`, render a small `#NNNNN` badge. If merged, draw a 1px line down the column edge to `merged_at` + a merge arrow marker. If closed without merge, dotted line to `closed_at` + close marker. Click → opens GitHub URL.

PRs do not get their own column — they live inline on their stream's column.

### Idle gap folds

Cross-stream gaps without `tmux_pane_focus` or `user_message` events for >= `idle_threshold` (default 15 min, configurable) compress to fixed-height (~40px) hatched bands regardless of duration. Agent activity alone does **not** prevent folding — overnight unattended agent runs should compress so we don't waste vertical space on hours of empty timeline.

Click a fold → expand to full linear scale for that section. Click again → re-collapse. State stored in component, not persisted (folds re-collapse on page reload).

The detection runs server-side in the `/api/timeline` response so client just renders what server declares.

### Time scale

Custom piecewise-linear scale built on `d3-scale`:

- **Active regions**: 1px per minute default. Continuously zoomable via pinch / Ctrl+wheel — wide enough to see a week+ of history at a glance, tight enough that a 5-minute window fills the screen. Concretely: ~0.05px/min to ~10px/min.
- **Folded regions**: fixed ~40px regardless of underlying duration.

Zoom + viewport top time stored in URL hash (`#z=2.5&t=2026-05-26T21:00`) so refresh / share-link preserves view.

### Stream colors

Primary palette: `d3-scale-chromatic`'s `schemeTableau10` (10 distinguishable colors designed by Tableau for categorical data). Streams sorted by `first_event_at` globally, indexed mod 10.

For viewports where >10 streams are simultaneously visible, fall back to `schemePaired` (12) or `schemeSet3` (12 pastels) for the overflow streams. User can override per-stream via the detail panel; override stored in optional `streams.color` column (NULL = compute from index).

### "Now" indicator + live behavior

- Top of viewport renders a 1px accent line at "now".
- When at the top of the timeline, new events animate in at the top (existing content pushes down).
- When user has scrolled past `now - 30s`, **pause auto-follow**. A floating "↑ N new events" pill appears in the header. Click → scroll to top, resume auto-follow.

## Data model

### Reads

| Endpoint | Returns |
|---|---|
| `GET /api/streams` | All streams + metadata |
| `GET /api/timeline?before=ISO&duration=24h` | Pre-computed intervals + point events for a window (see shape below) |
| `GET /api/sessions/:session_id` | Session detail (parent/children, prompts, subagent lineage) |
| `GET /api/events/:event_id` | Single event with all available text (truncated, from `agent_sessions`) |
| `GET /api/prs?stream_id=…` | Linked PRs for a stream (via tag resolution + `gh`) |

`/api/timeline` is the workhorse. Frontend requests chunks as you scroll backward. Server uses `tt-core::allocate_time` to compute intervals — same code path as `tt report`. Shape:

```jsonc
{
  "window": {"start": "2026-05-25T21:00Z", "end": "2026-05-26T21:00Z"},
  "server_version": 1234,
  "streams_active": ["stream-uuid-1", "stream-uuid-2"],
  "ribbons": {
    "stream-uuid-1": {
      "focus_intervals": [{"start": "...", "end": "..."}],
      "delegated_intervals": [{"start": "...", "end": "..."}],
      "events": [
        {"ts": "...", "kind": "user_message", "session_id": "ses_..."},
        {"ts": "...", "kind": "subagent_start", "subagent_type": "explore", "session_id": "ses_..."},
        {"ts": "...", "kind": "session_start", "session_id": "ses_..."},
        {"ts": "...", "kind": "session_end", "session_id": "ses_..."}
      ]
    }
  },
  "idle_gaps": [{"start": "...", "end": "...", "duration_min": 54}],
  "prs": [
    {"stream_id": "...", "number": 12515, "title": "...", "url": "...",
     "opened_at": "...", "merged_at": "...", "state": "open" | "merged" | "closed"}
  ]
}
```

### Writes

| Endpoint | Body | Effect |
|---|---|---|
| `POST /api/streams` | `{name, tags, color?}` | Create empty stream |
| `PATCH /api/streams/:id` | `{name?, tags?, color?}` | Rename / retag / recolor |
| `DELETE /api/streams/:id?reassign_to=ID` | — | Reassign events, delete stream |
| `POST /api/streams/:id/merge` | `{other_id, keep_name_from: "self" \| "other"}` | Move other's events into self, delete other |
| `POST /api/streams/:id/split` | `{split_at: ISO, new_stream: {name, tags}}` | Events from `split_at` onward move to new stream |
| `POST /api/events/assign` | `{selector: {session_ids? \| event_ids? \| cwd_pattern?}, target_stream_id}` | Bulk reassignment |

### Edit operation semantics

Each mutation has explicit transactional behavior. Implementer must not improvise edge cases.

#### Create (`POST /api/streams`)

- Inserts into `streams` with new UUID, `created_at = now`, `updated_at = now`.
- Inserts each tag into `stream_tags`.
- Color: if provided, store; if null, computed at render time from palette index.
- No events affected. No recompute.

#### Rename / Retag / Recolor (`PATCH /api/streams/:id`)

- Updates `streams.name` / `streams.color` / `streams.updated_at`.
- For tags: **full replacement** — delete all existing rows in `stream_tags` for this stream, insert provided list. Clients send the final tag set, not a diff.
- No events affected. No recompute.

#### Delete (`DELETE /api/streams/:id?reassign_to=ID`)

- Required: `reassign_to` query param identifying a different existing stream.
- In one transaction:
  1. `UPDATE events SET stream_id = :target WHERE stream_id = :id`.
  2. `DELETE FROM stream_tags WHERE stream_id = :id`.
  3. `DELETE FROM streams WHERE id = :id`.
- Recompute period = `[min(moved_events.timestamp), max(moved_events.timestamp)]`.
- 4xx if target doesn't exist or equals source.

#### Merge (`POST /api/streams/:id/merge`)

- Body: `{other_id, keep_name_from: "self" | "other"}`.
- In one transaction:
  1. `UPDATE events SET stream_id = :keeper WHERE stream_id = :loser`.
  2. Tag union: `INSERT OR IGNORE INTO stream_tags(stream_id, tag) SELECT :keeper, tag FROM stream_tags WHERE stream_id = :loser`.
  3. `DELETE FROM stream_tags WHERE stream_id = :loser`.
  4. `DELETE FROM streams WHERE id = :loser`.
  5. If `keep_name_from = "other"`: also `UPDATE streams SET name = :loser_name, color = :loser_color WHERE id = :keeper`.
- Recompute period = `[min(moved_events.timestamp), max(moved_events.timestamp)]`.

#### Split (`POST /api/streams/:id/split`)

- Body: `{split_at: ISO, new_stream: {name, tags}}`.
- Semantics: events with `timestamp >= split_at` move to a newly-created stream. Events with `timestamp < split_at` stay on the source.
- Edge cases:
  - `split_at < first_event_at`: 4xx (trivial split, user probably meant rename).
  - `split_at > last_event_at`: 4xx (no-op split, user probably meant rename).
  - `split_at` falls between two events: clean partition, no fragmentation needed.
  - Multiple events at the exact `split_at` timestamp: they move to the new stream (the `>=` boundary).
- New stream inherits **none** of the source's metadata except as explicitly provided in `new_stream.{name, tags}`. Color computed from palette index. User updates color separately if desired.
- Recompute period = `[split_at, max(source.last_event_at)]`.

#### Reassign events (`POST /api/events/assign`)

- Body: `{selector, target_stream_id}` where selector is **exactly one** of:
  - `{event_ids: [...]}` — exact event IDs
  - `{session_ids: [...]}` — all events with these session_ids
  - `{cwd_pattern: {cwd_like, start, end}}` — pattern + time range
- **Drag-select on a stream column** translates to `{event_ids: [...]}`. Frontend computes the set (all events within that column AND that time range, including focus + tool_use + user_message + session_start/end) and POSTs as explicit IDs. Backend just applies — no selector ambiguity on the server.
- In one transaction: `UPDATE events SET stream_id = :target WHERE id IN (...)`.
- Recompute period = `[min(affected.timestamp), max(affected.timestamp)]`.

**All edit endpoints** return the affected stream(s) in the response so the frontend can patch its state without a full refetch. Corresponding SSE events follow as a separate broadcast so other open tabs sync. Edits are NOT idempotent on retry by client — the frontend is responsible for not double-submitting; server treats every POST as a new operation.

Write handler structure:

1. Begin DB transaction.
2. Apply mutation.
3. Bump `meta.db_version`.
4. Commit.
5. Return 200 with updated state.
6. Spawn background recompute task (`allocate_for_period` over the affected time range; full-period not per-stream — allocation focus state is global, per-stream incremental recompute is unsound).
7. Broadcast `stream_*` SSE event immediately; broadcast `recompute_started` / `recompute_done` from the background task.

Recompute uses a **single-flight** pattern: if a recompute is in progress, the new request enqueues and coalesces. Prevents thundering-herd under rapid editing.

### Live updates (SSE)

`GET /api/sse` opens a long-lived event stream. Event types:

```
connected            { server_time, server_version, last_event_at }
events_appended      { since: ISO, count: N, streams_affected: [stream_id] }
stream_created       { stream: <full stream object> }
stream_updated       { stream_id, fields_changed: [name|tags|color], stream }
stream_deleted       { stream_id, reassigned_to: stream_id }
recompute_started    { stream_ids?: [...] | null /* null = all */ }
recompute_progress   { pct: 0..100, current_stream_id? }
recompute_done       { stream_ids: [...] }
resync_required      { reason: "lagged" | "schema_changed" }
heartbeat            {}  /* every 30s via axum's KeepAlive */
```

Connection management:
- `tokio::sync::broadcast` channel inside the server, capacity 1024.
- Each open SSE handler subscribes a receiver; mutations and the db_version poller publish.
- Lagging consumers receive `Lagged` from the channel → server sends `resync_required` with reason `"lagged"`; client refetches `/api/timeline`.
- Multiple browser tabs each get their own SSE; broadcast reaches all of them.
- Initial-load race: `/api/timeline` response includes `server_version`. Client ignores SSE events whose `version <= server_version` until caught up.

### Detecting external CLI writes

A single-row `meta(key='db_version')` counter. Every write path bumps it inside the same transaction. Centralized in a `tt-db::bump_db_version(tx)` helper called from:
- `tt-cli::ingest` (sessions, pane-focus)
- `tt-cli::import` (sync)
- `tt-cli::classify` (--apply)
- `tt-cli::tag`
- `tt-cli::streams` (any mutation subcommand)
- `tt-dashboard::api` (every write handler)

Server polls `SELECT value FROM meta WHERE key='db_version'` every 2s. On change, computes delta (events with `timestamp > last_known_ts`) and broadcasts `events_appended`.

## Interaction model

### Selection

- **Click stream header** → opens right-side detail panel, marks stream selected.
- **Drag-select a time range on a column** → `d3.brush` highlights range; floating "Reassign N events to…" button appears.
- **Right-click stream / range** → context menu (merge / split / reassign / delete).
- **Cmd/Ctrl+click stream headers** → multi-select (for merge).

### Edit operations

| Op | UX |
|---|---|
| **Create** | "+ New" header button → modal {name, tags, color?} |
| **Rename** | Inline in panel, save on blur or Enter |
| **Retag** | Chip editor in panel, autocomplete from existing tags |
| **Recolor** | Color picker in panel |
| **Delete** | Panel button → modal with required `reassign_to` dropdown |
| **Merge** | Keyboard `m` then click two columns, OR right-click → modal with `keep_name_from` radio |
| **Split** | Keyboard `s` with stream selected and timeline cursor at split point, OR right-click → modal |
| **Reassign events** | Drag-select on column → floating button → target dropdown |

Inline edits (rename/retag/recolor) are optimistic with toast-based 5s undo. Destructive ops (delete/merge/split) require modal confirmation.

### Keyboard shortcuts

| Key | Action |
|---|---|
| `j` / `k` | Step one event back / forward |
| `g g` | Jump to top (now) |
| `G` | Jump to bottom (earliest loaded) |
| `/` | Focus search box (filter visible streams by name/tag) |
| `e` | Edit selected stream |
| `m` | Merge mode |
| `s` | Split selected stream at timeline cursor |
| `Esc` | Close panel / cancel mode |

### Tooltips

`d3-quadtree` keyed by `(event_ts, column_x)` for O(log n) hit-testing on canvas elements. Tooltips render as HTML overlays positioned by `@floating-ui/dom` (handles placement + viewport collision). Tooltips stay open while cursor is over them (allows scrolling long content and clicking links).

Per-element content:
- **Dot** (`user_message`): stream name, timestamp, snippet from `agent_sessions.starting_prompt` or matching `user_prompts[]` (truncated, per Non-goals — full text is v2).
- **Side tick** (`subagent_start`): agent type, session ID, start time.
- **Ribbon segment**: stream name + tag chips, segment start/end, duration.
- **PR marker**: full title, opened/merged times, link to GitHub.

## Schema additions (MVP)

```sql
-- Live update detection
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT OR IGNORE INTO meta(key, value) VALUES('db_version', '0');

-- Optional per-stream color override (NULL = compute from palette index)
ALTER TABLE streams ADD COLUMN color TEXT;
```

`SCHEMA_VERSION` in tt-db bumps from N to N+1. Per project convention, schema mismatch is fail-fast — users with older DBs delete and re-ingest. No data migration.

## Schema additions (v3, deferred)

Out of MVP scope; documented here so the design can accommodate it later.

```sql
CREATE TABLE session_stream_spans (
    session_id TEXT NOT NULL,
    stream_id TEXT NOT NULL,
    start_ts TEXT NOT NULL,
    end_ts TEXT NOT NULL,
    source TEXT NOT NULL,  -- 'pattern' | 'llm' | 'user'
    PRIMARY KEY (session_id, start_ts),
    FOREIGN KEY (stream_id) REFERENCES streams(id) ON DELETE CASCADE
);
```

When v3 lands: orchestrator sessions get LLM-classified into per-stream sub-spans. The allocator consults this table during attribution. UI renders sub-span boundaries within a session's ribbon.

## Crate layout

```
crates/tt-dashboard/
├── Cargo.toml
├── src/
│   ├── lib.rs           # serve(config) entry
│   ├── server.rs        # axum routes + middleware
│   ├── api.rs           # HTTP handlers
│   ├── sse.rs           # broadcast channel + SSE handler + db_version poller
│   ├── recompute.rs     # background recompute task (single-flight)
│   ├── pr_cache.rs      # gh shell-out + TTL cache
│   └── assets.rs        # rust-embed wrapper
└── web/
    ├── package.json
    ├── vite.config.ts
    ├── tsconfig.json
    ├── index.html
    └── src/
        ├── main.ts
        ├── App.svelte
        ├── lib/
        │   ├── api/
        │   │   ├── client.ts          # fetch wrappers
        │   │   └── sse.ts             # EventSource subscription
        │   ├── store.svelte.ts        # reactive state (runes)
        │   ├── timeline/
        │   │   ├── Timeline.svelte    # main canvas + svg + html layers
        │   │   ├── canvas-renderer.ts # ribbon/dot/tick drawing
        │   │   ├── time-scale.ts      # piecewise scale (idle compression)
        │   │   ├── virtualize.ts      # TanStack Virtual integration
        │   │   └── hit-test.ts        # quadtree
        │   ├── streams/
        │   │   ├── column-layout.ts   # stable column ordering
        │   │   ├── DetailPanel.svelte
        │   │   └── lifespan.ts        # mounted-when logic
        │   ├── edit/
        │   │   ├── Toolbar.svelte
        │   │   ├── MergeDialog.svelte
        │   │   ├── SplitDialog.svelte
        │   │   ├── DeleteDialog.svelte
        │   │   ├── ReassignFloater.svelte
        │   │   └── Tooltip.svelte
        │   └── theme/
        │       └── palette.ts         # schemeTableau10 + assignment
        └── app.css
```

tt-cli additions: `crates/tt-cli/src/commands/dashboard.rs` calling `tt_dashboard::serve(config)`. New CLI flags: `--port`, `--no-browser`.

tt-db additions: a few queries (`events_in_range`, `sessions_with_lineage`), the `meta` table accessor, and `bump_db_version(tx)`.

## MVP acceptance criteria

Dogfooding test: at the end of Week 4, the dashboard must be sufficient for Sami's daily standup workflow without falling back to `tt report`. Concretely:

### Functional

- [ ] Open `tt dashboard`. See the most recent ~24h at the top by default.
- [ ] Header has time-range presets: **Today / Yesterday / Last 7 days / Custom**.
- [ ] Per-stream summary panel (sticky right or collapsible): for each visible stream in the current range, lists direct + delegated time. Sortable by total time. Numbers match `tt report --day` / `--last-day` exactly (same `allocate_for_period` wrapper, by construction).
- [ ] One-click **"Copy standup summary"** button produces stream-grouped markdown of the form `*<stream name>* — Nh direct / Mh delegated` for the selected range, suitable for pasting into the standup Slack post.
- [ ] **Anomaly indicators** visible per stream:
  - Stream has duplicate `agent_session(ended)` events in range → warning icon + tooltip explanation.
  - Stream has sessions where `tool_call_count > 0` but computed delegated time = 0 → warning (catches the duplicate-`ended` symptom).
  - Stream has tmux_pane_focus events with `stream_id IS NULL` after pattern-matching → "unclassified events" badge.
- [ ] **Quick reassign workflow**: see a chunk of obviously-misattributed time (e.g., wrong stream's pane during gym), drag-select range on the column, pick correct stream from dropdown, ribbon updates within 5 seconds (including recompute).

### Performance

- [ ] Initial load of last 7 days of timeline data: < 2 seconds to first paint.
- [ ] Scrolling backward through history at 60fps until at least 30 days back.
- [ ] Edit operation (merge / split / reassign) round-trip including recompute: < 5 seconds for a typical day's worth of events.

### Correctness

- [ ] For any selected time range, sum of per-stream `direct_ms` shown in the summary panel = `tt report --json` direct total for the same range — both via `allocate_for_period`.
- [ ] After an edit, refreshing the page produces the same state.
- [ ] After an edit in one browser tab, a second open tab sees the change via SSE within 2 seconds.

If any of these are unmet at end of Week 4, the MVP is not done. **Sequencing reshuffles before scope reduces.** If a week falls behind, push the rendering completeness from Week 3 to absorb the slip; do not cut acceptance criteria.

## Sequencing

| Week | Deliverable |
|---|---|
| **1** | Backend skeleton. `tt dashboard` starts axum on :8765, serves stub JSON for `/api/streams` and `/api/timeline`. SSE channel set up; `db_version` polling broadcasts `events_appended` when tested with a manual SQL insert. `rust-embed` configured; debug = filesystem, release = embedded. |
| **2** | Frontend skeleton. Svelte 5 + Vite scaffolded. `EventSource` wired to SSE. Layout grid renders: time axis + stream columns + viewport. Basic ribbon rendering on canvas. No interactions yet. |
| **3** | Rendering completeness. All four ribbon states. Point events (dots, ticks, session markers). Idle gap folding. PR markers (mock data → real `gh` cache). Hover tooltips. Virtualization for bidirectional scroll. |
| **4** | Edit operations + polish. Detail panel, all 7 edit ops with their dialogs/flows. Recompute coordination. Keyboard shortcuts. Toast undo for inline edits. End-to-end test in `tt-cli/tests/e2e_dashboard.rs`. |

Total: ~4 weeks of focused work for MVP β (calibration + edit, no LLM-narrative attribution).

## Testing strategy

### Backend (Rust)

- Unit tests per API handler with in-memory tt-db (`Database::open_in_memory()`).
- SSE channel: tests for multi-subscriber broadcast and `Lagged` handling.
- Recompute single-flight: test that two overlapping POSTs result in one queued recompute, not parallel runs.
- `db_version` polling: integration test where external `INSERT` triggers `events_appended` within 3s.
- Snapshot tests for `/api/timeline` JSON shape with fixture event sequences.

### Frontend (TypeScript)

- Component tests for edit dialogs (Vitest).
- Canvas renderer: pixel-snapshot tests via Playwright (rendering pipeline → screenshot diff).
- Time-scale unit tests: piecewise mapping with mocked idle gaps.
- Hit-testing: synthetic event grid + assertions on quadtree lookup.

### Integration (E2E)

`crates/tt-cli/tests/e2e_dashboard.rs`:
- Spawn `tt dashboard` on an ephemeral port.
- `reqwest` against endpoints, assert shape.
- Open `EventSource`, trigger a write, assert SSE event arrives.
- Playwright (optional v1.1) for browser-level E2E.

## Distribution

```
crates/tt-dashboard/web/dist/    # built JS bundle (gitignored)
```

CI builds the frontend before the binary: `cd crates/tt-dashboard/web && npm ci && npm run build`. `web/dist/` is then embedded by `rust-embed` into the binary. Existing `scripts/deploy-remote.sh` extended to run the frontend build before the binary copy.

Local development: `cd crates/tt-dashboard/web && npm run dev` for Vite HMR alongside `cargo run -- dashboard` (in debug, `rust-embed` reads from disk).

## Known limitations (documented, accepted for MVP)

- **Hover verbatim text** is whatever `agent_sessions.starting_prompt` and `user_prompts[]` already contain (truncated). Full per-event text needs a v2 `event_text` table and extended sync.
- **Orchestrator sessions** display under their single assigned stream. The dashboard cannot today represent "this session drove three streams in sequence." A v3 `session_stream_spans` table + LLM classifier addresses this.
- **PRs from private repos** require local `gh` auth. Cache failures (e.g., `gh` not installed, no auth) render as a gray placeholder rather than crashing the column.
- **No undo stack.** Toast-based 5s undo covers inline edits; destructive ops have modal confirmation. Full undo/redo is v2.
- **No mobile / responsive.** Dashboard targets ≥1024px wide screens.
- **Single-user.** Multi-user / hosted is a vN concern that would require auth + multi-tenant redesign.

## Open implementation decisions (low-risk, defer to plan)

- Exact default for `idle_threshold` — 15 min is a starting point; will likely tune in v1.1.
- Whether to show a "this might be an orchestrator session" hint heuristically before v3 lands (e.g., session touches >1 cwd above some threshold). Defer; just attribute by single stream for MVP.
- Whether stream column width is fixed or adaptive when >10 streams active. Start fixed (~100px) with horizontal scroll; revisit if friction emerges in dogfooding.

## Future work (post-MVP, out of scope)

- **v2**: per-event verbatim hover text (`event_text` table + sync extension), undo/redo, steering-vs-light dot classification
- **v3**: `session_stream_spans` table + LLM narrative attribution for orchestrator sessions
- **v4**: pinch-zoom touchpad gestures, search/filter UX, saved views
- **vN**: optional shared multi-user dashboard (requires auth, multi-tenant redesign)

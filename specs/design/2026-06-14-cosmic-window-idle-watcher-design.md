# COSMIC Window/Idle Watcher (`tt watch`)

## Problem

tt only sees terminal activity (tmux pane focus, agent sessions). Time spent **outside the
terminal** — writing proposals in a browser, Slack comms, reading docs/PDFs — is invisible. In
practice this is a large, unaccounted slice of the week, and it silently distorts reports and
weekly reviews (entire categories of work simply don't exist in the data).

ActivityWatch (AW) is the obvious off-the-shelf fix, but it doesn't work here:

1. **The bundled `aw-watcher-window` is X11-only** (per AW docs). On this machine's COSMIC/Wayland
   session it reports `app: "unknown", title: "unknown"` forever — verified empirically.
2. **`aw-qt` (the tray launcher) crashes on COSMIC's Wayland tray**, orphaning the server and
   killing the watchers — verified empirically.
3. Even working, AW is a **separate stack** (its own server, DB, REST API) that we'd then have to
   bridge into tt via an importer.

Since tt is already a Rust activity-signal collector and a window/idle watcher is just another
passive event source, we build the capability **into tt** and drop the AW stack entirely.

## Goals (v1)

- Capture the **active window** (`app_id` + window title) and **idle/AFK** state on the COSMIC
  desktop, as first-class tt events.
- Attribute that time to streams through tt's **existing `tt classify` LLM pipeline** (no new
  parallel mental model).
- Make non-terminal time show up correctly in `tt report` / weekly reviews.

## Non-Goals (v1)

- Browser-tab URLs (AW's browser extension) — deferred.
- Backends other than COSMIC (X11, Sway/Hyprland, KDE, GNOME) — deferred behind a trait.
- Deterministic app→stream mapping rules — rejected in favor of the LLM classify path.
- Per-keystroke/input-volume metrics (`aw-watcher-input`) — out of scope.

## Constraints

### Hard (non-negotiable)

- **The daemon writes directly to SQLite.** It opens one connection with a `busy_timeout` and calls
  `db.insert_events()` (the same `INSERT OR IGNORE` dedup path as the drain). WAL mode (already
  enabled) serializes the ~1/sec writes against concurrent `tt` commands, and tt's ingest commits
  many *small* transactions so the write lock is held only briefly. Window events are therefore
  live in `tt report` immediately — no JSONL, no drain, no report lag. (The earlier "never touch
  SQLite" framing was over-cautious: it came from the *ephemeral, per-event* tmux hook and the
  *remote* JSONL sync transport, neither of which applies to a single persistent local daemon —
  Toggl writes direct to SQLite for exactly this reason.) Lifecycle caveat: a schema-migrating tt
  upgrade requires a daemon restart (systemd handles it).
- **Allocation stays centralized.** `allocate_time()` already requires all events for a period in
  one place; window/idle events join that same model.
- **One focus at a time (single global timeline).** Allocation credits exactly one stream per
  instant across all machines. This is *correct* for a single human — it prevents double-counting
  direct time — so focus is deliberately **not** split per machine. (A stray/automated focus event
  is bounded by `attention_window` + idle and self-corrects on the next real focus event.)

### Soft (preferences)

- **tt is relicensed `GPL-3.0-only`** to depend on the `cctk`/`cosmic-protocols` git crates directly
  (verified GPL-3.0-only). Owner decision: tt is a personal tool, copyleft costs nothing here, and
  this is far less build setup than generating bindings from the (permissive) XML. Compliance
  to-do: flip `Cargo.toml` `license`, add a `GPL-3.0` `LICENSE` file, and allow both GPL-3.0 **and
  the `cctk` git source** in `cargo-deny` (`deny.toml` has `unknown-git = "deny"`).
  (`ext-foreign-toplevel-list-v1` / `ext-idle-notify-v1` come from `wayland-protocols`, MIT/Apache
  — only the COSMIC toplevel crate is GPL.)

## Current Architecture (verified)

- **Event model** (`tt-core/src/event_type.rs`): `EventType` already includes `WindowFocus`,
  `BrowserTab`, and `AfkChange` variants (alongside `AgentSession`, `TmuxPaneFocus`, etc.). The
  schema anticipated this feature.
- **`StoredEvent`** (`tt-db/src/lib.rs`): explicit columns only — **no generic JSON/`data` column**
  (the `data` field is *rebuilt* from columns via `build_data_json()`). Has `status`
  (`"idle"`/`"active"`) and `idle_duration_ms` for AFK, plus `cwd`, `stream_id`, etc. **No
  `app_id`/`title` columns yet.**
- **Local capture** (`tt-cli/src/commands/ingest.rs`): the tmux hook calls
  `tt ingest pane-focus …`, which appends one JSON line to
  `~/.local/share/time-tracker/events.jsonl` under a file lock, with a 500 ms per-key debounce and
  1 MB rotation to `events.jsonl.1`.
- **Drain**: `import_local_events()` reads `events.jsonl` + `.1` into the DB with `INSERT OR IGNORE`
  (idempotent, deterministic IDs); runs during `tt ingest sessions`.
- **cwd→stream auto-assign**: exact-cwd or project-suffix match, only when unambiguous; otherwise
  left for `tt classify`. **Window events have no cwd**, so they bypass auto-assign and rely
  entirely on classify.
- **Allocation** (`tt-core/src/allocation.rs`, ~1366 LOC): `AllocatableEvent` trait;
  `attention_window` = 60 s, `agent_timeout` = 30 min.
- Everything is **synchronous**; `tokio` is a dependency but unused. No existing daemon/loop/signal
  code.

## Design

### 1. Runtime: `tt watch` (daemon)

New subcommand `tt watch` (alias of `tt watch cosmic` for v1). Synchronous loop, ~1 s
`event_queue.roundtrip()` poll. Loads normal config + machine identity, opens the DB (one
connection, `busy_timeout` set), binds the COSMIC + idle Wayland protocols, and **inserts events
directly** via `db.insert_events()`. Fails fast with a clear diagnostic if the COSMIC globals are
absent.

```
COSMIC compositor ──wayland──> tt watch (sync loop ~1s)
                                  │ db.insert_events()  (WAL, busy_timeout)
                                  ▼
                                SQLite ──> allocation ──> classify
```

### 2. Backend trait boundary

Wayland deps live in `tt-cli` (the runtime), never in `tt-core`/`tt-db` (which stay pure).

```rust
trait WindowBackend {
    /// Poll current desktop state; called every poll interval.
    fn poll(&mut self) -> Result<Snapshot>;
}

struct Snapshot {
    active: Option<ActiveWindow>, // { app_id: String, title: String }
    idle:   IdleState,            // Active | Idle { since: Instant }
}
```

v1 impl `CosmicBackend` uses:

- `wayland-client` 0.31
- `wayland-protocols` 0.32 (`features = ["staging", "client"]`) → `ext-foreign-toplevel-list-v1`
  (gives `app_id` + `title`) and `ext-idle-notify-v1` (idle).
- `cctk` (git `pop-os/cosmic-protocols`, pinned rev) → `zcosmic_toplevel_info_v1` (gives the
  `activated` state). **Bind both** `ext-foreign-toplevel-list-v1` and `zcosmic_toplevel_info_v1`;
  COSMIC does not implement the wlr protocol.

Adding X11/other backends later = new `WindowBackend` impls behind the same trait + protocol-
presence detection.

### 3. Event model

Two event types, both inserted directly into the DB via `db.insert_events()`:

**`WindowFocus`** — an *active-window snapshot* (not merely "focus changed"). Emitted on:

1. watcher **startup** (initial active window once known),
2. active **`(app_id, title)` change**, debounced ~500 ms to kill title churn,
3. **idle→active resume** — **always**, even if the window is unchanged. *(Load-bearing: validated
   against AW. Without it, returning to the same window after idle fails to re-establish focus
   and AFK time isn't correctly subtracted.)*

New nullable columns `window_app_id`, `window_title`. `source = "local.cosmic"`. No `cwd`.

**`AfkChange`** — reuses existing `status` / `idle_duration_ms` columns:

- `status = "idle"`: **backdated** to `notify_time − idle_timeout`. *(`ext-idle-notify-v1` fires
  exactly `idle_timeout` after last input, so this equals the true last-input time — matches AW's
  `last_input` semantics precisely, and is more accurate than AW's pynput polling.)*
- `status = "active"`: on resume, carries `idle_duration_ms = resume_time − idle_start`.

**Deterministic IDs** (keep `INSERT OR IGNORE` idempotent without collapsing distinct events):
`machine_id + source + event_type + timestamp + (app_id|status)`. **Do not** include volatile
Wayland object IDs (not stable across compositor sessions).

### 4. Schema & migration

- Add nullable columns `window_app_id TEXT`, `window_title TEXT` to `events`.
- Bump `SCHEMA_VERSION` 8 → 9.
- **Introduce a forward-migration path** (tt's first): on open, if `db_version < SCHEMA_VERSION`,
  apply ordered additive migrations (`ALTER TABLE events ADD COLUMN …`) then set the version.
  Keep **fail-fast** for `db_version > SCHEMA_VERSION` (newer-than-expected). Existing rows get
  `NULL` for the new columns — correct, since pre-capture events have no window data (nothing to
  backfill).

This replaces the current "recreate on mismatch" shortcut for additive changes; non-destructive,
preserves all stream/tag history.

### 5. Allocation changes (`allocation.rs`)

The current model is **already attention-window based and structurally AW-equivalent**: a focus
event accrues direct time up to `attention_window_ms`; activity events (e.g. `tmux_scroll`) reset the
window; `AfkChange(idle)` closes focus, backdated via `idle_duration_ms`. A `resolve_focus_stream`
hierarchy already maps the foreground app → stream (terminal→tmux stream, browser→browser-tab
stream, other→the window's own stream). The required changes are therefore small and targeted:

- **Make `WindowFocus` establish focus.** Today the `WindowFocus` arm only updates
  `window_focus_state` (the resolver's "which app/stream"); it does **not** set `focus_state`, so
  non-terminal/non-browser GUI apps (Slack desktop, PDF/doc readers) — and browser work without the
  extension — accrue **zero** direct time. Change it to also open a focus interval (closing the prior
  one capped at `attention_window_ms`), like the `tmux_pane_focus` / `user_message` arms — **except**
  for unclassified events: a window event with no stream resolves to no focus (accrues nothing),
  whereas `tmux_pane_focus` falls back to the UNASSIGNED bucket. This difference is intentional —
  pre-classify, *every* window event is unclassified, and bucketing raw GUI focus (browsing,
  settings) as "unassigned" would be noise; window time is attributed only after `tt classify`
  assigns runs to streams (see §6).
- **Browser fallback in `resolve_focus_stream`.** A browser app with no `BrowserTab` info (v1 has no
  browser extension) currently resolves to `None` → no time. Fall back to the window's own
  `stream_id` so browser-based work (e.g. a proposal in Google Docs) is captured.
- **`attention_window_ms` is the tail-bound** (the AW `pulsetime` equivalent) — **no separate
  "max-span cap" is needed** (that was a misread of the model). **Leave the global default
  unchanged**: `attention_window_ms` is shared by terminal/tmux allocation and `recompute`/`report`,
  so window focus reuses the existing cap (changing it would distort terminal reports + snapshots).
  (The `Default` is 300 s while the doc comment says 60 s — reconcile the comment, leave the value.)
- `AfkChange(idle)` already closes focus correctly (backdated); the watcher's resume `WindowFocus`
  (§3.3) re-establishes focus after idle.
- **Single global focus timeline (correct, not a compromise).** One focus at a time across all
  machines prevents double-counting direct time — the right model for a single human. Focus is
  deliberately not split per machine; stray cross-machine focus is bounded by `attention_window` +
  idle and self-heals (see Open Questions).

### 6. Classify changes — window-run synthesis

Raw single-window events are noisy and largely unclassifiable. Before presenting to the LLM,
`tt classify` synthesizes **window runs**: contiguous `WindowFocus` events (same machine, split by
idle/gaps) collapsed into `{ start, end, duration, app_id, sampled titles, **`event_ids`**, nearby
terminal/agent activity within ±15 min (tunable), idle splits }`. The LLM classifies *runs*; `--apply`
assigns them via a **new `assign_by_event_ids` path** — window events have `cwd = NULL`, so the existing
`cwd_like` pattern assignment can't touch them (Oracle catch). Preserves the existing stream model and
gives the classifier enough context to recognize "Slack thread about X", "browser docs for Y", "proposal doc Z".

### 7. systemd

- **systemd user service** for `tt watch`: imports `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR`,
  `DBUS_SESSION_BUS_ADDRESS`; `Restart=on-failure` with a sane `RestartSec`; started from / after
  the graphical session. On Wayland disconnect or compositor restart, exit non-zero and let systemd
  restart. Restart the service after a schema-migrating tt upgrade. Graceful shutdown is trivial —
  each event is its own `INSERT OR IGNORE`, no buffered state.
- **No drain timer or JSONL retention.** Writing directly to SQLite removes the `events.jsonl`
  buffer entirely — nothing to rotate or drain on a timer; window events are persisted live.

### 8. Defaults

| Knob | Default | Source / rationale |
|------|---------|--------------------|
| `idle_timeout` | 180 s | AW parity (`aw-watcher-afk`) |
| `poll_interval` | 1 s | AW parity (`aw-watcher-window`) |
| focus debounce | 500 ms | matches tmux-hook debounce |
| `attention_window_ms` | existing default, **unchanged** | shared with terminal/tmux + `recompute`/`report`; window focus reuses it (changing it would distort terminal reports) |

## ActivityWatch Alignment

Validated against AW source (`aw-watcher-afk`, `aw-server-rust/aw-transform`, `aw-core`,
`aw-client`):

- **Match**: idle backdating to last-input; idle defaults (180 s / poll); the **`attention_window`**
  tail-bound (AW's `pulsetime` equivalent — caps a focus interval with no further activity); explicit
  `idle_duration_ms` on resume (better than AW recomputing it). Activity events that reset the window
  (`tmux_scroll`, and now `WindowFocus`/resume) play AW's heartbeat role.
- **Differ deliberately**: AW re-sends the active window as a 1 Hz heartbeat; we emit **on-change +
  resume only** (Oracle: don't flood classify with periodic duplicates) and let the existing
  `attention_window` model reconstruct duration. Equivalent **iff** (a) we emit the resume
  `WindowFocus` (§3.3) and (b) we reuse the existing global `attention_window` (§5).
- **Single timeline by design**: AW intersects separate window/AFK buckets; tt uses one global focus
  timeline. For a single human that is *more* correct (one focus at a time, no double-count) — not a gap.

## Testing Strategy

- **Pure logic, no Wayland**: `WindowFocus` establishing focus + the `resolve_focus_stream` browser
  fallback, idle backdating, window-run synthesis — unit-tested with fixture events (mirrors the
  existing `TestEvent` builders incl. `window_focus()`, `make_event()`, and
  `Database::open_in_memory()`). Snapshot tests for classify run output.
- **Migration**: open a v8 DB fixture → assert it upgrades to v9 with columns added and rows
  preserved; assert fail-fast on a v10 DB.
- **`CosmicBackend`**: thin and isolated behind the trait; manually verified by running
  `tt watch -vvv` (or `--no-write`) and confirming real app-ids/titles + idle transitions on
  COSMIC. (Wayland integration isn't unit-testable; keep the backend dumb and the logic in tested
  pure functions.)

## Implementation Sequence (one spec, sequenced for safety)

1. **Schema + migration** (v8→v9, additive `window_app_id`/`window_title` columns) + `StoredEvent`
   fields + `insert_events` + `build_data_json` mapping `window_app_id`→`data["app"]`,
   `window_title`→`data["title"]`.
2. **`tt watch` + `CosmicBackend`** — capture working end-to-end, verified by querying the DB
   (`tt classify --json` shows the window events; delivers the "stop losing non-terminal time" goal).
3. **Allocation** — make `WindowFocus` establish focus + `resolve_focus_stream` browser fallback; set
   no `attention_window` change (reuse the existing global cap). Smallest integration step.
4. **Classify** — window-run synthesis + `--apply` wiring.
5. **systemd** unit + **relicense tt to `GPL-3.0-only`** (`Cargo.toml` `license`, add `LICENSE`,
   allow in `cargo-deny`).

Overall effort: **Large** (the hard parts are schema/allocation/classify integration, not the
Wayland polling).

## Open Questions / Risks

1. **Idle = zero input for `idle_timeout` (180 s).** `ext-idle-notify-v1` is machine-global and
   resets on any seat input — keyboard, clicks, and **scroll/axis** events (and, on nearly all
   compositors, bare pointer motion). So **scrolling counts as activity**: reading-while-scrolling
   in a browser *or* a tmux pane stays active, and terminal/tmux input is already subsumed by this
   global signal — no separate tmux-scroll capture is needed for the idle determination. The
   caveat is therefore narrow: only *zero-input* watching for >180 s (e.g., a streaming log you
   stare at, or a full page read without scrolling) counts as idle — an inherited AW limitation.
   Acceptable for v1; `idle_timeout` is configurable. **Verify during impl**: confirm `cosmic-comp`
   counts bare pointer *motion* as activity (scroll/click/keys certainly do); COSMIC is young.
2. **Privacy.** Window titles can contain document names, Slack channels, email subjects, customer
   names. All storage is local; titles only ever reach the local classify flow. Note an optional
   title-redaction/filter mechanism as a future addition (AW has one).
3. **COSMIC protocol churn.** `cosmic-toplevel-info` is young (~8 mo, one regression cycle). Pin the
   `cctk` git rev; isolate behind the `WindowBackend` trait so protocol changes don't leak into
   core.
4. **Cross-machine focus is *not* a real conflict.** Allocation uses one global, time-ordered focus
   timeline; the most-recent focus event wins. Because you generate focus events by actually
   focusing something (one human, one keyboard), cross-machine events take turns rather than
   collide — and a single timeline correctly prevents double-counting direct time. The only way to
   mis-attribute is a *spurious* focus event (e.g., a scripted remote `tmux select-pane` while
   you're on the laptop); that is bounded by `attention_window` + idle (a few minutes) and
   self-corrects on your next real focus event. Per-machine focus timelines are explicitly *not*
   pursued — they would risk double-counting. Revisit only if spurious events prove common.


## Related: priorities/todos drift check (separate spec)

The window-run streams produced by classify (§6) are a **direct input** to the priorities/todos
drift check (`specs/design/2026-06-14-priorities-todos-design.md` — a markdown + skill feature that
makes **no tt code or schema changes**). What *this* spec needs to account for:

- **This watcher unblocks the drift check for non-terminal work.** Until window events are captured
  and classified into streams, that drift check is blind to non-terminal time and will falsely flag
  non-terminal-heavy priorities (comms, writing, reading) as under-invested. The classify phase
  (§6 / plan Task 8) is the integration point — the drift check should only be trusted for
  non-terminal categories *after* it lands.
- **Re-normalization caveat:** this watcher increases total tracked time, so every priority's
  *share* of time shifts even when its absolute time is unchanged. Anything consuming
  `tt report --json` for share-of-time comparisons must expect that shift once the watcher is live.
- **Deliberately kept as separate specs.** The two features have opposite shapes (this one changes
  tt internals + schema; the todos feature explicitly changes neither) and contradictory non-goals,
  so they are not merged — they connect only through the time data this watcher enriches.
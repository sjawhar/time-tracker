# Architecture Overview

## System Context

The time tracker runs across two environments:

- **Remote** (dev server): tmux hooks capture focus events; Claude logs are parsed on-demand
- **Local** (laptop): SQLite stores all events; reports and analysis run here

User manually syncs data from remote to local via `tt sync`.

## High-Level Architecture

See [components.md](components.md) for detailed component breakdown and architecture diagram.

**Key simplification:** No daemon on remote. tmux hooks write to a JSONL file; Claude logs are parsed on-demand during sync.

## Key Decisions

| Decision | Summary | ADR |
|----------|---------|-----|
| Event transport | Pull-based sync via SSH | [ADR-001](decisions/001-event-transport.md) |
| Tech stack | Shell stub (remote) + Python (local) | [tech-stack.md](../implementation/tech-stack.md) |
| Event IDs | Deterministic content hash | [data-model.md](../design/data-model.md) |

---

## Implementation Considerations

_Raised during design review. To be addressed during implementation._

### Database Performance

- **WAL mode**: Enable `PRAGMA journal_mode=WAL` and `PRAGMA synchronous=NORMAL` for concurrent read/write
- **Composite indexes**: Consider adding `idx_events_timestamp_type` and `idx_events_stream_timestamp` if single-column indexes prove insufficient
- **Timestamp format**: Standardize on ISO 8601 with consistent precision; consider INTEGER (epoch ms) if performance requires
- **Batch inserts**: Buffer rapid events (e.g., window focus changes) and insert in transactions

### Stream Recomputation

- Recomputation scoped to a time range (e.g., one day), not all-time
- Events with `assignment_source = 'user'` are preserved during recomputation
- `needs_recompute` flag on streams enables lazy recomputation
- Consider storing inference parameters used for computation if reproducibility becomes important

### Event Deduplication

**Decided:** Use deterministic content-based IDs: `id = hash(source + type + timestamp + data)`

This ensures the same logical event always produces the same ID. Import is idempotent via `UNIQUE` constraint on ID — no separate deduplication logic needed.

### Watcher Health Monitoring

- No mechanism to detect if a watcher crashed vs user was idle
- Consider periodic heartbeat events from watchers
- Could add a `source_health` table tracking `last_event_at` per source
- Defer unless debugging becomes difficult

### Attention Allocation Algorithm

**Problem statement**

We need a deterministic, event-sourced algorithm to allocate time when multiple agents run in parallel. The user can only give direct attention to one context at a time, but delegated work (agents running) can overlap. The system must attribute:
- **Direct time**: user attention to exactly one stream at any instant.
- **Delegated time**: agent work time to one or more streams, regardless of user focus.

**Research findings (informal)**
- ActivityWatch uses AFK detection to suppress idle time.
- WakaTime uses heartbeat-style events to treat recent activity as continuous work.
- Toggl and ManicTime emphasize idle handling and retroactive AFK correction.

**Proposed approach (interval-based state model)**

Build a merged, ordered timeline of events. Between each pair of event timestamps, treat state as constant and allocate time to streams based on the current state.

State tracked over time:
- `afk_state`: true/false.
- `window_focus`: app + title.
- `tmux_pane_focus`: pane id + stream id (if known).
- `browser_tab`: url/title + stream id (if known).
- `last_attention_at`: timestamp of last attention signal.
- `agent_sessions`: per stream, active if between `agent_session:started` and `agent_session:ended`.
- `last_agent_activity_at`: per stream, timestamp of last `agent_tool_use` or `agent_message`.

**Attention signals (refresh `last_attention_at`)**
- `user_message`
- `tmux_scroll`
- `tmux_pane_focus`
- `window_focus`
- `browser_tab`
- local editor typing/heartbeat events (when available)

**Direct time rules**
1. Direct time accrues only when `afk_state == false`.
2. Direct time accrues only if `now - last_attention_at <= ATTENTION_WINDOW`.
3. Direct time is attributed to exactly one stream, determined by focus hierarchy:
   - If `window_focus` is terminal: use `tmux_pane_focus` stream (if known).
   - Else if `window_focus` is browser: use `browser_tab` stream (if known).
   - Else: use `window_focus` stream (app/title-based mapping).
4. If focus is unknown or no stream can be resolved, direct time is recorded as `Unattributed` (null stream).

**Delegated time rules**
1. Delegated time accrues to any stream with an active agent session.
2. If an explicit `agent_session:ended` is missing, keep the session active while
   `now - last_agent_activity_at <= AGENT_ACTIVITY_WINDOW`.
3. Delegated time is **not** suppressed by `afk_state` (agents can work while user is AFK).
4. Delegated time can overlap across multiple streams.

**AFK handling**
- Respect AFK state for direct time (no direct time while AFK).
- Allow retroactive AFK correction: when an AFK event arrives after a threshold,
  reclassify the preceding interval as AFK and zero direct time for that interval.

**Parameters (MVP constants)**
- `ATTENTION_WINDOW = 120s`
- `AGENT_ACTIVITY_WINDOW = 300s`

**Edge cases and failure modes**
- Missing focus events: direct time becomes `Unattributed` until a focus event arrives.
- Terminal not focused: tmux pane focus is ignored; browser/window focus drives attribution.
- Multiple active agents: delegated time overlaps; direct time remains single-stream.
- Delayed AFK transition: retroactively adjust prior interval to avoid counting idle time.
- Sparse activity signals: attention window prevents direct time from dropping to zero between events.

**Acceptance criteria**
- Deterministic: given the same ordered event stream, allocations are identical.
- No direct time during AFK intervals.
- Direct time is never split between streams at the same instant.
- Delegated time continues while agents are active, even if user is AFK.
- Attribution is recomputable from raw events without external state.

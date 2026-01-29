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
| Tech stack | Rust everywhere | [tech-stack.md](../implementation/tech-stack.md) |
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

This section defines how time is attributed to streams when the user is working with one or more AI agents.

#### Definitions

| Term | Definition |
|------|------------|
| **Direct time** | Time when the user is actively focused on a stream—viewing, typing, scrolling, or interacting with an agent |
| **Delegated time** | Time when an AI agent is working autonomously on behalf of the user |
| **Focus state** | Which stream currently has the user's attention (exclusive—only one at a time) |
| **Agent activity** | Whether an agent session is active and working (non-exclusive—multiple agents can run in parallel) |
| **Attention window** | Grace period after last focus event before direct time pauses (distinct from AFK detection) |
| **Timeline end** | The timestamp used to close open intervals when calculating time |

#### Core Principles

1. **Direct time follows focus** — Only one stream receives direct time at any moment. Users can only look at one thing at a time.

2. **Delegated time is additive** — All streams with active agents accumulate delegated time simultaneously. Multiple agents can work in parallel.

3. **Direct and delegated can overlap** — When you're focused on a stream while its agent works, you get both. This represents supervised agent work.

4. **AFK pauses direct only** — When the user is idle, direct time stops but delegated time continues (agents keep working).

5. **Uncategorized events excluded** — Events with `stream_id = null` do not contribute to time calculations until assigned to a stream.

#### Algorithm

**Step 1: Build focus timeline**

Process events chronologically to construct a list of focus state transitions:

```
Events:
  09:00:00 - tmux_pane_focus(pane=%1, cwd=/project-a)
  09:15:00 - tmux_pane_focus(pane=%2, cwd=/project-b)
  09:30:00 - afk_change(status=idle)
  09:45:00 - afk_change(status=active)
  09:45:01 - tmux_pane_focus(pane=%2, cwd=/project-b)

Focus timeline:
  [09:00:00, 09:15:00) → Stream A (direct)
  [09:15:00, 09:30:00) → Stream B (direct)
  [09:30:00, 09:45:00) → AFK (no direct)
  [09:45:00, ...)      → Stream B (direct)
```

Focus state changes on:
- `tmux_pane_focus` → focus shifts to that pane's stream
- `window_focus` to terminal → maintain current tmux pane focus; if none exists, wait for next `tmux_pane_focus`
- `window_focus` to non-terminal → focus shifts to local.window stream (if AW enabled)
- `afk_change` (idle) → focus clears, direct time pauses
- `afk_change` (active) → does NOT restore focus; wait for next focus event
- `tmux_scroll` or `user_message` → confirms focus, resets attention window timer

**Attention window vs AFK detection**: These are distinct mechanisms:
- **AFK detection** (`afk_threshold_ms`, default 5 min): Detects when user is physically away. Fires `afk_change(idle)` after no keyboard/mouse activity. This is handled by the AFK watcher.
- **Attention window** (`attention_window_ms`, default 1 min): Extends direct time after the last focus-related event (pane focus, scroll, message). This handles reading/thinking within a focused context.

Example: User focuses on Stream A at 09:00, then reads output without interacting. At 09:01, attention window expires—direct time pauses. At 09:05, AFK fires—but direct time already stopped at 09:01. The attention window is the tighter constraint for focus-based work.

**Step 2: Build agent activity timeline**

Process events chronologically to determine agent active intervals per stream.

**Agent-to-stream mapping**: Agent sessions are mapped to streams based on the `cwd` field in the `agent_session` event. The session's `cwd` is matched against streams that contain events with the same working directory. If no matching stream exists yet, a new stream is created for that `cwd`.

```
Events:
  09:00:00 - agent_session(action=started, session_id=abc, cwd=/project-x)
  09:05:00 - agent_tool_use(session_id=abc, tool=Edit)
  09:20:00 - agent_session(action=ended, session_id=abc)

Agent activity timeline (Stream for /project-x):
  [09:05:00, 09:20:00) → agent active
```

Agent state transitions:
- First `agent_tool_use` after `agent_session(started)` → agent becomes active (delegated time starts)
- `agent_session(ended)` → agent becomes inactive
- No `agent_tool_use` for `agent_timeout_ms` after last tool use → assume session ended (crashed)

**Important**: Delegated time starts at the first `agent_tool_use`, not at session start. A session without any `agent_tool_use` event contributes 0 delegated time.

**Step 3: Allocate time**

Iterate through event-to-event intervals. For each interval `[t₀, t₁)`:

1. Determine focus state at t₀
2. Determine which agents are active at t₀
3. Calculate interval duration: `duration = t₁ - t₀`
4. Attribute time:
   - If focused stream exists and not AFK: add `duration` to that stream's `time_direct_ms`
   - For each stream with active agent: add `duration` to that stream's `time_delegated_ms`

**Timeline end**: The last event in the query range determines where open intervals close. For reporting a time period (e.g., "this week"):
- Query events where `timestamp >= period_start AND timestamp < period_end`
- The final interval extends from last event to `min(period_end, last_event + attention_window_ms)` for focus, or to `period_end` for active agents
- Open agent sessions at period end contribute delegated time up to `period_end`

This ensures reports are deterministic and don't change based on when they're generated.

#### Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `attention_window_ms` | 60000 (1 min) | After last focus/scroll/message event, continue attributing direct time for this duration |
| `agent_timeout_ms` | 1800000 (30 min) | If no `agent_tool_use` for this duration after the most recent tool use, assume session crashed. Session ends at last tool use timestamp. |

#### Time Attribution Rules

| Scenario | Direct Time | Delegated Time |
|----------|-------------|----------------|
| User focused on Stream A, no agents running | A | — |
| User focused on Stream A, agent working in A | A | A |
| User focused on Stream A, agent working in B | A | B |
| User focused on Stream A, agents in A and B | A | A, B |
| User AFK, agent working in A | — | A |
| User focused on browser, agent working in A | browser stream | A |
| No focus events, agent working in A | — | A |

#### Edge Cases

**1. Non-agent terminal work**

User focused on a pane without any agent running still receives direct time. Agents are not required for direct time attribution.

**2. Multiple panes, same stream**

Stream inference groups panes by working directory. Multiple panes with the same `cwd` belong to the same stream. Focus on any pane in the stream counts as focus on the stream.

**3. Agent works faster than focus switches**

Agent session may complete before user switches focus to observe it. Delegated time is attributed for the full session duration regardless of user focus. Direct time depends on where the user was actually focused.

**4. Rapid focus switching**

The tmux hook debounces focus events (500ms minimum between events for the same pane). After debouncing, even short focus periods are counted. There is no minimum duration threshold—every interval contributes.

**5. User focused on non-tracked application**

If the terminal loses focus but agents continue running, delegated time continues accumulating. Direct time either goes to a local.window stream (if ActivityWatch watchers are enabled) or pauses.

**6. No explicit agent session end**

Claude sessions may crash or be killed without emitting a proper close event. Use timeout: if no `agent_tool_use` for `agent_timeout_ms` after the most recent tool use, assume the session ended at that last `agent_tool_use` timestamp. The timeout is measured from the last tool use, not from session start.

**7. Agent session with no tool use**

A session that starts but never uses any tools (immediate crash, user killed it, etc.) contributes 0 delegated time. At least one `agent_tool_use` is required to confirm the agent actually worked.

#### Reporting Implications

**Total time calculation**: Direct and delegated time can overlap, so they should NOT be summed naively. The report's "Total tracked" represents wall-clock time: the duration of the union of all intervals where any stream received either direct or delegated time.

**Example report section:**
```
SUMMARY
───────
Total tracked:  2h 30m
Direct time:    1h 45m (70%)
Delegated time: 1h 30m (60%)
```

Note: Percentages can exceed 100% combined because they overlap during supervised agent work.

#### Examples

**Example 1: Single agent, user focused**

```
09:00 - User focuses on pane in /project-x
09:00 - Agent session starts in /project-x
09:05 - Agent uses Edit tool (delegated time begins here)
09:30 - Agent session ends
09:45 - User switches to different pane

Stream X:
  Direct:    45 min (09:00 - 09:45)
  Delegated: 25 min (09:05 - 09:30)
```

Note: Delegated time starts at 09:05 (first tool use), not 09:00 (session start).

**Example 2: Multiple agents, user switches focus**

```
09:00 - User focuses on pane in /project-x
09:00 - Agent A starts in /project-x
09:05 - Agent A uses tool (delegated time for X begins)
09:10 - Agent B starts in /project-y
09:15 - Agent B uses tool (delegated time for Y begins)
09:20 - User switches focus to /project-y
09:30 - Agent A ends
09:40 - Agent B ends
09:50 - User goes AFK

Stream X:
  Direct:    20 min (09:00 - 09:20)
  Delegated: 25 min (09:05 - 09:30)

Stream Y:
  Direct:    30 min (09:20 - 09:50)
  Delegated: 25 min (09:15 - 09:40)
```

**Example 3: User AFK while agent works**

```
09:00 - User focuses on /project-x
09:00 - Agent starts
09:05 - Agent uses tool (delegated time begins)
09:10 - User goes AFK
09:30 - Agent ends
09:45 - User returns, focuses on /project-x

Stream X:
  Direct:    10 min (09:00 - 09:10), then from 09:45 onward
  Delegated: 25 min (09:05 - 09:30)
```

#### Acceptance Criteria

1. Direct time is exclusive: at most one stream receives direct time at any moment
2. Delegated time is concurrent: multiple streams can accumulate delegated time simultaneously
3. AFK periods contribute 0 direct time but delegated time continues
4. Agent sessions without tool use contribute 0 delegated time
5. Total tracked time equals wall-clock time (union of intervals), not sum of direct + delegated
6. User corrections (`assignment_source = 'user'`) are preserved through recomputation

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

Defines how time is attributed to streams when the user works with multiple AI agents in parallel.

#### Definitions

| Term | Definition |
|------|------------|
| **Direct time** | Time the user actively attends to a stream (reading, typing, scrolling) |
| **Delegated time** | Time an agent spends working on a stream, regardless of user focus |
| **Focus state** | The current target of user attention (a stream, or null) |
| **Active session** | An agent session that has started but not yet ended |
| **Attention window** | Grace period after last user activity before assuming user stopped attending |

#### Core Principle

**Direct attention is exclusive; delegated work is parallel.**

At any moment:
- Direct time accrues to exactly one stream (the one with focus), or none
- Delegated time accrues to all streams with active agent sessions

This matches reality: the user can only look at one thing at a time, but multiple agents can work simultaneously.

#### Focus State Structure

```
FocusState {
    current_stream: StreamId | null           // Where direct attention goes
    active_sessions: Set<SessionId>           // Which agent sessions are running
    session_last_event: Dict<SessionId, Timestamp>  // For session timeout detection
    last_activity: Timestamp                  // For idle detection
    is_afk: bool                              // Away from keyboard
}
```

#### Event-Driven State Transitions

| Event Type | State Update | Rationale |
|------------|--------------|-----------|
| `tmux_pane_focus` | `current_stream` ← stream for pane's cwd (or null if unmapped), `last_activity` ← now | User switched panes |
| `window_focus` (terminal) | Restore previous `current_stream` if available, `last_activity` ← now | Terminal active; restoring prevents lost time when returning to same pane |
| `window_focus` (non-terminal) | `current_stream` ← null | User left terminal; no stream gets direct time |
| `tmux_scroll` | Confirm `current_stream`, `last_activity` ← now | User reading output |
| `user_message` | `current_stream` ← message's session's stream, `last_activity` ← now | Definitive attention proof |
| `agent_session` (started) | Add session to `active_sessions`, record in `session_last_event` | Agent began working |
| `agent_session` (ended) | Remove session from `active_sessions` | Agent finished |
| `agent_tool_use` | Update `session_last_event[session_id]` | Agent still active (prevents timeout) |
| `afk_change` (idle) | `is_afk` ← true | User stepped away |
| `afk_change` (active) | `is_afk` ← false, `last_activity` ← now | User returned |
| `manual_time_block` | Treated as focus event for block's duration | User declared work period |

**Event Priority:** When events have identical timestamps: `user_message` > `tmux_pane_focus` > `tmux_scroll` > other events. When grouping by timestamp, only the highest-priority event updates mutually-exclusive state fields (like `current_stream`). Non-conflicting updates (like adding to `active_sessions`) all apply.

**Unmapped cwd:** If `tmux_pane_focus` cwd doesn't map to any known stream, set `current_stream = null`. This handles utility panes (htop, logs) that aren't project work — direct time stops accruing until the user switches to a recognized project directory.

#### Session-to-Stream Mapping

Agent sessions are mapped to streams via the events that belong to them:

1. Find all events with the given `session_id` that have a non-null `stream_id`
2. Return the `stream_id` from the earliest such event
3. If no events have a `stream_id` yet (stream inference hasn't run), fall back to inferring from `cwd`

**Important:** Time calculation should run after stream inference. If called before inference, sessions with `stream_id = null` events have their delegated time discarded.

**`agent_session(started)` events:** Should include `cwd` in their payload to enable stream inference. The stream is determined by clustering logic (see Stream Inference), not stored directly on the session.

#### Time Calculation Algorithm

```python
# Event types that indicate user activity (for seeding last_activity)
ACTIVITY_EVENT_TYPES = ['user_message', 'tmux_pane_focus', 'tmux_scroll', 'afk_change']

# Event types that determine focus (for seeding current_stream)
FOCUS_EVENT_TYPES = ['user_message', 'tmux_pane_focus', 'window_focus', 'manual_time_block']

# Event priority for same-timestamp resolution (higher = wins)
EVENT_PRIORITY = {'user_message': 4, 'tmux_pane_focus': 3, 'tmux_scroll': 2}

def calculate_time(events: List[Event], start: Timestamp, end: Timestamp) -> Dict[StreamId, TimeBreakdown]:
    results = defaultdict(lambda: TimeBreakdown(direct_ms=0, delegated_ms=0))

    # 1. Initialize state from most recent events BEFORE the query window
    state = seed_initial_state(events, start)

    # 2. Build timeline of state changes within the window
    window_events = [e for e in events if start <= e.timestamp <= end]

    # Group by timestamp, apply only highest-priority for conflicting updates
    timeline = [(start, copy(state))]
    for ts, group in groupby(sorted(window_events, key=lambda e: e.timestamp), key=lambda e: e.timestamp):
        events_at_ts = sorted(group, key=lambda e: -EVENT_PRIORITY.get(e.type, 0))
        focus_set = False
        for event in events_at_ts:
            # For focus-setting events, only apply the highest-priority one
            if event.type in FOCUS_EVENT_TYPES:
                if not focus_set:
                    apply_state_transition(state, event)
                    focus_set = True
                # Skip lower-priority focus events
            else:
                # Non-focus events (session start/end, tool_use) always apply
                apply_state_transition(state, event)
        timeline.append((ts, copy(state)))

    timeline.append((end, None))  # Sentinel for final interval

    # 3. Calculate time for each interval
    for i in range(len(timeline) - 1):
        t, state = timeline[i]
        next_t = timeline[i + 1][0]
        interval_ms = (next_t - t).total_milliseconds()

        # Prune stale sessions (no events for SESSION_TIMEOUT)
        prune_stale_sessions(state, t)

        # Direct time: only when NOT AFK and NOT idle
        if not state.is_afk and state.current_stream and not is_idle(state, t):
            results[state.current_stream].direct_ms += interval_ms

        # Delegated time: accrues even during AFK (agents keep working)
        for session_id in state.active_sessions:
            stream = get_stream_for_session(session_id)
            if stream:
                results[stream].delegated_ms += interval_ms

    return results

def is_idle(state: FocusState, t: Timestamp) -> bool:
    return (t - state.last_activity).total_milliseconds() > ATTENTION_WINDOW_MS

def prune_stale_sessions(state: FocusState, t: Timestamp) -> None:
    """Remove sessions that haven't had any events for SESSION_TIMEOUT_MS."""
    stale = [
        sid for sid, last_event in state.session_last_event.items()
        if (t - last_event).total_milliseconds() > SESSION_TIMEOUT_MS
    ]
    for sid in stale:
        state.active_sessions.discard(sid)
        del state.session_last_event[sid]

def seed_initial_state(events: List[Event], start: Timestamp) -> FocusState:
    """Find the most recent events before `start` to initialize state."""
    state = FocusState(
        current_stream=None,
        active_sessions=set(),
        session_last_event={},
        last_activity=start,  # Will be updated below if activity found
        is_afk=False          # Will be updated below if afk_change found
    )

    pre_start_events = [e for e in events if e.timestamp < start]

    # Find most recent focus event → set current_stream
    focus_events = [e for e in pre_start_events if e.type in FOCUS_EVENT_TYPES]
    if focus_events:
        latest_focus = max(focus_events, key=lambda e: e.timestamp)
        apply_state_transition(state, latest_focus)

    # Find most recent activity event → set last_activity
    activity_events = [e for e in pre_start_events if e.type in ACTIVITY_EVENT_TYPES]
    if activity_events:
        latest_activity = max(activity_events, key=lambda e: e.timestamp)
        state.last_activity = latest_activity.timestamp

    # Find most recent afk_change → set is_afk
    afk_events = [e for e in pre_start_events if e.type == 'afk_change']
    if afk_events:
        latest_afk = max(afk_events, key=lambda e: e.timestamp)
        state.is_afk = (latest_afk.data['status'] == 'idle')

    # Find all sessions that started before `start` and haven't ended
    for e in sorted(pre_start_events, key=lambda e: e.timestamp):
        if e.type == 'agent_session':
            sid = e.data['session_id']
            if e.data['action'] == 'started':
                state.active_sessions.add(sid)
                state.session_last_event[sid] = e.timestamp
            elif e.data['action'] == 'ended':
                state.active_sessions.discard(sid)
                state.session_last_event.pop(sid, None)
        elif e.type == 'agent_tool_use' and 'session_id' in e.data:
            sid = e.data['session_id']
            if sid in state.active_sessions:
                state.session_last_event[sid] = e.timestamp

    return state
```

#### Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `ATTENTION_WINDOW_MS` | 120000 (2 min) | Grace period after last activity before assuming idle |
| `AFK_THRESHOLD_MS` | 300000 (5 min) | Threshold for AFK watcher (configured externally) |
| `SESSION_TIMEOUT_MS` | 1800000 (30 min) | Assume session ended if no events for this duration |

**Rationale for 120s attention window:** Users often read long agent output without providing input. 60s was too aggressive; 120s balances accuracy with practicality.

#### Invariants

These properties must hold for any calculation:

1. `sum(direct_ms across all streams) ≤ wall_clock_time` — Direct attention is exclusive
2. `direct_ms = 0` when `is_afk = true` for entire range — No direct time while AFK
3. `delegated_ms` can exceed `wall_clock_time` — Multiple agents work in parallel
4. Events with `stream_id = null` contribute to no stream's time
5. User corrections (`assignment_source = 'user'`) are respected; algorithm uses assigned stream

#### Edge Cases

| Scenario | Behavior |
|----------|----------|
| **Stale session** (no `ended` event) | Session auto-removed from `active_sessions` after `SESSION_TIMEOUT_MS` without events |
| **Query starts mid-activity** | State seeded from most recent focus/activity/afk events before query window |
| **Same-timestamp events** | Highest-priority event wins for focus; non-conflicting updates all apply |
| **No focus events for a session** | Session gets `direct_ms = 0`, `delegated_ms = agent_runtime` |
| **AFK during agent work** | Direct time stops; delegated time continues until session ends |
| **Non-terminal focus** (browser) | `current_stream = null`; no stream gets direct time |
| **Manual time block** | Expands to synthetic focus-start and focus-end events during preprocessing |
| **Unmapped cwd** | Utility panes (htop, logs) set `current_stream = null` |
| **Terminal focus without pane event** | If returning to same pane, `window_focus(terminal)` restores previous `current_stream` |
| **Agent session with no user interaction** | Gets `delegated_ms` only; no direct time attributed |
| **Clock skew between machines** | Events may appear out of order. Clocks must be NTP-synchronized. |

#### Examples

**Example 1: Single Agent Session**

```
10:00:00  user_message(session=A, stream=S1)     → focus=S1, active={A}
10:00:30  agent_tool_use(session=A)              → (no change)
10:05:00  agent_session(A, ended)                → active={}

Result for S1:
  direct_ms  = 120000  (10:00:00 to 10:02:00, then idle timeout)
  delegated_ms = 300000  (10:00:00 to 10:05:00)
```

**Example 2: Three Parallel Agents with Focus Switches**

```
10:00:00  user_message(session=A, stream=S1)     → focus=S1, active={A}, last_activity=10:00
10:01:00  agent_session(B, started, stream=S2)  → active={A,B}
10:02:00  tmux_pane_focus(cwd→S2)               → focus=S2, last_activity=10:02
10:03:00  agent_session(C, started, stream=S3)  → active={A,B,C}
10:04:00  tmux_scroll                            → last_activity=10:04
10:10:00  (all sessions ended)                   → active={}

Result:
  S1: direct=120000 (10:00-10:02), delegated=600000 (10:00-10:10)
  S2: direct=240000 (10:02-10:06, idle at 10:06 = 10:04 + 2min), delegated=540000 (10:01-10:10)
  S3: direct=0 (never focused), delegated=420000 (10:03-10:10)

Note: S1 direct time is 2 minutes (the attention window) from 10:00. S2 direct time is 4 minutes:
10:02-10:04 (2 min active typing/scrolling) + 10:04-10:06 (2 min attention window grace period).
```

**Example 3: AFK Period with Running Agents**

```
10:00:00  user_message(session=A, stream=S1)     → focus=S1, active={A}
10:02:00  afk_change(idle)                       → is_afk=true
10:15:00  afk_change(active)                     → is_afk=false
10:15:30  agent_session(A, ended)                → active={}

Result for S1:
  direct_ms  = 120000  (10:00:00 to 10:02:00)
  delegated_ms = 930000  (10:00:00 to 10:15:30)
```

#### Performance

**Expected scale:**
- Typical day: 10,000 events
- Monthly: 100,000 events
- Query latency: <1s for weekly report on 100K events

**Optimization:** Computation is scoped to the query time range. Events outside the range are only used for initial state seeding (finding most recent focus event).

#### Materialized Totals

The `Stream.time_direct_ms` and `Stream.time_delegated_ms` fields are materialized totals computed by running this algorithm with the range `(stream.first_event_at, now)`. They are recomputed when `needs_recompute = true`.

#### UX Considerations

These issues were raised during review and should be addressed in the UX specs:

1. **Terminology:** "Direct" and "Delegated" are abstract. Consider renaming to "Hands-on" and "Agent work" in user-facing output (see `ux-reports.md`).

2. **Unattributed time visibility:** Time spent on non-terminal windows (browser research) is not attributed to any stream. Consider showing this as a separate row in reports.

3. **Session timeout warnings:** When sessions are auto-pruned due to `SESSION_TIMEOUT_MS`, consider showing a warning in `tt status` so users understand why delegated time stopped early.

4. **Multi-tag math explanation:** When tag totals exceed report total (due to multi-tagged streams), add a footer explaining this in the report output.

---

## Design Decisions

### Why Not Fractional Attribution?

Early brainstorming proposed a weighted/decay model that would allocate fractional attention across streams. This was rejected because:

1. **Complexity without benefit** — Users expect whole-number time allocation
2. **Hard to explain** — "Why did stream A get 38.6% of my attention?" is confusing
3. **Focus-based is simpler** — Maps directly to user mental model
4. **Matches reality** — You really can only look at one screen at a time

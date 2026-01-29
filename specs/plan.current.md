# Plan: Implement direct/delegated time calculation

## Task

From `specs/plan.md`:
> - [ ] Implement direct/delegated time calculation

## Spec Reference

The attention allocation algorithm is fully specified in `specs/architecture/overview.md` (lines 59-289). Key definitions:

- **Direct time**: Time when user is actively focused on a stream
- **Delegated time**: Time when an AI agent is working autonomously

## Acceptance Criteria (from spec)

1. Direct time is exclusive: at most one stream receives direct time at any moment
2. Delegated time is concurrent: multiple streams can accumulate delegated time simultaneously
3. AFK periods contribute 0 direct time but delegated time continues
4. Agent sessions without tool use contribute 0 delegated time
5. Total tracked time equals wall-clock time (union of intervals), not sum of direct + delegated
6. User corrections (`assignment_source = 'user'`) are preserved through recomputation

## Algorithm Summary

### Step 1: Build focus timeline

Process events chronologically to construct focus state transitions:
- `tmux_pane_focus` → focus shifts to that pane's stream
- `afk_change(idle)` → focus clears, direct time pauses
- `afk_change(active)` → does NOT restore focus; wait for next focus event
- `tmux_scroll` or `user_message` → confirms focus, resets attention window timer

### Step 2: Build agent activity timeline

Map agent sessions to streams by `cwd`:
- First `agent_tool_use` after `agent_session(started)` → agent becomes active
- `agent_session(ended)` → agent becomes inactive
- No `agent_tool_use` for `agent_timeout_ms` → assume session crashed

### Step 3: Allocate time

For each event-to-event interval `[t₀, t₁)`:
1. Determine focus state at t₀
2. Determine which agents are active at t₀
3. Calculate interval duration: `duration = t₁ - t₀`
4. Attribute time:
   - If focused stream exists and not AFK: add `duration` to that stream's `time_direct_ms`
   - For each stream with active agent: add `duration` to that stream's `time_delegated_ms`

### Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `attention_window_ms` | 60000 (1 min) | Grace period after last focus/scroll/message event |
| `agent_timeout_ms` | 1800000 (30 min) | Assume session crashed if no tool use |

## Implementation Approach

Follow the existing pattern from stream inference (`tt-core/src/inference.rs`):
1. **Core algorithm in tt-core** as a pure function
2. **Trait for event access** to work with both test fixtures and StoredEvent
3. **CLI command orchestration** in tt-cli

**Important sequencing:** Allocation runs *after* stream inference. Events need `stream_id` assigned before time can be attributed.

### File Changes

#### 1. New: `crates/tt-core/src/allocation.rs` (~350 lines)

**Types:**
```rust
/// Configuration for time allocation.
pub struct AllocationConfig {
    /// Grace period after last focus event (default: 60s)
    pub attention_window_ms: i64,
    /// Agent timeout for crashed sessions (default: 30min)
    pub agent_timeout_ms: i64,
}

/// Computed time for a single stream.
pub struct StreamTime {
    pub stream_id: String,
    pub time_direct_ms: i64,
    pub time_delegated_ms: i64,
}

/// Result of time allocation calculation.
pub struct AllocationResult {
    /// Time computed per stream.
    pub stream_times: Vec<StreamTime>,
    /// Total wall-clock time with any activity (union of intervals, not sum).
    pub total_tracked_ms: i64,
}
```

**Trait for event access:**
```rust
/// An event suitable for time allocation.
pub trait AllocatableEvent {
    fn timestamp(&self) -> DateTime<Utc>;
    fn event_type(&self) -> &str;
    fn stream_id(&self) -> Option<&str>;
    fn session_id(&self) -> Option<&str>;
    fn data(&self) -> &serde_json::Value;
}
```

Keeping `data()` in the trait is pragmatic—JSON parsing is trivial (2-3 field accesses), and test fixtures work fine with `json!()` following the pattern in `inference.rs`.

**Core function:**
```rust
/// Calculate time allocation for a time range.
///
/// Events must be sorted by timestamp ascending.
/// Events with `stream_id = None` are excluded from time attribution.
///
/// `period_end` specifies where to close open intervals. If None, uses
/// last event timestamp + attention_window for focus, or last event for agents.
pub fn allocate_time<E: AllocatableEvent>(
    events: &[E],
    config: &AllocationConfig,
    period_end: Option<DateTime<Utc>>,
) -> AllocationResult
```

**Internal data structures:**
- `FocusState` enum: `Focused { stream_id, last_activity_at }` | `Unfocused { since }`
- `AgentSession` struct: `session_id`, `stream_id`, `first_tool_use_at`, `last_tool_use_at`, `ended`
- Agent sessions tracked by `session_id` in a HashMap
- `ActivityInterval` struct for tracking intervals for total time union calculation

**Algorithm implementation:**
1. Single pass through events
2. On each event, first check for lazy attention window expiry (no synthetic events)
3. Close previous interval and attribute time based on current state
4. Update state machine based on event type
5. Handle agent timeout lazily (check gap since last tool use when processing next event)
6. Finalization: close open intervals at `period_end` (or computed endpoint)

**Lazy attention window expiry (instead of synthetic events):**
```rust
fn check_attention_expiry(&mut self, current_time: DateTime<Utc>) {
    if let FocusState::Focused { stream_id, last_activity_at } = &self.focus_state {
        let window_end = *last_activity_at + Duration::milliseconds(self.config.attention_window_ms);
        if current_time > window_end {
            // Close direct time interval at window_end (not current_time)
            self.attribute_direct_time(stream_id, *last_activity_at, window_end);
            self.focus_state = FocusState::Unfocused { since: window_end };
        }
    }
}
```

This avoids synthetic events and handles the gap correctly—time between attention window expiry and next event is not attributed.

**Total tracked time calculation:**
Track all `(start, end)` intervals (both direct and delegated), merge overlapping intervals, sum merged durations. This gives wall-clock time per acceptance criteria #5.

#### 2. Modify: `crates/tt-core/src/lib.rs`

Add module and re-export:
```rust
mod allocation;
pub use allocation::{allocate_time, AllocatableEvent, AllocationConfig, AllocationResult, StreamTime};
```

#### 3. Modify: `crates/tt-db/src/lib.rs`

Implement `AllocatableEvent` for `StoredEvent`:
```rust
impl tt_core::AllocatableEvent for StoredEvent {
    fn timestamp(&self) -> DateTime<Utc> { self.timestamp }
    fn event_type(&self) -> &str { &self.event_type }
    fn stream_id(&self) -> Option<&str> { self.stream_id.as_deref() }
    fn session_id(&self) -> Option<&str> { self.session_id.as_deref() }
    fn data(&self) -> &serde_json::Value { &self.data }
}
```

Add method to update stream times:
```rust
/// Updates time fields for multiple streams.
pub fn update_stream_times(&self, times: &[StreamTime]) -> Result<u64, DbError>
```

#### 4. New: `crates/tt-cli/src/commands/recompute.rs` (~80 lines)

CLI command to trigger time recomputation:
```rust
/// Recompute time allocation for streams.
pub fn run(db: &Database, force: bool) -> Result<()>
```

Flow:
1. If `force`, fetch all events; otherwise fetch events for streams with `needs_recompute=true`
2. Call `allocate_time()` with events
3. Update streams with `db.update_stream_times()`
4. Clear `needs_recompute` flag

#### 5. Modify: `crates/tt-cli/src/commands/mod.rs`

Add `pub mod recompute;`

#### 6. Modify: `crates/tt-cli/src/main.rs`

Add `recompute` subcommand with `--force` flag.

## Test Cases

### Unit tests in `tt-core/src/allocation.rs`

1. **Single stream, continuous focus**
   - 3 focus events 5min apart → 10min direct time

2. **Focus switches between streams**
   - Focus A at 0, focus B at 10min → A gets 10min, B gets time until timeline end

3. **AFK pauses direct time**
   - Focus at 0, AFK(idle) at 10min, AFK(active) at 15min → 10min direct time (not 15min)

4. **AFK active doesn't restore focus**
   - Focus A at 0, AFK(idle) at 5min, AFK(active) at 10min, no focus event → no direct time after 5min

5. **Single agent session**
   - agent_session(started) at 0, agent_tool_use at 5min, agent_session(ended) at 30min → 25min delegated

6. **Agent session with no tool use**
   - agent_session(started) at 0, agent_session(ended) at 30min → 0 delegated time

7. **Agent timeout (crashed session)**
   - agent_session(started) at 0, agent_tool_use at 5min, no end event, next event at 60min → delegated ends at 35min (5+30)

8. **Concurrent agents in different streams**
   - Two agents running simultaneously → both accumulate delegated time

9. **User focused while agent works**
   - Focus on stream A, agent in A → both direct and delegated

10. **Attention window expiry**
    - Focus at 0, no further events, attention window 1min → direct time caps at 1min

11. **Scroll resets attention window**
    - Focus at 0, scroll at 30s, attention window 1min → direct time extends to 1min 30s

12. **Events in unfocused streams**
    - Events in stream B while focused on A → B gets no direct time (unless agent)

13. **Events with stream_id = null excluded**
    - Events not assigned to any stream → 0 direct, 0 delegated

14. **Combined focus + agent + AFK**
    - Focus A at 0, agent A starts at 0, tool use at 5min, AFK at 10min, agent ends at 30min
    - Direct: 10min (0-10), Delegated: 25min (5-30)
    - Validates: AFK pauses direct but not delegated

15. **Total tracked time (interval union)**
    - Direct 0-10min in A, Delegated 5-20min in A
    - Total tracked: 20min (union of [0,10) and [5,20) = [0,20))

### Integration test: CLI command

Test `recompute` command with in-memory database:
- Insert events, run inference, run recompute
- Verify stream times updated correctly
- Verify `needs_recompute` flag cleared

## Questions Resolved

1. **Which events trigger focus changes?**
   - `tmux_pane_focus` (primary), `afk_change`, `tmux_scroll`, `user_message`
   - Window focus handled via tmux pane focus (terminal has focus → look at tmux state)

2. **How to map agent sessions to streams?**
   - Use `cwd` field in `agent_session` event to match stream (same as inference)
   - Agent events already have `session_id` for tracking

3. **How to handle timeline boundaries?**
   - Query period has explicit start/end
   - Open intervals close at `min(period_end, last_activity + attention_window)`
   - Active agents at period end contribute until period_end

4. **Recomputation scope?**
   - Default: only streams with `needs_recompute=true`
   - `--force`: all streams

5. **What is `period_end` and where does it come from?**
   - Caller (CLI command) specifies the period for reporting
   - `allocate_time()` accepts optional `period_end` parameter
   - If None: last event + attention_window for focus, last event for agents
   - If Some: close open intervals at period_end

6. **Sequencing with inference?**
   - Allocation runs after inference
   - Events need `stream_id` assigned before time can be attributed
   - Events with `stream_id = null` are excluded from time calculations

7. **Attention window vs AFK - which fires first?**
   - Independent mechanisms, both can fire
   - Whichever comes first closes direct time
   - Default: attention window (1min) is tighter than AFK (5min)

## Checklist

- [x] Spec exists and is complete (`specs/architecture/overview.md` lines 59-289)
- [x] Files to change identified
- [x] Test cases outlined
- [x] No open questions remain

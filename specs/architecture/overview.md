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

**Critical TODO**: Define how direct time is allocated when multiple agents run in parallel.

Key insight: User can only interact with one thing at a time, so direct attention follows focus:
- `window_focus` determines if user is in terminal vs browser
- `tmux_pane_focus` determines which pane within terminal
- `tmux_scroll` indicates attention even without typing
- `user_message` is definitive proof of attention

The algorithm should:
1. Track current focus state from focus events
2. Attribute direct time to the stream associated with the focused context
3. Attribute delegated time to all streams with active agents regardless of focus

_This needs detailed specification before implementation._

---

## Preliminary Ideas

> **Note**: The following are preliminary ideas from early brainstorming. They should be validated during architecture design.

### Time Attribution Algorithm Sketch

For any time window [t₀, t₁], calculate attribution as:

```
attribution(context, t₀, t₁) = Σ(event_weight × recency_decay) / total_weight
```

Where:
- **event_weight**: Different events have different weights
  - `pane_focus`: 1.0 (strong signal)
  - `keypress`: 0.5 (activity confirmation)
  - `agent_tool_use`: 0.3 (agent working on context)
  - `agent_message`: 0.2 (agent responding)

- **recency_decay**: Events closer to the time point matter more
  - `decay = exp(-λ × time_since_event)`
  - λ chosen so 50% decay at ~2 minutes

### Example Calculation

```
Time: 10:02:30
Events in last 5 minutes:
  10:00:00 - pane_focus(ctx_A)      weight=1.0, decay=0.3  → 0.30
  10:01:00 - agent_tool_use(ctx_A)  weight=0.3, decay=0.5  → 0.15
  10:01:30 - pane_focus(ctx_B)      weight=1.0, decay=0.6  → 0.60
  10:02:00 - keypress(ctx_B)        weight=0.5, decay=0.8  → 0.40
  10:02:15 - agent_message(ctx_A)   weight=0.2, decay=0.9  → 0.18

Context A: 0.30 + 0.15 + 0.18 = 0.63
Context B: 0.60 + 0.40 = 1.00
Total: 1.63

Attribution at 10:02:30:
  Context A: 0.63/1.63 = 38.6%
  Context B: 1.00/1.63 = 61.4%
```

**Note**: This fractional attribution model may be over-engineered for MVP. Consider simpler approach: direct time goes to focused stream, delegated time to all active streams.

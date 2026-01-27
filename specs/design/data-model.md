# Data Model

## Design Decisions

1. **Event-sourced**: All state derived from immutable events
2. **Materialized streams**: Streams stored explicitly (not just computed views) for fast queries and user corrections, but can be recomputed from events
3. **Attention is derived**: Direct/delegated time computed from raw events, enabling retroactive tuning of inference parameters
4. **Local + remote events**: Local attention from extracted ActivityWatch watchers; remote activity from tmux hooks and agent logs
5. **User corrections preserved**: Manual stream assignments are tracked separately and survive recomputation
6. **Convenience tool, not secure vault**: Prioritize usability over encryption; no sensitive credentials expected in captured data

---

## Entities

### Event

The atomic unit of observation. Immutable once recorded.

**ID generation:** Deterministic hash of content: `id = hash(source + type + timestamp + data)`. This ensures the same logical event always produces the same ID, making imports idempotent without separate deduplication logic.

**Timestamps:** Always UTC with `Z` suffix. Shell hooks must use `date -u +%Y-%m-%dT%H:%M:%SZ`.

```
Event {
  id: UUID (deterministic, content-based)
  timestamp: DateTime (UTC, with Z suffix)
  type: EventType
  source: Source
  schema_version: Integer     # payload schema version (start at 1)
  data: EventData (type-specific payload)

  # Extracted for indexing (duplicated from data for query performance)
  cwd: String | null          # working directory, if applicable
  session_id: String | null   # agent session, if applicable

  # Stream assignment
  stream_id: UUID | null      # assigned stream (null = "Uncategorized")
  assignment_source: 'inferred' | 'user'  # preserves user corrections on recompute
}
```

Events with `stream_id = null` are displayed as "Uncategorized" in the UI, prompting user review.

### Stream

A coherent unit of work, grouping related events.

```
Stream {
  id: UUID
  created_at: DateTime
  updated_at: DateTime

  # Metadata
  name: String | null         # auto-generated or user-provided

  # Computed (materialized for performance, recomputable from events)
  time_direct_ms: Integer     # total human attention time
  time_delegated_ms: Integer  # total agent execution time
  first_event_at: DateTime
  last_event_at: DateTime
  needs_recompute: Boolean    # flag for lazy recomputation
}
```

### Tag

Tags are stored in a junction table for query performance.

```
StreamTag {
  stream_id: UUID
  tag: String
}
```

Post-MVP, tags may become full entities with properties (typed tags).

---

## Relationships

```
Event *--1 Stream       (each event belongs to at most one stream)
Stream 1--* StreamTag   (streams have multiple tags via junction table)
```

Events with `stream_id = null` are unassigned and shown as "Uncategorized" until inference or user assigns them.

---

## Event Schema

### Sources

| Source | Description |
|--------|-------------|
| `local.window` | Active window/app (from extracted AW watcher) |
| `local.afk` | Idle status (from extracted AW watcher) |
| `local.browser` | Browser tab focus (from extracted AW browser watcher) |
| `remote.tmux` | tmux pane/session events |
| `remote.agent` | Claude/agent session events |
| `manual` | User-entered events |

### Event Types & Payloads

#### Local Events (from ActivityWatch watchers)

**`window_focus`** — Active application changed
```json
{
  "app": "Terminal",
  "title": "sami@dev: ~/project",
  "url": null
}
```

**`afk_change`** — User went idle or returned
```json
{
  "status": "idle" | "active",
  "idle_duration_ms": 300000
}
```

**`browser_tab`** — Browser tab focused
```json
{
  "url": "https://docs.example.com/api",
  "title": "API Documentation",
  "domain": "docs.example.com"
}
```

#### Remote Events (from tmux/agents)

**`tmux_pane_focus`** — Pane focus changed within tmux
```json
{
  "pane_id": "%3",
  "session_name": "dev",
  "window_index": 1,
  "cwd": "/home/sami/project-x"
}
```

**`tmux_scroll`** — User scrolled in a pane (attention indicator even without typing)
```json
{
  "pane_id": "%3",
  "session_name": "dev",
  "direction": "up" | "down"
}
```

**`tmux_session`** — Session created/closed
```json
{
  "action": "created" | "closed",
  "session_name": "dev"
}
```

**`agent_session`** — Agent session started/ended
```json
{
  "action": "started" | "ended",
  "agent": "claude-code",
  "session_id": "abc123",
  "cwd": "/home/sami/project-x"
}
```

**`agent_tool_use`** — Agent used a tool
```json
{
  "agent": "claude-code",
  "session_id": "abc123",
  "tool": "Edit",
  "file": "/home/sami/project-x/main.py"
}
```

**`user_message`** — User sent message to agent
```json
{
  "agent": "claude-code",
  "session_id": "abc123",
  "length": 150,
  "has_image": false
}
```

#### Manual Events

**`manual_note`** — User logged a note
```json
{
  "text": "Thinking about API design",
  "duration_ms": 1800000
}
```

**`manual_time_block`** — User declared a time block
```json
{
  "description": "Meeting with client",
  "start": "2025-01-25T14:00:00Z",
  "end": "2025-01-25T15:00:00Z",
  "tags": ["meeting", "client:acme"]
}
```

---

## Derived Data (Recomputable)

These values are computed from events and can be recalculated with different parameters.

### Attention Classification

For any time range, attention is derived by correlating:
1. **Local focus** — which app/window has focus
2. **Remote focus** — which tmux pane has focus (if terminal is active)
3. **User input** — recent `user_message` events indicate definite attention
4. **Scroll activity** — `tmux_scroll` events indicate attention even without typing
5. **AFK status** — idle periods have no direct attention

**Parameters (tunable):**
- `attention_window_ms`: How long after last input/scroll to assume continued attention (default: 60000)
- `afk_threshold_ms`: How long before marking as away (default: 300000)

_Note: The specific algorithm for allocating direct time across concurrent streams is defined in the architecture docs._

### Stream Inference

Streams are inferred by clustering events based on:
- Working directory / repository
- Temporal proximity
- Agent session relationships
- (Post-MVP) LLM-powered semantic analysis

**Parameters (tunable):**
- `stream_gap_threshold_ms`: Max gap before starting new stream (default: 1800000 = 30 min)
- `directory_weight`: How strongly cwd influences clustering

---

## Storage

MVP: SQLite (single file, portable, good enough for single-user)

```sql
CREATE TABLE events (
  id TEXT PRIMARY KEY,
  timestamp TEXT NOT NULL,        -- ISO 8601 UTC
  type TEXT NOT NULL,
  source TEXT NOT NULL,
  schema_version INTEGER DEFAULT 1,
  data TEXT NOT NULL,             -- JSON payload (type-specific)

  -- Extracted for indexing (duplicated from data)
  cwd TEXT,
  session_id TEXT,

  -- Stream assignment
  stream_id TEXT,
  assignment_source TEXT DEFAULT 'inferred',  -- 'inferred' or 'user'

  FOREIGN KEY (stream_id) REFERENCES streams(id) ON DELETE SET NULL
);

CREATE TABLE streams (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  name TEXT,
  time_direct_ms INTEGER DEFAULT 0,
  time_delegated_ms INTEGER DEFAULT 0,
  first_event_at TEXT,
  last_event_at TEXT,
  needs_recompute INTEGER DEFAULT 0  -- boolean flag
);

CREATE TABLE stream_tags (
  stream_id TEXT NOT NULL,
  tag TEXT NOT NULL,
  PRIMARY KEY (stream_id, tag),
  FOREIGN KEY (stream_id) REFERENCES streams(id) ON DELETE CASCADE
);

-- Indexes
CREATE INDEX idx_events_timestamp ON events(timestamp);
CREATE INDEX idx_events_type ON events(type);
CREATE INDEX idx_events_stream ON events(stream_id);
CREATE INDEX idx_events_cwd ON events(cwd);
CREATE INDEX idx_events_session ON events(session_id);
CREATE INDEX idx_streams_updated ON streams(updated_at);
CREATE INDEX idx_stream_tags_tag ON stream_tags(tag);
```

---

## Resolved Decisions

1. **Browser watcher**: MVP includes browser tracking — too much attention happens there to defer.

2. **Event content storage**: Store metadata only for `user_message`. Full content has privacy implications; can add opt-in later if LLM analysis proves valuable.

3. **Privacy stance**: This is a convenience tool, not a secure vault. No encryption at rest for MVP.

4. **User corrections**: Preserved via `assignment_source` field. Recomputation only touches `inferred` assignments.

## Open Questions

1. **Multi-machine sync**: If user has multiple dev machines, how do events merge? Deferred to post-MVP.

2. **VCS events**: Are git/jj commit events valuable for stream boundary inference, or noise? Needs validation.

3. **Attention allocation algorithm**: How is direct time allocated when multiple agents run in parallel? See architecture docs for detailed specification (TODO).

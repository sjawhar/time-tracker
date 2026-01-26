# Technical Foundations

## tmux Hooks and Events

tmux provides a hooks system that can trigger commands on various events.

### Enabling Focus Events

```bash
# Required for pane-focus-in/out hooks
set -g focus-events on
```

After changing this, clients should detach and reattach.

### Available Hooks

**Pane Events**:
- `pane-focus-in` - Focus enters a pane (requires focus-events on)
- `pane-focus-out` - Focus leaves a pane (requires focus-events on)
- `pane-died` - Pane's program exits but remain-on-exit is enabled
- `pane-exited` - Pane's program exits
- `pane-set-clipboard` - Terminal clipboard is set

**Session Events**:
- `session-created` - New session created
- `session-closed` - Session closes
- `session-renamed` - Session renamed
- `client-session-changed` - Client's session changes

**Window Events**:
- `window-linked` - Window linked to session
- `window-unlinked` - Window unlinked from session
- `window-renamed` - Window renamed

**Client Events**:
- `client-attached` - Client connects
- `client-detached` - Client disconnects
- `client-resized` - Client resized

**Activity Events**:
- `alert-activity` - Window shows activity (with monitor-activity on)
- `alert-bell` - Window receives bell
- `alert-silence` - Window has been silent (with monitor-silence)

**Command Hooks**:
- `after-<command>` - Runs after any tmux command completes
- Examples: `after-new-session`, `after-new-window`, `after-split-window`

### Setting Hooks

```bash
# Global hook (all sessions)
set-hook -g pane-focus-in 'run-shell "echo focused"'

# Session-specific hook
set-hook -t mysession pane-focus-in 'run-shell "echo focused"'

# Remove a hook
set-hook -gu pane-focus-in

# Run hook immediately
set-hook -gR pane-focus-in
```

### Available Format Variables

When hooks run, these variables are available:
- `#{pane_id}` - Unique pane identifier
- `#{pane_current_path}` - Current working directory
- `#{pane_title}` - Pane title (can be set by programs)
- `#{session_name}` - Session name
- `#{window_name}` - Window name
- `#{window_index}` - Window index
- `#{client_tty}` - Client terminal

### Logging with pipe-pane

```bash
# Log all pane output to a file
pipe-pane -o 'cat >> ~/logs/pane-#{pane_id}.log'
```

---

## Agent Session Logs

### Claude Code

**Location**: `~/.claude/projects/<project-hash>/sessions/`

**Format**: JSONL (JSON Lines)

**Event Types**:
- User messages
- Assistant messages
- Tool use (with tool name)
- Tool results
- System messages

**Metadata Available**:
- Timestamps
- Token counts
- Session ID
- Model used

**OpenTelemetry Support**:
```bash
export CLAUDE_CODE_ENABLE_TELEMETRY=1
export OTEL_METRICS_EXPORTER=otlp
export OTEL_LOGS_EXPORTER=otlp
```

Metrics prefixed with `claude_code.*`

### Codex (OpenAI)

**Location**: `~/.codex/history.jsonl` (default, configurable via CODEX_HOME)

**Configuration**:
- `history.max_bytes` - Cap history file size
- Automatic compaction when exceeded

**Events Available**:
- Metrics prefixed with `codex.*`
- Tool fields populated (apply_patch, shell, etc.)
- Notifications: `agent-turn-complete`, `approval-requested`

### Generic Agent Protocol (Proposed)

A standard format any agent could emit:

```json
{
  "type": "agent_event",
  "agent": "claude-code",
  "version": "1.0",
  "session_id": "uuid",
  "timestamp": "ISO8601",
  "event": "tool_use",
  "data": {
    "tool": "Edit",
    "file": "/path/to/file.py"
  }
}
```

---

## Event Sourcing Pattern

### Core Concept

Store all state changes as an append-only sequence of events. Current state is derived by replaying events.

### Benefits for Time Tracking

1. **Complete Audit Trail**: Every activity recorded
2. **Retroactive Re-categorization**: Change project assignment without losing data
3. **Point-in-Time Queries**: "What was I doing at 3pm Tuesday?"
4. **Rebuild Views**: Add new report types without migration
5. **Debugging**: Replay events to understand behavior

### Event Store Schema (Minimal)

```sql
CREATE TABLE events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  timestamp TEXT NOT NULL,        -- ISO8601
  event_type TEXT NOT NULL,       -- e.g., 'pane_focus', 'agent_tool'
  data TEXT NOT NULL              -- JSON payload
);

CREATE INDEX idx_events_timestamp ON events(timestamp);
CREATE INDEX idx_events_type ON events(event_type);
```

### Materialized Views

Precomputed aggregations rebuilt from events:
- Daily time summaries
- Context time totals
- Active contexts list

### Snapshots

For performance, periodically snapshot state:
- Avoids replaying entire history
- Rebuild from last snapshot + subsequent events

---

## Activity Detection

### Human Presence Detection

Options for detecting whether human is at keyboard:

1. **Input device monitoring** (Linux evdev)
   - Direct keyboard/mouse event monitoring
   - Privacy-sensitive, requires permissions

2. **tmux focus events**
   - `client-attached` / `client-detached`
   - `pane-focus-in` / `pane-focus-out`
   - Less granular but privacy-friendly

3. **Idle time queries**
   - `xprintidle` on X11
   - Similar tools for Wayland
   - Not available in pure SSH/tmux

4. **Heuristic from events**
   - No human input events for N minutes → idle
   - Agent events without human events → autonomous

### Agent Activity Detection

1. **Session file watching**
   - inotify on Linux for file changes
   - Tail JSONL files for new events

2. **Process monitoring**
   - Check if agent processes are running
   - `pgrep claude` or similar

3. **Output monitoring**
   - tmux `alert-activity` hook
   - Pane output indicates agent working

---

## Integration Points

### Git Hooks

```bash
# .git/hooks/post-commit
tt event git-commit \
  --repo "$(git rev-parse --show-toplevel)" \
  --branch "$(git branch --show-current)" \
  --hash "$(git rev-parse HEAD)"

# .git/hooks/post-checkout
tt event git-checkout \
  --repo "$(git rev-parse --show-toplevel)" \
  --branch "$(git branch --show-current)"
```

### Export APIs

**Toggl Track API**:
- POST `/api/v9/workspaces/{workspace_id}/time_entries`
- Fields: description, start, stop, duration, project_id, tags

**Clockify API**:
- POST `/api/v1/workspaces/{workspaceId}/time-entries`
- Similar fields

Both support CSV import as fallback.

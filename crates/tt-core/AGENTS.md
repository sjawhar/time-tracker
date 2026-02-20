# tt-core — Domain Logic

Core algorithms and types for time tracking. Pure computation + session parsing (file I/O for Claude, SQLite for OpenCode).

## Modules

| Module | Lines | Role |
|--------|-------|------|
| `allocation.rs` | 1366 | Time allocation algorithm (direct + delegated) |
| `session.rs` | 980 | Claude Code session scanning/parsing |
| `opencode.rs` | ~790 | OpenCode session scanning from SQLite (`opencode.db`) |
| `project.rs` | ~50 | Git remote → project name extraction |

## Allocation Algorithm (`allocation.rs`)

Computes direct (human focus) and delegated (agent) time per stream.

### Flow

1. Build **focus timeline** from `tmux_pane_focus`, `afk_change`, `tmux_scroll`, `window_focus`, `browser_tab` events
2. Build **agent activity timeline** from `agent_session` + `agent_tool_use` events
3. Walk intervals: attribute time based on focus state and agent state

### Key Types

- `AllocatableEvent` — trait that `StoredEvent` (tt-db) implements. Methods: `timestamp()`, `event_type()`, `stream_id()`, `session_id()`, `action()`, `data()`
- `AllocationConfig` — `attention_window_ms` (default 60s), `agent_timeout_ms` (default 30min)
- `StreamTime` — result per stream: `time_direct_ms` + `time_delegated_ms`
- `FocusState` — enum: `Focused { stream_id, focus_start }` | `Unfocused`
- `AgentSession` — tracks per-session: `first_tool_use_at`, `last_tool_use_at`, `ended`

### Rules

- Focus gaps > `attention_window_ms` are capped (no inflated time from sparse events)
- AFK with `idle_duration_ms` retroactively subtracts idle time (capped at attention_window)
- Agent sessions without tool_use events get zero delegated time
- Agent timeout: no tool_use for `agent_timeout_ms` → session ends at last tool_use
- `user_message` events establish focus on their stream (like `tmux_pane_focus`) — sending a message to an agent counts as direct work
- Focus hierarchy: terminal uses tmux stream, browser uses browser stream

### Testing

`TestEvent` struct with builder methods: `tmux_focus()`, `afk_change()`, `agent_session()`, `agent_tool_use()`, `user_message()`, `window_focus()`, `browser_tab()`. 34 test cases cover edge cases (gaps, capping, concurrent agents, AFK retroactive, user message focus).

## Session Scanning (`session.rs` + `opencode.rs`)

Parses Claude Code (`~/.claude/projects/`) JSONL session files and OpenCode (`~/.local/share/opencode/opencode.db`) SQLite database.

### Key Types

- `AgentSession` — parsed session: `session_id`, `source`, `parent_session_id`, `session_type`, `project_path`, `start_time`, `end_time`, `message_count`, `user_prompts`, etc.
- `SessionSource` — enum: `Claude` | `OpenCode`
- `SessionType` — enum: `User` | `Agent` | `Subagent`. In `session.rs` inferred from session_id format; in `opencode.rs` `Subagent` is set when `parent_id` is present.

### Parsing Rules

- `user_prompts`: max 5, each truncated to `MAX_PROMPT_LENGTH` bytes (currently 2000), UTF-8 boundary safe
- `user_message_timestamps`: max 1000
- `message_count`, `assistant_message_count`, `tool_call_count` are `i32` and saturate at `i32::MAX`
- Parent session ID extracted from directory structure (Claude) or session metadata (OpenCode)
- Empty/whitespace-only user prompts are skipped

## Project Identification (`project.rs`)

`extract_project_name()`: workspace path → project name. Strips known workspace prefixes, falls back to last path component.

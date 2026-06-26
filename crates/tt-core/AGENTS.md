# tt-core — Domain Logic

Core algorithms and types for time tracking. Pure computation + session parsing (file I/O for Claude, SQLite for OpenCode).

## Modules

| Module | Lines | Role |
|--------|-------|------|
| `allocation.rs` | ~1854 | Time allocation algorithm (direct + delegated) |
| `session.rs` | 980 | Claude Code session scanning/parsing |
| `opencode.rs` | ~880 | OpenCode session scanning. Reads `session` from monolithic `opencode.db`; reads `message`/`part` from per-session shard at `sessions/<id>.db` when present, else from monolithic. |
| `project.rs` | ~50 | Git remote → project name extraction |

## Allocation Algorithm (`allocation.rs`)

Computes direct (human focus) and delegated (agent) time per stream.

### Flow

1. Build **focus timeline** from `tmux_pane_focus`, `afk_change`, `tmux_scroll`, `window_focus`, `browser_tab` events
2. Build **agent activity timeline** from `agent_session` + `agent_tool_use` events
3. Walk intervals: attribute time based on focus state and agent state

> **Capture status (important):** `tmux_scroll` is now emitted by `tt ingest scroll`, wired to the `pane-mode-changed` tmux hook in `config/tmux-hook.conf` (fires on copy-mode *entry*, e.g. mouse-wheel up — NOT on every wheel tick, so long copy-mode reading past the `attention_window` still relies on the cap). `window_focus`/`afk_change` come from the COSMIC `tt-watcher` daemon. `browser_tab` remains an **unimplemented input** — no emission path, 0 such events in the DB — so browser focus falls back to the window's own stream (→ UNASSIGNED until classified). Heads-down terminal work between focus/scroll events still leans on the `attention_window` cap.

### Key Types

- `AllocatableEvent` — trait that `StoredEvent` (tt-db) implements. Methods: `timestamp()`, `event_type()`, `stream_id()`, `session_id()`, `action()`, `data()`
- `AllocationConfig` — `attention_window_ms` (default 300s / 5min; tests use 60s), `agent_timeout_ms` (default 30min)
- `StreamTime` — result per stream: `time_direct_ms` + `time_delegated_ms`
- `FocusState` — enum: `Focused { stream_id, focus_start }` | `Unfocused`
- `AgentSession` — tracks per-session: `first_tool_use_at`, `last_tool_use_at`, `ended`

### Rules

- Focus gaps > `attention_window_ms` are capped (no inflated time from sparse events)
- AFK with `idle_duration_ms` retroactively subtracts idle time (capped at attention_window)
- Agent sessions without tool_use events get zero delegated time
- Agent timeout: no tool_use for `agent_timeout_ms` → session ends at last tool_use
- `user_message` events establish focus on their stream (like `tmux_pane_focus`) — sending a message to an agent counts as direct work
- Focus hierarchy (`resolve_focus_stream`): terminal app → tmux stream; browser app → browser-tab stream, falling back to the window's own stream when there's no `browser_tab` info; other GUI app → the window's stream
- `window_focus` establishes focus for non-terminal/non-browser GUI apps (Slack, doc/PDF readers): it closes the prior interval against the *old* window state first, then opens the new focus. A GUI/browser window with **no resolvable stream still accrues direct time to the UNASSIGNED bucket** (same as unassigned tmux focus) — active GUI time is never dropped to zero; it waits in UNASSIGNED until classify attributes it.

### Streams are semantic — there is NO deterministic surface→stream mapping

A **stream** is a coherent unit of *work*, identified by human/LLM judgment. It is **not** derivable from any surface signal. Each of these is NOT a stream:

- a **working directory** is not a stream (one repo/dir hosts many streams; one stream spans many dirs)
- a **window title** is not a stream
- a **browser tab / URL** is not a stream
- an **app name** is not a stream
- **"unfocused"** is not a stream

**Do NOT add deterministic rules that map cwd / window title / browser tab / app name → stream.** That approach is *fundamentally unsound* and is a known dead end: the same surface belongs to different streams over time, and a single stream spans many surfaces. There is no rule that recovers the mapping — only semantic judgment does.

Classification is therefore done by an LLM/human via `tt classify --apply` (per-session or temporal context), never by a fixed surface rule. Surface signals may at most be **weak temporal hints** for that classifier — never an attribution rule. Events with no resolvable stream stay **UNASSIGNED** until semantically classified; they must not be silently dropped, nor back-filled by surface heuristics.

### Testing

`TestEvent` struct with builder methods: `tmux_focus()`, `afk_change()`, `agent_session()`, `agent_tool_use()`, `user_message()`, `window_focus()`, `browser_tab()`. 34 test cases cover edge cases (gaps, capping, concurrent agents, AFK retroactive, user message focus).

## Session Scanning (`session.rs` + `opencode.rs`)

Parses Claude Code (`~/.claude/projects/`) JSONL session files and OpenCode (`~/.local/share/opencode/opencode.db`) SQLite database. The user's OpenCode fork shards messages/parts into per-session SQLite files at `~/.local/share/opencode/sessions/<id>.db`; `build_agent_session` opens the shard when present and falls back to the monolithic connection when not (schema is identical between the two). Corrupt or non-SQLite shards trigger a logged warning and the same fallback.

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

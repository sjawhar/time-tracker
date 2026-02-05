# OpenCode Session Ingestion Design

**Goal:** Extend session ingestion to support both Claude Code and OpenCode, using a unified `AgentSession` model.

**Date:** 2026-02-05

---

## Background

The time-tracker currently ingests Claude Code sessions from `~/.claude/projects/`. The user also uses OpenCode, which stores sessions in `~/.local/share/opencode/storage/`. Both tools serve the same purpose (AI coding assistants), so their session data should be unified.

### Data Format Comparison

**Claude Code** (`~/.claude/projects/{project}/{session}.jsonl`):
- JSONL files with inline messages
- Each line: `{type: "user"|"assistant"|"summary", timestamp, cwd, message: {content}}`
- User prompts vs tool results distinguished by content type (string vs array)

**OpenCode** (`~/.local/share/opencode/storage/`):
- Normalized JSON across directories:
  - `session/{project_hash}/{session_id}.json` - metadata
  - `message/{session_id}/{message_id}.json` - message metadata
  - `part/{message_id}/{part_id}.json` - content
- All user messages are actual prompts (tool results embedded in assistant parts)

---

## Design

### Data Model

**Rename:** `ClaudeSession` -> `AgentSession`

**New enum:**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionSource {
    #[default]
    Claude,
    OpenCode,
}
```

**AgentSession fields:**

| Field | Type | Notes |
|-------|------|-------|
| `session_id` | `String` | Was `claude_session_id` |
| `source` | `SessionSource` | New field |
| `parent_session_id` | `Option<String>` | Unchanged |
| `session_type` | `SessionType` | Unchanged |
| `project_path` | `String` | Unchanged |
| `project_name` | `String` | Unchanged |
| `start_time` | `DateTime<Utc>` | Unchanged |
| `end_time` | `Option<DateTime<Utc>>` | Unchanged |
| `message_count` | `i32` | Unchanged |
| `summary` | `Option<String>` | Claude: summary message. OpenCode: title field |
| `user_prompts` | `Vec<String>` | Unchanged |
| `starting_prompt` | `Option<String>` | Unchanged |
| `assistant_message_count` | `i32` | Unchanged |
| `tool_call_count` | `i32` | Unchanged |
| `user_message_timestamps` | `Vec<DateTime<Utc>>` | Unchanged |

**SessionType derivation:**
- Claude: from session ID pattern (user/agent/subagent)
- OpenCode: from `parentID` presence (user/subagent only - no background agents)

### Database Schema

**Rename table:** `claude_sessions` -> `agent_sessions`

```sql
CREATE TABLE agent_sessions (
    session_id TEXT PRIMARY KEY,
    source TEXT NOT NULL DEFAULT 'claude',
    parent_session_id TEXT,
    session_type TEXT NOT NULL DEFAULT 'user',
    project_path TEXT NOT NULL,
    project_name TEXT NOT NULL,
    start_time TEXT NOT NULL,
    end_time TEXT,
    message_count INTEGER NOT NULL,
    summary TEXT,
    user_prompts TEXT DEFAULT '[]',
    starting_prompt TEXT,
    assistant_message_count INTEGER DEFAULT 0,
    tool_call_count INTEGER DEFAULT 0
);

CREATE INDEX idx_agent_sessions_start_time ON agent_sessions(start_time);
CREATE INDEX idx_agent_sessions_project_path ON agent_sessions(project_path);
CREATE INDEX idx_agent_sessions_parent ON agent_sessions(parent_session_id);
```

Note: `user_message_timestamps` are not persisted in the database — they are
only used transiently during event generation at indexing time.

**Migration:** Drop and recreate (schema v7). No data migration needed.

### OpenCode Parsing

New module `tt-core/src/opencode.rs`:

1. Scan `~/.local/share/opencode/storage/session/*/` directories
2. For each `{session_id}.json`:
   - Parse session metadata (id, directory, title, parentID, time)
   - List messages from `storage/message/{session_id}/`
   - For user messages, read parts to extract text content
   - Count assistant messages and tool parts
3. Return `Vec<AgentSession>` with `source: OpenCode`

**Performance:** Use rayon for parallel parsing. Lazy part loading - only read parts for user prompt extraction.

### CLI

No changes to CLI interface. `tt ingest sessions` will scan both sources and report combined stats:

```
Scanning Claude Code sessions...
Scanning OpenCode sessions...
Indexed 150 sessions (120 Claude, 30 OpenCode)
```

### File Changes

| File | Change |
|------|--------|
| `tt-core/src/session.rs` | Rename types, add `SessionSource` |
| `tt-core/src/opencode.rs` | New: OpenCode parser |
| `tt-core/src/lib.rs` | Export new types |
| `tt-db/src/lib.rs` | Rename table, add source column, bump schema |
| `tt-cli/src/commands/ingest.rs` | Call both scanners |

---

## Implementation Tasks

1. Add `SessionSource` enum to `tt-core/src/session.rs`
2. Rename `ClaudeSession` -> `AgentSession`, add `source` field
3. Update `tt-db` schema (v6): rename table, add column
4. Create `tt-core/src/opencode.rs` with OpenCode parser
5. Update `tt-cli/src/commands/ingest.rs` to scan both sources
6. Update all references (`claude_session_id` -> `session_id`, etc.)
7. Run tests and verify with `tt ingest sessions`

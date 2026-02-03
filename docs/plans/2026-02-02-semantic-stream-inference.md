# Stream Inference Data Export - Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Create `tt export` command that outputs layered context for stream inference. Each layer is opt-in via flags. No inference logic—just clean data export.

**Architecture:** Composable flags (`--events`, `--agents`, `--streams`, `--gaps`) each produce a section in the JSON output. Consumer (human or LLM) uses this data to make stream assignment decisions.

**Tech Stack:** Rust (tt CLI), SQLite (tt-db)

---

## Design

### Command Structure

```bash
tt export [FLAGS] --start <TIME> [--end <TIME>]

FLAGS:
    --events     Include chronological events
    --agents     Include Claude session metadata
    --streams    Include existing streams
    --gaps       Include gaps between user input events
```

**Time formats:**
- ISO 8601: `2026-02-02T09:00:00Z`
- Relative: `4 hours ago`, `2 days ago`, `30 minutes ago`

### Output Structure

```json
{
  "time_range": {
    "start": "2026-02-02T08:00:00Z",
    "end": "2026-02-02T12:00:00Z"
  },
  "events": [...],      // if --events
  "agents": [...],      // if --agents
  "streams": [...],     // if --streams
  "gaps": [...]         // if --gaps
}
```

### Events Schema (--events)

```json
{
  "events": [
    {
      "id": "event-uuid",
      "timestamp": "2026-02-02T09:15:32Z",
      "type": "user_message",
      "source": "claude",
      "cwd": "/home/user/project",
      "claude_session_id": "session-uuid",
      "stream_id": null
    },
    {
      "id": "event-uuid-2",
      "timestamp": "2026-02-02T09:15:45Z",
      "type": "tmux_pane_focus",
      "source": "remote.tmux",
      "cwd": "/home/user/project",
      "tmux_session": "work",
      "pane_id": "%3",
      "stream_id": "stream-uuid"
    }
  ]
}
```

### Agents Schema (--agents)

```json
{
  "agents": [
    {
      "claude_session_id": "session-uuid",
      "project_path": "/home/user/project",
      "project_name": "project",
      "start_time": "2026-02-02T09:00:00Z",
      "end_time": "2026-02-02T10:30:00Z",
      "summary": "Implemented OAuth token refresh flow",
      "starting_prompt": "Add token refresh to the auth module...",
      "user_prompts": [
        "Add token refresh to the auth module...",
        "Now add tests for the refresh logic",
        "Fix the error handling"
      ],
      "user_prompt_count": 8,
      "assistant_message_count": 12,
      "tool_call_count": 156
    }
  ]
}
```

### Streams Schema (--streams)

```json
{
  "streams": [
    {
      "id": "stream-uuid",
      "name": "OAuth Implementation",
      "time_direct_ms": 3600000,
      "time_delegated_ms": 7200000,
      "first_event_at": "2026-02-01T14:00:00Z",
      "last_event_at": "2026-02-01T17:30:00Z"
    }
  ]
}
```

### Gaps Schema (--gaps)

Gaps between user-initiated events (user messages, tmux focus, scroll, etc.).

```json
{
  "gaps": [
    {
      "start": "2026-02-02T10:30:00Z",
      "end": "2026-02-02T11:15:00Z",
      "duration_minutes": 45,
      "before_event_type": "user_message",
      "after_event_type": "tmux_pane_focus"
    }
  ]
}
```

**Gap threshold:** Only include gaps > 5 minutes (configurable via `--gap-threshold`).

---

## Task 1: Create Export Command Structure

**Files:**
- Create: `crates/tt-cli/src/commands/export.rs`
- Modify: `crates/tt-cli/src/commands/mod.rs`
- Modify: `crates/tt-cli/src/cli.rs`
- Modify: `crates/tt-cli/src/main.rs`

### Step 1: Add Export command to CLI

```rust
/// Export data for stream inference
Export {
    /// Include chronological events
    #[arg(long)]
    events: bool,

    /// Include Claude session metadata
    #[arg(long)]
    agents: bool,

    /// Include existing streams
    #[arg(long)]
    streams: bool,

    /// Include gaps between user input events
    #[arg(long)]
    gaps: bool,

    /// Minimum gap duration to include (minutes)
    #[arg(long, default_value = "5")]
    gap_threshold: u32,

    /// Start of time range (ISO 8601 or relative like "4 hours ago")
    #[arg(long)]
    start: Option<String>,

    /// End of time range (ISO 8601, defaults to now)
    #[arg(long)]
    end: Option<String>,
},
```

### Step 2: Create export.rs with output types

```rust
use serde::Serialize;
use chrono::{DateTime, Utc};

#[derive(Serialize)]
pub struct ExportOutput {
    pub time_range: TimeRange,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<EventExport>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agents: Option<Vec<AgentExport>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub streams: Option<Vec<StreamExport>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gaps: Option<Vec<GapExport>>,
}

#[derive(Serialize)]
pub struct TimeRange {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Serialize)]
pub struct EventExport {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    #[serde(rename = "type")]
    pub event_type: String,
    pub source: String,
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmux_session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    pub stream_id: Option<String>,
}

#[derive(Serialize)]
pub struct AgentExport {
    pub claude_session_id: String,
    pub project_path: String,
    pub project_name: String,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pub summary: Option<String>,
    pub starting_prompt: Option<String>,
    pub user_prompts: Vec<String>,
    pub user_prompt_count: i32,
    pub assistant_message_count: i32,
    pub tool_call_count: i32,
}

#[derive(Serialize)]
pub struct StreamExport {
    pub id: String,
    pub name: Option<String>,
    pub time_direct_ms: i64,
    pub time_delegated_ms: i64,
    pub first_event_at: Option<DateTime<Utc>>,
    pub last_event_at: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
pub struct GapExport {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub duration_minutes: i64,
    pub before_event_type: String,
    pub after_event_type: String,
}
```

### Step 3: Wire up in main.rs and mod.rs

### Step 4: Test structure compiles

```bash
cargo build -p tt-cli
```

---

## Task 2: Implement --events Flag

**Files:**
- Modify: `crates/tt-cli/src/commands/export.rs`

### Step 1: Add event export function

```rust
fn export_events(db: &Database, start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Vec<EventExport>> {
    let events = db.get_events_in_range(start, end)?;

    let exports: Vec<EventExport> = events
        .into_iter()
        .map(|e| {
            let claude_session_id = e.data
                .get("claude_session_id")
                .and_then(|v| v.as_str())
                .map(String::from);

            let tmux_session = e.data
                .get("tmux_session")
                .or_else(|| e.data.get("session_name"))
                .and_then(|v| v.as_str())
                .map(String::from);

            let pane_id = e.data
                .get("pane_id")
                .and_then(|v| v.as_str())
                .map(String::from);

            EventExport {
                id: e.id,
                timestamp: e.timestamp,
                event_type: e.event_type,
                source: e.source,
                cwd: e.cwd().map(String::from),
                claude_session_id,
                tmux_session,
                pane_id,
                stream_id: e.stream_id,
            }
        })
        .collect();

    Ok(exports)
}
```

### Step 2: Test

```bash
cargo run -- export --events --start "1 hour ago" | jq '.events | length'
```

---

## Task 3: Implement --agents Flag

**Files:**
- Modify: `crates/tt-cli/src/commands/export.rs`

### Step 1: Add agent export function

```rust
fn export_agents(db: &Database, start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Vec<AgentExport>> {
    let sessions = db.claude_sessions_in_range(start, end)?;

    let exports: Vec<AgentExport> = sessions
        .into_iter()
        .map(|s| AgentExport {
            claude_session_id: s.claude_session_id,
            project_path: s.project_path,
            project_name: s.project_name,
            start_time: s.start_time,
            end_time: s.end_time,
            summary: s.summary,
            starting_prompt: s.starting_prompt,
            user_prompts: s.user_prompts,
            user_prompt_count: s.message_count,
            assistant_message_count: s.assistant_message_count,
            tool_call_count: s.tool_call_count,
        })
        .collect();

    Ok(exports)
}
```

### Step 2: Test

```bash
cargo run -- export --agents --start "24 hours ago" | jq '.agents | length'
```

---

## Task 4: Implement --streams Flag

**Files:**
- Modify: `crates/tt-cli/src/commands/export.rs`

### Step 1: Add stream export function

```rust
fn export_streams(db: &Database) -> Result<Vec<StreamExport>> {
    let streams = db.list_streams()?;

    let exports: Vec<StreamExport> = streams
        .into_iter()
        .map(|s| StreamExport {
            id: s.id,
            name: s.name,
            time_direct_ms: s.time_direct_ms,
            time_delegated_ms: s.time_delegated_ms,
            first_event_at: s.first_event_at,
            last_event_at: s.last_event_at,
        })
        .collect();

    Ok(exports)
}
```

### Step 2: Test

```bash
cargo run -- export --streams | jq '.streams'
```

---

## Task 5: Implement --gaps Flag

**Files:**
- Modify: `crates/tt-cli/src/commands/export.rs`

### Step 1: Define user event types

```rust
const USER_EVENT_TYPES: &[&str] = &[
    "user_message",
    "tmux_pane_focus",
    "tmux_scroll",
    "window_focus",
    "browser_tab",
];

fn is_user_event(event_type: &str) -> bool {
    USER_EVENT_TYPES.contains(&event_type)
}
```

### Step 2: Add gap detection function

```rust
fn export_gaps(
    db: &Database,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    threshold_minutes: u32,
) -> Result<Vec<GapExport>> {
    let events = db.get_events_in_range(start, end)?;

    // Filter to user events only
    let user_events: Vec<_> = events
        .iter()
        .filter(|e| is_user_event(&e.event_type))
        .collect();

    if user_events.len() < 2 {
        return Ok(vec![]);
    }

    let threshold_ms = i64::from(threshold_minutes) * 60 * 1000;
    let mut gaps = Vec::new();

    for window in user_events.windows(2) {
        let before = window[0];
        let after = window[1];
        let gap_ms = (after.timestamp - before.timestamp).num_milliseconds();

        if gap_ms >= threshold_ms {
            gaps.push(GapExport {
                start: before.timestamp,
                end: after.timestamp,
                duration_minutes: gap_ms / 60_000,
                before_event_type: before.event_type.clone(),
                after_event_type: after.event_type.clone(),
            });
        }
    }

    Ok(gaps)
}
```

### Step 3: Test

```bash
cargo run -- export --gaps --start "8 hours ago" | jq '.gaps'
```

---

## Task 6: Wire Up Run Function

**Files:**
- Modify: `crates/tt-cli/src/commands/export.rs`

### Step 1: Implement run function

```rust
pub fn run(
    db: &Database,
    events: bool,
    agents: bool,
    streams: bool,
    gaps: bool,
    gap_threshold: u32,
    start: Option<String>,
    end: Option<String>,
) -> Result<()> {
    let end = end
        .map(|s| parse_datetime(&s))
        .transpose()?
        .unwrap_or_else(Utc::now);
    let start = start
        .map(|s| parse_datetime(&s))
        .transpose()?
        .unwrap_or_else(|| end - Duration::hours(24));

    let output = ExportOutput {
        time_range: TimeRange { start, end },
        events: if events {
            Some(export_events(db, start, end)?)
        } else {
            None
        },
        agents: if agents {
            Some(export_agents(db, start, end)?)
        } else {
            None
        },
        streams: if streams {
            Some(export_streams(db)?)
        } else {
            None
        },
        gaps: if gaps {
            Some(export_gaps(db, start, end, gap_threshold)?)
        } else {
            None
        },
    };

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
```

### Step 2: Update main.rs to call export::run

```rust
Commands::Export {
    events,
    agents,
    streams,
    gaps,
    gap_threshold,
    start,
    end,
} => {
    export::run(&db, *events, *agents, *streams, *gaps, *gap_threshold, start.clone(), end.clone())?;
}
```

### Step 3: Run full tests

```bash
cargo test -p tt-cli
cargo clippy --all-targets

# Test combinations
cargo run -- export --events --start "2 hours ago" | jq 'keys'
cargo run -- export --events --agents --start "2 hours ago" | jq 'keys'
cargo run -- export --events --agents --streams --gaps --start "8 hours ago" | jq 'keys'
```

---

## Task 7: Emit Claude Conversation Turn Events

**Files:**
- Modify: `crates/tt-core/src/session.rs`
- Modify: `crates/tt-cli/src/commands/sessions.rs`

Claude session turns should be events so they appear in `--events` output.

### Step 1: Add turn event emission during session indexing

When parsing sessions, emit events for:
- `user_message` - each user turn (not tool results)
- `assistant_message` - each assistant response
- `session_start` - when session begins
- `session_end` - when session ends (if applicable)

```rust
pub struct ConversationTurnEvent {
    pub timestamp: DateTime<Utc>,
    pub event_type: String,  // user_message, assistant_message, session_start, session_end
    pub claude_session_id: String,
    pub project_path: String,
}
```

### Step 2: Store turn events during session index

```rust
// In sessions index command:
for session in &sessions {
    db.upsert_claude_session(session)?;

    // Emit session_start event
    let start_event = StoredEvent {
        id: format!("{}-start", session.claude_session_id),
        timestamp: session.start_time,
        event_type: "session_start".to_string(),
        source: "claude".to_string(),
        data: json!({
            "claude_session_id": session.claude_session_id,
            "project_path": session.project_path,
        }),
        stream_id: None,
        assignment_source: None,
    };
    db.upsert_event(&start_event)?;

    // Emit turn events (extracted during parsing)
    for turn in &session.turns {
        let event = StoredEvent {
            id: format!("{}-{}-{}",
                session.claude_session_id,
                turn.event_type,
                turn.timestamp.timestamp_millis()),
            timestamp: turn.timestamp,
            event_type: turn.event_type.clone(),
            source: "claude".to_string(),
            data: json!({
                "claude_session_id": session.claude_session_id,
                "project_path": session.project_path,
            }),
            stream_id: None,
            assignment_source: None,
        };
        db.upsert_event(&event)?;
    }
}
```

### Step 3: Test

```bash
cargo run -- sessions index
cargo run -- export --events --start "1 hour ago" | jq '.events[] | select(.source == "claude")'
```

---

## Summary

| Task | Description |
|------|-------------|
| 1 | Create export command structure with composable flags |
| 2 | Implement `--events` (chronological events) |
| 3 | Implement `--agents` (Claude session metadata) |
| 4 | Implement `--streams` (existing streams) |
| 5 | Implement `--gaps` (gaps between user input events) |
| 6 | Wire up run function with all flags |
| 7 | Emit Claude conversation turns as events |

**Execution order:** 1 → 2 → 3 → 4 → 5 → 6 → 7

**Usage examples:**
```bash
# Just events for a time range
tt export --events --start "4 hours ago"

# Events + agent context for stream inference
tt export --events --agents --start "4 hours ago"

# Full context for comprehensive analysis
tt export --events --agents --streams --gaps --start "8 hours ago"

# Custom gap threshold (10 minutes instead of default 5)
tt export --gaps --gap-threshold 10 --start "8 hours ago"
```

# Infer Streams Skill - Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Simplify `tt infer` to output context JSON to stdout for Claude Code to make stream inference decisions.

**Architecture:** Invert control flow - `tt infer` outputs minimal context, Claude Code reads it and decides how to group work into streams. No API calls from `tt`.

**Tech Stack:** Rust (tt CLI)

---

## Task 1: Update Session Parsing to Capture Counts

**Files:**
- Modify: `crates/tt-core/src/session.rs`

The `ClaudeSession` struct needs additional fields for message counts.

### Step 1: Add count fields to ClaudeSession

```rust
pub struct ClaudeSession {
    pub claude_session_id: String,
    pub parent_session_id: Option<String>,
    pub project_path: String,
    pub project_name: String,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pub message_count: i32,
    pub summary: Option<String>,
    pub user_prompts: Vec<String>,
    // NEW:
    pub starting_prompt: Option<String>,
    pub assistant_message_count: i32,
    pub tool_call_count: i32,
}
```

### Step 2: Update parse_session_file to count messages

In the parsing loop, track:
- `assistant_message_count`: messages where `type == "assistant"`
- `tool_call_count`: count tool_use blocks in assistant messages

The `starting_prompt` is `user_prompts[0]` if it exists.

### Step 3: Update database schema

Add columns to `claude_sessions` table:
- `starting_prompt TEXT`
- `assistant_message_count INTEGER DEFAULT 0`
- `tool_call_count INTEGER DEFAULT 0`

### Step 4: Update upsert_claude_session and claude_sessions_in_range

Store and retrieve the new fields.

### Step 5: Run tests

```bash
cargo test -p tt-core
cargo test -p tt-db
```

---

## Task 2: Simplify `tt infer` to Output JSON Context

**Files:**
- Modify: `crates/tt-cli/src/cli.rs`
- Modify: `crates/tt-cli/src/commands/infer.rs`
- Modify: `crates/tt-cli/src/main.rs`

### Step 1: Simplify CLI flags

Remove from `Infer`:
- `llm: bool`
- `dry_run: bool`
- `assign_events: bool`

Keep:
- `force: bool` (for clearing assignments)
- `start: Option<String>`
- `end: Option<String>`

```rust
/// Output context for stream inference.
Infer {
    /// Clear all inferred assignments first
    #[arg(long)]
    force: bool,

    /// Start of time range (ISO 8601 or relative like "2 hours ago")
    #[arg(long)]
    start: Option<String>,

    /// End of time range (ISO 8601, defaults to now)
    #[arg(long)]
    end: Option<String>,
},
```

### Step 2: Add relative time parsing

Support both ISO 8601 and relative times:

```rust
fn parse_datetime(s: &str) -> Result<DateTime<Utc>> {
    // Try ISO 8601 first
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }

    // Try relative time: "N hours/minutes/days ago"
    let re = regex::Regex::new(r"^(\d+)\s+(hour|minute|day|week)s?\s+ago$")?;
    if let Some(caps) = re.captures(s) {
        let n: i64 = caps[1].parse()?;
        let now = Utc::now();
        let dt = match &caps[2] {
            "minute" => now - Duration::minutes(n),
            "hour" => now - Duration::hours(n),
            "day" => now - Duration::days(n),
            "week" => now - Duration::weeks(n),
            _ => anyhow::bail!("Unknown time unit"),
        };
        return Ok(dt);
    }

    anyhow::bail!("Invalid datetime: {s}. Use ISO 8601 or relative (e.g., '2 hours ago')")
}
```

### Step 3: Rewrite run function to output JSON

```rust
use serde::Serialize;

#[derive(Serialize)]
struct InferContext {
    time_range: TimeRange,
    events: Vec<EventInfo>,
    sessions: Vec<SessionInfo>,
}

#[derive(Serialize)]
struct TimeRange {
    start: DateTime<Utc>,
    end: DateTime<Utc>,
}

#[derive(Serialize)]
struct EventInfo {
    id: String,
    timestamp: DateTime<Utc>,
    cwd: Option<String>,
    jj_project: Option<String>,
}

#[derive(Serialize)]
struct SessionInfo {
    id: String,
    project_name: String,
    start_time: DateTime<Utc>,
    end_time: Option<DateTime<Utc>>,
    summary: Option<String>,
    starting_prompt: Option<String>,
    user_prompt_count: i32,
    assistant_message_count: i32,
    tool_call_count: i32,
}

pub fn run(
    db: &Database,
    force: bool,
    start: Option<String>,
    end: Option<String>,
) -> Result<()> {
    if force {
        let cleared = db.clear_inferred_assignments()?;
        let deleted = db.delete_orphaned_streams()?;
        eprintln!("Cleared {cleared} assignments, deleted {deleted} orphaned streams");
    }

    let end = end.map(|s| parse_datetime(&s)).transpose()?.unwrap_or_else(Utc::now);
    let start = start.map(|s| parse_datetime(&s)).transpose()?.unwrap_or_else(|| end - Duration::hours(24));

    let sessions = db.claude_sessions_in_range(start, end)?;
    let events = db.get_events_in_range(start, end)?;  // Need to add this method

    let context = InferContext {
        time_range: TimeRange { start, end },
        events: events.into_iter().map(|e| EventInfo {
            id: e.id,
            timestamp: e.timestamp,
            cwd: e.cwd,
            jj_project: e.jj_project().map(String::from),
        }).collect(),
        sessions: sessions.into_iter().map(|s| SessionInfo {
            id: s.claude_session_id,
            project_name: s.project_name,
            start_time: s.start_time,
            end_time: s.end_time,
            summary: s.summary,
            starting_prompt: s.starting_prompt,
            user_prompt_count: s.user_prompts.len() as i32,
            assistant_message_count: s.assistant_message_count,
            tool_call_count: s.tool_call_count,
        }).collect(),
    };

    println!("{}", serde_json::to_string_pretty(&context)?);
    Ok(())
}
```

### Step 4: Add get_events_in_range to database

```rust
pub fn get_events_in_range(
    &self,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Vec<StoredEvent>, DbError> {
    // Query events where timestamp BETWEEN start AND end
}
```

### Step 5: Update main.rs

```rust
Commands::Infer { force, start, end } => {
    infer::run(&db, *force, start.clone(), end.clone())?;
}
```

### Step 6: Run tests and verify

```bash
cargo test -p tt-cli
cargo clippy --all-targets
cargo run -- infer --start "2 hours ago" | head -50
```

---

## Task 3: Add `tt stream create` Command

**Files:**
- Create: `crates/tt-cli/src/commands/stream.rs`
- Modify: `crates/tt-cli/src/commands/mod.rs`
- Modify: `crates/tt-cli/src/cli.rs`
- Modify: `crates/tt-cli/src/main.rs`

### Step 1: Add Stream subcommand to CLI

```rust
/// Manage streams
#[command(subcommand)]
Stream(StreamCommands),

#[derive(Subcommand, Debug)]
pub enum StreamCommands {
    /// Create a new stream (prints ID to stdout)
    Create {
        /// Name for the stream
        name: String,
    },
}
```

### Step 2: Create stream.rs

```rust
use anyhow::{Context, Result};
use chrono::Utc;
use tt_db::{Database, Stream};
use uuid::Uuid;

pub fn create(db: &Database, name: String) -> Result<()> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();

    let stream = Stream {
        id: id.clone(),
        name: Some(name),
        created_at: now,
        updated_at: now,
        time_direct_ms: 0,
        time_delegated_ms: 0,
        first_event_at: None,
        last_event_at: None,
        needs_recompute: true,
    };

    db.insert_stream(&stream).context("failed to create stream")?;
    println!("{id}");
    Ok(())
}
```

### Step 3: Wire up in main.rs and mod.rs

### Step 4: Test

```bash
cargo run -- stream create "Test Stream"
# Should output UUID
```

---

## Task 4: Remove Old LLM Inference Code

**Files:**
- Modify: `crates/tt-llm/src/lib.rs`
- Modify: `crates/tt-cli/src/commands/infer.rs`

### Step 1: Remove from tt-llm

Delete:
- `build_stream_inference_prompt` function
- `InferredStreamResponse` struct
- `StreamInferenceResponse` struct
- `parse_stream_inference_response` function
- `Client::infer_streams` method

Keep:
- `Client` for tag suggestions
- `LlmSuggestion` and related code

### Step 2: Remove from infer.rs

Delete:
- `run_llm` function
- `assign_events_to_llm_streams` function
- Related tests

### Step 3: Remove tokio if no longer needed

Check if `tt-cli` still needs async runtime. If not, remove from Cargo.toml.

### Step 4: Run tests

```bash
cargo test
cargo clippy --all-targets
```

---

## Summary

| Task | Description |
|------|-------------|
| 1 | Add session counts (assistant_message_count, tool_call_count, starting_prompt) |
| 2 | Simplify `tt infer` to output JSON context |
| 3 | Add `tt stream create` command |
| 4 | Remove old LLM inference code |

**Output format:**
```json
{
  "time_range": {"start": "...", "end": "..."},
  "events": [{"id", "timestamp", "cwd", "jj_project"}],
  "sessions": [{"id", "project_name", "start_time", "end_time", "summary", "starting_prompt", "user_prompt_count", "assistant_message_count", "tool_call_count"}]
}
```

**Execution order:** 1 → 2 → 3 → 4

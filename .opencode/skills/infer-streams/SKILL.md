---
name: infer-streams
description: Use when analyzing tt context output to identify work streams, assign events to streams, and generate time reports. Triggers on weekly review, standup prep, or explicit stream inference requests.
---

# Stream Inference

Identify work streams from time-tracker data, persist them in tt's database, and let tt's allocation algorithm compute direct/delegated time.

**Key principle:** The LLM identifies and classifies streams. `tt recompute` calculates time. Never reimplement time calculation.

## Key Concepts

| Concept | Definition | Example |
|---------|-----------|---------|
| **Project** | A codebase/repository. Subdirectories are the SAME project. | `/home/sami/pivot/agents` → `pivot` |
| **Stream** | A specific task/feature/PR within a project. Spans hours to 2-3 days. | "pivot: engine refactor", "legion: LEG-125" |

**BAD names:** "pivot work", "legion development" (too coarse)
**GOOD names:** "pivot: pipeline API redesign", "legion: controller-worker separation"

## Arguments

Optional: Time range (default: since last stream ended, or "7 days ago")

Example: `/infer-streams 3 days ago`

## Phase 1: Ingest + Determine Range

```bash
cargo build 2>/dev/null && cargo run -- ingest sessions
tt streams list --json
```

**CRITICAL: Always ingest before context.** Without it you miss 75%+ of sessions.

If streams exist, start from where the last stream ended. If empty, use "7 days ago" or user-specified range.

## Phase 2: Gather Context

```bash
tt context --events --agents --streams --gaps --start "{time_range}"
```

## Phase 3: Identify Streams

For each project, group agent sessions into streams using:

1. **`summary`** — describes what was worked on
2. **`starting_prompt`** — reveals intent
3. **`project_path`** / `cwd` — identifies repo (merge subdirectories)
4. **Temporal gaps** — >2 hours between activity often means different streams
5. **Semantic similarity** — related sessions = ONE stream

Present the proposed streams to the user for review before persisting.

## Phase 4: Create Streams + Assign Events

For each approved stream:

### 4a. Create stream and capture ID
```bash
STREAM_ID=$(tt streams create "project: stream name")
```

### 4b. Assign events via SQLite

No CLI command exists for event assignment. Use Python to update tt's database directly:

```python
import sqlite3, os
db = sqlite3.connect(os.path.expanduser("~/.local/share/tt/tt.db"))

# Assign agent session events (user_message, agent_session, agent_tool_use)
db.execute("""
    UPDATE events SET stream_id = ?, assignment_source = 'inferred'
    WHERE session_id IN (?, ?, ?) AND stream_id IS NULL
""", (stream_id, *session_ids))

# Assign tmux_pane_focus events (these have NO session_id — match by cwd + time)
db.execute("""
    UPDATE events SET stream_id = ?, assignment_source = 'inferred'
    WHERE event_type = 'tmux_pane_focus'
      AND cwd LIKE ?
      AND stream_id IS NULL
      AND timestamp BETWEEN ? AND ?
""", (stream_id, cwd_pattern, start_iso, end_iso))

db.commit()
db.close()
```

**Important:** `tmux_pane_focus` events have no `session_id`. They MUST be assigned by cwd pattern + time range.

### 4c. Recompute time
```bash
tt recompute --force
```

This runs the proper allocation algorithm which handles attention windows, AFK, agent timeouts, and focus tracking correctly.

## Phase 5: Report Results

```bash
tt streams list
```

Present a consolidated table. All times in Pacific Time (UTC-8).

```markdown
## Stream Inference Results

**Time range:** {start} to {end} (Pacific Time)

| Project | Stream | Direct | Delegated |
|---------|--------|--------|-----------|
| pivot | engine refactor | 2h 15m | 72.9h |
| legion | opencode-plugin | 43m | 38.3h |
| **TOTAL** | | **X hrs** | **Y hrs** |

### Stream Details
- **pivot: engine refactor** — Sessions: ses_abc, ses_def. Engine/scheduler cleanup.

### Unassigned Events
{Any events that couldn't be classified — should be zero}
```

## Common Mistakes

| Mistake | Fix |
|---------|-----|
| Computing time in Python | **Never.** Use `tt recompute`. The allocation algorithm handles attention windows, AFK, agent timeouts. |
| Ignoring tmux_pane_focus events | These have NO session_id. Assign by cwd + time range. |
| Skipping ingestion | **Always** `tt ingest sessions` first. |
| Starting from "8 hours ago" | Check `tt streams list` — start from where streams end. |
| Treating project as stream | Project = repo. Stream = task/feature. |
| Splitting subdirectories | `/pivot/agents` is part of `pivot`. |
| Streams too coarse | "pivot work" → "pivot: pipeline API redesign". |
| Leaving events unassigned | Everything gets assigned. Use "misc: {activity}" for unclear. |

## Done When

1. All events assigned to streams (check `tt context --events` for unassigned)
2. `tt recompute --force` completed
3. `tt streams list` shows direct/delegated time per stream
4. Report presented to user

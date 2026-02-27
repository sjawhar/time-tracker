---
name: infer-streams
description: Use when analyzing tt classify output to identify work streams, assign events to streams, and generate time reports. Triggers on weekly review, standup prep, or explicit stream inference requests.
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

## Phase 1: Ingest + Sync + Determine Range

**CRITICAL: Run the FULL ingestion pipeline. Partial data = wrong answer.**

```bash
cargo build 2>/dev/null && cargo run -- ingest sessions
```

Then sync ALL remote machines. Check `tt machines` — if any remotes exist, sync them:

```bash
tt machines                    # List known remotes
tt sync <remote-label>         # For EACH remote machine
```

If code in `export.rs`, `import.rs`, or `sync.rs` was changed this session, you MUST deploy the updated binary to remotes first:

```bash
cargo build --release
./scripts/deploy-remote.sh <remote-label>
```

Then determine the time range:

```bash
tt streams list --json
```

If streams exist, start from where the last stream ended. If empty, use "7 days ago" or user-specified range.

## Phase 2: Gather Context

```bash
tt classify --unclassified --json --start "{time_range}"
```

This shows unclassified sessions and event clusters. Use `--summary` for a compact view.
For full detail including gaps, use:

```bash
tt classify --json --unclassified --gaps --start "{time_range}"
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

Build a JSON file matching the `tt classify --apply` format:

```json
{
  "streams": [
    {"name": "project: stream name", "tags": ["project:project-name"]}
  ],
  "assign_by_session": [
    {"session_id": "ses_abc", "stream": "project: stream name"},
    {"session_id": "ses_def", "stream": "project: stream name"}
  ],
  "assign_by_pattern": [
    {
      "cwd_like": "%/project-name/%",
      "start": "2026-02-26T08:00:00Z",
      "end": "2026-02-27T08:00:00Z",
      "stream": "project: stream name"
    }
  ]
}
```

- Use `assign_by_session` for agent session events (all events for that session move together)
- Use `assign_by_pattern` for non-session events (tmux_pane_focus, AFK) by CWD + time range

Apply:

```bash
tt classify --apply assignments.json
```

This creates streams, assigns events, and runs `tt recompute --force` automatically.

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
| Ignoring tmux_pane_focus events | These have NO session_id. Use `assign_by_pattern` in the --apply JSON. |
| Using raw SQL to assign events | **Never.** Use `tt classify --apply`. Raw SQL can split sessions across streams. |
| Skipping ingestion | **Always** `tt ingest sessions` first. |
| Skipping remote sync | **Always** check `tt machines` and sync all remotes. Remote events are often 50%+ of total data. |
| Reporting partial results | **Never** show a report or time number if remotes haven't been synced or events are unassigned. Incomplete data = wrong answer. |
| Starting from "8 hours ago" | Check `tt streams list` — start from where streams end. |
| Treating project as stream | Project = repo. Stream = task/feature. |
| Splitting subdirectories | `/pivot/agents` is part of `pivot`. |
| Streams too coarse | "pivot work" → "pivot: pipeline API redesign". |
| Leaving events unassigned | Everything gets assigned. Use "misc: {activity}" for unclear. |
| Stopping after classify --apply | `tt classify --apply` runs recompute automatically. No separate step needed. |

## Done When

1. All events assigned to streams (check `tt classify --unclassified`)
2. `tt streams list` shows direct/delegated time per stream
3. Report presented to user
4. Report presented to user

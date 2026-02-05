---
name: infer-streams
description: Use when analyzing tt context output to identify work streams, calculate direct/delegated time breakdowns, or assign orphaned events to existing streams.
---

# Stream Inference

Analyze time-tracker context data to identify logical work streams and calculate time breakdowns.

## Key Concepts

**PROJECT** = A codebase/repository (e.g., `pivot`, `time-tracker`, `legion`)
- Identified by the root directory (e.g., `/home/sami/pivot`)
- Subdirectories are part of the SAME project: `/home/sami/pivot/agents`, `/home/sami/pivot/pipe` → all `pivot`

**STREAM** = A specific task, feature, or PR within a project
- More granular than a project
- Examples: "pivot: execution engine refactor", "time-tracker: CLI fixes", "legion: worker skill implementation"
- A stream typically spans hours to 2-3 days, NOT weeks

## Arguments

Optional: Time range (default: "8 hours ago")

Example: `/infer-streams 3 days ago`

## Phase 1: Run Ingestion

**CRITICAL: Always run ingestion before gathering context.** Without this, `tt context` returns stale/incomplete data — you may miss 75%+ of sessions.

```bash
cargo build 2>/dev/null && cargo run -- ingest sessions
```

## Phase 2: Gather Context

```bash
tt context --events --agents --streams --gaps --start "{time_range}"
```

## Phase 3: Identify Projects

**Extract project from path:**
- `/home/sami/pivot` → project: `pivot`
- `/home/sami/pivot/agents` → project: `pivot` (subdirectory)
- `/home/sami/pivot/pipe` → project: `pivot` (subdirectory)
- `/home/sami/time-tracker/default` → project: `time-tracker`

**Detect renamed directories:**
- If two directories have similar content/activity patterns but different names, they may be the same project renamed

## Phase 4: Identify Streams Within Projects

For each project, identify distinct streams using:

1. **Claude session summaries** - Each session's `summary` field describes what was worked on
2. **Starting prompts** - The `starting_prompt` reveals intent
3. **Temporal gaps** - Large gaps (>2 hours) between activity often indicate different streams
4. **Semantic similarity** - Group sessions working on related features

**Stream granularity rules:**
- If multiple sessions have related summaries (e.g., "execution engine", "async migration"), group as ONE stream
- If sessions are unrelated (e.g., "CLI fixes" vs "add inference feature"), they are DIFFERENT streams
- A stream should be describable as a single task/feature, not "all work on project X"

**BAD stream names (too coarse):**
- "pivot work"
- "core platform"
- "legion development"

**GOOD stream names (task-specific):**
- "pivot: execution engine async migration"
- "pivot: pipeline class refactor"
- "pivot: CLI error handling"
- "legion: controller implementation"
- "time-tracker: stream inference feature"

## Phase 5: Calculate Time

**All times reported in Pacific Time (UTC-8).**

**IMPORTANT: Report BOTH direct time AND delegated time. Direct time comes FIRST.**

### Direct Time (report first)
Time the user spent actively interacting (focus events, user messages).

Calculation:
- Sum gaps between consecutive user events where gap < 5 minutes
- If gap > 1 minute on a DIFFERENT stream, that's a stream switch
- Attribute time to the stream that was active at the START of each interval

### Delegated Time (report second)
Time AI agents spent working, with parallelism multipliers.

Calculation:
- For each agent session: `end_time - start_time`
- Multiplier = number of concurrent agents (no discount)
- Example: 3 agents × 10 min = 30 agent-minutes
- Track `parent_session_id` to identify subagents

## Phase 6: Output Report

**All times in Pacific Time.**

**Use ONE consolidated table for the main breakdown (easier to read than separate tables per stream).**

```markdown
## Stream Inference Results

**Time range:** {start_PT} to {end_PT} (Pacific Time)
**Events analyzed:** {count}
**Agents found:** {count}

### Time Breakdown by Stream and Day

**One consolidated table (Direct first, then Delegated):**

| Day (PT) | Project | Stream | Direct | Delegated | Peak Parallelism |
|----------|---------|--------|--------|-----------|------------------|
| Fri Jan 31 | pivot | execution engine refactor | 45 min | 2.5 hrs | 3 agents |
| Fri Jan 31 | pivot | pipeline class refactor | 30 min | 1.8 hrs | 2 agents |
| Fri Jan 31 | time-tracker | CLI fixes | 20 min | 45 min | 1 agent |
| Sat Feb 1 | pivot | execution engine refactor | 1.2 hrs | 4.1 hrs | 5 agents |
| Sat Feb 1 | dotfiles | session repair | 15 min | 30 min | 1 agent |
| **TOTAL** | | | **2.8 hrs** | **9.6 hrs** | |

**Both Direct and Delegated columns are required.**

### Stream Details

For each stream, brief summary:

- **pivot: execution engine refactor** - Sessions: abc123, def456. Async migration work.
- **pivot: pipeline class refactor** - Session: ghi789. Removing global registry.
- **time-tracker: CLI fixes** - Session: jkl012. Bug fixes and cleanup.

### Recommendations

{Any streams that need clarification or manual review}
```

## Common Mistakes

| Mistake | Fix |
|---------|-----|
| Skipping ingestion | **Always** run `tt ingest sessions` before `tt context`. Without it you miss most data. |
| Treating project as stream | Project = repo. Stream = task/feature within repo |
| Splitting subdirectories as different projects | `/pivot/agents` is part of `pivot`, not a separate project |
| Streams too coarse ("pivot work") | Use task-specific names ("pivot: execution engine refactor") |
| Not detecting renamed directories | Check if similar activity patterns suggest a directory rename |
| Leaving events unassigned | EVERYTHING gets assigned. Use "misc: {activity}" if unclear |
| Only reporting delegated time | ALWAYS report BOTH direct and delegated time |
| Separate tables per stream | Use ONE consolidated table for all streams |
| Reporting times in UTC | Always use Pacific Time (UTC-8) |
| Streams lasting multiple days | Look for logical decomposition into streams of activity |

## Done When

1. Every event is assigned to a stream (no unassigned activity)
2. Projects are identified at repo level (subdirectories merged)
3. Streams are task-granular (not just project names)
4. Time breakdown includes direct vs delegated per stream per day
5. All times shown in Pacific Time

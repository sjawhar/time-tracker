---
name: classify-streams
description: Use when streams lack project/activity tags and need classification. Triggers: "classify streams", "tag untagged streams", "map streams to ontology", "tt streams shows untagged", "organize time tracking data".
---

# Classify Streams

Tag untagged streams by mapping them to the shared ontology taxonomy. Uses path heuristics and LLM analysis to assign project and activity tags from `ontology.toml`.

## When to Use

- User requests stream classification or tagging ("classify my streams", "tag time entries")
- `tt context` or `tt streams` output shows streams without project/activity tags
- Organizing historical time tracking data for reports
- After adding new projects/activities to ontology
- Preparing for weekly review (Phase 0 prep step)

## Arguments

- `period`: Optional. Time range to classify (default: "1 week ago"). Natural language or ISO 8601.

Example: `/classify-streams 3 days ago`

## Workflow

1. Load ontology (`.claude/skills/ontology.toml`)
2. Ingest & gather data (`tt ingest sessions`, `tt context --streams --agents`)
3. Identify untagged streams (`tt streams --json`)
4. Classify each stream (path heuristics + LLM fallback)
5. Apply tags (`tt tag <stream-id> <tag>`)
6. Report results (tagged streams + flagged unknowns)
7. Ontology updates (user approval required)

## Phase 1: Load Ontology

Read the shared ontology to get the valid tag taxonomy:

```bash
cat .claude/skills/ontology.toml
```

Parse two lists:
- `projects.names[]` — valid project tags (e.g., `time-tracker`, `pivot`, `legion`)
- `activities.names[]` — valid activity tags (e.g., `development`, `code_review`, `planning`)

**These are the ONLY valid tags.** Do NOT invent tags outside this taxonomy (see Common Mistakes).

Store both lists in memory for use in Phase 4.

## Phase 2: Ingest, Sync & Gather Data

**CRITICAL: Run the FULL ingestion pipeline. Partial data = wrong answer.**

```bash
tt ingest sessions
```

Then sync ALL remote machines — remote events are often 50%+ of total data:

```bash
tt machines                    # List known remotes
tt sync <remote-label>         # For EACH remote machine listed
```

Then gather context for the target period:

```bash
tt context --streams --agents --start "$START"
```

This returns JSON with:
- `streams[]`: Stream IDs, names, working directories, git projects, time totals
- `agents[]`: Claude sessions with `project_name`, `summary`, `tool_call_count`, `start_time`, `end_time`

## Phase 3: Identify Untagged Streams

List all streams with their existing tags:

```bash
tt streams --json
```

For each stream, check existing tags. A stream needs classification if:
- It has **no project tag** (no tag matching any `projects.names[]` entry)
- It has **no activity tag** (no tag matching any `activities.names[]` entry)

**MUST NOT reclassify:**
- Streams that already have a project tag from the ontology
- Streams that already have an activity tag from the ontology
- Streams with manually-applied tags (respect user intent)

Filter to only streams within the target time period (use `first_event_at` / `last_event_at`).

## Phase 4: Classify Each Untagged Stream

For each untagged stream, determine the best project and activity tags.

### Step 4a: Project Tag — Path Heuristics First

Use the stream's `working_directory` and `git_project` fields to infer the project:

**Path-to-project mapping:**
- `/home/sami/time-tracker/...` → `time-tracker`
- `/home/sami/pivot/...` → `pivot`
- `/home/sami/legion/...` → `legion`
- `/home/sami/.dotfiles/...` → `dotfiles`

**Rules (matching `extract_project_from_path()` in `tt-core`):**
1. Strip home directory prefix (`/home/<user>/`)
2. Use the first meaningful directory component as the project name
3. Subdirectories belong to the parent project (`/pivot/agents` → `pivot`)
4. Known container dirs (`projects/`, `repos/`, `src/`, `code/`) → use next component
5. Generic paths (`/tmp`, `/var`, `/home/user`) → no project, needs LLM

**Match extracted name against `projects.names[]`:**
- Exact match → use it
- Case-insensitive match → use the ontology's canonical form
- No match → flag for LLM analysis (Step 4c)

### Step 4b: Activity Tag — Session Content Analysis

Infer activity type from session metadata:

| Signal | Activity Tag |
|--------|-------------|
| High `tool_call_count`, code edits in summary | `development` |
| Summary mentions "review", "PR review" | `code_review` |
| Summary mentions "plan", "design", "architecture" | `planning` or `design` |
| Summary mentions "debug", "fix", "investigate" | `development` |
| Summary mentions "deploy", "CI", "infra" | `ops` |
| Summary mentions "docs", "README", "documentation" | `writing` |
| Summary mentions "research", "evaluate", "explore" | `research` |
| Low tool calls, mostly conversation | `planning` or `meetings` |

If the stream has agent sessions, use `summary` and `starting_prompt` fields. When `summary` is null, check `starting_prompt` (truncated to 100 chars).

### Step 4c: LLM Fallback for Ambiguous Streams

When path heuristics fail or activity is unclear, gather more detail:

```bash
tt context --events --streams --start "$STREAM_START" --end "$STREAM_END"
```

Then use LLM reasoning to map to the **closest ontology match**. The LLM prompt must:
1. Include the full list of valid project and activity tags from ontology
2. Include stream metadata (working dirs, git project, session summaries, event types)
3. Instruct: "Choose the BEST matching tag from the provided lists. If no tag fits, respond with 'unknown'"

**CRITICAL: The LLM selects from the ontology — it does NOT create new tags.**

### Step 4d: Handle Unknown Streams

If neither heuristics nor LLM can confidently map a stream:
- Mark it as `unknown` (do NOT tag it)
- Record the stream ID, metadata, and LLM's best guess for Phase 7
- Continue to next stream

## Phase 5: Apply Tags

For each classified stream, apply tags:

```bash
# Apply project tag
tt tag <stream-id> <project-tag>

# Apply activity tag
tt tag <stream-id> <activity-tag>
```

**Apply tags one at a time.** Verify each command succeeds before continuing.

**Do NOT apply:**
- Tags not in ontology.toml
- Tags to streams that already have that tag type
- The `unknown` placeholder — it's for reporting only

## Phase 6: Report Results

Present a summary of all actions taken:

```markdown
## Stream Classification Report

**Period:** {start} to {end}
**Streams analyzed:** {total}
**Newly tagged:** {count}
**Already tagged (skipped):** {count}
**Unknown (flagged):** {count}

### Newly Tagged Streams

| Stream ID | Stream Name | Project Tag | Activity Tag | Source |
|-----------|-------------|-------------|--------------|--------|
| abc123 | pivot refactor | pivot | development | path |
| def456 | tt CLI fixes | time-tracker | development | path |
| ghi789 | team planning | org | planning | llm |

### Unknown Streams (Need Review)

| Stream ID | Stream Name | Best Guess | Reason |
|-----------|-------------|------------|--------|
| jkl012 | misc session | ??? | No matching project in ontology; working dir was /tmp |
```

## Phase 7: Ontology Update Proposals

If any streams were flagged as `unknown` in Phase 4d, present suggested ontology additions for **user approval**:

```markdown
### Suggested Ontology Additions

Based on {N} unclassified streams, consider adding:

**Projects:**
- `new-project-name` — seen in 3 streams, working dir `/home/sami/new-project-name`

**Activities:**
- `new-activity` — seen in 2 streams, pattern: {description}

Would you like to add any of these to the ontology? (I will NOT modify ontology.toml without your approval.)
```

**CRITICAL: NEVER auto-modify `ontology.toml`.** Only update it after explicit user approval.

If the user approves:
1. Add the new entries to `ontology.toml`
2. Re-run classification for the previously unknown streams
3. Apply the new tags

## Common Mistakes

| Mistake | Fix |
|---------|-----|
| Skipping ingestion | **Always** run `tt ingest sessions` before `tt context`. Without it you miss most data. |
| Skipping remote sync | **Always** check `tt machines` and sync all remotes before classification. Remote events are often 50%+ of total data. |
| Inventing tags outside ontology | **Only** use tags from `ontology.toml`. Flag unknowns for user approval. |
| Reclassifying already-tagged streams | Check existing tags first. Skip streams that already have the relevant tag type. |
| Auto-extending ontology | **Never** modify `ontology.toml` without user approval. Present suggestions, wait for confirmation. |
| Using project name as activity tag | Projects and activities are separate taxonomies. A stream needs one of each. |
| Ignoring `starting_prompt` | When `summary` is null, `starting_prompt` often reveals the work's intent. |
| Tagging with "unknown" | `unknown` is a reporting status, not a tag. Leave unclassifiable streams untagged. |
| Not matching ontology casing | Use the exact string from `ontology.toml` (e.g., `time-tracker` not `Time Tracker`). |
| Classifying streams outside period | Filter by `first_event_at` / `last_event_at` within the target period. |
| Running tt commands wrong | Use `tt` not `cargo run --`. Build once, then use the binary. |

## Fallback Handling

**If `tt context` returns no streams:**
- Verify ingestion ran successfully
- Check the time period isn't too narrow
- Try `tt streams --json` directly to confirm data exists

**If `tt tag` fails:**
- Verify stream ID exists with `tt streams --json`
- Check tag string matches ontology exactly (case-sensitive)
- Report the error and continue with remaining streams

**If ontology.toml is missing:**
- Stop and report: "Ontology file not found at `.claude/skills/ontology.toml`. Create it before running classify-streams."
- Do NOT proceed without the ontology constraint

**If all streams are already tagged:**
- Report "All streams in period already classified" and exit cleanly

## Done When

1. Ontology loaded and validated
2. All streams in period checked for existing tags
3. Untagged streams classified using path heuristics + LLM fallback
4. Tags applied only from ontology taxonomy
5. Unknown streams flagged with suggested ontology additions
6. Report presented with tagged + unknown breakdown
7. Ontology modifications only after explicit user approval

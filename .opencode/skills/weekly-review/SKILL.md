---
name: weekly-review
description: Use when the user asks to "run weekly review" OR wants to reflect on the past week with historical trend analysis and time tracking data. Trigger phrases: "weekly review", "review the week", "weekly reflection", "how did last week go".
---

# Weekly Review

Interactive weekly reflection with time-tracker integration and historical trend analysis.

**Run on Sundays** - Reviews the previous week (Monday through Sunday).

## When to Use

- User says "run weekly review", "do my weekly", or "weekly reflection"
- User wants to review the past week's work with historical context
- User mentions analyzing time allocation or productivity trends
- It's Sunday and user wants to reflect on the previous week

**Trigger phrases**: "weekly review", "review the week", "how did last week go", "weekly reflection"

## Arguments

**Required** (first argument): Path to JSONL data file
- Example: `/weekly-review ~/app/ignore/weekly-reviews.jsonl`

The file will be created if it doesn't exist. Data is appended (one JSON object per line).

## Quick Reference

0. **Classify**: Invoke classify-streams to tag untagged streams
1. **Trends**: Show 8-week historical patterns with bar charts
2. **Data**: Ingest sessions + fetch tt report for time breakdown
3. **Context**: Present time tables, session summary, reflection questions
4. **Narrate**: User speaks freely, Claude tracks and maps to structure
5. **Structure**: Present filled form, highlight actual vs perceived gaps
6. **Challenge**: Red team pushes back on patterns and blind spots
7. **Save**: Append to JSONL file

## Critical Constraints

These rules take priority over other instructions:

1. **Don't condense or editorialize user prose** — When organizing user's words into sections, preserve their full text (removing filler words is fine). Don't summarize "I'm still not prioritizing PMing monitorability enough. I'm not attending stand-ups yet" into "Not prioritizing well". The detail enables future red-teaming.

2. **Preserve full detail for future analysis** — Specific names, events, dates, and context are the value. Future red-teaming depends on being able to reference "Beth" or "Tuesday's planning meeting" — generic summaries lose this.

3. **Confirm before phase transitions** — Always confirm with the user before moving from narration (Phase 4) to structured presentation (Phase 5). Don't assume "done" from ambiguous signals.

## Argument Parsing

The JSONL path is passed as the skill's `args` parameter:

```
/weekly-review ~/path/to/weekly-reviews.jsonl
```

**Handling**:
- If `args` is empty or missing, prompt the user: "Please provide a path to your weekly reviews JSONL file"
- Expand `~` to the user's home directory (use `$HOME` or equivalent)
- Create the file if it doesn't exist (first review will have no historical trends)

## Config Loading

**Review config**: Read `.claude/skills/weekly-review/config.toml` for:
- `[prompts]` — reflection questions
- `[goals]` — checkbox items
- `[ratings]` — rating dimensions and scale
- `[review]` — output path, week start day

**Ontology config**: Read `.claude/skills/ontology.toml` for:
- `projects.names[]` — valid project tags
- `activities.names[]` — valid activity tags

**Fallbacks**: If either config is missing, use sensible defaults. The review can proceed without config — it just uses built-in defaults for questions, goals, and ratings.

## Phase 0: Classify Streams (Prep)

**REQUIRED**: Tag untagged streams before collecting report data. This ensures `tt report` has meaningful `by_tag` groupings.

Invoke the classify-streams skill:

```
/classify-streams 1 week ago
```

This runs the full classification pipeline: load ontology, ingest sessions, identify untagged streams, classify using path heuristics + LLM, apply tags, and report results.

**Wait for classification to complete** before proceeding to Phase 1.

**Fallback**: If classify-streams fails or is unavailable:
1. Inform the user: "Stream classification unavailable — proceeding with existing tags. Report data may have more entries in the 'untagged' bucket."
2. Continue to Phase 1. The review still works — just with less structured tag data.
3. Do NOT block the entire review on classification failure.

## Phase 1: Trend Analysis

1. **Load historical data** from the provided JSONL path
   - If file doesn't exist, start fresh (no historical trends to show)
   - Parse last 8 weeks of entries

2. **Fetch 8-week trend data** from tt:

```bash
tt report --weeks 8 --json
```

This returns `{ "weeks": [{week1}, {week2}, ...] }` with each week containing `by_tag`, `totals`, `period`, and `agent_sessions`.

3. **Generate trend visualizations** using `plotext` via `uvx`:

```bash
uvx --with plotext python -c "
import plotext as plt

weeks = ['Nov23', 'Nov30', 'Dec7', 'Dec14', 'Dec21', 'Dec29', 'Jan4', 'Jan11']
development = [30, 30, 40, 20, 30, 50, 40, 45]

plt.bar(weeks, development)
plt.title('Development ↗ up')
plt.ylim(0, 60)
plt.plotsize(40, 12)
plt.theme('clear')
print(plt.build())
" | sed 's/\x1b\[[0-9;]*m//g'
```

**For each major activity/rating**, generate a separate chart. Show 3-4 charts that are most relevant:
- Activities with biggest changes (up or down trends)
- Any ratings that hit unusually low values (from JSONL history)
- The user's top time-consuming activities

**Cross-reference**: Combine JSONL historical data (perceived time, ratings, reflections) with tt report data (actual time per tag) for richer trend analysis.

**Annotate anomalies** in the title or after the chart:
- "Mental: 3 ← lowest in 8 weeks"
- "Development ↗ trending up (avg 35%, last week 45%)"

4. **Detect patterns**:
   - Recurring themes in bottlenecks/mistakes (look for repeated words/phrases in JSONL history)
   - Correlation between low ratings and specific activities
   - Priority tracking: did last week's priorities appear in this week's successes?
   - Surface anomalies: "You've mentioned sleep issues in 3 of the last 5 weeks"

5. **Present trends**: "Here's how your past 8 weeks looked. Keep this in mind as you reflect."

## Phase 2: Data Collection

**CRITICAL: Run the FULL ingestion pipeline. Partial data = wrong answer. Do NOT skip steps or present incomplete numbers.**

```bash
tt ingest sessions
```

Then sync ALL remote machines — remote events are often 50%+ of total data:

```bash
tt machines                    # List known remotes
tt sync <remote-label>         # For EACH remote machine listed
```

Check for unassigned events. If any exist, run stream inference BEFORE generating the report:

```bash
# Quick check — if unassigned count > 0, you MUST run infer-streams
tt context --events --start "$WEEK_START" --end "$WEEK_END" 2>/dev/null | python3 -c "
import json,sys; d=json.load(sys.stdin)
u=sum(1 for e in d.get('events',[]) if not e.get('stream_id'))
print(f'Unassigned: {u}')
"
```

If unassigned > 0, invoke `/infer-streams` before continuing. Then recompute:

```bash
tt recompute --force
```

Then fetch the current week's report data:

```bash
tt report --last-week --json
```

This returns JSON with:
- `by_tag[]`: Time grouped by tag (project and activity tags) — `tag`, `time_direct_ms`, `time_delegated_ms`, `streams[]`
- `untagged[]`: Streams without tags — `stream_id`, `stream_name`, `time_direct_ms`, `time_delegated_ms`
- `totals`: `time_direct_ms`, `time_delegated_ms`
- `period`: `start`, `end`
- `agent_sessions`: `total`, `by_source`, `by_type`, `top_sessions[]`
- `week_start_day`: "monday"

**Supplementary details** (optional, for richer session context):

```bash
tt context --agents --streams --start "$WEEK_START" --end "$WEEK_END"
```

This provides full agent session data with `summary`, `starting_prompt`, `tool_call_count`, etc.

## Phase 3: Present Context

1. **Load ontology** from `.claude/skills/ontology.toml` — use `projects.names[]` and `activities.names[]` to organize the presentation.

2. **Show computed time allocations** from tt report data:

```
Time by Tag (from tt report):

Projects:
  time-tracker:  12.5h direct | 8.2h delegated  (35%)
  pivot:          5.0h direct | 15.0h delegated  (28%)
  legion:         2.0h direct | 3.5h delegated   (8%)
  [untagged]:     1.5h direct | 0.5h delegated   (3%)

Activities:
  development:   15.0h direct | 22.0h delegated  (52%)
  code_review:    3.0h direct | 0h delegated      (4%)
  planning:       2.0h direct | 1.0h delegated     (4%)
```

3. **Show agent session summary**:

```
Agent Sessions (Mon-Sun):
  Total: 47 sessions
  By source: Claude Code 12, OpenCode 35
  By type: User 30, Subagent 17

  Top sessions by duration:
    1. "Implement TUI extraction" (pivot, 4.2h)
    2. "Weekly review feature plan" (time-tracker, 2.1h)
    3. ...
```

4. **Display reflection questions** from `config.toml` `[prompts]` section (or defaults):
   - Time allocation (activities and projects)
   - Goal tracking checkboxes (from `[goals]`)
   - Reflection questions (successes, mistakes, bottlenecks)
   - Self-assessment ratings (from `[ratings]`, 1-7 scale)
   - Next week priorities

5. **Prompt**: "tt says you spent X% on Development, Y% on Code Review. You had N agent sessions across M projects. Does that feel right? Start narrating your thoughts — I'll track them."

## Phase 4: Free-Form Narration

**Let the user speak/type freely.** Track and map to form sections:

- **Time corrections**: "Actually spent more time on Ops than tt shows" → adjust activities
- **Goal tracking**: "I paired with Thomas on Tuesday" → mark checkbox
- **Reflections**: "The deploy went smoothly" → successes; "Should have communicated earlier" → mistakes
- **Ratings**: "Productivity was maybe a 5" → track rating
- **Priorities**: "Next week I need to focus on the viewer" → next_week.priorities

**Brief acknowledgments**: "Got it — tracking as a bottleneck" (keep responses minimal)

**CRITICAL: Don't condense or editorialize.** When organizing user's words into sections:
- Reorganize into the appropriate fields, but preserve the user's full prose
- Don't summarize "I'm still not prioritizing PMing monitorability enough. I'm not attending stand-ups yet, I don't have daily syncs with Lawrence" into "Not prioritizing well"
- The detail is the value — future red-teaming depends on specific names, events, and context
- If the user gives bullet points, keep them as bullet points
- If the user gives long prose, keep it as long prose

**Exit condition**: When user signals completion ("done", "ready", "that's it", or similar):
1. Summarize what you've captured: "I have: 3 successes, 2 mistakes, 1 bottleneck, ratings for mental/productivity/engagement..."
2. Ask for confirmation: "Ready to see the structured form, or want to add more?"
3. Only proceed to Phase 5 after explicit confirmation

## Phase 5: Structured Presentation

1. **Present the filled-in form** with all sections organized:

```
## Time Allocation

### Activities (perceived → actual)
- Development: 30-40% (tt: 52%)    ← gap: tt shows more
- Meetings: 20-30% (tt: 5%)        ← gap: perceived much higher
- Code Review: 10-20% (tt: 4%)
...

### Projects (perceived → actual)
- time-tracker: 40-50% (tt: 35%)
- pivot: 20-30% (tt: 28%)
...

### Direct vs Delegated Time
- Total direct: 21.0h
- Total delegated: 27.2h
- Delegation ratio: 56% delegated

## Goals
✓ Paired with an engineer
✓ Left comments on writeups
✗ Read team writeups
Notes: "Paired with Thomas on the auth bug"

## Reflection
Successes: Got the feature shipped ahead of schedule...
Mistakes: Should have communicated the timeline change earlier...
Bottlenecks: Waiting on PR reviews blocked me for a day...
Action: Set up office hours for quick reviews
Priorities check: Yes, working on the highest impact item
Iteration: Add a daily standup check-in

## Ratings
Mental: 5  Productivity: 6  Prioritization: 5
Time mgmt: 4  Engagement: 5  Overall: 5

## Next Week
Priorities: 1. Ship viewer  2. Onboard Rafael  3. Plan Q1
On track (project): Yes
On track (personal): Unsure
```

2. **Highlight gaps**: "Missing: What's one thing you'll change next week?"

3. **Show discrepancies**: "Your perceived Meeting time (20-30%) vs tt actual (5%) — are you counting informal conversations that tt doesn't track?"

4. **Show actual vs perceived time** side by side. The gap between these is valuable for red-teaming.

5. **Ask for edits**: "Anything to adjust before we continue?"

## Phase 6: Red Team Challenge

**Launch the `red-teamer` agent** (using Task tool with `subagent_type: "red-teamer"`).

**Context to pass to the agent** (include all of this in the prompt):
1. **Current week data**: The complete structured form from Phase 5 (time allocations, reflections, ratings, priorities)
2. **tt data**: Actual time per tag (direct + delegated), delegation ratio, agent session counts
3. **Historical trends**: Summary of last 8 weeks — recurring themes in successes/mistakes/bottlenecks, rating patterns, priority follow-through
4. **Repeated patterns**: Phrases or themes that appear in multiple weeks (e.g., "communicate better" appearing 3 times)

**Challenge areas for the agent to probe**:
- Compare self-assessment vs objective tt data (perceived vs actual time)
- Delegation patterns — too much? too little? right balance?
- Recurring bottlenecks that haven't been addressed
- Priorities that don't align with actual time spent
- Patterns from history: "You've said 'communicate better' for 3 weeks — what's blocking that?"
- Blind spots: activities consuming time without mention

**Present challenges** to the user. Let them respond and revise if needed.

## Phase 7: Save

1. **Build the JSON object** with all collected data (see schema below)
   - Include `red_team` with challenges raised and user responses
   - Ensure `week.start` and `week.end` are filled (YYYY-MM-DD)
   - Don't abbreviate reflection fields — preserve the user's full prose as they said it

2. **Show the user what will be saved** before saving
   - Display the reflection fields in full (these are the most important)
   - Ask: "Does this capture everything? Ready to save?"

3. **Append to the provided JSONL file**
   - Each line is one complete JSON object
   - Append with newline separator

4. **Show summary**: "Week of Jan 12-18 saved. Review #31."

## JSONL Schema

Each line in `weekly-reviews.jsonl` is a JSON object. See the full example in `.claude/skills/weekly-review/example.json`.

**Field notes**:
- **`reflection.*`**: Preserve the user's full prose — don't condense. The detail enables future red-teaming.
- **`red_team`**: Captures the red team challenges and user's responses for future reference.
- **Time semantics** — two different representations:
  - `activities` and `projects`: User's *perceived* time allocation (ranges like "0-5", "10-20", "40-50"). These reflect how the user *felt* they spent their time, adjusted from tt data based on narration.
  - `tt.*`: *Actual* recorded time from time-tracker (millisecond-precise). Raw data — direct (human focus) and delegated (agent work) per tag. Not adjusted.
  - The gap between perceived and actual is valuable for red-teaming ("You felt you spent 30% on meetings but tt shows 5%").
- **`tt.by_tag`**: Contains both project and activity tags. Each tag has `direct_ms` and `delegated_ms`. Multi-tagged streams are fully attributed to each tag.
- **`tt.agent_sessions`**: Session counts from tt report data.
- `goals.completed`: Array of checkbox IDs that were checked.
- `ratings`: Integer 1-7 scale.
- `next_week.on_track_*`: "yes", "no", or "unsure".
- **`optional`**: For ad-hoc reflection prompts. The `prompt` field is the question asked, `response` is the user's answer.
- **`week.start` and `week.end`**: Always fill these (YYYY-MM-DD format) — don't leave empty.

## Fallback Handling

### Classification Failure (Phase 0)
If classify-streams is unavailable or fails:
1. Inform the user: "Stream classification unavailable — proceeding with existing tags"
2. Skip Phase 0, continue to Phase 1
3. Report data will have more entries in the "untagged" bucket — this is acceptable

### tt Report Failure (Phase 2)
If `tt report` is unavailable or fails:
1. Inform the user: "tt report unavailable — we'll do manual time entry"
2. Skip Phase 2 data collection
3. In Phase 3, present empty time allocation tables
4. Ask user to estimate percentages directly: "How did you spend your time this week? Estimate percentages for your main activities."
5. In Phase 7, the `tt` field will be empty/null

### Chart Failure (Phase 1)
If `plotext` fails or produces garbled output:
1. Fall back to text-based visualization:
```
Development:  ████████████████████ 52%
Code Review:  ██████ 12%
Planning:     ████ 8%
Writing:      ████ 8%
Other:        ██████████ 20%
```
2. Use Unicode block characters (█) scaled to percentage
3. Still show trend indicators: "↗ up from 30%" or "↘ down from 50%"

### Config Missing
If `.claude/skills/weekly-review/config.toml` is missing:
- Use built-in defaults for reflection questions, goals, ratings
- Log a note: "No config found — using defaults"

If `.claude/skills/ontology.toml` is missing:
- Skip ontology-based organization in Phase 3
- Present raw tt report data without project/activity grouping

## Edge Cases

- **Missing JSONL file**: Start fresh, no historical trends to show
- **Malformed JSON lines**: Skip and warn, continue with valid data
- **No tt data**: Show empty tables, proceed with manual entry (see Fallback Handling)
- **Empty by_tag (all untagged)**: Present untagged streams by name. Classification may not have run or may have found no matching tags.
- **User skips sections**: Fill with empty values, note gaps
- **Not Sunday**: Warn but allow running anyway ("Running mid-week — date range may be unexpected")
- **Empty weeks in trend data**: Show gaps in charts, note "no data for week of X"

## Common Mistakes

| Mistake | Fix |
|---------|-----|
| Skipping Phase 0 (classify-streams) | **Always** run classify-streams first. Tagged data makes the entire review more meaningful. |
| Skipping ingestion | **Always** run `tt ingest sessions` before `tt report`. Without it you miss most data. |
| Skipping remote sync | **Always** check `tt machines` and sync all remotes. Remote events are often 50%+ of total data. |
| Presenting partial results | Run the FULL pipeline (ingest → sync → infer streams → recompute → report). Don't stop partway and show incomplete numbers — it's worse than no answer. |
| Not running stream inference | If unassigned events exist, run `/infer-streams` BEFORE generating the report. Unassigned events = missing time. |
| Calling Toggl MCP tools | Use `tt report` exclusively. tt replaces Toggl for this workflow. |
| Hardcoding project/activity lists | Read from `ontology.toml`. Never hardcode taxonomy. |
| Condensing user prose | Preserve full text. "Not prioritizing well" loses the detail that enables red-teaming. |
| Skipping phase confirmations | Always confirm before transitioning from narration (Phase 4) to structured (Phase 5). |
| Using `cargo run --` instead of `tt` | Use the `tt` binary directly. Build once if needed, then use the binary. |
| Missing `week.start`/`week.end` | Always populate these in the JSONL output (YYYY-MM-DD format). |
| Using `toggl` field in JSONL | Use the `tt` field. Schema uses time-tracker data, not Toggl. |
| Rushing through narration | Let the user talk. Brief acknowledgments only. Don't lead or suggest what they should say. |
| Including reflect-style analysis | Session pattern analysis is a separate skill. Weekly review focuses on time, goals, and reflection. |
| Using `tt context --streams` for time data | `context --streams` returns **lifetime** per-stream totals from the DB, not period-scoped. Use `tt report` for time within a specific period. |

## Goal Tracking Checkboxes

Read goal definitions from `config.toml` `[goals]` section. Default goals if config is missing:

- `paired_engineer`: Paired with an engineer this week
- `read_writeups`: Read team members' writeups
- `left_comments`: Left comments on others' work
- `gave_feedback`: Gave constructive feedback
- `asked_help`: Asked for help when stuck
- `documented`: Documented decisions or learnings
- `mentored`: Mentored or helped onboard someone
- `shipped`: Shipped something to users

## Notes

- Uses `tt report` and `tt context` for time data — NOT Toggl
- Red team uses Task tool with `subagent_type: "red-teamer"`
- **Charts**: Use `uvx --with plotext python` for terminal bar charts (no install needed)
- **Taxonomy**: Read from shared `ontology.toml`, not hardcoded
- **Direct vs delegated**: tt tracks human-focus time (direct) and agent-work time (delegated) separately. Both are valuable for the review.
- JSONL format allows easy appending and parsing
- **Config precedence**: `config.toml` settings > built-in defaults. Ontology from `ontology.toml` > no ontology.

## Appendix: Toggl Calibration (Optional)

Before fully switching from Toggl to tt, you may want to compare time tracking for the same week to verify accuracy.

### One-Time Comparison

1. Pick a recent week where you used both Toggl and tt
2. Run `tt report --weeks 1 --json` for that week
3. Query Toggl API for the same week (or export from Toggl UI)
4. Compare total hours by project/activity

### Example Comparison

| Project | Toggl | tt (direct + delegated) | Difference |
|---------|-------|-------------------------|------------|
| time-tracker | 8.5h | 9.2h | +0.7h |
| legion | 4.2h | 3.8h | -0.4h |
| meetings | 2.0h | 2.1h | +0.1h |

### Allocation Parameters

tt's time calculation uses these defaults (configurable in `AllocationConfig`):
- **attention_window_ms**: 60000 (60 seconds) — events within this window are grouped as continuous focus
- **agent_timeout_ms**: 1800000 (30 minutes) — agent sessions idle longer than this are considered ended

These determine how tt interprets event patterns into focus time. Adjust if you find consistent over/under-reporting compared to Toggl.

### After Calibration

Once satisfied with accuracy, remove Toggl from Phase 2 data collection and rely solely on tt.

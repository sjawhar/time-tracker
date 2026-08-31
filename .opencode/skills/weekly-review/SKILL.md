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
- Example: `/weekly-review ~/OneDrive/Apps/TimeTracker/data/weekly-reviews.jsonl`

The file will be created if it doesn't exist. Data is appended (one JSON object per line).

## Quick Reference

1. **Classify**: Tag untagged streams — the data-integrity gate, NOT optional prep (see Phase 1). Every later number depends on it. Includes primary-source verification: read the week's own user messages before trusting any stream name.
2. **Trends**: Show 8-week historical patterns with bar charts
3. **Data**: Ingest sessions + fetch tt report for time breakdown
4. **Context**: Present time tables, session summary, reflection questions
5. **Narrate**: User speaks freely, Claude tracks and maps to structure
6. **Structure**: Present filled form, highlight actual vs perceived gaps
7. **Challenge**: Red team pushes back on patterns and blind spots
8. **Save**: Append to JSONL file
9. **Post-review actions** *(optional)*: Run user-configured follow-up instructions (e.g. 1-on-1 prep, standup draft) defined in `weekly-review.toml` `[post_review_actions]`

## Critical Constraints

These rules take priority over other instructions:

1. **Don't condense or editorialize user prose** — When organizing user's words into sections, preserve their full text (removing filler words is fine). Don't summarize "I'm still not prioritizing PMing monitorability enough. I'm not attending stand-ups yet" into "Not prioritizing well". The detail enables future red-teaming.

2. **Preserve full detail for future analysis** — Specific names, events, dates, and context are the value. Future red-teaming depends on being able to reference "Beth" or "Tuesday's planning meeting" — generic summaries lose this.

3. **Confirm before phase transitions** — Always confirm with the user before moving from narration (Phase 5) to structured presentation (Phase 6). Don't assume "done" from ambiguous signals.

4. **`tt report` is the single source of truth for time — never re-derive it.** Every time number comes from `tt report`, which already merges ALL focus sources (tmux pane focus, COSMIC window-focus, browser, afk) into one allocation. NEVER compute active/direct time yourself in python or SQL, NEVER pick-and-choose event types (no "watcher-only" vs "terminal-only" — they are one focus timeline), and NEVER hand the user a tight/loose range — the allocator yields one exact number. If a figure looks impossible (a full workday reading ~3h, or direct ≪ a normal day), **suspect the BINARY or the DATA, not the algorithm**: check `tt --version` against the repo's latest release / `Cargo.toml` and rebuild/`mise upgrade` if stale, then run the Phase 1 coverage gate. Hand-rolling time math in python is the exact failure that turns a stale-binary bug into hours of wrong analysis — do not do it.

5. **Coverage is part of the review, not a precondition someone else handles.** If the week's activity is not attributed, the numbers are not worth reading (Phase 1 tells you exactly what to check and what to do). Presenting, saving, or narrating over a large unattributed remainder — or telling the user "there's a bunch of unclassified time" and moving on — is a failed review. You close the gap by getting the classifier caught up and correcting what is demonstrably wrong, never by inventing buckets. The gate is re-checked at Phase 4 (before presenting) and Phase 8 (before saving).

## Argument Parsing

The JSONL path is passed as the skill's `args` parameter:

```
/weekly-review ~/OneDrive/Apps/TimeTracker/data/weekly-reviews.jsonl
```

**Handling**:
- If `args` is empty or missing, default to `~/OneDrive/Apps/TimeTracker/data/weekly-reviews.jsonl`
- Expand `~` to the user's home directory (use `$HOME` or equivalent)
- Create the file if it doesn't exist (first review will have no historical trends)

## Config Loading

**Review config**: Read `~/.config/time-tracker/weekly-review.toml` for:
- `[prompts]` — reflection questions
- `[goals]` — checkbox items
- `[ratings]` — rating dimensions and scale
- `[review]` — output path, week start day
- `[post_review_actions]` — user-defined freeform instructions for Phase 9

**Ontology config**: Read `~/.config/time-tracker/ontology.toml` for:
- `projects.names[]` — valid project tags
- `activities.names[]` — valid activity tags

**Fallbacks**: If either config is missing, use sensible defaults. The review can proceed without config — it just uses built-in defaults for questions, goals, and ratings.

## Phase 1: Verify Classification Coverage — DATA-INTEGRITY GATE (do not skip)

**This is not prep. It is the gate the entire review depends on.** `tt report`'s `by_tag` and
the perceived-vs-actual comparison (Phase 6) are only worth reading if the week's activity is
actually attributed to streams.

### Why you cannot move on without this

The only reason to pull tt data at all is to give the user an **objective check against self-delusion.** The reflection (Phases 5–6) is where the user grades their week; the tt numbers are the one thing keeping that grade honest. If a large share of the week's time is unattributed, that check is broken — the user is grading on vibes, which is the exact failure this skill exists to prevent. A review built on half-attributed data is *worse* than one with no data, because it looks grounded while being arbitrary.

**Flagging the gap is NOT closing it.** Writing "⚠️ coverage caveat — treat as floors" and proceeding is the single most common failure here. It feels responsible and it ships the broken instrument anyway. Do not do it. Either the time is attributed, or you stop.

### You consume classifications; you do not produce them

The `tt-serve` daemon classifies continuously — it ingests new sessions every ~30s and runs the
LLM classifier on a short debounce. **There is exactly one machine-inference path, `tt classify
--auto`, and you are not it.** That path holds every guard that makes inference safe: it picks
candidates newest-first under a per-pass cap, inherits subagents from their parents, filters
harness-injected text, refuses to invent a container for work it cannot place, and leaves
anything ambiguous unassigned on purpose.

So your job here is to check coverage and, if the classifier is behind, let it catch up:

```bash
tt classify --auto        # one bounded pass, newest-first; safe to run on demand
tt report --week --json   # re-read the totals afterwards
```

**Never author stream assignments in bulk** — not from a JSON blob, not by cwd pattern, not by
time range, and above all not with raw SQL against `tt.db`. Those surfaces are gone because an
agent using them minted **68 misnamed streams across 55,031 events** (weekly buckets like
`workorder-5: IPI envs (Jun14-20)`, catch-alls like `agent-c: misc`). The ontology rules that
were supposed to prevent that lived in prose, and prose is not a guard.

### The two corrections you may make

Both are narrow and both require you to have actually looked at the work:

```bash
tt proposals ls                              # classifier suggestions awaiting a human
tt proposals accept <id>                     # confirm one
tt proposals reject <id> [--stream <ref>]    # reject, optionally redirecting its events

tt streams assign <stream-ref> --session <id>    # a specific session you know is misfiled
tt streams assign <stream-ref> --event <id>      # a specific event you know is misfiled
```

`tt streams assign` writes `assignment_source = "user"`, which no machine writer will ever
overwrite. That is exactly why it must stay small: name only the sessions and events you have
actually judged, and never use it to sweep a remainder. It will not create the target stream —
if the stream should exist, `tt streams create` it deliberately first.

### Primary-source verification — read the week's messages before trusting any stream name

Coverage says every event has *a* stream; it says nothing about whether the streams are
*right*. The check that catches what every mechanical pass misses is reading the primary
source: the user's own messages. It is small — a week is roughly 100–150 top-level
sessions — and it takes minutes with a digest, not hours.

1. **Extract the week's human turns** from the agent-session transcripts (top-level session
   files only — subagent "user" turns are dispatch prompts written by other agents, not the
   user). Filter harness-injected text with the same leading-marker rules
   `tt_core::injection` uses.
2. **Build a per-session digest**: session id, turn count, first/middle/last turns, and the
   stream its events currently carry. Read all of it.
3. **Judge each session's stream against what the user actually said.** The defect classes
   this reliably catches, none of which coverage numbers or window titles reveal:
   - **Split conversations** — a continued/resumed session recorded under a new id whose
     halves landed on different streams (one conversation, two rows).
   - **Conglomerates** — an "A + B" stream gluing two unrelated initiatives that interleaved
     in time. The converse is NOT a defect: one initiative with two facets ("verification +
     the report it feeds") is coherent.
   - **Streams named after the vehicle, not the work** — a session name, a dispatch
     mechanism, an output artifact. A stream names an initiative.
   - **Duplicate families** — one long-running thread re-minted under many near-identical
     names as its topic drifted day to day.
   - **Anchor-less accretion** — a stream with zero sessions and zero user messages whose
     time is pure ambient focus events; its events belong to whatever the user was actually
     messaging at those moments.
4. **Repair through the sanctioned surfaces only** — `tt streams merge` for duplicate
   families, `tt streams rename` for wrong names, `tt streams assign --session` for misfiled
   sessions, `tt streams dissolve` for rows that never named work. Never invent a container
   for what the reading cannot place.

When a time figure looks wrong to the user, decompose it to the event level before defending
it: a stream can be exactly right and still surprising (a standing thread touched 100+ times
in five-minute sips reads as hours the user never *felt*), or plausibly named and completely
hollow (all ambient focus, no sessions). The event composition tells you which.

**Operator guidance for this verification lives in config, never in this skill.** Two homes:

- `~/.config/time-tracker/weekly-review.toml` may carry a `[classification]` section with a
  freeform `instructions` string — domain vocabulary, entity glossaries, which initiatives the
  operator considers one workstream vs several, known duplicate-prone thread names. Load it (if
  present) before judging any session's stream and treat it as the operator's own taxonomy
  rulings. Absent section: proceed on the generic rules above.
- The always-on equivalents for the automatic classifier are tt's own `[classifier]`
  `context_instructions` (a prompt block every classification sees) and `context_command`
  (a lookup command the model may call mid-classification). Anything the verification keeps
  re-teaching belongs there, so the daemon learns it too.

### Hard gate — do NOT advance to Phase 2 until ALL THREE hold

1. You checked coverage against `tt report` (not just glanced at a session list).
2. **`tt report` `totals.unassigned_direct_ms` is small enough that the week's shape is not in
   question** — minutes, or a residue that cannot move any project's share. Hours of unattributed
   *direct* time means the instrument is broken; do not rationalize it, present it, or save it.
3. You ran the primary-source verification above and repaired what it caught — a week whose
   coverage is perfect but whose stream names are wrong presents confidently and reviews
   garbage.

**Why direct and not delegated:** direct time is the human-attention signal the whole review rests on. 8 unattributed hours is an entire workday you cannot see — enough to flip "you barely touched sales" into "sales was a third of your week." Delegated time may retain a larger unattributed tail (boundary agent sessions) without distorting the review.

### If the remainder is still large

Work the cause, in this order — and note that each step is a *diagnosis*, not a bulk write:

1. **Is the classifier running and healthy?** `tt status` reports the verdict; a missing API key
   leaves the classifier `Unconfigured` and nothing gets classified at all. Run `tt classify --auto`
   and see whether the count moves.
2. **Is the data even present?** `tt machines` flags remotes gone dark; an unsynced machine's
   events simply are not there. `tt sync <remote>` for each, then re-check.
3. **Is it work the classifier deliberately refused?** Focus events with no resolvable owner, and
   sessions whose content names no initiative, stay unassigned **on purpose**. That is the design:
   an unresolved event registers as classification lag rather than being swept into a container
   nobody authorized. Report the residual honestly; do not manufacture a home for it.

**Slack in particular is genuinely unattributable** — `Threads - … - Slack` names the workspace,
not the initiative, and tt cannot read thread content. Leave it unassigned rather than guessing.

### Containers that should not exist

If the week's report shows an hours-scale stream named after an activity (`nav`, `ops`,
`terminal`, `misc`, `comms`, `meetings`) or a date range (`… (Jun14-20)`), that is a container
minted before the name guard existed. Report it:

```bash
tt streams list --misnamed
```

Repairing one is an operator decision made deliberately, per stream — `tt streams rename` then
`tt streams merge` for a real initiative that was bucketed weekly, `tt streams dissolve` for a
container that never named real work. **Do not bulk-dissolve the list**: most date-suffixed
streams are real initiatives, and dissolving them would discard tens of thousands of correct
assignments. If repairing is out of scope for this review, say so in the review rather than
silently presenting the bad line as if it were a workstream.

### Fallback — genuine tool failure ONLY

"It's annoying / the remainder is junk / I'm short on time" is **not** a valid reason to skip. The only thing that can interrupt this gate is the tooling itself failing (`tt` unavailable, the classifier unconfigured or erroring) — and even then you do **NOT** auto-skip. **STOP, tell the user the tool failed and what it means (the review's time data would be unreliable), and ask for express permission to proceed without it.** Continue only if the user explicitly grants it; then run on manual estimates only, note it in the saved entry, and do not present per-project tt numbers as if they're sound. No automatic skipping, ever — a bypass requires the user's express okay.

## Phase 2: Trend Analysis

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

## Phase 3: Data Collection

**CRITICAL: Run the FULL ingestion pipeline. Partial data = wrong answer. Do NOT skip steps or present incomplete numbers.**

```bash
tt ingest sessions
```

Then sync ALL remote machines — remote events are often 50%+ of total data:

```bash
tt machines                    # List known remotes
tt sync <remote-label>         # For EACH remote machine listed
```

Then fetch the current week's report data — `tt report` is also where you read coverage, since
its `totals.unassigned_*_ms` is the authoritative unattributed figure:

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

**Supplementary detail** (optional): `tt streams list` shows each stream's direct/delegated
totals, tags, and description, and `tt proposals ls` shows classifier suggestions still awaiting
a human. Between them they give per-stream context without dumping raw sessions.

## Phase 4: Present Context

**GATE RE-CHECK (mandatory before presenting):** re-read the `tt report` totals —
`totals.unassigned_direct_ms` must still be small enough that no project's share is in
question, and no hours-scale catch-all stream (`nav`/`ops`/`comms`/`meetings`/`project:other`)
may appear. Present any real remainder as its own line rather than dropping it. If the
classifier is behind, STOP: return to Phase 1 and let it catch up first.

1. **Load ontology** from `~/.config/time-tracker/ontology.toml` — use `projects.names[]` and `activities.names[]` to organize the presentation.

2. **Show computed time allocations** from tt report data:

```
Time by Tag (from tt report):

Projects:
  time-tracker:  12.5h direct | 8.2h delegated  (35%)
  webapp:         5.0h direct | 15.0h delegated  (28%)
  cli-tool:       2.0h direct | 3.5h delegated   (8%)
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
    1. "Refactor data layer" (webapp, 4.2h)
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

## Phase 5: Free-Form Narration

**Let the user speak/type freely.** Track and map to form sections:

- **Time corrections**: "Actually spent more time on Ops than tt shows" → adjust activities
- **Goal tracking**: "I paired with a teammate on Tuesday" → mark checkbox
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
3. Only proceed to Phase 6 after explicit confirmation

## Phase 6: Structured Presentation

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
- webapp: 20-30% (tt: 28%)
...

### Direct vs Delegated Time
- Total direct: 21.0h
- Total delegated: 27.2h
- Delegation ratio: 56% delegated

## Goals
✓ Paired with an engineer
✓ Left comments on writeups
✗ Read team writeups
Notes: "Paired with a teammate on the auth bug"

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
Priorities: 1. Ship viewer  2. Onboard new hire  3. Plan Q1
On track (project): Yes
On track (personal): Unsure
```

2. **Highlight gaps**: "Missing: What's one thing you'll change next week?"

3. **Show discrepancies**: "Your perceived Meeting time (20-30%) vs tt actual (5%) — are you counting informal conversations that tt doesn't track?"

4. **Show actual vs perceived time** side by side. The gap between these is valuable for red-teaming.

5. **Ask for edits**: "Anything to adjust before we continue?"

## Phase 7: Red Team Challenge

**Launch the `red-teamer` agent** (using Task tool with `subagent_type: "red-teamer"`).

**Context to pass to the agent** (include all of this in the prompt):
1. **Current week data**: The complete structured form from Phase 6 (time allocations, reflections, ratings, priorities)
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

## Phase 8: Save

**FINAL GATE (mandatory before saving):** the entry you save becomes permanent history that
future reviews trend against. Confirm one last time that its time numbers come from a caught-up
classifier — a small `unassigned_direct_ms` and no catch-all buckets — and that any real
remainder is recorded in the entry rather than rounded away. Saving an entry built on a stale
classifier poisons every future trend comparison; if the gate fails here, go back to Phase 1,
fix it, re-pull, and re-present before saving.

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

## Phase 9: Post-Review Actions (optional)

Run any user-configured follow-up actions defined in `~/.config/time-tracker/weekly-review.toml` under `[post_review_actions]`. This is the user's customizable extension point for things they always want to do at the end of a review (1-on-1 prep, drafting standup notes, queuing emails, journaling prompts, etc.).

**This phase is OPTIONAL.** Skip it cleanly if there's no config or the user declines.

### Step 1: Check for config

Look for a `[post_review_actions]` section in `~/.config/time-tracker/weekly-review.toml`. The expected shape is a single freeform `instructions` field:

```toml
[post_review_actions]
instructions = """
<freeform markdown the agent should treat as instructions>
"""
```

If the section is missing, empty, or whitespace-only: **skip Phase 9 entirely.** Do not prompt the user. Do not ask them to configure it. Just end the review.

### Step 2: Confirm with the user

If `instructions` is present and non-empty, show a one-line confirmation:

> "Post-review actions configured. Run them now? (y / skip)"

If the user says skip, no, or anything other than affirmative: **end the review.** Do not run the instructions. Do not save the skip anywhere.

### Step 3: Run the instructions

Treat the `instructions` field as a freeform brief from the user about what to do after the review is saved. Read it carefully and execute as written.

**Context the agent has available** (the user can reference any of these in their instructions):
- The full JSONL entry just saved (last line of the JSONL file)
- Historical JSONL entries (last 8+ weeks of `reflection`, `ratings`, `red_team`, `next_week`)
- The `[goals]`, `[ratings]`, and `[prompts]` config
- The `ontology.toml` taxonomy
- tt data (run `tt report`, `tt streams list` etc. if needed)

**Behavioral guarantees**:
- Output is **ephemeral** (chat only). Do NOT save action outputs to JSONL. Do NOT modify the just-saved review entry.
- The freeform instructions are user intent. Follow them as faithfully as the rest of the skill, but do not invent additional follow-ups not requested.
- If instructions ask for confirmation/iteration with the user, do that.
- If instructions are unclear or contradict earlier review phases, ask the user to clarify rather than guessing.

### Step 4: Final summary

After completing the actions: "Post-review actions complete." Then end.

## JSONL Schema

Each line in `weekly-reviews.jsonl` is a JSON object. See the full example in `.opencode/skills/weekly-review/example.json`.

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
- **`delivered[]`**: Objective list of artifacts shipped this week. Separate from `reflection.successes` (which is subjective). Items have shape `{ id, title, url, kind }`. `kind` is one of `pr` | `doc` | `commit` | `decision` | `release`. Populate from PRs merged in the week window, decisions made, releases cut, etc. Future weeks should always include this field — empty array if nothing shipped.

## Fallback Handling

### Classification Failure (Phase 1)
Coverage is the data-integrity gate. If `tt` is unavailable or the classifier is failing, do **NOT** auto-skip and proceed — bypassing the gate requires the user's express permission.
1. **STOP.** Tell the user: "Stream classification FAILED — I won't bypass it without your okay, because the time data for this review would be unreliable. Here's what failed: …"
2. Ask for explicit permission to proceed without trustworthy time data.
3. Only if the user expressly grants it: continue on manual estimates, note in the saved entry that the time data is unreliable, and do not present per-project tt numbers as sound. Otherwise, pause the review until classification can run.

### tt Report Failure (Phase 3)
If `tt report` is unavailable or fails:
1. Inform the user: "tt report unavailable — we'll do manual time entry"
2. Skip Phase 3 data collection
3. In Phase 4, present empty time allocation tables
4. Ask user to estimate percentages directly: "How did you spend your time this week? Estimate percentages for your main activities."
5. In Phase 8, the `tt` field will be empty/null

### Chart Failure (Phase 2)
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
If `~/.config/time-tracker/weekly-review.toml` is missing:
- Use built-in defaults for reflection questions, goals, ratings
- Log a note: "No config found — using defaults"
If `~/.config/time-tracker/ontology.toml` is missing:
- Skip ontology-based organization in Phase 4
- Present raw tt report data without project/activity grouping

### Post-Review Actions Missing or Skipped (Phase 9)
If `[post_review_actions]` is absent or `instructions` is empty: skip Phase 9 silently — no prompt, no warning. Phase 9 is optional by design.
If user declines to run them at the confirmation prompt: end the review without running. Do not save the skip.

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
| Skipping Phase 1 (coverage) | **Always** verify coverage against `tt report` first, and run `tt classify --auto` if the classifier is behind. Attributed data makes the entire review more meaningful. |
| Skipping ingestion | **Always** run `tt ingest sessions` before `tt report`. Without it you miss most data. |
| Skipping remote sync | **Always** check `tt machines` and sync all remotes. Remote events are often 50%+ of total data. |
| Presenting partial results | Run the FULL pipeline (ingest → sync → infer streams → recompute → report). Don't stop partway and show incomplete numbers — it's worse than no answer. |
| Hand-authoring stream assignments | **Never.** No JSON blobs, no cwd patterns, no time ranges, no raw SQL. `tt classify --auto` is the only inference path; `tt streams assign` corrects one named session or event at a time. |
| Inventing a bucket for the remainder | A stream named after an activity (`nav`, `ops`, `meetings`) or a date range is not a workstream. Work the classifier refuses to place stays unassigned on purpose — report it. |
| Calling Toggl MCP tools | Use `tt report` exclusively. tt replaces Toggl for this workflow. |
| Hardcoding project/activity lists | Read from `~/.config/time-tracker/ontology.toml`. Never hardcode taxonomy. |
| Condensing user prose | Preserve full text. "Not prioritizing well" loses the detail that enables red-teaming. |
| Skipping phase confirmations | Always confirm before transitioning from narration (Phase 5) to structured (Phase 6). |
| Using `cargo run --` instead of `tt` | Use the `tt` binary directly. Build once if needed, then use the binary. |
| Missing `week.start`/`week.end` | Always populate these in the JSONL output (YYYY-MM-DD format). |
| Using `toggl` field in JSONL | Use the `tt` field. Schema uses time-tracker data, not Toggl. |
| Rushing through narration | Let the user talk. Brief acknowledgments only. Don't lead or suggest what they should say. |
| Including reflect-style analysis | Session pattern analysis is a separate skill. Weekly review focuses on time, goals, and reflection. |
| Reading time from anywhere but `tt report` | `tt streams list` totals are as of the last `tt recompute`, not period-scoped. Use `tt report` for time within a specific period. |
| Running Phase 9 instructions without user confirmation | **Always** ask "Run them now? (y / skip)" before executing the freeform instructions. The instructions are opt-in per-review. |
| Saving Phase 9 action output to JSONL | Phase 9 outputs are **ephemeral**. The just-saved review entry must remain untouched. If the user wants persistence, they update their instructions to write to a different file. |
| Treating absent `[post_review_actions]` as an error | It's optional. Skip silently. Don't prompt the user to configure it. |
| Inventing follow-ups beyond user instructions | Phase 9 only does what the freeform `instructions` say. Don't add suggestions, summaries, or actions not requested. |

## Goal Tracking Checkboxes

Read goal definitions from `~/.config/time-tracker/weekly-review.toml` `[goals]` section. Default goals if config is missing:

- `paired_engineer`: Paired with an engineer this week
- `read_writeups`: Read team members' writeups
- `left_comments`: Left comments on others' work
- `gave_feedback`: Gave constructive feedback
- `asked_help`: Asked for help when stuck
- `documented`: Documented decisions or learnings
- `mentored`: Mentored or helped onboard someone
- `shipped`: Shipped something to users

## Notes

- Uses `tt report` for time data — NOT Toggl
- Red team uses Task tool with `subagent_type: "red-teamer"`
- **Charts**: Use `uvx --with plotext python` for terminal bar charts (no install needed)
- **Taxonomy**: Read from shared `~/.config/time-tracker/ontology.toml`, not hardcoded
- **Direct vs delegated**: tt tracks human-focus time (direct) and agent-work time (delegated) separately. Both are valuable for the review.
- JSONL format allows easy appending and parsing
- **Config precedence**: `weekly-review.toml` settings > built-in defaults. Ontology from `ontology.toml` > no ontology.

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
| cli-tool | 4.2h | 3.8h | -0.4h |
| meetings | 2.0h | 2.1h | +0.1h |

### Allocation Parameters

tt's time calculation uses these defaults (configurable in `AllocationConfig`):
- **attention_window_ms**: 60000 (60 seconds) — events within this window are grouped as continuous focus
- **agent_timeout_ms**: 1800000 (30 minutes) — agent sessions idle longer than this are considered ended

These determine how tt interprets event patterns into focus time. Adjust if you find consistent over/under-reporting compared to Toggl.

### After Calibration

Once satisfied with accuracy, remove Toggl from Phase 3 data collection and rely solely on tt.

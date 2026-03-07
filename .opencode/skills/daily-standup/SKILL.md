---
name: daily-standup
description: Use when posting a daily standup, status update, or YTH report to Slack using time-tracker activity data
---

# Daily Standup

Generate and post a daily standup to Slack using time-tracker activity data.

## Arguments

- `channel`: Optional. Slack channel name (default: `#daily-standup`, channel ID `C09QA182QG2`)
- `date`: Optional. Day to report on (default: yesterday). Use natural language like "yesterday", "Feb 2", or ISO 8601.

Example: `/daily-standup #daily-standup yesterday`

## Workflow

```dot
digraph standup {
  rankdir=TB;
  node [shape=box];

  start [label="1. Parse date\n(convert to ISO 8601 UTC)" shape=ellipse];
  ingest [label="2. Run ingestion\ntt ingest sessions"];
  gather [label="3. Gather context\ntt classify --json"];
  analyze [label="4. Create streams\n(infer-streams skill)"];
  compute [label="5. Get computed time\ntt report --last-day" style=bold];
  prs [label="6. Look up PRs\n(gh search prs)"];
  draft [label="7. Draft update\n(logical projects, PR links)"];
  confirm [label="8. Confirm channel\nwith user"];
  post [label="9. Post to Slack"];

  start -> ingest -> gather -> analyze -> compute -> prs -> draft -> confirm -> post;
}
```

## Phase 1: Parse Date

Convert user's date to ISO 8601 UTC range.

**Critical**: `tt classify` accepts ISO 8601 format or relative strings (e.g., "2 days ago").

| User input | Start (UTC) | End (UTC) |
|------------|-------------|-----------|
| "yesterday" | Yesterday 08:00 UTC | Today 08:00 UTC |
| "Feb 2" | 2026-02-02T08:00:00Z | 2026-02-03T08:00:00Z |
| "today" | Today 08:00 UTC | Now |

Use `date` command to compute if needed:
```bash
# Yesterday's range (Pacific = UTC-8)
START=$(date -u -d "yesterday 00:00 PST" +%Y-%m-%dT%H:%M:%SZ)
END=$(date -u -d "today 00:00 PST" +%Y-%m-%dT%H:%M:%SZ)
```

## Phase 2: Run Ingestion + Sync Remotes

**CRITICAL: Always run the full ingestion pipeline. Partial data = wrong answer. Do NOT skip steps or present partial results.**

```bash
cargo build 2>/dev/null && cargo run -- ingest sessions
```

Then sync ALL remote machines — remote events are often 50%+ of total data:

```bash
tt machines                    # List known remotes
tt sync <remote-label>         # For EACH remote machine listed
```

This scans Claude Code and OpenCode session files, syncs remote events, and stores everything in the database. Without it, `tt classify` returns incomplete data.

## Phase 3: Gather Context

```bash
tt classify --json --start "$START" --end "$END"
```

This outputs JSON with:
- `sessions[]`: Agent sessions with `session_id`, `project_name`, `summary`, `tool_call_count`, `start_time`, `end_time`
- `event_clusters[]`: Non-session activity grouped by CWD + time

## Phase 4: Create Streams

**REQUIRED: Invoke the `infer-streams` skill** and follow its full workflow. Do NOT launch a subagent — use the Skill tool:

```
Skill("infer-streams")
```

You MUST execute the infer-streams workflow end-to-end:
1. Group sessions into streams (project + task name)
2. Build the `tt classify --apply` JSON with `assign_by_session` entries
3. Run `tt classify --apply assignments.json` — this persists streams AND runs `tt recompute`

**Do NOT skip this step.** Even for a daily standup, you must create and apply stream assignments. The allocation algorithm is the only source of truth for time — it accounts for attention windows, AFK detection, overlapping sessions, and agent timeouts that are invisible in raw session metadata.

## Phase 5: Get Computed Time

**MANDATORY GATE: No time numbers without this step.**

```bash
tt report --last-day --json    # For yesterday
tt report --day --json         # For today
```

This is the ONLY acceptable source of time data for the standup. The JSON output contains per-tag breakdowns with exact `time_direct_ms` and `time_delegated_ms` values.

**DO NOT estimate, guess, or mentally compute times from session metadata.** Fields like `duration_minutes` and `tool_call_count` do not account for the allocation algorithm's attention windows, AFK gaps, or overlapping parallel sessions. They will be wrong — typically by 2x or more.

Convert the millisecond values to hours/minutes for the report. Use these exact numbers.

## Phase 6: Look Up PRs

**CRITICAL: Always look up open and recently merged PRs to link in the report.** A standup without PR links is incomplete — readers want to see the artifacts.

```bash
# Open PRs
gh search prs --author=@me --state=open --limit=20 --json url,title,repository,state

# Recently merged PRs
gh search prs --author=@me --merged --sort=updated --limit=20
```

Match PRs to streams by repo name and title. Every accomplishment that produced or advanced a PR should link to it using markdown: `[PR #123](url)`.

## Phase 7: Draft Update

**Format: YTH (Yesterday, Today, Hopes/Blockers)**

Write for readers who have no context about your projects.

### Organize by Logical Project, Not Repository

**CRITICAL: Do NOT report streams grouped by repo directory.** The infer-streams output groups by repo path (e.g., `dotfiles`, `sami-home`, `eval-pipeline`). You MUST reorganize into logical projects that make sense to the reader.

**Common reorganizations:**
- Dotfiles/config repos are never a project — classify by what the work was actually for (e.g., install script for tool X → tool X project; PR reviews for repo Y → repo Y)
- Home directory sessions are usually meta-work — attribute to the actual project or group as "tooling"
- Multiple repos serving one goal should be combined (e.g., a library repo + the app that depends on it → one section)
- Infra debugging (CPU, disk, network) should be attributed to the project it was blocking

**Ask the user** if you're unsure how to group — they know what the logical projects are. Present your best guess and let them correct it.

### Include PR Links

Every accomplishment that produced or advanced a PR should include a markdown link: `[PR #123](url)`. Use the PRs gathered in Phase 6.

### Template

```markdown
## Standup - {Day of Week} {Date}

### Yesterday ({Date})

- **{Logical Project}** — {direct}h direct | {delegated}h delegated
  - {Accomplishment} — [PR #N](url) (merged/open)
  - {Another accomplishment}

- **{Logical Project}** — {direct} direct | {delegated} delegated
  - {Accomplishment} — [PR #N](url)

**Totals:** {X}h direct | {Y}h delegated

{Optional: Brief note about slowdowns or context}

### Today

- {Plan item 1}
- {Plan item 2}

### Blockers

- {Blocker, or "None" if clear}
```

**Writing guidelines:**
- Project names should be recognizable (use repo names or common abbreviations)
- Accomplishments should be specific enough that someone unfamiliar can understand impact
- Avoid jargon like "refactoring" without saying what/why
- Time numbers come from `tt report` — copy them exactly, do not round or estimate
- Delegated time can be very high with parallel agents (10+ hours is normal for heavy days)

## Phase 8: Ask for "Today" Plans

The time-tracker data only shows past activity. Ask the user:
- "What are your plans for today?"
- "Any blockers?"

If user provided plans in the invocation, use those. Otherwise, infer reasonable continuations from yesterday's work (e.g., "continue X" or "open PR for Y").

## Phase 9: Confirm Before Posting

Show the drafted message and confirm:
- Target channel is correct
- Content looks accurate

Use AskUserQuestion if channel wasn't provided or to confirm before posting.

## Phase 10: Post to Slack

Use `mcp__slack__conversations_add_message`:
- `channel_id`: Use `#channel-name` format
- `content_type`: `text/markdown`
- `payload`: The drafted message

**Slack markdown note:** Slack's markdown support is limited. Nested bullets may appear flattened in some clients. Format for readability anyway—the structure helps even if rendering is imperfect.

```markdown
- **Project** — time
  - Sub-item (2 spaces before -)
  - Another sub-item
```

If posting fails:
1. Check channel name/ID
2. Try without special formatting
3. Report error to user

## Common Mistakes

| Mistake | Fix |
|---------|-----|
| Skipping ingestion | **Always** run `tt ingest sessions` before `tt classify`. Without it you miss most data. |
| Skipping remote sync | **Always** check `tt machines` and sync all remotes. Remote events are often 50%+ of total data. |
| Reporting partial/incomplete numbers | Run the FULL pipeline (ingest → sync → classify → assign → report) before showing any report. Don't stop partway and present incomplete numbers. |
| Guessing time from session metadata | **Never.** `duration_minutes` and `tool_call_count` don't account for attention windows, AFK, or overlapping sessions. Always use `tt report` output. Guesses are typically off by 2x+. |
| Skipping stream creation | You MUST create streams via `tt classify --apply` before running `tt report`. Without streams, the report has no data. |
| Reporting by repo directory | Organize by logical project, not filesystem path. `dotfiles` is never a project. Ask the user. |
| No PR links | Always run `gh search prs` and link every PR mentioned in the report. |
| Date format error | Use ISO 8601: `2026-02-02T08:00:00Z` or relative like "1 day ago" |
| Wrong year | Check current date first—don't assume from context |
| Updates too terse | Include context for outsiders ("what" and "why") |
| Launching subagent for analysis | Use `Skill("infer-streams")`, NOT the Task tool |
| Missing sessions | Check `starting_prompt` when `summary` is null |
| No "Today" plans | Ask user—can't infer future plans from past data |

## Example Output

```markdown
## Standup - Mon Feb 3, 2026

### Yesterday (Feb 2)

- **Legion** — 2h direct | 10h delegated
  - Completed daemon worker monitoring implementation (workers now report health status)
  - Set up tmux-based session architecture for parallel agent execution

- **eval-pipeline** — 15min direct | 30min delegated
  - Started validation work for new test harness (in progress)

- **time-tracker** — 1h direct | 5h delegated
  - Fixed report time calculation to use period events, not cumulative totals

**Totals:** ~3h direct | ~15h delegated (parallel agent sessions)

### Today

- Finish eval-pipeline validation and open PR
- Debug Legion worker communication issues

### Blockers

- None currently
```

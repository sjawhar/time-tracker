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
| **Project** | A logical, time-bound work *initiative*, inferred from session CONTENT — **not** a repo/cwd (see Classification Discipline + ontology rule #1). | folder `monorepo/teammate-x` → the *work* done there, e.g. `webapp: reviewing a teammate's PRs` |
| **Stream** | A specific task/feature/PR within a project. Spans hours to 2-3 days. | "webapp: engine refactor", "cli-tool: WRK-125" |

**BAD names:** "webapp work", "cli-tool development" (too coarse)
**GOOD names:** "webapp: pipeline API redesign", "cli-tool: controller-worker separation"

## Classification Discipline (READ FIRST — overrides any "project = repo" guidance below)

The shared ontology (`~/.config/time-tracker/ontology.toml`) is the source of truth and **overrides this skill** wherever they conflict. Load it every run — it defines the valid project + activity vocabulary; use only tags from it. Its rules, restated because they get violated constantly:

1. **Project = a unit of WORK, inferred from CONTENT — never the directory/cwd/repo.** A monorepo (or any repo) can host many projects at once, and one project can span repos. A folder like `monorepo/teammate-x` is not a workstream — the workstream is *what was done there* (e.g. reviewing a teammate's submissions). Infer the project + stream from session `summary`, `starting_prompt`, PR/issue/ticket/work-order refs, and window titles. cwd is at most a weak tiebreaker; by itself it is never a valid project or stream name.

2. **window_focus / browser / Slack events carry their context in `window_title` + `window_app_id` — that IS their cwd-equivalent.** Classify them by what the title says (which doc, which channel, which site), exactly like you classify a tmux event by its cwd. NEVER assign a *titled* window event by time-proximity, and NEVER dump window activity into a catch-all. Only a blank / `New Tab` / sign-in flicker with no usable title may fall through.

3. **Project (WHAT) and activity (HOW) are different axes — do not confuse them.** Activity types (e.g. `meetings`, `messages`, `ops`, `admin`, `planning`) describe the *nature* of work; they are **NOT projects or workstreams**. A planning meeting about project X is `project:X` + `activity:meetings`; a message thread about a contract is the relevant project + `activity:messages` — never a workstream called "meetings" or "messages". Overhead that genuinely serves no single project (a general standup, a cross-channel message scroll) → `project:other`/`misc` + its activity tag, still never a project named after the activity.

4. **Overhead is work, not `personal`.** Enterprise/compliance work (MFA, device management, credential policy, security forms) is its own project per the ontology (rule #2). `personal` is **life only** (media, food, personal messages, errands). Lumping meetings, comms, security, or contract work into `personal` is a bug.

5. **Every stream gets a project tag AND a primary `activity:` tag**, and is named by the work done (`webapp: pipeline API redesign`) — never by a folder or by an activity type.

6. **No lazy catch-all `ELSE`.** If one bucket sweeps up many distinct kinds of work, you have not classified it — split it by content.

## Arguments

Optional: Time range (default: since last stream ended, or "7 days ago")

Example: `/infer-streams 3 days ago`

## Phase 1: Ingest + Sync + Determine Range

**CRITICAL: Run the FULL ingestion pipeline. Partial data = wrong answer.**

```bash
tt ingest sessions
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

**Coverage check — enumerate EVERY active cwd, not just the obvious ones.** `tt sync`
auto-assigns events by cwd only for cwds already linked to a stream; brand-new cwds stay
`stream_id = NULL` and silently vanish from `tt report`. List them all so none are missed:

```bash
DB=~/.local/share/time-tracker/tt.db
sqlite3 "$DB" "SELECT COALESCE(stream_id,'NULL') AS sid, cwd, COUNT(*) AS n
  FROM events WHERE timestamp >= '{start}' AND timestamp < '{end}'
  GROUP BY sid, cwd ORDER BY n DESC;"
```

Every cwd with `sid = NULL` and non-trivial `n` MUST be assigned a stream in Phase 4
(via `cwd_like` rules or per-session assignment). Do not finish with NULL cwds remaining.

## Phase 3: Identify Streams

Group sessions into streams by **what was worked on** — content first, cwd last:

1. **`summary`** — what was actually done (the primary signal)
2. **`starting_prompt`** — intent
3. **PR / issue / Taiga / work-order refs** in the content → identify the project
4. **`window_title`** (browser/Slack/window_focus) — the doc/channel/site names the work
5. **Temporal + semantic clustering** — related sessions within >2h gaps = ONE stream
6. **`cwd`** — a *weak tiebreaker only*, NEVER the basis for a project or stream name (ontology rule #1): a monorepo hosts many projects; a folder like `monorepo/teammate-x` is not a workstream.

Sessions may carry `"linked_todo": {"id", "text", "stream_slug"}`. Sessions with the same
`linked_todo.id` belong to the same stream; use the todo text when naming the stream and choosing
its stable slug.

Present the proposed streams to the user for review before persisting.

## Phase 4: Create Streams + Assign Events

Build a JSON file matching the `tt classify --apply` format:

```json
{
  "streams": [
    {
      "name": "project: stream name",
      "slug": "project-stream-name",
      "tags": ["project:project-name"]
    }
  ],
  "assign_by_session": [
    {"session_id": "ses_abc", "stream": "project-stream-name"},
    {"session_id": "ses_def", "stream": "project-stream-name"}
  ],
  "assign_by_pattern": [
    {
      "cwd_like": "%/project-name/%",
      "start": "2026-02-26T08:00:00Z",
      "end": "2026-02-27T08:00:00Z",
      "stream": "project-stream-name"
    }
  ]
}
```

- Every `streams` entry requires a stable, lowercase kebab-case `slug` of at most 32 characters. Pick a short, memorable identity such as `watcher-rewrite`.
- Set every `assign_by_*` `stream` value to that slug. Assignment refs resolve slugs first, then display names; unknown refs error rather than creating a stream.
- Use `assign_by_session` for agent session events (all events for that session move together)
- Use `assign_by_pattern` for non-session events (`tmux_pane_focus`, AFK) by cwd + time range. For **`window_focus`** events (browser/Slack — no cwd) classify by **`window_title` content** with title-match rules (which doc/channel/site), NOT by cwd, proximity, or a catch-all — the title is their context (Classification Discipline #2).

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
| webapp | engine refactor | 2h 15m | 72.9h |
| cli-tool | auth-plugin | 43m | 38.3h |
| **TOTAL** | | **X hrs** | **Y hrs** |

### Stream Details
- **webapp: engine refactor** — Sessions: ses_abc, ses_def. Engine/scheduler cleanup.

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
| Tagging project by cwd/repo | Project = a work *initiative* inferred from CONTENT (ontology rule #1). A folder is not a project — never tag by directory. |
| Naming a stream after a folder or an activity | Name streams by the work done (`webapp: pipeline API redesign`), not a directory (`repo/subdir`) or an activity type (`meetings`). |
| Streams too coarse | "webapp work" → "webapp: pipeline API redesign". |
| Leaving events unassigned | Everything gets assigned. Use "misc: {activity}" for unclear. |
| Assigning window/browser/Slack events by cwd, proximity, or a catch-all | They carry `window_title` — classify by it, exactly like any other event. Only a blank/New-Tab/sign-in flicker may fall through. |
| Lumping meetings / comms / infosec / contracts into `personal` | Overhead is work: meetings → `activity:meetings`, Slack/email → `activity:messages`, MDM/MFA/security-forms → `infosec`, contracts/billing → `activity:admin`. `personal` = life only. |
| A stream with only a project tag | Every stream needs a project tag AND a primary `activity:` tag. |
| Stopping after classify --apply | `tt classify --apply` runs recompute automatically. No separate step needed. |

## Done When

1. All events assigned to streams (check `tt classify --unclassified`)
2. `tt streams list` shows direct/delegated time per stream
3. Report presented to user
4. `(unassigned)` bucket in `tt report` is near-zero (no meaningful NULL-stream cwds remain)

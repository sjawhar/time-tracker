# Session-Centric Classification Layer

## Problem

tt collects good raw data (events from tmux hooks, agent sessions from Claude/OpenCode)
but the pipeline from raw data to useful time reports is broken:

1. **Stream assignment requires raw SQL.** LLM skills write Python scripts to UPDATE events
   directly in SQLite. There's no CLI workflow for classification.
2. **Sessions get split across streams.** Assigning events by time window puts `agent_session
   (started)` in one stream and `agent_tool_use` events in another. The allocation algorithm
   needs all events for a session in the same stream.
3. **Auto-assignment creates garbage.** CWD-based auto-assignment during `ingest sessions`
   creates catch-all streams when multiple worktrees share a project prefix.
4. **Delegated time is undercounted.** The allocation algorithm uses a 30-minute timeout
   heuristic for session end detection, even when the actual session end_time is known.
5. **Classification is not persistent.** Each consumer (standup, weekly review) must
   re-run the full inference pipeline.

## Core Principle

**Classify once, query many times.** The LLM classifies events into streams (with tags).
tt propagates assignments, handles orphan events, and computes time. Any consumer
(daily standup, weekly review, monthly prioritization) queries the result.

## Data Model

No changes to the core data model. Events remain the atomic unit. `event.stream_id`
is the source of truth for time allocation. Streams have tags for arbitrary grouping
(e.g., `project:agent-c`, `priority:high`).

Sessions are a useful signal for the LLM — they have summaries, prompts, project paths.
But the classification interface is event-centric with session-level shortcuts.

## Design

### 1. `tt classify` command

Two modes:

#### `tt classify --start <range> [--end <range>]`

Show what needs classification. Output includes:

**Sessions:** One row per session — session_id, source, project_path, start_time,
end_time, duration, summary/starting_prompt (truncated), tool_call_count,
user_prompt_count, proposed_stream (from prior classifications), proposed_tags.

**Non-session events:** User activity (tmux_pane_focus, AFK, scroll) grouped by
CWD + time clusters. Shows CWD, time range, event count, proposed_stream.

Default output is human-readable. `--json` for programmatic use by LLM skills.

The `--unclassified` flag restricts output to events without a stream_id —
"what's new since last classification?"

The `--summary` flag provides a compact view: one line per session, one line per
activity cluster. Enough for the LLM to propose classifications without reading
every event. Full detail available via the default mode.

#### `tt classify --apply <file_or_stdin>`

Accept assignments from the LLM and execute them. Input format:

```json
{
  "streams": [
    {
      "name": "agent-c: issue 1456 error counting",
      "tags": ["project:agent-c"]
    }
  ],
  "assign_by_session": [
    {
      "session_id": "ses_abc",
      "stream": "agent-c: issue 1456 error counting"
    }
  ],
  "assign_by_pattern": [
    {
      "cwd_like": "%/agent-c/viewer%",
      "start": "2026-02-26T08:00:00Z",
      "end": "2026-02-27T08:00:00Z",
      "stream": "agent-c: viewer perf investigation"
    }
  ]
}
```

Three assignment types:

- **By session:** All events with matching session_id get the stream_id.
  Covers agent events (the 80% case). No split possible.
- **By pattern:** All events matching CWD pattern + time range get the
  stream_id. Covers non-session events (tmux focus, AFK, etc.).
- **By event ID:** (future, if needed) Individual event assignment.

When applied:
1. Create streams that don't exist yet
2. Apply tags to streams
3. Execute session assignments (UPDATE events SET stream_id WHERE session_id = ?)
4. Execute pattern assignments (UPDATE events SET stream_id WHERE cwd LIKE ? AND timestamp BETWEEN ? AND ?)
5. Run `recompute` on affected streams

All assignments set `assignment_source = 'inferred'`. User corrections
(`assignment_source = 'user'`) are never overwritten.

### 2. `tt context` improvements

Current output dumps a flat JSON blob. Changes:

- **Session-grouped output.** Events grouped under their parent session, with
  session metadata as headers. Non-session events in a separate "user activity"
  section grouped by CWD + time clusters.
- **`--unclassified` flag.** Only events/sessions without stream_id. This is
  what the LLM needs for incremental classification.
- **`--summary` flag.** Compact mode for initial classification proposals.

### 3. Conservative auto-assignment during ingestion

Current `ingest sessions` auto-assigns by CWD matching and creates catch-all
streams. Changes:

- **Only auto-assign with high confidence.** A new session from the same
  project_path as a previously classified session gets the same stream.
  Ambiguous cases are left unassigned for the LLM.
- **Never auto-create streams.** Streams are created by `tt streams create`
  or by `tt classify --apply`. Auto-assignment only assigns to existing streams.
- **Persist classification history.** When `classify --apply` assigns
  session X (from project_path P) to stream Y, remember that mapping.
  Future sessions from the same project_path get proposed (not auto-assigned)
  with that stream.

### 4. Allocation algorithm fixes

#### 4a. Use session end_time instead of timeout heuristic

The `agent_sessions` table has the actual `end_time` for sessions that
completed normally. The allocation algorithm currently ignores this and
relies solely on the 30-minute timeout after last tool_use.

Change: When processing an `agent_session(started)` event, look up the
session's `end_time` from the agent_sessions table. If known, use it
as the session end. The timeout heuristic only applies to sessions with
no end_time (truly crashed/killed sessions).

This fixes the delegated time undercount for sessions with gaps between
tool calls (e.g., parent sessions waiting for subagents).

#### 4b. Split-session validation

Add a validation check during `recompute`: if any session has events in
multiple streams, emit a warning. This catches data integrity issues
regardless of how assignment happened.

### 5. What doesn't change

- **Events as atomic unit.** `event.stream_id` is the source of truth.
- **Allocation algorithm core logic.** Direct time exclusive, delegated
  time additive, AFK pauses direct only.
- **Tags.** Flat, user-defined. Used for project grouping (`project:X`)
  and arbitrary categorization.
- **Sync/export/import.** Multi-machine data flow unchanged.
- **Skills layer.** daily-standup, infer-streams, weekly-review still
  orchestrate but call `tt classify` instead of raw SQL.

## Summary of Changes

| Change | What | Why |
|--------|------|-----|
| `tt classify` | New command: show unclassified data, apply assignments | Replaces raw SQL in LLM skills |
| `tt context` improvements | `--unclassified`, `--summary`, session-grouped | LLM gets digestible input |
| Conservative auto-assignment | Don't auto-create streams, don't assign when ambiguous | Eliminates garbage catch-all streams |
| Session end_time in allocation | Use known end_time instead of timeout heuristic | Fixes delegated time undercount |
| Split-session validation | Warn during recompute if session events span streams | Catches data integrity issues |

## Success Criteria

1. The daily-standup skill can run without any raw SQL or Python scripts
2. `tt classify --apply` never produces split sessions
3. Delegated time for a session with known end_time matches session duration
   (minus time before first tool_use)
4. Re-running `ingest sessions` on already-classified data doesn't create
   garbage streams or overwrite existing classifications
5. A weekly review can query the same classified data without re-running inference

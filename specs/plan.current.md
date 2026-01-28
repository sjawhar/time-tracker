# Plan: Finalize CLI Commands Spec

**Task**: Update `design/ux-cli.md` with actual commands

## Research Summary

### What's Currently Implemented

**Remote (Rust binary - `tt-cli`):**
- `tt ingest <type> --pane --cwd --session [--window]` — Receive events from tmux hooks
- `tt export` — Output all events as JSONL to stdout
- `tt status` — Show current tracking status (stub)

**Local (Python - `tt-local`):**
- `tt import` — Import events from stdin (JSONL format)
- `tt sync <remote>` — SSH + `tt export` + `tt import` in one command
- `tt events [--since] [--type] [--limit]` — Query events from local database
- `tt status` — Show last event time per source

### Competitor CLI Patterns

**Toggl CLI** ([toggl-cli](https://github.com/AuHau/toggl-cli)):
- `toggl start` / `stop` / `now` — Timer control
- `toggl ls` — List recent entries
- `toggl sum` — Show summary by days
- `toggl continue` — Resume last entry
- Entity management: `projects ls`, `clients ls`, `tags ls`

**ActivityWatch** ([aw-client](https://docs.activitywatch.net/en/latest/cli.html)):
- `aw-client buckets` — List all buckets
- `aw-client events <bucket>` — Query events
- `aw-client heartbeat <bucket> <data>` — Send heartbeat
- `aw-client report` — Generate activity report

**WakaTime** ([wakatime-cli](https://github.com/wakatime/wakatime-cli)):
- Primarily library-style CLI for plugins
- Focuses on `--heartbeat` for passive collection
- Auto-detection of project, language, branch

**Key Insight**: Traditional time trackers use start/stop timers. WakaTime and ActivityWatch are event-based (closer to our model). Our CLI should reflect the observe-first, infer-streams philosophy.

### CLI Design Best Practices

From [Command Line Interface Guidelines](https://clig.dev/):
1. **Human-first design**: Prefer named flags over positional arguments
2. **Consistency**: Follow established patterns (`--help`, `--json`, `--verbose`)
3. **Progress feedback**: Show status for operations >100ms
4. **Layered help**: Examples first, extensive docs via `--help`
5. **Machine-friendly output**: Support `--json` for scripting

## Recommended Approach

### Design Principles for `tt`

1. **Observe, don't interrogate** — No start/stop timers. Events are collected passively.
2. **Streams, not tasks** — The primary unit is the stream, not individual time entries.
3. **Minimal manual intervention** — Commands for correction exist but shouldn't be needed often.
4. **Fast startup** — Remote `tt` must start in <50ms (it's called on every pane focus).
5. **Progressive disclosure** — Simple commands for common tasks, flags for power users.

### Command Categories

| Category | Purpose | MVP? |
|----------|---------|------|
| Collection | Passive event capture | ✓ (done) |
| Sync | Remote → local data transfer | ✓ (done) |
| Query | Inspect raw events | ✓ (done) |
| Status | Overview of current state | ✓ |
| Reporting | Time breakdowns by stream/tag | ✓ |
| Streams | List/manage inferred streams | ✓ |
| Tagging | Apply/edit tags on streams | ✓ |
| Debug | Troubleshooting | ✓ |

### Proposed MVP Commands

```
# Status (what's happening now?)
tt status                    # Overview: last sync, active streams, today's direct time

# Reporting (where did time go?)
tt report --week             # Weekly breakdown by tag
tt report --day [DATE]       # Daily breakdown
tt streams [--today|--week]  # List streams with time

# Stream management
tt streams                   # List recent streams
tt tag <stream> <tag>        # Add tag to stream
tt untag <stream> <tag>      # Remove tag from stream

# Sync (already implemented)
tt sync <remote>             # Pull events from remote

# Query/debug (already implemented)
tt events [--since] [--type] # Dump raw events

# Help
tt --help                    # Overall help
tt <command> --help          # Command-specific help
```

### Commands NOT in MVP

- `tt start` / `tt stop` — Conflicts with observe-first philosophy
- `tt note` — Manual annotations (post-MVP)
- `tt contexts` — Contexts are a post-MVP abstraction
- `tt agent-time` / `tt agent-cost` — Requires LLM cost tracking (post-MVP)
- `tt config` / `tt rules` — Configuration via file for MVP, no CLI editing
- Real-time `--watch` modes — Post-MVP

## Spec Outline

The updated `ux-cli.md` will cover:

1. **Design Principles** — Human-first, observe-first, streams not timers
2. **Command Reference** — Each command with:
   - Synopsis
   - Description
   - Options
   - Examples
3. **Output Formats** — Human-readable default, `--json` for scripting
4. **Error Handling** — How errors are displayed, exit codes
5. **Configuration** — How CLI finds config/database

## Open Questions (Resolved)

| Question | Resolution |
|----------|------------|
| Should remote and local have different CLIs? | Yes. Rust binary for remote (fast), Python for local (rich features). Same `tt` name, context-dependent behavior. |
| How to identify streams for tagging? | By stream ID (short hash prefix) or fuzzy match on description |
| What's the primary report format? | Plain text with optional `--json`. Fancy formatting (bars, colors) is nice-to-have. |

## Review Feedback

### Architecture Review

**Remote/Local Detection**: The plan needs to clarify how `tt` resolves which mode to use. Simplest approach: separate installations per environment (whichever binary is installed). The spec should document that remote machines have the Rust binary, local machines have Python.

**Stream Inference Timing**: Document that inference runs lazily during `tt report`/`tt streams` queries. No separate `tt infer` command needed for MVP.

**Missing `tt stream <id>`**: Consider adding a command to inspect single stream details (its events, time breakdown, tags). Useful for debugging inference issues. **Decision**: Defer to post-MVP; users can use `tt events --since ... --type ...` as a workaround.

**Report Dimensions**: Spec must clarify that reports show:
- Direct time and delegated time
- Breakdown by tag (with "untagged" as fallback)
- Consider `--by-stream` flag for stream-level detail

**Tagging UX**: Define how users identify streams. Recommendation: show short IDs (like git's `abc1234`) in `tt streams` output. Users tag by ID prefix.

### UX Review

**Streams vs Report Naming**: Clarify distinction:
- `tt report` = aggregated time summaries (totals by tag)
- `tt streams` = list of individual work sessions with times

**Stream ID Workflow**: Add concrete examples showing full tagging workflow in the spec. Show `tt streams` output, then `tt tag <id> <tag>`.

**Status Command**: Define what "active" means in an observe-first system. "Active" = had recent events. Example output needed:
```
$ tt status
Collection: healthy (last event 3s ago)
Today: 4h 23m across 3 streams

Active now:
  auth-service     2h 15m  (last: 3s ago)

Background:
  dashboard        1h 30m  (last: 45m ago)
```

**Remote/Local Transparency**: Output should indicate mode. Example: `[remote mode - no local database]`

**Timeline View (US-004)**: `tt streams --today` serves as the timeline. For editing, `tt tag`/`tt untag` handles metadata changes. Full `tt edit <stream>` is post-MVP.

**Tag Discoverability**: Add `tt tags` command to list all tags with stream counts. Low effort, high value.

**Error Message Patterns**: Include actionable error examples in spec:
```
$ tt sync remote1
Error: Cannot reach remote1 (connection refused)
Try: ssh remote1 'tt export' to test connectivity
```

## Updated Proposed Commands

```
# Status (what's happening now?)
tt status                    # Overview: collection health, active streams, today's time

# Reporting (aggregated summaries)
tt report --week             # Weekly breakdown by tag (direct + delegated time)
tt report --day [DATE]       # Daily breakdown

# Stream management (individual sessions)
tt streams [--today|--week]  # List streams with times and IDs
tt tag <stream-id> <tag>     # Add tag to stream (by ID prefix)
tt untag <stream-id> <tag>   # Remove tag from stream
tt tags                      # List all tags with counts

# Sync (already implemented)
tt sync <remote>             # Pull events from remote

# Query/debug (already implemented)
tt events [--since] [--type] # Dump raw events

# Help
tt --help                    # Overall help
tt <command> --help          # Command-specific help
```

## Next Steps

After this plan is approved:
1. Update `specs/design/ux-cli.md` with the command reference
2. Document each command with synopsis, description, options, example output
3. Define output formats (human-readable + `--json`)
4. Define error handling conventions with actionable messages
5. Document remote vs local mode with example outputs for each

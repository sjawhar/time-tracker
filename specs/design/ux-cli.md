# UX: Command-Line Interface

This spec defines the CLI commands for Time Tracker (`tt`).

## Design Principles

1. **Observe, don't interrogate** — No start/stop timers. Events are collected passively.
2. **Streams, not tasks** — The primary unit is the stream (a coherent work session), not individual time entries.
3. **Minimal manual intervention** — Commands for correction exist but shouldn't be needed often.
4. **Fast startup** — Remote `tt` must start in <50ms (it's called on every pane focus).
5. **Progressive disclosure** — Simple commands for common tasks, flags for power users.
6. **Human-first** — Readable output by default, `--json` for scripting.

## Architecture: Remote vs Local

The `tt` command exists in two forms:

| Environment | Implementation | Purpose |
|-------------|----------------|---------|
| Remote (dev server) | Rust binary | Fast event capture, export |
| Local (laptop) | Python CLI | Rich queries, reporting, analysis |

The same `tt` command name is used in both environments. Which one runs depends on what's installed. Users don't need to think about this—they run `tt` and it does the right thing for where they are.

**Remote mode indicators**: Output includes `[remote mode]` when relevant to help users understand context.

**Mode detection**: Commands that only work in one mode fail fast with guidance:
```bash
$ tt report --week    # on remote machine
Error: 'tt report' is not available in remote mode.
Sync events to your local machine first:
  tt sync <remote-hostname>
```

---

## Command Reference

### Collection Commands (Remote)

#### `tt ingest`

Receive events from tmux hooks. Called automatically—users rarely invoke this directly.

```
tt ingest <TYPE> --pane <ID> --cwd <PATH> --session <NAME> [--window <INDEX>]
```

**Arguments:**
- `TYPE` — Event type (e.g., `pane-focus`)
- `--pane` — tmux pane ID (e.g., `%3`)
- `--cwd` — Current working directory
- `--session` — tmux session name
- `--window` — Window index (default: 0)

**Example:**
```bash
# Called by tmux hook (not manually)
tt ingest pane-focus --pane %3 --cwd /home/user/project --session dev
```

#### `tt export`

Output all events as JSONL to stdout. Used by `tt sync` on the local machine.

```
tt export
```

**Output:** JSONL stream of all events, sorted by timestamp.

**Example:**
```bash
$ tt export | head -1
{"id":"abc123","timestamp":"2025-01-28T10:00:00Z","type":"tmux_pane_focus",...}
```

---

### Sync Commands (Local)

#### `tt sync`

Pull events from a remote machine via SSH.

```
tt sync <REMOTE> [--timeout <SECONDS>] [--dry-run]
```

**Arguments:**
- `REMOTE` — SSH host (e.g., `devserver`, `user@host`)
- `--timeout` — SSH timeout in seconds (default: 60)
- `--dry-run` — Show what would be imported without modifying the database

**Deduplication:** Events are identified by content-based IDs (hash of source + type + timestamp + data). Importing the same event twice is a no-op—the database uses `INSERT OR IGNORE`.

**Example:**
```bash
$ tt sync devserver
Synced from devserver: 42 new events (150 received, 108 already existed)
```

**Dry-run example:**
```bash
$ tt sync devserver --dry-run
Would import from devserver: 42 new events (150 received, 108 already exist)
```

**Error handling:**
```bash
$ tt sync unreachable
Error: Cannot reach unreachable (connection refused)
Try: ssh unreachable 'tt export' to test connectivity
```

#### `tt import`

Import events from stdin. Rarely used directly—`tt sync` handles this.

```
tt import [--db <PATH>]
```

**Example:**
```bash
$ cat events.jsonl | tt import
Imported 42 events
```

---

### Status Commands

#### `tt status`

Show overview of time tracking state.

```
tt status
```

**Local output:**
```
Database: ~/.local/share/tt/events.db
Collection: healthy (last event 3s ago)

Today: 4h 23m across 3 streams

Active now:
  auth-service     2h 15m  (last: 3s ago)

Background:
  dashboard        1h 30m  (last: 45m ago)
  docs-update      37m     (last: 2h ago)

Last sync: 5m ago from devserver
```

**Remote output:**
```
[remote mode - no local database]

Events buffer: 847 events (142 KB)
Last event: 3s ago

Sources:
  remote.tmux:  847 events (last: 3s ago)
  remote.agent: 1,234 events (last: 15m ago)
```

**"Active" definition:** A stream is "active" if it had an event within the last 10 minutes.

---

### Reporting Commands

#### `tt report`

Show aggregated time summaries by tag.

```
tt report [--day [DATE] | --week] [--json]
```

**Options:**
- `--day [DATE]` — Daily report (default: today). DATE format: `YYYY-MM-DD`
- `--week` — Weekly report (Mon-Sun containing today)
- `--json` — Output as JSON instead of human-readable format

**Example output:**
```
$ tt report --week

Time Report: Jan 20-26, 2025

Total: 32h 15m
  Direct:    22h 30m (70%)
  Delegated: 9h 45m  (30%)

By Tag:
  acme-webapp      18h 30m  ████████████░░░░
    Direct:   12h 00m
    Delegated: 6h 30m

  personal          8h 45m  █████░░░░░░░░░░░
    Direct:    6h 15m
    Delegated: 2h 30m

  untagged          5h 00m  ███░░░░░░░░░░░░░
    Direct:    4h 15m
    Delegated: 45m
```

**JSON output:**
```json
{
  "period": {"start": "2025-01-20", "end": "2025-01-26"},
  "total_ms": 116100000,
  "direct_ms": 81000000,
  "delegated_ms": 35100000,
  "by_tag": [
    {"tag": "acme-webapp", "total_ms": 66600000, "direct_ms": 43200000, "delegated_ms": 23400000},
    {"tag": "personal", "total_ms": 31500000, "direct_ms": 22500000, "delegated_ms": 9000000},
    {"tag": null, "total_ms": 18000000, "direct_ms": 15300000, "delegated_ms": 2700000}
  ]
}
```

---

### Stream Commands

#### `tt streams`

List inferred work streams.

```
tt streams [--today | --week] [--json]
```

**Options:**
- `--today` — Streams active today (default)
- `--week` — Streams active this week
- `--json` — Output as JSON

**Example output:**
```
$ tt streams --today

Streams: Jan 28, 2025

ID       Time     Tags           Description
───────────────────────────────────────────────────────────
a1b2c3d  2h 15m   acme-webapp    /home/user/acme/webapp
e4f5g6h  1h 30m   personal       /home/user/side-project
i7j8k9l  45m      (untagged)     /home/user/experiments
```

**Stream ID format:** 7-character hex prefix (like git short SHA). Users can use any unique prefix when tagging.

**Empty state:**
```
$ tt streams --today

Streams: Jan 28, 2025

No streams found for today.
Hint: Run 'tt status' to check collection health.
```

#### `tt tag`

Add a tag to a stream.

```
tt tag <STREAM-ID> <TAG>
```

**Arguments:**
- `STREAM-ID` — Full or prefix of stream ID
- `TAG` — Tag to apply (e.g., `acme-webapp`, `client:acme`, `billable`)

**Example:**
```bash
$ tt tag a1b auth-service
Tagged stream a1b2c3d with 'auth-service'
```

**Idempotent:** Tagging a stream with a tag it already has is a no-op (succeeds silently).

**Error handling:**
```bash
$ tt tag xyz my-tag
Error: No stream found matching 'xyz'
Hint: Run 'tt streams' to see available streams
```

```bash
$ tt tag a my-tag
Error: Ambiguous stream ID 'a' matches multiple streams: a1b2c3d, a9f8e7c
Hint: Use a longer prefix to disambiguate
```

#### `tt untag`

Remove a tag from a stream.

```
tt untag <STREAM-ID> <TAG>
```

**Example:**
```bash
$ tt untag a1b personal
Removed tag 'personal' from stream a1b2c3d
```

#### `tt tags`

List all tags with stream counts.

```
tt tags [--json]
```

**Example output:**
```
$ tt tags

Tags (4 total, all-time):

Tag             Streams   Time
─────────────────────────────────
acme-webapp          12   18h 30m
personal              8   8h 45m
client:acme           5   12h 00m
billable              3   15h 00m
```

**Note:** Time shown is total across all streams with that tag. Streams can have multiple tags. A stream tagged with both `acme-webapp` and `billable` contributes its time to both rows—totals may exceed total tracked time.

---

### Query Commands

#### `tt events`

Query raw events for debugging.

```
tt events [--since <TIMESTAMP>] [--type <TYPE>] [--limit <N>] [--json]
```

**Options:**
- `--since` — Filter events after this ISO 8601 timestamp
- `--type` — Filter by event type (e.g., `tmux_pane_focus`, `user_message`)
- `--limit` — Maximum number of events to output
- `--json` — Output as JSON (default is JSONL)

**Example:**
```bash
$ tt events --type user_message --limit 3
{"id":"abc","timestamp":"2025-01-28T10:00:00Z","type":"user_message",...}
{"id":"def","timestamp":"2025-01-28T10:05:00Z","type":"user_message",...}
{"id":"ghi","timestamp":"2025-01-28T10:15:00Z","type":"user_message",...}
```

---

### Utility Commands

#### `tt version`

Show version and mode information.

```
tt version
```

**Example output:**
```bash
$ tt version
tt 0.1.0 (local mode)
Database: ~/.local/share/tt/events.db
```

```bash
$ tt version   # on remote
tt 0.1.0 (remote mode)
Events buffer: ~/.local/share/time-tracker/events.jsonl
```

---

### Global Options

All commands support:

| Option | Description |
|--------|-------------|
| `--help` | Show help for this command |
| `--verbose` | Enable detailed output |
| `--config <PATH>` | Use alternate config file |
| `--db <PATH>` | Use alternate database (local only) |

---

## Output Formats

### Human-Readable (Default)

- Clean, scannable output
- Aligned columns where appropriate
- Progress bars for visual proportion
- Relative times where helpful ("3s ago", "yesterday")

### JSON (`--json`)

- Machine-parseable output
- All times in milliseconds
- Timestamps in ISO 8601 format
- Null for missing values (not omitted keys)

---

## Error Handling

### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | General error |
| 2 | Invalid arguments |

### Error Message Format

Errors include:
1. What went wrong
2. Why it might have happened
3. How to fix it (when possible)

**Example:**
```
Error: Cannot reach devserver (connection refused)
The SSH connection to devserver failed.
Try: ssh devserver 'tt export' to test connectivity
```

---

## Configuration

The CLI finds configuration in this order:
1. `--config` flag
2. `$TT_CONFIG` environment variable
3. `~/.config/tt/config.toml`
4. Defaults

Database location:
1. `--db` flag
2. `$TT_DB` environment variable
3. `~/.local/share/tt/events.db`

---

## Stream Inference

Streams are computed lazily when needed (during `tt report`, `tt streams`, or `tt status`). There is no separate `tt infer` command.

**Inference logic** (see `architecture/overview.md` for details):
- Events are clustered by working directory and temporal proximity
- Events within 30 minutes of each other in the same directory belong to the same stream
- Agent sessions create explicit stream boundaries

**First-query latency:** The first `tt streams` or `tt report` after sync may be slower as streams are computed. Subsequent queries use cached results until new events arrive.

---

## Acceptance Criteria

1. All commands listed above are implemented and documented
2. `tt --help` shows available commands with brief descriptions
3. `tt <command> --help` shows detailed usage for each command
4. `--json` output is valid JSON for all supported commands
5. Error messages include actionable guidance
6. Remote commands (`ingest`, `export`, `status`) start in <50ms
7. Local queries (`events`, `streams`) on 10,000 events complete in <1s
8. `tt report` on 10,000 events with stream inference completes in <3s
9. Empty states show helpful hints instead of blank output
10. Mode-specific commands fail fast with sync guidance when run in wrong mode

---

## Commands NOT in MVP

These commands from early brainstorming are explicitly deferred:

- `tt start` / `tt stop` — Conflicts with observe-first philosophy
- `tt note` — Manual annotations
- `tt contexts` — Higher-level abstraction over streams
- `tt agent-time` / `tt agent-cost` — Requires LLM cost tracking
- `tt config` / `tt rules` — CLI-based config editing
- `--watch` modes — Real-time updates
- `tt stream <id>` — Detailed single-stream view (use `tt events` as workaround)

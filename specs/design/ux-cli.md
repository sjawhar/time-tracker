# UX: Command-Line Interface

The `tt` CLI is the primary interface for time tracking. Commands are designed for both interactive use and scripting.

## Terminology

**Direct time**: Time when you're actively interacting with a context—typing, clicking, scrolling. Your attention is focused there.

**Delegated time**: Time when an AI agent (Claude Code) is working on your behalf. The agent is executing, but your attention may be elsewhere.

A session can have both: you might work on a task for 30 minutes (direct), then delegate to Claude for 2 hours (delegated) while you context-switch.

## Design Principles

### Verb-Based Commands

Commands follow a verb-based pattern (`sync`, `report`, `tag`) rather than noun-verb (`docker container create`). This matches how users think: "what do I want to do?" not "what object am I manipulating?".

### Human-First Output

Default output is designed for human readability. Use `--json` for machine-readable output when scripting.

### Fast Startup

Remote commands (`ingest`, `export`) must start in <50ms since they're called from tmux hooks. Local commands have more leeway but should remain responsive.

### Meaningful Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | General error |
| 2 | Invalid arguments / usage error |

### Local Timezone for Boundaries

Week and day boundaries use the local timezone. "Monday" means Monday in your timezone, not UTC. Events are stored in UTC but interpreted in local time for reporting.

## Command Reference

### Data Collection

These commands handle event collection and synchronization.

#### `tt ingest pane-focus`

Receive a tmux pane focus event. Called by tmux hooks, not users.

```
tt ingest pane-focus --pane <id> --cwd <path> --session <name> [--window <index>]
```

**Options:**
- `--pane` — tmux pane ID (e.g., `%3`)
- `--cwd` — current working directory of the pane
- `--session` — tmux session name
- `--window` — tmux window index (optional)

**Example:**
```bash
# In ~/.tmux.conf:
set-hook -g pane-focus-in 'run-shell "tt ingest pane-focus \
  --pane #{pane_id} \
  --cwd #{pane_current_path} \
  --session #{session_name} \
  --window #{window_index}"'
```

#### `tt export`

Output all events for sync to local machine. Reads from `~/.time-tracker/events.jsonl` and parses Claude Code session logs.

```
tt export
```

Outputs JSONL to stdout. Designed to be piped over SSH.

#### `tt import`

Import events from stdin into local SQLite database.

```
tt import
```

Reads JSONL from stdin. Duplicate events (same ID) are silently ignored.

**Example:**
```bash
cat events.jsonl | tt import
```

#### `tt sync <remote>`

Pull events from a remote host via SSH.

```
tt sync <remote>
```

**Arguments:**
- `<remote>` — SSH host (e.g., `user@host` or alias from `~/.ssh/config`)

Executes `ssh <remote> tt export` and pipes output to local import.

**Example:**
```bash
tt sync devserver
tt sync user@dev.example.com
```

**Errors:**
```bash
$ tt sync devserver
Error: SSH connection failed: Connection refused

$ tt sync devserver
Error: Remote command failed: 'tt' not found
Hint: Install tt on the remote machine first.
```

**Timeout:** Sync uses SSH's configured timeout. Configure `ConnectTimeout` in `~/.ssh/config` if needed.

### Inspection

Commands for checking status and debugging.

#### `tt status`

Show tracking health: sync status, event sources, and warnings.

```
tt status
```

**Output:**
```
Database: ~/.local/share/tt/events.db

Sources:
  remote.agent:  2025-01-29T12:45:00Z
  remote.tmux:   2025-01-29T12:43:00Z

Sync status:
  devserver:  2 min ago (847 events)
  staging:    1 hour ago (234 events)  [stale]
```

The `[stale]` warning appears when a remote hasn't been synced recently (threshold: 1 hour).

#### `tt events`

Dump raw events as JSONL for debugging.

```
tt events [--after <timestamp>] [--before <timestamp>]
```

**Options:**
- `--after` — only show events after this timestamp (ISO 8601)
- `--before` — only show events before this timestamp (ISO 8601)

**Example:**
```bash
tt events --after 2025-01-29T00:00:00Z | jq .
```

#### `tt streams`

List all streams with time totals and tags.

```
tt streams [--json]
```

**Output:**
```
STREAMS (last 7 days)

ID       Name                    Direct    Delegated  Tags
───────  ──────────────────────  ────────  ─────────  ──────────────────
abc123   tmux/dev/session-1      2h 15m    4h 30m     acme-webapp, urgent
def456   tmux/dev/session-2      45m       1h 20m
ghi789   tmux/staging/session-1  30m       15m        acme-webapp

Tip: Use 'tt tag <id> <tag>' to group sessions into projects.
```

Streams without tags show an empty Tags column. The tip helps users discover tagging.

### Reports

Commands for generating time reports.

#### `tt report`

Generate time reports for various periods.

```
tt report [--week | --last-week | --day | --last-day] [--json]
```

**Options:**
- `--week` — current week (Monday to Sunday)
- `--last-week` — previous week
- `--day` — today
- `--last-day` — yesterday
- `--json` — output as JSON

Default: `--week`

**Output (human-readable):**
```
TIME REPORT: Week of Jan 27, 2025

BY TAG
──────
acme-webapp                               6h 45m  ████████░░
  Direct:    2h 45m
  Delegated: 4h 00m

internal                                  2h 30m  ███░░░░░░░
  Direct:    1h 30m
  Delegated: 1h 00m

(untagged)                                1h 15m  █░░░░░░░░░
  Direct:    45m
  Delegated: 30m
  Sessions: tmux/dev/session-2, tmux/staging/session-1

SUMMARY
───────
Total tracked:  10h 30m
Direct time:    5h 00m (48%)
Delegated time: 5h 30m (52%)
```

**Note:** Reports work without tagging by grouping by session. The `(untagged)` section shows which sessions need tagging and encourages organization.

#### `tt week`

Shortcut for `tt report --week`.

```
tt week [--json]
```

#### `tt today`

Shortcut for `tt report --day`.

```
tt today [--json]
```

#### `tt yesterday`

Shortcut for `tt report --last-day`.

```
tt yesterday [--json]
```

### Stream Management

Commands for organizing and tagging streams.

#### `tt tag <stream> <tag>`

Add a tag to a stream.

```
tt tag <stream> <tag>
```

**Arguments:**
- `<stream>` — stream ID (e.g., `abc123`) or stream name (e.g., `tmux/dev/session-1`)
- `<tag>` — tag to add

Tags are additive—multiple tags per stream are supported.

**Example:**
```bash
$ tt tag abc123 acme-webapp
Tagged stream abc123 (tmux/dev/session-1) as "acme-webapp"
Tags: acme-webapp

$ tt tag abc123 urgent
Tagged stream abc123 (tmux/dev/session-1) as "urgent"
Tags: acme-webapp, urgent

# Can also use stream name
$ tt tag tmux/dev/session-2 internal
Tagged stream def456 (tmux/dev/session-2) as "internal"
Tags: internal
```

Confirmation output shows all current tags, not just the one added.

**Errors:**
```bash
$ tt tag nonexistent project-x
Error: Stream 'nonexistent' not found.

Hint: Use 'tt streams' to see available stream IDs.
```

## Common Workflows

### Initial Setup (Remote)

1. Install `tt` on the remote machine
2. Add tmux hook to `~/.tmux.conf`:
   ```bash
   set-hook -g pane-focus-in 'run-shell "tt ingest pane-focus \
     --pane #{pane_id} \
     --cwd #{pane_current_path} \
     --session #{session_name} \
     --window #{window_index}"'
   ```
3. Reload tmux: `tmux source ~/.tmux.conf`

### Daily Usage (Local)

```bash
# Sync events from remote(s)
tt sync devserver

# Check tracking health
tt status

# See today's progress
tt today

# Review untagged streams
tt streams

# Tag streams for reporting
tt tag abc123 acme-webapp
tt tag def456 internal

# Generate weekly report (Monday morning)
tt report --last-week
```

### Scripting

```bash
# Export weekly report as JSON for integration
tt week --json > report.json

# Check if sync is stale (exit code 1 if any remote is stale)
tt status --json | jq -e '.sync_status | all(.stale == false)'

# List all streams with a specific tag
tt streams --json | jq '.streams[] | select(.tags | contains(["acme-webapp"]))'
```

## Output Formats

### Human-Readable (Default)

Designed for terminal display with aligned columns, progress bars, and helpful hints.

### JSON (`--json`)

Machine-readable output for scripting. Available on commands that produce structured data:
- `tt status --json`
- `tt streams --json`
- `tt events` (always JSONL)
- `tt report --json`
- `tt week --json`
- `tt today --json`

## Global Options

These options are available on all commands:

| Option | Description |
|--------|-------------|
| `-v, --verbose` | Enable verbose output |
| `-c, --config <path>` | Path to config file |
| `--json` | Output as JSON (where applicable) |
| `-h, --help` | Show help |
| `-V, --version` | Show version |

## Deferred (Post-MVP)

The following commands are not in MVP scope but may be added based on usage patterns:

- `tt start/stop` — Manual time tracking
- `tt note` — Annotations
- `tt config` — Configuration management
- `tt summarize --session=<id>` — LLM summarization on remote
- `tt untag <stream-id> <tag>` — Remove tag from stream (may be needed earlier than post-MVP if tagging mistakes are common)
- `tt streams --recompute` — Force stream recomputation
- `tt agent-time` / `tt agent-cost` — Agent-specific views
- `tt tags` — List all tags in use (for discovery)

## Scalability Notes

The current design syncs all events on each `tt sync`. For large databases, incremental sync (tracking last sync position per remote) may be needed. This is acceptable for MVP but should be revisited if sync becomes slow.

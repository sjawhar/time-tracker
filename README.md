# tt - AI-Native Time Tracker

A time tracking system designed for AI-augmented development workflows. Instead of manual start/stop timers, `tt` passively observes your activity and infers where your time went.

## Quick Start

### 1. Install the Binary

```bash
cargo build --release
mkdir -p ~/.local/bin
cp target/release/tt ~/.local/bin/

# Verify tt is accessible (open a new terminal if needed)
tt --version
```

If `tt --version` fails, ensure `~/.local/bin` is in your PATH:
```bash
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc
source ~/.bashrc
```

### 2. Set Up tmux Integration

Copy the hook configuration to a stable location:

```bash
mkdir -p ~/.config/tt
cp /path/to/time-tracker/config/tmux-hook.conf ~/.config/tt/
```

Add to your `~/.tmux.conf`:

```bash
source-file ~/.config/tt/tmux-hook.conf
```

Reload and verify:

```bash
tmux source-file ~/.tmux.conf

# Verify the hook is loaded
tmux show-hooks -g | grep pane-focus
# Should show: pane-focus-in[0] -> run-shell "tt ingest..."

# IMPORTANT: Detach and reattach for focus-events to work
tmux detach
tmux attach
```

### 3. Verify Events Are Being Captured

Switch between a few tmux panes, then check:

```bash
tt events | tail -3
```

You should see output like:
```json
{"id":"...","timestamp":"2025-01-29T14:32:01Z","type":"tmux_pane_focus","cwd":"/home/user/project",...}
```

Each unique working directory becomes a "stream" that `tt` tracks separately.

### 4. View Your First Report

```bash
tt today
```

You should see time allocated to the directories you've been working in. That's it - you're tracking time.

---

## Key Concepts

Understanding these concepts helps you get the most out of `tt`:

| Concept | Description |
|---------|-------------|
| **Event** | A raw observation with a timestamp (pane focus, git commit, etc.). Immutable. |
| **Stream** | A coherent unit of work, inferred from events. Usually one per working directory. |
| **Direct Time** | Your active attention - reading, deciding, providing input. |
| **Delegated Time** | Time when AI agents work while you focus elsewhere. |
| **Tag** | A label for categorizing streams (e.g., `project:website`, `client:acme`). |

**The key insight**: Traditional time trackers assume one task at a time. `tt` handles multiple concurrent streams - you might have 3 AI agents running in different panes, and `tt` tracks which one has your attention at any moment.

---

## Multi-Machine Setup

If you work on remote servers via SSH, sync their events to your local machine.

### Prerequisites

On each remote machine:
1. Install `tt` binary (follow steps 1-2 above)
2. Set up tmux hooks
3. Verify events are being captured locally

### Syncing Events

```bash
# Sync from a remote host
tt sync user@remote-host

# Uses your ~/.ssh/config for custom ports/keys
tt sync myserver  # if "myserver" is configured in ssh config
```

**How it works**: Remote machines store events in `~/.local/share/tt/events.jsonl` (a simple JSON log). When you run `tt sync`, it SSHes to the remote, runs `tt export`, and imports the events into your local SQLite database for fast querying.

**Sync is idempotent**: Running it multiple times won't create duplicates. Events are deduplicated by their unique ID.

---

## Command Reference

### Reporting

Generate time reports for various periods. Reports use your local timezone.

```bash
# Current week (default)
tt week
tt report --week

# Last week
tt report --last-week

# Today / Yesterday
tt today
tt yesterday
tt report --day
tt report --last-day

# JSON output for scripting
tt report --json
```

### Stream Management

View and organize your tracked time by stream.

```bash
# List all streams with time totals
tt streams

# Tag a stream for categorization
tt tag <stream-id> project:time-tracker
tt tag <stream-id> client:acme

# Get AI-powered tag suggestions based on stream content
tt suggest <stream-id>
```

### Event Collection

These commands are typically called automatically by hooks, not manually.

```bash
# Ingest a pane focus event (called by tmux hooks)
tt ingest pane-focus --pane "%3" --cwd "/home/user/project" --session "main" --window "0"

# Export all events as JSONL (used by sync)
tt export

# Import events from stdin
cat events.jsonl | tt import
```

### Syncing

Transfer events from remote machines to your local database.

```bash
# Sync from remote (runs: ssh remote tt export | tt import)
tt sync user@hostname
```

### Debugging

Diagnose issues with event collection or time allocation.

```bash
# Show current tracking status
tt status

# Query events with filters
tt events
tt events --after "2025-01-29T00:00:00Z"
tt events --before "2025-01-30T00:00:00Z"

# Recompute time allocations (if times look wrong)
tt recompute

# Run stream inference on unassigned events
tt infer
```

---

## Configuration

Configuration file: `~/.config/tt/config.toml`

```toml
# Database location (default: ~/.local/share/tt/tt.db)
database_path = "/custom/path/tt.db"
```

Environment variables with `TT_` prefix override config file values.

---

## Data Storage

| Location | Machine | Purpose |
|----------|---------|---------|
| `~/.local/share/tt/events.jsonl` | Remote | Raw event log (simple JSON, easy to sync) |
| `~/.local/share/tt/tt.db` | Local | SQLite database (fast queries, reports) |
| `~/.config/tt/config.toml` | Both | Configuration |

**Why the split?** Remote machines write to a simple append-only JSON file - no dependencies, works anywhere. Your local machine imports these into SQLite for fast querying across all your machines.

---

## Architecture

```
Remote Machine                    Local Machine
┌─────────────────┐              ┌─────────────────┐
│ tmux hooks      │              │ SQLite DB       │
│      ↓          │   tt sync    │      ↑          │
│ events.jsonl    │ ──────────→  │ tt import       │
│      ↓          │              │      ↓          │
│ tt export       │              │ tt report       │
└─────────────────┘              └─────────────────┘
```

### Event Types

| Type | Source | Description |
|------|--------|-------------|
| `tmux_pane_focus` | tmux hooks | Pane gained focus |
| `window_focus` | ActivityWatch | Desktop window gained focus |
| `browser_tab` | Browser extension | Browser tab changed |
| `afk_change` | ActivityWatch | User went idle/active |

### Time Allocation

The allocation algorithm determines how to credit time to streams:

1. **Focus tracking**: When you focus a tmux pane, time accrues to that stream
2. **Focus hierarchy** (when window focus data available):
   - Terminal apps (Terminal, iTerm, Alacritty, etc.) → use tmux stream
   - Browser apps (Chrome, Firefox, Safari, etc.) → use browser tab stream
   - Other apps → use window focus stream
3. **AFK detection**: Idle periods are excluded from direct time

---

## Integrations

### tmux (Built-in)

Configured via `config/tmux-hook.conf`. Records pane focus events automatically.

### ActivityWatch (Optional)

For window focus and AFK detection on your local machine:

1. Install [ActivityWatch](https://activitywatch.net/)
2. Export `window_focus` and `afk_change` events
3. Import: `cat aw-events.jsonl | tt import`

### Browser Extensions (Optional)

For browser tab tracking:

1. Use a browser extension that logs tab changes
2. Export as `browser_tab` events with `stream_id` field
3. Import: `cat browser-events.jsonl | tt import`

---

## Troubleshooting

### No events appearing

1. **Verify hook is loaded**:
   ```bash
   tmux show-hooks -g | grep pane-focus
   # Should show: pane-focus-in[0] -> run-shell "tt ingest..."
   ```

2. **Verify `tt` is in PATH**:
   ```bash
   which tt
   # If not found, hooks will silently fail
   ```

3. **Test manual ingest**:
   ```bash
   tt ingest pane-focus --pane "%0" --cwd "$(pwd)" --session "test" --window "0"
   tt events | tail -1
   ```

4. **Check file permissions**:
   ```bash
   ls -la ~/.local/share/tt/
   ```

### Sync failing

1. **Verify SSH key auth**:
   ```bash
   ssh user@host echo ok
   # Should print "ok" without password prompt
   ```

2. **Verify `tt` exists on remote**:
   ```bash
   ssh user@host which tt
   ```

3. **Check remote has events**:
   ```bash
   ssh user@host cat ~/.local/share/tt/events.jsonl | head -1
   ```

### Time looks wrong

1. **Recompute allocations**:
   ```bash
   tt recompute
   ```

2. **Check event count**:
   ```bash
   tt events | wc -l
   ```

3. **Verify stream assignments**:
   ```bash
   tt streams
   ```

### Starting fresh

To clear all data and start over:

```bash
# On local machine
rm ~/.local/share/tt/tt.db

# On remote machines
rm ~/.local/share/tt/events.jsonl
```

### Disabling tracking

Remove or comment out the `source-file` line in `~/.tmux.conf`, then:
```bash
tmux source-file ~/.tmux.conf
```

---

## Development

```bash
cargo build           # Build
cargo test            # Run tests
cargo clippy --all-targets  # Lint
cargo fmt             # Format
cargo deny check      # Check dependencies
```

### Project Structure

```
crates/
├── tt-cli/     # CLI binary (clap)
├── tt-core/    # Domain logic: events, streams, allocation
├── tt-db/      # SQLite storage (rusqlite)
└── tt-llm/     # Claude API integration
```

---

## License

[Add license information]

# UX: TUI Dashboard

The `tt dash` command launches an interactive terminal dashboard for real-time visibility into tracked time and active streams.

## Purpose: When TUI vs CLI

**CLI for:** One-off queries, scripting, quick status checks, report generation
**TUI for:** Ongoing visibility, monitoring parallel agents, real-time awareness while working

The TUI answers: "What's happening right now?" while the CLI answers: "What happened?"

Use the TUI when you want passive visibility into multiple active agent sessions, or when you're actively switching between contexts and want orientation.

---

## Launch

```bash
tt dash [--poll <ms>]
```

**Options:**
- `--poll <ms>` — Polling interval in milliseconds (default: 1000)

**Exit codes:**
- `0` — Normal exit (user pressed `q`)
- `1` — Error (database not found, terminal too small, etc.)

---

## Layout

Two-panel design with header row, responsive to terminal width.

### Default Layout (≥100 columns)

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  Today: 4h 23m (direct: 2h 15m | delegated: 2h 08m) | devserver: 2m [stale]  │
├────────────────────────────────────┬─────────────────────────────────────────┤
│  ACTIVE STREAMS (3)                │  STREAM DETAILS                         │
│  * claude-code  tmux:dev:0   12m   │  acme-webapp/fix-auth                   │
│    claude-code  tmux:dev:2   45m   │  Tags: acme, urgent                     │
│    codex        tmux:stg:1   3m    │  Direct: 8m | Delegated: 4m             │
│  ─────────────────────────────────│                                         │
│  IDLE TODAY (2)                    │  Recent events:                         │
│    session-3 (15m ago)             │  - Edit: src/auth.rs                    │
│    session-4 (1h ago)              │  - Bash: cargo test                     │
│                                    │  - Read: README.md                      │
├────────────────────────────────────┴─────────────────────────────────────────┤
│  j/k nav  f focus  t tag  S sync  r refresh  ? help  q quit                  │
└──────────────────────────────────────────────────────────────────────────────┘
```

### Responsive Behavior

| Terminal Width | Behavior |
|----------------|----------|
| < 100 cols | Single column mode. Details panel hidden; press `Enter` to show as overlay |
| 100-140 cols | Two columns with truncated stream names (use `…`) |
| > 140 cols | Full names, expanded detail panel |

**Minimum width:** 60 columns. Below this, show an error and exit.

### Layout Regions

1. **Header Row** — Today's totals, sync status for each remote
2. **Left Panel: Stream List** — Active streams (with focus indicator), then idle streams
3. **Right Panel: Details** — Info about selected stream (or placeholder if none selected)
4. **Status Line** — Keybinding hints

---

## Stream List

The left panel shows all streams with activity today, split into two sections.

### Active Streams

Streams with an agent currently running OR user focus in the last 5 minutes.

```
ACTIVE STREAMS (3)
* claude-code  tmux:dev:0   12m   ← focused stream
  claude-code  tmux:dev:2   45m
  codex        tmux:stg:1   3m
```

**Columns:**
- Focus indicator (`*` for current pane)
- Agent type (or blank for manual terminal work)
- Stream name (truncated with `…` if needed)
- Time elapsed (direct + delegated)

Sorted by: Most recent activity first.

### Idle Streams

Streams with no recent activity (>5 min) but events today.

```
─────────────────────────────
IDLE TODAY (2)
  session-3 (15m ago)
  session-4 (1h ago)
```

Show time since last event. Collapsed by default if no active streams.

### Focus Indicator

The stream corresponding to the user's current tmux pane is marked with:
- `*` prefix
- `[FOCUSED]` label (optional, space permitting)
- Highlighted styling (bold or reverse video)

This is critical for orientation. The TUI must know which pane the user is looking at.

**Detection:** Match the current pane's working directory against streams. If no match, show "(no match)" in the details panel with a hint to sync.

---

## Details Panel

Shows information about the currently selected stream.

### When a stream is selected:

```
STREAM DETAILS
────────────────────────────────
acme-webapp/fix-auth

Tags: acme, urgent

Time today:
  Direct:    8m
  Delegated: 4m
  Total:     12m

Recent events:
  14:23  Edit: src/auth.rs
  14:21  Bash: cargo test
  14:18  Read: README.md
  14:15  Edit: Cargo.toml
  ...
```

**Fields:**
- Stream name (full, not truncated)
- Tags (comma-separated, or "(untagged)")
- Direct/delegated time breakdown
- Recent events (last 10, with timestamps)

### When no stream is selected:

```
STREAM DETAILS
────────────────────────────────
(select a stream with j/k)
```

---

## Header Row

Compact summary at the top of the screen.

```
Today: 4h 23m (direct: 2h 15m | delegated: 2h 08m) | devserver: 2m | staging: 1h [stale]
```

**Components:**
- `Today:` — Total tracked time for today
- `direct:` — Time when user was actively focused
- `delegated:` — Time when agents were working
- Remote names with time since last sync
- `[stale]` marker if sync is >1 hour old

**Header overflow:** If the header exceeds terminal width, prioritize:
1. Today's total (always shown)
2. Stale remotes (show `[stale]` indicators)
3. Other remotes (truncate with `...` or omit)

**Staleness threshold:** 1 hour (matches `tt status` behavior).

---

## Status Line

Bottom row with context-sensitive keybinding hints.

```
j/k nav  Enter detail  t tag  S sync  r refresh  ? help  q quit
```

Hints change based on context:
- In details panel: show `Tab` to return
- During filter: show `Esc` to cancel
- During tagging: show tag-specific hints

---

## Keybindings

### Navigation

| Key | Action |
|-----|--------|
| `j` / `↓` | Move selection down |
| `k` / `↑` | Move selection up |
| `g` | Go to top of list |
| `G` | Go to bottom of list |
| `f` | Jump to focused stream (current tmux pane) |
| `Tab` | Switch between panels |
| `Enter` | Show details (in narrow mode) or toggle expansion |
| `Home` / `End` | Go to top / bottom (alternative to `g`/`G`) |
| `Page Up` / `Page Down` | Scroll by page |

### Actions

| Key | Action |
|-----|--------|
| `t` | Tag selected stream (opens picker) |
| `u` | Remove tag from selected stream (opens picker) |
| `S` | Sync all remotes |
| `r` | Refresh data from database |
| `/` | Filter streams by name/tag |

### General

| Key | Action |
|-----|--------|
| `?` | Show help overlay |
| `q` / `Ctrl-C` | Quit |
| `Esc` | Cancel current action / close overlay |

### Tagging Flow

When `t` is pressed:

1. **Picker opens** with existing tags
2. **Type to filter** — list narrows as you type
3. **Navigate** with `j`/`k`
4. **Select** with `Enter` — applies tag to stream
5. **Create new:** If filter matches no tags, `Enter` creates the new tag
6. **Cancel** with `Esc`

```
┌─ Tag Stream ─────────────────┐
│ Filter: acm█                 │
│ ─────────────────────────── │
│ > acme-webapp                │
│   acme-infra                 │
│                              │
│ Enter select  Esc cancel     │
└──────────────────────────────┘
```

When filter matches no existing tags:

```
┌─ Tag Stream ─────────────────┐
│ Filter: new-project█         │
│ ─────────────────────────── │
│ No matching tags.            │
│                              │
│ Enter create "new-project"   │
└──────────────────────────────┘
```

### Untag Flow

When `u` is pressed on a tagged stream:

1. **Picker opens** with stream's current tags
2. **Select** with `j`/`k` and `Enter` to remove
3. **Cancel** with `Esc`

When `u` is pressed on an untagged stream:

Show status message: "Stream has no tags"

---

## Real-Time Updates

The TUI polls the database at a configurable interval (default 1 second).

### What updates each poll:

- Active streams list (new sessions appearing, ended sessions moving to idle)
- Time counters on active streams (tick up)
- Focus indicator (matches current pane)
- Header totals

### Optimization:

- **Change detection:** Check `MAX(timestamp)` from events AND `MAX(updated_at)` from streams before full refresh
- **Lazy details:** Details panel fetches only when selection changes OR selected stream's `updated_at` changes
- **Prepared statements:** Keep compiled SQL across polls
- **Connection reuse:** Single database connection for TUI lifetime
- **Busy timeout:** Set `PRAGMA busy_timeout = 1000` for resilience when other `tt` commands are running

### Sync integration:

Pressing `S` runs sync in the foreground with a cancellation option:

1. Display spinner with "Syncing... (Esc to cancel)"
2. Run sync in separate thread with timeout
3. On completion, refresh all data
4. On error or cancel, show status message and continue with stale data

---

## Colors

Terminal-native ANSI 16 colors for maximum compatibility.

| Element | Color |
|---------|-------|
| Focused stream | Bold white on blue (or reverse video) |
| Active streams | Normal (white/default) |
| Direct time | Green |
| Delegated time | Cyan |
| Stale indicator | Yellow |
| Error | Red |
| Idle streams | Dim (gray) |
| Panel borders | Default |
| Section headers | Bold |

### Accessibility

- **NO_COLOR support:** Respect the `NO_COLOR` environment variable. When set, disable all colors and use ASCII-only indicators.
- **TERM=dumb:** Detect dumb terminals and fall back to minimal formatting.
- **Focus indicator:** Always show the `*` prefix regardless of color/styling. Do not rely solely on color or bold for the focus indicator.
- **Labels over colors:** Time breakdown uses explicit labels (`direct:`/`delegated:`) in addition to colors.

---

## Edge Cases

### Narrow Terminal (< 100 cols)

Single-column mode:
- Show only stream list
- Press `Enter` to open details as overlay
- Press `Esc` to close overlay

### Terminal Too Small (< 60 cols or < 10 rows)

```
Terminal too small.
Minimum: 60x10

Resize terminal and press any key, or q to quit.
```

### No Active Streams

```
ACTIVE STREAMS
────────────────────────────────

No active streams.
Last activity: 45 minutes ago

Press 'S' to sync from remotes.
```

### No Events Today

```
ACTIVE STREAMS
────────────────────────────────

No events recorded today.

Hint: Run 'tt sync <remote>' to pull events.
```

### Database Empty (First Run)

```
┌── Welcome to tt dash! ───────────────────────────────────┐
│                                                          │
│  No data yet. To get started:                            │
│                                                          │
│  1. Install tt on your remote dev server                 │
│  2. Add tmux hook (see 'tt --help')                      │
│  3. Run 'tt sync <remote>' to pull events                │
│                                                          │
│  Press 'q' to quit, '?' for help.                        │
└──────────────────────────────────────────────────────────┘
```

### Database Error

Show in status line, don't crash:

```
│  ⚠ Database error: disk full. Showing stale data.                      │
```

Continue displaying cached data. Retry on next poll.

### Many Streams

Scrollable list. Use `/` to filter by name or tag.

### Long Stream Names

Truncate with `…` in list view. Show full name in details panel (wrapped if needed).

```
claude-code  tmux:dev:very-long-se…  12m
```

### Selected Stream Deleted

If the selected stream is deleted (by recompute or external process) between polls:

1. Clear selection
2. Show toast message: "Stream no longer exists"
3. Do not crash or show stale data

Implementation: Store stream ID for selection, not list index. Verify stream exists before rendering details.

### Stream with No Events Yet

If a stream exists but has no events (edge case during initial sync):

```
STREAM DETAILS
────────────────────────────────
tmux:dev:0

(no events yet)
```

### Filter Yields Zero Results

When filter is active but no streams match:

```
(0 results)

Press Esc to clear filter.
```

### Not a TTY

If stdout is not a terminal (piped or redirected):

```
Error: tt dash requires an interactive terminal.
```

Exit with code 1.

---

## Technical Notes

### Library

Use **Ratatui** with **crossterm** backend:
- Ratatui: Rust TUI framework (successor to tui-rs)
- crossterm: Cross-platform terminal manipulation

### Crate Structure

TUI is a module within `tt-cli`, not a separate crate:

```
crates/tt-cli/src/
├── commands/
│   ├── mod.rs
│   └── dash.rs          # Entry point: tt dash
└── tui/
    ├── mod.rs           # App state, event loop
    ├── app.rs           # Application state struct
    ├── ui.rs            # Main layout function
    ├── widgets/         # Custom widgets
    │   ├── mod.rs
    │   ├── stream_list.rs
    │   └── details.rs
    └── handlers.rs      # Keyboard input handling
```

### Event Loop Pattern

```rust
loop {
    terminal.draw(|f| ui(f, &app))?;

    if event::poll(Duration::from_millis(poll_interval))? {
        if let Event::Key(key) = event::read()? {
            if handle_key(&mut app, key)? == Action::Quit {
                break;
            }
        }
    }

    if app.should_refresh() {
        app.refresh_data(&db)?;
    }
}
```

### State Management

Use an enum for modal states to keep key handling explicit:

```rust
enum Mode {
    Normal,
    Filter { input: String },
    TagPicker { filter: String, selected: usize },
    UntagPicker { selected: usize },
    HelpOverlay,
    SyncInProgress,
}
```

In each mode, keybindings dispatch to mode-specific handlers. This prevents key events leaking between contexts.

### Error Recovery

- Install panic hook to restore terminal state on crash
- Install SIGHUP handler to restore terminal state before exit (SSH disconnect)
- Handle SIGTSTP/SIGCONT (Ctrl-Z/fg) to properly suspend and restore terminal state
- Catch database errors, show in status line, keep displaying stale data
- Handle terminal resize events gracefully

### Input Handling

- Debounce rapid key repeats for navigation (process at most one navigation per 50ms)
- Clear input queue when opening overlays to prevent key leakage
- Validate all input as UTF-8 before processing

### Testing Strategy

- **Unit tests:** Data transformation functions (formatting durations, truncating names)
- **Snapshot tests:** Render buffer output for various app states
- **Integration tests:** TUI with mock database

---

## Deferred (Future)

- Week sparkline in header (visual summary without full report)
- Stream merging (combine related streams)
- Custom keybindings via config
- Mouse support
- Export current view to clipboard
- Notifications (desktop notifications for agent completion)
- Recompute streams from TUI (`R` key) - low value, use `tt recompute` CLI instead

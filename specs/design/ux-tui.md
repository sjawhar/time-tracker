# UX: Terminal UI Dashboard

Real-time dashboard for monitoring parallel agent sessions and time allocation.

## Purpose

The TUI provides a live view of what you're working on and what agents are doing in the background. Use it when you need visibility into parallel work.

**Use CLI for:**
- Quick queries (`tt status`, `tt streams`, `tt report`)
- One-off operations (`tt tag`, `tt sync`)
- Scripting and automation
- Remote-only operations

**Use TUI for:**
- Real-time monitoring of parallel work
- Interactive stream management (tagging, reviewing)
- Daily/weekly review sessions
- Timeline visualization of overlapping streams

## Entry Command

```bash
tt dashboard
```

## Layout

Two-panel dashboard with header, footer, and modal overlays.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ tt                                                    Jan 28, 2025  14:23  │
├─────────────────────────────────────────────────────────────────────────────┤
│  TODAY  6h 23m                          ▶ acme-webapp                       │
│  ├ Direct     4h 12m  ████████████░░░░                                      │
│  └ Delegated  2h 11m  ██████░░░░░░░░░░                                      │
├───────────────────────────────┬─────────────────────────────────────────────┤
│  STREAMS  3 active · 2 idle   │  SESSIONS                                   │
│                               │                                             │
│▶ acme-webapp       2h 15m  ●  │  ● session-abc   12m   running              │
│  personal          1h 45m     │  ✓ session-def   45m   completed            │
│  experiments          45m     │  ✓ session-xyz   1h 18m completed           │
│                               │                                             │
│                               │  TIME BREAKDOWN                             │
│                               │  Direct     1h 30m  ████████████░░░░        │
│                               │  Delegated     45m  █████░░░░░░░░░░░        │
│                               │                                             │
│                               │  TAGS                                       │
│                               │  acme-webapp · feature-auth                 │
│                               │                                             │
│                               │  PATH                                       │
│                               │  /home/user/acme/webapp                     │
├───────────────────────────────┴─────────────────────────────────────────────┤
│  [j/k]move  [Enter]sessions  [t]ag  [a]ctive  [g]refresh  [r]eport  [?]help │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Layout Components

| Component | Content |
|-----------|---------|
| Header | App name, current date, last refresh time |
| Summary | Today's totals with direct/delegated bars, current stream name with ▶ |
| Streams Panel (left) | Scrollable stream list with status indicators |
| Detail Panel (right) | Sessions, time breakdown, tags, path for selected stream |
| Footer | Available keybindings |

### Stream Status Indicators

| Indicator | Meaning |
|-----------|---------|
| `●` | Active (has running session) |
| (blank) | Idle (no running sessions) |

### Session Status

| Status | Condition |
|--------|-----------|
| `● running` | Event within last 3 minutes |
| `✓ completed` | No events for 30m+ (SESSION_TIMEOUT_MS) |

Note: The intermediate "stale" state was removed as it caused more confusion than value. A 3-minute threshold for "running" accommodates reading docs or thinking without falsely alarming users.

## Views

### Dashboard (Main)

The default view showing streams and detail panels. Always visible unless a modal is open.

### Report Modal

Overlay showing weekly report (same output as `tt report --week`). Opens with `r`, closes with `q` or `Esc`.

Report data is fetched once on modal open (not live-updating). Shows "Report as of {timestamp}" in header. Close and reopen to refresh.

### Tag Editor Modal

Text input with autocomplete from existing tags. Opens with `t`, closes with `Enter` (save) or `Esc` (cancel).

```
┌─ Tag Stream ─────────────────────────────────┐
│                                              │
│  Stream: acme-webapp                         │
│  Current tags: feature-auth, backend         │
│                                              │
│  Add tag: feature-a█                         │
│                                              │
│  Suggestions: (↑/↓ select, Tab complete)    │
│    feature-api                               │
│    feature-admin                             │
│                                              │
│  [Enter] save  [Backspace] delete last tag   │
│  [Esc] cancel                                │
└──────────────────────────────────────────────┘
```

**Tag editor behavior:**
- Shows existing tags so users know what's already set
- `Enter` adds the typed tag to the stream
- `Backspace` on empty input deletes the last existing tag
- `↑`/`↓` navigate suggestions, `Tab` auto-completes
- Suggestions limited to 5 items, prefix matching only
- Exact matches filtered from suggestions

### Help Modal

Shows all keybindings. Opens with `?`, closes with any key.

## Interactions

### Keybindings

| Key | Action |
|-----|--------|
| `j` / `↓` | Move down in stream list |
| `k` / `↑` | Move up in stream list |
| `Enter` | Show session detail / drill-in |
| `t` | Open tag editor for selected stream |
| `a` | Toggle active-only filter |
| `g` | Manual refresh |
| `r` | Open report modal |
| `?` | Show help modal |
| `q` / `Esc` | Back / quit (closes modal, then quits) |
| `1-9` | Jump to stream N (quick access, shown in list) |

Footer shows: `[j/k]move  [1-9]jump  [Enter]detail  [t]ag  [a]ctive  [g]refresh  [?]help`

### Navigation Philosophy

- Vim-style navigation (`j`/`k`) as primary
- Arrow keys supported as alternative
- Single-key actions for common operations
- `?` for discoverability (shows help)
- Number keys for quick stream selection when managing 10+ streams

## State Management

### Model Structure

```rust
struct Model {
    data_state: DataState,
    streams: Vec<StreamWithSessions>,  // Includes sessions for detail panel
    selected_stream_id: Option<String>,
    scroll_offset: usize,              // First visible stream in list
    filter: FilterState,
    modal: ModalState,
    last_refresh: Instant,
    error: Option<String>,
}

enum DataState {
    Loading,
    Loaded,
    Error(String),
}

enum FilterState {
    All,
    ActiveOnly,
}

enum ModalState {
    None,
    Help,
    Report(ReportData),
    TagEditor {
        stream_id: String,              // Lock in target at modal open
        existing_tags: Vec<String>,
        input: String,
        cursor: usize,
        suggestions: Vec<String>,
        selected_suggestion: Option<usize>,
    },
}
```

**Invariant:** `selected_stream_id` must always be `None` or reference a stream in `streams`. After loading new data, validate and reset if invalid.

### Message/Command Pattern

Elm Architecture for async handling:

```rust
enum Message {
    KeyPress(Key),
    Tick,                        // Poll timer fired
    DataLoaded(QueryResult),     // Async query completed
    Error(String),               // Display error in status bar
}

enum Command {
    QueryDatabase,
    AddTag { stream_id: String, tag: String },
    RemoveTag { stream_id: String, tag: String },
}

// update() returns Option<Command> - None means no async work needed
fn update(model: &mut Model, msg: Message) -> Option<Command>
```

## Refresh & Polling

### Adaptive Polling

| Condition | Poll Interval |
|-----------|---------------|
| Recent activity (< 5 min) | 1-2 seconds |
| Normal | 5 seconds |
| Idle (no activity > 10 min) | 10-15 seconds |

### Visual Feedback

- Timestamp in header shows last refresh time
- Manual refresh with `g` key
- Status bar shows errors if refresh fails

## Data Consistency

The TUI reads from SQLite while `tt sync` or `tt import` may write to it.

### Requirements

1. **WAL mode**: Set in `tt-db::Database::open` (not TUI's responsibility). TUI assumes WAL.
2. **SQLITE_BUSY handling**: Use retry with exponential backoff (50ms, 100ms, 200ms) for all DB operations. After 3 failures, show stale data with "(stale)" indicator.
3. **Async DB access**: Use `tokio::task::spawn_blocking` for sync rusqlite calls.
4. **Mutations re-fetch**: Before tagging, re-query stream by ID. Use optimistic locking: if stream `updated_at` changed, show "Tag was changed externally. Overwrite?" confirmation.
5. **Vanishing entities**: If selected stream disappears, auto-select first stream with status message. If stream count drops to 0, show empty state.

### Poll Failure Handling

- After 5 consecutive poll failures, show modal: "Database unavailable. [R]etry or [Q]uit"
- Use exponential backoff: 1s, 2s, 4s, 8s, cap at 30s
- On first success after failures, clear error state and reset poll interval

## Terminal Requirements

- **Minimum size**: 80x24 characters
- **Undersized terminal**: Show warning overlay "Terminal too small (need 80x24)"
- **Resize handling**: Handle `SIGWINCH` / crossterm resize events to trigger re-render
- **Wide terminals**: Panels scale proportionally (not fixed-width)

## Timezone Handling

- All displayed times are local time (converted from UTC storage)
- "TODAY" calculated based on local midnight-to-midnight
- Session activity thresholds (3 min, 30m) use monotonic time where possible

## Empty States

### No Streams

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ tt                                                    Jan 28, 2025  14:23  │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│                          No streams found                                   │
│                                                                             │
│                     Run `tt sync <remote>` to import data                   │
│                                                                             │
├─────────────────────────────────────────────────────────────────────────────┤
│  [g]refresh  [q]uit  [?]help                                                │
└─────────────────────────────────────────────────────────────────────────────┘
```

### No Active Streams (with filter)

Shows "0 of N streams shown (active filter on - press [a] to show all)" when active-only filter hides all streams.

## Component Structure

```
tt-tui (new crate)
├── Dashboard
│   ├── Header (date, totals, current stream name)
│   ├── StatusBar (last refresh, errors, stale indicator)
│   ├── StreamsPanel (filterable list with scroll)
│   │   └── StreamListItem[] (with 1-9 index labels)
│   ├── DetailPanel (sessions, time breakdown, metadata)
│   │   ├── SessionList
│   │   ├── TimeBreakdown
│   │   └── StreamMetadata
│   └── Footer (keybindings)
├── Modals
│   ├── HelpModal
│   ├── ReportModal (shows "as of {timestamp}")
│   ├── TagEditorModal (shows existing tags, input, suggestions)
│   └── ErrorModal (database unavailable)
└── Model
    ├── data_state: DataState
    ├── streams: Vec<StreamWithSessions>
    ├── selected_stream_id: Option<String>
    ├── scroll_offset: usize
    ├── filter: FilterState
    ├── modal: ModalState
    └── last_refresh: Instant
```

**Crate decision**: Create `tt-tui` crate as a library dependency of `tt-cli` (feature-gated with `--features tui`). This keeps CLI fast when TUI isn't needed while allowing direct function calls (no subprocess IPC).

## Implementation Notes

- **Framework**: Rust + ratatui with crossterm backend
- **Architecture**: Elm Architecture (Model-Update-View)
- **Async**: tokio for non-blocking polling and database queries
- **Testing**:
  - Unit test `update` function with mock messages
  - Snapshot test `view` output with `insta`
  - Integration test with test database

## Acceptance Criteria

1. `tt dashboard` launches TUI without errors
2. Shows streams with correct direct/delegated time
3. Updates automatically (poll-based refresh visible via timestamp)
4. `j`/`k` navigation works, `Enter` drills into stream detail
5. `t` opens tag editor, changes persist to database
6. `a` toggles active-only filter
7. `g` triggers manual refresh
8. `r` shows report modal
9. `?` shows help modal
10. `q`/`Esc` closes modals, then quits
11. Handles empty states gracefully
12. Works when database is being written by concurrent `tt sync`

## Deferred (Out of Scope)

- Mouse support
- Custom themes/colors
- Timeline visualization (horizontal time bars)
- Keyboard shortcuts customization
- Split view showing multiple stream details
- Real-time updates via file watchers (polling is sufficient for MVP)

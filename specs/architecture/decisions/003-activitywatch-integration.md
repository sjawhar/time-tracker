# ADR-003: ActivityWatch Integration

## Status

Proposed

## Context

The time tracker captures events on remote servers (tmux pane focus, Claude Code sessions) and syncs them to the local machine. However, the attention allocation algorithm also requires **local activity data**:

- **Window focus**: Which application is active (to distinguish terminal vs browser)
- **AFK status**: Whether the user is at the keyboard (to stop direct time accrual when idle)
- **Browser tabs**: What the user is researching (for potential stream context)

Without local data, the algorithm can only account for time spent in terminal applications. Browser research, documentation reading, and other non-terminal work would be invisible.

### Why ActivityWatch

[ActivityWatch](https://activitywatch.net/) is an established open-source time tracker that already solves the hard problems of local activity collection:

- Cross-platform (Windows, macOS, Linux)
- Privacy-first (data stays local)
- Modular watchers for different data sources
- Well-documented REST API

Rather than re-implementing these watchers, we can import data from an existing ActivityWatch installation.

### Constraints

- User may or may not have ActivityWatch installed
- ActivityWatch stores data in SQLite, accessible via REST API when running
- Events have duration (heartbeat-based), unlike our point-in-time events
- Must work with existing `tt sync` mental model

## Decision

Integrate with ActivityWatch via batch import through its REST API, using `tt sync --aw`.

### Why REST API (not direct database access)

| Factor | REST API | Direct SQLite |
|--------|----------|---------------|
| **Stability** | Public API; unlikely to break | Internal schema; may change between versions |
| **Availability** | Requires aw-server running | Works offline |
| **Concurrency** | Thread-safe | Potential locking conflicts |
| **Path discovery** | Fixed URL (localhost:5600) | Varies by OS and install method |

The REST API is more stable and consistent. The trade-off is requiring ActivityWatch to be running—users who want local tracking will likely have it running anyway. If "AW not running" proves to be a significant friction point post-MVP, we can add SQLite fallback with version detection.

### Why batch import (not real-time polling)

- **Consistency**: Matches the pull-based model for remote events (`tt sync <remote>`)
- **Simplicity**: No daemon to manage; runs on-demand
- **Sufficient for MVP**: Reports are generated periodically, not in real-time

Real-time polling could be added post-MVP if use cases emerge.

## Implementation

### Command: `tt sync --aw`

Extend the existing `tt sync` command:

```bash
tt sync devserver      # Sync from remote machine (existing)
tt sync --aw           # Sync from local ActivityWatch
tt sync devserver --aw # Sync both (future)
```

**Alternative considered**: `tt sync activitywatch` as a source name. This would be more consistent with the remote sync grammar, but was rejected because:
1. ActivityWatch is always local, unlike remotes which have hostnames
2. The `--aw` flag clearly indicates "in addition to" when combined with a remote
3. Users are unlikely to confuse `--aw` with other sync sources

### Event Mapping

| ActivityWatch Bucket | tt Event Type | tt Source |
|---------------------|---------------|-----------|
| `aw-watcher-window_{hostname}` | `window_focus` | `local.window` |
| `aw-watcher-afk_{hostname}` | `afk_change` | `local.afk` |
| `aw-watcher-web-*` | `browser_tab` | `local.browser` |

### Event Transformation

**Window events**: Direct 1:1 mapping with field extraction.

**AFK events**: ActivityWatch stores duration-based events. Transform to paired point-in-time events:

```
AW Event: {timestamp: T1, duration: 300, status: "afk"}
    ↓
tt Events:
  - {timestamp: T1, type: afk_change, status: "idle"}
  - {timestamp: T1+300s, type: afk_change, status: "active"}
```

**AFK transformation edge cases:**

| Case | Handling |
|------|----------|
| **`not-afk` events** | ActivityWatch also produces `status: "not-afk"` events with duration. Transform identically: start event with `status: "active"`, end event with `status: "idle"`. |
| **In-progress AFK period** | If `timestamp + duration > now`, do NOT generate the synthetic end event. The user might still be AFK. The end event will be generated on the next sync when the duration is finalized. |
| **Back-to-back AFK periods** | Overlapping end/start events are fine—the algorithm handles rapid state changes. |
| **Zero-duration events** | Generate only the start event (no end event to generate). |

**Synthetic event IDs**: Include a suffix to ensure uniqueness:
- Start event: `hash(source + type + timestamp + data + ":start")`
- End event: `hash(source + type + end_timestamp + data + ":end")`

**Browser events**: Extract domain from URL for indexing.

### Incremental Sync

Track sync position in `sync_state` table:

```sql
CREATE TABLE sync_state (
    source TEXT PRIMARY KEY,        -- 'activitywatch:aw-watcher-window_laptop'
    last_event_id INTEGER,          -- ActivityWatch event ID (more reliable than timestamp)
    last_timestamp TEXT NOT NULL,   -- ISO 8601 (fallback for API queries)
    last_sync_at TEXT NOT NULL,     -- When the sync completed
    event_count INTEGER DEFAULT 0   -- Total events synced
);
```

**Why track both ID and timestamp**: The ActivityWatch API supports `?start=<timestamp>` queries. However, timestamp-based sync can miss events if two events share the same timestamp. Track the last event ID to detect and handle this case:

1. Query `GET /api/0/buckets/{id}/events?start={last_timestamp}`
2. Filter out events with `id <= last_event_id` from the response
3. Update `last_event_id` and `last_timestamp` after successful import

First sync defaults to 7-day lookback to avoid overwhelming the database. Override with `--since`.

### Bucket Discovery

On each sync:
1. Query `/api/0/buckets/` for all buckets
2. Filter to buckets matching current hostname
3. Process matching window, AFK, and browser watchers

**Hostname matching**: Normalize both the system hostname and bucket suffix:
- Convert to lowercase
- Strip domain suffix (`.local`, `.lan`, etc.)
- Match if normalized names are equal

This handles common variations like `Laptop` vs `laptop`, `dev.local` vs `dev`.

Re-discovery on every sync handles:
- New watchers installed
- Hostname changes (new buckets picked up automatically)
- Bucket ID changes (e.g., after reinstall)

### Error Handling

| Condition | Message | Exit Code |
|-----------|---------|-----------|
| Connection refused | See detailed message below | 1 |
| Buckets exist but none match hostname | See detailed message below | 0 (warning) |
| No watchers found (AW just installed) | `ActivityWatch running but no data yet. Wait a few minutes for watchers to collect activity.` | 0 |
| API error (5xx) | `ActivityWatch returned an error. Try restarting aw-server.` | 1 |

**Connection refused message** (distinguishes "not running" from "not installed"):

```
Could not connect to ActivityWatch at localhost:5600.

If you have ActivityWatch installed:
  - Make sure aw-server is running
  - Check the system tray for the ActivityWatch icon

If you don't have ActivityWatch:
  - Local activity tracking requires ActivityWatch
  - Install from: https://activitywatch.net/
  - Or skip this step if you only track terminal activity
```

**Hostname mismatch message**:

```
ActivityWatch is running, but no watchers found for this host (laptop).
Available watchers: aw-watcher-window_desktop, aw-watcher-afk_desktop

This usually means ActivityWatch was set up on a different machine.
To use data from a different host, set TT_ACTIVITYWATCH_HOSTNAME=desktop
```

### Configuration

```bash
TT_ACTIVITYWATCH_URL=http://localhost:5601 tt sync --aw      # Custom port
TT_ACTIVITYWATCH_HOSTNAME=desktop tt sync --aw               # Match different hostname
```

### Output Format

**Normal sync:**
```
Synced from ActivityWatch (Jan 21 - Jan 28):
  Window changes:  623 new (847 total)
  AFK periods:      12 new (34 total)
  Browser tabs:     82 new (156 total)
```

**First sync:**
```
First sync from ActivityWatch (importing last 7 days):
  Window changes:  4,231 events
  AFK periods:       89 events
  Browser tabs:    1,456 events

ActivityWatch integration is now active.
Run 'tt report' to see your time breakdown.
```

**Already up to date:**
```
Synced from ActivityWatch: already up to date (last sync: 2m ago)
```

**Stale data warning in `tt report`:**
```
Note: Local activity data is 2 days old. Run 'tt sync --aw' to update.
```

Only show this warning if:
- User has previously synced AW data (not a new installation)
- Last sync was >24 hours ago
- The report period extends past the last sync timestamp

## Consequences

### Positive

- **Reuses proven technology**: ActivityWatch watchers are mature and cross-platform
- **Clean separation**: ActivityWatch collects, tt analyzes
- **Public API**: Stable interface, won't break on internal changes
- **Unified mental model**: `tt sync` for all data sources
- **Incremental sync**: Efficient after initial import
- **Clear first-run experience**: Distinguishes installation states

### Negative

- **Requires ActivityWatch**: User must install and run ActivityWatch
- **Not real-time**: Must manually sync; stale data possible
- **Two databases**: Events duplicated between AW and tt SQLite

### Deferred

- **Automatic sync**: `tt report --sync` to sync before generating report
- **Direct SQLite fallback**: Read AW database when server not running
- **cwd inference from window titles**: Terminal titles often contain paths
- **Browser URL → stream mapping**: Research URLs could help infer stream context

## Acceptance Criteria

- [ ] `tt sync --aw` connects to ActivityWatch REST API
- [ ] Window, AFK, and browser events are imported and transformed
- [ ] AFK transformation handles `not-afk` events and in-progress periods correctly
- [ ] Sync is incremental (tracks both timestamp and event ID)
- [ ] First sync defaults to 7-day lookback (configurable via `--since`)
- [ ] Events have deterministic IDs (idempotent import)
- [ ] Clear error messages distinguish "not running" from "not installed"
- [ ] Hostname matching is case-insensitive and strips domain suffix
- [ ] `tt status` shows last ActivityWatch sync time
- [ ] `tt report` warns when AW data is stale (>24h past last sync)
- [ ] `TT_ACTIVITYWATCH_URL` and `TT_ACTIVITYWATCH_HOSTNAME` environment variables are respected

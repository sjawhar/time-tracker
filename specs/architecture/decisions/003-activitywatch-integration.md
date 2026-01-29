# ADR-003: ActivityWatch Integration

## Status

**Accepted**

## Context

The time tracker captures events on remote dev servers (tmux hooks, Claude session logs) but needs local activity data (AFK status, active window) to calculate accurate attention allocation.

**Constraints:**
- **Privacy**: Local activity data should remain local
- **Optional dependency**: Not all users want to install ActivityWatch
- **Minimal reinvention**: AFK and window tracking are solved problems
- **Architecture fit**: Must integrate with existing event-sourced model

**The gap**: How does the time tracker capture local desktop activity without reinventing OS-level watchers?

## Decision Drivers

1. **Privacy alignment** — Both ActivityWatch and Time Tracker are local-first
2. **Avoid reinventing** — AFK detection and window tracking require OS-specific APIs that AW has refined over years
3. **Optional dependency** — Time Tracker should work without AW (remote-only mode)
4. **Simplicity** — Consume existing data rather than build new collectors

## Options Considered

### Option A: Consumer (Pull from AW REST API)

Time Tracker queries ActivityWatch's REST API to get events.

```
[ActivityWatch] ──REST API──> [Time Tracker] ──> [SQLite]
```

**Pros:** AW handles local data collection, no duplicate watchers, graceful degradation
**Cons:** Dependency on AW being installed/running, schema coupling, AW updates could break integration

### Option B: Watcher (Push to AW)

Time Tracker acts as an ActivityWatch watcher, pushing events to AW.

**Pros:** TT events visible in AW dashboard, unified view
**Cons:** TT becomes dependent on AW, remote events can't reach AW, mismatched architecture

### Option C: Independent Implementation

Time Tracker implements its own local watchers.

**Pros:** Full control, no dependencies
**Cons:** Reinventing solved problems, more code to maintain, duplicates work if user runs AW

## Decision

**Option A: Consumer (Pull from AW REST API) with graceful degradation**

Rationale:
1. **AW solves a hard problem well** — AFK and window tracking require OS-specific APIs; AW has years of refinement
2. **Our differentiation is elsewhere** — Time Tracker's value is remote event capture and attention allocation, not local watchers
3. **Privacy alignment** — Both tools are local-first; no conflict
4. **Graceful degradation** — Treating AW as optional means TT works without it, but we accept schema coupling to AW's event model when AW is present
5. **Avoids duplication** — Users already running AW don't need duplicate watchers

## Consequences

### Good

- Leverage mature, well-tested local activity tracking without maintenance burden
- Privacy preserved: all data stays local
- Optional dependency: works without AW (remote-only mode)
- Users with existing AW setup get immediate value

### Bad

- External dependency that could break on AW updates
- Schema coupling: we depend on AW's bucket naming, event structure, and watcher semantics
- AFK threshold controlled by AW, not TT
- Users must install and configure AW separately for full functionality

### Neutral

- Browser tracking (`aw-watcher-web`) deferred to post-MVP
- May evolve to independent implementation if AW dependency proves problematic

## Implementation Notes

### Data Model Mapping

| AW Bucket | TT Source | TT Event Type |
|-----------|-----------|---------------|
| `aw-watcher-afk_{hostname}` | `local.afk` | `afk_change` |
| `aw-watcher-window_{hostname}` | `local.window` | `window_focus` |
| `aw-watcher-web_{hostname}` | `local.browser` | `browser_tab` (post-MVP) |

Skip `aw-watcher-tmux` and editor watchers — TT has better coverage via direct tmux hooks and Claude log parsing.

### AW API Endpoints

```
GET http://localhost:5600/api/0/buckets
GET http://localhost:5600/api/0/buckets/{bucket_id}/events?start={timestamp}&end={timestamp}
```

**Version compatibility**: AW API uses `api/0/` prefix. Test against AW stable releases (v0.12+). If bucket naming convention changes in future AW versions, use the buckets list API to discover buckets by watcher type rather than constructing names.

### Configuration

```toml
# ~/.config/tt/config.toml
[activitywatch]
enabled = true  # default
url = "http://localhost:5600"  # configurable for non-standard setups (Docker, VMs, custom port)
```

### Event Transformation

AW events are interval-based (`{timestamp, duration, data}`). TT expects point events.

**Transformation rules:**

1. **Sort events by timestamp** before processing
2. **Skip zero-duration events** — they produce timestamp collisions
3. **Merge overlapping intervals** — take the union; don't emit spurious state changes
4. **Handle gaps** — if gap between intervals exceeds 2× AFK threshold, emit a `data_gap` marker event

**AFK events:**
```
AW: { timestamp: "09:00:00Z", duration: 300, data: { status: "not-afk" } }
TT: Two events:
    - afk_change(status=active) at 09:00:00Z
    - afk_change(status=idle) at 09:05:00Z (timestamp + duration)
```

If two consecutive AW events have the same status (e.g., both `not-afk`), deduplicate by only emitting one `afk_change` at the start of the merged interval.

**Window focus events:**
```
AW: { timestamp: "09:00:00Z", duration: 60, data: { app: "Terminal", title: "zsh" } }
TT: window_focus(app="Terminal", title="zsh") at 09:00:00Z
    (next window event marks end of previous interval)
```

For the final window event in a sync batch, the interval remains open. It will be closed by the next sync or by attention window expiration during report generation.

**Timestamp handling:**
- AW returns ISO 8601 timestamps (should be UTC)
- If timezone is missing, assume local time and convert to UTC with a warning
- If timestamp is more than 5 minutes in the future, log warning (clock skew detected)

### Sync Behavior

AW sync happens automatically as part of `tt sync`:

```
$ tt sync devserver
Syncing from devserver... 847 events
Syncing from ActivityWatch... 1,204 events
```

**Multi-remote behavior**: AW sync happens once per `tt sync` invocation, not per remote:
```
$ tt sync devserver staging
Syncing from devserver... 847 events
Syncing from staging... 234 events
Syncing from ActivityWatch... 1,204 events
```

**Local-only sync**: `tt sync` with no arguments syncs only from AW:
```
$ tt sync
Syncing from ActivityWatch... 1,204 events
```

**When AW is unavailable**, show brief note in sync output (not just logs):
```
$ tt sync devserver
Syncing from devserver... 847 events
ActivityWatch: not detected (run 'tt doctor' for help)
```

### Watermark Tracking

Store last-synced timestamp per AW bucket in local database:

```sql
CREATE TABLE aw_sync_state (
  bucket_id TEXT PRIMARY KEY,
  last_synced_at TEXT NOT NULL  -- ISO 8601 timestamp (query end time, not max event time)
);
```

**Critical**: Set watermark to the query `end` parameter, not the max timestamp of returned events. This prevents missing events if AW returns them out of order.

**Atomicity**: Update watermark and insert events in the same database transaction. If the transaction fails, neither is persisted.

**New buckets**: When a bucket is first discovered (no watermark), query from 30 days ago or from the earliest remote event timestamp, whichever is more recent.

**Large backfills**: If the initial sync would return >10,000 events, process in daily batches to avoid memory issues. Update watermark after each batch.

### Clock Synchronization

**Assumption**: All machines (local laptop, remote dev servers) have synchronized clocks via NTP. Without NTP, attention allocation may be incorrect.

**Clock skew detection**: During sync, compare the most recent AW event timestamp with local time. If they differ by more than 5 minutes:
```
Warning: ActivityWatch timestamps differ from system time by 8 minutes.
         Check clock synchronization. Time reports may be inaccurate.
```

**Remote/local interleaving**: The attention allocation algorithm interleaves events from all sources by timestamp. If remote server clock is off, this interleaving will be incorrect. Document this as a known limitation; require NTP as a prerequisite.

### AFK Threshold Interaction

AW's default AFK threshold is 3 minutes; TT's attention window is 60 seconds.

**Interaction semantics:**
- `afk_change(idle)` from AW is an explicit event that clears focus
- Attention window expiration is computed during report generation (not an event)
- When AW data is present, `afk_change` is authoritative for idle detection
- Attention window still applies to direct time within non-idle periods

**Example:** User is focused on Stream A, stops typing to read code:
- At T=0: User typing, focused on Stream A
- At T=60: Attention window expires (direct time calculation pauses)
- At T=180: AW fires `afk_change(idle)` (3 min threshold)
- Result: Direct time ends at T=60 (attention window), not T=180

**Implication**: Document that AFK detection timing is controlled by AW when AW is present, but attention window is TT's own computation.

### Error Handling

| Condition | Behavior |
|-----------|----------|
| AW not running | Show note in sync output, skip AW sync, continue |
| AW bucket missing | Skip that bucket, continue with others |
| AW API error | Warning log + note in sync output, continue |
| AW disabled in config | Skip AW sync silently |
| Connection timeout | 5s connect timeout, 30s read timeout, 1 retry with exponential backoff |

Tiered messaging:
- **Implicit context** (sync, report): Brief notes, don't block workflow
- **Explicit context** (status, doctor): Full explanation with guidance

### Status and Doctor Commands

**`tt status` output:**
```
$ tt status
Local sources:
  ActivityWatch:  connected (afk, window)

Remote sources:
  devserver:  2 min ago (847 events)
```

When AW unavailable:
```
$ tt status
Local sources:
  ActivityWatch:  not detected

Remote sources:
  devserver:  2 min ago (847 events)

Hint: Install ActivityWatch for idle detection and window tracking.
      https://activitywatch.net/downloads/
```

**`tt doctor` output** (for setup verification):
```
$ tt doctor

SYSTEM CHECK

Remote sources:
  devserver        OK (last sync: 2 min ago)

Local sources:
  ActivityWatch    OK (connected, tracking: afk, window)
    AFK threshold: 3 minutes (configured in ActivityWatch settings)
    Last event: 30 seconds ago

Data quality:
  This week: 92% coverage (8% gaps from AW not running on Mon-Tue)
```

When AW not installed:
```
Local sources:
  ActivityWatch    NOT INSTALLED

  ActivityWatch provides:
  - Idle detection: Time when you're away from keyboard is not counted
  - Window tracking: See time spent in different applications

  Without ActivityWatch, time reports may overcount by 15-30%.

  Install: https://activitywatch.net/downloads/
```

When AW installed but not running:
```
Local sources:
  ActivityWatch    NOT RUNNING

  ActivityWatch is installed but not currently running.
  Start it from your applications menu or system tray.
```

### Data Source Visibility

Reports should indicate data sources:

```
WEEK OF Jan 27 – Feb 2, 2025
Data sources: devserver (remote), ActivityWatch (local)

Tue Jan 28    1h 15m  ██░░░░░░░░  [incomplete: no local data]
Wed Jan 29    6h 30m  ██████████
```

Days without AW data are flagged as incomplete to explain potential underreporting.

### What Users Lose Without AW

**Without ActivityWatch:**
- No idle detection: time continues accumulating when user is away from keyboard (until attention window expires)
- No window tracking: time spent outside terminal (browser research, documentation) is invisible
- Typical impact: 15-30% overreporting or inaccurate attribution

This is surfaced via `tt doctor` and in report footnotes.

### Testing Strategy

Mock AW server for unit tests. Test cases:
1. AW running, all buckets present
2. AW running, partial buckets (afk only)
3. AW not running
4. AW running but returns errors
5. Event transformation correctness
6. Watermark tracking across syncs
7. **Zero-duration AFK event** — verify no duplicate timestamps
8. **Overlapping AFK intervals** — verify correct merge behavior
9. **Gap in AFK data > 2× threshold** — verify gap handling
10. **Watermark atomicity** — crash between fetch and insert, verify recovery
11. **Clock skew simulation** — AW timestamps 5 minutes ahead
12. **Large backfill** — 100,000+ events in first sync
13. **Rapid consecutive syncs** — verify no duplicate events stored

### Privacy Note

AW window tracking captures window titles, which may contain sensitive information (email subjects, document names, chat messages). This data is stored locally in TT's SQLite database. Users should be aware that window titles are captured when AW is enabled.

## Non-Goals

This ADR does not decide:
- Browser tracking specifics (post-MVP)
- Independent watcher implementation (fallback if AW proves problematic)
- AW data retention/cleanup (AW manages its own data)
- Multi-machine AW synchronization
- Window title sanitization or hashing (future consideration)

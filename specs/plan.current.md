# ADR-003: ActivityWatch Integration

**Task:** ADR: ActivityWatch integration — Document decision and rationale

## Research Summary

### What is ActivityWatch?

[ActivityWatch](https://github.com/ActivityWatch/activitywatch) is an open-source automated time tracker with:
- **Privacy-first design**: All data stored locally
- **Modular architecture**: Server + watchers + REST API
- **Cross-platform**: Windows, macOS, Linux

**Core concepts:**
- **Buckets**: Containers grouping activity data by source (hostname, client type)
- **Events**: `{timestamp, duration, data}`
- **Heartbeats**: API that merges adjacent identical events within a time window

### Relevant ActivityWatch Watchers

| Watcher | What it tracks | Our interest |
|---------|----------------|--------------|
| `aw-watcher-afk` | Keyboard/mouse → idle detection | **High** — AFK detection |
| `aw-watcher-window` | Active app/window title | **High** — Non-terminal focus |
| `aw-watcher-web` | Browser tab (Chrome/Firefox) | **Medium** — Browser tracking |
| `aw-watcher-tmux` | tmux pane focus | **Low** — We already do this |
| `aw-watcher-vim/vscode` | Editor activity | **Low** — We track agents directly |

Sources:
- [ActivityWatch Architecture](https://docs.activitywatch.net/en/latest/architecture.html)
- [Watchers Documentation](https://docs.activitywatch.net/en/latest/watchers.html)
- [REST API Reference](https://docs.activitywatch.net/en/latest/api/rest.html)

### Time Tracker's Current Design

The specs already anticipate ActivityWatch integration:

**From data-model.md:**
- Event sources include `local.window`, `local.afk`, `local.browser`
- Event types include `window_focus`, `afk_change`, `browser_tab`

**From architecture/overview.md:**
- "Local attention from extracted ActivityWatch watchers"
- Focus algorithm handles `window_focus` to non-terminal
- AFK detection already part of attention allocation

**The gap:** No ADR documents the decision to integrate with ActivityWatch or the approach.

## Options

### Option A: AW Consumer (Pull Model)

Time Tracker queries ActivityWatch's REST API to get events.

```
[ActivityWatch] ──REST API──> [Time Tracker] ──> [SQLite]
```

**Implementation:**
1. `tt sync --local` or `tt aw-sync` command
2. Query AW buckets: `http://localhost:5600/api/0/buckets/{id}/events`
3. Transform AW events → TT events
4. Import to local SQLite

**Pros:**
- AW handles all local data collection (mature, well-tested)
- No duplicate watchers needed
- AW already running on many developers' machines
- Graceful degradation if AW not installed

**Cons:**
- Dependency on another tool being installed/running
- Need to transform AW event schema to TT schema
- AFK threshold may differ between AW and TT
- AW updates could break integration

### Option B: AW Watcher (Push Model)

Time Tracker acts as an ActivityWatch watcher, pushing events to AW.

```
[Time Tracker] ──heartbeat──> [ActivityWatch]
```

**Pros:**
- TT events visible in AW dashboard
- Unified view of all activity

**Cons:**
- TT becomes dependent on AW
- Remote events can't easily reach AW
- AW is local-only; doesn't match our remote-first architecture

### Option C: Independent Implementation

Time Tracker implements its own local watchers (no AW dependency).

**Pros:**
- Full control over implementation
- No external dependencies
- Consistent event format

**Cons:**
- Reinventing what AW does well
- More code to maintain
- Duplicates work if user also runs AW

### Option D: Optional Consumer with Graceful Degradation

ActivityWatch is an optional enhancement. When available, TT pulls AW data. When absent, TT works without local activity tracking.

**Implementation:**
- Check if AW is running at localhost:5600
- If available: pull afk + window events
- If unavailable: skip local events, rely on remote-only data

## Recommendation

**Option D: Optional Consumer with Graceful Degradation**

### Rationale

1. **AW solves a hard problem well**: AFK detection and window tracking require OS-specific APIs. AW has years of refinement.

2. **Our differentiation is elsewhere**: Time Tracker's value is remote event capture (tmux, Claude agents) and attention allocation. Local activity tracking is complementary, not core.

3. **Privacy alignment**: Both TT and AW are local-first, privacy-focused. No conflict.

4. **Minimal coupling**: By treating AW as optional, we:
   - Don't force users to install another tool
   - Don't break if AW API changes (graceful degradation)
   - Support users who only care about remote tracking

5. **Avoids duplication**: Users who already run AW don't need duplicate watchers.

### What to Consume

| AW Bucket | TT Event Type | Priority |
|-----------|---------------|----------|
| `aw-watcher-afk` | `afk_change` | **MVP** — Required for attention calculation |
| `aw-watcher-window` | `window_focus` | **MVP** — Context when terminal not focused |
| `aw-watcher-web` | `browser_tab` | **Post-MVP** — Nice-to-have |

Skip `aw-watcher-tmux` and editor watchers — TT has better coverage via direct tmux hooks and Claude log parsing.

### Edge Cases

1. **AW not running**: Log warning, proceed with remote-only data
2. **AW bucket missing**: Skip that data source, continue with others
3. **AW API changes**: Version check; fail gracefully with warning
4. **AFK threshold mismatch**: Use AW's events as-is; don't second-guess their detection
5. **Duplicate events**: TT's deterministic IDs prevent duplicates across syncs

### Integration with Existing Architecture

**Data flow:**
```
┌─────────────────────────────────────────────────────────────────┐
│                       LOCAL (laptop)                            │
│                                                                 │
│  ┌──────────────────┐      ┌──────────────┐                    │
│  │  ActivityWatch   │──────│ tt aw-sync   │                    │
│  │  (if running)    │      │ (optional)   │                    │
│  └──────────────────┘      └──────┬───────┘                    │
│                                   │                             │
│                                   ▼                             │
│  ┌──────────────┐     ┌──────────────┐     ┌────────────────┐  │
│  │   tt sync    │────▶│  tt import   │────▶│  SQLite store  │  │
│  │  (remote)    │     │              │     │                │  │
│  └──────────────┘     └──────────────┘     └────────────────┘  │
│                                                    │            │
│                                                    ▼            │
│                              ┌─────────────────────────────────┐│
│                              │ attention allocation algorithm  ││
│                              │ (uses both remote + AW events)  ││
│                              └─────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────┘
```

**Command integration:**
- `tt sync <remote>` — unchanged, pulls remote events
- `tt sync --local` or `tt aw-sync` — pulls from local AW instance
- `tt status` — shows "AW: connected" or "AW: not available"

## ADR Outline

The ADR should cover:

1. **Status**: Accepted
2. **Context**: Need local activity data (AFK, window focus) for complete attention tracking
3. **Decision Drivers**: Privacy, optional dependencies, avoid reinventing
4. **Options**: A (Consumer), B (Watcher), C (Independent), D (Optional Consumer)
5. **Decision**: Option D — Optional Consumer with Graceful Degradation
6. **Consequences**: Good (leverage AW, optional), Bad (dependency, schema coupling)
7. **Implementation Notes**:
   - AW API endpoints to query
   - Event transformation mapping
   - Error handling for AW unavailable
   - `tt status` shows AW connection status

## Review Feedback

### Architecture Review (code-architect)

**Verdict:** Core decision is correct. Gaps are implementation details.

**Key concerns:**

1. **Collapse Options A and D** — they're the same architecture; "graceful degradation" is an implementation quality attribute, not a separate option.

2. **Sync timing undefined** — When does AW sync happen? If only on-demand, reports could have stale AFK data. Recommend: `tt report` auto-syncs AW data.

3. **Interval-to-point event transformation** — AW events have `duration` (interval-based). TT expects point events. Example:
   ```
   AW: { timestamp: "09:00:00", duration: 300, data: { status: "not-afk" } }
   TT: Two events:
       - afk_change(status=active) at 09:00:00
       - afk_change(status=idle) at 09:05:00 (derived)
   ```
   This conversion needs explicit documentation.

4. **Make AW endpoint configurable** — Don't hardcode `localhost:5600`. Users run AW in VMs, containers, or non-standard ports. Add to `config.toml`.

5. **AFK threshold mismatch** — AW defaults to 3 min, TT to 5 min. Which wins? Options:
   - Accept AW's threshold (simple, but TT config is ignored)
   - Document that TT uses AW's definition when AW is present
   - Tell users to configure AW if they want different thresholds

6. **Watermark management** — How is "last synced" tracked per bucket? Not specified.

7. **Testing strategy** — How to test without AW running? Need mock server or API stub.

8. **Data source visibility in reports** — So users know what data contributed to the report.

### UX Review (ux-designer)

**Verdict:** Grade B-. Architecture sound, but UX for optional dependency has significant gaps.

**Key concerns:**

1. **No onboarding story** — How does a first-time user learn AW exists and would help? Most developers have never heard of ActivityWatch.

   **Recommendation:** Add `tt doctor` command:
   ```
   $ tt doctor
   [OK] Database: ~/.local/share/tt/events.db
   [OK] Remote sync: 2 remotes configured

   [INFO] ActivityWatch not detected
         Install: https://activitywatch.net/downloads/
   ```

2. **Command naming is confusing** — Neither `tt sync --local` nor `tt aw-sync` is good.

   **Recommendation:** Make AW sync automatic in `tt sync`. No separate command.
   ```
   $ tt sync devserver
   Syncing from devserver... 847 events
   Syncing from ActivityWatch... 1,204 events
   ```

3. **Error messages not specified** — When to warn vs. be silent?

   **Recommendation:** Tiered messaging:
   - Implicit context (sync, report): Subtle notes, don't block
   - Explicit context (status, doctor): Full explanation with guidance

4. **Status output incomplete** — Design needed:
   ```
   Local sources:
     ActivityWatch:  connected (afk, window)

   Remote sources:
     devserver:  2 min ago (847 events)
   ```

5. **Mixed-state UX undefined** — What if AW is partially configured? What if AW has gaps (laptop was asleep)?

   **Recommendation:** Flag incomplete data days in reports:
   ```
   Tue Jan 28    1h 15m  ██░░░░░░░░  [incomplete: no local data]
   ```

6. **No way to disable AW** — Add config option `activitywatch.enabled = false`.

7. **Quantify the value** — What do users lose without AW? "Without ActivityWatch, `tt` cannot detect idle time or time spent outside terminal. This typically results in 15-30% underreporting."

## ADR Outline (Revised)

Based on review feedback, the ADR should cover:

1. **Status**: Accepted
2. **Context**: Need local activity data for complete attention tracking
3. **Decision Drivers**: Privacy, optional dependencies, avoid reinventing
4. **Options**:
   - A: Consumer (pull from AW REST API)
   - B: Watcher (push to AW)
   - C: Independent implementation
5. **Decision**: Option A (Consumer) with graceful degradation
6. **Consequences**: Good (leverage AW), Bad (dependency, schema coupling)
7. **Implementation Notes**:
   - **AW endpoint**: Configurable, default `localhost:5600`
   - **Sync timing**: Automatic before `tt report`; also via `tt sync`
   - **Event transformation**: Interval → point events (detailed mapping)
   - **Watermark tracking**: Store `last_synced_timestamp` per bucket
   - **Error handling**: Tiered messaging based on context
   - **Config**: `activitywatch.enabled` (default true), `activitywatch.url`
   - **Testing**: Mock AW server for unit tests
8. **UX Considerations**:
   - `tt doctor` for discoverability
   - Data source visibility in reports
   - AFK threshold behavior documented
9. **What Users Lose Without AW**: Explicit statement of degraded functionality

## Open Questions (Resolved)

1. **Should we vendor AW client library?** No — use raw HTTP; the API is simple
2. **When to sync AW data?** Automatic before `tt report`; also via `tt sync`
3. **How to handle overlapping events?** TT's deterministic IDs + import deduplication handles this
4. **Command naming?** No separate command; `tt sync` includes AW automatically
5. **AFK threshold?** Use AW's threshold when AW is present; document this

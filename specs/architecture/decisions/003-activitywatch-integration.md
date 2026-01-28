# ADR-003: ActivityWatch Integration

## Status

**Accepted**

## Context

The MVP needs local attention signals (active window, browser tab, AFK) without building OS-specific watchers. The system is offline-first and event-sourced: raw events are immutable, and derived state is recomputed on demand. We want a simple, cross-platform way to capture local activity while keeping the CLI lightweight and avoiding background daemons.

ActivityWatch provides watcher-based collection with an `aw-server` that stores per-watcher buckets. It supports active window tracking and AFK detection across major platforms, with known constraints (Wayland coverage and macOS permissions).

## Decision

Integrate with ActivityWatch as the local attention signal source by importing ActivityWatch exports. The MVP will read ActivityWatch export JSON from:
- REST export on localhost (`/api/0/export`), or
- a user-provided export file when REST is unavailable.

`tt` maps ActivityWatch buckets to `tt` event sources/types:
- `currentwindow` -> `local.window` (`window_focus`)
- `afkstatus` -> `local.afk` (`afk_change`)
- `web.tab.current` -> `local.browser` (`browser_tab`)

Imported ActivityWatch events are treated as raw, immutable events with no additional AFK filtering; `tt` performs its own attention allocation on top. Event IDs are deterministic based on ActivityWatch event timestamp + bucket type + data payload to ensure idempotent imports.

Scope for MVP is local-only: no cross-host aggregation of ActivityWatch data.

## Consequences

**Pros:**
- Mature, cross-platform watchers without custom OS work
- Reuses existing AFK detection and active window signals
- Aligns with offline-first, event-sourced design
- Simple ingestion path (REST export or file)

**Cons:**
- Depends on `aw-server` running locally for REST exports
- Wayland support varies by compositor; some desktops are not supported
- macOS window titles require Accessibility permissions
- Bucket schema coupling; changes in ActivityWatch may require updates

## Research Findings

- ActivityWatch uses per-watcher buckets with UTC timestamps, duration, and JSON data payloads.
- Default watchers are `aw-watcher-afk` and `aw-watcher-window`, with optional web tab watcher.
- REST export endpoints provide full-bucket JSON, suitable for offline import.
- Watcher coverage is not uniform on Wayland; macOS requires explicit permissions for window titles.

## Options Considered

1. **Build native OS watchers**
   - Rejected: high maintenance across OSes, slows MVP delivery.

2. **Rely only on remote signals (tmux focus)**
   - Rejected: misses local activity and AFK, underestimates attention.

3. **Integrate ActivityWatch exports**
   - Accepted: fastest path to reliable local attention signals.

## Edge Cases & Failure Modes

- `aw-server` not running or unreachable; import falls back to file-based export.
- Watchers missing (e.g., no web watcher), leading to sparse signals.
- ActivityWatch data gaps (sleep, crash) create periods with no local events.
- Multiple hosts exporting to the same local machine; MVP ignores cross-host merges.

## Acceptance Criteria

- ADR documents ActivityWatch as the chosen local signal source.
- Mapping from ActivityWatch buckets to `tt` event sources/types is specified.
- Import method (REST export with file fallback) and idempotent event IDs are defined.
- Known platform limitations (Wayland/macOS) are documented.

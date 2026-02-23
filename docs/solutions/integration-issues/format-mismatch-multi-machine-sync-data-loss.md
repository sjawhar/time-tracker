---
title: "Multi-Machine Event Sync Silent Data Loss"
problem_type: integration-issue
component: tt-cli/export, tt-cli/import, tt-cli/sync, tt-db
severity: critical
date_resolved: 2026-02-23
symptoms:
  - Weekly report showed 120h instead of 262h (54% undercounted)
  - Remote machine events had NULL cwd, session_id, action after import
  - Zero events from ngrok-remote assigned to any stream
  - Streams had NULL first/last_event_at, hiding them from 7-day views
root_cause_count: 6
tags: [multi-machine, sync, export, import, serde, cwd, data-loss]
related:
  - docs/plans/2026-02-19-multi-machine-sync-plan.md
  - specs/architecture/decisions/001-event-transport.md
---

# Multi-Machine Event Sync Silent Data Loss

## Symptom

`tt report --last-week` showed 120h total (50h direct). After fixing, it showed 262h total (77h direct). 142 hours of work from the remote machine (ngrok-remote) were silently dropped.

## Root Causes (6 cascading issues)

### 1. Export format nested fields that import expected flat

`ExportEvent` serialized `cwd`, `session_id`, `action` inside a nested `data: {}` object. `StoredEvent` (the import target) expected these as top-level fields, and its `data` field was `#[serde(skip)]`.

**Result:** Every agent event from remote machines lost `cwd`, `session_id`, and `action` on import. 161k+ events imported with NULL metadata.

**Fix:** Changed `ExportEvent.data` from `pub data: Value` to `#[serde(flatten)] pub data: Value`.

```rust
// Before: nested — import loses everything inside data{}
pub struct ExportEvent {
    pub data: Value,  // {"action":"started","cwd":"/home/...","session_id":"..."}
}

// After: flattened — fields appear at top level
pub struct ExportEvent {
    #[serde(flatten)]
    pub data: Value,  // action, cwd, session_id all become top-level
}
```

### 2. Only agent_session events carried cwd

`user_message` and `agent_tool_use` events (95% of all agent events) had no `cwd` field. Only the `agent_session` start/end events carried the working directory.

**Result:** Even after fixing the flatten issue, 22k+ events per week had no cwd and couldn't be assigned to streams.

**Fix:** Added `cwd: Option<String>` to `UserMessageData` and `AgentToolUseData`. Changed `seen_sessions` from `HashSet<String>` to `HashMap<String, Option<String>>` to track and propagate the session's cwd to all events.

### 3. Cross-machine paths broke exact cwd matching

Remote machine used `/home/sami/time-tracker/default`, local used `/home/ubuntu/time-tracker/default`. The `auto_assign_events_to_streams` function matched by exact cwd path.

**Fix:** Added `project_suffix()` that strips `/home/<username>/` and matches by the remaining path. Tries exact match first, falls back to suffix match.

```rust
fn project_suffix(cwd: &str) -> Option<&str> {
    let path = cwd.strip_prefix("/home/")?;
    let after_user = path.find('/')? + 1;
    Some(&path[after_user..])
}
```

### 4. Missing auto_assign_events_to_streams function

`index_sessions()` called `auto_assign_events_to_streams(db)` but the function was never implemented. The code didn't compile.

**Fix:** Implemented the function — builds a cwd-to-stream map from assigned events, matches unassigned events.

### 5. NULL last_event_id crashed sync

`get_machine_last_event_id_by_label()` used `row.get::<_, String>(0)` which panics on NULL. After resetting a machine's sync cursor, the sync command crashed.

**Fix:** Changed to `row.get::<_, Option<String>>(0)` with `.flatten()`.

### 6. Remote binary not updated after local fixes

The export format was fixed locally, but the remote machine still ran the old binary with nested `data`. Re-syncing just re-imported the same broken format.

**Fix:** `cargo build --release && ./scripts/deploy-remote.sh ngrok-remote`, then delete old events and re-sync.

## Investigation Timeline

1. `cargo build` fails — missing `auto_assign_events_to_streams` (5 min fix)
2. Sync from ngrok-remote — 161k events imported, but only 24 auto-assigned
3. Discovered: ngrok-remote events have `cwd` inside nested `data` object
4. Fixed export with `#[serde(flatten)]` — but remote binary still old
5. Deployed new binary to remote — events now flat, but still 0 cwd on user_message/tool_use
6. Added cwd to all event types — deployed again
7. Still only exact-match auto-assign — added suffix matching
8. Final: created 17 streams via stream inference, assigned all 230k+ events
9. Report: 262h total, 77h direct

## Key Lesson

**The export and import formats were never tested together.** Each worked independently but had incompatible assumptions about JSON structure. A single round-trip integration test would have caught this immediately:

```rust
#[test]
fn export_import_roundtrip() {
    // Export events from a test session
    let exported = export_to_string(&test_events);
    // Import them back
    let imported = import_from_string(&exported);
    // Verify fields survived the trip
    assert_eq!(imported[0].cwd, test_events[0].cwd);
    assert_eq!(imported[0].session_id, test_events[0].session_id);
}
```

## Prevention Checklist

- [ ] Add export/import round-trip integration test that verifies all fields survive
- [ ] Add `tt sync` version check — refuse to sync if remote binary is older than local
- [ ] Use a shared intermediate type for export/import instead of separate `ExportEvent` and `StoredEvent` deserialization
- [ ] Enforce NOT NULL on `streams.first_event_at` and `streams.last_event_at` — or update them in `recompute`
- [ ] Add `tt doctor` command that checks: events without cwd, streams with NULL timestamps, unassigned event counts
- [ ] Test cross-machine paths in CI (events with `/home/alice/` imported to `/home/bob/` env)

## Files Changed

| File | Change |
|------|--------|
| `crates/tt-cli/src/commands/export.rs` | `#[serde(flatten)]` on data, added cwd to UserMessageData/AgentToolUseData, `HashMap` for session tracking |
| `crates/tt-cli/src/commands/ingest.rs` | Implemented `auto_assign_events_to_streams()`, added `project_suffix()` for cross-machine matching |
| `crates/tt-cli/src/commands/import.rs` | (reverted attempted hoist — fix was in export instead) |
| `crates/tt-db/src/lib.rs` | `get_machine_last_event_id_by_label` NULL handling, added `delete_events_by_machine` |

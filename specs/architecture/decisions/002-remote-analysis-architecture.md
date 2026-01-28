# ADR-002: Remote Analysis Architecture (Pane Context)

## Status

**Accepted**

## Context

The local machine generates reports from events captured on remote tmux sessions. Reports must be deterministic and available offline, even if the remote host is unavailable. Pane identifiers are temporal, and pane context (path, command, title) can change between capture and report time. We need a reliable way for local analysis to attribute activity to the correct pane context without live remote queries.

Constraints:
- Remote hosts may be offline when reporting
- No new daemon for MVP
- Events are immutable (event-sourced truth)
- SSH environment forwarding is non-default and fragile

## Decision

Capture pane context on the remote at focus time and embed it in the event payload. The tmux `pane-focus-in` hook runs `tt ingest` with tmux format expansion to include the context snapshot (session name, window name, pane id, pane title, current path, current command, host). The local machine uses the embedded snapshot for reporting and does not query the remote at sync or report time.

No SSH `SendEnv`/`AcceptEnv` reliance is introduced, and no background daemon is added. Periodic heartbeat events are deferred unless evidence shows focus events are too sparse.

## Consequences

**Pros:**
- Offline, deterministic reporting
- No live remote dependency
- Simple remote capture logic via tmux hooks
- Event payload fully describes attribution context

**Cons:**
- Context can become stale if a pane changes without a focus event
- Sparse focus events may underrepresent context changes
- Future heartbeat events might be required if gaps appear

## Research Findings

- tmux focus hooks (`pane-focus-in`/`pane-focus-out`) require `focus-events` and are the canonical trigger for focus changes.
- tmux format strings expose the required context fields for snapshots.
- SSH environment forwarding is not enabled by default and is brittle for context capture.
- Activity tracking systems (WakaTime/ActivityWatch) support sparse signals with downstream aggregation.

## Options Considered

1. **Live SSH query at report time**
   - Rejected: non-deterministic, remote may be offline, context can drift from historical state.

2. **SSH environment forwarding for context**
   - Rejected: non-default configuration and security constraints; brittle across hosts.

3. **Periodic context heartbeats**
   - Deferred: adds overhead and noise without evidence of gaps in focus events.

## Edge Cases & Failure Modes

- Pane path/command changes without focus events, leading to stale context snapshots.
- tmux `focus-events` disabled (no focus hooks fired) resulting in missing context events.
- Users running non-tmux sessions (no pane context available) still rely on generic events.

## Acceptance Criteria

- Remote `tt ingest` events include pane context fields captured at focus time.
- Local reports require no live SSH queries for pane context.
- Documentation states no dependence on SSH env forwarding or background daemons.
- Known limitations around sparse focus events are documented with a deferred heartbeat option.

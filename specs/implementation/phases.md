# Implementation Phases

This document defines the long-term roadmap for Time Tracker beyond MVP.

## Overview

Phases define **what happens after MVP**. They are outcome-driven, not time-based—transitions occur when specific criteria are met, not after arbitrary durations.

### Principles

1. **Validate before expanding**: Each phase builds on validated learnings from the previous one
2. **User signals drive transitions**: Move phases based on feedback patterns, not calendars
3. **Technical debt is addressed incrementally**: No dedicated "refactoring phase"—each phase includes relevant hardening
4. **Roadmap evolves**: This document will be updated as we learn from real usage

### Key Decisions

| Decision | Rationale |
|----------|-----------|
| TUI reads SQLite directly | No runtime dependency on API server; faster startup |
| Rules Engine in Phase 1 | Immediate time savings against "<5 min/week" goal; low risk |
| Incremental sync in Phase 2 | ~50 lines implementation; unconditional to avoid decision overhead |

---

## Phase 1: Usage Validation + Quick Wins

**Goal**: Validate that MVP meets success criteria in real use while eliminating predictable friction and enabling mistake recovery.

### Entry Criteria

- MVP implementation complete and deployed
- User actively using the system for daily work

### Features

| Feature | Rationale |
|---------|-----------|
| Bug fixes | Address issues discovered in real usage |
| Rules Engine | Eliminates repetitive manual tagging for predictable paths (e.g., `~/work/acme/*` always gets `acme` tag). Includes `tt rules list`, `tt rules validate`, and `tt rules test --path` for rule management. |
| `tt untag <stream> <tag>` | Recovery from tagging mistakes. Without this, users cannot fix incorrect tags without database editing. |
| `tt tags` | List all tags in use. Required for discovering existing tags when writing rules or tagging new streams. |
| Config improvements | Adjust defaults based on real usage patterns |
| First-run onboarding | Clear guidance when commands run with no data or missing configuration |

### Technical Work

- Enable WAL mode in SQLite (required for Phase 2 TUI)
  - Migration runs automatically on first startup, uses exclusive lock
  - Other `tt` commands detect migration-in-progress and wait
- Verify all event sources are producing data (tmux, Claude sessions)
- Glob path canonicalization must timeout after 1 second to handle slow/hanging mounts

### Exit Criteria

- [ ] <5 minutes/week manual work
  - Measured by user estimate or by counting manual `tt tag`/`tt untag` invocations
  - Most streams should be auto-tagged by rules
- [ ] Stable for 4 weeks:
  - No unhandled panics or data corruption
  - Sync succeeds >95% of user-initiated sync commands
- [ ] All event sources producing data (>95% capture rate over 7 days)
- [ ] User asking for features rather than reporting bugs

### Signals to Proceed

User feedback patterns shift from "this is broken" to "it would be nice if..."

### Rollback Criteria

If Phase 1 features introduce regressions, revert the specific feature rather than the entire phase. Rules Engine can be disabled via config without losing data.

---

## Phase 2: Usability (TUI + Incremental Sync)

**Goal**: Make daily use effortless through a real-time dashboard and faster sync.

### Entry Criteria

- Phase 1 exit criteria met
- WAL mode enabled in database

### Features

| Feature | Rationale |
|---------|-----------|
| TUI dashboard | Real-time visibility without repeated CLI queries; passive monitoring while working (see `ux-tui.md` for detailed spec) |
| Incremental sync position tracking | Eliminates re-importing already-synced events. ~50 lines, significant UX improvement. |
| Sync-in-progress indicator | TUI shows "Sync in progress..." during imports to prevent confusion from fluctuating numbers |

### Technical Work

- Ratatui + crossterm for terminal UI
- Handle terminal compatibility edge cases (see `ux-tui.md` for requirements)
- Terminal resize during overlays: close overlays, return to Normal mode
- Prepared statements for efficient polling

### Exit Criteria

- [ ] User can complete daily workflow in TUI: check today's time, tag new streams, verify sync status
- [ ] TUI works reliably in user's actual environment (local, SSH, tmux nesting as applicable)
- [ ] TUI runs for 8+ hours without crash during active use
- [ ] Sync of 1000+ events completes in <5 seconds (incremental sync working)

### Signals to Proceed

User asks "can I export this to Toggl?" or similar integration requests.

### Rollback Criteria

If TUI causes >2 data integrity issues in first 2 weeks, revert to CLI-only while addressing root cause. Incremental sync is independent and should not need rollback.

---

## Phase 3: Integration (API + Export)

**Goal**: Connect Time Tracker with external timesheet and productivity tools.

### Entry Criteria

- Phase 2 exit criteria met
- Clear user demand for external tool integration

### Features

| Feature | Rationale |
|---------|-----------|
| API server | Programmatic access for scripts, external tools (see `integrations.md` for detailed spec) |
| Toggl or Clockify export | Direct API sync to at least one external timesheet system |
| Webhooks (conditional) | Push notifications; implement only if user demand exists |

### Technical Work

- HTTP server (localhost only, no auth needed)
- SQLite schema additions for webhook state persistence (if webhooks implemented)
- Export format mappings (tt concepts → Toggl/Clockify concepts)
- Rate limiting and error handling for external API calls
- Webhook retries processed on next `tt` command invocation
- Webhook retry queue capped at 100 entries per endpoint (oldest discarded with warning)

### Exit Criteria

- [ ] Time data flows to at least one external system without manual re-entry
- [ ] API endpoints match spec in `integrations.md`
- [ ] Sync failures surface clearly with actionable recovery steps

### Signals to Proceed

Query latency exceeds 500ms or sync time exceeds 30 seconds.

---

## Phase 4: Scale & Reliability

**Goal**: Handle larger event volumes and longer history without degradation.

### Entry Criteria

- Phase 3 exit criteria met OR performance issues emerge
- Event count approaching limits where current queries are slow

### Features

| Feature | Rationale |
|---------|-----------|
| Query optimization | Index tuning, query rewriting for common patterns |
| Event archival | Move old events to separate archive without losing history |
| Parallel/batch sync | Faster sync for large event volumes |

### Technical Work

- Profiling to identify actual bottlenecks (not theoretical)
- Composite indexes for common query patterns
- Archival strategy designed based on actual data:
  - Must preserve sync markers (use timestamps, not event IDs)
  - Reports must include archived data when queried
  - Initial options: separate SQLite file with query federation, or compressed events table

### Exit Criteria

- [ ] Report generation and TUI refresh feel instantaneous (no perceptible delay)
- [ ] Benchmark queries:
  - `tt status` <100ms at 1M events
  - `tt report --week` <500ms at 1M events
  - `tt report --all-time` <2s at 1M events
- [ ] Sync time scales sub-linearly with event count
- [ ] Archival does not break report generation or sync

---

## Phase 5: Future Exploration

**Goal**: Explore features based on validated user demand.

### Entry Criteria

- Phase 4 exit criteria met
- Clear user demand for one of the candidate features

### Candidate Features

These are ideas, not commitments. Implement based on actual demand:

| Candidate | Notes |
|-----------|-------|
| Advanced analytics | Trends over time, productivity patterns, cost tracking |
| ActivityWatch integration | Import AW events for desktop application tracking |
| Mobile companion (read-only) | Read-only view of time data |
| More event sources | Codex sessions, other editors |

### Exit Criteria

N/A—this phase is ongoing exploration.

---

## Not In Scope

These features are explicitly excluded from the roadmap:

| Feature | Rationale |
|---------|-----------|
| Multi-user/team features | Single-user tool; simplicity is a feature. Reconsider only if user demand emerges AND architecture permits. |
| Cloud hosting | Local-first by design; user controls their data |
| Mobile app (write) | No input on mobile; read-only is Phase 5 candidate |
| Real-time sync | Pull-based architecture is simpler and sufficient. TUI requires manual sync for remote data. |
| Multiple databases | Single SQLite file per user. Use `TT_DATABASE_PATH` env var for testing if needed. |
| Enterprise features | SSO, audit trails, compliance—not our market |
| GUI application | TUI is the interactive interface; API (Phase 3) enables third-party frontends |
| Undo/history for tagging | Tagging is immediate; no audit trail of tag changes |

---

## Risks

| Risk | Mitigation |
|------|------------|
| Claude log format changes | Monitor for silent failures; `tt status` warns if no Claude events recently |
| Claude API changes | Pin to specific API version; rules engine reduces LLM dependency; monitor Anthropic changelog |
| TUI terminal compatibility | Test in user's actual environment early in Phase 2 |
| Glob canonicalization on slow mounts | 1-second timeout; skip rule with warning on timeout |
| Event archival breaks sync | Use timestamps for sync position, not event IDs |
| Concurrent CLI usage | WAL mode + `PRAGMA busy_timeout = 1000` |
| LLM API cost runaway if rules disabled | Rate limit LLM calls; warn if rules file has errors |
| Clock skew between local and remote | `tt status` warns if remote timestamps >5 minutes in future |
| Disk full during sync | Wrap sync in transaction; rollback entire sync on failure |
| Config changes require restart | Commands print config load time; `tt rules list` warns if config newer than load |

---

## Appendix: Phase Transition Signals

How to know when to move between phases:

| Transition | Signal |
|------------|--------|
| MVP → Phase 1 | MVP deployed and actively used |
| Phase 1 → Phase 2 | User asking for features, not bug fixes |
| Phase 2 → Phase 3 | User asking about external tool integration |
| Phase 3 → Phase 4 | Query latency >500ms or sync >30 seconds |
| Phase 4 → Phase 5 | Specific feature requests from user |

The roadmap is a guide, not a contract. Adapt based on what we learn.

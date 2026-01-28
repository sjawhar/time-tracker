# Implementation Phases

Long-term roadmap for Time Tracker development.

## Design Principles

1. **Phases deliver user value** — Each phase ships tangible capabilities, not internal milestones
2. **Optional features are explicit** — Some phases are only relevant for certain workflows
3. **Performance matters early** — Validate query performance before expanding data sources
4. **Defer team features** — Single-user value first; team features risk premature abstraction

---

## Phase Overview

| Phase | Theme | Status | Primary Value |
|-------|-------|--------|---------------|
| 1 | Foundation | ✅ Complete | Prove data collection works |
| 2a | Billing | Planned | Ship billing value fast |
| 2b | Dashboard | Planned | Daily visibility |
| 3a | Human vs Agent Analytics | Planned | Better analytics |
| 3b | PM Tool Insights | Optional | Priority reporting |
| 4a | Local Collection | Planned | Git integration |
| 4b | Desktop Integration | Optional | Full workday capture |
| Future | TBD | — | Team features, mobile |

**Optional phases** are workflow-dependent:
- **3b (PM Tool Insights):** Only if you use Linear or similar PM tools for task tracking
- **4b (Desktop Integration):** Only if you want browser/app tracking (requires ActivityWatch)

**Parallelization note:** Phases 2a and 2b can proceed in parallel where resources allow. The TUI benefits from rules being complete but doesn't hard-depend on them.

---

## Phase 1: Foundation ✅

**Status:** Complete

**Scope:** Prove that passive data collection can produce useful time tracking without manual timers.

### Deliverables

- Event collection from tmux hooks (pane focus, commands)
- Claude Code session log parsing (agent activity)
- SSH-based sync to local SQLite
- Stream inference via directory + temporal clustering
- Weekly reports with LLM-suggested tags
- Manual tagging corrections

### Success Criteria

- <5 minutes per week of manual categorization work
- Event collection runs without user intervention
- Reports accurately reflect actual work done

---

## Phase 2a: Billing Workflow

**Theme:** Connect to billing systems. Ship value quickly.

**Rationale:** Automatic tagging must work before TUI is useful (tags are central to the UI). Export to billing systems is independent and high-value. Multi-remote is about daily usability.

### Deliverables

1. **Automatic tagging (rules engine)** — Deterministic auto-tagging for known patterns (spec'd in `architecture/integrations.md`)
2. **Export to billing systems** — Push to Toggl, Clockify, or CSV (spec'd in `architecture/integrations.md`)
3. **`tt stream <id>`** — Detailed view of a single stream
4. **Multi-remote improvements** — Automatic sync scheduling, deduplication of events synced from multiple machines

### Success Criteria

- 90% of streams auto-tagged correctly by rules (measured: user doesn't override tag within 7 days)
- Can push a week's work to Toggl in under 2 minutes without errors
- Sync completes without errors or manual deduplication when run from multiple machines

### Dependencies

- Automatic tagging: None (greenfield)
- Export: Can work with existing tags (user or LLM); benefits from rules but doesn't require them
- Multi-remote: None (infrastructure work)

### Schema Migration

The rules engine adds `assignment_source` to `stream_tags` (see `integrations.md`). Migration strategy:
- `ALTER TABLE stream_tags ADD COLUMN assignment_source TEXT NOT NULL DEFAULT 'inferred'`
- Existing user-assigned tags (via `tt tag`) should be migrated to `assignment_source = 'user'` based on metadata if available

---

## Phase 2b: Dashboard & Manual Entry

**Theme:** Real-time visibility and filling gaps.

**Rationale:** The TUI is most valuable after rules work (tags are central to display). Manual entry covers meetings and offline work that events can't capture.

### Deliverables

1. **TUI dashboard** — Real-time view of streams and agent sessions (spec'd in `design/ux-tui.md`)
2. **Manual entry** — `tt add`, `tt edit`, `tt delete` for meetings and offline work

### Manual Entry Scope

```bash
# Add an entry for today
tt add --start "2pm" --end "3pm" --tag meeting "Team sync"

# Add an entry for a different date
tt add --date yesterday --start "2pm" --end "3pm" --tag meeting "Client call"
tt add --date "2025-01-15" --start "10am" --end "11am" --tag meeting "Planning"

# Multiple tags
tt add --start "2pm" --end "3pm" --tag meeting --tag client-acme "Acme sync"

# Edit an existing entry
tt edit <entry-id> --end "3:30pm"

# Delete an entry
tt delete <entry-id>
```

Validation:
- Start must be before end
- Overlap with existing streams is allowed (parallel work is legitimate) — show informational warning only
- Manual entries stored as synthetic events with `source: manual`

**Note:** Manual entry can be implemented independently and has no dependency on Phase 2a. Consider implementing early if meeting tracking is a pain point.

### Success Criteria

- TUI shows activity updates within 5 seconds of event occurrence
- Meetings and calls captured without external tools
- TUI starts in <500ms, refreshes without flicker

### Dependencies

- TUI dashboard: Benefits from rules (tags display better) but doesn't hard-depend on them
- Manual entry: None (can implement independently or even before Phase 2a)

---

## Phase 3a: Human vs Agent Analytics

**Theme:** Deeper analytics from existing data.

**Rationale:** Human vs agent time classification already exists in the model. This phase improves how we surface those insights.

### Deliverables

1. **Human vs agent display improvements** — Separate "hands-on" from "delegated" in reports with clearer formatting
2. **Parallelization analysis** — Identify which task types benefit most from delegation
3. **Extended summaries** — Monthly and quarterly reports

### User Stories Addressed

- US-005: Understand where time goes
- US-006: Identify delegation opportunities

### Success Criteria

- Report includes delegation ratio breakdown by tag (e.g., "acme: 60% direct, 40% delegated")
- Monthly summaries generate in <5 seconds at 10K events

### Performance Checkpoint

Before Phase 4, validate query performance at scale:
- 10,000+ events
- 3+ months of history
- Multiple concurrent streams

**Threshold:** If monthly report generation exceeds 5 seconds at 10K events, document specific query optimizations required before proceeding to Phase 4.

---

## Phase 3b: PM Tool Insights (Optional)

**Theme:** Insights requiring external data sources.

**Rationale:** Narrower audience — only relevant for users with Linear or similar PM tools. Validate demand before committing.

### Deliverables

1. **Priority reporting** — Time spent by Linear priority level (P0/P1/P2)
2. **Untracked work detection** — Streams with no linked Linear issue (potential scope creep)

### Success Criteria

- Users can spot time sinks and untracked work
- Linear integration setup takes <5 minutes

### When to Skip

- Users not using Linear or similar PM tools
- No demand signal during Phase 2/3a validation

### Deferred

- LLM cost tracking (data not currently captured; rabbit hole risk)

---

## Phase 4a: Local Collection

**Theme:** More signals from local development tools.

### Deliverables

1. **Git integration** — Capture commit and branch events
2. **Rules engine branch matching** — Match streams by git branch pattern

### Implementation Approach

```bash
# Git post-commit hook calls tt ingest
tt ingest --type git-commit --ref HEAD --message "$(git log -1 --format=%s)"
```

Rules can then match:
```toml
[[rules.auto_tag]]
match = { git_branch = "feature/*" }
tags = ["feature-work"]
```

**Hook installation:** TBD — options include a `tt hooks install` command or documentation-only approach. Need to handle existing repos vs new clones, and consider jj compatibility.

### Success Criteria

- Commits attributed to streams automatically
- Rules can filter by branch pattern (e.g., `release/*` vs `feature/*`)

### Pain Points Addressed

- P7: Multi-project attribution via branch patterns

---

## Phase 4b: Desktop Integration (Optional)

**Theme:** Capture non-terminal activity via ActivityWatch.

**Rationale:** ActivityWatch already solves cross-platform window/browser tracking. Integrate rather than replicate.

### Deliverables

1. **ActivityWatch import** — Pull browser and app focus events on demand
2. **Category mapping** — ActivityWatch categories map to tt tags via rules

### Integration Model

ActivityWatch runs independently; `tt` imports on demand:

```bash
# Pull recent browser/app focus data
tt sync --activitywatch

# Or as part of regular sync
tt sync dev-remote --activitywatch
```

Not a required dependency — users who don't want AW can ignore it entirely.

### Success Criteria

- Research time (browser) attributed to streams
- Users without ActivityWatch unaffected

### Pain Points Addressed

- P11: Non-terminal attention (browser research, docs reading)
- P13: Cross-device activity (via AW's existing device support)

### Deferred

- IDE plugins (VS Code, JetBrains) — Evaluate if ActivityWatch doesn't cover enough

---

## Future (TBD)

**Status:** Not yet designed.

These are potential directions to explore based on user feedback after Phases 2-4 are validated:

| Feature | Notes |
|---------|-------|
| Mobile app | View-only dashboard; requires API server |
| API server | REST endpoints for external access |
| Webhooks | Automation triggers for report generation |
| Team dashboards | Aggregate views across users |
| Shared tag taxonomies | Organization-wide tag standards |
| Automated billing schedules | Push to billing systems on schedule |

**Why deferred:** Designing team features now would bias single-user decisions. The architecture should emerge from validated single-user patterns.

---

## Explicitly Not Planned

| Feature | Reason |
|---------|--------|
| Manual start/stop timers | Conflicts with observe-first philosophy |
| Real-time file watchers | Polling is sufficient; watchers add complexity |
| Custom themes | Low value vs implementation cost |
| Desktop notifications | Out of scope for CLI tool |
| Fractional attribution | Splitting one stream across multiple projects — rejected in ADR; recommend separate streams instead |
| Web UI | CLI/TUI is sufficient; web adds hosting complexity |
| Multi-user/shared databases | Single-user architecture; team features deferred to Future |
| Historical data import | Importing old entries from Toggl/Clockify — out of scope; start fresh |
| LLM cost tracking | Data not currently captured; scope creep risk |

### Deferred (May Revisit)

| Feature | Reason | Revisit If |
|---------|--------|------------|
| Calendar integration | Adds OAuth complexity for Google/Outlook | Manual entry proves insufficient for meeting-heavy workflows |

---

## Related Documents

- [Architecture Overview](../architecture/overview.md) — System context and design principles
- [Integrations](../architecture/integrations.md) — Rules engine and push integrations spec
- [TUI Dashboard](../design/ux-tui.md) — Terminal UI specification
- [CLI UX](../design/ux-cli.md) — Command interface patterns
- [Data Model](../design/data-model.md) — Event schema, stream structure

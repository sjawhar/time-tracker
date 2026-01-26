# Implementation Plan

## Milestones

### Milestone 1: Working Prototype (Data Collection)

**Goal**: Start capturing real usage data as quickly as possible to inform algorithm design.

**Scope**:
- Event sources: tmux pane focus events + Claude Code session parsing
- SQLite database with events table (schema from `design/data-model.md`)
- Minimal CLI: `tt ingest` (receives events), `tt events` (dumps raw events)
- No stream inference yet - just raw event capture
- No reports, no tags, no UI

**Not in scope**:
- Browser/window focus tracking
- Stream inference algorithm
- Direct/delegated time distinction
- Any reporting or export
- Git hooks

**Success criteria**:
- tmux focus events captured passively via hook
- Claude session events captured by watching `~/.claude/projects/*/sessions/*.jsonl`
- Can query `SELECT * FROM events` and see real data
- Zero manual intervention required after setup

---

### Milestone 2: MVP (Minimum Viable Product)

**Goal**: A usable product that delivers value - automatic time tracking with minimal user effort.

**Scope**:
- Event sources: tmux focus + Claude session activity (from prototype)
- Stream inference: directory-based clustering with temporal gaps
- **Direct/delegated time**: Core feature - track human attention vs agent execution
- **LLM-suggested tags**: System suggests tags based on session content, user confirms/overrides
- Basic reports: `tt report --week` shows time breakdown with direct/delegated split
- Manual tagging: `tt tag <stream> <tag>` for corrections

**Not in scope (post-MVP)**:
- Toggl export
- TUI dashboard
- Browser tracking
- Git hooks
- Linear integration
- Multi-machine sync

**Success criteria**:
- <5 min/week spent on manual categorization (down from current ~30 min)
- Weekly report shows time breakdown by project with human:agent ratio
- LLM tag suggestions are accurate >80% of the time

---

## Open Design Questions

These need to be resolved and documented in ADRs before implementation.

### 1. Remote Analysis Architecture

**Problem**: How does the local machine learn what a tmux pane is for (project, activity type) without transmitting sensitive session content?

**Proposed approach**: Run LLM analysis on the remote host where Claude Code sessions live. The remote host has:
- Access to `~/.claude/projects/*/sessions/*.jsonl` files
- Can analyze session content locally
- Exposes an API/daemon that local machine can query

**Questions to resolve**:
- What triggers analysis? (on-demand query? background daemon? periodic batch?)
- What data comes back to local? (just tags/project names, not content)
- How does local machine communicate with remote? (SSH command? HTTP API? Unix socket?)
- Where does the LLM run? (remote host calls Claude API? local calls API with summarized context?)

**Status**: Needs ADR — **blocks MVP**, not prototype

### 2. ActivityWatch Integration

**Decision**: Use ActivityWatch watchers as a starting point for local event capture (window focus, AFK detection).

**Status**: Decided — needs ADR documenting rationale — **blocks MVP**, not prototype

---

## Spec Completion Tasks

### Unlocks Prototype

These specs are **already complete enough** to start prototype:

| Spec | Status | Notes |
|------|--------|-------|
| `design/data-model.md` | Complete | Event schema defined |
| `research/technical-foundations.md` | Complete | tmux hooks, agent logs documented |

These specs **need completion** before prototype:

- [ ] **Decide tech stack** — Update `implementation/tech-stack.md` with language choice (Rust vs Go vs Python)

### Unlocks MVP

These specs need completion after prototype, before MVP:

- [ ] **Finalize CLI commands** — Update `ux-cli.md` with actual commands (not just preliminary sketch)
- [ ] **Finalize report format** — Update `ux-reports.md` with MVP report output
- [ ] **Define attention allocation algorithm** — Complete the "Critical TODO" in `architecture/overview.md`
- [ ] **ADR: Remote analysis architecture** — Resolve open question #1
- [ ] **ADR: ActivityWatch integration** — Document decision for open question #2

### Deferred (Post-MVP)

These can wait until after MVP ships:

- [ ] `ux-tui.md` — TUI dashboard design
- [ ] `architecture/integrations.md` — Rules engine, webhooks, API server
- [ ] `implementation/phases.md` — Long-term roadmap (we have milestones for now)

---

## Prototype Implementation Tasks

_Unblocked once tech stack is decided._

- [ ] Set up project structure and build system
- [ ] Implement SQLite event store (schema from `data-model.md`)
- [ ] Implement `tt ingest` command (receive events via stdin/args)
- [ ] Implement `tt events` command (dump raw events)
- [ ] Create tmux hook configuration (calls `tt ingest`)
- [ ] Implement Claude session file watcher
- [ ] Test end-to-end: tmux activity → events in database
- [ ] Deploy and start collecting real data

---

## MVP Implementation Tasks

_Unblocked once prototype is running and MVP specs are complete._

- [ ] Implement stream inference (directory + temporal clustering)
- [ ] Implement direct/delegated time calculation
- [ ] Implement `tt report --week` command
- [ ] Implement LLM tag suggestion (calls Claude API)
- [ ] Implement `tt tag <stream> <tag>` for corrections
- [ ] Implement `tt streams` to list/manage streams
- [ ] Validate against success criteria (<5 min/week manual work)

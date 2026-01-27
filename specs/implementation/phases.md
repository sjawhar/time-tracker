# Implementation Phases

_To be planned after requirements and architecture are established._

## Phase Overview

_High-level phases and their goals._

## Phase Details

_For each phase: scope, deliverables, success criteria._

---

## Preliminary Ideas

> **Note**: The following are preliminary ideas from early brainstorming. The actual implementation plan is in [plan.md](../plan.md) with two defined milestones: Prototype (data collection) and MVP.

### Original Phase Sketch (5 Phases)

**Phase 1: Core Foundation**
- Event store (SQLite with append patterns)
- Basic event schema
- CLI skeleton (`tt status`, `tt today`)
- tmux hook integration

**Phase 2: Context System**
- Context creation and management
- Context rules engine
- Auto-detection from git/tmux
- Context linking

**Phase 3: Agent Integration**
- Claude Code session log parsing
- Codex session log parsing
- Generic agent event protocol
- Human/agent time classification

**Phase 4: Attribution & Reporting**
- Fractional attribution algorithm
- Report generation
- TUI dashboard
- Export formats (Toggl, Clockify CSV)

**Phase 5: Polish & Integration**
- API server
- Webhooks
- Toggl/Clockify direct API sync
- Documentation and onboarding

### Revised Milestones

The 5-phase breakdown has been condensed into two milestones:

1. **Prototype** - Minimal data collection (tmux + Claude sessions → SQLite)
2. **MVP** - Usable product with reports and LLM-suggested tags

See [plan.md](../plan.md) for current milestone definitions.

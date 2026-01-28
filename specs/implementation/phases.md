# Implementation Phases

## Problem Statement

Time Tracker needs a clear, flexible roadmap beyond MVP that communicates direction without locking to dates. The roadmap must align with the product's event-sourced, local-first architecture, avoid feature-factory behavior, and remain credible as priorities shift.

## Research Findings

- Date-based roadmaps quickly go stale; horizons like Now/Next/Later keep focus on intent and confidence. (Aha!, ProdPad)
- Roadmaps should emphasize outcomes/themes to prevent “feature lists” and enable reprioritization. (Aha!, Atlassian)
- Trust depends on updates; a roadmap that is not refreshed becomes noise. (Atlassian)
- Competitors highlight automatic capture, privacy/local storage, and optional integrations. (ActivityWatch, WakaTime)

## Principles

- **Local-first, privacy-first**: data stays local by default; sync is explicit and user-controlled.
- **Event-sourced truth**: raw events are immutable; derived views can be recomputed.
- **Minimal configuration**: no speculative settings; defaults should work.
- **Performance**: fast startup and low overhead on remote machines.
- **Optional intelligence**: LLM use is additive, not required for core workflows.

## Roadmap Format

- **Horizons**: Post-MVP (Near-term), Mid-term, Long-term/Optional.
- **Confidence labels**: Committed, Planned, Exploratory.
- **Update cadence**: Revisit after each major release and at least quarterly.
- **Structure per horizon**: Themes/Outcomes, Scope, Deliverables, Success Criteria, Non-goals.

## Phases

### Post-MVP (Near-term) — Committed

**Themes/Outcomes**
- Capture coverage improves with minimal new collectors.
- Reliability and transparency: users trust the data.
- Reports are useful without manual cleanup.

**Scope**
- Add 1–2 collectors that fit local-first constraints (e.g., editor heartbeat, browser tab focus).
- Health monitoring: watcher heartbeats and stale-data alerts.
- Report expansion: daily/weekly summaries, export formats.
- Privacy controls: clear visibility into collected metadata.

**Deliverables**
- Collector interface with at least one new watcher implemented.
- Heartbeat/health view in CLI (`tt status` shows freshness by watcher).
- Export: CSV + one common external format (e.g., Toggl CSV).
- “What gets captured” documentation + CLI toggle for sensitive fields.

**Success Criteria**
- >90% of active time captured for targeted workflows.
- Users can identify and fix a dead watcher within minutes.
- Reports require <5 minutes of manual edits per week.

**Non-goals**
- No daemonized always-on service.
- No cloud storage or accounts.

### Mid-term — Planned

**Themes/Outcomes**
- Multi-source aggregation across devices/remotes.
- Higher-quality stream inference with fewer manual tags.
- Interop with adjacent tools (without violating local-first).

**Scope**
- Cross-device/remote sync (explicit, pull-based).
- Stream inference using richer context signals (repo, window title, browser domain).
- Integrations that can run locally (ActivityWatch import, optional data exchange).

**Deliverables**
- `tt sync --all` across multiple remotes with conflict-free import.
- Stream inference v2 with confidence scores and audit trail.
- Import pipeline for ActivityWatch data (read-only).

**Success Criteria**
- Two-device workflow with consistent reports and no duplicates.
- Tag corrections decrease week-over-week for active users.
- Integration import does not increase local startup time by >10%.

**Non-goals**
- No automatic cloud push or background sync.
- No centralized dashboard.

### Long-term / Optional — Exploratory

**Themes/Outcomes**
- Advanced insights and automation without sacrificing privacy.
- Rich reporting for teams, while staying local-first by default.

**Scope**
- Optional “insight packs” (productivity trends, project allocation summaries).
- Team workflows via export/share, not shared storage.
- Pluggable collectors beyond core (OS-level focus, mobile).

**Deliverables**
- Insight report module (opt-in, local computation).
- Shareable report bundles (redacted export).
- Collector plugin API with a documented stable contract.

**Success Criteria**
- Insights are useful without manual curation in >70% of trials.
- Exports are safe-by-default (no sensitive fields unless enabled).

**Non-goals**
- No mandatory accounts or cloud-hosted analysis.

## Risks and Failure Modes

- **Scope creep**: roadmap turns into a feature list with unclear outcomes.
- **Date pressure**: deadlines reduce trust when inevitably missed.
- **Privacy regressions**: adding collectors increases sensitivity without controls.
- **Performance regressions**: more watchers slow remote workflows.

Mitigations: stick to horizons, validate each new collector against privacy/perf checks, and require explicit success criteria for each phase.

## Acceptance Criteria

- Roadmap uses horizons and confidence labels, no hard dates.
- Each phase includes themes/outcomes, scope, deliverables, success criteria, and non-goals.
- Explicit alignment with event-sourced, local-first principles.
- Risks and failure modes listed with mitigations.
- Document is clear enough to guide planning without further clarification.

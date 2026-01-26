# AI-Native Time Tracker: Specification Index

A time tracking system designed for AI-augmented development workflows with interleaved tasks, concurrent agent sessions, and remote tmux-based development.

## Status

- **Phase**: Discovery → Design
- **Last Updated**: 2025-01-26

### Status Legend

| Status | Meaning |
|--------|---------|
| Complete | Fully written, ready for implementation |
| Draft | Has content but needs refinement |
| Stub | Template with preliminary ideas, needs work |

---

## Planning

| Document | Status | Description |
|----------|--------|-------------|
| [Plan](plan.md) | Draft | Milestones, open questions, implementation tasks |

---

## Research

Background research on existing solutions, technical capabilities, and prior art.

| Document | Status | Description |
|----------|--------|-------------|
| [Market Landscape](research/market-landscape.md) | Complete | Existing time tracking tools, gaps, and opportunities |
| [Technical Foundations](research/technical-foundations.md) | Complete | tmux hooks, agent session logs, event sourcing patterns |
| [References](research/references.md) | Complete | Links, citations, and source material |

---

## Discovery

User research, interviews, and requirements gathering.

| Document | Status | Description |
|----------|--------|-------------|
| [User Interviews](discovery/user-interviews.md) | Complete | Raw interview notes and transcripts |
| [Personas](discovery/personas.md) | Draft | User archetypes and their characteristics |
| [Pain Points](discovery/pain-points.md) | Complete | Problems to solve, ranked by severity |
| [User Stories](discovery/user-stories.md) | Complete | Stories derived from discovery |

---

## Design

Product design, UX, and conceptual model.

| Document | Status | Description |
|----------|--------|-------------|
| [Core Concepts](design/core-concepts.md) | Complete | Terminology, mental model, key abstractions |
| [Data Model](design/data-model.md) | Complete | Entities, relationships, event schema |
| [UX: CLI](design/ux-cli.md) | Stub | Command-line interface design |
| [UX: TUI](design/ux-tui.md) | Stub | Terminal UI dashboard design (post-MVP) |
| [UX: Reports](design/ux-reports.md) | Stub | Report formats and outputs |

---

## Architecture

Technical architecture and system design.

| Document | Status | Description |
|----------|--------|-------------|
| [Overview](architecture/overview.md) | Draft | High-level system architecture |
| [Components](architecture/components.md) | Stub | Component breakdown and responsibilities |
| [Integrations](architecture/integrations.md) | Stub | tmux, agents, git, export targets |
| [Decisions](architecture/decisions/) | Stub | Architecture Decision Records (ADRs) |

---

## Implementation

Build planning and execution.

| Document | Status | Description |
|----------|--------|-------------|
| [Phases](implementation/phases.md) | Stub | Implementation phases (see plan.md for milestones) |
| [MVP](implementation/mvp.md) | Stub | Minimum viable product scope |
| [Tech Stack](implementation/tech-stack.md) | Stub | Language, libraries, tooling choices |
